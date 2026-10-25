//! The destinations one run of a copying tool has put in place, so a later
//! operand naming one of them is refused rather than replacing it: in
//! `mv a/x b/x d/` the second would put `b/x` over the `a/x` just moved
//! there, whose source is already gone.

use alloc::string::String;

use tairix_abi::{Errno, FileId};
use tairix_collections::{HashMap, HashSet};
use tairix_hash::BuildSipHash13;

/// The nodes a run has put in place, each by identity with the name it was
/// put at, and by path alone on a backing that offers no identity.
///
/// A node is held at its own name under any spelling a volume's folding of
/// ASCII case gives it; another name for it — a hard link — is not, since
/// replacing that name loses nothing the run made.
///
/// Keyed, because a name can be a hostile volume's to choose.
pub struct Created {
    nodes: HashMap<FileId, String, BuildSipHash13>,
    names: HashSet<String, BuildSipHash13>,
}

impl Created {
    /// An empty record hashing under `keys`.
    #[must_use]
    pub const fn new(keys: BuildSipHash13) -> Self {
        Self {
            nodes: HashMap::with_hasher(keys),
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
        let mut name = String::new();
        name.try_reserve_exact(path.len())
            .map_err(|_| Errno::OutOfMemory)?;
        name.push_str(path);
        let grown = if id.is_none() {
            self.names.try_insert(name).map(|_| ())
        } else {
            self.nodes.try_insert(id, name).map(|_| ())
        };
        grown.map_err(|_| Errno::OutOfMemory)
    }

    /// Whether the node `id` found at `path` is one the run put in place
    /// there.
    #[must_use]
    pub fn holds(&self, id: FileId, path: &str) -> bool {
        if id.is_none() {
            self.names.contains(path)
        } else {
            self.nodes
                .get(&id)
                .is_some_and(|put| put.eq_ignore_ascii_case(path))
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
    fn a_node_is_known_at_its_name_however_its_case_is_spelt() {
        let mut created = Created::new(BuildSipHash13::UNKEYED);
        assert!(!created.holds(NODE, "/d/x"));
        created.note(NODE, "/d/x").expect("room");
        assert!(created.holds(NODE, "/d/x"));
        assert!(
            created.holds(NODE, "/d/X"),
            "one name under a folding volume's other spelling"
        );
        assert!(!created.holds(FileId { node: 8, ..NODE }, "/d/x"));
    }

    #[test]
    fn another_name_for_a_node_the_run_made_is_free_to_replace() {
        let mut created = Created::new(BuildSipHash13::UNKEYED);
        created.note(NODE, "/d/x").expect("room");
        assert!(
            !created.holds(NODE, "/d/z"),
            "a hard link the run did not make"
        );
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
