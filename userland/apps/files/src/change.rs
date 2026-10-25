//! A Properties window's change, made to the node the window describes.
//!
//! The window reads a name once and offers its changes against what it read,
//! so a change reaches the name itself, never what a link there names, and
//! only while the name still holds the node described: one swapped since
//! answers [`Errno::Stale`] and nothing changes. The filesystem is reached
//! through the tree seam it is handed, so the rule is tested on the host.

use alloc::string::String;

use tairix_abi::{Errno, FileId};
use tairix_fscopy::tree::{Node, Roots};

/// What a Properties window changes about the node it describes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Change {
    /// Its permission bits.
    Mode(u32),
    /// Its owner and group.
    Owner {
        /// The owner.
        uid: u32,
        /// The group.
        gid: u32,
    },
    /// Its extended attribute `key`, set to `value`.
    SetAttr {
        /// The attribute.
        key: String,
        /// What it is set to.
        value: String,
    },
    /// Its extended attribute, removed.
    RemoveAttr(String),
}

/// Make `change` to the node `path` names through `roots`, while it is the
/// node `described`.
///
/// # Errors
///
/// [`Errno::Stale`] for a name that holds another node now, else the open's,
/// the look's or the change's refusal.
pub fn make<R: Roots>(
    roots: &R,
    (path, described): (&str, FileId),
    change: &Change,
) -> Result<(), Errno> {
    let node = roots.open_node(path, false)?;
    if node.stat()?.id != described {
        return Err(Errno::Stale);
    }
    match change {
        Change::Mode(mode) => node.set_mode(*mode),
        Change::Owner { uid, gid } => node.set_owner(*uid, *gid),
        Change::SetAttr { key, value } => node.attr_set(key, value.as_bytes()),
        Change::RemoveAttr(key) => node.attr_remove(key),
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use alloc::string::String;

    use tairix_abi::Errno;
    use tairix_fscopy::fixture::{Tree, USER};

    use super::{make, Change};

    fn every_change() -> [Change; 4] {
        [
            Change::Mode(0o600),
            Change::Owner {
                uid: USER.0,
                gid: USER.1,
            },
            Change::SetAttr {
                key: String::from("user.note"),
                value: String::from("mine"),
            },
            Change::RemoveAttr(String::from("user.note")),
        ]
    }

    #[test]
    fn a_change_to_a_link_reaches_the_link_and_never_its_target() {
        let tree = Tree::new()
            .file("/target", b"theirs")
            .with_mode("/target", 0o640)
            .attr("/target", "user.note", b"kept")
            .link("/link", "/target");
        let link = tree.id("/link").expect("listed");
        assert_eq!(
            make(&tree, ("/link", link), &Change::Mode(0o777 ^ 0o4)),
            Ok(())
        );
        let set = Change::SetAttr {
            key: String::from("user.note"),
            value: String::from("mine"),
        };
        // The link's own attributes are its volume's business; whatever it
        // answers, the target's stay as they were.
        let _ = make(&tree, ("/link", link), &set);
        assert_eq!(tree.mode("/target"), Some(0o640));
        assert_eq!(
            tree.attr_of("/target", "user.note").as_deref(),
            Some(&b"kept"[..])
        );
    }

    #[test]
    fn a_name_swapped_since_it_was_described_is_left_alone() {
        for change in every_change() {
            let tree = Tree::new()
                .file("/a", b"described")
                .file("/swap", b"other")
                .with_mode("/swap", 0o644)
                .attr("/swap", "user.note", b"kept");
            let described = tree.id("/a").expect("listed");
            tree.rename("/swap", "/a");
            assert_eq!(
                make(&tree, ("/a", described), &change),
                Err(Errno::Stale),
                "{change:?}"
            );
            assert_eq!(tree.mode("/a"), Some(0o644), "{change:?}");
            assert_eq!(
                tree.attr_of("/a", "user.note").as_deref(),
                Some(&b"kept"[..]),
                "{change:?}"
            );
        }
    }

    #[test]
    fn a_name_swapped_for_a_link_between_the_look_and_the_change_is_never_followed() {
        let tree = Tree::new()
            .file("/a", b"described")
            .file("/victim", b"theirs")
            .with_mode("/victim", 0o600);
        let described = tree.id("/a").expect("listed");
        tree.before_line(
            "open_node /a",
            Box::new(|tree: &Tree| tree.relink("/a", "/victim")),
        );
        assert_eq!(
            make(&tree, ("/a", described), &Change::Mode(0o666)),
            Err(Errno::Stale)
        );
        assert_eq!(tree.mode("/victim"), Some(0o600));
    }

    #[test]
    fn each_change_lands_on_the_node_described() {
        let tree = Tree::new().file("/a", b"described");
        let described = tree.id("/a").expect("listed");
        for change in every_change() {
            assert_eq!(
                make(&tree, ("/a", described), &change),
                Ok(()),
                "{change:?}"
            );
        }
        assert_eq!(tree.mode("/a"), Some(0o600));
        assert_eq!(tree.attr_of("/a", "user.note"), None);
    }
}
