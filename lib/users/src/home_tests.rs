use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{NodeId, NodeSecurity};
use tairix_abi::driver::DriverError;
use tairix_abi::home::{HOME_USER_FILES_DIR, USER_FILES_SUBDIRS};

use super::{provision_home_shape, HomeShapeError, HomeTree};
use crate::policy::{
    appdata_root_security, appdata_transit_security, APPDATA_ROOT, APPDATA_ROOT_PARENTS, HOME_MODE,
    HOME_SUBDIRS,
};

const UID: u32 = 1001;
const GID: u32 = 100;

struct Node {
    directory: bool,
    security: Option<NodeSecurity>,
    children: BTreeMap<String, NodeId>,
}

/// An in-memory tree whose node 1 is the home being shaped.
struct Tree {
    nodes: Vec<Node>,
    /// Refuse every create, as a full volume does.
    full: bool,
}

const HOME: NodeId = NodeId::from_raw(1);

impl Tree {
    fn new() -> Self {
        let blank = || Node {
            directory: true,
            security: None,
            children: BTreeMap::new(),
        };
        Self {
            nodes: alloc::vec![blank(), blank()],
            full: false,
        }
    }

    fn node(&self, id: NodeId) -> &Node {
        &self.nodes[usize::try_from(id.raw()).expect("an index")]
    }

    fn at(&self, path: &[&str]) -> Option<&Node> {
        let mut here = HOME;
        for name in path {
            here = *self.node(here).children.get(*name)?;
        }
        Some(self.node(here))
    }

    fn add(&mut self, dir: NodeId, name: &str, directory: bool) -> NodeId {
        let id = NodeId::from_raw(u64::try_from(self.nodes.len()).expect("small"));
        self.nodes.push(Node {
            directory,
            security: None,
            children: BTreeMap::new(),
        });
        let parent = usize::try_from(dir.raw()).expect("an index");
        self.nodes[parent].children.insert(name.to_string(), id);
        id
    }
}

impl HomeTree for Tree {
    fn child(&mut self, dir: NodeId, name: &str) -> Result<Option<(NodeId, bool)>, DriverError> {
        Ok(self
            .node(dir)
            .children
            .get(name)
            .map(|&id| (id, self.node(id).directory)))
    }

    fn create_dir(&mut self, dir: NodeId, name: &str) -> Result<NodeId, DriverError> {
        if self.full || !self.node(dir).directory {
            return Err(DriverError::NoSpace);
        }
        Ok(self.add(dir, name, true))
    }

    fn stamp(&mut self, node: NodeId, security: NodeSecurity) -> Result<(), DriverError> {
        let at = usize::try_from(node.raw()).expect("an index");
        self.nodes[at].security = Some(security);
        Ok(())
    }
}

fn private() -> NodeSecurity {
    NodeSecurity::new(HOME_MODE, UID, GID)
}

#[test]
fn a_fresh_home_gets_every_folder_owner_only_with_the_user_files_inside() {
    let mut tree = Tree::new();
    provision_home_shape(&mut tree, HOME, UID, GID).expect("shaped");

    let transit = appdata_transit_security(UID, GID).expect("transit");
    assert_eq!(tree.at(&[]).and_then(|n| n.security), Some(transit));
    for name in HOME_SUBDIRS {
        let node = tree.at(&[name]).expect("provisioned");
        assert!(node.directory, "{name}");
        let want = if APPDATA_ROOT_PARENTS.contains(&name) {
            transit
        } else {
            private()
        };
        assert_eq!(node.security, Some(want), "{name}");
    }
    for parent in APPDATA_ROOT_PARENTS {
        assert_eq!(
            tree.at(&[parent, APPDATA_ROOT]).and_then(|n| n.security),
            Some(appdata_root_security())
        );
    }
    for child in USER_FILES_SUBDIRS {
        let node = tree
            .at(&[HOME_USER_FILES_DIR, child])
            .expect("a user files folder");
        assert!(node.directory);
        assert_eq!(node.security, Some(private()), "{child}");
    }
}

/// Re-provisioning fills in only what is missing: a folder the account
/// already has keeps its record, and a file in a folder's place is neither
/// replaced nor descended into.
#[test]
fn reshaping_fills_in_the_missing_and_leaves_what_is_there() {
    let mut tree = Tree::new();
    let files = tree.add(HOME, HOME_USER_FILES_DIR, true);
    let custom = NodeSecurity::new(0o750, UID, GID);
    tree.stamp(files, custom).expect("stamped");
    tree.add(files, "Music", false);
    tree.add(HOME, "Desktop", false);

    provision_home_shape(&mut tree, HOME, UID, GID).expect("shaped");

    assert_eq!(
        tree.at(&[HOME_USER_FILES_DIR]).and_then(|n| n.security),
        Some(custom)
    );
    assert!(
        !tree
            .at(&[HOME_USER_FILES_DIR, "Music"])
            .expect("kept")
            .directory
    );
    assert!(tree
        .at(&[HOME_USER_FILES_DIR, "Pictures"])
        .is_some_and(|n| n.directory));
    let desktop = tree.at(&["Desktop"]).expect("kept");
    assert!(!desktop.directory && desktop.children.is_empty());

    // A second run changes nothing it has not already made.
    let before = tree.nodes.len();
    provision_home_shape(&mut tree, HOME, UID, GID).expect("idempotent");
    assert_eq!(tree.nodes.len(), before);
}

#[test]
fn a_per_app_data_parent_that_is_not_a_folder_is_refused() {
    let mut tree = Tree::new();
    tree.add(HOME, APPDATA_ROOT_PARENTS[0], false);
    assert_eq!(
        provision_home_shape(&mut tree, HOME, UID, GID),
        Err(HomeShapeError::Occupied)
    );

    let mut tree = Tree::new();
    let parent = tree.add(HOME, APPDATA_ROOT_PARENTS[1], true);
    tree.add(parent, APPDATA_ROOT, false);
    assert_eq!(
        provision_home_shape(&mut tree, HOME, UID, GID),
        Err(HomeShapeError::Occupied)
    );
}

#[test]
fn a_volume_that_refuses_a_create_fails_the_walk_with_its_reason() {
    let mut tree = Tree::new();
    tree.full = true;
    assert_eq!(
        provision_home_shape(&mut tree, HOME, UID, GID),
        Err(HomeShapeError::Driver(DriverError::NoSpace))
    );
}
