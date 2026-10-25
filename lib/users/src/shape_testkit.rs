//! An in-memory [`ShapeTree`] for the shape walks' tests.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{NodeId, NodeSecurity};
use tairix_abi::driver::DriverError;

use super::ShapeTree;

pub(crate) struct Node {
    pub(crate) directory: bool,
    pub(crate) security: Option<NodeSecurity>,
    pub(crate) children: BTreeMap<String, NodeId>,
}

/// An in-memory tree whose node 1 is the directory being shaped.
pub(crate) struct Tree {
    pub(crate) nodes: Vec<Node>,
    /// Refuse every create, as a full volume does.
    pub(crate) full: bool,
    /// Records replaced on nodes already there.
    pub(crate) stamps: usize,
}

/// The directory being shaped.
pub(crate) const ROOT: NodeId = NodeId::from_raw(1);

impl Tree {
    pub(crate) fn new() -> Self {
        let blank = || Node {
            directory: true,
            security: None,
            children: BTreeMap::new(),
        };
        Self {
            nodes: alloc::vec![blank(), blank()],
            full: false,
            stamps: 0,
        }
    }

    pub(crate) fn node(&self, id: NodeId) -> &Node {
        &self.nodes[usize::try_from(id.raw()).expect("an index")]
    }

    pub(crate) fn at(&self, path: &[&str]) -> Option<&Node> {
        let mut here = ROOT;
        for name in path {
            here = *self.node(here).children.get(*name)?;
        }
        Some(self.node(here))
    }

    pub(crate) fn add(&mut self, dir: NodeId, name: &str, directory: bool) -> NodeId {
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

impl ShapeTree for Tree {
    fn child(&mut self, dir: NodeId, name: &str) -> Result<Option<(NodeId, bool)>, DriverError> {
        Ok(self
            .node(dir)
            .children
            .get(name)
            .map(|&id| (id, self.node(id).directory)))
    }

    fn create_dir(
        &mut self,
        dir: NodeId,
        name: &str,
        security: NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        if self.full || !self.node(dir).directory {
            return Err(DriverError::NoSpace);
        }
        let node = self.add(dir, name, true);
        let at = usize::try_from(node.raw()).expect("an index");
        self.nodes[at].security = Some(security);
        Ok(node)
    }

    fn stamp(&mut self, node: NodeId, security: NodeSecurity) -> Result<(), DriverError> {
        let at = usize::try_from(node.raw()).expect("an index");
        self.nodes[at].security = Some(security);
        self.stamps += 1;
        Ok(())
    }
}
