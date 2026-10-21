use super::{
    shard::{Shard, LOCAL_ONE},
    Inner,
};
use std::{
    panic::Location,
    sync::{Arc, Weak},
    time::Duration,
};

#[cfg(not(miri))]
use std::time::Instant;

/// Provenance record for a single live [`Guard`].
///
/// The `Guard` holds the only strong reference; its shard's registry holds a `Weak`. A record
/// whose `Weak` no longer upgrades represents a dropped guard, so `Guard::drop` pays no
/// accounting cost.
#[derive(Debug)]
pub(crate) struct GuardInfo {
    location: &'static Location<'static>,
    #[cfg(not(miri))]
    created_at: Instant,
}

impl GuardInfo {
    pub(crate) fn new(location: &'static Location<'static>) -> Arc<Self> {
        Arc::new(Self {
            location,
            #[cfg(not(miri))]
            created_at: Instant::now(),
        })
    }

    pub(crate) fn location(&self) -> &'static Location<'static> {
        self.location
    }

    /// Time elapsed since the guard was created. `None` under miri, where the
    /// system clock is unavailable.
    #[allow(clippy::unnecessary_wraps)] // the Option is for the miri cfg
    pub(crate) fn age(&self) -> Option<Duration> {
        #[cfg(not(miri))]
        {
            Some(self.created_at.elapsed())
        }
        #[cfg(miri)]
        {
            None
        }
    }
}

/// Registry of provenance records for the guards created on one shard.
///
/// Entries are `Weak`: a dropped guard's entry simply stops upgrading, so
/// `Guard::drop` performs no accounting here. Dead entries are swept
/// opportunistically during registration (amortized O(1) per registration,
/// keeping memory bounded by the live guard count) and at read time.
#[derive(Debug, Default)]
pub(crate) struct GuardRegistry {
    guards: Vec<Weak<GuardInfo>>,
    sweep_threshold: usize,
}

impl GuardRegistry {
    const MIN_SWEEP_THRESHOLD: usize = 16;

    pub(crate) fn register(&mut self, guard: Weak<GuardInfo>) {
        if self.guards.len() >= self.sweep_threshold.max(Self::MIN_SWEEP_THRESHOLD) {
            self.sweep();
        }
        self.guards.push(guard);
    }

    pub(crate) fn sweep(&mut self) {
        self.guards.retain(|guard| guard.strong_count() > 0);
        self.sweep_threshold = self.guards.len() * 2;
    }

    pub(crate) fn live(&self) -> impl Iterator<Item = Arc<GuardInfo>> + '_ {
        self.guards.iter().filter_map(Weak::upgrade)
    }
}

/// The presence of a Guard delays shutdown.
///
/// Dropping all Guards in a [`Swansong`][crate::Swansong]'s subset allows the
/// [`ShutdownCompletion`][crate::ShutdownCompletion] future returned by awaiting the
/// [`Swansong`][crate::Swansong] to complete.
///
/// Each clone is treated distinctly in the guard count, and inherits the
/// creation location of the guard it was cloned from.
///
/// Each Guard records the source location it was created at (captured with
/// `#[track_caller]`). If shutdown stalls on straggling guards,
/// [`Swansong::guard_report`][crate::Swansong::guard_report] reports the
/// outstanding guards by creation site. Additionally, a Guard dropped after
/// shutdown has been initiated logs its creation site at
/// [`log::Level::Debug`], making it easy to watch stragglers resolve.
///
/// A Guard keeps its originating node's coordination state alive for the Guard's
/// lifetime, so that its drop can correctly update ancestor subtree accounting
/// even if every Swansong handle to that node has been dropped.
///
/// Two guards are [`Eq`] if they share the same [`Swansong`][crate::Swansong].
#[derive(Debug)]
pub struct Guard {
    /// The shard that counts this guard: the creating thread's, or the original's for a clone.
    shard: Arc<Shard>,
    /// A child's guard keeps the child alive (`spec.md` §3.4), so that its interrupts keep
    /// running and its departure can reach its ancestors. A root's guards don't, since that
    /// would be a shared write per guard, and nothing can observe a collected root.
    pin: Option<Arc<Inner>>,
    info: Arc<GuardInfo>,
}

impl Guard {
    #[track_caller]
    pub(crate) fn new(inner: &Arc<Inner>) -> Self {
        Self::at_location(inner, Location::caller())
    }

    fn at_location(inner: &Arc<Inner>, location: &'static Location<'static>) -> Self {
        let shard = inner.shard();
        let info = GuardInfo::new(location);
        shard.register(&info);
        shard.arrive(LOCAL_ONE, inner);
        Self {
            shard: Arc::clone(shard),
            pin: (!inner.is_root()).then(|| Arc::clone(inner)),
            info,
        }
    }

    /// A guard on an interrupt's node, through the shard it holds. If that is an arrived root
    /// shard, this needs no access to the node. Otherwise it is `None` if the node has been
    /// collected.
    #[track_caller]
    pub(crate) fn for_shard(shard: &Arc<Shard>) -> Option<Self> {
        if shard.is_root() && shard.arrive_if_arrived(LOCAL_ONE) {
            let info = GuardInfo::new(Location::caller());
            shard.register(&info);
            return Some(Self {
                shard: Arc::clone(shard),
                pin: None,
                info,
            });
        }
        let inner = shard.node()?;
        Some(Self::at_location(&inner, Location::caller()))
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.shard.is_stopped() && log::log_enabled!(log::Level::Debug) {
            if let Some(age) = self.info.age() {
                log::debug!(
                    "guard created at {} dropped during shutdown after {age:.1?}",
                    self.info.location()
                );
            } else {
                log::debug!(
                    "guard created at {} dropped during shutdown",
                    self.info.location()
                );
            }
        }
        if self.shard.depart(LOCAL_ONE) {
            if let Some(node) = &self.pin {
                self.shard.flush(node);
            } else if let Some(node) = self.shard.node() {
                self.shard.flush(&node);
            }
        }
    }
}

impl Clone for Guard {
    fn clone(&self) -> Self {
        let info = GuardInfo::new(self.info.location());
        self.shard.register(&info);
        self.shard.arrive_nonempty(LOCAL_ONE);
        Self {
            shard: Arc::clone(&self.shard),
            pin: self.pin.clone(),
            info,
        }
    }
}

impl Eq for Guard {}
impl PartialEq for Guard {
    fn eq(&self, other: &Self) -> bool {
        self.shard.same_node(&other.shard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_memory_stays_bounded_without_reads() {
        let mut registry = GuardRegistry::default();
        for _ in 0..1000 {
            let info = GuardInfo::new(Location::caller());
            registry.register(Arc::downgrade(&info));
            drop(info);
        }
        assert!(registry.guards.len() <= GuardRegistry::MIN_SWEEP_THRESHOLD);
    }

    #[test]
    fn registry_tracks_live_guards() {
        let mut registry = GuardRegistry::default();
        let infos: Vec<_> = (0..100)
            .map(|_| {
                let info = GuardInfo::new(Location::caller());
                registry.register(Arc::downgrade(&info));
                info
            })
            .collect();
        assert_eq!(registry.live().count(), 100);
        drop(infos);
        assert_eq!(registry.live().count(), 0);
    }
}
