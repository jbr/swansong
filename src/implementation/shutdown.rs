use super::Inner;
use event_listener::{EventListener, Listener};
use futures_lite::Future;
use std::{
    pin::Pin,
    sync::Arc,
    task::{ready, Context, Poll},
};

/// A [`Future`] that will be ready when the [`Swansong`][crate::Swansong] has been
/// stopped AND all guards are dropped.
///
/// This type can also be used in blocking contexts with [`Shutdown::block`]
#[derive(Debug)]
pub struct Shutdown(Arc<Inner>, Option<EventListener>);

impl Shutdown {
    pub(crate) fn new(inner: &Arc<Inner>) -> Self {
        Self(Arc::clone(inner), None)
    }

    /// Blocks the current thread until shutdown is complete.
    ///
    /// Do not use this in async contexts. Instead, await this [`Shutdown`].
    pub fn block(self) {
        let Self(inner, mut zero_listener) = self;
        loop {
            if inner.is_stopped_relaxed() && inner.is_zero_relaxed() {
                return;
            }

            let listener = if let Some(listener) = zero_listener.take() {
                listener
            } else {
                let listener = inner.listen_zero();
                if inner.is_stopped() && inner.is_zero() {
                    return;
                }
                listener
            };

            listener.wait();
        }
    }
}

impl Future for Shutdown {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Self(inner, zero_listener) = &mut *self;

        loop {
            if inner.is_stopped_relaxed() && inner.is_zero_relaxed() {
                log::trace!("stopped and zero, all done!");
                return Poll::Ready(());
            }
            let listener = match zero_listener {
                Some(listener) => listener,
                None => {
                    log::trace!("registering new listener");
                    let listener = zero_listener.insert(inner.listen_zero());

                    if inner.is_stopped() && inner.is_zero() {
                        log::trace!("stopped and zero, all done!");
                        return Poll::Ready(());
                    }
                    listener
                }
            };
            ready!(Pin::new(listener).poll(cx));
            log::trace!("zero event notified!");
            *zero_listener = None;
        }
    }
}
