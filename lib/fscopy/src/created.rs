//! The destinations one run of a copying tool has put in place, so a later
//! operand naming one of them is refused rather than replacing it: in
//! `mv a/x b/x d/` the second would put `b/x` over the `a/x` just moved
//! there, whose source is already gone.

use alloc::string::String;

use tairix_abi::{Errno, FileId};
use tairix_collections::HashSet;
use tairix_hash::BuildSipHash13;

/// The nodes a run has put in place, by identity, and by path on a backing
/// that offers no identity.
///
/// Keyed, because a name can be a hostile volume's to choose.
pub struct Created {
    nodes: HashSet<FileId, BuildSipHash13>,
    names: HashSet<String, BuildSipHash13>,
}

impl Created {
    /// An empty record hashing under `keys`.
    #[must_use]
    pub const fn new(keys: BuildSipHash13) -> Self {
        Self {
            nodes: HashSet::with_hasher(keys),
            names: HashSet::with_hasher(keys),
        }
    }

    /// Remember that the run put the node `id` in place at `path`.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the record cannot grow: the caller stops
    /// rather than go on with an operand unguarded.
    pub fn note(&mut self, id: FileId, path: &str) -> Result<(), Errno> {
        let grown = if id.is_none() {
            let mut name = String::new();
            name.try_reserve_exact(path.len())
                .map_err(|_| Errno::OutOfMemory)?;
            name.push_str(path);
            self.names.try_insert(name)
        } else {
            self.nodes.try_insert(id)
        };
        grown.map(|_| ()).map_err(|_| Errno::OutOfMemory)
    }

    /// Whether the node `id` found at `path` is one the run put in place.
    #[must_use]
    pub fn holds(&self, id: FileId, path: &str) -> bool {
        if id.is_none() {
            self.names.contains(path)
        } else {
            self.nodes.contains(&id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Created;
    use tairix_abi::FileId;
    use tairix_hash::BuildSipHash13;

    const NODE: FileId = FileId {
        volume: [1; 16],
        node: 7,
    };

    #[test]
    fn a_node_is_known_by_its_identity_whatever_it_is_called() {
        let mut created = Created::new(BuildSipHash13::UNKEYED);
        assert!(!created.holds(NODE, "/d/x"));
        created.note(NODE, "/d/x").expect("room");
        assert!(
            created.holds(NODE, "/d/X"),
            "one node under another spelling"
        );
        assert!(!created.holds(FileId { node: 8, ..NODE }, "/d/x"));
    }

    #[test]
    fn a_backing_with_no_identity_is_known_by_the_path() {
        let mut created = Created::new(BuildSipHash13::UNKEYED);
        created.note(FileId::NONE, "/d/x").expect("room");
        assert!(created.holds(FileId::NONE, "/d/x"));
        assert!(!created.holds(FileId::NONE, "/d/y"));
        assert!(!created.holds(NODE, "/d/x"));
    }
}
