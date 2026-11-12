//! Deterministic fuzz harness for `COMMAND.COM`, typed at with any line and
//! running any batch file, on a booted system. For any sequence of lines:
//!
//! 1. Nothing panics, and the machine comes back to wanting a key within a
//!    bound: no line runs on.
//! 2. Once a line is done the memory chain walks to its end and the shell
//!    holds only its own block and its environment.
//!
//! Every question asked is answered: a critical error failed, a yes or no
//! given either way, a pause ended, `EDIT` left without saving; a Ctrl+C
//! ends what waits for input before the probe looks. Lines are built from
//! the shell's own words — commands, switches, names there and not,
//! patterns, devices, redirections, pipes, variables, batch parameters — and
//! noise; the disk's two batch files hold such lines too, but can neither
//! loop nor run each other round, so a line that runs on is the shell's
//! fault, and its `EDIT.INI` hostile settings; a folder shared as C: is
//! reached by the same words. Never typed is what takes the console
//! from the harness: `CTTY`, or a shell that runs on. A probe program of the
//! harness's own counts the shell's blocks. Runs the fixed smoke sweep under
//! plain `cargo test`; keeps drawing from the same seeded stream until
//! `TAIRIX_FUZZ_BUDGET_SECS` elapses under `cargo xtask fuzz`.

use tairix_fuzzseed::Prng;
use tairix_pcemu::asm::{Asm, Cond, Reg16};
use tairix_pcemu::bios::{self, Firmware};
use tairix_pcemu::chips::keyboard::{us_key, Scan, SHIFT};
use tairix_pcemu::cpu::Bus;
use tairix_pcemu::dos;
use tairix_pcemu::dos::volume::{self, File};
use tairix_pcemu::folder::memhost::MemHost;
use tairix_pcemu::folder::server::Server;
use tairix_pcemu::folder::{DosName, Share};
use tairix_pcemu::machine::{Machine, Template};
use tairix_pcemu::storage::floppy::Floppy;
use tairix_pcemu::storage::MemoryDisk;
use tairix_pcemu::time::{Nanos, NANOS_PER_SECOND};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 1;
/// Lines a run types, and how often the probe looks.
const LINES: usize = 24;
const PROBE_EVERY: usize = 6;
/// Questions one line may ask before the next line is typed.
const ROUNDS: usize = 12;
const SECOND: Nanos = NANOS_PER_SECOND;
/// What `FILE.TXT` holds.
const FILE_TEXT: &[u8] = b"the file's line\r\n";
/// Past every disk's slowest retries.
const RETRIES: Nanos = 8 * SECOND;
/// How long anything may run before it wants a key from the console: its
/// retries, as many times over as there are keys in `FILE.TXT`, since a
/// prompt answered from it takes a byte a key, and a drive put right is
/// tried again at each.
const BUSY: Nanos = (FILE_TEXT.len() as u64 + 1) * RETRIES;
/// Where the probe leaves its count: the BIOS's intra-application area.
const MARK: u32 = 0x4F0;
/// The count before the probe has run, and when the chain is broken.
const UNRUN: u16 = 0xEEEE;
const BROKEN: u16 = 0xFFFF;
const CTRL: Scan = Scan::plain(0x1D);
const C: Scan = Scan::plain(0x2E);
const ALT: Scan = Scan::plain(0x38);
const ESC: Scan = Scan::plain(0x01);
const F: Scan = Scan::plain(0x21);
const X: Scan = Scan::plain(0x2D);
/// `EDIT`'s menu bar, along the top of the screen while it runs.
const EDIT_BAR: &[u8] = b"  File  Edit  Search  Options";

/// The first words of a line.
const WORDS: &[&str] = &[
    "dir",
    "type",
    "copy",
    "del",
    "erase",
    "ren",
    "md",
    "cd",
    "rd",
    "set",
    "path",
    "echo",
    "ver",
    "vol",
    "date",
    "time",
    "cls",
    "break",
    "verify",
    "chcp",
    "truename",
    "rem",
    "if",
    "for",
    "goto",
    "shift",
    "call",
    "pause",
    "exit",
    "lh",
    "t1",
    "t2.bat",
    "a:",
    "b:",
    "z:",
    "missing",
    "@echo",
    ":here",
    "file.txt",
    "dir\\inner.txt",
    "find",
    "find /v /c",
    "find /n /i",
    "sort",
    "sort /r",
    "sort /+3",
    "more",
    "attrib",
    "attrib +r",
    "attrib -a +h",
    "attrib /s",
    "label",
    "label a:",
    "tree",
    "tree /f",
    "tree /a /f",
    "mem",
    "mem /p",
    "mem /d",
    "mem /c",
    // No serial retry: a port the mouse never readies would retry for ever
    // until Ctrl+Break, as DOS's does. No DELTREE: a pattern after it would
    // take the probe's directory. FORMAT only of B:, and DISKCOPY only from
    // it to itself: it holds no disk. FDISK finds no fixed disk to change.
    "mode",
    "mode /status",
    "mode con",
    "mode lpt1",
    "mode lpt1:132,8",
    "mode lpt1:,,p",
    "mode lpt1=com1",
    "mode lpt2=com1",
    "mode com1:96,n,8,1",
    "mode co80",
    "mode bw40",
    "mode 40",
    "mode mono",
    "mode ,r",
    "mode ,l,t",
    "mode con lines=43",
    "fc",
    "fc /b",
    "fc /a /n",
    "fc /w /c /lb3",
    "fc /1",
    "move",
    "move /y",
    "move /-y",
    "xcopy",
    "xcopy /s",
    "xcopy /s /e /v",
    "xcopy /p /m",
    "xcopy /d:1-1-90 /a",
    "xcopy /w",
    "chkdsk",
    "chkdsk /v",
    "chkdsk /f",
    "chkdsk a: /f /v",
    "chkdsk *.*",
    "sys",
    "sys b:",
    "sys a: b:",
    "sys a:\\safe b:",
    "format",
    "format b:",
    "format b: /s",
    "format b: /v:fuzz /1",
    "format b: /4 /8",
    "format b: /t:40 /n:9",
    "format b: /f:360",
    "format b: /b /s",
    "diskcopy b: b:",
    "diskcopy b: b: /1",
    "diskcopy a:x b:",
    "diskcopy a: d:",
    "diskcopy a: b: c:",
    "fdisk",
    "fdisk /q",
    "fdisk 1",
    "fdisk 1 /pri:30 /q",
    "fdisk 2 /ext:5 /log:1",
    "fdisk 3 /pri:1",
    "fdisk /pri:x",
    "doskey",
    "doskey /h",
    "doskey /m",
    "doskey /insert",
    "doskey /bufsize=256",
    "doskey /reinstall",
    "doskey x=echo $1$techo $*",
    "doskey x=",
    "doskey y=dir $2 $g$g out.txt",
    "x",
    "y",
    "edit",
    "edit file.txt",
    "edit /b /h",
    "edit missing.txt /nohi",
    "edit dir",
    "edit /zz",
    "edit a b",
    // The shared folder, drive C:.
    "c:",
    "dir c:",
    "type c:file.txt",
    "copy file.txt c:",
    "copy c:file.txt b:",
    "cd c:dir",
    "md c:made",
    "rd c:made",
    "del c:*.txt",
    "ren c:file.txt *.bak",
    "attrib c:*.*",
    "vol c:",
    "label c:",
    "chkdsk c:",
    "tree c: /f",
    "xcopy c:\\*.* b: /s",
];
/// The folder shared as drive C: a file and a directory with one.
fn folder() -> Share {
    let host = MemHost::new()
        .file("file.txt", b"shared\r\n")
        .file("dir/inner.txt", b"inner\r\n");
    let label = DosName::new(*b"SHARED     ", false).unwrap();
    Share::new(
        label,
        true,
        Box::new(Server::new(host.root(), label, true, 0x5C)),
    )
}

/// What follows them.
const ARGS: &[&str] = &[
    "file.txt",
    "c:",
    "c:*.*",
    "c:\\dir",
    "c:file.txt",
    "*.*",
    "*.txt",
    "f?le.*",
    "b:*.*",
    "z:",
    "1:",
    "@:",
    "a:\\",
    "\\",
    "..",
    ".",
    "dir",
    "dir\\inner.txt",
    "nowhere\\x",
    "new.txt",
    "con",
    "nul",
    "prn",
    "aux",
    "/s",
    "/w",
    "/p",
    "/b",
    "/o",
    "/o-n",
    "/os",
    "/a:h",
    "/a-d",
    "/l",
    "/?",
    "/a",
    "/v",
    "on",
    "off",
    "exist",
    "not",
    "errorlevel",
    "1",
    "==",
    "x==x",
    "%1",
    "%2",
    "%0",
    "%path%",
    "%%v",
    "in",
    "(*.*)",
    "(a b c)",
    "do",
    "echo",
    "here",
    "\"quoted line\"",
    "+",
    ",",
    ";",
    "=",
    "12-25-1999",
    "2-30-99",
    "13:45",
    "25:61:99",
    "t1",
    "t2",
];
/// What joins them.
const GLUE: &[&str] = &[
    " ",
    " ",
    " ",
    " ",
    " >out.txt",
    " >>out.txt",
    " <file.txt",
    " | t1",
    " |",
    ">",
    "<",
    " >nul",
    "  ",
];

/// A hostile line: the shell's words, or noise.
fn line(rng: &mut Prng) -> String {
    if rng.below(12) == 0 {
        let len = rng.below(40);
        return (0..len)
            .map(|_| char::from(b' ' + u8::try_from(rng.below(95)).unwrap()))
            .collect();
    }
    let mut out = String::from(*rng.pick(WORDS));
    for _ in 0..rng.below(5) {
        out.push_str(rng.pick(GLUE));
        out.push_str(rng.pick(ARGS));
    }
    if rng.below(15) == 0 {
        out = format!("command /c {out}");
    }
    out
}

/// A batch file of hostile lines that names none of `unnamed`, nor itself
/// through `%0`, which still expands, so no batch file runs itself; and
/// jumps only forward, to its last line, the one label `here` names.
fn batch(rng: &mut Prng, unnamed: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..=rng.below(8) {
        let mut text = match rng.below(8) {
            0 => String::from("goto here"),
            1 => String::from("shift"),
            2 => String::from("echo %1 %2"),
            _ => line(rng),
        };
        for name in unnamed {
            text = text.replace(name, "missing");
        }
        text = text.replace("%0", "%0x").replace(":here", ":hare");
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b":here\r\n");
    out
}

/// What `EDIT.INI` holds, which `EDIT` reads as untrusted text: its own
/// settings with values in and out of range, other names, and noise.
fn edit_ini(rng: &mut Prng) -> Vec<u8> {
    const LINES: &[&str] = &[
        "Colors=1E",
        "Colors=FF",
        "Colors=",
        "colors = 4f",
        "ScrollBars=0",
        "ScrollBars=7",
        "TabStops=4",
        "TabStops=0",
        "TabStops=999",
        "[EDIT]",
        "=",
        "x=y",
    ];
    let mut out = Vec::new();
    for _ in 0..rng.below(12) {
        if rng.below(4) == 0 {
            out.extend((0..rng.below(40)).map(|_| u8::try_from(rng.below(256)).unwrap()));
        } else {
            out.extend_from_slice(rng.pick(LINES).as_bytes());
        }
        out.extend_from_slice(if rng.below(3) == 0 { b"\n" } else { b"\r\n" });
    }
    out
}

/// The probe: walks the memory chain from the kernel's list of lists and
/// leaves how many blocks its parent holds at [`MARK`], or [`BROKEN`].
fn probe() -> Vec<u8> {
    let mut asm = Asm::new(0x100);
    let (walk, kind_ok, not_its, broken, done) = (
        asm.label(),
        asm.label(),
        asm.label(),
        asm.label(),
        asm.label(),
    );
    asm.bytes(&[0xB4, 0x62, 0xCD, 0x21]); // mov ah, 62h; int 21h
    asm.bytes(&[0x8E, 0xC3]); // mov es, bx
    asm.bytes(&[0x26, 0x8B, 0x16, 0x16, 0x00]); // mov dx, es:[16h]: the parent
    asm.bytes(&[0xB4, 0x52, 0xCD, 0x21]); // mov ah, 52h; int 21h
    asm.bytes(&[0x26, 0x8B, 0x47, 0xFE]); // mov ax, es:[bx-2]: the first block
    asm.zero(Reg16::Cx);
    asm.mov_imm16(Reg16::Si, 0x1000);
    asm.bind(&walk);
    asm.bytes(&[0x8E, 0xC0]); // mov es, ax
    asm.bytes(&[0x26, 0x8A, 0x1E, 0x00, 0x00]); // mov bl, es:[0]
    asm.bytes(&[0x80, 0xFB, b'M']); // cmp bl, 'M'
    asm.jcc(Cond::E, &kind_ok);
    asm.bytes(&[0x80, 0xFB, b'Z']); // cmp bl, 'Z'
    asm.jcc(Cond::Ne, &broken);
    asm.bind(&kind_ok);
    asm.bytes(&[0x26, 0x39, 0x16, 0x01, 0x00]); // cmp es:[1], dx
    asm.jcc(Cond::Ne, &not_its);
    asm.bytes(&[0x41]); // inc cx
    asm.bind(&not_its);
    asm.bytes(&[0x80, 0xFB, b'Z']); // cmp bl, 'Z'
    asm.jcc(Cond::E, &done);
    asm.bytes(&[0x26, 0x03, 0x06, 0x03, 0x00]); // add ax, es:[3]
    asm.bytes(&[0x40, 0x4E]); // inc ax; dec si
    asm.jcc(Cond::Ne, &walk);
    asm.bind(&broken);
    asm.mov_imm16(Reg16::Cx, BROKEN);
    asm.bind(&done);
    asm.mov_imm16(Reg16::Ax, 0x0040);
    asm.bytes(&[0x8E, 0xC0]); // mov es, ax
    asm.bytes(&[0x26, 0x89, 0x0E, 0xF0, 0x00]); // mov es:[0F0h], cx
    asm.mov_imm16(Reg16::Ax, 0x4C00);
    asm.int(0x21);
    asm.finish().unwrap()
}

/// The disk a run boots: the shell, files and a directory to work on, two
/// batch files of hostile lines, and the probe where no line reaches it.
fn disk(rng: &mut Prng) -> Floppy {
    let medium = tairix_fat::format::Medium {
        sectors: 720,
        bytes_per_sector: 512,
        sectors_per_track: 9,
        heads: 2,
        hidden_sectors: 0,
        media: 0xFD,
    };
    let shape = tairix_fat::format::Shape {
        kind: Some(tairix_fat::boot::Kind::Fat12),
        sectors_per_cluster: Some(2),
        root_entries: 112,
        serial: 1,
        label: None,
    };
    let bpb = tairix_fat::format::plan(&medium, &shape).unwrap();
    let (shell, probe) = (tairix_pcemu::dos::command_com().unwrap(), probe());
    let (first, second) = (batch(rng, &["t1"]), batch(rng, &["t1", "t2"]));
    let settings = edit_ini(rng);
    let files = [
        File {
            path: "COMMAND.COM",
            bytes: &shell,
            attributes: 0,
        },
        File {
            path: "AUTOEXEC.BAT",
            bytes: b"@echo off\r\nprompt $p$g\r\n",
            attributes: 0,
        },
        File {
            path: "FILE.TXT",
            bytes: FILE_TEXT,
            attributes: 0,
        },
        File {
            path: "DIR\\INNER.TXT",
            bytes: b"inner",
            attributes: 0,
        },
        File {
            path: "T1.BAT",
            bytes: &first,
            attributes: 0,
        },
        File {
            path: "T2.BAT",
            bytes: &second,
            attributes: 0,
        },
        File {
            path: "SAFE\\PROBE.COM",
            bytes: &probe,
            attributes: 0x01,
        },
        File {
            path: "EDIT.INI",
            bytes: &settings,
            attributes: 0,
        },
    ];
    let utilities = tairix_pcemu::dos::utilities().unwrap();
    let files: Vec<File<'_>> = files
        .into_iter()
        .chain(utilities.iter().map(|(name, image)| File {
            path: name,
            bytes: image,
            attributes: 0,
        }))
        .collect();
    let image = volume::volume(&bpb, None, &files, tairix_fat::stamp::Stamp::NONE).unwrap();
    Floppy::raw(Box::new(MemoryDisk::new(image, false).unwrap())).unwrap()
}

struct Run {
    machine: Machine,
}

impl Run {
    fn new(rng: &mut Prng) -> Self {
        let config = Template::IbmPc.config();
        let mut machine = Machine::new(&config).unwrap();
        Firmware::new(&config, [None, None])
            .unwrap()
            .install(machine.board_mut())
            .unwrap();
        machine.board_mut().insert_floppy(0, disk(rng)).unwrap();
        machine.board_mut().share(0, folder());
        let mut run = Self { machine };
        run.settle();
        run
    }

    fn peek(&self, linear: u32) -> u8 {
        self.machine.board().peek(linear)
    }

    fn peek16(&self, linear: u32) -> u16 {
        u16::from_le_bytes([self.peek(linear), self.peek(linear + 1)])
    }

    /// The text page and the cursor: what a wait for a key leaves alone.
    fn screen(&self) -> (Vec<u8>, u16) {
        let text = (0..80 * 25 * 2)
            .map(|at| self.peek(0xB_8000 + at))
            .collect();
        (text, self.peek16(0x450))
    }

    /// The screen's columns, as the BIOS keeps them: `MODE` may set 40.
    fn columns(&self) -> u32 {
        u32::from(self.peek16(0x44A)).clamp(1, 80)
    }

    /// The cursor's line as far as the cursor; at a line's start, the line
    /// before, which a question ending in a new line leaves the cursor
    /// under. At 40 columns a question may wrap, so the row before is read
    /// too.
    fn asking(&self) -> String {
        let columns = self.columns();
        let (column, row) = (u32::from(self.peek(0x450)), u32::from(self.peek(0x451)));
        let (first, row, width) = match (column, row) {
            (0, 1..) => (row - 1, row - 1, columns),
            (_, 1..) if columns < 80 => (row - 1, row, column.min(columns)),
            _ => (row, row, column.min(columns)),
        };
        let cell = |row: u32, c: u32| char::from(self.peek(0xB_8000 + (row * columns + c) * 2));
        let before = (first..row).flat_map(|row| (0..columns).map(move |c| (row, c)));
        before
            .chain((0..width).map(|c| (row, c)))
            .map(|(row, c)| cell(row, c))
            .collect()
    }

    /// Whether DOS waits at the console for a key, with none waiting for
    /// it; a halt anywhere else waits on a device.
    fn idle(&self) -> bool {
        let empty = self.peek16(0x41A) == self.peek16(0x41C);
        dos::waits_for_key(&self.machine) && empty
    }

    /// Runs until the machine waits for a key, which it must within
    /// [`BUSY`].
    fn settle(&mut self) {
        let begun = self.machine.now();
        let mut last = self.screen();
        let mut quiet = 0;
        loop {
            let now = self.machine.now();
            bios::run_until(&mut self.machine, now + SECOND / 20);
            let seen = self.screen();
            quiet = if self.idle() && seen == last {
                quiet + 1
            } else {
                0
            };
            if quiet >= 3 {
                return;
            }
            last = seen;
            let running = self.machine.now() - begun;
            assert!(
                running < BUSY,
                "it ran on:\n{}\n{:X?}",
                self.shown(),
                self.machine.cpu().regs()
            );
        }
    }

    /// The screen's text, a line a row.
    fn shown(&self) -> String {
        let (text, _) = self.screen();
        let stride = usize::try_from(self.columns() * 2).unwrap();
        let rows: Vec<String> = text
            .chunks(stride)
            .take(25)
            .map(|row| {
                row.iter()
                    .step_by(2)
                    .map(|&b| char::from(b))
                    .collect::<String>()
            })
            .collect();
        rows.iter()
            .map(|row| row.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn key(&mut self, key: Scan, down: bool) {
        let now = self.machine.now();
        bios::run_until(&mut self.machine, now + SECOND / 50);
        self.machine.board_mut().key(key, down);
    }

    fn break_in(&mut self) {
        for (key, down) in [(CTRL, true), (C, true), (C, false), (CTRL, false)] {
            self.key(key, down);
        }
    }

    /// Types `text`, each character on the key that makes it.
    fn type_text(&mut self, text: &str) {
        for byte in text.bytes() {
            let Some((key, shifted)) = us_key(byte) else {
                continue;
            };
            if shifted {
                self.key(SHIFT, true);
            }
            self.key(key, true);
            self.key(key, false);
            if shifted {
                self.key(SHIFT, false);
            }
        }
    }

    /// Whether `EDIT` holds the screen.
    fn editing(&self) -> bool {
        let (text, _) = self.screen();
        text.iter()
            .step_by(2)
            .take(EDIT_BAR.len())
            .copied()
            .eq(EDIT_BAR.iter().copied())
    }

    /// `EDIT` left: what is open in it cancelled, File's Exit chosen, and
    /// the document's changes not saved.
    fn leave_edit(&mut self) {
        for _ in 0..3 {
            self.key(ESC, true);
            self.key(ESC, false);
        }
        for (key, down) in [
            (ALT, true),
            (F, true),
            (F, false),
            (ALT, false),
            (X, true),
            (X, false),
        ] {
            self.key(key, down);
        }
        self.settle();
        if self.shown().contains("Save it now?") {
            self.type_text("n");
        }
    }

    /// Answers the question the system asks, if it is one it knows: `false`
    /// when the wait is for anything else, the prompt among them.
    fn answer(&mut self, rng: &mut Prng, ending: bool) -> bool {
        let asking = self.asking();
        let asking = asking.trim_end();
        if self.editing() {
            self.leave_edit();
        } else if asking.ends_with("Fail?") {
            self.type_text("f");
        } else if asking.ends_with("D = directory)?") {
            self.type_text(if rng.below(2) == 0 { "f" } else { "d" });
        } else if asking.ends_with("(Y/N)?") || asking.ends_with("(YES/NO)") {
            let answer = if ending || rng.below(2) == 0 {
                "y"
            } else {
                "n"
            };
            self.type_text(answer);
            self.settle();
            // A question read as a line keeps its answer's echo where it was
            // asked until Enter takes it.
            if self
                .asking()
                .trim_end()
                .ends_with(&[asking, answer].concat())
            {
                self.type_text("\r");
            }
        } else if asking.contains("Press any key")
            || asking.ends_with("-- More --")
            || asking.ends_with("ENTER for none)?")
            || asking.ends_with("press ENTER when ready...")
        {
            self.type_text("\r");
        } else {
            return false;
        }
        self.settle();
        true
    }

    /// Types `text` and Enter, then answers what it asks.
    fn enter(&mut self, rng: &mut Prng, text: &str, ending: bool) {
        self.type_text(text);
        self.type_text("\r");
        self.settle();
        for _ in 0..ROUNDS {
            if !self.answer(rng, ending) {
                return;
            }
        }
    }

    /// Breaks whatever runs, ends any batch file, and has the probe count
    /// the shell's blocks.
    fn check(&mut self, rng: &mut Prng) {
        for _ in 0..3 {
            for at in 0..2 {
                self.machine.board_mut().write(
                    MARK + at,
                    UNRUN.to_le_bytes()[usize::try_from(at).unwrap()],
                    0,
                );
            }
            self.break_in();
            self.settle();
            for _ in 0..ROUNDS {
                if !self.answer(rng, true) {
                    break;
                }
            }
            self.enter(rng, "a:\\safe\\probe", true);
            match self.peek16(MARK) {
                UNRUN => {}
                BROKEN => panic!("the memory chain is broken"),
                held => {
                    assert_eq!(
                        held, 2,
                        "the shell holds its block and its environment, no more"
                    );
                    return;
                }
            }
        }
        panic!(
            "the probe never ran: the shell lost its prompt at {:?}",
            self.asking()
        );
    }
}

fn run(rng: &mut Prng) {
    let mut run = Run::new(rng);
    run.check(rng);
    for n in 0..LINES {
        let text = line(rng);
        run.enter(rng, &text, false);
        if n % PROBE_EVERY == PROBE_EVERY - 1 {
            run.check(rng);
        }
    }
    run.check(rng);
}

#[test]
fn hostile_lines_never_panic_hang_or_leak() {
    let seed = tairix_fuzzseed::start(
        "hostile_lines_never_panic_hang_or_leak",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    );
    let mut rng = Prng::new(seed);
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut iteration = 0;
    while if deadline.is_some() {
        tairix_fuzzseed::within_budget(deadline)
    } else {
        iteration < SMOKE_ITERATIONS
    } {
        run(&mut rng);
        iteration += 1;
    }
}
