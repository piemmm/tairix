//! The feature set this process was delivered, and the families resolved
//! against it on first use.
//!
//! The kernel delivers every process the migration-safe common feature set in
//! its startup vector, and `lib/rt` records it here with [`deliver`] before
//! `main` runs. A family keeps its implementation in a [`Resolved`] cell that
//! resolves against that set the first time the family is used, so no program
//! has to remember to resolve anything.
//!
//! With nothing delivered — a host test, or the kernel, which resolves its own
//! families explicitly against a set masked of the extended register state — a
//! family runs its baseline and installs nothing, so an explicit
//! [`Resolved::install`] still decides.

use tairix_sync::OnceCell;

use crate::CpuFeatureSet;

static DELIVERED: OnceCell<CpuFeatureSet> = OnceCell::new();

/// Record the feature set this process was delivered.
///
/// The first delivery stands and a later one is ignored, so a family resolved
/// against the first can never disagree with one resolved afterwards.
pub fn deliver(features: CpuFeatureSet) {
    let _ = DELIVERED.set(features);
}

/// The feature set this process was delivered, or `None` when nothing has
/// been.
#[must_use]
pub fn delivered() -> Option<CpuFeatureSet> {
    DELIVERED.get().ok().flatten().copied()
}

/// One family's implementation, resolved the first time it is used.
pub struct Resolved<T: Copy> {
    cell: OnceCell<T>,
}

impl<T: Copy> Resolved<T> {
    /// A family nothing has resolved yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cell: OnceCell::new(),
        }
    }

    /// The implementation in force: the one installed; else the one `select`
    /// picks against the delivered set, installed for every later call; else,
    /// with nothing delivered, `baseline`, installing nothing.
    ///
    /// Two threads racing the first call both run `select` and arrive at the
    /// same deterministic answer; one installs it. `select` must not reach
    /// this family itself: nothing is installed until it returns, so it would
    /// resolve again without end.
    #[inline]
    pub fn get(&self, select: impl FnOnce(CpuFeatureSet) -> T, baseline: T) -> T {
        if let Ok(Some(chosen)) = self.cell.get() {
            return *chosen;
        }
        self.resolve(delivered(), select, baseline)
    }

    /// Install `chosen` unless an implementation already is, reporting whether
    /// this call installed it: the explicit resolve of a caller that holds its
    /// own feature set.
    pub fn install(&self, chosen: T) -> bool {
        self.cell.set(chosen).is_ok()
    }

    #[cold]
    fn resolve(
        &self,
        delivered: Option<CpuFeatureSet>,
        select: impl FnOnce(CpuFeatureSet) -> T,
        baseline: T,
    ) -> T {
        let Some(features) = delivered else {
            return baseline;
        };
        let chosen = select(features);
        let _ = self.cell.set(chosen);
        chosen
    }
}

impl<T: Copy> Default for Resolved<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::Resolved;
    use crate::{CpuFeature, CpuFeatureSet};

    #[test]
    fn nothing_delivered_runs_the_baseline_and_installs_nothing() {
        let family = Resolved::<u8>::new();
        assert_eq!(family.resolve(None, |_| 7, 1), 1);
        assert!(family.install(3), "the cell is still empty");
        assert_eq!(family.get(|_| 7, 1), 3);
    }

    #[test]
    fn a_delivered_set_is_resolved_once_and_kept() {
        let family = Resolved::<u8>::new();
        let features = CpuFeatureSet::new().with(CpuFeature::Sse2);
        let chosen = family.resolve(
            Some(features),
            |set| if set.contains(CpuFeature::Sse2) { 2 } else { 1 },
            1,
        );
        assert_eq!(chosen, 2);
        assert_eq!(
            family.get(|_| 9, 1),
            2,
            "a resolved family never selects again"
        );
        assert!(!family.install(5), "nor can a later install replace it");
    }
}
