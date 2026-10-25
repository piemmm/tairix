//! Tests for the move engine, driven against the shared in-memory tree and a
//! recording output.

use super::{run as engine_run, Seams, USAGE};
use crate::command::{Clobber, Command, Options, TargetMode};
use crate::error::MvError;
use crate::io::{Kind, Output, Prompt, Staging};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use tairix_abi::Errno;
use tairix_fscopy::fixture::{Tree, USER};
use tairix_hash::BuildSipHash13;
use tairix_help::{HelpSource, SourceError};

/// A prompt no non-interactive run may ever reach.
struct NeverAsked;

impl Prompt for NeverAsked {
    fn confirm(&self, question: &str) -> Result<bool, Errno> {
        panic!("unexpected prompt: {question}");
    }
}

/// A scripted prompt: answers in order, recording every question; an
/// exhausted script fails the read.
struct Answers {
    replies: RefCell<Vec<bool>>,
    asked: RefCell<Vec<String>>,
}

impl Answers {
    fn new(replies: &[bool]) -> Self {
        Self {
            replies: RefCell::new(replies.to_vec()),
            asked: RefCell::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<String> {
        self.asked.borrow().clone()
    }
}

impl Prompt for Answers {
    fn confirm(&self, question: &str) -> Result<bool, Errno> {
        self.asked.borrow_mut().push(question.to_string());
        let mut replies = self.replies.borrow_mut();
        if replies.is_empty() {
            return Err(Errno::NotFound);
        }
        Ok(replies.remove(0))
    }
}

/// A Help tree with no documents at all: the short-help fallback path.
struct NoHelp;

impl HelpSource for NoHelp {
    fn locale_dirs(&self) -> Result<Vec<String>, SourceError> {
        Ok(Vec::new())
    }

    fn read(&self, _locale_dir: &str, _file_name: &str) -> Result<Option<Vec<u8>>, SourceError> {
        Ok(None)
    }
}

/// A Help tree holding one canonical `mv.md` document.
struct OneDoc;

const DOC: &str = "## NAME\n\nmv — move files and directories\n\n\
                   ## SYNOPSIS\n\n`mv [-f] [--] source... dest`\n\n\
                   ## DESCRIPTION\n\nMoves things.\n";

impl HelpSource for OneDoc {
    fn locale_dirs(&self) -> Result<Vec<String>, SourceError> {
        Ok(alloc::vec![String::from("en-US")])
    }

    fn read(&self, locale_dir: &str, file_name: &str) -> Result<Option<Vec<u8>>, SourceError> {
        if locale_dir == "en-US" && file_name == "mv.md" {
            Ok(Some(DOC.as_bytes().to_vec()))
        } else {
            Ok(None)
        }
    }
}

/// Staging names numbered in the order they are drawn, beside their names.
#[derive(Default)]
struct Numbered(Cell<u32>);

impl Staging for Numbered {
    fn staging_name(&self, name: &str) -> Result<String, Errno> {
        self.0.set(self.0.get() + 1);
        Ok(format!("{name}.stage{}.new", self.0.get()))
    }
}

/// Where the `n`th staging name a run draws for `target` puts its copy.
fn staged(target: &str, n: u32) -> String {
    format!("{target}.stage{n}.new")
}

/// The engine over `fs` with every other seam named.
fn engine(
    command: Command,
    fs: &Tree,
    prompt: &dyn Prompt,
    help: &dyn HelpSource,
    out: &dyn Output,
    diagnostics: &dyn Output,
) -> Result<bool, MvError> {
    let staging = Numbered::default();
    engine_run(
        command,
        None,
        &Seams {
            fs,
            staging: &staging,
            prompt,
            help,
            out,
            diagnostics,
            keys: BuildSipHash13::UNKEYED,
        },
    )
}

/// The engine under a prompt that must never be reached, answering what it
/// reported when it did not run clean: a failed operand is reported and the
/// next still moved. What it wrote on its diagnostics stream is appended to
/// `out`, as one terminal shows both.
fn run(command: Command, fs: &Tree, out: &Recorder) -> Result<(), String> {
    let diagnostics = Recorder::new();
    let outcome = engine(command, fs, &NeverAsked, &NoHelp, out, &diagnostics);
    out.absorb(&diagnostics);
    settled(outcome, &diagnostics)
}

/// A run's outcome as the tool shows it: nothing, the failures it reported,
/// or the one that ended it.
fn settled(outcome: Result<bool, MvError>, diagnostics: &Recorder) -> Result<(), String> {
    match outcome {
        Ok(true) => Ok(()),
        Ok(false) => Err(diagnostics.text()),
        Err(fatal) => Err(format!("mv: {fatal}\n")),
    }
}

/// `failure` as the tool reports it.
fn shown(failure: &MvError) -> Result<(), String> {
    Err(format!("mv: {failure}\n"))
}

/// The calls that changed the tree or committed it, in order.
fn changes(fs: &Tree) -> Vec<String> {
    fs.calls()
        .into_iter()
        .filter(|call| {
            call.starts_with("sync ") || call.starts_with("rename ") || call.starts_with("unlink ")
        })
        .collect()
}

/// Every path the tree holds at or beneath `prefix`.
fn paths_under(fs: &Tree, prefix: &str) -> Vec<String> {
    fs.paths()
        .into_iter()
        .filter(|path| path == prefix || path.starts_with(&format!("{prefix}/")))
        .collect()
}

fn is_dir(fs: &Tree, path: &str) -> bool {
    fs.kind(path) == Some(Kind::Directory)
}

/// Run `hook` just before the first call listed as `line`.
fn before(fs: &Tree, line: &str, hook: impl FnOnce(&Tree) + 'static) {
    fs.before_line(line, Box::new(hook));
}

/// Captures every byte written; optionally fails on the first write.
struct Recorder {
    bytes: RefCell<Vec<u8>>,
    fail: bool,
}

impl Recorder {
    fn new() -> Self {
        Self {
            bytes: RefCell::new(Vec::new()),
            fail: false,
        }
    }

    fn failing() -> Self {
        Self {
            bytes: RefCell::new(Vec::new()),
            fail: true,
        }
    }

    fn text(&self) -> String {
        String::from_utf8(self.bytes.borrow().clone()).expect("utf8 output")
    }

    /// Append what `other` captured.
    fn absorb(&self, other: &Self) {
        self.bytes
            .borrow_mut()
            .extend_from_slice(&other.bytes.borrow());
    }
}

impl Output for Recorder {
    fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
        if self.fail {
            return Err(Errno::NotFound);
        }
        self.bytes.borrow_mut().extend_from_slice(bytes);
        Ok(())
    }
}

fn mv_with(options: Options, sources: &[&str], dest: &str) -> Command {
    Command::Move {
        options,
        sources: sources.iter().map(|p| (*p).to_string()).collect::<Vec<_>>(),
        dest: dest.to_string(),
    }
}

fn mv(no_clobber: bool, sources: &[&str], dest: &str) -> Command {
    mv_with(
        Options {
            clobber: if no_clobber {
                Clobber::Skip
            } else {
                Clobber::Overwrite
            },
            ..Options::DEFAULT
        },
        sources,
        dest,
    )
}

fn prompting() -> Options {
    Options {
        clobber: Clobber::Prompt,
        ..Options::DEFAULT
    }
}

#[test]
fn help_writes_usage() {
    let fs = Tree::new();
    let out = Recorder::new();
    assert_eq!(run(Command::Help, &fs, &out), Ok(()));
    assert_eq!(out.text(), USAGE);
}

#[test]
fn help_renders_the_short_help_from_the_document() {
    let fs = Tree::new();
    let out = Recorder::new();
    assert_eq!(
        engine(Command::Help, &fs, &NeverAsked, &OneDoc, &out, &out),
        Ok(true)
    );
    let text = out.text();
    assert!(text.contains("mv — move files and directories"), "{text}");
    assert!(text.contains("mv [-f] [--] source... dest"), "{text}");
}

#[test]
fn renames_a_file_to_a_new_path() {
    let fs = Tree::new().file("/a.txt", b"hello");
    let out = Recorder::new();
    assert_eq!(run(mv(false, &["/a.txt"], "/b.txt"), &fs, &out), Ok(()));
    assert_eq!(fs.bytes("/b.txt").as_deref(), Some(&b"hello"[..]));
    // The source no longer exists; `mv` is silent on success.
    assert!(!fs.exists("/a.txt"));
    assert_eq!(out.text(), "");
}

#[test]
fn renames_a_directory() {
    let fs = Tree::new().dir("/d").file("/d/f", b"x");
    let out = Recorder::new();
    assert_eq!(run(mv(false, &["/d"], "/e"), &fs, &out), Ok(()));
    assert!(is_dir(&fs, "/e"));
    assert!(!fs.exists("/d"));
    assert_eq!(fs.bytes("/e/f").as_deref(), Some(&b"x"[..]));
}

#[test]
fn moves_a_file_into_an_existing_directory_under_its_basename() {
    let fs = Tree::new().file("/src/a.txt", b"data").dir("/dst");
    let out = Recorder::new();
    assert_eq!(run(mv(false, &["/src/a.txt"], "/dst"), &fs, &out), Ok(()));
    assert_eq!(fs.bytes("/dst/a.txt").as_deref(), Some(&b"data"[..]));
    assert!(!fs.exists("/src/a.txt"));
}

#[test]
fn moves_several_files_into_a_directory() {
    let fs = Tree::new()
        .file("/a", b"one")
        .file("/b", b"two")
        .dir("/dst");
    let out = Recorder::new();
    assert_eq!(run(mv(false, &["/a", "/b"], "/dst"), &fs, &out), Ok(()));
    assert_eq!(fs.bytes("/dst/a").as_deref(), Some(&b"one"[..]));
    assert_eq!(fs.bytes("/dst/b").as_deref(), Some(&b"two"[..]));
}

/// GNU 9.4: `mv m1 m2 f` is `target 'f': Not a directory`, and with no `f`
/// `target 'f': No such file or directory`.
#[test]
fn several_sources_need_a_directory_target() {
    let fs = Tree::new().file("/a", b"x").file("/b", b"y");
    let out = Recorder::new();
    let target = |path: &str, errno| {
        shown(&MvError::Target {
            path: String::from(path),
            directory: false,
            errno,
        })
    };
    assert_eq!(
        run(mv(false, &["/a", "/b"], "/a"), &fs, &out),
        target("/a", Errno::NotADirectory)
    );
    assert_eq!(
        run(mv(false, &["/a", "/b"], "/c"), &fs, &out),
        target("/c", Errno::NotFound)
    );
    assert!(!fs.exists("/c"));
}

#[test]
fn a_missing_source_fails_closed() {
    let fs = Tree::new();
    let out = Recorder::new();
    assert_eq!(
        run(mv(false, &["/absent"], "/dst"), &fs, &out),
        shown(&MvError::Stat {
            path: String::from("/absent"),
            errno: Errno::NotFound
        })
    );
}

/// GNU 9.4: `mv missing c e/` names the missing one, still moves `c` and
/// exits 1.
#[test]
fn a_failed_source_is_reported_and_the_next_still_moved() {
    let fs = Tree::new().file("/b", b"present").dir("/dst");
    let out = Recorder::new();
    assert_eq!(
        run(mv(false, &["/absent", "/b"], "/dst"), &fs, &out),
        shown(&MvError::Stat {
            path: String::from("/absent"),
            errno: Errno::NotFound
        })
    );
    assert_eq!(fs.bytes("/dst/b").as_deref(), Some(&b"present"[..]));
    assert!(!fs.exists("/b"));
}

/// GNU 9.4 says `not replacing 'b'` and ends with status 1.
#[test]
fn no_clobber_skips_an_existing_destination() {
    let fs = Tree::new().file("/a", b"new").file("/b", b"old");
    let out = Recorder::new();
    assert_eq!(
        run(mv(true, &["/a"], "/b"), &fs, &out),
        shown(&MvError::NotReplaced(String::from("/b")))
    );
    // The destination is untouched and the source remains.
    assert_eq!(fs.bytes("/b").as_deref(), Some(&b"old"[..]));
    assert_eq!(fs.bytes("/a").as_deref(), Some(&b"new"[..]));
}

/// `-n` decides on what it saw, so a destination created between its look
/// and its rename must not be destroyed: the kernel refuses the move and the
/// source is skipped as if the file had been there all along.
#[test]
fn no_clobber_never_replaces_a_destination_created_after_its_look() {
    let fs = Tree::new().file("/a", b"new");
    before(&fs, "rename /a -> /b", |fs| fs.write("/b", b"theirs"));
    let out = Recorder::new();
    assert_eq!(
        run(mv(true, &["/a"], "/b"), &fs, &out),
        shown(&MvError::NotReplaced(String::from("/b")))
    );
    assert_eq!(fs.bytes("/b").as_deref(), Some(&b"theirs"[..]));
    assert_eq!(fs.bytes("/a").as_deref(), Some(&b"new"[..]));
}

/// `-i` asks before replacing anything, including a destination that only
/// appeared after its look.
#[test]
fn interactive_asks_about_a_destination_created_after_its_look() {
    for (reply, kept) in [(false, &b"theirs"[..]), (true, &b"new"[..])] {
        let fs = Tree::new().file("/a", b"new");
        before(&fs, "rename /a -> /b", |fs| fs.write("/b", b"theirs"));
        let out = Recorder::new();
        let prompt = Answers::new(&[reply]);
        assert_eq!(
            engine(
                mv_with(prompting(), &["/a"], "/b"),
                &fs,
                &prompt,
                &NoHelp,
                &out,
                &out
            ),
            Ok(reply),
            "a declined question fails the run"
        );
        assert_eq!(fs.bytes("/b").as_deref(), Some(kept));
        assert_eq!(prompt.asked(), ["overwrite '/b'?"]);
    }
}

#[test]
fn overwrites_an_existing_destination_by_default() {
    let fs = Tree::new().file("/a", b"new").file("/b", b"old");
    let out = Recorder::new();
    assert_eq!(run(mv(false, &["/a"], "/b"), &fs, &out), Ok(()));
    assert_eq!(fs.bytes("/b").as_deref(), Some(&b"new"[..]));
    assert!(!fs.exists("/a"));
}

#[test]
fn a_failed_rename_surfaces_the_errno() {
    let fs = Tree::new()
        .file("/a", b"x")
        .refuse("rename /a -> /b", Errno::PermissionDenied);
    let out = Recorder::new();
    assert_eq!(
        run(mv(false, &["/a"], "/b"), &fs, &out),
        shown(&MvError::Rename {
            source: String::from("/a"),
            target: String::from("/b"),
            errno: Errno::PermissionDenied
        })
    );
}

#[test]
fn a_destination_that_blocks_the_rename_is_kept_with_the_source() {
    let fs = Tree::new()
        .file("/a", b"fresh")
        .file("/b", b"stale")
        .refuse("rename /a -> /b", Errno::PermissionDenied);
    let out = Recorder::new();
    assert_eq!(
        run(mv(false, &["/a"], "/b"), &fs, &out),
        shown(&MvError::Rename {
            source: String::from("/a"),
            target: String::from("/b"),
            errno: Errno::PermissionDenied
        })
    );
    // Nothing moved: the source and the stale destination both remain.
    assert_eq!(fs.bytes("/a").as_deref(), Some(&b"fresh"[..]));
    assert_eq!(fs.bytes("/b").as_deref(), Some(&b"stale"[..]));
}

/// The rename moves only the node `mv` looked at: one swapped in at the name
/// since is refused, and stays where it is.
#[test]
fn a_source_swapped_after_its_look_is_not_moved() {
    let fs = Tree::new().file("/a", b"looked at");
    before(&fs, "rename /a -> /b", |fs| {
        fs.remove("/a");
        fs.write("/a", b"swapped in");
    });
    assert_eq!(
        run(mv(false, &["/a"], "/b"), &fs, &Recorder::new()),
        shown(&MvError::Rename {
            source: String::from("/a"),
            target: String::from("/b"),
            errno: Errno::Stale
        })
    );
    assert_eq!(fs.bytes("/a").as_deref(), Some(&b"swapped in"[..]));
    assert!(!fs.exists("/b"));
}

#[test]
fn force_only_stops_the_questions_and_never_destroys_a_destination_of_another_kind() {
    // GNU's `-f` is "do not prompt": a directory still cannot replace a file,
    // and the file is not removed to make room.
    let fs = Tree::new()
        .dir("/d")
        .file("/d/f", b"inside")
        .file("/b", b"kept");
    let forced = Options {
        clobber: Clobber::Overwrite,
        target_mode: TargetMode::NoDirectory,
        ..Options::DEFAULT
    };
    assert_eq!(
        run(mv_with(forced, &["/d"], "/b"), &fs, &Recorder::new()),
        shown(&MvError::OverwriteNonDirectory {
            source: String::from("/d"),
            target: String::from("/b"),
        })
    );
    assert_eq!(fs.bytes("/b").as_deref(), Some(&b"kept"[..]));
    assert_eq!(fs.bytes("/d/f").as_deref(), Some(&b"inside"[..]));
}

#[test]
fn cross_device_file_is_copied_then_removed() {
    let fs = Tree::new().file("/src/a", b"payload").volume("/src");
    let out = Recorder::new();
    assert_eq!(run(mv(false, &["/src/a"], "/dst"), &fs, &out), Ok(()));
    assert_eq!(fs.bytes("/dst").as_deref(), Some(&b"payload"[..]));
    assert!(!fs.exists("/src/a"));
}

#[test]
fn cross_device_large_file_streams_across_the_chunk_boundary() {
    let payload: Vec<u8> = (0..(tairix_fscopy::CHUNK_LEN * 2 + 7))
        .map(|i| u8::try_from(i % 251).unwrap_or_default())
        .collect();
    let fs = Tree::new().file("/src/big", &payload).volume("/src");
    let out = Recorder::new();
    assert_eq!(run(mv(false, &["/src/big"], "/copy"), &fs, &out), Ok(()));
    assert_eq!(fs.bytes("/copy"), Some(payload));
    assert!(!fs.exists("/src/big"));
}

#[test]
fn cross_device_directory_is_reproduced_then_removed() {
    let fs = Tree::new()
        .dir("/src/d")
        .file("/src/d/f", b"top")
        .dir("/src/d/sub")
        .file("/src/d/sub/g", b"nested")
        .volume("/src");
    let out = Recorder::new();
    assert_eq!(run(mv(false, &["/src/d"], "/e"), &fs, &out), Ok(()));
    assert!(is_dir(&fs, "/e"));
    assert!(is_dir(&fs, "/e/sub"));
    assert_eq!(fs.bytes("/e/f").as_deref(), Some(&b"top"[..]));
    assert_eq!(fs.bytes("/e/sub/g").as_deref(), Some(&b"nested"[..]));
    // The whole source subtree is gone.
    assert!(!fs.exists("/src/d"));
}

#[test]
fn cross_device_read_failure_surfaces_the_errno() {
    let fs = Tree::new()
        .file("/src/a", b"x")
        .volume("/src")
        .refuse("read /src/a", Errno::PermissionDenied);
    let out = Recorder::new();
    assert_eq!(
        run(mv(false, &["/src/a"], "/dst"), &fs, &out),
        shown(&MvError::Read {
            path: String::from("/src/a"),
            errno: Errno::PermissionDenied
        })
    );
    // The source remains, because the copy failed before removal.
    assert!(fs.exists("/src/a"));
    assert!(
        fs.paths().iter().all(|p| !p.starts_with("/dst")),
        "a failed move leaves nothing made for it"
    );
}

#[test]
fn a_move_that_failed_leaves_no_ancestry_for_the_next_operand() {
    // `/src/a/s`'s staged copy ended on its unreadable file; moving `/src/a`
    // then meets `s` again, which is no move into itself.
    let fs = Tree::new()
        .dir("/src/a/s")
        .file("/src/a/s/f", b"x")
        .dir("/dst")
        .volume("/src")
        .refuse("read /src/a/s/f", Errno::PermissionDenied);
    let unread = MvError::Read {
        path: String::from("/src/a/s/f"),
        errno: Errno::PermissionDenied,
    };
    assert_eq!(
        run(
            mv(false, &["/src/a/s", "/src/a"], "/dst"),
            &fs,
            &Recorder::new()
        ),
        Err(format!("mv: {unread}\nmv: {unread}\n"))
    );
    assert!(fs.exists("/src/a/s/f"));
}

#[test]
fn cross_device_write_failure_surfaces_the_errno() {
    let fs = Tree::new()
        .file("/src/a", b"x")
        .volume("/src")
        .refuse("write /dst.stage1.new", Errno::PermissionDenied);
    let out = Recorder::new();
    assert_eq!(
        run(mv(false, &["/src/a"], "/dst"), &fs, &out),
        shown(&MvError::Write {
            path: String::from("/dst"),
            errno: Errno::PermissionDenied
        })
    );
    assert!(fs.exists("/src/a"));
    assert!(
        fs.paths().iter().all(|p| !p.starts_with("/dst")),
        "a failed move leaves no partial file, staged or not"
    );
}

#[test]
fn an_attribute_key_holding_control_bytes_is_reported_escaped() {
    let refused = MvError::Attribute {
        key: String::from("user.\u{1b}[2J"),
        target: String::from("/dst"),
        error: Errno::Busy,
    };
    assert_eq!(
        refused.to_string(),
        alloc::format!(
            "setting attribute 'user.'$'\\033''[2J' for '/dst': {}",
            Errno::Busy
        )
    );
}

#[test]
fn an_attribute_key_is_quoted_as_every_name_is() {
    let refused = MvError::Attribute {
        key: String::from("user.it's\u{202e}gpj"),
        target: String::from("/dst"),
        error: Errno::Busy,
    };
    assert_eq!(
        refused.to_string(),
        alloc::format!(
            "setting attribute 'user.it'\\''s'$'\\342\\200\\256''gpj' for '/dst': {}",
            Errno::Busy
        )
    );
}

#[test]
fn cross_device_remove_failure_surfaces_the_errno() {
    let fs = Tree::new()
        .file("/src/a", b"x")
        .volume("/src")
        .refuse("unlink /src/a", Errno::PermissionDenied);
    let out = Recorder::new();
    assert_eq!(
        run(mv(false, &["/src/a"], "/dst"), &fs, &out),
        shown(&MvError::Remove {
            path: String::from("/src/a"),
            errno: Errno::PermissionDenied
        })
    );
    // The destination already holds the copied data.
    assert_eq!(fs.bytes("/dst").as_deref(), Some(&b"x"[..]));
}

#[test]
fn help_output_failure_propagates() {
    let fs = Tree::new();
    let out = Recorder::failing();
    assert_eq!(
        run(Command::Help, &fs, &out),
        shown(&MvError::Output(Errno::NotFound))
    );
}

#[test]
fn a_trailing_slash_source_moves_under_its_basename() {
    let fs = Tree::new().dir("/d").file("/d/f", b"x").dir("/dst");
    let out = Recorder::new();
    assert_eq!(run(mv(false, &["/d/"], "/dst"), &fs, &out), Ok(()));
    assert!(is_dir(&fs, "/dst/d"));
    assert_eq!(fs.bytes("/dst/d/f").as_deref(), Some(&b"x"[..]));
}

#[test]
fn an_alias_root_source_lands_under_its_own_root_spelling() {
    // Behaviour moved with the sweep onto the shared `leaf_name`: the private
    // base name this tool carried was not alias-aware and named `Home:/` as
    // `Home:`, dropping the separator that makes it a root.
    assert_eq!(tairix_path::leaf_name("Home:/"), "Home:/");
    // And an ordinary path is unchanged by the move.
    assert_eq!(tairix_path::leaf_name("/a/b/"), "b");
}

#[test]
fn an_empty_source_operand_names_nothing_and_fails_closed() {
    // `leaf_name` names nothing for the empty spelling, and the move fails on
    // the unreachable source rather than acting on a guessed path.
    assert_eq!(tairix_path::leaf_name(""), "");
    let fs = Tree::new().dir("/dst");
    let out = Recorder::new();
    assert!(run(mv(false, &[""], "/dst"), &fs, &out).is_err());
}

#[test]
fn interactive_asks_before_overwriting() {
    let fs = Tree::new()
        .file("/a", b"fresh")
        .file("/b", b"stale")
        .file("/c", b"old")
        .dir("/dst")
        .file("/dst/a", b"blockA")
        .file("/dst/c", b"blockC");
    let out = Recorder::new();
    let prompt = Answers::new(&[true, false]);
    assert_eq!(
        engine(
            mv_with(prompting(), &["/a", "/c"], "/dst"),
            &fs,
            &prompt,
            &NoHelp,
            &out,
            &out
        ),
        Ok(false)
    );
    // `/dst/a` was confirmed and replaced; `/dst/c` was declined and kept
    // (its source stays put), which ends the run failed with nothing to say,
    // as GNU 9.4's.
    assert_eq!(fs.bytes("/dst/a").as_deref(), Some(&b"fresh"[..]));
    assert_eq!(fs.bytes("/dst/c").as_deref(), Some(&b"blockC"[..]));
    assert_eq!(fs.bytes("/c").as_deref(), Some(&b"old"[..]));
    assert_eq!(
        prompt.asked(),
        ["overwrite '/dst/a'?", "overwrite '/dst/c'?"]
    );
}

#[test]
fn an_unanswerable_prompt_fails_closed() {
    let fs = Tree::new().file("/a", b"fresh").file("/b", b"stale");
    let out = Recorder::new();
    let prompt = Answers::new(&[]);
    assert_eq!(
        engine(
            mv_with(prompting(), &["/a"], "/b"),
            &fs,
            &prompt,
            &NoHelp,
            &out,
            &out
        ),
        Err(MvError::Prompt(Errno::NotFound))
    );
    assert_eq!(fs.bytes("/b").as_deref(), Some(&b"stale"[..]));
}

#[test]
fn verbose_reports_each_move_in_gnu_wording() {
    let fs = Tree::new().file("/a", b"x").dir("/dst");
    let out = Recorder::new();
    let verbose = Options {
        verbose: true,
        ..Options::DEFAULT
    };
    assert_eq!(run(mv_with(verbose, &["/a"], "/dst"), &fs, &out), Ok(()));
    assert_eq!(out.text(), "renamed '/a' -> '/dst/a'\n");
}

#[test]
fn target_directory_moves_every_source_into_it() {
    let fs = Tree::new()
        .file("/a", b"one")
        .file("/b", b"two")
        .dir("/dst");
    let out = Recorder::new();
    let into = Options {
        target_mode: TargetMode::Directory,
        ..Options::DEFAULT
    };
    assert_eq!(run(mv_with(into, &["/a", "/b"], "/dst"), &fs, &out), Ok(()));
    assert_eq!(fs.bytes("/dst/a").as_deref(), Some(&b"one"[..]));
    assert_eq!(fs.bytes("/dst/b").as_deref(), Some(&b"two"[..]));
}

#[test]
fn target_directory_must_exist_and_be_a_directory() {
    let fs = Tree::new().file("/a", b"one").file("/blocker", b"x");
    let out = Recorder::new();
    let into = Options {
        target_mode: TargetMode::Directory,
        ..Options::DEFAULT
    };
    assert_eq!(
        run(mv_with(into, &["/a"], "/blocker"), &fs, &out),
        shown(&MvError::Target {
            path: String::from("/blocker"),
            directory: true,
            errno: Errno::NotADirectory
        })
    );
    assert_eq!(
        run(mv_with(into, &["/a"], "/absent"), &fs, &out),
        shown(&MvError::Target {
            path: String::from("/absent"),
            directory: true,
            errno: Errno::NotFound
        })
    );
}

#[test]
fn no_target_directory_refuses_several_sources() {
    let fs = Tree::new().file("/a", b"x").file("/b", b"y").dir("/dst");
    let out = Recorder::new();
    let exact = Options {
        target_mode: TargetMode::NoDirectory,
        ..Options::DEFAULT
    };
    assert_eq!(
        run(mv_with(exact, &["/a", "/b"], "/dst"), &fs, &out),
        shown(&MvError::Usage)
    );
}

/// Run `command` with its diagnostics apart from its output.
fn run_apart(command: Command, fs: &Tree) -> (Result<(), String>, Recorder, Recorder) {
    let (out, diagnostics) = (Recorder::new(), Recorder::new());
    let outcome = engine(command, fs, &NeverAsked, &NoHelp, &out, &diagnostics);
    (settled(outcome, &diagnostics), out, diagnostics)
}

#[test]
fn a_link_to_a_directory_moves_across_volumes_as_a_link_and_spares_its_target() {
    // The link is the object moved: what it names is neither copied nor, as
    // the source is removed, emptied.
    let fs = Tree::new()
        .dir("/keep")
        .file("/keep/precious", b"do not lose")
        .dir("/src")
        .link("/src/shortcut", "/keep")
        .volume("/src");
    let (result, _, _) = run_apart(mv(false, &["/src/shortcut"], "/dst"), &fs);
    assert_eq!(result, Ok(()));
    assert_eq!(
        fs.link_target("/dst").as_deref(),
        Some("/keep"),
        "the link itself arrives"
    );
    assert!(!fs.exists("/src/shortcut"), "the source link is gone");
    assert_eq!(
        fs.bytes("/keep/precious").as_deref(),
        Some(&b"do not lose"[..])
    );
    assert!(is_dir(&fs, "/keep"));
}

#[test]
fn links_inside_a_moved_directory_stay_links() {
    let fs = Tree::new()
        .file("/elsewhere", b"target bytes")
        .dir("/src/tree")
        .file("/src/tree/file", b"own bytes")
        .link("/src/tree/alias", "../elsewhere")
        .volume("/src");
    let (result, _, _) = run_apart(mv(false, &["/src/tree"], "/moved"), &fs);
    assert_eq!(result, Ok(()));
    assert_eq!(fs.bytes("/moved/file").as_deref(), Some(&b"own bytes"[..]));
    assert_eq!(
        fs.link_target("/moved/alias").as_deref(),
        Some("../elsewhere"),
        "stored verbatim"
    );
    assert!(!fs.exists("/src/tree"));
    assert_eq!(
        fs.bytes("/elsewhere").as_deref(),
        Some(&b"target bytes"[..])
    );
}

#[test]
fn a_move_across_volumes_carries_attributes_and_holes_and_reports_what_it_lost() {
    let fs = Tree::new()
        .dir("/src")
        .sparse("/src/image", 256 * 1024, &[(4096, b"data")])
        .attr("/src/image", "acorn.filetype", b"fce")
        .attr("/src/image", "mac.type", b"BINA")
        .refuses_attr("mac.type", Errno::NotSupported)
        .volume("/src");
    let (result, out, diagnostics) = run_apart(mv(false, &["/src/image"], "/dst"), &fs);
    assert_eq!(result, Ok(()));
    assert_eq!(fs.bytes("/dst").map(|bytes| bytes.len()), Some(256 * 1024));
    assert_eq!(fs.written("/dst"), 4, "the holes stay holes");
    assert_eq!(
        fs.attr_of("/dst", "acorn.filetype").as_deref(),
        Some(&b"fce"[..])
    );
    assert_eq!(
        diagnostics.text(),
        "mv: warning: extended attributes not kept on 1 file (the destination cannot store them): mac.type\n"
    );
    assert_eq!(out.text(), "");
    assert!(!fs.exists("/src/image"));
}

// --- moving across volumes: one replacement, made whole first ------------

#[test]
fn a_move_across_volumes_replaces_the_destination_only_once_the_copy_is_on_the_medium() {
    let fs = Tree::new()
        .file("/src/a", b"new")
        .file("/dst", b"old")
        .volume("/src");
    let stage = staged("/dst", 1);
    assert_eq!(
        run(mv(false, &["/src/a"], "/dst"), &fs, &Recorder::new()),
        Ok(())
    );
    assert_eq!(fs.bytes("/dst").as_deref(), Some(&b"new"[..]));
    assert!(!fs.exists("/src/a"));
    assert!(!fs.exists(&stage), "the staging name is gone");
    assert_eq!(
        changes(&fs),
        alloc::vec![
            String::from("rename /src/a -> /dst"),
            format!("sync {stage}"),
            format!("rename {stage} -> /dst"),
            String::from("sync /dst"),
            String::from("unlink /src/a"),
        ],
        "on the medium before the rename, renamed before the source goes"
    );
}

#[test]
fn a_move_across_volumes_that_fails_keeps_the_destination_it_would_have_replaced() {
    let stage = staged("/dst", 1);
    let fs = Tree::new()
        .file("/src/a", b"new")
        .file("/dst", b"old")
        .volume("/src")
        .refuse(&format!("write {stage}"), Errno::NoSpace);
    assert_eq!(
        run(mv(false, &["/src/a"], "/dst"), &fs, &Recorder::new()),
        shown(&MvError::Write {
            path: String::from("/dst"),
            errno: Errno::NoSpace
        })
    );
    assert_eq!(fs.bytes("/dst").as_deref(), Some(&b"old"[..]));
    assert_eq!(fs.bytes("/src/a").as_deref(), Some(&b"new"[..]));
    assert!(!fs.exists(&stage));
}

/// Another name for the node an earlier operand moved into place — a hard
/// link, as `d/z` names the node `a/x` is moved from — is replaced as GNU's
/// `mv` replaces it: the run's own name keeps the node.
#[test]
fn another_name_for_what_an_earlier_operand_moved_is_replaced() {
    let fs = Tree::new()
        .file("/a/x", b"first")
        .file("/b/z", b"second")
        .dir("/d")
        .hard_link("/d/z", "/a/x");
    assert_eq!(
        run(mv(false, &["/a/x", "/b/z"], "/d"), &fs, &Recorder::new()),
        Ok(())
    );
    assert_eq!(fs.bytes("/d/x").as_deref(), Some(&b"first"[..]));
    assert_eq!(fs.bytes("/d/z").as_deref(), Some(&b"second"[..]));
}

/// GNU 9.4's refusal: the second operand would put `b/x` over the `a/x` the
/// first moved there, whose source is already gone.
#[test]
fn an_operand_never_replaces_what_an_earlier_one_moved_into_place() {
    for across in [false, true] {
        let fs = Tree::new()
            .file("/src/a/x", b"first")
            .file("/src/b/x", b"second")
            .file("/src/b/y", b"third")
            .dir("/d");
        let fs = if across { fs.volume("/src") } else { fs };
        assert_eq!(
            run(
                mv(false, &["/src/a/x", "/src/b/x", "/src/b/y"], "/d"),
                &fs,
                &Recorder::new()
            ),
            shown(&MvError::JustCreated {
                source: String::from("/src/b/x"),
                target: String::from("/d/x"),
            }),
            "across volumes: {across}"
        );
        assert_eq!(fs.bytes("/d/x").as_deref(), Some(&b"first"[..]));
        assert_eq!(fs.bytes("/src/b/x").as_deref(), Some(&b"second"[..]));
        assert_eq!(fs.bytes("/d/y").as_deref(), Some(&b"third"[..]));
        assert!(!fs.exists("/src/a/x"));
    }
    let err = MvError::JustCreated {
        source: String::from("b/x"),
        target: String::from("d/x"),
    };
    assert_eq!(
        format!("{err}"),
        "will not overwrite just-created 'd/x' with 'b/x'"
    );
}

#[test]
fn a_directory_may_replace_the_empty_one_an_earlier_operand_moved_there() {
    let fs = Tree::new().dir("/a/s").dir("/b/s").dir("/d");
    assert_eq!(
        run(mv(false, &["/a/s", "/b/s"], "/d"), &fs, &Recorder::new()),
        Ok(())
    );
    assert!(fs.exists("/d/s"));
    assert!(!fs.exists("/a/s") && !fs.exists("/b/s"));
}

#[test]
fn a_staging_name_another_holds_is_refused_and_left_alone() {
    for directory in [false, true] {
        let taken = staged("/dst", 1);
        let fs = if directory {
            Tree::new()
                .file("/src/a/x", b"new")
                .file(&format!("{taken}/theirs"), b"kept")
        } else {
            Tree::new()
                .file("/src/a", b"new")
                .file("/dst", b"old")
                .file(&taken, b"kept")
        };
        let fs = fs.volume("/src");
        let outcome = run(mv(false, &["/src/a"], "/dst"), &fs, &Recorder::new());
        assert_eq!(
            outcome,
            shown(&MvError::Create {
                path: String::from("/dst"),
                errno: Errno::AlreadyExists
            }),
            "directory: {directory}"
        );
        let kept = if directory {
            format!("{taken}/theirs")
        } else {
            taken
        };
        assert_eq!(fs.bytes(&kept).as_deref(), Some(&b"kept"[..]));
        if directory {
            assert!(!fs.exists("/dst"));
            assert_eq!(fs.bytes("/src/a/x").as_deref(), Some(&b"new"[..]));
        } else {
            assert_eq!(fs.bytes("/dst").as_deref(), Some(&b"old"[..]));
            assert_eq!(fs.bytes("/src/a").as_deref(), Some(&b"new"[..]));
        }
    }
}

#[test]
fn a_move_across_volumes_whose_commit_fails_keeps_its_source() {
    let fs = Tree::new()
        .file("/src/a", b"new")
        .volume("/src")
        .refuse("sync /dst", Errno::DeviceFault);
    assert_eq!(
        run(mv(false, &["/src/a"], "/dst"), &fs, &Recorder::new()),
        shown(&MvError::Write {
            path: String::from("/dst"),
            errno: Errno::DeviceFault
        })
    );
    assert_eq!(fs.bytes("/dst").as_deref(), Some(&b"new"[..]));
    assert_eq!(fs.bytes("/src/a").as_deref(), Some(&b"new"[..]));
}

#[test]
fn a_directory_moved_across_volumes_keeps_the_renames_rules() {
    let tree = || {
        Tree::new()
            .dir("/src/d")
            .file("/src/d/f", b"inside")
            .file("/src/file", b"plain")
            .dir("/full")
            .file("/full/keep", b"kept")
            .dir("/empty")
            .file("/afile", b"in the way")
            .volume("/src")
    };
    let onto = |source: &str, target: &str| {
        let fs = tree();
        let options = Options {
            target_mode: TargetMode::NoDirectory,
            ..Options::DEFAULT
        };
        let result = run(mv_with(options, &[source], target), &fs, &Recorder::new());
        (result, fs)
    };
    let (result, fs) = onto("/src/d", "/full");
    assert_eq!(
        result,
        shown(&MvError::Rename {
            source: String::from("/src/d"),
            target: String::from("/full"),
            errno: Errno::NotEmpty
        })
    );
    assert_eq!(fs.bytes("/full/keep").as_deref(), Some(&b"kept"[..]));
    assert_eq!(paths_under(&fs, "/full").len(), 2, "nothing was copied in");
    let (result, fs) = onto("/src/d", "/afile");
    assert_eq!(
        result,
        shown(&MvError::OverwriteNonDirectory {
            source: String::from("/src/d"),
            target: String::from("/afile"),
        })
    );
    assert_eq!(fs.bytes("/afile").as_deref(), Some(&b"in the way"[..]));
    let (result, _) = onto("/src/file", "/empty");
    assert_eq!(
        result,
        shown(&MvError::OverwriteDirectory {
            target: String::from("/empty"),
        })
    );
    let (result, fs) = onto("/src/d", "/empty");
    assert_eq!(result, Ok(()), "an empty directory is replaced");
    assert_eq!(fs.bytes("/empty/f").as_deref(), Some(&b"inside"[..]));
    assert!(!fs.exists("/src/d"));
}

#[test]
fn a_directory_never_moves_into_its_own_subtree() {
    // Within a volume the kernel refuses it; across one (a volume attached
    // inside the source) the copy is refused before anything is written.
    let fs = Tree::new().dir("/a/sub").file("/a/f", b"x");
    assert_eq!(
        run(mv(false, &["/a"], "/a/sub/in"), &fs, &Recorder::new()),
        shown(&MvError::IntoItself {
            source: String::from("/a"),
            target: String::from("/a/sub/in"),
        })
    );
    let across = Tree::new()
        .dir("/a/mnt")
        .file("/a/f", b"x")
        .volume("/a/mnt");
    assert_eq!(
        run(mv(false, &["/a"], "/a/mnt/in"), &across, &Recorder::new()),
        shown(&MvError::IntoItself {
            source: String::from("/a"),
            target: String::from("/a/mnt/in"),
        })
    );
    assert!(!across.exists("/a/mnt/in"));
    assert_eq!(paths_under(&across, "/a/mnt"), ["/a/mnt"]);
}

#[test]
fn a_source_beyond_a_cycle_of_links_is_named_as_the_source() {
    let fs = Tree::new().dir("/b").link("/loop", "/loop");
    assert_eq!(
        run(mv(false, &["/loop/src"], "/b/c"), &fs, &Recorder::new()),
        shown(&MvError::Stat {
            path: String::from("/loop/src"),
            errno: Errno::LinkLoop,
        })
    );
}

#[test]
fn a_moved_node_keeps_its_owner_and_mode_and_loses_set_id_bits_it_cannot_keep() {
    let theirs = || {
        Tree::new()
            .file("/src/tool", b"bytes")
            .owned("/src/tool", 42, 42)
            .with_mode("/src/tool", 0o4755)
            .volume("/src")
    };
    let fs = theirs();
    assert_eq!(
        run(mv(false, &["/src/tool"], "/tool"), &fs, &Recorder::new()),
        Ok(())
    );
    assert_eq!(
        (fs.owner("/tool"), fs.mode("/tool")),
        (Some(USER), Some(0o755)),
        "the mover may not give it away, so it is the mover's, set-ID bit dropped"
    );
    let fs = theirs().privileged();
    assert_eq!(
        run(mv(false, &["/src/tool"], "/tool"), &fs, &Recorder::new()),
        Ok(())
    );
    assert_eq!(
        (fs.owner("/tool"), fs.mode("/tool")),
        (Some((42, 42)), Some(0o755)),
        "given back, its bits set first; the system clears the set-ID bit as its owner changes"
    );
}

/// A tree to move across volumes, the victims outside it, and the links an
/// attacker swaps into it.
fn bait_tree() -> Tree {
    Tree::new()
        .file("/src/d/f", b"F")
        .file("/src/d/sub/g", b"G")
        .link("/src/bait-f", "/victim")
        .link("/src/bait-sub", "/vdir")
        .volume("/src")
        .file("/victim", b"victim")
        .file("/vdir/x", b"x")
        .dir("/dst")
        .link("/dst/bait-stage", "/vdir")
}

/// Swap each name a move reads, writes or stages under, as it stands, for a
/// link to a victim.
fn swap_names(tree: &Tree) {
    for (name, bait) in [
        ("/src/d/f", "/src/bait-f"),
        ("/src/d/sub", "/src/bait-sub"),
        ("/dst/d.stage1.new", "/dst/bait-stage"),
    ] {
        if tree.exists(name) && tree.exists(bait) {
            tree.rename(name, &format!("{name}.gone"));
            tree.rename(bait, name);
        }
    }
}

#[test]
fn a_name_swapped_for_a_link_at_any_point_never_leads_a_move_out_or_loses_data() {
    let clean = bait_tree();
    let _ = run(mv(false, &["/src/d"], "/dst/d"), &clean, &Recorder::new());
    for n in 0..clean.calls().len() {
        let fs = bait_tree();
        fs.before_call(n, Box::new(swap_names));
        let _ = run(mv(false, &["/src/d"], "/dst/d"), &fs, &Recorder::new());
        assert_eq!(fs.bytes("/victim"), Some(b"victim".to_vec()), "call {n}");
        assert_eq!(fs.bytes("/vdir/x"), Some(b"x".to_vec()), "call {n}");
        assert_eq!(
            fs.paths()
                .iter()
                .filter(|path| path.starts_with("/vdir/"))
                .count(),
            1,
            "nothing was made or removed through a link, call {n}"
        );
        // Swapped aside within the tree, a node moves with it.
        for (bytes, places) in [
            (
                &b"F"[..],
                ["/src/d/f", "/src/d/f.gone", "/dst/d/f", "/dst/d/f.gone"],
            ),
            (
                &b"G"[..],
                [
                    "/src/d/sub/g",
                    "/src/d/sub.gone/g",
                    "/dst/d/sub/g",
                    "/dst/d/sub.gone/g",
                ],
            ),
        ] {
            assert!(
                places
                    .iter()
                    .any(|place| fs.bytes(place).as_deref() == Some(bytes)),
                "{bytes:?} is kept somewhere, call {n}: {:?}",
                fs.paths()
            );
        }
    }
}

#[test]
fn a_node_the_mover_may_not_give_away_keeps_the_group_the_mover_may_keep() {
    let moved = |gid| {
        let fs = Tree::new()
            .file("/src/tool", b"bytes")
            .owned("/src/tool", 42, gid)
            .with_mode("/src/tool", 0o6755)
            .volume("/src");
        assert_eq!(
            run(mv(false, &["/src/tool"], "/tool"), &fs, &Recorder::new()),
            Ok(())
        );
        (fs.owner("/tool"), fs.mode("/tool"))
    };
    assert_eq!(
        moved(USER.1),
        (Some(USER), Some(0o2755)),
        "its group kept, so its set-group-ID bit"
    );
    assert_eq!(moved(99), (Some(USER), Some(0o755)));
}

#[test]
fn a_moved_link_keeps_its_owner_where_the_mover_may_give_it() {
    let fs = Tree::new()
        .privileged()
        .dir("/src/d")
        .link("/src/d/l", "elsewhere")
        .owned("/src/d/l", 42, 42)
        .volume("/src");
    assert_eq!(
        run(mv(false, &["/src/d"], "/d"), &fs, &Recorder::new()),
        Ok(())
    );
    assert_eq!(fs.owner("/d/l"), Some((42, 42)));
    assert_eq!(fs.link_target("/d/l").as_deref(), Some("elsewhere"));
}

#[test]
fn a_directory_moves_across_volumes_under_a_umask_withholding_its_owner_s_write() {
    let fs = Tree::new()
        .checking_permissions()
        .with_umask(0o277)
        .dir("/src/d/sub")
        .file("/src/d/sub/f", b"bytes")
        .dir("/dst")
        .volume("/src");
    assert_eq!(
        run(mv(false, &["/src/d"], "/dst/d"), &fs, &Recorder::new()),
        Ok(())
    );
    assert_eq!(fs.bytes("/dst/d/sub/f"), Some(b"bytes".to_vec()));
    assert_eq!(fs.mode("/dst/d"), Some(0o755));
    assert!(!fs.exists("/src/d"));
}

#[test]
fn a_directory_kept_for_an_entry_that_would_not_go_is_not_called_changed() {
    let fs = Tree::new()
        .dir("/src/d/sub")
        .file("/src/d/sub/f", b"bytes")
        .volume("/src")
        .refuse("unlink /src/d/sub/f", Errno::PermissionDenied);
    let refused = MvError::Remove {
        path: String::from("/src/d/sub/f"),
        errno: Errno::PermissionDenied,
    };
    assert_eq!(
        run(mv(false, &["/src/d"], "/d"), &fs, &Recorder::new()),
        shown(&refused)
    );
    assert_eq!(
        fs.bytes("/d/sub/f"),
        Some(b"bytes".to_vec()),
        "the copy is whole"
    );
    assert!(fs.exists("/src/d/sub/f"));
}

/// D1368: every staged node is its owner's alone while it is filled, and
/// takes its source's mode only once it is.
#[test]
fn a_staged_copy_is_owner_only_while_it_is_filled() {
    let fs = Tree::new()
        .dir("/src/mail")
        .file("/src/mail/inbox", b"private")
        .dir("/src/mail/sent")
        .file("/src/mail/sent/one", b"x")
        .dir("/b")
        .volume("/src");
    assert_eq!(
        run(mv(false, &["/src/mail"], "/b/mail"), &fs, &Recorder::new()),
        Ok(())
    );
    let seen = fs.fill_modes();
    assert!(
        seen.iter().any(|(path, _)| path.ends_with("/one")),
        "the fill was watched: {seen:?}"
    );
    for (path, mode) in &seen {
        assert_eq!(mode & 0o077, 0, "{path} was {mode:o} while it was filled");
    }
    assert_eq!(fs.mode("/b/mail"), Some(0o755));
    assert_eq!(fs.mode("/b/mail/inbox"), Some(0o644));
    assert_eq!(fs.mode("/b/mail/sent"), Some(0o755));
}

#[test]
fn a_directory_moved_across_volumes_takes_its_mode_after_its_entries() {
    let fs = Tree::new()
        .dir("/src/ro")
        .file("/src/ro/f", b"x")
        .with_mode("/src/ro", 0o555)
        .volume("/src");
    assert_eq!(
        run(mv(false, &["/src/ro"], "/ro"), &fs, &Recorder::new()),
        Ok(())
    );
    assert_eq!(fs.mode("/ro"), Some(0o555));
    assert_eq!(fs.bytes("/ro/f").as_deref(), Some(&b"x"[..]));
}

#[test]
fn no_clobber_keeps_a_destination_that_appears_while_a_move_across_volumes_runs() {
    let fs = Tree::new().file("/src/a", b"new").volume("/src");
    before(&fs, "rename /src/a -> /dst", |fs| {
        fs.write("/dst", b"theirs");
    });
    let stage = staged("/dst", 1);
    assert_eq!(
        run(mv(true, &["/src/a"], "/dst"), &fs, &Recorder::new()),
        shown(&MvError::NotReplaced(String::from("/dst")))
    );
    assert_eq!(fs.bytes("/dst").as_deref(), Some(&b"theirs"[..]));
    assert_eq!(fs.bytes("/src/a").as_deref(), Some(&b"new"[..]));
    assert!(!fs.exists(&stage));
}

#[test]
fn verbose_reports_a_move_across_volumes_as_a_copy_and_a_removal_and_nothing_it_skipped() {
    let fs = Tree::new()
        .file("/src/a", b"x")
        .file("/kept", b"k")
        .file("/b", b"y")
        .volume("/src");
    let out = Recorder::new();
    let verbose = |clobber| Options {
        verbose: true,
        clobber,
        ..Options::DEFAULT
    };
    assert_eq!(
        run(
            mv_with(verbose(Clobber::Overwrite), &["/src/a"], "/dst"),
            &fs,
            &out
        ),
        Ok(())
    );
    assert_eq!(
        run(mv_with(verbose(Clobber::Skip), &["/b"], "/kept"), &fs, &out),
        shown(&MvError::NotReplaced(String::from("/kept")))
    );
    assert_eq!(
        out.text(),
        "copied '/src/a' -> '/dst'\nremoved '/src/a'\nmv: not replacing '/kept'\n"
    );
}

/// GNU 9.4 reports a tree moved across volumes entry by entry: each
/// directory as it is created and everything else as it is copied, then each
/// entry as the source goes.
#[test]
fn verbose_reports_a_directory_moved_across_volumes_entry_by_entry() {
    let fs = Tree::new()
        .dir("/src/t")
        .file("/src/t/f", b"x")
        .dir("/src/t/d")
        .file("/src/t/d/g", b"y")
        .link("/src/t/ln", "f")
        .volume("/src");
    let out = Recorder::new();
    let verbose = Options {
        verbose: true,
        ..Options::DEFAULT
    };
    assert_eq!(
        run(mv_with(verbose, &["/src/t"], "/dst"), &fs, &out),
        Ok(())
    );
    assert_eq!(
        out.text(),
        "created directory '/dst'\n\
         copied '/src/t/f' -> '/dst/f'\n\
         created directory '/dst/d'\n\
         copied '/src/t/d/g' -> '/dst/d/g'\n\
         copied '/src/t/ln' -> '/dst/ln'\n\
         removed '/src/t/f'\n\
         removed '/src/t/d/g'\n\
         removed directory '/src/t/d'\n\
         removed '/src/t/ln'\n\
         removed directory '/src/t'\n"
    );
}

#[test]
fn a_name_holding_control_bytes_is_quoted_in_every_message() {
    let refused = MvError::Stat {
        path: String::from("/a\nb\u{1b}[2J"),
        errno: Errno::NotFound,
    };
    assert_eq!(
        refused.to_string(),
        format!(
            "cannot stat '/a'$'\\n''b'$'\\033''[2J': {}",
            Errno::NotFound
        )
    );
    let moved = Recorder::new();
    let fs = Tree::new().file("/a\u{1b}", b"x");
    let verbose = Options {
        verbose: true,
        ..Options::DEFAULT
    };
    assert_eq!(
        run(mv_with(verbose, &["/a\u{1b}"], "/b"), &fs, &moved),
        Ok(())
    );
    assert_eq!(moved.text(), "renamed '/a'$'\\033' -> '/b'\n");
}

/// D1365: another user moves the staged copy aside, puts a link to the
/// mover's home at its name and fills the destination so the move must fail.
/// The fill goes on into the node it made, wherever it went, and the copy is
/// discarded through that node: the home the link names is never written,
/// changed or emptied.
#[test]
fn a_failed_directory_move_never_reaches_what_a_planted_link_names() {
    let fs = Tree::new()
        .dir("/src/proj")
        .file("/src/proj/notes", b"mine")
        .dir("/home")
        .file("/home/precious", b"keep me")
        .dir("/b/proj")
        .volume("/src");
    let stage = staged("/b/proj", 1);
    let swapped = stage.clone();
    before(&fs, "entries /src/proj", move |fs| {
        fs.rename(&swapped, "/b/aside");
        fs.relink(&swapped, "/home");
        fs.write("/b/proj/theirs", b"in the way");
    });
    let replacing = Options {
        target_mode: TargetMode::NoDirectory,
        ..Options::DEFAULT
    };
    assert_eq!(
        run(
            mv_with(replacing, &["/src/proj"], "/b/proj"),
            &fs,
            &Recorder::new()
        ),
        shown(&MvError::Rename {
            source: String::from("/src/proj"),
            target: String::from("/b/proj"),
            errno: Errno::Stale
        })
    );
    assert_eq!(paths_under(&fs, "/home"), ["/home", "/home/precious"]);
    assert_eq!(fs.bytes("/home/precious").as_deref(), Some(&b"keep me"[..]));
    assert_eq!(fs.mode("/home"), Some(0o755));
    assert_eq!(
        fs.kind(&stage),
        Some(Kind::Link),
        "the planted link is left"
    );
    assert_eq!(
        paths_under(&fs, "/b/aside"),
        ["/b/aside"],
        "the copy is emptied"
    );
    assert_eq!(fs.bytes("/src/proj/notes").as_deref(), Some(&b"mine"[..]));
}

/// D1367: the staged file's name is swapped for a link to a victim as the
/// fill begins. Its bytes and mode go to the node made, never through the
/// link, and the placement finds the name no longer holds it: the victim is
/// untouched, nothing is put in place and the source is kept.
#[test]
fn a_staged_file_swapped_for_a_link_is_neither_written_through_nor_put_in_place() {
    let fs = Tree::new()
        .file("/src/a", b"new")
        .with_mode("/src/a", 0o600)
        .file("/victim", b"theirs")
        .volume("/src");
    let stage = staged("/dst", 1);
    let swapped = stage.clone();
    before(&fs, "open_file /src/a", move |fs| {
        fs.rename(&swapped, "/aside");
        fs.relink(&swapped, "/victim");
    });
    assert_eq!(
        run(mv(false, &["/src/a"], "/dst"), &fs, &Recorder::new()),
        shown(&MvError::Rename {
            source: String::from("/src/a"),
            target: String::from("/dst"),
            errno: Errno::Stale
        })
    );
    assert_eq!(fs.bytes("/victim").as_deref(), Some(&b"theirs"[..]));
    assert_eq!(fs.mode("/victim"), Some(0o644));
    assert_eq!(fs.link_target(&stage).as_deref(), Some("/victim"));
    assert!(!fs.exists("/dst"));
    assert_eq!(fs.bytes("/src/a").as_deref(), Some(&b"new"[..]));
}

/// D1378: a file appended and a file added while a directory is copied are
/// kept, with the directory holding them, and each is reported; only what was
/// copied unchanged is removed.
#[test]
fn what_is_added_or_appended_during_a_move_is_kept_and_reported() {
    let fs = Tree::new()
        .dir("/src/d")
        .file("/src/d/f", b"before")
        .file("/src/d/g", b"copied")
        .volume("/src");
    let stage = staged("/dst", 1);
    before(&fs, &format!("rename {stage} -> /dst"), |fs| {
        fs.write("/src/d/f", b"before, and appended");
        fs.write("/src/d/new", b"added");
    });
    let changed = |path: &str| MvError::Changed(String::from(path));
    assert_eq!(
        run(mv(false, &["/src/d"], "/dst"), &fs, &Recorder::new()),
        Err(format!(
            "mv: {}\nmv: {}\n",
            changed("/src/d/f"),
            changed("/src/d")
        ))
    );
    assert_eq!(fs.bytes("/dst/f").as_deref(), Some(&b"before"[..]));
    assert_eq!(fs.bytes("/dst/g").as_deref(), Some(&b"copied"[..]));
    assert_eq!(
        fs.bytes("/src/d/f").as_deref(),
        Some(&b"before, and appended"[..])
    );
    assert_eq!(fs.bytes("/src/d/new").as_deref(), Some(&b"added"[..]));
    assert!(
        !fs.exists("/src/d/g"),
        "what was copied unchanged is removed"
    );
}

#[test]
fn a_change_kept_is_named_quoted() {
    assert_eq!(
        MvError::Changed(String::from("/src/d\nf")).to_string(),
        "not removing '/src/d'$'\\n''f', which changed while it was moved"
    );
}
