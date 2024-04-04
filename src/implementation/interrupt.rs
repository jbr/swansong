use crate::{Guard, Inner};
use event_listener::EventListener;
use std::{
    ops::{Deref, DerefMut},
    sync::{Arc, Weak},
};

#[cfg(any(feature = "tokio", feature = "futures-io"))]
mod async_read;
#[cfg(any(feature = "tokio", feature = "futures-io"))]
mod async_write;

mod future;
mod iterator;
mod stream;

pin_project_lite::pin_project! {
    /// A wrapper type that implements Stream when wrapping a [`Stream`] and [`Future`] when wrapping a
    /// Future
    ///
    /// When the associated [`Swansong`][crate::Swansong] is stopped with
    /// [`Swansong::shut_down`][crate::Swansong::shut_down] or all clones of the [`Swansong`] have dropped,
    /// the Future or Stream within this Stop will wake and return `Poll::Ready(None)` on next poll,
    /// regardless of where it is being polled.
    #[derive(Debug)]
    pub struct Interrupt<T> {
        inner: WeakInner,
        #[pin]
        wrapped_type: T,
        guard: Option<Guard>,
        stop_listener: StopListener,
    }
}

impl<T: Eq> Eq for Interrupt<T> {}

impl<T, U> PartialEq<Interrupt<U>> for Interrupt<T>
where
    T: PartialEq<U>,
{
    fn eq(&self, other: &Interrupt<U>) -> bool {
        self.inner.ptr_eq(&other.inner) && self.wrapped_type == other.wrapped_type
    }
}

impl<T> Interrupt<T> {
    pub(crate) fn new(inner: &Arc<Inner>, wrapped_type: T) -> Self {
        Self {
            inner: WeakInner(Arc::downgrade(inner)),
            wrapped_type,
            guard: None,
            stop_listener: StopListener(None),
        }
    }

    /// Chainable setter to delay shutdown until this wrapper type has dropped.
    #[must_use]
    pub fn guarded(mut self) -> Self {
        if let Some(inner) = self.inner.upgrade() {
            self.guard = Some(Guard::new(&inner));
        }
        self
    }

    /// Take the wrapped type out of this Interrupt.
    ///
    /// If the interrupt is guarded with [`Interrupt::guarded`], this will decrement the guard count.
    pub fn into_inner(self) -> T {
        self.wrapped_type
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.inner.is_stopped()
    }

    #[cfg(any(feature = "futures-io", feature = "tokio"))]
    pub(crate) fn is_stopped_relaxed(&self) -> bool {
        self.inner.is_stopped_relaxed()
    }
}

impl<T> Deref for Interrupt<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.wrapped_type
    }
}

impl<T> DerefMut for Interrupt<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.wrapped_type
    }
}

#[derive(Debug)]
struct WeakInner(Weak<Inner>);
impl Deref for WeakInner {
    type Target = Weak<Inner>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl WeakInner {
    fn is_stopped(&self) -> bool {
        self.upgrade().as_deref().map_or(true, Inner::is_stopped)
    }
    fn is_stopped_relaxed(&self) -> bool {
        self.upgrade()
            .as_deref()
            .map_or(true, Inner::is_stopped_relaxed)
    }
}

#[derive(Debug)]
struct StopListener(Option<EventListener>);
impl StopListener {
    fn listen(&mut self, weak_inner: &WeakInner) -> Option<&mut EventListener> {
        let Self(listener) = self;
        if let Some(listener) = listener {
            return Some(listener);
        };
        let inner = weak_inner.upgrade()?;
        let listener = listener.insert(inner.listen_stop());
        if inner.is_stopped() {
            None
        } else {
            Some(listener)
        }
    }
}
impl Deref for StopListener {
    type Target = Option<EventListener>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl DerefMut for StopListener {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
