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

/*!
# 🦢 A graceful shutdown

## How this crate works

Each [`Swansong`] represents a functional unit of behavior that needs to coordinate termination. The
primary type [`Swansong`] provides the control interface.

To allow Swansong to interrupt/cancel/seal various types, use
[`Swansong::interrupt`]. [`Interrupt<T>`] has custom implementations of
[`Future`][std::future::Future], [`Stream`][futures_core::Stream],
[`AsyncRead`][futures_io::AsyncRead], [`AsyncWrite`][futures_io::AsyncWrite],
[`AsyncBufRead`][futures_io::AsyncBufRead], and [`Iterator`]. See further documentation for the
behaviors for each of these types at [`Interrupt`].

Each of the trait implementations will behave normally with a small overhead to check shutdown state
until shutdown has been initiated with [`Swansong::shut_down`]. When shutdown has been initiated,
any type inside of an associated Interrupt will be interrupted/canceled/sealed at an appropriate
time.

Swansong graceful shutdown is considered in progress while there are outstanding [`Guard`]s. All
guards must be dropped for shutdown to be considered complete. Guards can be created in three ways:

* [`Swansong::guard`] returns a standalone [`Guard`] that can be moved into a closure or future,
  stored in a struct, or otherwise retained temporarily during a block of code that should not be
  abruptly stopped. For example, a http server might move a [`Guard`] into a closure or async task
  that handle an individual request.
* [`Swansong::guarded`] returns a [`Guarded`] wrapper type provides transparent implementations of
  various traits like [`Future`][std::future::Future] and [`Stream`][futures_core::Stream], as well
  as [`Deref`][std::ops::Deref]ing to the wrapped type. This is identical to moving a [`Guard`] into the wrapped
  type, but sometimes it's easier to compose the guard around a named future than to move the guard
  into the future.
* [`Interrupt::guarded`] allows an Interrupt wrapper to also act as a guard.


## Async Example
```rust
# use async_global_executor as executor;
# use async_io::Timer;
# use std::{future::pending, time::Duration};
# use swansong::Swansong;
# futures_lite::future::block_on(async {
let swansong = Swansong::new();
executor::spawn(swansong.interrupt(pending::<()>()).guarded()).detach();
executor::spawn(swansong.guarded(Timer::after(Duration::from_secs(1)))).detach();
swansong.shut_down().await;
# });
```

## Sync example

```rust
# use std::{iter, thread, time::Duration};
# use swansong::Swansong;
let swansong = Swansong::new();

thread::spawn({
   let guard = swansong.guard();
   move || {
       let _guard = guard;
       thread::sleep(Duration::from_secs(1));
    }
});

thread::spawn({
    let swansong = swansong.clone();
    move || {
        for n in swansong.interrupt(iter::repeat_with(|| fastrand::u8(..)))
        {
            thread::sleep(Duration::from_millis(100));
            println!("{n}");
        }
    }
});

thread::sleep(Duration::from_millis(500));

swansong.shut_down().block();
```
*/

use std::{future::IntoFuture, sync::Arc};

mod implementation;
use implementation::Inner;
pub use implementation::{Guard, Guarded, Interrupt, ShutdownCompletion};

/// # 🦢 Shutdown manager
///
/// See crate level docs for overview and example.
///
#[derive(Clone, Debug, Default)]
pub struct Swansong {
    inner: Arc<Inner>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// enum that represents the current status of this `Swansong`.
pub enum ShutdownState {
    /// Shutdown has not been initiated.
    Running,
    /// Shutdown has been initiated, and we are waiting for any `Guard`s to drop.
    ShuttingDown,
    /// Shutdown has been initiated and all guards have been Dropped.
    Complete,
}

impl ShutdownState {
    /// Is the shutdown state [`ShutdownState::Running`]?
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self, ShutdownState::Running)
    }

    /// Is the shutdown state [`ShutdownState::ShuttingDown`]?
    #[must_use]
    pub fn is_shutting_down(&self) -> bool {
        matches!(self, ShutdownState::ShuttingDown)
    }

    /// Is the shutdown state [`ShutdownState::Complete`]?
    #[must_use]
    pub fn is_complete(&self) -> bool {
        matches!(self, ShutdownState::Complete)
    }
}

impl Swansong {
    /// Construct a new `Swansong`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Initiate graceful shutdown.
    ///
    /// This will gracefully stop any associated [`Interrupt`]s.
    ///
    /// This returns a [`ShutdownCompletion`] type, which can be blocked on with
    /// [`ShutdownCompletion::block`] or awaited as a [`Future`][std::future::Future].
    #[allow(clippy::must_use_candidate)] // It's fine not to use the returned ShutdownCompletion.
    pub fn shut_down(&self) -> ShutdownCompletion {
        self.inner.stop();
        ShutdownCompletion::new(&self.inner)
    }

    /// Blocks the current thread until graceful shutdown is complete
    ///
    /// Do not use this in async contexts. Instead, await the [`Swansong`]
    pub fn block_on_shutdown_completion(self) {
        ShutdownCompletion::new(&self.inner).block();
    }

    /// Determine if this `Swansong` is running, shutting down, or complete.
    #[must_use]
    pub fn state(&self) -> ShutdownState {
        let stopped = self.inner.is_stopped();
        let complete = stopped && self.inner.is_zero();
        if complete {
            ShutdownState::Complete
        } else if stopped {
            ShutdownState::ShuttingDown
        } else {
            ShutdownState::Running
        }
    }

    /// Wrap any type with an [`Interrupt`], allowing it to cancel or seal on shutdown.
    ///
    /// [`Interrupt<T>`] has custom implementations of [`Future`][std::future::Future],
    /// [`Stream`][futures_core::Stream], [`AsyncRead`][futures_io::AsyncRead],
    /// [`AsyncWrite`][futures_io::AsyncWrite], [`AsyncBufRead`][futures_io::AsyncBufRead], and
    /// [`Iterator`]. See further documentation for the behaviors for each of these types at
    /// [`Interrupt`].
    ///
    /// Each of the trait implementations will behave normally with a small overhead to check
    /// shutdown state until shutdown has been initiated with [`Swansong::shut_down`]. When shutdown
    /// has been initiated, any type inside of an associated Interrupt will be
    /// interrupted/canceled/sealed at an appropriate time.
    #[must_use]
    pub fn interrupt<T>(&self, wrapped_type: T) -> Interrupt<T> {
        Interrupt::new(&self.inner, wrapped_type)
    }

    /// Returns a new [`Guard`], which forstalls shutdown until it is dropped.
    #[must_use]
    pub fn guard(&self) -> Guard {
        Guard::new(&self.inner)
    }

    /// The current number of outstanding `Guard`s.
    #[must_use]
    pub fn guard_count(&self) -> usize {
        self.inner.guard_count_relaxed()
    }

    /// Attach a guard to the provided type, delaying shutdown until it drops.
    ///
    /// This function returns a [`Guarded`] wrapper type provides transparent implementations of
    /// various traits like [`Future`][std::future::Future] and [`Stream`][futures_core::Stream], as
    /// well as [`Deref`][std::ops::Deref]ing to the wrapped type. This is identical to moving a [`Guard`] into the
    /// wrapped type, but sometimes it's easier to compose the guard around a named future than to
    /// move the guard into the future.  See [`Guarded`] for more information about trait
    /// implementations on [`Guarded`]
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
    type IntoFuture = ShutdownCompletion;

    fn into_future(self) -> Self::IntoFuture {
        ShutdownCompletion::new(&self.inner)
    }
}
