//! The mode-changing engine: open each operand through its final link, read
//! its mode and set the new one through that one descriptor, and — with
//! `-R` — walk its tree through the directories the run holds open, leaving
//! every link met there alone.

use alloc::format;
use alloc::string::String;

use tairix_abi::{Errno, FileKind, FileStat, FS_MODE_MASK};
use tairix_fscopy::tree::{each_node, untouched_link, EachNode, Node, Roots, ViewRoot};
use tairix_help::{own_short_help, HelpSource};
use tairix_util::mode::Mode;
use tairix_util::output::{Carried, Output};
use tairix_util::quote::quote;

use crate::command::{Command, Options, Verbosity};
use crate::error::ChmodError;

/// The usage banner the short-help switches print when `chmod`'s own Help
/// tree is unavailable.
pub const USAGE: &str = "\
usage: chmod [-cfRv] [--] MODE file...

  -R, --recursive       change files and directories recursively
  -c, --changes         report only files whose mode actually changed
  -v, --verbose         report every file processed
  -f, --silent, --quiet suppress most error messages
  --preserve-root       with -R, refuse to change '/'
  --no-preserve-root    treat '/' like any other directory (the default)
  -h, -?, --help        show this message

MODE is either an octal value (e.g. 644, 0755) or a comma-separated list of
symbolic clauses [ugoa]*([-+=]([rwxXst]*|[ugo]))+ or [-+=][0-7]+ (e.g. g+w,
o-rx, a=rx, u+s, g=u, =644); a clause naming no one leaves the umask's bits
alone. `--` ends option parsing: every later argument is an operand. To set a
mode beginning with `-`, write it without the dash (a-w) or end options first
(chmod -- -w f).
";

/// `chmod`'s own command word: the short-help switches render its own Help
/// document through the same engine as any other command's.
const OWN_WORD: &str = "chmod";

/// Run one [`Command`], changing the mode of its files through `roots`.
/// `locale` is the user's `LANG` preference, if set; `umask` is the
/// process's creation mask, whose bits a clause naming no field leaves
/// alone; `help` is the tool's own `Help/` tree, read by the short-help
/// switches.
///
/// Each file is opened through its final link, as GNU's `chmod` reaches an
/// operand, and its new mode — computed from the parsed [`Mode`] and the
/// file's current mode and kind — is set through the very descriptor its
/// current mode was read from. With `-R` a directory's tree is then walked
/// through the directories the run holds open: each entry is opened by name
/// from its own directory, read and changed through that descriptor, and a
/// link met there — listed as one, or swapped in since — is left alone with
/// what it names. `-v` reports every file processed on `out` and `-c` only
/// those whose mode actually changed; otherwise `chmod` writes nothing on
/// success beyond the [`Command::Help`] banner.
///
/// A file that cannot be inspected, changed or listed ([`ChmodError::Stat`],
/// [`ChmodError::Dangling`], [`ChmodError::Apply`], [`ChmodError::Read`]) is
/// reported on `err` in GNU's words — silently under `-f` — and the rest are
/// still changed, the rest of its directory too, as GNU's are, answering
/// `Ok(false)` at the end; every file changed answers `Ok(true)`.
///
/// # Errors
///
/// [`ChmodError::Output`] — writing the usage banner, a report or a
/// diagnostic failed.
pub fn run<R: Roots>(
    command: Command,
    locale: Option<&str>,
    roots: &R,
    umask: u32,
    help: &dyn HelpSource,
    out: &dyn Output,
    err: &dyn Output,
) -> Result<bool, ChmodError> {
    match command {
        Command::Help => short_help(locale, help, out).map(|()| true),
        Command::Change {
            options,
            mode,
            files,
        } => {
            let mut changing = Changing {
                roots,
                applied: Applied { mode: &mode, umask },
                options,
                out,
                // `-f` silences filesystem failures, never a dead terminal.
                carried: Carried::new(err, OWN_WORD).silenced(options.quiet),
                view_root: ViewRoot::default(),
            };
            for file in &files {
                each_node(roots, file, options.recursive, &mut changing)?;
            }
            Ok(changing.carried.clean())
        }
    }
}

/// The mode a run applies, with the umask its clauses naming no field spare.
#[derive(Clone, Copy)]
struct Applied<'a> {
    mode: &'a Mode,
    umask: u32,
}

impl Applied<'_> {
    /// The bits a node holding `current`, a directory when `dir`, is given.
    fn bits_for(self, current: u32, dir: bool) -> u32 {
        self.mode.resolve(current, dir, self.umask)
    }
}

/// Render `chmod`'s own short help (`NAME` + `SYNOPSIS` + compact `OPTIONS`)
/// from its own Help tree through the one shared engine; when no document
/// can be served (a build without the bundle's documents) the usage banner
/// stands in — the tool's own text, not fabricated help content — so `-h`
/// never fails.
fn short_help(
    locale: Option<&str>,
    help: &dyn HelpSource,
    out: &dyn Output,
) -> Result<(), ChmodError> {
    let bytes =
        own_short_help(help, locale, OWN_WORD).unwrap_or_else(|| String::from(USAGE).into_bytes());
    out.write_all(&bytes).map_err(ChmodError::Output)
}

/// One run's mode, its reporting, and the failures it carries past.
struct Changing<'a, R> {
    roots: &'a R,
    applied: Applied<'a>,
    options: Options,
    out: &'a dyn Output,
    carried: Carried<'a>,
    view_root: ViewRoot,
}

impl<R: Roots> Changing<'_, R> {
    /// Under `-v`, name a file whose mode could not be read or changed.
    fn inaccessible(&self, path: &str) -> Result<(), ChmodError> {
        if self.options.verbosity != Verbosity::All {
            return Ok(());
        }
        let line = format!("{} could not be accessed\n", quote(path));
        self.out
            .write_all(line.as_bytes())
            .map_err(ChmodError::Output)
    }
}

impl<R: Roots> EachNode<R::Dir> for Changing<'_, R> {
    type Error = ChmodError;

    /// Give `node`, at `path` and read as `stat`, the run's mode through
    /// itself, reporting per the verbosity policy; a refusal is carried past.
    fn node(&mut self, node: &dyn Node, stat: &FileStat, path: &str) -> Result<(), ChmodError> {
        let old_mode = stat.mode & FS_MODE_MASK;
        let new_mode = self
            .applied
            .bits_for(stat.mode, stat.kind == FileKind::Directory);
        let set = node.set_mode(new_mode).map_err(|errno| ChmodError::Apply {
            path: String::from(path),
            errno,
        });
        let changed = self.carried.absorb(set)?.is_some();
        report(path, old_mode, new_mode, changed, self.options, self.out)
    }

    fn link(&mut self, path: &str) -> Result<(), ChmodError> {
        if self.options.verbosity != Verbosity::All {
            return Ok(());
        }
        self.out
            .write_all(untouched_link(path).as_bytes())
            .map_err(ChmodError::Output)
    }

    fn dangling(&mut self, path: &str) -> Result<(), ChmodError> {
        self.carried
            .carry(ChmodError::Dangling(String::from(path)))?;
        self.inaccessible(path)
    }

    fn unopened(&mut self, path: &str, errno: Errno) -> Result<(), ChmodError> {
        self.carried.carry(ChmodError::Stat {
            path: String::from(path),
            errno,
        })?;
        self.inaccessible(path)
    }

    fn unread(&mut self, path: &str, errno: Errno) -> Result<(), ChmodError> {
        self.carried.carry(ChmodError::Read {
            path: String::from(path),
            errno,
        })
    }

    fn refuses(&mut self, dir: &R::Dir, path: &str) -> Result<bool, ChmodError> {
        if !self.options.preserve_root {
            return Ok(false);
        }
        let refusal = if ViewRoot::spelled(path) {
            ChmodError::PreserveRoot(String::from(path))
        } else {
            match self.view_root.holds(self.roots, dir) {
                Ok(false) => return Ok(false),
                Ok(true) => ChmodError::PreserveRoot(String::from(path)),
                Err(errno) => ChmodError::Stat {
                    path: String::from(path),
                    errno,
                },
            }
        };
        self.carried.carry(refusal)?;
        Ok(true)
    }
}

/// Report one processed file in the GNU wording, per the verbosity policy:
/// `-v` reports every file, a refused change included, and `-c` only an
/// actual change.
fn report(
    path: &str,
    old_mode: u32,
    new_mode: u32,
    applied: bool,
    options: Options,
    out: &dyn Output,
) -> Result<(), ChmodError> {
    let changed = old_mode != new_mode;
    let wanted = match options.verbosity {
        Verbosity::None => false,
        Verbosity::Changes => applied && changed,
        Verbosity::All => true,
    };
    if !wanted {
        return Ok(());
    }
    let line = if !applied {
        format!(
            "failed to change mode of {path} from {old_mode:04o} ({}) to {new_mode:04o} ({})\n",
            perm_string(old_mode),
            perm_string(new_mode),
            path = quote(path)
        )
    } else if changed {
        format!(
            "mode of {path} changed from {old_mode:04o} ({}) to {new_mode:04o} ({})\n",
            perm_string(old_mode),
            perm_string(new_mode),
            path = quote(path)
        )
    } else {
        format!(
            "mode of {path} retained as {old_mode:04o} ({})\n",
            perm_string(old_mode),
            path = quote(path)
        )
    };
    out.write_all(line.as_bytes()).map_err(ChmodError::Output)
}

/// The nine-character `rwx` rendering of `mode`'s permission bits.
fn perm_string(mode: u32) -> String {
    const PERMISSIONS: [(u32, char); 9] = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];
    let mut s = String::with_capacity(9);
    for (bit, ch) in PERMISSIONS {
        s.push(if mode & bit != 0 { ch } else { '-' });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::{run as engine_run, USAGE};
    use crate::command::parse;
    use crate::error::ChmodError;
    use alloc::boxed::Box;
    use alloc::format;
    use alloc::string::String;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use tairix_abi::Errno;
    use tairix_fscopy::fixture::Tree;
    use tairix_fscopy::tree::Kind;
    use tairix_help::{HelpSource, SourceError};
    use tairix_util::output::Output;

    /// The creation mask the runs are under unless a test says otherwise.
    const UMASK: u32 = 0o022;

    /// A Help tree with no documents at all: the short-help fallback path.
    struct NoHelp;

    impl HelpSource for NoHelp {
        fn locale_dirs(&self) -> Result<Vec<String>, SourceError> {
            Ok(Vec::new())
        }

        fn read(
            &self,
            _locale_dir: &str,
            _file_name: &str,
        ) -> Result<Option<Vec<u8>>, SourceError> {
            Ok(None)
        }
    }

    /// A Help tree holding one canonical `chmod.md` document.
    struct OneDoc;

    const DOC: &str = "## NAME\n\nchmod — change file mode bits\n\n\
                       ## SYNOPSIS\n\n`chmod [-cfRv] [--] MODE file...`\n\n\
                       ## DESCRIPTION\n\nChanges modes.\n";

    impl HelpSource for OneDoc {
        fn locale_dirs(&self) -> Result<Vec<String>, SourceError> {
            Ok(alloc::vec![String::from("en-US")])
        }

        fn read(&self, locale_dir: &str, file_name: &str) -> Result<Option<Vec<u8>>, SourceError> {
            if locale_dir == "en-US" && file_name == "chmod.md" {
                Ok(Some(DOC.as_bytes().to_vec()))
            } else {
                Ok(None)
            }
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

        fn text(&self) -> String {
            String::from_utf8(self.written()).expect("utf-8 output")
        }
    }

    impl Output for Recorder {
        fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
            self.bytes.borrow_mut().extend_from_slice(bytes);
            Ok(())
        }
    }

    /// The engine over `args` under `umask`, answering what it reported on
    /// its error stream when it did not run clean: a failed operand is
    /// reported (or, under `-f`, not) and the next still changed.
    fn run_under(args: &[&str], umask: u32, tree: &Tree, out: &Recorder) -> Result<(), String> {
        let err = Recorder::new();
        match engine_run(
            parse(args).expect("valid command"),
            None,
            tree,
            umask,
            &NoHelp,
            out,
            &err,
        ) {
            Ok(true) => Ok(()),
            Ok(false) => Err(err.text()),
            Err(fatal) => Err(format!("chmod: {fatal}\n")),
        }
    }

    /// [`run_under`] the usual mask.
    fn run_args(args: &[&str], tree: &Tree, out: &Recorder) -> Result<(), String> {
        run_under(args, UMASK, tree, out)
    }

    /// `failure` as the tool reports it.
    fn shown(failure: &ChmodError) -> Result<(), String> {
        Err(format!("chmod: {failure}\n"))
    }

    /// Every node the run set a mode on, in order.
    fn changed(tree: &Tree) -> Vec<String> {
        tree.calls()
            .iter()
            .filter_map(|call| call.strip_prefix("set_mode "))
            .map(String::from)
            .collect()
    }

    #[test]
    fn help_prints_the_usage_banner() {
        // With no Help documents available the tool's own usage banner
        // stands in — never fabricated help content.
        let tree = Tree::new();
        let out = Recorder::new();
        assert_eq!(run_args(&["--help"], &tree, &out), Ok(()));
        assert_eq!(out.written(), USAGE.as_bytes());
    }

    #[test]
    fn help_renders_the_short_help_from_the_document() {
        let tree = Tree::new();
        let out = Recorder::new();
        assert_eq!(
            engine_run(
                parse(&["-h"]).expect("valid command"),
                None,
                &tree,
                UMASK,
                &OneDoc,
                &out,
                &Recorder::new()
            ),
            Ok(true)
        );
        let written = out.text();
        assert!(
            written.contains("chmod — change file mode bits"),
            "{written}"
        );
        assert!(
            written.contains("chmod [-cfRv] [--] MODE file..."),
            "{written}"
        );
    }

    #[test]
    fn an_octal_mode_replaces_a_file_mode() {
        let tree = Tree::new().file("/f", b"").with_mode("/f", 0o600);
        let out = Recorder::new();
        assert_eq!(run_args(&["644", "/f"], &tree, &out), Ok(()));
        assert_eq!(tree.mode("/f"), Some(0o644));
        assert_eq!(changed(&tree), ["/f"]);
        assert!(out.written().is_empty());
    }

    #[test]
    fn a_symbolic_mode_transforms_the_current_mode() {
        let tree = Tree::new().file("/f", b"").with_mode("/f", 0o644);
        let out = Recorder::new();
        assert_eq!(run_args(&["g+w", "/f"], &tree, &out), Ok(()));
        assert_eq!(tree.mode("/f"), Some(0o664));
    }

    /// GNU 9.4 under umask `022`: a clause naming no field acts as `a` would,
    /// save that the bits the umask holds are not changed — silently, as GNU
    /// changes a mode given after `--`.
    #[test]
    fn a_clause_naming_no_field_spares_the_umask() {
        for (spec, current, want) in [
            ("-w", 0o777, 0o577),
            ("+w", 0o600, 0o600),
            ("+rwx", 0o600, 0o755),
            ("=r", 0o600, 0o444),
            ("a-w", 0o777, 0o555),
        ] {
            let tree = Tree::new().file("/f", b"").with_mode("/f", current);
            let out = Recorder::new();
            assert_eq!(run_args(&["--", spec, "/f"], &tree, &out), Ok(()), "{spec}");
            assert_eq!(tree.mode("/f"), Some(want), "{spec}");
        }
        let tree = Tree::new().file("/f", b"").with_mode("/f", 0);
        assert_eq!(
            run_under(&["+rwx", "/f"], 0o077, &tree, &Recorder::new()),
            Ok(())
        );
        assert_eq!(tree.mode("/f"), Some(0o700));
    }

    /// GNU 9.4: an octal mode of fewer than five digits leaves a directory's
    /// set-group-ID bit, which keeps its new entries in its group.
    #[test]
    fn a_directorys_set_id_bits_stay_unless_the_mode_names_them() {
        let tree = Tree::new()
            .dir("/shared")
            .with_mode("/shared", 0o2775)
            .file("/f", b"")
            .with_mode("/f", 0o2755);
        let out = Recorder::new();
        assert_eq!(run_args(&["755", "/shared", "/f"], &tree, &out), Ok(()));
        assert_eq!(tree.mode("/shared"), Some(0o2755));
        assert_eq!(tree.mode("/f"), Some(0o755), "a file keeps no such bit");
        assert_eq!(run_args(&["00755", "/shared"], &tree, &out), Ok(()));
        assert_eq!(tree.mode("/shared"), Some(0o755));
    }

    #[test]
    fn several_files_are_all_changed() {
        let tree = Tree::new()
            .file("/a", b"")
            .with_mode("/a", 0o600)
            .file("/b", b"")
            .with_mode("/b", 0o600)
            .file("/c", b"")
            .with_mode("/c", 0o600);
        let out = Recorder::new();
        assert_eq!(run_args(&["640", "/a", "/b", "/c"], &tree, &out), Ok(()));
        assert_eq!(tree.mode("/a"), Some(0o640));
        assert_eq!(tree.mode("/b"), Some(0o640));
        assert_eq!(tree.mode("/c"), Some(0o640));
    }

    #[test]
    fn without_recursive_only_the_named_directory_changes() {
        let tree = Tree::new().dir("/d").file("/d/f", b"");
        let out = Recorder::new();
        assert_eq!(run_args(&["700", "/d"], &tree, &out), Ok(()));
        assert_eq!(tree.mode("/d"), Some(0o700));
        // The child is untouched.
        assert_eq!(tree.mode("/d/f"), Some(0o644));
    }

    #[test]
    fn recursive_changes_the_directory_then_its_contents() {
        let tree = Tree::new()
            .dir("/d")
            .with_mode("/d", 0o700)
            .file("/d/f", b"")
            .with_mode("/d/f", 0o600)
            .dir("/d/sub")
            .with_mode("/d/sub", 0o700)
            .file("/d/sub/g", b"")
            .with_mode("/d/sub/g", 0o600);
        let out = Recorder::new();
        assert_eq!(run_args(&["-R", "go+r", "/d"], &tree, &out), Ok(()));
        assert_eq!(tree.mode("/d"), Some(0o744));
        assert_eq!(tree.mode("/d/f"), Some(0o644));
        assert_eq!(tree.mode("/d/sub"), Some(0o744));
        assert_eq!(tree.mode("/d/sub/g"), Some(0o644));
        // Each directory is changed before its contents.
        assert_eq!(changed(&tree), ["/d", "/d/f", "/d/sub", "/d/sub/g"]);
    }

    #[test]
    fn recursive_conditional_execute_uses_each_node_kind() {
        // `a+X` gives a directory execute but leaves a non-executable file.
        let tree = Tree::new()
            .dir("/d")
            .with_mode("/d", 0o644)
            .file("/d/f", b"")
            .with_mode("/d/f", 0o644);
        let out = Recorder::new();
        assert_eq!(run_args(&["-R", "a+X", "/d"], &tree, &out), Ok(()));
        assert_eq!(tree.mode("/d"), Some(0o755));
        assert_eq!(tree.mode("/d/f"), Some(0o644));
    }

    #[test]
    fn a_missing_operand_is_reported_and_the_next_still_changed() {
        // GNU 9.4: `chmod 600 missing b` names the missing one, still changes
        // `b` and exits 1.
        let tree = Tree::new().file("/b", b"").with_mode("/b", 0o600);
        let out = Recorder::new();
        assert_eq!(
            run_args(&["644", "/a", "/b"], &tree, &out),
            shown(&ChmodError::Stat {
                path: String::from("/a"),
                errno: Errno::NotFound
            })
        );
        assert_eq!(tree.mode("/b"), Some(0o644));
    }

    #[test]
    fn a_stat_permission_error_surfaces() {
        let tree = Tree::new()
            .file("/f", b"")
            .with_mode("/f", 0o600)
            .refuse("stat /f", Errno::PermissionDenied);
        let out = Recorder::new();
        assert_eq!(
            run_args(&["644", "/f"], &tree, &out),
            shown(&ChmodError::Stat {
                path: String::from("/f"),
                errno: Errno::PermissionDenied
            })
        );
        assert_eq!(tree.mode("/f"), Some(0o600));
    }

    #[test]
    fn an_apply_error_surfaces() {
        let tree = Tree::new()
            .file("/f", b"")
            .with_mode("/f", 0o600)
            .refuse("set_mode /f", Errno::PermissionDenied);
        let out = Recorder::new();
        assert_eq!(
            run_args(&["644", "/f"], &tree, &out),
            shown(&ChmodError::Apply {
                path: String::from("/f"),
                errno: Errno::PermissionDenied
            })
        );
        // The mode is unchanged.
        assert_eq!(tree.mode("/f"), Some(0o600));
    }

    #[test]
    fn a_read_dir_error_during_recursion_surfaces() {
        let tree = Tree::new()
            .dir("/d")
            .with_mode("/d", 0o755)
            .file("/d/f", b"")
            .with_mode("/d/f", 0o600)
            .refuse("entries /d", Errno::PermissionDenied);
        let out = Recorder::new();
        assert_eq!(
            run_args(&["-R", "700", "/d"], &tree, &out),
            shown(&ChmodError::Read {
                path: String::from("/d"),
                errno: Errno::PermissionDenied
            })
        );
        // The directory itself was changed before the read failed.
        assert_eq!(tree.mode("/d"), Some(0o700));
        assert_eq!(tree.mode("/d/f"), Some(0o600));
    }

    #[test]
    fn an_entry_that_fails_is_reported_and_the_rest_of_its_directory_still_changed() {
        let tree = Tree::new()
            .dir("/d")
            .with_mode("/d", 0o700)
            .dir("/d/a")
            .with_mode("/d/a", 0o700)
            .file("/d/a/f", b"")
            .with_mode("/d/a/f", 0o600)
            .file("/d/b", b"")
            .with_mode("/d/b", 0o600)
            .dir("/d/c")
            .with_mode("/d/c", 0o700)
            .file("/d/c/g", b"")
            .with_mode("/d/c/g", 0o600)
            .file("/d/e", b"")
            .with_mode("/d/e", 0o600)
            .refuse("set_mode /d/a", Errno::PermissionDenied)
            .refuse("entries /d/c", Errno::PermissionDenied);
        let out = Recorder::new();
        let refused = ChmodError::Apply {
            path: String::from("/d/a"),
            errno: Errno::PermissionDenied,
        };
        let unread = ChmodError::Read {
            path: String::from("/d/c"),
            errno: Errno::PermissionDenied,
        };
        assert_eq!(
            run_args(&["-R", "go+r", "/d"], &tree, &out),
            Err(format!("chmod: {refused}\nchmod: {unread}\n"))
        );
        assert_eq!(tree.mode("/d/a"), Some(0o700));
        assert_eq!(tree.mode("/d/a/f"), Some(0o644));
        assert_eq!(tree.mode("/d/b"), Some(0o644));
        assert_eq!(tree.mode("/d/c"), Some(0o744));
        assert_eq!(tree.mode("/d/c/g"), Some(0o600));
        assert_eq!(tree.mode("/d/e"), Some(0o644));
    }

    #[test]
    fn preserve_root_refuses_the_view_root_however_reached() {
        let volume = "id::b7f2e4e6-8d7a-4ef8-a13e-d3b84d4e8001";
        let tree = Tree::new()
            .file("/f", b"")
            .with_mode("/f", 0o600)
            .named_root(volume, "/");
        let by_volume = format!("{volume}/");
        for spelled in ["/", "//", by_volume.as_str()] {
            let out = Recorder::new();
            let refusal = ChmodError::PreserveRoot(String::from(spelled));
            assert_eq!(
                run_args(&["-R", "--preserve-root", "go+r", spelled], &tree, &out),
                shown(&refusal),
                "{spelled}"
            );
            assert!(changed(&tree).is_empty(), "{spelled}");
        }
        assert_eq!(
            shown(&ChmodError::PreserveRoot(by_volume.clone())),
            Err(format!(
                "chmod: it is dangerous to operate recursively on '{by_volume}' (same as '/')\n\
                 chmod: use --no-preserve-root to override this failsafe\n"
            ))
        );
        let out = Recorder::new();
        assert_eq!(
            run_args(
                &["-R", "--preserve-root", "--no-pres", "go+r", "/"],
                &tree,
                &out
            ),
            Ok(())
        );
        assert_eq!(
            tree.mode("/f"),
            Some(0o644),
            "the default is GNU's: no failsafe"
        );
    }

    #[test]
    fn a_directory_swapped_after_its_change_is_not_gone_into() {
        let tree = Tree::new()
            .dir("/d/sub")
            .file("/d/sub/f", b"")
            .with_mode("/d/sub/f", 0o600)
            .dir("/other")
            .file("/other/g", b"")
            .with_mode("/other/g", 0o600);
        tree.before_line(
            "open_dir /d/sub",
            Box::new(|tree: &Tree| {
                tree.rename("/d/sub", "/d/old");
                tree.rename("/other", "/d/sub");
            }),
        );
        let out = Recorder::new();
        let unread = ChmodError::Read {
            path: String::from("/d/sub"),
            errno: Errno::Stale,
        };
        assert_eq!(run_args(&["-R", "go+r", "/d"], &tree, &out), shown(&unread));
        assert_eq!(
            tree.mode("/d/sub/g"),
            Some(0o600),
            "what came in is untouched"
        );
        assert_eq!(tree.mode("/d/old/f"), Some(0o600));
    }

    #[test]
    fn verbose_names_an_entry_that_could_not_be_reached_as_gnu_does() {
        let tree = Tree::new()
            .file("/d/f", b"")
            .refuse("open_node /d/f", Errno::PermissionDenied);
        let out = Recorder::new();
        let refused = ChmodError::Stat {
            path: String::from("/d/f"),
            errno: Errno::PermissionDenied,
        };
        assert_eq!(
            run_args(&["-Rv", "755", "/d"], &tree, &out),
            shown(&refused)
        );
        assert!(
            out.text().ends_with("'/d/f' could not be accessed\n"),
            "{}",
            out.text()
        );
    }

    /// A tree holding links to a directory and a file beside it.
    fn linking_out() -> Tree {
        Tree::new()
            .dir("/d")
            .with_mode("/d", 0o700)
            .link("/d/dlink", "/out")
            .link("/d/flink", "/out/secret")
            .dir("/out")
            .with_mode("/out", 0o700)
            .file("/out/secret", b"")
            .with_mode("/out/secret", 0o600)
    }

    #[test]
    fn a_link_met_in_the_walk_is_left_alone_with_what_it_names() {
        let tree = linking_out();
        let out = Recorder::new();
        assert_eq!(run_args(&["-R", "-v", "go+r", "/d"], &tree, &out), Ok(()));
        assert_eq!(tree.mode("/out"), Some(0o700));
        assert_eq!(tree.mode("/out/secret"), Some(0o600));
        assert_eq!(
            out.text(),
            "mode of '/d' changed from 0700 (rwx------) to 0744 (rwxr--r--)\n\
             neither symbolic link '/d/dlink' nor referent has been changed\n\
             neither symbolic link '/d/flink' nor referent has been changed\n"
        );
        // `-c` reports changes only, and a link left alone is none.
        let tree = linking_out();
        let out = Recorder::new();
        assert_eq!(run_args(&["-R", "-c", "go+r", "/d"], &tree, &out), Ok(()));
        assert_eq!(
            out.text(),
            "mode of '/d' changed from 0700 (rwx------) to 0744 (rwxr--r--)\n"
        );
    }

    #[test]
    fn a_link_named_as_an_operand_changes_what_it_names() {
        let tree = Tree::new()
            .link("/flink", "/secret")
            .file("/secret", b"")
            .with_mode("/secret", 0o600);
        let out = Recorder::new();
        assert_eq!(run_args(&["go+r", "/flink"], &tree, &out), Ok(()));
        assert_eq!(tree.mode("/secret"), Some(0o644));
    }

    #[test]
    fn an_operand_that_cannot_be_reached_is_named_as_gnu_names_it() {
        let tree = Tree::new().link("/dl", "/nowhere");
        let out = Recorder::new();
        assert_eq!(
            run_args(&["-v", "600", "/dl", "/missing"], &tree, &out),
            Err(format!(
                "chmod: {}\nchmod: {}\n",
                ChmodError::Dangling(String::from("/dl")),
                ChmodError::Stat {
                    path: String::from("/missing"),
                    errno: Errno::NotFound,
                }
            ))
        );
        assert_eq!(
            out.text(),
            "'/dl' could not be accessed\n'/missing' could not be accessed\n"
        );
    }

    /// `/d` holding a file and a directory, and beside it a victim with a
    /// link to it and to its file waiting outside the tree.
    fn beside_a_victim() -> Tree {
        Tree::new()
            .dir("/d")
            .with_mode("/d", 0o700)
            .file("/d/f", b"")
            .with_mode("/d/f", 0o600)
            .dir("/d/sub")
            .with_mode("/d/sub", 0o700)
            .file("/d/sub/g", b"")
            .with_mode("/d/sub/g", 0o600)
            .dir("/victim")
            .with_mode("/victim", 0o700)
            .file("/victim/secret", b"")
            .with_mode("/victim/secret", 0o600)
            .link("/to_dir", "/victim")
            .link("/to_file", "/victim/secret")
    }

    /// Swap `/d/sub` and `/d/f`, each while it is still what it was, for the
    /// links to the victim.
    fn swap_both_for_links(tree: &Tree) {
        if tree.kind("/d/sub") == Some(Kind::Directory) {
            tree.rename("/d/sub", "/sub_aside");
            tree.rename("/to_dir", "/d/sub");
        }
        if tree.kind("/d/f") == Some(Kind::File) {
            tree.rename("/d/f", "/f_aside");
            tree.rename("/to_file", "/d/f");
        }
    }

    /// Wherever among the seam calls the swap lands — between a node's read
    /// and its change included — the change never reaches what a link names.
    #[test]
    fn an_entry_swapped_for_a_link_mid_walk_never_carries_the_change() {
        let args = ["-R", "go+r", "/d"];
        let undisturbed = beside_a_victim();
        assert_eq!(run_args(&args, &undisturbed, &Recorder::new()), Ok(()));
        for call in 0..undisturbed.calls().len() {
            let tree = beside_a_victim();
            tree.before_call(call, Box::new(swap_both_for_links));
            let ran = engine_run(
                parse(&args).expect("valid command"),
                None,
                &tree,
                UMASK,
                &NoHelp,
                &Recorder::new(),
                &Recorder::new(),
            );
            assert!(ran.is_ok(), "the swap before call {call} ended the run");
            assert_eq!(tree.mode("/victim"), Some(0o700), "swap before call {call}");
            assert_eq!(
                tree.mode("/victim/secret"),
                Some(0o600),
                "swap before call {call}"
            );
        }
    }

    /// A name's control bytes are escaped where GNU escapes them, never
    /// written raw to the terminal.
    #[test]
    fn a_report_names_its_file_escaped() {
        let tree = Tree::new().file("/f\u{1b}", b"");
        let out = Recorder::new();
        assert_eq!(run_args(&["-v", "644", "/f\u{1b}"], &tree, &out), Ok(()));
        assert_eq!(
            out.written(),
            b"mode of '/f'$'\\033' retained as 0644 (rw-r--r--)\n"
        );
    }

    #[test]
    fn verbose_reports_changed_and_retained_in_gnu_wording() {
        let tree = Tree::new().file("/f", b"");
        let out = Recorder::new();
        assert_eq!(run_args(&["-v", "644", "/f"], &tree, &out), Ok(()));
        assert_eq!(
            out.written(),
            b"mode of '/f' retained as 0644 (rw-r--r--)\n"
        );
        let out = Recorder::new();
        assert_eq!(run_args(&["-v", "664", "/f"], &tree, &out), Ok(()));
        assert_eq!(
            out.written(),
            b"mode of '/f' changed from 0644 (rw-r--r--) to 0664 (rw-rw-r--)\n"
        );
    }

    #[test]
    fn verbose_reports_a_refused_change_after_its_diagnostic_as_gnu_does() {
        let tree = Tree::new()
            .file("/f", b"")
            .refuse("set_mode /f", Errno::PermissionDenied);
        let refused = ChmodError::Apply {
            path: String::from("/f"),
            errno: Errno::PermissionDenied,
        };
        let out = Recorder::new();
        assert_eq!(run_args(&["-v", "600", "/f"], &tree, &out), shown(&refused));
        assert_eq!(
            out.written(),
            b"failed to change mode of '/f' from 0644 (rw-r--r--) to 0600 (rw-------)\n"
        );
        let out = Recorder::new();
        assert_eq!(
            run_args(&["-v", "-f", "600", "/f"], &tree, &out),
            Err(String::new())
        );
        assert_eq!(
            out.written(),
            b"failed to change mode of '/f' from 0644 (rw-r--r--) to 0600 (rw-------)\n"
        );
        let out = Recorder::new();
        assert!(run_args(&["-c", "600", "/f"], &tree, &out).is_err());
        assert!(out.written().is_empty(), "-c reports no refused change");
    }

    #[test]
    fn changes_reports_only_an_actual_change() {
        let tree = Tree::new().file("/f", b"");
        let out = Recorder::new();
        // The mode is already 644: `-c` reports nothing.
        assert_eq!(run_args(&["-c", "644", "/f"], &tree, &out), Ok(()));
        assert!(out.written().is_empty());
        let out = Recorder::new();
        assert_eq!(run_args(&["-c", "600", "/f"], &tree, &out), Ok(()));
        assert_eq!(
            out.written(),
            b"mode of '/f' changed from 0644 (rw-r--r--) to 0600 (rw-------)\n"
        );
    }

    #[test]
    fn quiet_suppresses_the_failure_but_still_fails_the_run() {
        let tree = Tree::new().file("/b", b"").with_mode("/b", 0o600);
        let out = Recorder::new();
        // `/a` is missing: `-f` says nothing, `/b` is still changed, and the
        // run fails.
        assert_eq!(
            run_args(&["-f", "644", "/a", "/b"], &tree, &out),
            Err(String::new())
        );
        assert_eq!(tree.mode("/b"), Some(0o644));
        assert!(out.written().is_empty());
    }
}
