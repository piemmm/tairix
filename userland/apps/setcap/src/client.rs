//! The gate-setting engine: open each operand through its final link, set
//! the new capability gate through that descriptor, and — with `-R` — walk
//! its tree through the directories the run holds open, leaving every link
//! met there alone.

use alloc::string::String;

use tairix_abi::{CapabilityId, Errno, FileStat};
use tairix_fscopy::tree::{each_node, Dir, EachNode, Node, Roots};
use tairix_util::output::Carried;

use crate::command::Command;
use crate::error::SetcapError;
use crate::io::{Gates, Output};

/// The usage banner printed by [`Command::Help`].
pub const USAGE: &str = "\
usage: setcap [-R] [--] CAP file...

  -R, --recursive  change files and directories recursively
  -h, --help       show this message

CAP is a canonical capability name (e.g. CAP_AUDIT_READ) to install as the
file's gate, or `-` to clear the gate. `--` ends option parsing: every later
argument is an operand.
";

/// Run one [`Command`], setting the capability gate of its files, each
/// opened through `roots` and its gate set through `gates`.
///
/// Each file is opened through its final link and the new gate set through
/// that descriptor; with `-R` a directory is changed and then its tree,
/// walked through the directories the run holds open: each entry is opened
/// by name from its own directory and changed through that descriptor, and a
/// link met there — listed as one, or swapped in since — is left alone.
/// `setcap` writes nothing on success; `out` carries only the
/// [`Command::Help`] banner.
///
/// A file that cannot be opened, changed or listed is reported on `err`
/// and the rest are still changed — the rest of its directory too —
/// answering `Ok(false)` at the end; every file changed answers `Ok(true)`.
///
/// # Errors
///
/// [`SetcapError::Output`] — writing the usage banner or a diagnostic
/// failed. A failure to open, change or list a node is reported instead.
pub fn run<R: Roots>(
    command: Command,
    roots: &R,
    gates: &dyn Gates,
    out: &dyn Output,
    err: &dyn Output,
) -> Result<bool, SetcapError> {
    match command {
        Command::Help => out
            .write_all(USAGE.as_bytes())
            .map(|()| true)
            .map_err(SetcapError::Output),
        Command::Set {
            recursive,
            cap,
            files,
        } => {
            let mut setting = Setting {
                gates,
                cap,
                recursive,
                carried: Carried::new(err, "setcap"),
            };
            for file in &files {
                each_node(roots, file, setting.recursive, &mut setting)?;
            }
            Ok(setting.carried.clean())
        }
    }
}

/// One run's gate and seams, and the failures it carries past.
struct Setting<'a> {
    gates: &'a dyn Gates,
    cap: Option<CapabilityId>,
    recursive: bool,
    carried: Carried<'a>,
}

impl<D: Dir> EachNode<D> for Setting<'_> {
    type Error = SetcapError;

    /// Set the run's gate on `node`, at `path`, through `node` itself; a
    /// refusal is carried past.
    fn node(&mut self, node: &dyn Node, _: &FileStat, path: &str) -> Result<(), SetcapError> {
        let applied =
            self.gates
                .set_capability(node, self.cap)
                .map_err(|errno| SetcapError::Apply {
                    path: String::from(path),
                    errno,
                });
        self.carried.absorb(applied).map(drop)
    }

    fn link(&mut self, _: &str) -> Result<(), SetcapError> {
        Ok(())
    }

    fn dangling(&mut self, path: &str) -> Result<(), SetcapError> {
        self.carried
            .carry(SetcapError::Dangling(String::from(path)))
    }

    fn unopened(&mut self, path: &str, errno: Errno) -> Result<(), SetcapError> {
        self.carried.carry(SetcapError::Stat {
            path: String::from(path),
            errno,
        })
    }

    fn unread(&mut self, path: &str, errno: Errno) -> Result<(), SetcapError> {
        self.carried.carry(SetcapError::Read {
            path: String::from(path),
            errno,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{run, USAGE};
    use crate::command::parse;
    use crate::error::SetcapError;
    use crate::io::{Gates, Output};
    use alloc::boxed::Box;
    use alloc::format;
    use alloc::string::String;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use tairix_abi::{CapabilityId, Errno};
    use tairix_fscopy::fixture::{set_gate, Tree};
    use tairix_fscopy::tree::{Kind, Node};

    /// The gates the fixture keeps, set through the node held.
    struct NodeGates;

    impl Gates for NodeGates {
        fn set_capability(&self, node: &dyn Node, cap: Option<CapabilityId>) -> Result<(), Errno> {
            set_gate(node, cap)
        }
    }

    /// A terminal that records every byte written to it.
    struct Recorder {
        bytes: RefCell<Vec<u8>>,
    }

    impl Recorder {
        fn new() -> Self {
            Self {
                bytes: RefCell::new(Vec::new()),
            }
        }

        fn written(&self) -> Vec<u8> {
            self.bytes.borrow().clone()
        }
    }

    impl Output for Recorder {
        fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
            self.bytes.borrow_mut().extend_from_slice(bytes);
            Ok(())
        }
    }

    /// The engine over `args`, answering what it reported when it did not
    /// run clean: a failed operand is reported and the next still changed.
    fn run_args(args: &[&str], tree: &Tree, out: &Recorder) -> Result<(), String> {
        let err = Recorder::new();
        match run(
            parse(args).expect("valid command"),
            tree,
            &NodeGates,
            out,
            &err,
        ) {
            Ok(true) => Ok(()),
            Ok(false) => Err(String::from_utf8(err.written()).expect("utf-8 diagnostics")),
            Err(fatal) => Err(format!("setcap: {fatal}\n")),
        }
    }

    /// `failure` as the tool reports it.
    fn shown(failure: &SetcapError) -> Result<(), String> {
        Err(format!("setcap: {failure}\n"))
    }

    /// Every node the run set a gate on, in order.
    fn changed(tree: &Tree) -> Vec<String> {
        tree.calls()
            .iter()
            .filter_map(|call| call.strip_prefix("attr_set "))
            .map(String::from)
            .collect()
    }

    #[test]
    fn help_prints_the_usage_banner() {
        let tree = Tree::new();
        let out = Recorder::new();
        assert_eq!(run_args(&["--help"], &tree, &out), Ok(()));
        assert_eq!(out.written(), USAGE.as_bytes());
    }

    #[test]
    fn a_named_capability_installs_the_gate() {
        let tree = Tree::new().file("/f", b"");
        let out = Recorder::new();
        assert_eq!(run_args(&["CAP_AUDIT_READ", "/f"], &tree, &out), Ok(()));
        assert_eq!(tree.gate("/f"), Some(CapabilityId::AUDIT_READ));
        assert_eq!(changed(&tree), ["/f"]);
        assert!(out.written().is_empty());
    }

    #[test]
    fn the_dash_clears_an_existing_gate() {
        let tree = Tree::new()
            .file("/f", b"")
            .gated("/f", CapabilityId::FS_MOUNT);
        let out = Recorder::new();
        assert_eq!(run_args(&["-", "/f"], &tree, &out), Ok(()));
        assert_eq!(tree.gate("/f"), None);
    }

    #[test]
    fn several_files_are_all_changed() {
        let tree = Tree::new().file("/a", b"").file("/b", b"").file("/c", b"");
        let out = Recorder::new();
        assert_eq!(
            run_args(&["CAP_NET_RAW", "/a", "/b", "/c"], &tree, &out),
            Ok(())
        );
        assert_eq!(tree.gate("/a"), Some(CapabilityId::NET_RAW));
        assert_eq!(tree.gate("/b"), Some(CapabilityId::NET_RAW));
        assert_eq!(tree.gate("/c"), Some(CapabilityId::NET_RAW));
    }

    #[test]
    fn without_recursive_only_the_named_directory_changes() {
        let tree = Tree::new()
            .dir("/d")
            .file("/d/f", b"")
            .gated("/d/f", CapabilityId::FS_MOUNT);
        let out = Recorder::new();
        assert_eq!(run_args(&["CAP_NET_RAW", "/d"], &tree, &out), Ok(()));
        assert_eq!(tree.gate("/d"), Some(CapabilityId::NET_RAW));
        // The child is untouched.
        assert_eq!(tree.gate("/d/f"), Some(CapabilityId::FS_MOUNT));
    }

    #[test]
    fn recursive_changes_the_directory_then_its_contents() {
        let tree = Tree::new().file("/d/f", b"").file("/d/sub/g", b"");
        let out = Recorder::new();
        assert_eq!(
            run_args(&["-R", "CAP_AUDIT_WRITE", "/d"], &tree, &out),
            Ok(())
        );
        for path in ["/d", "/d/f", "/d/sub", "/d/sub/g"] {
            assert_eq!(tree.gate(path), Some(CapabilityId::AUDIT_WRITE), "{path}");
        }
        // Each directory is changed before its contents.
        assert_eq!(changed(&tree), ["/d", "/d/f", "/d/sub", "/d/sub/g"]);
    }

    #[test]
    fn a_missing_operand_is_reported_and_the_next_still_changed() {
        let tree = Tree::new().file("/b", b"");
        let out = Recorder::new();
        assert_eq!(
            run_args(&["CAP_NET_RAW", "/a", "/b"], &tree, &out),
            shown(&SetcapError::Stat {
                path: String::from("/a"),
                errno: Errno::NotFound
            })
        );
        assert_eq!(tree.gate("/b"), Some(CapabilityId::NET_RAW));
    }

    #[test]
    fn a_stat_permission_error_surfaces() {
        let tree = Tree::new()
            .file("/f", b"")
            .refuse("open_node /f", Errno::PermissionDenied);
        let out = Recorder::new();
        assert_eq!(
            run_args(&["CAP_NET_RAW", "/f"], &tree, &out),
            shown(&SetcapError::Stat {
                path: String::from("/f"),
                errno: Errno::PermissionDenied
            })
        );
        assert_eq!(tree.gate("/f"), None);
    }

    #[test]
    fn an_apply_error_surfaces() {
        let tree = Tree::new()
            .file("/f", b"")
            .refuse("attr_set /f", Errno::PermissionDenied);
        let out = Recorder::new();
        assert_eq!(
            run_args(&["CAP_NET_RAW", "/f"], &tree, &out),
            shown(&SetcapError::Apply {
                path: String::from("/f"),
                errno: Errno::PermissionDenied
            })
        );
        // The gate is unchanged.
        assert_eq!(tree.gate("/f"), None);
    }

    #[test]
    fn a_read_dir_error_during_recursion_surfaces() {
        let tree = Tree::new()
            .file("/d/f", b"")
            .refuse("entries /d", Errno::PermissionDenied);
        let out = Recorder::new();
        assert_eq!(
            run_args(&["-R", "CAP_NET_RAW", "/d"], &tree, &out),
            shown(&SetcapError::Read {
                path: String::from("/d"),
                errno: Errno::PermissionDenied
            })
        );
        // The directory itself was changed before the read failed.
        assert_eq!(tree.gate("/d"), Some(CapabilityId::NET_RAW));
        assert_eq!(tree.gate("/d/f"), None);
    }

    #[test]
    fn an_entry_that_fails_is_reported_and_the_rest_of_its_directory_still_changed() {
        let tree = Tree::new()
            .file("/d/a/f", b"")
            .file("/d/b", b"")
            .file("/d/c/g", b"")
            .file("/d/e", b"")
            .refuse("attr_set /d/a", Errno::PermissionDenied)
            .refuse("entries /d/c", Errno::PermissionDenied);
        let out = Recorder::new();
        let refused = SetcapError::Apply {
            path: String::from("/d/a"),
            errno: Errno::PermissionDenied,
        };
        let unread = SetcapError::Read {
            path: String::from("/d/c"),
            errno: Errno::PermissionDenied,
        };
        assert_eq!(
            run_args(&["-R", "CAP_NET_RAW", "/d"], &tree, &out),
            Err(format!("setcap: {refused}\nsetcap: {unread}\n"))
        );
        for path in ["/d", "/d/a/f", "/d/b", "/d/c", "/d/e"] {
            assert_eq!(tree.gate(path), Some(CapabilityId::NET_RAW), "{path}");
        }
        assert_eq!(tree.gate("/d/a"), None);
        assert_eq!(tree.gate("/d/c/g"), None);
    }

    #[test]
    fn a_link_met_in_the_walk_is_left_alone() {
        let tree = Tree::new()
            .file("/d/f", b"")
            .link("/d/l", "/out/secret")
            .file("/out/secret", b"");
        let out = Recorder::new();
        assert_eq!(run_args(&["-R", "CAP_NET_RAW", "/d"], &tree, &out), Ok(()));
        assert_eq!(changed(&tree), ["/d", "/d/f"]);
        assert_eq!(tree.gate("/out/secret"), None);
    }

    #[test]
    fn a_link_named_as_an_operand_changes_what_it_names() {
        let tree = Tree::new().link("/flink", "/secret").file("/secret", b"");
        let out = Recorder::new();
        assert_eq!(run_args(&["CAP_AUDIT_READ", "/flink"], &tree, &out), Ok(()));
        assert_eq!(tree.gate("/secret"), Some(CapabilityId::AUDIT_READ));
    }

    #[test]
    fn a_dangling_operand_is_named_as_chmod_names_it() {
        let tree = Tree::new().link("/dl", "/nowhere");
        let out = Recorder::new();
        assert_eq!(
            run_args(&["CAP_NET_RAW", "/dl"], &tree, &out),
            shown(&SetcapError::Dangling(String::from("/dl")))
        );
        assert!(changed(&tree).is_empty());
    }

    /// `/d/sub` holding a file, and beside the tree a victim with a link to
    /// it waiting at `/planted`.
    fn beside_a_victim() -> Tree {
        Tree::new()
            .file("/d/sub/f", b"")
            .file("/victim/secret", b"")
            .link("/planted", "/victim")
    }

    /// Swap `/d/sub`, while it is still the directory, for the link to the
    /// victim.
    fn swap_sub_for_the_link(tree: &Tree) {
        if tree.kind("/d/sub") == Some(Kind::Directory) {
            tree.rename("/d/sub", "/aside");
            tree.rename("/planted", "/d/sub");
        }
    }

    /// Wherever among the seam calls the swap lands, no gate is set through
    /// the link: the victim and what it holds keep none.
    #[test]
    fn an_entry_swapped_for_a_link_mid_walk_never_carries_the_gate() {
        let args = ["-R", "CAP_NET_RAW", "/d"];
        let undisturbed = beside_a_victim();
        assert_eq!(run_args(&args, &undisturbed, &Recorder::new()), Ok(()));
        for call in 0..undisturbed.calls().len() {
            let tree = beside_a_victim();
            tree.before_call(call, Box::new(swap_sub_for_the_link));
            let ran = run(
                parse(&args).expect("valid command"),
                &tree,
                &NodeGates,
                &Recorder::new(),
                &Recorder::new(),
            );
            assert!(ran.is_ok(), "the swap before call {call} ended the run");
            assert_eq!(tree.gate("/victim"), None, "swap before call {call}");
            assert_eq!(tree.gate("/victim/secret"), None, "swap before call {call}");
        }
    }
}
