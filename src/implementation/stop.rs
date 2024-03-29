use super::Inner;
use event_listener::EventListener;
use std::{
    ops::{Deref, DerefMut},
    sync::{Arc, Weak},
};

mod future;
mod stream;

pin_project_lite::pin_project! {
    /// A wrapper type that implements Stream when wrapping a Stream and Future when wrapping a
    /// Future
    ///
    /// When the associated [`Swansong`][crate::Swansong] is stopped with
    /// [`Swansong::stop`][crate::Swansong::stop] or all clones of the [`Swansong`] have dropped,
    /// the Future or Stream within this Stop will wake and return `Poll::Ready(None)` on next poll,
    /// regardless of where it is being polled.
    pub struct Stop<T> {
        inner: Weak<Inner>,
        #[pin]
        wrapped_type: T,
        guarded: bool,
        stop_listener: Option<EventListener>,
    }

    impl<T> PinnedDrop for Stop<T> {
        fn drop(this: Pin<&mut Self>) {
            if let Some(inner) = this.project().inner.upgrade() {
                inner.decrement();
            }
        }
    }
}

impl<T> Stop<T> {
    pub(crate) fn new(inner: &Arc<Inner>, wrapped_type: T) -> Self {
        Self {
            inner: Arc::downgrade(inner),
            wrapped_type,
            guarded: false,
            stop_listener: None,
        }
    }

    /// Chainable setter to delay shutdown until this wrapper type has dropped.
    #[must_use]
    pub fn guarded(mut self) -> Self {
        if let Some(inner) = self.inner.upgrade() {
            inner.increment();
            self.guarded = true;
        }
        self
    }
}

impl<T> Deref for Stop<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.wrapped_type
    }
}

impl<T> DerefMut for Stop<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.wrapped_type
    }
}
