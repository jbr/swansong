//! Per-thread shards of a node's stop signal.
//!
//! Interrupts on a node register for stop wakeups on the shard belonging to the thread that
//! created them, not on one node-wide list, so in steady state an interrupt only writes memory
//! that its own thread owns. Stopping a node latches and drains every shard. See
//! `docs/invariants.md` §3 for the contract this implements.
//!
//! Ordering between registration and stop comes from each shard's lock rather than from
//! `SeqCst`: [`Shard::stop`] sets `stopped` before it takes the lock to drain, and registration
//! re-checks `stopped` under that lock. A registration therefore either lands before the drain
//! and is woken by it, or observes the stop. Every other read of `stopped` may be `Relaxed`,
//! because the poll that follows a wake is ordered after the wake, and so after the store.

use super::Inner;
use std::{
    cell::Cell,
    fmt::{self, Debug, Formatter},
    mem,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Weak,
    },
    task::{Context, Poll, Waker},
};

#[cfg(swansong_loom)]
use loom::sync::{atomic::AtomicBool, Mutex};
#[cfg(not(swansong_loom))]
use std::sync::{atomic::AtomicBool, Mutex};

/// The most shards a root gets, however many threads the machine has.
#[cfg(not(swansong_loom))]
const MAX_SHARDS: usize = 256;

/// How many shards a root gets: the available parallelism, rounded up to a power of two.
#[cfg(not(swansong_loom))]
pub(crate) fn root_shard_count() -> usize {
    static COUNT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *COUNT.get_or_init(|| {
        std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .next_power_of_two()
            .min(MAX_SHARDS)
    })
}

/// Two shards keep loom's state space small while still exercising more than one.
#[cfg(swansong_loom)]
pub(crate) fn root_shard_count() -> usize {
    2
}

/// The calling thread's index, assigned round-robin on first use. A node reduces it modulo its
/// shard count, so threads spread evenly over every node's shards with no per-node state, and a
/// thread exiting needs no cleanup.
pub(crate) fn thread_index() -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    thread_local! {
        static INDEX: Cell<usize> = const { Cell::new(usize::MAX) };
    }
    INDEX.with(|index| {
        if index.get() == usize::MAX {
            index.set(NEXT.fetch_add(1, Ordering::Relaxed));
        }
        index.get()
    })
}

/// One thread's share of a node's stop signal.
///
/// `stopped` is written once, by [`Shard::stop`]. Until then the shard's memory is touched only
/// by the interrupts registered on it, which in steady state all belong to one thread. The
/// alignment keeps shards that belong to different threads off each other's cache lines.
#[repr(align(128))]
pub(crate) struct Shard {
    stopped: AtomicBool,
    waiters: Mutex<Waiters>,
    node: Weak<Inner>,
}

impl Debug for Shard {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shard")
            .field("stopped", &self.is_stopped())
            .finish_non_exhaustive()
    }
}

impl Shard {
    pub(crate) fn new(node: Weak<Inner>) -> Self {
        Self {
            stopped: AtomicBool::new(false),
            waiters: Mutex::new(Waiters::default()),
            node,
        }
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    /// Latch this shard and wake every interrupt registered on it.
    ///
    /// Registration re-checks `stopped` under the lock taken here, so once the drain has run the
    /// shard accepts no further registrations and its slots are never reused.
    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        let waiters = mem::take(&mut *self.waiters.lock().unwrap());
        for waker in waiters.into_wakers() {
            waker.wake();
        }
    }
}

/// An interrupt's connection to its node's stop signal: the shard it reads the stop flag from
/// and registers on, plus its registration once it has one.
///
/// Registration is lazy, made only once the wrapped value returns Pending, and persists across
/// polls; the registered waker is replaced only when the task's waker changes. Dropping the
/// listener removes the registration.
#[derive(Debug)]
pub(crate) struct StopListener {
    shard: Arc<Shard>,
    registration: Option<Registration>,
}

#[derive(Debug)]
struct Registration {
    key: usize,
    /// A clone of the waker held in the shard, so that a re-poll from the same task can confirm
    /// its registration without taking the lock.
    waker: Waker,
}

impl StopListener {
    pub(crate) fn new(shard: Arc<Shard>) -> Self {
        Self {
            shard,
            registration: None,
        }
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.shard.is_stopped()
    }

    /// Ready if the node has stopped. Otherwise, ensure the current task will be woken when it
    /// stops, and return Pending.
    pub(crate) fn poll_stopped(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        let waker = cx.waker();
        if let Some(registration) = &self.registration {
            if registration.waker.will_wake(waker) {
                return if self.shard.is_stopped() {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                };
            }
        }

        // Clone outside the lock: cloning a waker runs executor code.
        let (ours, theirs) = (waker.clone(), waker.clone());
        let mut waiters = self.shard.waiters.lock().unwrap();
        if self.shard.is_stopped() {
            log::trace!("stopped before registering");
            return Poll::Ready(());
        }
        let replaced = self
            .registration
            .as_ref()
            .and_then(|registration| waiters.remove(registration.key));
        let key = waiters.insert(theirs);
        drop(waiters);
        drop(replaced);
        self.registration = Some(Registration { key, waker: ours });
        Poll::Pending
    }

    /// The node, unless it has been collected.
    pub(crate) fn node(&self) -> Option<Arc<Inner>> {
        self.shard.node.upgrade()
    }

    /// Whether both listeners belong to the same node, whichever shards they use.
    pub(crate) fn same_node(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.shard.node, &other.shard.node)
    }
}

impl Drop for StopListener {
    fn drop(&mut self) {
        if let Some(Registration { key, waker }) = self.registration.take() {
            // After a drain the shard stays empty, so a drained key is simply absent.
            let removed = self.shard.waiters.lock().unwrap().remove(key);
            drop(removed);
            drop(waker);
        }
    }
}

/// The wakers registered on a shard, in a slab keyed by registration.
#[derive(Debug, Default)]
struct Waiters {
    slots: Vec<Slot>,
    /// Head of the free list: the index of a vacant slot, or `slots.len()` if none is vacant.
    next_vacant: usize,
}

#[derive(Debug)]
enum Slot {
    Occupied(Waker),
    /// Vacant, linking to the next vacant slot.
    Vacant(usize),
}

impl Waiters {
    fn insert(&mut self, waker: Waker) -> usize {
        let key = self.next_vacant;
        if let Some(slot) = self.slots.get_mut(key) {
            let Slot::Vacant(next) = mem::replace(slot, Slot::Occupied(waker)) else {
                unreachable!("the free list only links vacant slots");
            };
            self.next_vacant = next;
        } else {
            self.slots.push(Slot::Occupied(waker));
            self.next_vacant = self.slots.len();
        }
        key
    }

    fn remove(&mut self, key: usize) -> Option<Waker> {
        let slot = self.slots.get_mut(key)?;
        match mem::replace(slot, Slot::Vacant(self.next_vacant)) {
            Slot::Occupied(waker) => {
                self.next_vacant = key;
                Some(waker)
            }
            vacant @ Slot::Vacant(_) => {
                *slot = vacant;
                None
            }
        }
    }

    fn into_wakers(self) -> impl Iterator<Item = Waker> {
        self.slots.into_iter().filter_map(|slot| match slot {
            Slot::Occupied(waker) => Some(waker),
            Slot::Vacant(_) => None,
        })
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| matches!(slot, Slot::Occupied(_)))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiters_reuse_vacated_slots() {
        let mut waiters = Waiters::default();
        let keys: Vec<_> = (0..4)
            .map(|_| waiters.insert(Waker::noop().clone()))
            .collect();
        assert_eq!(keys, [0, 1, 2, 3]);

        assert!(waiters.remove(1).is_some());
        assert!(waiters.remove(3).is_some());
        assert!(waiters.remove(3).is_none(), "a vacant key removes nothing");
        assert!(
            waiters.remove(7).is_none(),
            "an out-of-range key removes nothing"
        );
        assert_eq!(waiters.len(), 2);

        assert_eq!(waiters.insert(Waker::noop().clone()), 3);
        assert_eq!(waiters.insert(Waker::noop().clone()), 1);
        assert_eq!(waiters.insert(Waker::noop().clone()), 4);
        assert_eq!(waiters.len(), 5);
        assert_eq!(waiters.into_wakers().count(), 5);
    }

    #[test]
    fn waiters_survive_emptying() {
        let mut waiters = Waiters::default();
        for round in 0..3 {
            let keys: Vec<_> = (0..=round)
                .map(|_| waiters.insert(Waker::noop().clone()))
                .collect();
            for key in keys {
                assert!(waiters.remove(key).is_some());
            }
            assert_eq!(waiters.len(), 0);
        }
        assert_eq!(waiters.slots.len(), 3);
    }
}

#[cfg(all(test, swansong_loom))]
mod loom_models {
    //! Models of the register/stop protocol (`docs/invariants.md` §3 and §7). Run with
    //! `RUSTFLAGS="--cfg swansong_loom" cargo test --lib --release loom`.

    use crate::{implementation::Inner, Interrupt};
    use loom::{
        sync::atomic::{AtomicUsize, Ordering},
        thread,
    };
    use std::{
        future::{pending, Future, Pending},
        pin::Pin,
        sync::Arc,
        task::{Context, Poll, Wake, Waker},
    };

    #[derive(Default)]
    struct Wakes(AtomicUsize);

    impl Wake for Wakes {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl Wakes {
        fn waker() -> (Arc<Self>, Waker) {
            let wakes = Arc::new(Self::default());
            (Arc::clone(&wakes), Waker::from(wakes))
        }

        fn count(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn poll(interrupt: &mut Interrupt<Pending<()>>, waker: &Waker) -> Poll<Option<()>> {
        Pin::new(interrupt).poll(&mut Context::from_waker(waker))
    }

    /// L1, L2: a poll racing stop either observes it or registers in time to be woken by it.
    #[test]
    fn register_vs_stop() {
        loom::model(|| {
            let root = Inner::new_root();
            let mut interrupt = Interrupt::new(&root, pending());
            let (wakes, waker) = Wakes::waker();

            let stopper = thread::spawn({
                let root = Arc::clone(&root);
                move || root.stop()
            });
            let polled = poll(&mut interrupt, &waker);
            stopper.join().unwrap();

            if polled.is_pending() {
                assert!(wakes.count() > 0, "pending interrupt not woken by stop");
            }
            assert_eq!(poll(&mut interrupt, &waker), Poll::Ready(None));
        });
    }

    /// A re-poll from the same task skips the lock; the registration it relies on must still
    /// be woken by a racing stop.
    #[test]
    fn same_waker_repoll_vs_stop() {
        loom::model(|| {
            let root = Inner::new_root();
            let mut interrupt = Interrupt::new(&root, pending());
            let (wakes, waker) = Wakes::waker();
            assert!(poll(&mut interrupt, &waker).is_pending());

            let stopper = thread::spawn({
                let root = Arc::clone(&root);
                move || root.stop()
            });
            let polled = poll(&mut interrupt, &waker);
            stopper.join().unwrap();

            if polled.is_pending() {
                assert!(wakes.count() > 0, "pending interrupt not woken by stop");
            }
            assert_eq!(poll(&mut interrupt, &waker), Poll::Ready(None));
        });
    }

    /// L4: a registered interrupt migrates to another thread and is polled with a new waker
    /// while stop drains its shard. If that poll returns Pending, the new waker must be woken.
    #[test]
    fn migration_and_waker_replacement_vs_stop() {
        loom::model(|| {
            let root = Inner::new_root();
            let mut interrupt = Interrupt::new(&root, pending());
            let (_, first_waker) = Wakes::waker();
            assert!(poll(&mut interrupt, &first_waker).is_pending());

            let stopper = thread::spawn({
                let root = Arc::clone(&root);
                move || root.stop()
            });
            let migrated = thread::spawn(move || {
                let (wakes, waker) = Wakes::waker();
                let polled = poll(&mut interrupt, &waker);
                (interrupt, wakes, waker, polled)
            });
            stopper.join().unwrap();
            let (mut interrupt, wakes, waker, polled) = migrated.join().unwrap();

            if polled.is_pending() {
                assert!(wakes.count() > 0, "replacement waker not woken by stop");
            }
            assert_eq!(poll(&mut interrupt, &waker), Poll::Ready(None));
        });
    }

    /// Dropping one registration while stop drains must not disturb another's: the survivor is
    /// woken, and no registration outlives the drain.
    #[test]
    fn deregistration_vs_stop() {
        loom::model(|| {
            let root = Inner::new_root();
            let mut dropped = Interrupt::new(&root, pending());
            let mut survivor = Interrupt::new(&root, pending());
            let (_, dropped_waker) = Wakes::waker();
            let (wakes, survivor_waker) = Wakes::waker();
            assert!(poll(&mut dropped, &dropped_waker).is_pending());
            assert!(poll(&mut survivor, &survivor_waker).is_pending());

            let dropper = thread::spawn(move || drop(dropped));
            root.stop();
            dropper.join().unwrap();

            assert!(wakes.count() > 0, "surviving interrupt not woken by stop");
            for shard in root.shards() {
                assert_eq!(shard.waiters.lock().unwrap().len(), 0);
            }
            assert_eq!(poll(&mut survivor, &survivor_waker), Poll::Ready(None));
        });
    }

    /// `spec.md` §5.8: when a child is collected, an interrupt pending on it goes terminal and
    /// is woken to observe that.
    #[test]
    fn collection_vs_pending_interrupt() {
        loom::model(|| {
            let root = Inner::new_root();
            let child = Inner::new_child(&root);
            let mut interrupt = Interrupt::new(&child, pending());
            let (wakes, waker) = Wakes::waker();

            let collector = thread::spawn(move || drop(child));
            let polled = poll(&mut interrupt, &waker);
            collector.join().unwrap();

            if polled.is_pending() {
                assert!(wakes.count() > 0, "interrupt on collected child not woken");
            }
            assert_eq!(poll(&mut interrupt, &waker), Poll::Ready(None));
            assert!(root.shards().iter().all(|shard| !shard.is_stopped()));
        });
    }
}
