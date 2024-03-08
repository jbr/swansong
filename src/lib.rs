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

mod guard;

use futures_lite::Stream;
use guard::Manager;
use std::future::{Future, IntoFuture};
use stopper::Stopper;

/// A graceful shutdown synchronization mechanism
///
/// Awaiting the Swansong will complete when it is stopped AND all associated [`Guard`][types::Guard]s have been
/// dropped.
///
/// Swansongs are cheap to clone.
#[derive(Clone, Debug, Default)]
pub struct Swansong {
    stopper: Stopper,
    manager: Manager,
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
    pub fn stop(&self) {
        log::trace!("initiating graceful shutdown");
        self.stopper.stop();
    }

    /// Determine if this Swansong has already been stopped
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.stopper.is_stopped()
    }

    /// Returns a [`Future`] which wraps the provided future
    /// and stops it when this Swansong has been stopped.
    ///
    /// Note that the `Output` of the returned future is wrapped with an Option. If the future
    /// resolves to `None`, that indicates that it was stopped instead of polling to completion.
    ///
    /// Note that this can only be used with cancel-safe futures.
    pub fn wrap_future<F: Future>(&self, future: F) -> types::Future<F> {
        types::Future {
            inner: self.stopper.stop_future(future),
        }
    }

    /// Return a new [`Stream`] which will complete when this `Swansong`
    /// has been stopped.
    ///
    /// The Stream's Item is unchanged
    pub fn wrap_stream<S: Stream>(&self, stream: S) -> types::Stream<S> {
        types::Stream {
            inner: self.stopper.stop_stream(stream),
        }
    }

    /// Return a new shutdown guard.
    ///
    /// Graceful shutdown will be delayed until this and all clones of it are dropped.
    #[must_use = "A swansong guard only prevents shutdown until it is dropped"]
    pub fn guard(&self) -> types::Guard {
        self.manager.guard()
    }

    /// The current number of outstanding `Guard`s
    #[must_use]
    pub fn guard_count(&self) -> usize {
        self.manager.count()
    }
}

impl IntoFuture for Swansong {
    type Output = ();
    type IntoFuture = types::Shutdown;

    fn into_future(self) -> Self::IntoFuture {
        types::Shutdown {
            stopped: self.stopper.into_future(),
            guard: self.manager.into_future(),
        }
    }
}

/// Types that are available publicly but probably don't need to be named
pub mod types {
    use crate::guard::ZeroFuture;
    use futures_lite::{Future as Future_, Stream as Stream_};
    use std::{
        pin::Pin,
        task::{ready, Context, Poll},
    };
    use stopper::{FutureStopper, Stopped, StreamStopper};

    pub use crate::guard::Guard;

    pin_project_lite::pin_project! {
        /// A wrapper type around a stream that allows it to be stopped by a
        /// [`Swansong`][crate::Swansong]
        pub struct Stream<S> { #[pin] pub(crate) inner: StreamStopper<S> }
    }

    impl<S: Stream_> Stream_ for Stream<S> {
        type Item = S::Item;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            self.project().inner.poll_next(cx)
        }

        fn size_hint(&self) -> (usize, Option<usize>) {
            self.inner.size_hint()
        }
    }

    pin_project_lite::pin_project! {
        /// A wrapper type around a [`Future`][Future_] that allows it to be stopped by a
        /// [`Swansong`][crate::Swansong] at any await point
        pub struct Future<F> { #[pin] pub(crate) inner: FutureStopper<F> }
    }

    impl<F: Future_> Future_ for Future<F> {
        type Output = Option<F::Output>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            self.project().inner.poll(cx)
        }
    }

    /// A [`Future`][Future_] that will be ready when the [`Swansong`][crate::Swansong] has been
    /// stopped AND all guards are dropped.
    #[derive(Debug)]
    pub struct Shutdown {
        pub(crate) stopped: Stopped,
        pub(crate) guard: ZeroFuture,
    }

    impl Future_ for Shutdown {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let Self { stopped, guard } = &mut *self;
            ready!(Pin::new(stopped).poll(cx));
            Pin::new(guard).poll(cx)
        }
    }
}
