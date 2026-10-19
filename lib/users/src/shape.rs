//! What laying a fixed, OS-owned directory shape onto a volume needs.

use tairix_abi::driver::filesystem::{
    FilesystemRead, FilesystemSecurity, FilesystemWrite, NodeId, NodeKind, NodeSecurity,
};
use tairix_abi::driver::DriverError;

/// Why a shape could not be laid out.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ShapeError {
    /// The volume refused an operation.
    Driver(DriverError),
    /// Something other than a directory holds a name the OS itself must keep
    /// as one.
    Occupied,
}

impl From<DriverError> for ShapeError {
    fn from(err: DriverError) -> Self {
        Self::Driver(err)
    }
}

/// The operations laying out a shape needs, which every volume one is laid
/// onto offers.
pub trait ShapeTree {
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

impl<F> ShapeTree for F
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

/// The directory `name` in `dir`, created with `security` when absent, or
/// `None` when something other than a directory holds the name.
pub(crate) fn ensure_dir<T>(
    tree: &mut T,
    dir: NodeId,
    name: &str,
    security: NodeSecurity,
) -> Result<Option<NodeId>, DriverError>
where
    T: ShapeTree + ?Sized,
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

#[cfg(test)]
#[path = "shape_testkit.rs"]
pub(crate) mod testkit;
