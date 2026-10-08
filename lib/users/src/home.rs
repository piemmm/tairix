//! The one walk that gives a home directory its fixed shape.
//!
//! Every route that provisions a home — the account-administration backing,
//! the image builder, the test fixtures — gives it the shape through
//! [`provision_home_shape`], so a seeded account and a created one can never
//! get different homes.

use tairix_abi::driver::filesystem::{
    FilesystemRead, FilesystemSecurity, FilesystemWrite, NodeId, NodeKind, NodeSecurity,
};
use tairix_abi::driver::DriverError;
use tairix_abi::home::{HOME_USER_FILES_DIR, USER_FILES_SUBDIRS};

use crate::policy::{
    appdata_root_security, appdata_transit_security, APPDATA_ROOT, APPDATA_ROOT_PARENTS, HOME_MODE,
    HOME_SUBDIRS,
};

/// Why a home could not be given its shape.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HomeShapeError {
    /// The volume refused an operation.
    Driver(DriverError),
    /// Something other than a directory holds a name the OS itself must keep
    /// as one: a per-app data parent, or the gated root inside it.
    Occupied,
}

impl From<DriverError> for HomeShapeError {
    fn from(err: DriverError) -> Self {
        Self::Driver(err)
    }
}

/// The operations giving a home its shape needs, which every volume a home is
/// provisioned on offers.
pub trait HomeTree {
    /// The child `name` of `dir` and whether it is a directory, or `None` when
    /// there is none.
    ///
    /// # Errors
    ///
    /// The volume's refusal.
    fn child(&mut self, dir: NodeId, name: &str) -> Result<Option<(NodeId, bool)>, DriverError>;

    /// Create the directory `name` in `dir`.
    ///
    /// # Errors
    ///
    /// The volume's refusal.
    fn create_dir(&mut self, dir: NodeId, name: &str) -> Result<NodeId, DriverError>;

    /// Replace `node`'s security record.
    ///
    /// # Errors
    ///
    /// The volume's refusal.
    fn stamp(&mut self, node: NodeId, security: NodeSecurity) -> Result<(), DriverError>;
}

impl<F> HomeTree for F
where
    F: FilesystemRead + FilesystemWrite + FilesystemSecurity + ?Sized,
{
    fn child(&mut self, dir: NodeId, name: &str) -> Result<Option<(NodeId, bool)>, DriverError> {
        match self.lookup(dir, name.as_bytes()) {
            Ok(node) => Ok(Some((
                node,
                self.node_info(node)?.kind == NodeKind::Directory,
            ))),
            Err(DriverError::NotFound) => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn create_dir(&mut self, dir: NodeId, name: &str) -> Result<NodeId, DriverError> {
        self.create(dir, name.as_bytes(), NodeKind::Directory)
    }

    fn stamp(&mut self, node: NodeId, security: NodeSecurity) -> Result<(), DriverError> {
        self.set_security(node, security)
    }
}

/// Give `home`, owned by `(uid, gid)`, the fixed shape: every
/// [`HOME_SUBDIRS`] folder, and every [`USER_FILES_SUBDIRS`] folder inside
/// its `UserFiles`, each created owner-only for the account.
///
/// A name already present keeps whatever it holds — the account's own data is
/// never rewritten — and a non-directory there is left alone rather than
/// descended into. The per-app data parents are the exception: their gated
/// root is owned by the app-data service, which reaches it only through a
/// search-only transit grant on every directory on the way, so the home and
/// those parents are re-stamped and the root re-asserted on every run. Those
/// records are OS shape rather than the account's data, and a home that merely
/// *exists* would otherwise hold a store nothing can reach.
///
/// # Errors
///
/// [`HomeShapeError::Occupied`] when a per-app data parent or its root is not
/// a directory, and [`HomeShapeError::Driver`] for a refusal by the volume.
/// Folders created before a failure are left: each is an empty folder of the
/// shape, harmless to the account.
pub fn provision_home_shape<T>(
    tree: &mut T,
    home: NodeId,
    uid: u32,
    gid: u32,
) -> Result<(), HomeShapeError>
where
    T: HomeTree + ?Sized,
{
    let transit = appdata_transit_security(uid, gid)?;
    let private = NodeSecurity::new(HOME_MODE, uid, gid);
    tree.stamp(home, transit)?;
    for name in HOME_SUBDIRS {
        let appdata = APPDATA_ROOT_PARENTS.contains(&name);
        let Some(node) = ensure_dir(tree, home, name, private)? else {
            if appdata {
                return Err(HomeShapeError::Occupied);
            }
            continue;
        };
        if appdata {
            tree.stamp(node, transit)?;
            ensure_appdata_root(tree, node)?;
        } else if name == HOME_USER_FILES_DIR {
            for child in USER_FILES_SUBDIRS {
                ensure_dir(tree, node, child, private)?;
            }
        }
    }
    Ok(())
}

/// The directory `name` in `dir`, created with `security` when absent, or
/// `None` when something other than a directory holds the name.
fn ensure_dir<T>(
    tree: &mut T,
    dir: NodeId,
    name: &str,
    security: NodeSecurity,
) -> Result<Option<NodeId>, DriverError>
where
    T: HomeTree + ?Sized,
{
    match tree.child(dir, name)? {
        Some((node, true)) => Ok(Some(node)),
        Some((_, false)) => Ok(None),
        None => {
            let node = tree.create_dir(dir, name)?;
            tree.stamp(node, security)?;
            Ok(Some(node))
        }
    }
}

/// Ensure `parent` holds the gated per-app data root with the record only the
/// app-data service can reach through.
///
/// A root already there is re-stamped rather than trusted: a directory of that
/// name could only have come from a principal that is not the service, and the
/// store must not be served out of one.
fn ensure_appdata_root<T>(tree: &mut T, parent: NodeId) -> Result<(), HomeShapeError>
where
    T: HomeTree + ?Sized,
{
    let root = match tree.child(parent, APPDATA_ROOT)? {
        Some((node, true)) => node,
        Some((_, false)) => return Err(HomeShapeError::Occupied),
        None => tree.create_dir(parent, APPDATA_ROOT)?,
    };
    tree.stamp(root, appdata_root_security())?;
    Ok(())
}

#[cfg(test)]
#[path = "home_tests.rs"]
mod tests;
