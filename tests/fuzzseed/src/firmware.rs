//! The pinned Raspberry Pi firmware a host test checks discovery against: the
//! one rule for where it is, shared by the tests that read it and the gate
//! that stages it for them.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The variable naming the directory that holds the pinned firmware: an
/// operator's staging, or the cache the gate's test stage fills and names.
pub const PI_FIRMWARE_ENV: &str = "TAIRIX_PI_FIRMWARE";

/// The firmware cache's directory within a target directory.
pub const PI_FIRMWARE_CACHE: &str = "pi-firmware";

/// The pinned Raspberry Pi 4 device tree's file name.
pub const PI4_DTB: &str = "bcm2711-rpi-4-b.dtb";

/// The directory cargo builds the workspace at `workspace` into, given
/// `$CARGO_TARGET_DIR`: an absolute one as it stands, a relative one under the
/// workspace, where every `cargo xtask` subprocess runs, else `target/` there.
/// The workspace pins no `build.target-dir`, so the variable is the only
/// override in play.
#[must_use]
pub fn target_dir(workspace: &Path, cargo_target_dir: Option<&OsStr>) -> PathBuf {
    match cargo_target_dir {
        Some(dir) if !dir.is_empty() => workspace.join(dir),
        _ => workspace.join("target"),
    }
}

/// The pinned Pi 4 device tree, from the directory [`PI_FIRMWARE_ENV`] names,
/// else from the cache in `workspace`'s [`target_dir`]; `None` only where
/// nothing names a directory and the cache holds none, as in an offline
/// `cargo test`.
///
/// # Panics
///
/// Where [`PI_FIRMWARE_ENV`] names a directory without the tree: a run handed
/// the firmware must test it, never pass having read nothing.
#[must_use]
pub fn pi4_dtb(workspace: &Path) -> Option<Vec<u8>> {
    let target = target_dir(workspace, std::env::var_os("CARGO_TARGET_DIR").as_deref());
    pi4_dtb_in(
        std::env::var_os(PI_FIRMWARE_ENV).as_deref(),
        &target.join(PI_FIRMWARE_CACHE),
    )
}

fn pi4_dtb_in(named: Option<&OsStr>, cache: &Path) -> Option<Vec<u8>> {
    let Some(dir) = named else {
        return std::fs::read(cache.join(PI4_DTB)).ok();
    };
    let path = Path::new(dir).join(PI4_DTB);
    Some(std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}: the firmware {PI_FIRMWARE_ENV} names lacks the pinned tree",
            path.display()
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::{pi4_dtb_in, target_dir, PI4_DTB};
    use crate::scratch::Scratch;
    use std::ffi::OsStr;
    use std::path::Path;

    /// A named directory is read in preference to a cache holding another tree.
    #[test]
    fn a_named_directory_is_read_and_nothing_else() {
        let named = Scratch::under(&std::env::temp_dir(), "tairix-firmware-named");
        std::fs::write(named.path().join(PI4_DTB), b"named").expect("a tree");
        let cache = Scratch::under(&std::env::temp_dir(), "tairix-firmware-cache");
        std::fs::write(cache.path().join(PI4_DTB), b"cached").expect("another");
        assert_eq!(
            pi4_dtb_in(Some(named.path().as_os_str()), cache.path()),
            Some(b"named".to_vec())
        );
    }

    #[test]
    #[should_panic(expected = "lacks the pinned tree")]
    fn a_named_directory_without_the_tree_fails_rather_than_skips() {
        let named = Scratch::under(&std::env::temp_dir(), "tairix-firmware-empty");
        let cache = Scratch::under(&std::env::temp_dir(), "tairix-firmware-full");
        std::fs::write(cache.path().join(PI4_DTB), b"cached").expect("a tree");
        let _ = pi4_dtb_in(Some(named.path().as_os_str()), cache.path());
    }

    #[test]
    fn unnamed_the_cache_is_read_where_it_is_filled() {
        let cache = Scratch::under(&std::env::temp_dir(), "tairix-firmware-unfilled");
        assert_eq!(pi4_dtb_in(None, cache.path()), None, "an empty checkout");
        std::fs::write(cache.path().join(PI4_DTB), b"cached").expect("a tree");
        assert_eq!(pi4_dtb_in(None, cache.path()), Some(b"cached".to_vec()));
    }

    #[test]
    fn the_target_directory_follows_cargo() {
        let workspace = Path::new("/ws");
        assert_eq!(target_dir(workspace, None), workspace.join("target"));
        assert_eq!(
            target_dir(workspace, Some(OsStr::new(""))),
            workspace.join("target")
        );
        assert_eq!(
            target_dir(workspace, Some(OsStr::new("/runner/cache/target"))),
            Path::new("/runner/cache/target")
        );
        assert_eq!(
            target_dir(workspace, Some(OsStr::new("build/out"))),
            workspace.join("build/out")
        );
    }
}
