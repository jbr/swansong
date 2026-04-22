use super::Inner;
use std::sync::Arc;

/// The presence of a Guard delays shutdown.
///
/// Dropping all Guards in a [`Swansong`][crate::Swansong]'s subset allows the
/// [`ShutdownCompletion`][crate::ShutdownCompletion] future returned by awaiting the
/// [`Swansong`][crate::Swansong] to complete.
///
/// Each clone is treated distinctly in the guard count.
///
/// A Guard keeps its originating node's coordination state alive for the Guard's
/// lifetime, so that its drop can correctly update ancestor subtree accounting
/// even if every Swansong handle to that node has been dropped.
///
/// Two guards are [`Eq`] if they share the same [`Swansong`][crate::Swansong].
#[derive(Debug)]
pub struct Guard(Arc<Inner>);
impl Guard {
    pub(crate) fn new(inner: &Arc<Inner>) -> Self {
        inner.increment_guard();
        Self(Arc::clone(inner))
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.decrement_guard();
    }
}

impl Clone for Guard {
    fn clone(&self) -> Self {
        self.0.increment_guard();
        Self(Arc::clone(&self.0))
    }
}

impl Eq for Guard {}
impl PartialEq for Guard {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
