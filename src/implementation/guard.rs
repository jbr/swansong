use super::Inner;
use std::sync::{Arc, Weak};
/// The presence of a Guard delays shutdown.
///
/// Dropping all Guards allows the [`ShutdownCompletion`][crate::ShutdownCompletion] future returned by awaiting the
/// [`Swansong`][crate::Swansong] to complete.
///
/// Each clone is treated distinctly in the guard count.
///
/// Two guards are [`Eq`] if they share the same [`Swansong`][crate::Swansong].
#[derive(Debug)]
pub struct Guard(Weak<Inner>);
impl Guard {
    pub(crate) fn new(inner: &Arc<Inner>) -> Self {
        inner.increment();
        Self(Arc::downgrade(inner))
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(inner) = self.0.upgrade() {
            inner.decrement();
        }
    }
}

impl Clone for Guard {
    fn clone(&self) -> Self {
        if let Some(inner) = self.0.upgrade() {
            inner.increment();
        }
        Self(self.0.clone())
    }
}

impl Eq for Guard {}
impl PartialEq for Guard {
    fn eq(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }
}
