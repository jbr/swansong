use event_listener::{Event, EventListener, IntoNotification};
use std::{
    fmt::{Debug, Formatter, Result},
    future::{Future, IntoFuture},
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::{ready, Context, Poll},
};

pub struct Inner {
    count: AtomicUsize,
    event: Event,
}

impl Inner {
    fn new(start: usize) -> Self {
        Self {
            count: AtomicUsize::new(start),
            event: Event::new(),
        }
    }
}

impl Debug for Inner {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        f.debug_struct("GuardInner")
            .field("count", &self.count)
            .field("event", &"..")
            .finish()
    }
}

/// An atomic counter that increments on clone and decrements on drop
///
/// Graceful shutdown will be delayed for as long as at least one `Guard` exists.
#[derive(Debug)]
pub struct Guard(Arc<Inner>);

impl Default for Guard {
    fn default() -> Self {
        Self(Arc::new(Inner::new(1)))
    }
}

impl Clone for Guard {
    fn clone(&self) -> Self {
        self.0.increment();
        Self(self.0.clone())
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.decrement();
    }
}

impl Inner {
    fn count(&self) -> usize {
        self.count.load(Ordering::Relaxed)
    }

    fn wake(&self) {
        self.event.notify(usize::MAX.relaxed());
    }

    fn decrement(&self) {
        match self.count.fetch_sub(1, Ordering::Relaxed) {
            0 => {
                log::error!(concat!(
                    "Attempted to decrement from 0, saturating. ",
                    "Please file an issue; this should be unreachable."
                ));
                self.count.store(0, Ordering::SeqCst);
            }
            1 => {
                log::trace!("decrementing guard count to zero");
                self.wake();
            }
            n => {
                log::trace!("decrementing guard count from {} -> {}", n, n - 1);
            }
        }
    }

    fn increment(&self) {
        let previously = self.count.fetch_add(1, Ordering::Relaxed);
        log::trace!(
            "incrementing guard count from {} -> {}",
            previously,
            previously + 1
        );
    }
}

/**
Keep track of swansong `Guard`s

Awaiting a [`Manager`] will be pending until all connected guards are dropped
*/

#[derive(Debug)]
pub struct Manager(Arc<Inner>);

impl Clone for Manager {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl Default for Manager {
    fn default() -> Self {
        Self(Arc::new(Inner::new(0)))
    }
}

impl Manager {
    /// returns a new manager with a zero count. use [`Manager::counter`] to
    pub fn new() -> Self {
        Self::default()
    }

    /// returns the current guard count
    pub fn count(&self) -> usize {
        self.0.count()
    }

    /// creates a new `Guard` from this `Manager`, incrementing the count
    pub fn guard(&self) -> Guard {
        let guard = Guard(Arc::clone(&self.0));
        self.0.increment();
        guard
    }
}

impl IntoFuture for Manager {
    type Output = ();

    type IntoFuture = ZeroFuture;

    fn into_future(self) -> Self::IntoFuture {
        let listener = self.0.event.listen();
        ZeroFuture {
            inner: self.0,
            listener,
        }
    }
}

impl From<Guard> for Manager {
    fn from(value: Guard) -> Self {
        // value will be decremented on drop of the original
        Self(Arc::clone(&value.0))
    }
}

impl From<Manager> for Guard {
    fn from(manager: Manager) -> Self {
        manager.guard()
    }
}

/// A future that waits for the guard count to decrement to zero
#[derive(Debug)]
pub struct ZeroFuture {
    inner: Arc<Inner>,
    listener: EventListener,
}

impl Future for ZeroFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Self { inner, listener } = &mut *self;
        let mut listener = Pin::new(listener);
        loop {
            if 0 == inner.count() {
                return Poll::Ready(());
            };

            ready!(listener.as_mut().poll(cx));
        }
    }
}

#[cfg(test)]
mod test {
    use super::{Guard, Manager};
    use futures_lite::future::{block_on, poll_once};
    use std::{
        future::{Future, IntoFuture},
        pin::{pin, Pin},
        process::Termination,
        task::Poll,
    };
    use test_harness::test;

    async fn poll_manually<'a, F: Future>(future: &mut Pin<&mut F>) -> Poll<F::Output> {
        std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
    }

    fn harness<F, Fut, O>(test: F) -> O
    where
        F: FnOnce() -> Fut,
        O: Termination,
        Fut: Future<Output = O> + Send,
    {
        block_on(test())
    }

    #[test(harness)]
    async fn doctest_example() {
        let manager = Manager::new();
        assert_eq!(manager.count(), 0);
        {
            let _g = manager.guard();
            assert_eq!(manager.count(), 1);
        }

        assert_eq!(manager.count(), 0);
        let guard = manager.guard();
        assert_eq!(manager.count(), 1);
        let clone = guard.clone();
        assert_eq!(manager.count(), 2);
        let clone2 = guard.clone();
        assert_eq!(manager.count(), 3);
        let mut future = pin!(manager.clone().into_future());
        assert_eq!(poll_manually(&mut future).await, Poll::Pending);
        drop(clone);
        assert_eq!(poll_manually(&mut future).await, Poll::Pending);
        assert_eq!(manager.count(), 2);
        drop(clone2);
        assert_eq!(manager.count(), 1);
        drop(guard);
        manager.await; // ready
        assert_eq!(poll_manually(&mut future).await, Poll::Ready(()));
    }

    #[test(harness)]
    async fn manager_into_and_from() {
        let manager = Manager::new();
        let guard = manager.guard();
        assert_eq!(manager.count(), 1);
        {
            let _guard = guard.clone();
            assert_eq!(manager.count(), 2);
        }
        assert_eq!(manager.count(), 1);
        let manager2 = Manager::from(guard);
        assert_eq!(poll_once(manager.clone().into_future()).await, Some(()));
        assert_eq!(manager.count(), 0);
        let _guard = Guard::from(manager2);
        assert_eq!(manager.count(), 1);
    }

    #[test(harness)]
    async fn manager_test() {
        let manager = Manager::new();
        assert_eq!(manager.count(), 0);
        manager.clone().await; // ready immediately

        let guard = manager.guard();
        let mut clones = Vec::new();
        assert_eq!(manager.count(), 1);
        for i in 1..=10 {
            clones.push(guard.clone());
            assert_eq!(manager.count(), 1 + i);
            assert_eq!(manager.count(), 1 + i);
        }

        let _managers = std::iter::repeat_with(|| manager.clone())
            .take(10)
            .collect::<Vec<_>>();
        assert_eq!(manager.count(), 11); // unchanged,

        for (i, clone) in clones.drain(..).enumerate() {
            assert_eq!(manager.count(), 11 - i);
            assert_eq!(manager.count(), 11 - i);
            assert_eq!(poll_once(manager.clone().into_future()).await, None); // pending
            drop(clone);
            assert_eq!(manager.count(), 10 - i);
            assert_eq!(manager.count(), 10 - i);
        }

        assert_eq!(manager.count(), 1);

        drop(guard);
        assert_eq!(poll_once(manager.clone().into_future()).await, Some(()));
        assert_eq!(manager.count(), 0);
        assert_eq!(poll_once(manager.into_future()).await, Some(()));
    }
}
