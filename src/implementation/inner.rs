use super::{
    guard::GuardInfo,
    shard::{self, Shard, ARRIVED, KIDS_ONE, STOPPED},
    sync::{AtomicU64, AtomicUsize, Mutex},
};
use event_listener::{Event, EventListener, IntoNotification};
use std::sync::{atomic::Ordering, Arc, Weak};

/// `Inner::state` counts the node's arrived shards in its low 32 bits; the subset (this node and
/// its descendants) is nonempty iff that count is. [`STOPPED`] marks a stopped node, and for a
/// child, [`ARRIVED`] marks that it has arrived at a parent shard, whose index is in bits 32..48.
/// See `shard.rs` for the protocol.
const SHARDS_MASK: u64 = 0xFFFF_FFFF;
const PARENT_SHARD_SHIFT: u32 = 32;
const PARENT_SHARD_MASK: u64 = 0xFFFF;

#[derive(Debug)]
pub(crate) struct Inner {
    /// Notified at stop, for [`ShuttingDown`][crate::ShuttingDown]. Interrupts use `shards`.
    stop_event: Event,
    zero_event: Event,
    state: AtomicU64,
    swansong_count: AtomicUsize,
    /// Strong refs to every ancestor, deepest first: `[parent, grandparent, ..., root]`.
    /// Empty for a root. Strong (not Weak) so that a departure always has a valid parent to
    /// reach, even if every user-held Swansong to an intermediate ancestor has been dropped.
    ancestors: Vec<Arc<Inner>>,
    children: Mutex<Vec<Weak<Inner>>>,
    /// One shard per thread (modulo the length, a power of two), for interrupts and guards.
    /// Roots get [`shard::root_shard_count`] shards; children are per connection and lightly
    /// contended, so they get one.
    shards: Box<[Arc<Shard>]>,
    /// How many times the zero event has been notified, for the loom models.
    #[cfg(swansong_loom)]
    zero_notifications: AtomicUsize,
}

impl Inner {
    fn new(this: &Weak<Self>, ancestors: Vec<Arc<Self>>, shard_count: usize) -> Self {
        debug_assert!(shard_count.is_power_of_two());
        let root = ancestors.is_empty();
        Self {
            stop_event: Event::new(),
            zero_event: Event::new(),
            state: AtomicU64::new(0),
            swansong_count: AtomicUsize::new(1),
            ancestors,
            children: Mutex::default(),
            shards: (0..shard_count)
                .map(|_| Arc::new(Shard::new(Weak::clone(this), root)))
                .collect(),
            #[cfg(swansong_loom)]
            zero_notifications: AtomicUsize::new(0),
        }
    }

    pub(crate) fn new_root() -> Arc<Self> {
        Arc::new_cyclic(|this| Self::new(this, Vec::new(), shard::root_shard_count()))
    }

    pub(crate) fn new_child(parent: &Arc<Self>) -> Arc<Self> {
        let mut ancestors = Vec::with_capacity(parent.ancestors.len() + 1);
        ancestors.push(Arc::clone(parent));
        ancestors.extend(parent.ancestors.iter().cloned());

        let inner = Arc::new_cyclic(|this| Self::new(this, ancestors, 1));
        parent.add_child(Arc::downgrade(&inner));
        if parent.is_stopped() {
            inner.stop();
        }
        inner
    }

    fn add_child(&self, child: Weak<Inner>) {
        let mut children = self.children.lock().unwrap();
        children.retain(|c| c.upgrade().is_some_and(|c| !c.is_complete()));
        children.push(child);
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.state.load(Ordering::SeqCst) & STOPPED != 0
    }

    pub(crate) fn is_stopped_relaxed(&self) -> bool {
        self.state.load(Ordering::Relaxed) & STOPPED != 0
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.state.load(Ordering::SeqCst) & SHARDS_MASK == 0
    }

    pub(crate) fn is_zero_relaxed(&self) -> bool {
        self.state.load(Ordering::Relaxed) & SHARDS_MASK == 0
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.is_stopped() && self.is_zero()
    }

    pub(crate) fn is_root(&self) -> bool {
        self.ancestors.is_empty()
    }

    pub(crate) fn listen_zero(&self) -> EventListener {
        self.zero_event.listen()
    }

    pub(crate) fn listen_stop(&self) -> EventListener {
        self.stop_event.listen()
    }

    fn notify_zero(&self) {
        #[cfg(swansong_loom)]
        self.zero_notifications.fetch_add(1, Ordering::SeqCst);
        self.zero_event.notify(usize::MAX.relaxed());
    }

    #[cfg(all(test, swansong_loom))]
    pub(crate) fn zero_notifications(&self) -> usize {
        self.zero_notifications.load(Ordering::SeqCst)
    }

    fn shard_index(&self) -> usize {
        shard::thread_index() & (self.shards.len() - 1)
    }

    /// The calling thread's shard. An interrupt keeps the shard of the thread that created it,
    /// which is almost always the thread that polls it; one created on a spawner thread
    /// registers on the spawner's shard, contending there once per registration and drop.
    pub(crate) fn shard(&self) -> &Arc<Shard> {
        &self.shards[self.shard_index()]
    }

    #[cfg(all(test, swansong_loom))]
    pub(crate) fn shards(&self) -> &[Arc<Shard>] {
        &self.shards
    }

    pub(crate) fn swansong_clone(&self) {
        self.swansong_count.fetch_add(1, Ordering::SeqCst);
    }

    /// Decrement the swansong handle count. Returns `true` if this was the last handle.
    pub(crate) fn swansong_drop(&self) -> bool {
        self.swansong_count.fetch_sub(1, Ordering::SeqCst) == 1
    }

    pub(crate) fn stop(&self) {
        log::trace!("intending to stop");

        let previous = self.state.fetch_or(STOPPED, Ordering::SeqCst);
        if previous & STOPPED != 0 {
            log::trace!("was already stopped");
            return;
        }

        log::trace!("stopped");
        for shard in &self.shards {
            shard.stop(self);
        }
        self.stop_event.notify(usize::MAX.relaxed());

        // Snapshot children under the lock, then release before recursing to avoid
        // holding the lock across child.stop() calls.
        let children: Vec<Arc<Inner>> = {
            let guard = self.children.lock().unwrap();
            guard.iter().filter_map(Weak::upgrade).collect()
        };
        for child in children {
            child.stop();
        }

        // Empty when stopped. Otherwise the departure that empties the node will see the stop.
        if previous & SHARDS_MASK == 0 {
            self.notify_zero();
        }
    }

    /// One of this node's shards arrived. Count it, first arriving at a parent shard if this
    /// node hasn't.
    pub(crate) fn arrive(&self) {
        let Some(parent) = self.ancestors.first() else {
            self.state.fetch_add(1, Ordering::SeqCst);
            return;
        };
        let mut state = self.state.load(Ordering::SeqCst);
        loop {
            if state & ARRIVED == 0 {
                debug_assert_eq!(state & !STOPPED, 0, "an unarrived child is empty");
                let index = parent.shard_index();
                let shard = &parent.shards[index];
                shard.arrive(KIDS_ONE, parent);
                let arrived = state | ARRIVED | (index as u64) << PARENT_SHARD_SHIFT | 1;
                match self.state.compare_exchange(
                    state,
                    arrived,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => return,
                    Err(actual) => {
                        // Another arrival won; take ours back.
                        if shard.depart(KIDS_ONE) {
                            shard.flush(parent);
                        }
                        state = actual;
                    }
                }
            } else {
                match self.state.compare_exchange_weak(
                    state,
                    state + 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => return,
                    Err(actual) => state = actual,
                }
            }
        }
    }

    /// One of this node's shards departed. Uncount it; if that empties the subset, notify
    /// completions (once stopped) and depart from the parent shard.
    pub(crate) fn depart(&self) {
        let state = self.state.fetch_sub(1, Ordering::SeqCst) - 1;
        log::trace!("departed: {:#x} -> {state:#x}", state + 1);
        if state & SHARDS_MASK != 0 {
            return;
        }
        if state & STOPPED != 0 {
            self.notify_zero();
        }
        let Some(parent) = self.ancestors.first() else {
            return;
        };
        debug_assert!(state & ARRIVED != 0, "a counted child has arrived");
        // Fails if a shard arrived again in the meantime, which keeps this arrival.
        if self
            .state
            .compare_exchange(state, state & STOPPED, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let index = usize::try_from((state >> PARENT_SHARD_SHIFT) & PARENT_SHARD_MASK)
                .expect("shard index fits in usize");
            let shard = &parent.shards[index];
            if shard.depart(KIDS_ONE) {
                shard.flush(parent);
            }
        }
    }

    /// O(shards + subtree) walk summing real guards across self and all live descendants.
    pub(crate) fn guard_count_subtree(&self) -> usize {
        let local: usize = self.shards.iter().map(|shard| shard.guard_count()).sum();
        let children_sum: usize = self
            .children
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
            .map(|c| c.guard_count_subtree())
            .sum();
        local + children_sum
    }

    /// O(shards + subtree) walk collecting provenance records for all live guards in
    /// self and all live descendants.
    pub(crate) fn guard_infos_subtree(&self, out: &mut Vec<Arc<GuardInfo>>) {
        for shard in &self.shards {
            shard.guard_infos(out);
        }
        let children: Vec<Arc<Inner>> = {
            let children = self.children.lock().unwrap();
            children.iter().filter_map(Weak::upgrade).collect()
        };
        for child in children {
            child.guard_infos_subtree(out);
        }
    }
}

/// A collected node can never be stopped, so its interrupts go terminal instead (`spec.md`
/// §5.8). They hold its shards, not the node, so latch the shards, waking any that are pending,
/// and flush them so that a child stops counting toward its parent.
impl Drop for Inner {
    fn drop(&mut self) {
        for shard in &self.shards {
            shard.stop(self);
        }
    }
}
