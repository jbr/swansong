use super::shard::StopListener;
use crate::{Guard, Inner};
use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
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
        stop_listener: StopListener,
        #[pin]
        wrapped_type: T,
        guard: Option<Guard>,
    }
}

impl<T: Eq> Eq for Interrupt<T> {}

impl<T, U> PartialEq<Interrupt<U>> for Interrupt<T>
where
    T: PartialEq<U>,
{
    fn eq(&self, other: &Interrupt<U>) -> bool {
        self.stop_listener.same_node(&other.stop_listener)
            && self.wrapped_type == other.wrapped_type
    }
}

impl<T> Interrupt<T> {
    pub(crate) fn new(inner: &Inner, wrapped_type: T) -> Self {
        Self {
            stop_listener: StopListener::new(Arc::clone(inner.shard())),
            wrapped_type,
            guard: None,
        }
    }

    /// Chainable setter to delay shutdown until this wrapper type has dropped.
    ///
    /// The guard records the source location of this call for diagnostic
    /// purposes; see [`Swansong::guard_report`][crate::Swansong::guard_report].
    #[must_use]
    #[track_caller]
    pub fn guarded(mut self) -> Self {
        if let Some(guard) = Guard::for_shard(self.stop_listener.shard()) {
            self.guard = Some(guard);
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
        self.stop_listener.is_stopped()
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
