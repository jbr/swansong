use event_listener::{Event, EventListener, IntoNotification};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc, Mutex, Weak,
};

/// Packed subset nonemptiness: low 32 bits = `local_guards`, high 32 bits = `nonempty_kids`.
/// The subset (this node + descendants) is nonempty iff `packed != 0`.
const LOCAL_ONE: u64 = 1;
const KIDS_ONE: u64 = 1 << 32;
const LOCAL_MASK: u64 = 0xFFFF_FFFF;

#[derive(Debug, Default)]
pub(crate) struct Inner {
    stop_event: Event,
    zero_event: Event,
    packed: AtomicU64,
    stopped: AtomicBool,
    swansong_count: AtomicUsize,
    /// Strong refs to every ancestor, deepest first: `[parent, grandparent, ..., root]`.
    /// Empty for a root. Strong (not Weak) so that a guard's decrement walk always
    /// has valid ancestors to update, even if every user-held Swansong to an
    /// intermediate ancestor has been dropped.
    ancestors: Vec<Arc<Inner>>,
    children: Mutex<Vec<Weak<Inner>>>,
}

impl Inner {
    pub(crate) fn new_root() -> Arc<Self> {
        Arc::new(Self {
            swansong_count: AtomicUsize::new(1),
            ..Self::default()
        })
    }

    pub(crate) fn new_child(parent: &Arc<Self>) -> Arc<Self> {
        let mut ancestors = Vec::with_capacity(parent.ancestors.len() + 1);
        ancestors.push(Arc::clone(parent));
        ancestors.extend(parent.ancestors.iter().cloned());

        let inner = Arc::new(Self {
            swansong_count: AtomicUsize::new(1),
            ancestors,
            ..Self::default()
        });
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
        self.stopped.load(Ordering::SeqCst)
    }

    pub(crate) fn is_stopped_relaxed(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.packed.load(Ordering::SeqCst) == 0
    }

    pub(crate) fn is_zero_relaxed(&self) -> bool {
        self.packed.load(Ordering::Relaxed) == 0
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

    pub(crate) fn swansong_clone(&self) {
        self.swansong_count.fetch_add(1, Ordering::SeqCst);
    }

    /// Decrement the swansong handle count. Returns `true` if this was the last handle.
    pub(crate) fn swansong_drop(&self) -> bool {
        self.swansong_count.fetch_sub(1, Ordering::SeqCst) == 1
    }

    pub(crate) fn stop(&self) {
        log::trace!("intending to stop");

        if self.stopped.swap(true, Ordering::SeqCst) {
            log::trace!("was already stopped");
            return;
        }

        log::trace!("stopped");
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

        if self.is_zero() {
            self.zero_event.notify(usize::MAX.relaxed());
        }
    }

    /// Called by `Guard::new`. Increments this node's local guard count and, if the
    /// subset transitioned from empty to nonempty, propagates the transition
    /// up the ancestor chain.
    pub(crate) fn increment_guard(&self) {
        let prev = self.packed.fetch_add(LOCAL_ONE, Ordering::SeqCst);
        log::trace!("incremented local: {prev:#x} -> {:#x}", prev + LOCAL_ONE);
        if prev == 0 {
            for ancestor in &self.ancestors {
                let prev = ancestor.packed.fetch_add(KIDS_ONE, Ordering::SeqCst);
                if prev != 0 {
                    break;
                }
            }
        }
    }

    /// Called by `Guard::drop`. Decrements this node's local guard count and, if the
    /// subset transitioned from nonempty to empty, notifies this node's zero event
    /// (if stopped) and propagates the transition up the ancestor chain.
    pub(crate) fn decrement_guard(&self) {
        let prev = self.packed.fetch_sub(LOCAL_ONE, Ordering::SeqCst);
        let new = prev - LOCAL_ONE;
        log::trace!("decremented local: {prev:#x} -> {new:#x}");
        if new == 0 {
            if self.is_stopped() {
                self.zero_event.notify(usize::MAX.relaxed());
            }
            for ancestor in &self.ancestors {
                let prev = ancestor.packed.fetch_sub(KIDS_ONE, Ordering::SeqCst);
                let new = prev - KIDS_ONE;
                if new == 0 {
                    if ancestor.is_stopped() {
                        ancestor.zero_event.notify(usize::MAX.relaxed());
                    }
                } else {
                    break;
                }
            }
        }
    }

    /// O(subtree) walk summing real guards across self and all live descendants.
    pub(crate) fn guard_count_subtree(&self) -> usize {
        let local = usize::try_from(self.packed.load(Ordering::Relaxed) & LOCAL_MASK)
            .expect("local_guards fits in usize");
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
}
