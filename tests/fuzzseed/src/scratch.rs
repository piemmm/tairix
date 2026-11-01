//! A scratch directory of a test run's own, for the host tests that write a
//! tree to read back.

use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// A scratch directory of this test run's own, removed with whatever it holds
/// once dropped.
pub struct Scratch(PathBuf);

impl Scratch {
    /// A directory under `base` that this run makes, private to its user: one
    /// already there, as another user could have made it first, is refused
    /// rather than used.
    ///
    /// # Panics
    ///
    /// Where the directory cannot be made: a test with no scratch of its own
    /// cannot run.
    #[must_use]
    pub fn under(base: &Path, name: &str) -> Self {
        let made = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("a clock past the epoch")
            .as_nanos();
        Self::at(named(base, name, made)).expect("a scratch directory of this run's own")
    }

    /// The scratch directory at exactly `path`, made private to its user;
    /// an error where anything is already there.
    fn at(path: PathBuf) -> std::io::Result<Self> {
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }

    /// Where the directory is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// The path under `base` for one scratch: the process id and a number drawn
/// once per call set it apart from every other in this process, however
/// coarse the clock, and `made`, the clock's reading, from one an earlier
/// process with the same id left.
fn named(base: &Path, name: &str, made: u128) -> PathBuf {
    let drawn = crate::next_in_process();
    base.join(format!("{name}-{}-{drawn}-{made}", std::process::id()))
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::{named, Scratch};
    use std::path::Path;

    #[test]
    fn two_scratches_made_in_one_tick_of_the_clock_are_named_apart() {
        let base = Path::new("/scratch");
        assert_ne!(named(base, "same", 7), named(base, "same", 7));
    }

    #[test]
    fn a_path_already_there_is_refused_rather_than_used() {
        let first = Scratch::under(&std::env::temp_dir(), "tairix-fuzzseed-scratch");
        assert!(first.path().is_dir());
        let taken = Scratch::at(first.path().to_path_buf());
        assert!(taken.is_err(), "an existing directory is never adopted");
        let link = first.path().join("link");
        std::os::unix::fs::symlink(first.path(), &link).expect("a symlink");
        assert!(
            Scratch::at(link).is_err(),
            "nor a symlink standing at the name"
        );
        let gone = first.path().to_path_buf();
        drop(first);
        assert!(!gone.exists(), "dropped with what it holds");
    }
}
