#![forbid(unsafe_code, future_incompatible)]
#![deny(
    missing_debug_implementations,
    nonstandard_style,
    missing_copy_implementations,
    unused_qualifications,
    missing_docs,
    rustdoc::missing_crate_level_docs
)]
#![warn(clippy::pedantic)]
//! Graceful shutdown
//!
//! The primary entrypoint is [`Swansong`]

use futures_lite::Stream;
use std::{
    future::{Future, IntoFuture},
    sync::Arc,
};

mod implementation;
use implementation::Inner;
pub use implementation::{Guard, Guarded, Shutdown, Stop};

/// A graceful shutdown synchronization mechanism
///
/// Awaiting the Swansong will complete when it is stopped AND all associated
/// [`Guard`]s have been dropped.
///
/// Swansongs are cheap to clone.
///
/// Dropping all clones of the [`Swansong`] will stop all associated futures.
#[derive(Clone, Debug, Default)]
pub struct Swansong {
    inner: Arc<Inner>,
}

impl Swansong {
    /// Construct a new `Swansong`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Initiate graceful shutdown.
    ///
    /// This will gracefully stop any associated futures or streams.
    ///
    /// This returns a [`Shutdown`] type, which can be blocked on with [`Shutdown::block`] or
    /// awaited as a Future.
    #[allow(clippy::must_use_candidate)] // It's fine not to use the returned Shutdown.
    pub fn stop(&self) -> Shutdown {
        self.inner.stop();
        Shutdown::new(&self.inner)
    }

    /// Blocks the curren thread until graceful shutdown is complete
    ///
    /// Do not use this in async contexts. Instead, await the [`Swansong`]
    pub fn block(self) {
        Shutdown::new(&self.inner).block();
    }

    /// Determine if this Swansong has already been stopped
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.inner.is_stopped()
    }

    /// Returns a [`Future`] which wraps the provided future and stops it when this Swansong has
    /// been stopped.
    ///
    /// Note that the `Output` of the returned future is wrapped with an Option. If the future
    /// resolves to `None`, that indicates that it was stopped instead of polling to completion.
    ///
    /// Note that this can only be used with cancel-safe futures.
    #[must_use]
    pub fn stop_future<F: Future>(&self, future: F) -> Stop<F> {
        Stop::new(&self.inner, future)
    }

    /// Return a new [`Stream`] which will complete when this `Swansong` has been stopped.
    ///
    /// The Stream's Item is unchanged.
    #[must_use]
    pub fn stop_stream<S: Stream>(&self, stream: S) -> Stop<S> {
        Stop::new(&self.inner, stream)
    }

    /// Returns a new Guard, which forstalls shutdown until it is dropped.
    #[must_use]
    pub fn guard(&self) -> Guard {
        Guard::new(&self.inner)
    }

    /// The current number of outstanding `Guard`s
    #[must_use]
    pub fn guard_count(&self) -> usize {
        self.inner.guard_count_relaxed()
    }

    /// Attach a guard to the provided type, delaying shutdown until it drops.
    ///
    /// For Futures, this is identical to moving a `Guard` into the future, and exists as a
    /// convenience.
    ///
    /// `Guarded` implements `Future`, `Stream`, `AsyncRead`, and `AsyncWrite` when the inner type
    /// also does, and `Deref`s to the wrapped type.
    #[must_use]
    pub fn guarded<T>(&self, wrapped_type: T) -> Guarded<T> {
        Guarded::new(&self.inner, wrapped_type)
    }
}

/// If we are dropping the only [`Swansong`], stop the associated futures
impl Drop for Swansong {
    fn drop(&mut self) {
        if 1 == Arc::strong_count(&self.inner) {
            self.inner.stop();
        }
    }
}

impl IntoFuture for Swansong {
    type Output = ();
    type IntoFuture = Shutdown;

    fn into_future(self) -> Self::IntoFuture {
        Shutdown::new(&self.inner)
    }
}
