use tairix_abi::driver::filesystem::NodeSecurity;
use tairix_abi::driver::DriverError;
use tairix_abi::driver_store::SystemConfigFile;

use super::{
    default_dir_security, document_dir, provision_default_dirs, provision_settings_dirs,
    SETTINGS_DIRS, SETTINGS_ROOT,
};
use crate::shape::testkit::{Tree, ROOT};
use crate::shape::{ShapeError, ShapeTree};
use crate::{SERVICES_GID, SETTINGSD_UID, SYSTEM_GID, SYSTEM_UID, TIMED_UID};

fn security_of(tree: &Tree, name: &str) -> Option<NodeSecurity> {
    tree.at(&[name]).and_then(|node| node.security)
}

#[test]
fn every_directory_is_directly_inside_the_settings_root() {
    for dir in SETTINGS_DIRS {
        assert_eq!(
            dir.path
                .strip_prefix(SETTINGS_ROOT)
                .and_then(|rest| rest.strip_prefix('/')),
            Some(dir.name()),
            "{dir:?}"
        );
        assert!(!dir.name().is_empty() && !dir.name().contains('/'));
    }
    for (at, one) in SETTINGS_DIRS.iter().enumerate() {
        assert!(SETTINGS_DIRS[at + 1..]
            .iter()
            .all(|other| other.path != one.path));
    }
}

#[test]
fn the_settings_service_owns_both_documents_and_only_its_group_reaches_the_network_one() {
    let mut tree = Tree::new();
    provision_settings_dirs(&mut tree, ROOT).expect("laid out");
    let leaf = |which: SystemConfigFile| {
        which
            .dir()
            .rsplit('/')
            .next()
            .expect("a directory has a name")
    };
    assert_eq!(
        security_of(&tree, leaf(SystemConfigFile::System)),
        Some(NodeSecurity::new(0o755, SETTINGSD_UID.0, SERVICES_GID.0))
    );
    assert_eq!(
        security_of(&tree, leaf(SystemConfigFile::Network)),
        Some(NodeSecurity::new(0o750, SETTINGSD_UID.0, SERVICES_GID.0))
    );
    assert_eq!(
        security_of(&tree, "Time"),
        Some(NodeSecurity::new(0o755, TIMED_UID.0, SERVICES_GID.0))
    );
    for dir in SETTINGS_DIRS {
        assert!(tree.at(&[dir.name()]).is_some_and(|node| node.directory));
    }
    assert_eq!(tree.stamps, 0, "each directory is born with its record");
}

#[test]
fn a_directory_already_there_is_restamped_and_kept() {
    let mut tree = Tree::new();
    let network = SETTINGS_DIRS[1];
    let existing = tree.add(ROOT, network.name(), true);
    tree.add(existing, "network.conf", false);
    tree.stamp(existing, NodeSecurity::new(0o777, 1000, 1000))
        .expect("stamped");

    provision_settings_dirs(&mut tree, ROOT).expect("laid out");

    let node = tree.at(&[network.name()]).expect("kept");
    assert_eq!(
        node.security,
        Some(NodeSecurity::new(
            network.mode,
            network.uid.0,
            network.gid.0
        ))
    );
    assert!(node.children.contains_key("network.conf"));
    let before = tree.nodes.len();
    provision_settings_dirs(&mut tree, ROOT).expect("idempotent");
    assert_eq!(tree.nodes.len(), before);
}

#[test]
fn a_file_in_a_directory_s_place_is_refused() {
    let mut tree = Tree::new();
    tree.add(ROOT, SETTINGS_DIRS[0].name(), false);
    assert_eq!(
        provision_settings_dirs(&mut tree, ROOT),
        Err(ShapeError::Occupied)
    );
    let mut full = Tree::new();
    full.full = true;
    assert_eq!(
        provision_settings_dirs(&mut full, ROOT),
        Err(ShapeError::Driver(DriverError::NoSpace))
    );
}

#[test]
fn each_document_s_directory_is_its_own_and_its_default_has_the_same_reach() {
    for which in [SystemConfigFile::System, SystemConfigFile::Network] {
        let dir = document_dir(which);
        assert_eq!(dir.path, which.dir(), "{which:?}");
        assert_eq!(
            default_dir_security(which),
            NodeSecurity::new(dir.mode, SYSTEM_UID.0, dir.gid.0),
            "{which:?}"
        );
    }
}

#[test]
fn a_document_belongs_to_its_directory_s_owner_and_is_never_executable() {
    for dir in SETTINGS_DIRS {
        let document = dir.document_security();
        assert_eq!((document.uid, document.gid), (dir.uid.0, dir.gid.0));
        assert_eq!(document.mode & 0o111, 0, "{}", dir.path);
        assert_eq!(document.mode | 0o111, dir.mode | 0o111, "{}", dir.path);
    }
    assert_eq!(
        document_dir(SystemConfigFile::Network)
            .document_security()
            .mode,
        0o640
    );
    assert_eq!(
        document_dir(SystemConfigFile::System)
            .document_security()
            .mode,
        0o644
    );
}

#[test]
fn each_image_default_s_directory_is_laid_out_with_its_document_s_reach() {
    let mut tree = Tree::new();
    provision_default_dirs(&mut tree, ROOT).expect("laid out");
    for which in [SystemConfigFile::System, SystemConfigFile::Network] {
        let path = which.volume_path();
        let (dirs, _) = path.rsplit_once('/').expect("a file in a directory");
        let names: alloc::vec::Vec<&str> = dirs.split('/').collect();
        let node = tree.at(&names).expect("laid out");
        assert!(node.directory);
        assert_eq!(
            node.security,
            Some(default_dir_security(which)),
            "{which:?}"
        );
        let parent = tree.at(&names[..names.len() - 1]).expect("laid out");
        assert_eq!(
            parent.security,
            Some(NodeSecurity::new(0o755, SYSTEM_UID.0, SYSTEM_GID.0)),
            "a directory made on the way is born the system user's"
        );
    }
    assert_eq!(tree.stamps, 0, "each directory is born with its record");
    let before = tree.nodes.len();
    provision_default_dirs(&mut tree, ROOT).expect("idempotent");
    assert_eq!(tree.nodes.len(), before);
}
