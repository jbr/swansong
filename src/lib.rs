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

## Debugging stragglers

Each [`Guard`] knows the source location it was created at, captured with `#[track_caller]`.
Guards dropped after shutdown has been initiated log that site at [`log::Level::Debug`], so
enabling debug logging shows each straggler as it resolves.

For more, create the root with [`Swansong::with_guard_report`]. Then, if a shutdown stalls
because of straggling guards, [`Swansong::guard_report`] returns a [`GuardReport`] describing
the outstanding guards, aggregated by creation site and including the age of the oldest guard at
each site, and the drop log includes each straggler's age. Without it, guards record nothing, and
the report only counts them.

## Async Example
```rust
# #![cfg(not(miri))]
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
# #![cfg(not(miri))]
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
pub use implementation::{
    Guard, GuardReport, GuardReportEntry, Guarded, Interrupt, ShutdownCompletion, ShuttingDown,
};

/// # 🦢 Shutdown manager
///
/// See crate level docs for overview and example.
///
/// Two Swansongs are [`Eq`] if and only if they are clones of each other.
#[derive(Debug)]
pub struct Swansong {
    inner: Arc<Inner>,
}

impl Clone for Swansong {
    fn clone(&self) -> Self {
        self.inner.swansong_clone();
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Default for Swansong {
    fn default() -> Self {
        Self::new()
    }
}

impl Eq for Swansong {}

impl PartialEq for Swansong {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
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
        Self {
            inner: Inner::new_root(false),
        }
    }

    /// Construct a new `Swansong` whose [`Guard`]s record where and when they were created, for
    /// [`Swansong::guard_report`]. Its children do the same.
    ///
    /// This is a debugging aid for shutdowns that stall on straggling guards, so it is chosen
    /// when the root is created: a report switched on later would miss the guards that already
    /// exist. Recording costs an allocation, a clock read and a lock on every guard; without it,
    /// creating and dropping a guard costs none of these.
    ///
    /// ```rust
    /// # use swansong::Swansong;
    /// let swansong = Swansong::with_guard_report();
    /// let _guard = swansong.guard();
    /// let report = swansong.guard_report();
    /// assert_eq!(report.entries().len(), 1);
    /// println!("{report}");
    /// ```
    #[must_use]
    pub fn with_guard_report() -> Self {
        Self {
            inner: Inner::new_root(true),
        }
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

    /// Returns a [`ShuttingDown`] future that resolves when shutdown has been
    /// initiated on this `Swansong`.
    ///
    /// This does not initiate shutdown — it only observes the transition from
    /// [`ShutdownState::Running`] to [`ShutdownState::ShuttingDown`]. The future
    /// resolves as soon as [`Swansong::shut_down`] is called (on this handle or
    /// any parent), or when the last root handle is dropped.
    ///
    /// The returned future does not wait for outstanding [`Guard`]s to drop. To
    /// wait for full shutdown (stopped _and_ all guards dropped), use
    /// [`Swansong::shut_down`] or [`Swansong::into_future`].
    ///
    /// [`ShuttingDown`] can also be used in blocking contexts via
    /// [`ShuttingDown::block`].
    #[must_use]
    pub fn shutting_down(&self) -> ShuttingDown {
        ShuttingDown::new(&self.inner)
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
    ///
    /// The guard records the source location of this call for diagnostic
    /// purposes; see [`Swansong::guard_report`].
    #[must_use]
    #[track_caller]
    pub fn guard(&self) -> Guard {
        Guard::new(&self.inner)
    }

    /// The total number of outstanding `Guard`s in this Swansong's subtree.
    ///
    /// For a root or leaf Swansong with no children, this is just the Guards created
    /// via this Swansong (and its clones). For a Swansong with children, it is the
    /// sum of Guards held in this node plus all Guards in all descendant subtrees.
    /// Child Swansong handles themselves are not counted as Guards; only real
    /// `Guard` instances are.
    ///
    /// This walks the live subtree, reading each node's shards and locking each shard's
    /// child registry. Child registries retain reusable slots, so traversal time also depends
    /// on their historical peak occupancy, even after children have dropped. Prefer not to
    /// call this in a hot path.
    #[must_use]
    pub fn guard_count(&self) -> usize {
        self.inner.guard_count_subtree()
    }

    /// Report the provenance of the outstanding [`Guard`]s in this Swansong's
    /// subtree.
    ///
    /// Under a root created with [`Swansong::with_guard_report`], every guard
    /// records the source location it was created at (captured with
    /// `#[track_caller]`, so it points at the `swansong.guard()`,
    /// `swansong.guarded(..)`, or `interrupt.guarded()` call in your code;
    /// clones inherit the location of the guard they were cloned from) and
    /// when. This method aggregates the currently-live guards by creation site.
    /// It is useful for debugging a shutdown that is stalled on straggling
    /// guards — for example, by logging the report (via its
    /// [`Display`][std::fmt::Display] implementation) when
    /// [`Swansong::shut_down`] has not completed within a deadline. Otherwise
    /// the report only has the [`guard_count`][GuardReport::guard_count].
    ///
    /// Like [`Swansong::guard_count`], this walks the live subtree and scans retained
    /// child-registry slots. When provenance is enabled, it also locks each shard's guard
    /// registry to sweep dead entries and collect live records. Registry scans take place
    /// under their locks; this is intended for shutdown-time diagnostics rather than hot paths.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use swansong::Swansong;
    /// let swansong = Swansong::with_guard_report();
    /// let guard = swansong.guard();
    /// let report = swansong.guard_report();
    /// assert_eq!(report.guard_count(), 1);
    /// println!("{report}");
    /// drop(guard);
    /// assert!(swansong.guard_report().is_empty());
    /// ```
    #[must_use]
    pub fn guard_report(&self) -> GuardReport {
        if !self.inner.keeps_guard_report() {
            return GuardReport::unrecorded(self.guard_count());
        }
        let mut infos = Vec::new();
        self.inner.guard_infos_subtree(&mut infos);
        GuardReport::from_infos(infos)
    }

    /// Create a child [`Swansong`] linked to this parent.
    ///
    /// The child can be independently shut down without affecting the parent. However, when
    /// the parent is shut down, the child will also be shut down. The parent's shutdown will
    /// not be considered complete until the child [`Swansong`] and any associated
    /// [`ShutdownCompletion`]s are dropped.
    ///
    /// This supports multi-level hierarchies: a child can itself have children, forming a
    /// tree. Shutdown propagates downward through the entire tree.
    ///
    /// # Performance
    ///
    /// Creating a child registers it in amortized O(1) time, without scanning existing
    /// children. Registration takes a parent shard's lock and increments the parent's shared
    /// reference count. Roots spread registrations across shards, but different threads can
    /// select the same shard; child nodes have only one shard. The lock is also shared with
    /// interrupt registration and diagnostics, so cross-thread contention is still possible.
    /// Creating a child after the parent has stopped also stops the new child.
    ///
    /// # Example: HTTP/3 server
    ///
    /// ```rust
    /// # use swansong::Swansong;
    /// let server = Swansong::new();
    ///
    /// // Each connection gets its own child swansong
    /// let connection = server.child();
    ///
    /// // Connection can be shut down independently (e.g., on GOAWAY)
    /// connection.shut_down();
    ///
    /// // Or the entire server can shut down, which also stops all connections
    /// // server.shut_down().await;
    /// ```
    #[must_use]
    pub fn child(&self) -> Swansong {
        Swansong {
            inner: Inner::new_child(&self.inner),
        }
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
    #[track_caller]
    pub fn guarded<T>(&self, wrapped_type: T) -> Guarded<T> {
        Guarded::new(&self.inner, wrapped_type)
    }
}

/// If we are dropping the last `Swansong` handle to a **root** node, stop the
/// associated futures. Dropping the last handle to a **child** (non-root) node
/// does not signal shutdown: the parent remains the governing authority, and
/// the child stays in its current state until the parent propagates stop or
/// until all Guards in the child's subset naturally drain.
impl Drop for Swansong {
    fn drop(&mut self) {
        if self.inner.swansong_drop() && self.inner.is_root() {
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
