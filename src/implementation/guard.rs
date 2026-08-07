use super::Inner;
use std::{panic::Location, sync::Arc, time::Duration};

#[cfg(not(miri))]
use std::time::Instant;

/// Provenance record for a single live [`Guard`].
///
/// The `Guard` holds the only strong reference; the originating node's
/// registry holds a `Weak`. A record whose `Weak` no longer upgrades
/// represents a dropped guard, so `Guard::drop` pays no accounting cost.
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
/// outstanding guards by creation site.
///
/// A Guard keeps its originating node's coordination state alive for the Guard's
/// lifetime, so that its drop can correctly update ancestor subtree accounting
/// even if every Swansong handle to that node has been dropped.
///
/// Two guards are [`Eq`] if they share the same [`Swansong`][crate::Swansong].
#[derive(Debug)]
pub struct Guard {
    inner: Arc<Inner>,
    info: Arc<GuardInfo>,
}

impl Guard {
    #[track_caller]
    pub(crate) fn new(inner: &Arc<Inner>) -> Self {
        Self::at_location(inner, Location::caller())
    }

    fn at_location(inner: &Arc<Inner>, location: &'static Location<'static>) -> Self {
        let info = GuardInfo::new(location);
        inner.increment_guard(Arc::downgrade(&info));
        Self {
            inner: Arc::clone(inner),
            info,
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.inner.decrement_guard();
    }
}

impl Clone for Guard {
    fn clone(&self) -> Self {
        Self::at_location(&self.inner, self.info.location())
    }
}

impl Eq for Guard {}
impl PartialEq for Guard {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}
