//! The operation flows, over a filesystem the operations really change: the
//! in-memory tree behind the shared tree seam, listed, probed and operated on
//! as one model of the files. What the engine does to a tree is its own
//! tests' (`tairix_fscopy::op`); these drive it through the session's keys.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::{Errno, FileId, FileKind};
use tairix_appdata::Settings as SettingsStore;
use tairix_curses::{Event, Pos, Size, Window};
use tairix_fscopy::fixture::{staged, Tree};
use tairix_fscopy::op::{OpError, Verb};
use tairix_fscopy::tree::{Dir, Kind, Node, Roots};
use tairix_fscopy::{landing, Landing, Source, Unresolved};
use tairix_procinfo::VolumeBytes;

use super::{
    decode, painted_text, prompt_line, row_text, service, tag, type_text, CapturedInfo, ResizingTty,
};
use crate::app::handle_event;
use crate::fs::{Fs, FsEntry, VolumeInfo};
use crate::model::{Model, Overlay, Pane, Prompt, RepeatOp};
use crate::ops::{self, Run};
use crate::render::render;
use crate::OWN_WORD;

/// The session's seam over a [`Tree`]: what it lists, what its probes and
/// checks answer, and what its operations change are one filesystem.
struct TreeFs {
    tree: Tree,
    volumes: Vec<VolumeInfo>,
}

impl TreeFs {
    fn new(tree: Tree) -> Self {
        Self {
            tree,
            volumes: Vec::new(),
        }
    }

    fn bytes(&self, path: &str) -> Option<Vec<u8>> {
        self.tree.bytes(path)
    }

    fn is_dir(&self, path: &str) -> bool {
        self.tree.kind(path) == Some(Kind::Directory)
    }
}

impl Fs for TreeFs {
    fn list_dir(&mut self, path: &str) -> Result<Vec<FsEntry>, Errno> {
        let dir = self.tree.open_dir(path, true)?;
        dir.entries()?
            .into_iter()
            .map(|entry| {
                let stat = dir.open_node(&entry.name)?.stat()?;
                Ok(FsEntry {
                    name: entry.name,
                    kind: stat.kind,
                    id: entry.id,
                    size: stat.size,
                    modified: stat.times.modified,
                })
            })
            .collect()
    }

    fn volume_space(&mut self, _path: &str) -> Option<VolumeBytes> {
        None
    }

    fn list_volumes(&mut self) -> Vec<VolumeInfo> {
        self.volumes.clone()
    }

    fn stat_mode(&mut self, path: &str) -> Result<u32, Errno> {
        Ok(self.tree.open_node(path, true)?.stat()?.mode)
    }

    fn set_mode(&mut self, path: &str, mode: u32) -> Result<(), Errno> {
        self.tree.open_node(path, true)?.set_mode(mode)
    }

    fn attr_list(&mut self, path: &str) -> Result<Vec<String>, Errno> {
        self.tree.open_node(path, true)?.attr_keys()
    }

    fn attr_get(&mut self, path: &str, key: &str) -> Result<Vec<u8>, Errno> {
        self.tree.open_node(path, true)?.attr_value(key)
    }

    fn attr_set(&mut self, path: &str, key: &str, value: &[u8]) -> Result<(), Errno> {
        self.tree.open_node(path, true)?.attr_set(key, value)
    }

    fn attr_remove(&mut self, path: &str, key: &str) -> Result<(), Errno> {
        self.tree.open_node(path, true)?.attr_remove(key)
    }

    fn stat_kind(&mut self, path: &str) -> Result<FileKind, Errno> {
        Ok(self.tree.open_node(path, false)?.stat()?.kind)
    }

    fn landing(&mut self, src: &str, dir: &str) -> Result<Landing, Unresolved> {
        let source = self.tree.open_dir(src, true).map_err(Unresolved::Source)?;
        let dir = self.tree.open_dir(dir, true).map_err(Unresolved::Target)?;
        landing(&source, &dir)
    }

    fn read(&mut self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        self.tree.open_file(path, true, false)?.read_at(offset, buf)
    }

    fn mkdir(&mut self, path: &str, mode: u32) -> Result<(), Errno> {
        let (dir, name) = self.tree.parent(path)?;
        dir.make_dir(&name, mode).map(drop)
    }

    fn transfer(
        &mut self,
        verb: Verb,
        src: (&str, FileId),
        target: &str,
    ) -> Result<Box<dyn Run>, OpError> {
        Ok(Box::new(ops::transfer(
            &self.tree,
            verb,
            (src, target),
            staged,
        )?))
    }

    fn remove(&mut self, path: (&str, FileId)) -> Result<Box<dyn Run>, OpError> {
        Ok(Box::new(ops::remove(&self.tree, path)?))
    }
}

/// `/` holds `docs/` (holding `guide.md`), `a.rs`, `b.txt` and `big`, each
/// file `x` repeated to its size.
fn tree() -> Tree {
    Tree::new()
        .file("/docs/guide.md", &[b'x'; 5])
        .file("/b.txt", &[b'x'; 10])
        .file("/a.rs", &[b'x'; 30])
        .file("/big", &[b'x'; 999])
}

fn session(fs: &mut TreeFs) -> Model {
    Model::new(fs, "/", String::from("keys: q quits")).expect("root lists")
}

fn key(m: &mut Model, fs: &mut TreeFs, event: &Event) {
    handle_event(m, fs, &mut decode(), event);
}

/// Put the file pane's cursor on `name`.
fn select(m: &mut Model, name: &str) {
    m.pane = Pane::Files;
    m.file_cursor = m
        .visible_files()
        .iter()
        .position(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("the listing holds {name}"));
}

/// Press `verb` (`c`, `m`) on `name` and submit `destination`.
fn transfer(m: &mut Model, fs: &mut TreeFs, name: &str, verb: char, destination: &str) {
    select(m, name);
    key(m, fs, &Event::Char(verb));
    type_text(m, fs, destination);
    key(m, fs, &Event::Enter);
}

/// Rename `name` to `to` through the prompt pre-filled with its name.
fn rename(m: &mut Model, fs: &mut TreeFs, name: &str, to: &str) {
    select(m, name);
    key(m, fs, &Event::Char('r'));
    for _ in 0..name.len() {
        key(m, fs, &Event::Backspace);
    }
    type_text(m, fs, to);
    key(m, fs, &Event::Enter);
}

fn message(m: &Model) -> String {
    m.message.clone().expect("a report")
}

#[test]
fn rename_moves_a_file_within_its_directory() {
    let mut fs = TreeFs::new(tree());
    let mut m = session(&mut fs);
    rename(&mut m, &mut fs, "a.rs", "z.rs");
    assert_eq!(fs.bytes("/a.rs"), None);
    assert_eq!(fs.bytes("/z.rs"), Some(vec![b'x'; 30]));
    assert!(m.files.iter().any(|e| e.name == "z.rs"));
}

#[test]
fn rename_onto_an_existing_file_asks_first() {
    let mut fs = TreeFs::new(tree());
    let mut m = session(&mut fs);
    rename(&mut m, &mut fs, "a.rs", "b.txt");
    assert!(matches!(m.prompt, Some(Prompt::Overwrite(_))));
    key(&mut m, &mut fs, &Event::Char('s'));
    assert_eq!(fs.bytes("/a.rs"), Some(vec![b'x'; 30]));
    assert_eq!(fs.bytes("/b.txt"), Some(vec![b'x'; 10]));
}

/// The name was free when the user typed it; a file another program creates
/// before the rename lands is asked about, never replaced unasked.
#[test]
fn a_rename_asks_about_a_name_created_after_it_was_typed() {
    let mut fs = TreeFs::new(tree());
    fs.tree.before_line(
        "rename /a.rs -> /new.rs",
        Box::new(|tree: &Tree| tree.write("/new.rs", b"theirs")),
    );
    let mut m = session(&mut fs);
    rename(&mut m, &mut fs, "a.rs", "new.rs");
    assert!(matches!(m.prompt, Some(Prompt::Overwrite(_))));
    key(&mut m, &mut fs, &Event::Char('s'));
    assert_eq!(fs.bytes("/new.rs"), Some(b"theirs".to_vec()));
    assert_eq!(fs.bytes("/a.rs"), Some(vec![b'x'; 30]));
}

#[test]
fn delete_removes_a_file_after_confirmation() {
    let mut fs = TreeFs::new(tree());
    let mut m = session(&mut fs);
    select(&mut m, "a.rs");
    key(&mut m, &mut fs, &Event::Char('d'));
    assert!(matches!(m.prompt, Some(Prompt::ConfirmDelete(_))));
    key(&mut m, &mut fs, &Event::Char('y'));
    assert!(!fs.tree.exists("/a.rs"));
    assert!(!m.files.iter().any(|e| e.name == "a.rs"));
}

/// The confirmation names what the listing showed; a file another program
/// renames over that name before the answer is left alone.
#[test]
fn a_delete_leaves_a_name_that_has_come_to_hold_another_file() {
    let mut fs = TreeFs::new(tree());
    let mut m = session(&mut fs);
    select(&mut m, "a.rs");
    key(&mut m, &mut fs, &Event::Char('d'));
    fs.tree.rename("/b.txt", "/a.rs");
    key(&mut m, &mut fs, &Event::Char('y'));
    assert_eq!(fs.bytes("/a.rs"), Some(vec![b'x'; 10]));
    assert_eq!(
        message(&m),
        "/a.rs: changed meanwhile, so it was left as it is"
    );
}

/// A copy or move typed against a listed file refuses a file put under its
/// name while the destination was being typed.
#[test]
fn a_transfer_leaves_a_name_that_has_come_to_hold_another_file() {
    for verb in ['c', 'm'] {
        let mut fs = TreeFs::new(tree());
        let mut m = session(&mut fs);
        select(&mut m, "a.rs");
        key(&mut m, &mut fs, &Event::Char(verb));
        fs.tree.rename("/b.txt", "/a.rs");
        type_text(&mut m, &mut fs, "docs");
        key(&mut m, &mut fs, &Event::Enter);
        assert_eq!(fs.bytes("/a.rs"), Some(vec![b'x'; 10]), "{verb}");
        assert!(!fs.tree.exists("/docs/a.rs"), "{verb}");
    }
}

/// A tag remembers the file it was set on, not just its name.
#[test]
fn a_batch_leaves_a_tagged_name_that_has_come_to_hold_another_file() {
    let mut fs = TreeFs::new(tree());
    let mut m = session(&mut fs);
    select(&mut m, "a.rs");
    key(&mut m, &mut fs, &Event::Char('t'));
    select(&mut m, "big");
    key(&mut m, &mut fs, &Event::Char('t'));
    fs.tree.rename("/b.txt", "/a.rs");
    key(&mut m, &mut fs, &Event::Char('d'));
    key(&mut m, &mut fs, &Event::Char('y'));
    assert_eq!(fs.bytes("/a.rs"), Some(vec![b'x'; 10]));
    assert!(!fs.tree.exists("/big"));
    assert_eq!(
        m.report_lines,
        vec![String::from(
            "/a.rs: changed meanwhile, so it was left as it is"
        )]
    );
}

#[test]
fn delete_removes_a_directory_and_its_contents() {
    let mut fs = TreeFs::new(tree());
    let mut m = session(&mut fs);
    // Tree pane, cursor onto "docs" (which holds guide.md).
    key(&mut m, &mut fs, &Event::Down);
    key(&mut m, &mut fs, &Event::Char('d'));
    key(&mut m, &mut fs, &Event::Char('y'));
    assert!(!fs.tree.exists("/docs"));
    assert_eq!(m.files_dir, "/");
    assert!(m.tree_rows().iter().all(|r| r.path != "/docs"));
    assert_eq!(message(&m), "deleted 2 entries");
}

#[test]
fn copy_streams_a_file_to_a_new_absolute_destination() {
    let mut fs = TreeFs::new(tree());
    let mut m = session(&mut fs);
    transfer(&mut m, &mut fs, "big", 'c', "/big2");
    assert_eq!(fs.bytes("/big2"), Some(vec![b'x'; 999]));
    assert_eq!(fs.bytes("/big"), Some(vec![b'x'; 999]));
    assert!(m.files.iter().any(|e| e.name == "big2" && e.size == 999));
}

#[test]
fn copy_into_an_existing_directory_lands_under_the_base_name() {
    let mut fs = TreeFs::new(tree());
    let mut m = session(&mut fs);
    // A relative destination is joined onto the listed directory.
    transfer(&mut m, &mut fs, "a.rs", 'c', "docs");
    assert_eq!(fs.bytes("/docs/a.rs"), Some(vec![b'x'; 30]));
}

#[test]
fn copy_reproduces_a_directory_tree() {
    let mut fs = TreeFs::new(tree());
    let mut m = session(&mut fs);
    key(&mut m, &mut fs, &Event::Down); // onto docs
    key(&mut m, &mut fs, &Event::Char('c'));
    type_text(&mut m, &mut fs, "/copy");
    key(&mut m, &mut fs, &Event::Enter);
    assert!(fs.is_dir("/copy"));
    assert_eq!(fs.bytes("/copy/guide.md"), Some(vec![b'x'; 5]));
    assert_eq!(fs.bytes("/docs/guide.md"), Some(vec![b'x'; 5]));
    assert_eq!(message(&m), "copied 2 entries");
}

#[test]
fn copy_over_an_existing_file_asks_and_overwrites_on_o() {
    let mut fs = TreeFs::new(tree());
    let mut m = session(&mut fs);
    transfer(&mut m, &mut fs, "big", 'c', "/b.txt");
    assert!(matches!(m.prompt, Some(Prompt::Overwrite(_))));
    // Nothing was written while the question is open.
    assert_eq!(fs.bytes("/b.txt"), Some(vec![b'x'; 10]));
    key(&mut m, &mut fs, &Event::Char('o'));
    assert_eq!(fs.bytes("/b.txt"), Some(vec![b'x'; 999]));
}

#[test]
fn copy_over_an_existing_file_skips_on_s_and_cancels_on_c() {
    let mut fs = TreeFs::new(tree());
    let mut m = session(&mut fs);
    transfer(&mut m, &mut fs, "big", 'c', "/b.txt");
    key(&mut m, &mut fs, &Event::Char('s'));
    assert_eq!(fs.bytes("/b.txt"), Some(vec![b'x'; 10]));
    assert!(message(&m).contains("skipped"), "{}", message(&m));
    transfer(&mut m, &mut fs, "big", 'c', "/b.txt");
    key(&mut m, &mut fs, &Event::Char('c'));
    assert_eq!(fs.bytes("/b.txt"), Some(vec![b'x'; 10]));
    assert!(message(&m).contains("cancelled"), "{}", message(&m));
}

#[test]
fn a_merging_directory_copy_asks_per_conflicting_file() {
    let mut fs = TreeFs::new(
        Tree::new()
            .file("/src/one.txt", &[b'x'; 3])
            .file("/src/two.txt", &[b'x'; 4])
            .file("/dst/src/one.txt", &[b'x'; 9]),
    );
    let mut m = session(&mut fs);
    key(&mut m, &mut fs, &Event::Down); // onto dst
    key(&mut m, &mut fs, &Event::Down); // onto src
    key(&mut m, &mut fs, &Event::Char('c'));
    type_text(&mut m, &mut fs, "/dst");
    key(&mut m, &mut fs, &Event::Enter);
    assert!(matches!(m.prompt, Some(Prompt::Overwrite(_))));
    key(&mut m, &mut fs, &Event::Char('s'));
    assert!(m.prompt.is_none());
    assert_eq!(fs.bytes("/dst/src/one.txt"), Some(vec![b'x'; 9]));
    assert_eq!(fs.bytes("/dst/src/two.txt"), Some(vec![b'x'; 4]));
}

#[test]
fn the_overwrite_question_appears_on_the_message_line() {
    let mut fs = TreeFs::new(tree());
    let mut m = session(&mut fs);
    transfer(&mut m, &mut fs, "big", 'c', "/b.txt");
    let mut window = Window::new(Pos::new(0, 0), Size::new(6, 70));
    render(&m, &mut window);
    let line = row_text(&window, 5);
    assert!(line.starts_with("/b.txt exists"), "{line}");
    assert!(line.contains("o)verwrite"), "{line}");
}

#[test]
fn move_renames_within_one_volume() {
    let mut fs = TreeFs::new(tree());
    let mut m = session(&mut fs);
    transfer(&mut m, &mut fs, "a.rs", 'm', "/docs");
    assert_eq!(fs.bytes("/a.rs"), None);
    assert_eq!(fs.bytes("/docs/a.rs"), Some(vec![b'x'; 30]));
    assert!(!m.files.iter().any(|e| e.name == "a.rs"));
    assert_eq!(message(&m), "moved 1 entry");
}

#[test]
fn move_across_volumes_copies_then_removes_the_source() {
    let mut fs = TreeFs::new(
        Tree::new()
            .dir("/vol")
            .file("/src/data", &[b'x'; 7])
            .volume("/vol"),
    );
    let mut m = session(&mut fs);
    key(&mut m, &mut fs, &Event::Down); // onto src
    key(&mut m, &mut fs, &Event::Char('m'));
    type_text(&mut m, &mut fs, "/vol");
    key(&mut m, &mut fs, &Event::Enter);
    assert!(fs.is_dir("/vol/src"));
    assert_eq!(fs.bytes("/vol/src/data"), Some(vec![b'x'; 7]));
    assert!(!fs.tree.exists("/src"));
}

#[test]
fn a_cross_volume_move_skip_keeps_the_skipped_source() {
    let mut fs = TreeFs::new(
        Tree::new()
            .file("/vol/data", &[b'x'; 9])
            .file("/data", &[b'x'; 7])
            .volume("/vol"),
    );
    let mut m = session(&mut fs);
    transfer(&mut m, &mut fs, "data", 'm', "/vol");
    assert!(matches!(m.prompt, Some(Prompt::Overwrite(_))));
    key(&mut m, &mut fs, &Event::Char('s'));
    assert_eq!(fs.bytes("/vol/data"), Some(vec![b'x'; 9]));
    assert_eq!(fs.bytes("/data"), Some(vec![b'x'; 7]));
    assert!(message(&m).contains("skipped"), "{}", message(&m));
}

/// An entry changed while a move across volumes ran stays where it was, and
/// the report says so.
#[test]
fn a_cross_volume_move_names_what_it_kept() {
    let mut fs = TreeFs::new(
        Tree::new()
            .dir("/vol")
            .file("/src/data", &[b'x'; 7])
            .volume("/vol"),
    );
    fs.tree.before_line(
        "sync /vol/src",
        Box::new(|tree: &Tree| tree.write("/src/data", b"changed")),
    );
    let mut m = session(&mut fs);
    key(&mut m, &mut fs, &Event::Down); // onto src
    key(&mut m, &mut fs, &Event::Char('m'));
    type_text(&mut m, &mut fs, "/vol");
    key(&mut m, &mut fs, &Event::Enter);
    assert_eq!(fs.bytes("/src/data"), Some(b"changed".to_vec()));
    assert_eq!(
        message(&m),
        "moved 2 entries, /src/data kept: it changed meanwhile"
    );
}

#[test]
fn a_read_failure_mid_copy_removes_the_partial_target() {
    let mut fs = TreeFs::new(tree().refuse("read /big", Errno::PermissionDenied));
    let mut m = session(&mut fs);
    transfer(&mut m, &mut fs, "big", 'c', "/copy");
    assert!(message(&m).contains("permission denied"), "{}", message(&m));
    assert!(!fs.tree.exists("/copy"));
    assert!(!m.files.iter().any(|e| e.name == "copy"));
}

#[test]
fn a_write_failure_mid_copy_removes_the_partial_target() {
    let mut fs = TreeFs::new(tree().refuse("write /copy", Errno::NoSpace));
    let mut m = session(&mut fs);
    transfer(&mut m, &mut fs, "big", 'c', "/copy");
    assert!(
        message(&m).contains("no space left on device"),
        "{}",
        message(&m)
    );
    assert!(!fs.tree.exists("/copy"));
}

/// `/src` carries an attribute of its own and holds a sparse file carrying
/// RISC OS and Mac attributes; `/vol` is another volume.
fn metadata() -> Tree {
    Tree::new()
        .dir("/src/sub")
        .sparse(
            "/src/image",
            3 << 20,
            &[(0, &[7; 5000]), (2 << 20, &[9; 100])],
        )
        .attr("/src/image", "acorn.filetype", b"ffd")
        .attr("/src/image", "mac.type", b"TEXT")
        .attr("/src", "acorn.attr", b"DLWR/wr")
        .dir("/vol")
        .volume("/vol")
}

/// Copy (`c`) or move (`m`) the tree row under the root's first child to
/// the typed destination.
fn transfer_src(m: &mut Model, fs: &mut TreeFs, verb: char, destination: &str) {
    key(m, fs, &Event::Down);
    key(m, fs, &Event::Char(verb));
    type_text(m, fs, destination);
    key(m, fs, &Event::Enter);
}

#[test]
fn a_copy_keeps_a_files_holes_and_every_attribute_in_the_tree() {
    let mut fs = TreeFs::new(metadata());
    let mut m = session(&mut fs);
    transfer_src(&mut m, &mut fs, 'c', "/copy");
    assert_eq!(fs.bytes("/copy/image"), fs.bytes("/src/image"));
    assert_eq!(
        fs.tree.written("/copy/image"),
        5100,
        "only the stored runs are written"
    );
    assert_eq!(
        fs.tree.attr_of("/copy/image", "acorn.filetype").as_deref(),
        Some(&b"ffd"[..])
    );
    assert_eq!(
        fs.tree.attr_of("/copy/image", "mac.type").as_deref(),
        Some(&b"TEXT"[..])
    );
    assert_eq!(
        fs.tree.attr_of("/copy", "acorn.attr").as_deref(),
        Some(&b"DLWR/wr"[..]),
        "the new directory's own"
    );
    assert!(fs.is_dir("/copy/sub"));
    assert_eq!(m.message.as_deref(), Some("copied 3 entries"));
    assert!(m.advisories.is_empty());
}

#[test]
fn a_cross_volume_move_keeps_holes_and_attributes_and_removes_the_source() {
    let mut fs = TreeFs::new(metadata());
    let mut m = session(&mut fs);
    transfer_src(&mut m, &mut fs, 'm', "/vol");
    assert_eq!(
        fs.bytes("/vol/src/image").map(|bytes| bytes.len()),
        Some(3 << 20)
    );
    assert_eq!(fs.tree.written("/vol/src/image"), 5100);
    assert_eq!(
        fs.tree
            .attr_of("/vol/src/image", "acorn.filetype")
            .as_deref(),
        Some(&b"ffd"[..])
    );
    assert_eq!(
        fs.tree.attr_of("/vol/src", "acorn.attr").as_deref(),
        Some(&b"DLWR/wr"[..])
    );
    assert!(!fs.tree.exists("/src"));
}

#[test]
fn what_a_destination_cannot_keep_is_said_once_on_the_message_line_and_fd_3() {
    let mut fs = TreeFs::new(metadata().refuses_attr("mac.type", Errno::NotSupported));
    let mut m = session(&mut fs);
    transfer_src(&mut m, &mut fs, 'c', "/copy");
    assert_eq!(
        m.message.as_deref(),
        Some(
            "copied 3 entries — extended attributes not kept on 1 file \
             (the destination cannot store them): mac.type"
        )
    );
    assert_eq!(
        fs.tree.attr_of("/copy/image", "acorn.filetype").as_deref(),
        Some(&b"ffd"[..]),
        "the rest carried"
    );
    assert_eq!(m.advisories.len(), 1);
    let record = String::from_utf8_lossy(&m.advisories[0]).into_owned();
    assert!(record.contains("\"producer\":\"fstree\""), "{record}");
    assert!(record.contains("fs.attributes_not_kept"), "{record}");
}

#[test]
fn an_attribute_refused_for_a_fault_fails_the_copy_and_removes_the_target() {
    let mut fs = TreeFs::new(metadata().refuses_attr("mac.type", Errno::DeviceFault));
    let mut m = session(&mut fs);
    transfer_src(&mut m, &mut fs, 'c', "/copy");
    assert!(
        message(&m).contains("/copy/image: attribute mac.type: device fault"),
        "{}",
        message(&m)
    );
    assert!(!fs.tree.exists("/copy/image"));
}

#[test]
fn a_batch_reports_what_all_its_copies_left_as_one() {
    let mut fs = TreeFs::new(
        metadata()
            .file("/src/sub/note", &[b'x'; 3])
            .attr("/src/sub/note", "acorn.filetype", b"fff")
            .refuses_attr("acorn.filetype", Errno::NotSupported),
    );
    let mut m = session(&mut fs);
    tag(&mut m, "/src/image", FileKind::Regular, 3 << 20);
    tag(&mut m, "/src/sub/note", FileKind::Regular, 3);
    key(&mut m, &mut fs, &Event::Char('c'));
    type_text(&mut m, &mut fs, "/vol");
    key(&mut m, &mut fs, &Event::Enter);
    assert_eq!(
        m.message.as_deref(),
        Some(
            "batch: copied 2 of 2 tagged — extended attributes not kept on 2 files \
             (the destination cannot store them): acorn.filetype"
        )
    );
    assert_eq!(m.advisories.len(), 1, "one record for the whole batch");
    assert_eq!(
        fs.tree.attr_of("/vol/image", "mac.type").as_deref(),
        Some(&b"TEXT"[..])
    );
}

#[test]
fn the_session_delivers_an_operations_advisory_record_to_fd_3() {
    let mut fs = TreeFs::new(metadata().refuses_attr("mac.type", Errno::NotSupported));
    let mut m = session(&mut fs);
    let size = Size::new(24, 90);
    let tty = ResizingTty::new(
        size,
        &[(b"\x1b[B", size), (b"c/copy\r", size), (b"q", size)],
    );
    let mut screen = tairix_curses::Screen::new(tty, tairix_termcap::TermType::Xterm256Color, size);
    let mut host = service();
    let mut store = SettingsStore::open(&mut host, OWN_WORD);
    let mut info = CapturedInfo {
        records: Vec::new(),
    };
    crate::app::run(
        &mut m,
        &mut fs,
        &mut decode(),
        &mut screen,
        &mut info,
        &mut store,
    )
    .expect("session ends");
    assert!(fs.is_dir("/copy"), "the copy ran");
    let copies: Vec<_> = info
        .records
        .iter()
        .filter(|record| record.contains("fs.attributes_not_kept"))
        .collect();
    assert_eq!(copies.len(), 1, "{:?}", info.records);
    assert!(m.advisories.is_empty(), "delivered, not left queued");
}

#[test]
fn batch_delete_continues_past_a_failure_and_reports_it_in_order() {
    let mut fs = TreeFs::new(
        tree()
            .dir("/locked")
            .refuse("entries /locked", Errno::PermissionDenied),
    );
    let mut m = session(&mut fs);
    // Tag order fixes the batch (and report) order: b.txt, locked, a.rs.
    tag(&mut m, "/b.txt", FileKind::Regular, 10);
    tag(&mut m, "/locked", FileKind::Directory, 0);
    tag(&mut m, "/a.rs", FileKind::Regular, 30);
    key(&mut m, &mut fs, &Event::Char('d'));
    assert!(matches!(
        m.prompt,
        Some(Prompt::ConfirmBatchDelete { count: 3 })
    ));
    key(&mut m, &mut fs, &Event::Char('y'));
    assert!(!fs.tree.exists("/b.txt") && !fs.tree.exists("/a.rs"));
    assert!(fs.is_dir("/locked"));
    assert!(m.files.iter().any(|e| e.name == "locked"));
    assert_eq!(
        m.message.as_deref(),
        Some("batch: deleted 2 of 3 tagged, 1 failed")
    );
    assert_eq!(m.overlay, Overlay::Report);
    assert_eq!(
        m.report_lines,
        vec![String::from("/locked: permission denied")]
    );
    assert!(m.tags.contains("/locked"));
    assert_eq!(m.tags.count(), 1);
}

/// A destination directory already holding one of the batch's names, so a
/// copy pauses on the overwrite question.
fn conflicting() -> Tree {
    Tree::new()
        .file("/docs/guide.md", &[b'x'; 5])
        .file("/docs/b.txt", &[b'x'; 5])
        .file("/b.txt", &[b'x'; 10])
        .file("/big", &[b'x'; 999])
}

fn batch_copy_into_docs(m: &mut Model, fs: &mut TreeFs) {
    tag(m, "/b.txt", FileKind::Regular, 10);
    tag(m, "/big", FileKind::Regular, 999);
    key(m, fs, &Event::Char('c'));
    type_text(m, fs, "docs");
    key(m, fs, &Event::Enter);
}

#[test]
fn batch_copy_skip_continues_with_the_rest() {
    let mut fs = TreeFs::new(conflicting());
    let mut m = session(&mut fs);
    batch_copy_into_docs(&mut m, &mut fs);
    assert!(matches!(m.prompt, Some(Prompt::BatchOverwrite(_))));
    key(&mut m, &mut fs, &Event::Char('s'));
    assert_eq!(
        m.message.as_deref(),
        Some("batch: copied 2 of 2 tagged, 1 skipped")
    );
    assert_eq!(fs.bytes("/docs/b.txt").map(|b| b.len()), Some(5));
    assert_eq!(fs.bytes("/docs/big").map(|b| b.len()), Some(999));
    assert!(m.tags.is_empty());
}

#[test]
fn batch_cancel_drops_the_remaining_entries() {
    let mut fs = TreeFs::new(conflicting());
    let mut m = session(&mut fs);
    batch_copy_into_docs(&mut m, &mut fs);
    assert!(matches!(m.prompt, Some(Prompt::BatchOverwrite(_))));
    key(&mut m, &mut fs, &Event::Char('c'));
    assert!(message(&m).ends_with("(cancelled)"), "{}", message(&m));
    assert_eq!(m.report_lines, vec![String::from("/b.txt: cancelled")]);
    assert!(fs.bytes("/docs/big").is_none());
    assert_eq!(m.tags.count(), 2);
}

#[test]
fn confirm_delete_off_deletes_without_a_question() {
    let mut fs = TreeFs::new(tree());
    let mut m = session(&mut fs);
    m.settings.confirm_delete = false;
    select(&mut m, "a.rs");
    key(&mut m, &mut fs, &Event::Char('d'));
    assert!(m.prompt.is_none(), "no question was asked");
    assert!(!fs.tree.exists("/a.rs"), "the file is gone");
}

#[test]
fn confirm_batch_delete_off_runs_the_batch_without_a_question() {
    let mut fs = TreeFs::new(tree());
    let mut m = session(&mut fs);
    m.settings.confirm_batch_delete = false;
    tag(&mut m, "/a.rs", FileKind::Regular, 30);
    tag(&mut m, "/b.txt", FileKind::Regular, 10);
    key(&mut m, &mut fs, &Event::Char('d'));
    assert!(m.prompt.is_none(), "no question was asked");
    assert!(!fs.tree.exists("/a.rs") && !fs.tree.exists("/b.txt"));
}

#[test]
fn dot_repeats_the_last_copy_on_the_new_selection() {
    let mut fs = TreeFs::new(tree());
    let mut m = session(&mut fs);
    transfer(&mut m, &mut fs, "a.rs", 'c', "/docs");
    assert!(fs.bytes("/docs/a.rs").is_some());
    assert_eq!(m.last_op, Some(RepeatOp::CopyInto(String::from("/docs"))));
    select(&mut m, "b.txt");
    key(&mut m, &mut fs, &Event::Char('.'));
    assert!(fs.bytes("/docs/b.txt").is_some());
}

#[test]
fn dot_repeats_a_delete_and_still_asks() {
    let mut fs = TreeFs::new(tree());
    let mut m = session(&mut fs);
    select(&mut m, "a.rs");
    key(&mut m, &mut fs, &Event::Char('d'));
    key(&mut m, &mut fs, &Event::Char('y'));
    assert!(!fs.tree.exists("/a.rs"));
    assert_eq!(m.last_op, Some(RepeatOp::Delete));
    key(&mut m, &mut fs, &Event::Char('.'));
    assert!(matches!(m.prompt, Some(Prompt::ConfirmDelete(_))));
}

/// One scripted session across the stage's surface: tag two files, batch
/// copy them, verify through the volume list, adjust a setting, and quit
/// — every step through the ordinary key loop.
#[test]
fn a_scripted_session_tags_copies_and_visits_the_new_surfaces() {
    let mut fs = TreeFs::new(tree().file("/data/d.log", &[b'x'; 7]));
    fs.volumes = vec![
        VolumeInfo {
            target: String::from("/"),
            fstype: String::from("arxfs"),
            space: Some(VolumeBytes {
                total: 1000,
                free: 500,
                available: 500,
            }),
        },
        VolumeInfo {
            target: String::from("/data"),
            fstype: String::from("ext4"),
            space: None,
        },
    ];
    let mut m = session(&mut fs);
    select(&mut m, "a.rs");
    for event in [Event::Char('t'), Event::Char('t'), Event::Char('c')] {
        key(&mut m, &mut fs, &event);
    }
    type_text(&mut m, &mut fs, "/docs");
    key(&mut m, &mut fs, &Event::Enter);
    assert!(fs.bytes("/docs/a.rs").is_some() && fs.bytes("/docs/b.txt").is_some());
    let rest = [
        Event::Char('V'), // the volume list
        Event::Esc,       // close it
        Event::Char('S'), // the settings menu
        Event::Char('1'), // confirm-delete off (no home: session only)
        Event::Esc,       // close it
        Event::Char('q'), // quit
    ];
    for event in &rest {
        key(&mut m, &mut fs, event);
    }
    assert!(m.quit);
    assert!(!m.settings.confirm_delete);
    assert_eq!(m.overlay, Overlay::None);
}

/// `/` holds a link to a file, one to a directory, and one that names
/// nothing, beside the ordinary entries a transfer targets.
fn links() -> Tree {
    tree()
        .link("/to-file", "/a.rs")
        .link("/to-dir", "/docs")
        .link("/gone", "../removed")
}

#[test]
fn the_file_pane_shows_a_link_as_a_link() {
    let mut fs = TreeFs::new(links());
    let m = session(&mut fs);
    // A link's own byte count is the length of the path it stores, which is
    // no size a reader wants: the column names what the row is.
    let text = painted_text(&m);
    assert!(text.contains("<link>"), "{text}");
}

#[test]
fn copying_a_link_recreates_it_with_the_same_target() {
    let mut fs = TreeFs::new(links());
    let mut m = session(&mut fs);
    transfer(&mut m, &mut fs, "to-file", 'c', "/copy-of-link");
    assert_eq!(fs.tree.kind("/copy-of-link"), Some(Kind::Link));
    assert_eq!(
        fs.tree.link_target("/copy-of-link").as_deref(),
        Some("/a.rs")
    );
    assert_eq!(fs.tree.kind("/to-file"), Some(Kind::Link));
    assert_eq!(fs.bytes("/a.rs"), Some(vec![b'x'; 30]));
}

#[test]
fn copying_a_dangling_link_still_recreates_it() {
    let mut fs = TreeFs::new(links());
    let mut m = session(&mut fs);
    transfer(&mut m, &mut fs, "gone", 'c', "/still-gone");
    assert_eq!(fs.tree.kind("/still-gone"), Some(Kind::Link));
    assert_eq!(
        fs.tree.link_target("/still-gone").as_deref(),
        Some("../removed")
    );
}

#[test]
fn copying_a_tree_recreates_the_links_inside_it() {
    let mut fs = TreeFs::new(
        Tree::new()
            .file("/tree/f", &[b'x'; 3])
            .link("/tree/inner", "/a.rs")
            .file("/a.rs", &[b'x'; 30]),
    );
    let mut m = session(&mut fs);
    transfer(&mut m, &mut fs, "tree", 'c', "/tree-copy");
    assert_eq!(fs.bytes("/tree-copy/f"), Some(vec![b'x'; 3]));
    assert_eq!(fs.tree.kind("/tree-copy/inner"), Some(Kind::Link));
    assert_eq!(
        fs.tree.link_target("/tree-copy/inner").as_deref(),
        Some("/a.rs")
    );
}

#[test]
fn deleting_a_link_removes_the_link_and_not_its_target() {
    let mut fs = TreeFs::new(links());
    let mut m = session(&mut fs);
    select(&mut m, "to-dir");
    key(&mut m, &mut fs, &Event::Char('d'));
    key(&mut m, &mut fs, &Event::Char('y'));
    assert!(!fs.tree.exists("/to-dir"));
    assert!(fs.is_dir("/docs"));
    assert_eq!(fs.bytes("/docs/guide.md"), Some(vec![b'x'; 5]));
}

#[test]
fn moving_a_link_moves_the_link() {
    let mut fs = TreeFs::new(links().dir("/other").volume("/other"));
    let mut m = session(&mut fs);
    // Across volumes the link is recreated and the original removed, still
    // never touching what it names.
    transfer(&mut m, &mut fs, "to-file", 'm', "/other/moved");
    assert_eq!(fs.tree.kind("/other/moved"), Some(Kind::Link));
    assert_eq!(
        fs.tree.link_target("/other/moved").as_deref(),
        Some("/a.rs")
    );
    assert!(!fs.tree.exists("/to-file"));
    assert_eq!(fs.bytes("/a.rs"), Some(vec![b'x'; 30]));
}

#[test]
fn a_link_at_the_destination_is_asked_about_and_replaced_not_written_through() {
    let mut fs = TreeFs::new(links());
    let mut m = session(&mut fs);
    transfer(&mut m, &mut fs, "b.txt", 'c', "/to-file");
    assert!(matches!(m.prompt, Some(Prompt::Overwrite(_))));
    assert!(
        prompt_line(&m).contains("symbolic link"),
        "{}",
        prompt_line(&m)
    );
    assert_eq!(fs.tree.kind("/to-file"), Some(Kind::Link));
    key(&mut m, &mut fs, &Event::Char('o'));
    assert_eq!(fs.tree.kind("/to-file"), Some(Kind::File));
    assert_eq!(fs.bytes("/to-file"), Some(vec![b'x'; 10]));
    assert_eq!(fs.bytes("/a.rs"), Some(vec![b'x'; 30]));
}

#[test]
fn a_directory_is_never_transferred_onto_a_link() {
    let mut fs = TreeFs::new(links());
    let mut m = session(&mut fs);
    transfer(&mut m, &mut fs, "docs", 'c', "/gone");
    assert!(message(&m).contains("different kind"), "{}", message(&m));
    assert_eq!(fs.tree.kind("/gone"), Some(Kind::Link));
}

#[test]
fn a_failed_start_changes_nothing_and_says_why() {
    let mut fs = TreeFs::new(tree().refuse("parent /docs/a.rs", Errno::PermissionDenied));
    let mut m = session(&mut fs);
    transfer(&mut m, &mut fs, "a.rs", 'c', "docs");
    assert_eq!(
        message(&m),
        format!("/docs/a.rs: {}", Errno::PermissionDenied)
    );
    assert!(fs.bytes("/docs/a.rs").is_none());
    let names = m.files.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
    assert!(names.contains(&String::from("a.rs")));
}
