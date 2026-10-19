//! The directories under `/System/Settings` a service keeps its own state in.
//!
//! `/System/Settings` is the system user's, so a service writes only in a
//! directory laid out for it. The image builder and the test fixtures lay
//! them out through [`provision_settings_dirs`], so no two volumes can differ
//! in who owns which.

use tairix_abi::driver::filesystem::{NodeId, NodeSecurity};
use tairix_abi::driver_store::SystemConfigFile;
use tairix_abi::time::TIME_STATE_DIR;
use tairix_abi::SERVICE_OVERRIDES_DIR;

use crate::provision::{SERVICES_GID, SETTINGSD_UID, SYSTEM_GID, SYSTEM_UID, TIMED_UID};
use crate::record::{Gid, Uid};
use crate::shape::{ShapeError, ShapeTree};

/// The view path every [`SETTINGS_DIRS`] directory is directly beneath.
pub const SETTINGS_ROOT: &str = "/System/Settings";

/// One service's directory under [`SETTINGS_ROOT`], and whose it is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SettingsDir {
    /// The directory's view path.
    pub path: &'static str,
    /// Its permission bits.
    pub mode: u32,
    /// Its owner.
    pub uid: Uid,
    /// Its group.
    pub gid: Gid,
}

impl SettingsDir {
    /// The directory's name within [`SETTINGS_ROOT`].
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.path.rsplit('/').next().unwrap_or(self.path)
    }

    /// The record of a document in this directory: the directory's owner's,
    /// readable as far as the directory reaches. A service replaces its
    /// documents keeping their owner, and holds no authority to change one.
    #[must_use]
    pub const fn document_security(&self) -> NodeSecurity {
        NodeSecurity::new(self.mode & !0o111, self.uid.0, self.gid.0)
    }
}

/// Every service's directory under `/System/Settings`.
pub const SETTINGS_DIRS: [SettingsDir; 4] = [
    SettingsDir {
        path: SystemConfigFile::System.dir(),
        mode: 0o755,
        uid: SETTINGSD_UID,
        gid: SERVICES_GID,
    },
    // The network configuration names the machine's hardware and its
    // addresses, so only the services group may reach it.
    SettingsDir {
        path: SystemConfigFile::Network.dir(),
        mode: 0o750,
        uid: SETTINGSD_UID,
        gid: SERVICES_GID,
    },
    SettingsDir {
        path: TIME_STATE_DIR,
        mode: 0o755,
        uid: TIMED_UID,
        gid: SERVICES_GID,
    },
    SettingsDir {
        path: SERVICE_OVERRIDES_DIR,
        mode: 0o755,
        uid: SYSTEM_UID,
        gid: SYSTEM_GID,
    },
];

/// The directory holding the administrator's layer of `which`.
#[must_use]
pub const fn document_dir(which: SystemConfigFile) -> SettingsDir {
    match which {
        SystemConfigFile::System => SETTINGS_DIRS[0],
        SystemConfigFile::Network => SETTINGS_DIRS[1],
    }
}

/// The record of the directory the image's default of `which` sits in, on
/// the read-only volume: the administrator's directory's reach, owned by the
/// system user whose image writes it.
#[must_use]
pub const fn default_dir_security(which: SystemConfigFile) -> NodeSecurity {
    let dir = document_dir(which);
    NodeSecurity::new(dir.mode, SYSTEM_UID.0, dir.gid.0)
}

/// Lay out the directory each settings document's image default is planted
/// in on the read-only `/System` volume whose root is `volume`, with
/// [`default_dir_security`].
///
/// # Errors
///
/// [`ShapeError::Occupied`] when something other than a directory holds a
/// name on the way, and [`ShapeError::Driver`] for a refusal by the volume.
pub fn provision_default_dirs<T>(tree: &mut T, volume: NodeId) -> Result<(), ShapeError>
where
    T: ShapeTree + ?Sized,
{
    for which in [SystemConfigFile::System, SystemConfigFile::Network] {
        let path = which.volume_path();
        let dirs = path.rsplit_once('/').map_or("", |(dirs, _)| dirs);
        let mut node = volume;
        for name in dirs.split('/') {
            node = match tree.child(node, name)? {
                Some((found, true)) => found,
                Some((_, false)) => return Err(ShapeError::Occupied),
                None => tree.create_dir(node, name)?,
            };
        }
        tree.stamp(node, default_dir_security(which))?;
    }
    Ok(())
}

/// Lay out every [`SETTINGS_DIRS`] directory in `settings`, a volume's
/// `/System/Settings`.
///
/// One already there is re-stamped rather than trusted: its record is the
/// OS's, and one left as another principal's would let that principal write a
/// service's state.
///
/// # Errors
///
/// [`ShapeError::Occupied`] when something other than a directory holds one
/// of the names, and [`ShapeError::Driver`] for a refusal by the volume.
pub fn provision_settings_dirs<T>(tree: &mut T, settings: NodeId) -> Result<(), ShapeError>
where
    T: ShapeTree + ?Sized,
{
    for dir in SETTINGS_DIRS {
        let node = match tree.child(settings, dir.name())? {
            Some((node, true)) => node,
            Some((_, false)) => return Err(ShapeError::Occupied),
            None => tree.create_dir(settings, dir.name())?,
        };
        tree.stamp(node, NodeSecurity::new(dir.mode, dir.uid.0, dir.gid.0))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
