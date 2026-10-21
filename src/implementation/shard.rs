//! Per-thread shards of a node: its stop signal as interrupts see it, and its guard count.
//!
//! Interrupts and guards on a node use the shard belonging to the thread that created them, not
//! node-wide state, so in steady state they only write memory that their own thread owns. See
//! `docs/invariants.md` for the contract this implements.
//!
//! # Stop
//!
//! Ordering between registration and stop comes from each shard's lock rather than from
//! `SeqCst`: [`Shard::stop`] sets [`STOPPED`] before it takes the lock to drain, and
//! registration re-checks it under that lock. A registration therefore either lands before the
//! drain and is woken by it, or observes the stop. Interrupts' other reads of the bit may be
//! `Relaxed`, because the poll that follows a wake is ordered after the wake, and so after the
//! store.
//!
//! # Guards
//!
//! Counts form a tree. A shard counts the guards homed on it and the child nodes that arrived
//! at it; a node counts its arrived shards; a nonempty child node arrives at a shard of its
//! parent. Each level arrives at the one above before its first count becomes visible, and
//! departs only after its last count is gone, so while a guard exists every ancestor of its
//! home sees a nonempty subset (`docs/invariants.md` S3). A guard that finds its shard already
//! arrived relies on an arrival that has already completed.
//!
//! While its node runs, a shard stays arrived when it empties, so steady-state guard traffic
//! touches only the shard. Stopping or collecting the node flushes its empty shards, and from
//! then on a shard departs as soon as it empties. The stop bit lives in the same word as the
//! counts, so a departure and a stop are read-modify-writes of one location and can't both miss
//! that the shard is empty and stopped; the same holds for a node's count and its stop bit.

use super::{
    guard::{GuardInfo, GuardRegistry},
    sync::{AtomicU64, Mutex},
    Inner,
};
use std::{
    cell::Cell,
    fmt::{self, Debug, Formatter},
    mem,
    ops::Deref,
    sync::{atomic::Ordering, Arc, Weak},
    task::{Context, Poll, Waker},
};

/// A shard's `state` counts the guards homed on it in its low 32 bits,
pub(crate) const LOCAL_ONE: u64 = 1;
const LOCAL_MASK: u64 = 0xFFFF_FFFF;
/// the child nodes that arrived at it in the next 30,
pub(crate) const KIDS_ONE: u64 = 1 << 32;
/// whether its node has stopped or been collected,
pub(crate) const STOPPED: u64 = 1 << 62;
/// and whether it has itself arrived at its node. A shard that hasn't arrived is empty.
///
/// [`Inner`]'s state uses the same two bits: whether the node has stopped, and for a child,
/// whether it has arrived at a parent shard.
pub(crate) const ARRIVED: u64 = 1 << 63;

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
    #[cfg(not(swansong_loom))]
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    #[cfg(not(swansong_loom))]
    thread_local! {
        static INDEX: Cell<usize> = const { Cell::new(usize::MAX) };
    }

    // Reset for each loom execution, so that indices are a function of the schedule.
    #[cfg(swansong_loom)]
    loom::lazy_static! {
        static ref NEXT: loom::sync::atomic::AtomicUsize = loom::sync::atomic::AtomicUsize::new(0);
    }
    #[cfg(swansong_loom)]
    loom::thread_local! {
        static INDEX: Cell<usize> = Cell::new(usize::MAX);
    }

    INDEX.with(|index| {
        if index.get() == usize::MAX {
            index.set(NEXT.fetch_add(1, Ordering::Relaxed));
        }
        index.get()
    })
}

/// A shared handle to a shard.
///
/// A root's shards belong to different threads and are allocated side by side, so they are
/// padded to keep them off each other's cache lines. A child has a single shard, used by one
/// connection, and skips the padding: an over-aligned allocation costs a lot on some
/// allocators, and children are created per connection.
#[derive(Clone, Debug)]
pub(crate) enum ShardRef {
    Padded(Arc<CachePadded>),
    Plain(Arc<Shard>),
}

#[derive(Debug)]
#[repr(align(128))]
pub(crate) struct CachePadded(Shard);

impl ShardRef {
    pub(crate) fn new(node: Weak<Inner>, root: bool) -> Self {
        let shard = Shard::new(node, root);
        if root {
            Self::Padded(Arc::new(CachePadded(shard)))
        } else {
            Self::Plain(Arc::new(shard))
        }
    }
}

impl Deref for ShardRef {
    type Target = Shard;

    fn deref(&self) -> &Shard {
        match self {
            Self::Padded(shard) => &shard.0,
            Self::Plain(shard) => shard,
        }
    }
}

/// One thread's share of a node.
///
/// The shard's memory is touched only by the interrupts and guards that use it, which in steady
/// state all belong to one thread, and by stop.
pub(crate) struct Shard {
    /// See [`LOCAL_ONE`], [`KIDS_ONE`], [`STOPPED`] and [`ARRIVED`].
    state: AtomicU64,
    registered: Mutex<Registered>,
    node: Weak<Inner>,
    root: bool,
}

/// What a shard's interrupts, guards and child nodes register, under one lock: all of them
/// belong to the shard's thread, so they don't contend.
#[derive(Debug, Default)]
struct Registered {
    waiters: Slab<Waker>,
    guards: GuardRegistry,
    /// Children created on this shard's thread. Each removes itself when it's collected.
    children: Slab<Weak<Inner>>,
}

impl Debug for Shard {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shard")
            .field("stopped", &self.is_stopped())
            .field("guards", &self.guard_count())
            .finish_non_exhaustive()
    }
}

impl Shard {
    fn new(node: Weak<Inner>, root: bool) -> Self {
        Self {
            state: AtomicU64::new(0),
            registered: Mutex::new(Registered::default()),
            node,
            root,
        }
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.state.load(Ordering::Relaxed) & STOPPED != 0
    }

    /// Latch this shard, wake every interrupt registered on it, and flush it if it's empty.
    /// `node` is this shard's node.
    ///
    /// Registration re-checks the stop bit under the lock taken here, so once the drain has run
    /// the shard accepts no further registrations and its slots are never reused.
    pub(crate) fn stop(&self, node: &Inner) {
        let previous = self.state.fetch_or(STOPPED, Ordering::SeqCst);
        let waiters = mem::take(&mut self.registered.lock().unwrap().waiters);
        for waker in waiters.into_values() {
            waker.wake();
        }
        // Empty when stopped. Otherwise the departure that empties it will see the stop.
        if previous & !STOPPED == ARRIVED {
            self.flush(node);
        }
    }

    /// Count one more `unit` here, first arriving at `node`, this shard's node, if this shard
    /// hasn't arrived.
    pub(crate) fn arrive(&self, unit: u64, node: &Inner) {
        let mut state = self.state.load(Ordering::SeqCst);
        loop {
            if state & ARRIVED == 0 {
                debug_assert_eq!(state & !STOPPED, 0, "an unarrived shard is empty");
                node.arrive();
                match self.state.compare_exchange(
                    state,
                    state | ARRIVED | unit,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => return,
                    Err(actual) => {
                        // Another arrival won; take ours back.
                        node.depart();
                        state = actual;
                    }
                }
            } else {
                match self.state.compare_exchange_weak(
                    state,
                    state + unit,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => return,
                    Err(actual) => state = actual,
                }
            }
        }
    }

    /// Count one more `unit` on a root shard that has already arrived, which needs no access to
    /// the node. Returns false, counting nothing, if the shard hasn't arrived.
    pub(crate) fn arrive_if_arrived(&self, unit: u64) -> bool {
        let mut state = self.state.load(Ordering::SeqCst);
        while state & ARRIVED != 0 {
            match self.state.compare_exchange_weak(
                state,
                state + unit,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(actual) => state = actual,
            }
        }
        false
    }

    /// Count one more `unit` on a shard that a live guard keeps nonempty, such as when cloning it.
    pub(crate) fn arrive_nonempty(&self, unit: u64) {
        let previous = self.state.fetch_add(unit, Ordering::SeqCst);
        debug_assert!(previous & ARRIVED != 0 && previous & !(ARRIVED | STOPPED) != 0);
    }

    /// Count one fewer `unit`. Returns whether that emptied the shard after its node stopped, in
    /// which case the caller must [`flush`][Self::flush] it.
    #[must_use]
    pub(crate) fn depart(&self, unit: u64) -> bool {
        self.state.fetch_sub(unit, Ordering::SeqCst) - unit == ARRIVED | STOPPED
    }

    /// If this stopped shard is arrived but empty, depart from `node`, this shard's node.
    pub(crate) fn flush(&self, node: &Inner) {
        if self
            .state
            .compare_exchange(
                ARRIVED | STOPPED,
                STOPPED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            node.depart();
        }
    }

    /// Record a new guard's provenance.
    pub(crate) fn register(&self, info: &Arc<GuardInfo>) {
        self.registered
            .lock()
            .unwrap()
            .guards
            .register(Arc::downgrade(info));
    }

    pub(crate) fn guard_count(&self) -> usize {
        usize::try_from(self.state.load(Ordering::Relaxed) & LOCAL_MASK)
            .expect("local guard count fits in usize")
    }

    /// Register a child node, returning the key to deregister it with.
    pub(crate) fn register_child(&self, child: Weak<Inner>) -> usize {
        self.registered.lock().unwrap().children.insert(child)
    }

    pub(crate) fn deregister_child(&self, key: usize) {
        // A collected child's `Weak` only frees memory, but drop it outside the lock anyway.
        let removed = self.registered.lock().unwrap().children.remove(key);
        debug_assert!(removed.is_some(), "deregistered an unregistered child");
        drop(removed);
    }

    /// The live children registered here. The caller drops them after the lock is released:
    /// dropping a child's last `Arc` latches its shards and wakes its interrupts.
    pub(crate) fn children(&self) -> Vec<Arc<Inner>> {
        let registered = self.registered.lock().unwrap();
        registered
            .children
            .values()
            .filter_map(Weak::upgrade)
            .collect()
    }

    pub(crate) fn guard_infos(&self, out: &mut Vec<Arc<GuardInfo>>) {
        let guards = &mut self.registered.lock().unwrap().guards;
        guards.sweep();
        out.extend(guards.live());
    }

    pub(crate) fn is_root(&self) -> bool {
        self.root
    }

    /// The node, unless it has been collected.
    pub(crate) fn node(&self) -> Option<Arc<Inner>> {
        self.node.upgrade()
    }

    /// Whether both shards belong to the same node.
    pub(crate) fn same_node(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.node, &other.node)
    }

    #[cfg(all(test, swansong_loom))]
    pub(crate) fn state(&self) -> u64 {
        self.state.load(Ordering::SeqCst)
    }

    #[cfg(all(test, swansong_loom))]
    pub(crate) fn waiter_count(&self) -> usize {
        self.registered.lock().unwrap().waiters.len()
    }

    #[cfg(all(test, not(swansong_loom)))]
    pub(crate) fn child_count(&self) -> usize {
        self.registered.lock().unwrap().children.len()
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
    shard: ShardRef,
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
    pub(crate) fn new(shard: ShardRef) -> Self {
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
        let mut registered = self.shard.registered.lock().unwrap();
        if self.shard.is_stopped() {
            log::trace!("stopped before registering");
            return Poll::Ready(());
        }
        let replaced = self
            .registration
            .as_ref()
            .and_then(|registration| registered.waiters.remove(registration.key));
        let key = registered.waiters.insert(theirs);
        drop(registered);
        drop(replaced);
        self.registration = Some(Registration { key, waker: ours });
        Poll::Pending
    }

    pub(crate) fn shard(&self) -> &ShardRef {
        &self.shard
    }

    /// Whether both listeners belong to the same node, whichever shards they use.
    pub(crate) fn same_node(&self, other: &Self) -> bool {
        self.shard.same_node(&other.shard)
    }
}

impl Drop for StopListener {
    fn drop(&mut self) {
        if let Some(Registration { key, waker }) = self.registration.take() {
            // After a drain the shard stays empty, so a drained key is simply absent.
            let removed = self.shard.registered.lock().unwrap().waiters.remove(key);
            drop(removed);
            drop(waker);
        }
    }
}

/// Values under stable keys, reusing vacated keys: the wakers of a shard's registered
/// interrupts, and its children.
#[derive(Debug)]
struct Slab<T> {
    slots: Vec<Slot<T>>,
    /// Head of the free list: the index of a vacant slot, or `slots.len()` if none is vacant.
    next_vacant: usize,
}

impl<T> Default for Slab<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            next_vacant: 0,
        }
    }
}

#[derive(Debug)]
enum Slot<T> {
    Occupied(T),
    /// Vacant, linking to the next vacant slot.
    Vacant(usize),
}

impl<T> Slab<T> {
    fn insert(&mut self, value: T) -> usize {
        let key = self.next_vacant;
        if let Some(slot) = self.slots.get_mut(key) {
            let Slot::Vacant(next) = mem::replace(slot, Slot::Occupied(value)) else {
                unreachable!("the free list only links vacant slots");
            };
            self.next_vacant = next;
        } else {
            self.slots.push(Slot::Occupied(value));
            self.next_vacant = self.slots.len();
        }
        key
    }

    fn remove(&mut self, key: usize) -> Option<T> {
        let slot = self.slots.get_mut(key)?;
        match mem::replace(slot, Slot::Vacant(self.next_vacant)) {
            Slot::Occupied(value) => {
                self.next_vacant = key;
                Some(value)
            }
            vacant @ Slot::Vacant(_) => {
                *slot = vacant;
                None
            }
        }
    }

    fn values(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().filter_map(|slot| match slot {
            Slot::Occupied(value) => Some(value),
            Slot::Vacant(_) => None,
        })
    }

    fn into_values(self) -> impl Iterator<Item = T> {
        self.slots.into_iter().filter_map(|slot| match slot {
            Slot::Occupied(value) => Some(value),
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
    fn slab_reuses_vacated_slots() {
        let mut waiters = Slab::default();
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
        assert_eq!(waiters.into_values().count(), 5);
    }

    #[test]
    fn slab_survives_emptying() {
        let mut waiters = Slab::default();
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
