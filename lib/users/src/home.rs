//! The one walk that gives a home directory its fixed shape.
//!
//! Every route that provisions a home — the account-administration backing,
//! the image builder, the test fixtures — gives it the shape through
//! [`provision_home_shape`], so a seeded account and a created one can never
//! get different homes.

use tairix_abi::driver::filesystem::{NodeId, NodeSecurity};
use tairix_abi::home::{HOME_FILES_DIR, HOME_FILES_SUBDIRS, HOME_LIBRARY_DIR};
use tairix_abi::HOME_SCREENSAVER_STORE_DIR;

use crate::policy::{
    appdata_root_security, appdata_transit_security, APPDATA_ROOT, APPDATA_ROOT_PARENTS, HOME_MODE,
    HOME_SUBDIRS,
};
use crate::shape::{ensure_dir, ShapeError, ShapeTree};

/// Give `home`, owned by `(uid, gid)`, the fixed shape: every
/// [`HOME_SUBDIRS`] folder, every [`HOME_FILES_SUBDIRS`] folder inside its
/// `Home` folder, and the user's own screensaver store inside its `Library`,
/// each created owner-only for the account.
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
/// [`ShapeError::Occupied`] when a per-app data parent or its root is not
/// a directory, and [`ShapeError::Driver`] for a refusal by the volume.
/// Folders created before a failure are left: each is an empty folder of the
/// shape, harmless to the account.
pub fn provision_home_shape<T>(
    tree: &mut T,
    home: NodeId,
    uid: u32,
    gid: u32,
) -> Result<(), ShapeError>
where
    T: ShapeTree + ?Sized,
{
    let transit = appdata_transit_security(uid, gid)?;
    let private = NodeSecurity::new(HOME_MODE, uid, gid);
    tree.stamp(home, transit)?;
    for name in HOME_SUBDIRS {
        let appdata = APPDATA_ROOT_PARENTS.contains(&name);
        let Some(node) = ensure_dir(tree, home, name, private)? else {
            if appdata {
                return Err(ShapeError::Occupied);
            }
            continue;
        };
        if appdata {
            tree.stamp(node, transit)?;
            ensure_appdata_root(tree, node)?;
        }
        if name == HOME_LIBRARY_DIR {
            ensure_dir(tree, node, HOME_SCREENSAVER_STORE_DIR, private)?;
        } else if name == HOME_FILES_DIR {
            for child in HOME_FILES_SUBDIRS {
                ensure_dir(tree, node, child, private)?;
            }
        }
    }
    Ok(())
}

/// Ensure `parent` holds the gated per-app data root with the record only the
/// app-data service can reach through.
///
/// A root already there is re-stamped rather than trusted: a directory of that
/// name could only have come from a principal that is not the service, and the
/// store must not be served out of one.
fn ensure_appdata_root<T>(tree: &mut T, parent: NodeId) -> Result<(), ShapeError>
where
    T: ShapeTree + ?Sized,
{
    let root = match tree.child(parent, APPDATA_ROOT)? {
        Some((node, true)) => node,
        Some((_, false)) => return Err(ShapeError::Occupied),
        None => tree.create_dir(parent, APPDATA_ROOT)?,
    };
    tree.stamp(root, appdata_root_security())?;
    Ok(())
}

#[cfg(test)]
#[path = "home_tests.rs"]
mod tests;
