//! The reporting engine: open each operand through its final link, read its
//! gate through that descriptor, render a gated node, and — with `-R` — walk
//! its tree through the directories the run holds open, leaving every link
//! met there alone.

use alloc::format;
use alloc::string::String;

use tairix_abi::{CapabilityId, Errno, FileStat};
use tairix_fscopy::tree::{each_node, Dir, EachNode, Node, Roots};
use tairix_util::output::Carried;

use crate::command::Command;
use crate::error::GetcapError;
use crate::io::{Gates, Output};

/// The usage banner printed by [`Command::Help`].
pub const USAGE: &str = "\
usage: getcap [-R] [--] file...

  -R, --recursive  report files and directories recursively
  -h, --help       show this message

For each file that carries a capability gate, getcap prints `path CAP_NAME`.
A file with no gate produces no output. `--` ends option parsing: every later
argument is an operand.
";

/// Run one [`Command`], reporting the capability gate of its files, each
/// opened through `roots` and its gate read through `gates`.
///
/// Each file is opened through its final link and its gate read through
/// that descriptor; a gated file is rendered as `path CAP_NAME`, and a file
/// with no gate produces no output. With `-R` a directory is reported and
/// then its tree, walked through the directories the run holds open: each
/// entry is opened by name from its own directory, and a link met there —
/// listed as one, or swapped in since — is left alone.
///
/// A failed operand is reported on `err` and the next is still reported,
/// answering `Ok(false)` at the end; every operand reported answers `Ok(true)`.
///
/// # Errors
///
/// [`GetcapError::Output`] — writing the report, the usage banner or a
/// diagnostic failed. A failure to open, read or list a node is reported
/// instead.
pub fn run<R: Roots>(
    command: Command,
    roots: &R,
    gates: &dyn Gates,
    out: &dyn Output,
    err: &dyn Output,
) -> Result<bool, GetcapError> {
    match command {
        Command::Help => out
            .write_all(USAGE.as_bytes())
            .map(|()| true)
            .map_err(GetcapError::Output),
        Command::Report { recursive, files } => {
            let mut reporting = Reporting {
                gates,
                recursive,
                out,
                carried: Carried::new(err, "getcap"),
            };
            for file in &files {
                each_node(roots, file, reporting.recursive, &mut reporting)?;
            }
            Ok(reporting.carried.clean())
        }
    }
}

/// One run's seams, and the failures it carries past.
struct Reporting<'a> {
    gates: &'a dyn Gates,
    recursive: bool,
    out: &'a dyn Output,
    carried: Carried<'a>,
}

impl<D: Dir> EachNode<D> for Reporting<'_> {
    type Error = GetcapError;

    /// Report the gate of `node`, at `path`, read through `node` itself.
    fn node(&mut self, node: &dyn Node, _: &FileStat, path: &str) -> Result<(), GetcapError> {
        let queried = self
            .gates
            .capability(node)
            .map_err(|errno| GetcapError::Query {
                path: String::from(path),
                errno,
            });
        if let Some(Some(cap)) = self.carried.absorb(queried)? {
            self.out
                .write_all(render(path, cap).as_bytes())
                .map_err(GetcapError::Output)?;
        }
        Ok(())
    }

    fn link(&mut self, _: &str) -> Result<(), GetcapError> {
        Ok(())
    }

    fn dangling(&mut self, path: &str) -> Result<(), GetcapError> {
        self.carried
            .carry(GetcapError::Dangling(String::from(path)))
    }

    fn unopened(&mut self, path: &str, errno: Errno) -> Result<(), GetcapError> {
        self.carried.carry(GetcapError::Stat {
            path: String::from(path),
            errno,
        })
    }

    fn unread(&mut self, path: &str, errno: Errno) -> Result<(), GetcapError> {
        self.carried.carry(GetcapError::Read {
            path: String::from(path),
            errno,
        })
    }
}

/// Render one report line: `path CAP_NAME\n`.
///
/// A capability assigned a canonical name in `abi-v1` renders by that name;
/// an in-range identifier `abi-v1` has not yet named (so a node stored a
/// gate the running ABI does not name) renders as `CAP_<id>` rather than
/// being dropped, so the gate is never silently hidden.
fn render(path: &str, cap: CapabilityId) -> String {
    match cap.name() {
        Some(name) => format!("{path} {name}\n"),
        None => format!("{path} CAP_{}\n", cap.as_u16()),
    }
}

#[cfg(test)]
mod tests {
    use super::{run, USAGE};
    use crate::command::parse;
    use crate::error::GetcapError;
    use crate::io::{Gates, Output};
    use alloc::boxed::Box;
    use alloc::format;
    use alloc::string::String;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use tairix_abi::{CapabilityId, Errno};
    use tairix_fscopy::fixture::{gate_of, Tree};
    use tairix_fscopy::tree::{Kind, Node};

    /// The gates the fixture keeps, read through the node held.
    struct NodeGates;

    impl Gates for NodeGates {
        fn capability(&self, node: &dyn Node) -> Result<Option<CapabilityId>, Errno> {
            gate_of(node)
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

        fn text(&self) -> String {
            String::from_utf8(self.bytes.borrow().clone()).expect("utf-8")
        }
    }

    impl Output for Recorder {
        fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
            self.bytes.borrow_mut().extend_from_slice(bytes);
            Ok(())
        }
    }

    /// The engine over `args`, answering what it reported when it did not
    /// run clean: a failed operand is reported and the next still reported.
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
            Ok(false) => Err(err.text()),
            Err(fatal) => Err(format!("getcap: {fatal}\n")),
        }
    }

    /// `failure` as the tool reports it.
    fn shown(failure: &GetcapError) -> Result<(), String> {
        Err(format!("getcap: {failure}\n"))
    }

    #[test]
    fn help_prints_the_usage_banner() {
        let tree = Tree::new();
        let out = Recorder::new();
        assert_eq!(run_args(&["--help"], &tree, &out), Ok(()));
        assert_eq!(out.text(), USAGE);
    }

    #[test]
    fn a_gated_file_is_reported_by_name() {
        let tree = Tree::new()
            .file("/f", b"")
            .gated("/f", CapabilityId::AUDIT_READ);
        let out = Recorder::new();
        assert_eq!(run_args(&["/f"], &tree, &out), Ok(()));
        assert_eq!(out.text(), "/f CAP_AUDIT_READ\n");
    }

    #[test]
    fn an_ungated_file_produces_no_output() {
        let tree = Tree::new().file("/f", b"");
        let out = Recorder::new();
        assert_eq!(run_args(&["/f"], &tree, &out), Ok(()));
        assert!(out.text().is_empty());
    }

    #[test]
    fn an_unnamed_in_range_gate_renders_numerically() {
        let cap = CapabilityId::from_raw(200).expect("in range");
        let tree = Tree::new().file("/f", b"").gated("/f", cap);
        let out = Recorder::new();
        assert_eq!(run_args(&["/f"], &tree, &out), Ok(()));
        assert_eq!(out.text(), "/f CAP_200\n");
    }

    #[test]
    fn several_files_report_only_the_gated_ones_in_order() {
        let tree = Tree::new()
            .file("/a", b"")
            .gated("/a", CapabilityId::FS_MOUNT)
            .file("/b", b"")
            .file("/c", b"")
            .gated("/c", CapabilityId::NET_RAW);
        let out = Recorder::new();
        assert_eq!(run_args(&["/a", "/b", "/c"], &tree, &out), Ok(()));
        assert_eq!(out.text(), "/a CAP_FS_MOUNT\n/c CAP_NET_RAW\n");
    }

    #[test]
    fn without_recursive_only_the_named_directory_is_reported() {
        let tree = Tree::new()
            .dir("/d")
            .gated("/d", CapabilityId::FS_MOUNT)
            .file("/d/f", b"")
            .gated("/d/f", CapabilityId::NET_RAW);
        let out = Recorder::new();
        assert_eq!(run_args(&["/d"], &tree, &out), Ok(()));
        // The child is not visited.
        assert_eq!(out.text(), "/d CAP_FS_MOUNT\n");
    }

    #[test]
    fn recursive_reports_the_directory_then_its_contents() {
        let tree = Tree::new()
            .dir("/d")
            .gated("/d", CapabilityId::FS_MOUNT)
            .file("/d/f", b"")
            .dir("/d/sub")
            .file("/d/sub/g", b"")
            .gated("/d/sub/g", CapabilityId::AUDIT_WRITE);
        let out = Recorder::new();
        assert_eq!(run_args(&["-R", "/d"], &tree, &out), Ok(()));
        // The directory's own gate first, then the deep gated file; the
        // ungated nodes contribute nothing.
        assert_eq!(out.text(), "/d CAP_FS_MOUNT\n/d/sub/g CAP_AUDIT_WRITE\n");
    }

    #[test]
    fn a_missing_operand_is_reported_and_the_next_still_reported() {
        let tree = Tree::new()
            .file("/b", b"")
            .gated("/b", CapabilityId::FS_MOUNT);
        let out = Recorder::new();
        assert_eq!(
            run_args(&["/a", "/b"], &tree, &out),
            shown(&GetcapError::Stat {
                path: String::from("/a"),
                errno: Errno::NotFound
            })
        );
        assert_eq!(out.text(), "/b CAP_FS_MOUNT\n");
    }

    #[test]
    fn a_stat_permission_error_surfaces() {
        let tree = Tree::new()
            .file("/f", b"")
            .refuse("open_node /f", Errno::PermissionDenied);
        let out = Recorder::new();
        assert_eq!(
            run_args(&["/f"], &tree, &out),
            shown(&GetcapError::Stat {
                path: String::from("/f"),
                errno: Errno::PermissionDenied
            })
        );
    }

    #[test]
    fn a_query_error_surfaces() {
        let tree = Tree::new()
            .file("/f", b"")
            .gated("/f", CapabilityId::FS_MOUNT)
            .refuse("attr_keys /f", Errno::PermissionDenied);
        let out = Recorder::new();
        assert_eq!(
            run_args(&["/f"], &tree, &out),
            shown(&GetcapError::Query {
                path: String::from("/f"),
                errno: Errno::PermissionDenied
            })
        );
    }

    #[test]
    fn a_read_dir_error_during_recursion_surfaces() {
        let tree = Tree::new()
            .file("/d/f", b"")
            .gated("/d/f", CapabilityId::NET_RAW)
            .refuse("entries /d", Errno::PermissionDenied);
        let out = Recorder::new();
        assert_eq!(
            run_args(&["-R", "/d"], &tree, &out),
            shown(&GetcapError::Read {
                path: String::from("/d"),
                errno: Errno::PermissionDenied
            })
        );
        assert!(out.text().is_empty());
    }

    #[test]
    fn an_entry_that_fails_is_reported_and_the_rest_of_its_directory_still_reported() {
        let tree = Tree::new()
            .dir("/d/a")
            .gated("/d/a", CapabilityId::FS_MOUNT)
            .file("/d/a/f", b"")
            .gated("/d/a/f", CapabilityId::NET_RAW)
            .file("/d/c/g", b"")
            .gated("/d/c/g", CapabilityId::NET_RAW)
            .file("/d/e", b"")
            .gated("/d/e", CapabilityId::AUDIT_WRITE)
            .refuse("attr_keys /d/a", Errno::PermissionDenied)
            .refuse("entries /d/c", Errno::PermissionDenied);
        let out = Recorder::new();
        let unqueried = GetcapError::Query {
            path: String::from("/d/a"),
            errno: Errno::PermissionDenied,
        };
        let unread = GetcapError::Read {
            path: String::from("/d/c"),
            errno: Errno::PermissionDenied,
        };
        assert_eq!(
            run_args(&["-R", "/d"], &tree, &out),
            Err(format!("getcap: {unqueried}\ngetcap: {unread}\n"))
        );
        assert_eq!(out.text(), "/d/a/f CAP_NET_RAW\n/d/e CAP_AUDIT_WRITE\n");
    }

    #[test]
    fn a_link_met_in_the_walk_is_left_alone() {
        let tree = Tree::new()
            .file("/d/f", b"")
            .gated("/d/f", CapabilityId::NET_RAW)
            .link("/d/l", "/out/secret")
            .file("/out/secret", b"")
            .gated("/out/secret", CapabilityId::FS_MOUNT);
        let out = Recorder::new();
        assert_eq!(run_args(&["-R", "/d"], &tree, &out), Ok(()));
        assert_eq!(out.text(), "/d/f CAP_NET_RAW\n");
    }

    #[test]
    fn a_link_named_as_an_operand_reports_what_it_names() {
        let tree = Tree::new()
            .link("/flink", "/secret")
            .file("/secret", b"")
            .gated("/secret", CapabilityId::AUDIT_READ);
        let out = Recorder::new();
        assert_eq!(run_args(&["/flink"], &tree, &out), Ok(()));
        assert_eq!(out.text(), "/flink CAP_AUDIT_READ\n");
    }

    #[test]
    fn a_dangling_operand_is_named_as_chmod_names_it() {
        let tree = Tree::new().link("/dl", "/nowhere");
        let out = Recorder::new();
        assert_eq!(
            run_args(&["/dl"], &tree, &out),
            shown(&GetcapError::Dangling(String::from("/dl")))
        );
        assert!(out.text().is_empty());
    }

    /// `/d/sub` holding an ungated file, and beside the tree a gated victim
    /// with a link to it waiting at `/planted`.
    fn beside_a_victim() -> Tree {
        Tree::new()
            .file("/d/sub/f", b"")
            .dir("/victim")
            .gated("/victim", CapabilityId::AUDIT_READ)
            .file("/victim/secret", b"")
            .gated("/victim/secret", CapabilityId::AUDIT_READ)
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

    /// Wherever among the seam calls the swap lands, no gate is read through
    /// the link: only the victim's nodes carry one, and none is reported.
    #[test]
    fn an_entry_swapped_for_a_link_mid_walk_is_never_read_through() {
        let undisturbed = beside_a_victim();
        assert_eq!(
            run_args(&["-R", "/d"], &undisturbed, &Recorder::new()),
            Ok(())
        );
        for call in 0..undisturbed.calls().len() {
            let tree = beside_a_victim();
            tree.before_call(call, Box::new(swap_sub_for_the_link));
            let out = Recorder::new();
            let ran = run(
                parse(&["-R", "/d"]).expect("valid command"),
                &tree,
                &NodeGates,
                &out,
                &Recorder::new(),
            );
            assert!(ran.is_ok(), "the swap before call {call} ended the run");
            assert!(
                out.text().is_empty(),
                "swap before call {call}: {}",
                out.text()
            );
        }
    }
}
