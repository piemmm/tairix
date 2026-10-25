//! Behavioural tests for `stat`'s report engine, over in-memory seams (no
//! kernel).

use super::*;
use crate::command::parse;
use alloc::vec;
use core::cell::RefCell;

use tairix_abi::driver::filesystem::{MountFlags, NodeTimes, VolumeStats};
use tairix_abi::sysinfo::{MountAvailability, MountRecord, MountVolumeState, ViewBindingRecord};
use tairix_abi::FileId;
use tairix_help::SourceError;

/// A stamp with a known civil rendering: 2024-05-17 03:04:05.000000006 UTC.
const STAMP_SECS: i64 = 1_715_915_045;

/// That stamp as a [`Time64`]. Its nanoseconds are in range by
/// construction, so a rejection here would be a defect in the constant.
fn stamp() -> Time64 {
    Time64::new(STAMP_SECS, 6).unwrap_or(Time64::UNIX_EPOCH)
}

/// An in-memory tree: each path answers a stat, a stored target, and a
/// canonical spelling.
#[derive(Default)]
struct MockFs {
    nodes: Vec<(String, FileStat)>,
    targets: Vec<(String, String)>,
    canonical: Vec<(String, String)>,
    /// Whether the `-L` posture reached the seam, per call.
    asked: RefCell<Vec<(String, bool)>>,
}

impl Filesystem for MockFs {
    fn stat(&self, path: &str, dereference: bool) -> Result<FileStat, Errno> {
        self.asked
            .borrow_mut()
            .push((String::from(path), dereference));
        self.nodes
            .iter()
            .find(|(p, _)| p == path)
            .map_or(Err(Errno::NotFound), |(_, s)| Ok(*s))
    }

    fn read_link(&self, path: &str) -> Result<String, Errno> {
        self.targets
            .iter()
            .find(|(p, _)| p == path)
            .map_or(Err(Errno::OutOfRange), |(_, t)| Ok(t.clone()))
    }

    fn canonicalize(&self, path: &str) -> Result<String, Errno> {
        self.canonical
            .iter()
            .find(|(p, _)| p == path)
            .map_or_else(|| Ok(String::from(path)), |(_, c)| Ok(c.clone()))
    }
}

/// A namespace fixture: each volume shown from its root at its own path. An
/// empty one stands for a caller who cannot read the namespace.
struct MockMounts(Vec<(MountRecord, ViewBindingRecord)>);

impl Mounts for MockMounts {
    fn namespace(&self) -> Result<Namespace, Errno> {
        Ok(Namespace::new(
            self.0.iter().map(|(volume, _)| *volume).collect(),
            self.0.iter().map(|(_, view)| *view).collect(),
        ))
    }
}

/// A user directory fixture.
struct MockNames(Option<&'static str>);

impl Names for MockNames {
    fn user(&self, _uid: u32) -> Option<String> {
        self.0.map(String::from)
    }
}

/// A stream fixture capturing everything written.
#[derive(Default)]
struct MockOut(RefCell<String>);

impl MockOut {
    fn text(&self) -> String {
        self.0.borrow().clone()
    }
}

impl Output for MockOut {
    fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
        self.0
            .borrow_mut()
            .push_str(core::str::from_utf8(bytes).unwrap_or("<non-utf8>"));
        Ok(())
    }
}

/// A [`HelpSource`] with no documents, so `run` falls back to [`USAGE`].
struct NoHelp;

impl HelpSource for NoHelp {
    fn locale_dirs(&self) -> Result<Vec<String>, SourceError> {
        Ok(Vec::new())
    }

    fn read(&self, _locale_dir: &str, _file_name: &str) -> Result<Option<Vec<u8>>, SourceError> {
        Ok(None)
    }
}

fn stat_of(kind: FileKind, mode: u32) -> FileStat {
    FileStat {
        kind,
        nlink: 3,
        size: 1234,
        allocated: 2048,
        mode,
        uid: 1000,
        gid: 1001,
        id: FileId {
            volume: [
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
                0x0f, 0x10,
            ],
            node: 42,
        },
        times: NodeTimes {
            created: Time64::UNIX_EPOCH,
            modified: stamp(),
            accessed: stamp(),
            changed: stamp(),
        },
        content_gen: 0,
    }
}

/// A volume of `fstype` and `block_size` shown from its root at `target`,
/// identified by `target` so each fixture volume is distinct.
fn mount(target: &str, fstype: &str, block_size: u32) -> (MountRecord, ViewBindingRecord) {
    let mut id = [0xA5u8; 16];
    for (slot, byte) in id.iter_mut().zip(target.bytes()) {
        *slot ^= byte;
    }
    let usage = VolumeStats {
        block_size,
        in_reserve: false,
        total_blocks: 100,
        free_blocks: 40,
        avail_blocks: 30,
        files: 20,
        files_free: 7,
    };
    let volume = MountRecord::new(
        b"vol",
        target.as_bytes(),
        fstype.as_bytes(),
        MountFlags::default(),
        MountVolumeState {
            usage,
            availability: MountAvailability::Available,
            medium: None,
        },
        id,
    )
    .expect("record");
    let view =
        ViewBindingRecord::new(target.as_bytes(), b"/", MountFlags::default(), id).expect("view");
    (volume, view)
}

/// Render `args` over `fs`/`mounts`/`names`, returning `(stdout, stderr,
/// clean)`.
fn go(
    args: &[&str],
    fs: &MockFs,
    mounts: &MockMounts,
    names: &MockNames,
) -> (String, String, bool) {
    let out = MockOut::default();
    let err = MockOut::default();
    let reporter = Reporter {
        fs,
        mounts,
        names,
        out: &out,
        err: &err,
    };
    let command = parse(args).expect("parse");
    let clean = run(command, None, &reporter, &NoHelp).expect("run");
    (out.text(), err.text(), clean)
}

fn tree() -> MockFs {
    MockFs {
        nodes: vec![
            (
                String::from("/vol/file"),
                stat_of(FileKind::Regular, 0o100_644),
            ),
            (
                String::from("/vol/dir"),
                stat_of(FileKind::Directory, 0o40_755),
            ),
            (
                String::from("/vol/link"),
                stat_of(FileKind::Symlink, 0o120_777),
            ),
        ],
        targets: vec![(String::from("/vol/link"), String::from("../real/name"))],
        canonical: vec![(String::from("/vol/link"), String::from("/vol/real/name"))],
        asked: RefCell::new(Vec::new()),
    }
}

#[test]
fn each_file_specifier_renders_its_own_field() {
    let fs = tree();
    let mounts = MockMounts(vec![mount("/vol", "arxfs", 4096)]);
    let names = MockNames(Some("ada"));
    for (spec, want) in [
        ("%a", "644"),
        ("%A", "-rw-r--r--"),
        // 2048 bytes of allocation is four 512-byte blocks, and `%B` states
        // the unit the count is in.
        ("%b", "4"),
        ("%B", "512"),
        ("%f", "81a4"),
        ("%F", "regular file"),
        ("%g", "1001"),
        ("%h", "3"),
        ("%i", "42"),
        ("%m", "/vol"),
        ("%n", "/vol/file"),
        ("%N", "'/vol/file'"),
        ("%o", "4096"),
        ("%s", "1234"),
        ("%u", "1000"),
        ("%U", "ada"),
        ("%D", "0102030405060708090a0b0c0d0e0f10"),
    ] {
        let (out, err, clean) = go(&["-c", spec, "/vol/file"], &fs, &mounts, &names);
        assert_eq!(out, alloc::format!("{want}\n"), "{spec}");
        assert!(err.is_empty() && clean, "{spec}");
    }
}

#[test]
fn the_volume_id_is_rendered_as_the_128_bit_value_it_is() {
    // TAIRiX identifies a volume by a 16-byte id rather than a device
    // number, so `%d` is that id's decimal and `%D` its hex — the same
    // value in two spellings, never a truncation of it.
    let fs = tree();
    let mounts = MockMounts(vec![mount("/vol", "arxfs", 4096)]);
    let names = MockNames(None);
    let (hex, _, _) = go(&["-c%D", "/vol/file"], &fs, &mounts, &names);
    let (decimal, _, _) = go(&["-c%d", "/vol/file"], &fs, &mounts, &names);
    assert_eq!(hex.trim_end(), "0102030405060708090a0b0c0d0e0f10");
    let value = u128::from_str_radix(hex.trim_end(), 16).expect("hex parses");
    assert_eq!(decimal.trim_end(), alloc::format!("{value}"));
}

#[test]
fn a_link_is_described_as_itself_unless_dereferenced() {
    let fs = tree();
    let mounts = MockMounts(vec![mount("/vol", "arxfs", 4096)]);
    let names = MockNames(None);
    // `%N` shows the link beside what it stores; `%F` names the kind.
    let (out, _, _) = go(&["-c", "%F %N", "/vol/link"], &fs, &mounts, &names);
    assert_eq!(out, "symbolic link '/vol/link' -> '../real/name'\n");
    // The posture reaches the seam rather than being decided here.
    let _ = go(&["-L", "-c%F", "/vol/link"], &fs, &mounts, &names);
    let asked = fs.asked.borrow().clone();
    assert!(
        asked.iter().any(|(p, deref)| p == "/vol/link" && *deref),
        "{asked:?}"
    );
    assert!(
        asked.iter().any(|(p, deref)| p == "/vol/link" && !*deref),
        "{asked:?}"
    );
}

#[test]
fn a_stamp_the_format_does_not_keep_reads_as_absent() {
    // The fixture's creation stamp is the epoch, which is how a driver says
    // "this format keeps no birth time": GNU spells that `-`, and the
    // seconds form `0`.
    let fs = tree();
    let mounts = MockMounts(Vec::new());
    let names = MockNames(None);
    let (out, _, _) = go(&["-c", "%w %W", "/vol/file"], &fs, &mounts, &names);
    assert_eq!(out, "- 0\n");
    let (out, _, _) = go(&["-c", "%y %Y", "/vol/file"], &fs, &mounts, &names);
    assert_eq!(
        out,
        alloc::format!("2024-05-17 03:04:05.000000006 +0000 {STAMP_SECS}\n")
    );
}

#[test]
fn a_fact_the_platform_cannot_supply_renders_as_a_question_mark() {
    // No mount snapshot: `%m` and `%o` have nothing to report, and say so
    // rather than substituting a plausible path or block size.
    let fs = tree();
    let mounts = MockMounts(Vec::new());
    let names = MockNames(None);
    let (out, _, _) = go(&["-c", "%m %o", "/vol/file"], &fs, &mounts, &names);
    assert_eq!(out, "? ?\n");
    // Nor a user name: GNU's own `UNKNOWN`, never the number, so a name
    // field never quietly becomes a numeric one.
    let (out, _, _) = go(&["-c%U", "/vol/file"], &fs, &mounts, &names);
    assert_eq!(out, "UNKNOWN\n");
}

#[test]
fn the_mount_point_is_the_longest_prefix_of_the_canonical_path() {
    // Mounts nest, so first-match would report the wrong one; and the
    // *canonical* path is what decides it, so a link into another volume
    // reports the volume it lands on rather than the one it was typed under.
    let fs = MockFs {
        nodes: vec![(
            String::from("/a/link"),
            stat_of(FileKind::Symlink, 0o120_777),
        )],
        targets: vec![(String::from("/a/link"), String::from("/deep/inner/x"))],
        canonical: vec![(String::from("/a/link"), String::from("/deep/inner/x"))],
        asked: RefCell::new(Vec::new()),
    };
    let mounts = MockMounts(vec![
        mount("/", "arxfs", 4096),
        mount("/deep", "ext4", 1024),
        mount("/deep/inner", "fat32", 512),
    ]);
    let names = MockNames(None);
    let (out, _, _) = go(&["-c", "%m %o", "/a/link"], &fs, &mounts, &names);
    assert_eq!(out, "/deep/inner 512\n");
}

#[test]
fn a_mount_point_matches_whole_components_only() {
    let fs = MockFs {
        nodes: vec![(
            String::from("/volume/x"),
            stat_of(FileKind::Regular, 0o100_644),
        )],
        ..MockFs::default()
    };
    let mounts = MockMounts(vec![mount("/", "arxfs", 4096), mount("/vol", "ext4", 1024)]);
    let names = MockNames(None);
    // `/vol` must not claim `/volume/x`; the root mount covers it.
    let (out, _, _) = go(&["-c%m", "/volume/x"], &fs, &mounts, &names);
    assert_eq!(out, "/\n");
}

#[test]
fn the_filesystem_vocabulary_reports_the_volume() {
    let fs = tree();
    let mounts = MockMounts(vec![mount("/vol", "arxfs", 4096)]);
    let names = MockNames(None);
    let (out, _, clean) = go(
        &["-f", "-c", "%a %b %c %d %f %l %s %S %T %n", "/vol/file"],
        &fs,
        &mounts,
        &names,
    );
    assert_eq!(
        out,
        alloc::format!(
            "30 100 20 7 40 {} 4096 4096 arxfs /vol/file\n",
            tairix_abi::FS_NAME_MAX
        )
    );
    assert!(clean);
    // `%i` is the volume's own identity — the same value the file
    // vocabulary's `%D` reports for a node on it.
    let (id, _, _) = go(&["-f", "-c%i", "/vol/file"], &fs, &mounts, &names);
    let (device, _, _) = go(&["-c%D", "/vol/file"], &fs, &mounts, &names);
    assert_eq!(id, device);
}

/// GNU 9.4's default report keeps its indentation and column widths: the
/// mode zero-padded to four digits, the ids right-aligned.
#[test]
fn the_default_report_is_laid_out_as_gnu_lays_it_out() {
    let fs = tree();
    let mounts = MockMounts(vec![mount("/vol", "arxfs", 4096)]);
    let names = MockNames(None);
    let (out, _, clean) = go(&["/vol/file"], &fs, &mounts, &names);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("  File: "), "{out}");
    assert!(lines[1].starts_with("  Size: "), "{out}");
    assert!(lines[3].starts_with("Access: (0"), "{out}");
    assert!(
        lines[3].contains("  Uid: (") && lines[3].contains(")   Gid: ("),
        "{out}"
    );
    assert_eq!(
        lines.get(7).map(|line| &line[..8]),
        Some(" Birth: "),
        "{out}"
    );
    assert!(clean);
}

/// GNU 9.4's default `-f` layout: the name in double quotes, and the block
/// size beside the fundamental one.
#[test]
fn the_default_filesystem_report_is_laid_out_as_gnu_lays_it_out() {
    let fs = tree();
    let mounts = MockMounts(vec![mount("/vol", "arxfs", 4096)]);
    let names = MockNames(None);
    let (out, _, clean) = go(&["-f", "/vol/file"], &fs, &mounts, &names);
    let mut lines = out.lines();
    assert_eq!(lines.next(), Some("  File: \"/vol/file\""));
    assert!(lines.next().is_some_and(|id| id.starts_with("    ID: ")));
    assert_eq!(
        lines.next(),
        Some("Block size: 4096       Fundamental block size: 4096")
    );
    assert!(clean);
}

#[test]
fn an_absent_operand_is_diagnosed_and_the_run_continues() {
    let fs = tree();
    let mounts = MockMounts(vec![mount("/vol", "arxfs", 4096)]);
    let names = MockNames(None);
    let (out, err, clean) = go(&["-c%n", "/vol/gone", "/vol/file"], &fs, &mounts, &names);
    assert_eq!(out, "/vol/file\n");
    assert!(err.contains("/vol/gone"), "{err}");
    assert!(!clean, "a refused operand exits non-zero");
}

#[test]
fn the_default_and_terse_forms_render_every_field_they_name() {
    let fs = tree();
    let mounts = MockMounts(vec![mount("/vol", "arxfs", 4096)]);
    let names = MockNames(Some("ada"));
    let (full, _, _) = go(&["/vol/link"], &fs, &mounts, &names);
    // The full form names the link and what it stores, its kind, and its
    // mode in both spellings.
    assert!(full.contains("'/vol/link' -> '../real/name'"), "{full}");
    assert!(full.contains("symbolic link"), "{full}");
    assert!(full.contains("(0777/lrwxrwxrwx)"), "{full}");
    assert!(full.ends_with('\n'), "{full}");
    // The terse form is one line of space-separated fields.
    let (terse, _, _) = go(&["-t", "/vol/file"], &fs, &mounts, &names);
    assert_eq!(terse.lines().count(), 1, "{terse}");
    assert!(terse.starts_with("/vol/file 1234 4 "), "{terse}");
    let (terse_fs, _, _) = go(&["-tf", "/vol/file"], &fs, &mounts, &names);
    assert_eq!(terse_fs.lines().count(), 1, "{terse_fs}");
    assert!(terse_fs.contains("arxfs"), "{terse_fs}");
}

#[test]
fn help_falls_back_to_the_usage_banner() {
    let fs = tree();
    let mounts = MockMounts(Vec::new());
    let names = MockNames(None);
    let (out, _, clean) = go(&["-?"], &fs, &mounts, &names);
    assert_eq!(out, USAGE);
    assert!(clean);
}

/// A capturing stream the kernel attests as a terminal.
#[derive(Default)]
struct Console(MockOut);

impl Output for Console {
    fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
        self.0.write_all(bytes)
    }

    fn at_terminal(&self) -> bool {
        true
    }
}

#[test]
fn a_name_is_escaped_at_a_terminal_and_kept_in_a_pipe() {
    let fs = MockFs {
        nodes: vec![(
            String::from("/vol/e\u{1b}x"),
            stat_of(FileKind::Regular, 0o100_644),
        )],
        ..MockFs::default()
    };
    let mounts = MockMounts(vec![mount("/vol", "arxfs", 4096)]);
    let names = MockNames(None);
    for args in [&["-c", "%n"][..], &["-f", "-c", "%n"][..]] {
        let mut argv = args.to_vec();
        argv.push("/vol/e\u{1b}x");
        let out = Console::default();
        let err = MockOut::default();
        let reporter = Reporter {
            fs: &fs,
            mounts: &mounts,
            names: &names,
            out: &out,
            err: &err,
        };
        assert!(run(parse(&argv).expect("parse"), None, &reporter, &NoHelp).expect("run"));
        assert_eq!(out.0.text(), "'/vol/e'$'\\033''x'\n", "{args:?}");
        assert_eq!(
            go(&argv, &fs, &mounts, &names).0,
            "/vol/e\u{1b}x\n",
            "{args:?}"
        );
    }
}
