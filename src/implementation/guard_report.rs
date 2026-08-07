use super::guard::GuardInfo;
use std::{
    collections::BTreeMap,
    fmt::{self, Display, Formatter},
    panic::Location,
    sync::Arc,
    time::Duration,
};

/// A point-in-time snapshot of the outstanding [`Guard`][crate::Guard]s in a
/// [`Swansong`][crate::Swansong]'s subtree, aggregated by creation site.
///
/// Returned by [`Swansong::guard_report`][crate::Swansong::guard_report].
///
/// The [`Display`] implementation is suitable for logging when a shutdown
/// stalls on straggling guards:
///
/// ```text
/// 3 outstanding guards:
/// 2 × src/connection.rs:88:14 (oldest 42.3s)
/// 1 × src/tasks.rs:120:9 (oldest 3.1s)
/// ```
#[derive(Debug, Clone)]
pub struct GuardReport {
    entries: Vec<GuardReportEntry>,
}

/// The outstanding [`Guard`][crate::Guard]s that share a single creation site.
#[derive(Debug, Clone, Copy)]
pub struct GuardReportEntry {
    location: &'static Location<'static>,
    count: usize,
    oldest_age: Option<Duration>,
}

impl GuardReport {
    pub(crate) fn from_infos(infos: Vec<Arc<GuardInfo>>) -> Self {
        let mut by_location: BTreeMap<&'static Location<'static>, GuardReportEntry> =
            BTreeMap::new();
        for info in infos {
            let entry = by_location
                .entry(info.location())
                .or_insert_with(|| GuardReportEntry {
                    location: info.location(),
                    count: 0,
                    oldest_age: None,
                });
            entry.count += 1;
            entry.oldest_age = entry.oldest_age.max(info.age());
        }
        let mut entries: Vec<_> = by_location.into_values().collect();
        entries.sort_by(|a, b| {
            b.oldest_age
                .cmp(&a.oldest_age)
                .then_with(|| a.location.cmp(b.location))
        });
        Self { entries }
    }

    /// The total number of outstanding guards in this report.
    #[must_use]
    pub fn guard_count(&self) -> usize {
        self.entries.iter().map(GuardReportEntry::count).sum()
    }

    /// Whether there were no outstanding guards when this report was taken.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entries in this report, one per distinct creation site with at
    /// least one outstanding guard, sorted oldest-first.
    #[must_use]
    pub fn entries(&self) -> &[GuardReportEntry] {
        &self.entries
    }

    /// Iterate over the entries in this report, oldest-first.
    pub fn iter(&self) -> std::slice::Iter<'_, GuardReportEntry> {
        self.entries.iter()
    }
}

impl GuardReportEntry {
    /// The source location at which these guards were created, or from which
    /// they were cloned.
    #[must_use]
    pub fn location(&self) -> &'static Location<'static> {
        self.location
    }

    /// The number of outstanding guards created at this location.
    #[must_use]
    pub fn count(&self) -> usize {
        self.count
    }

    /// The age of the oldest outstanding guard created at this location.
    ///
    /// `None` when the system clock is unavailable, such as under miri.
    #[must_use]
    pub fn oldest_age(&self) -> Option<Duration> {
        self.oldest_age
    }
}

impl IntoIterator for GuardReport {
    type Item = GuardReportEntry;
    type IntoIter = std::vec::IntoIter<GuardReportEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<'a> IntoIterator for &'a GuardReport {
    type Item = &'a GuardReportEntry;
    type IntoIter = std::slice::Iter<'a, GuardReportEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

impl Display for GuardReport {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let count = self.guard_count();
        if count == 0 {
            return f.write_str("no outstanding guards");
        }
        let plural = if count == 1 { "" } else { "s" };
        write!(f, "{count} outstanding guard{plural}:")?;
        for entry in &self.entries {
            write!(f, "\n{entry}")?;
        }
        Ok(())
    }
}

impl Display for GuardReportEntry {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} × {}", self.count, self.location)?;
        if let Some(age) = self.oldest_age {
            write!(f, " (oldest {age:.1?})")?;
        }
        Ok(())
    }
}
