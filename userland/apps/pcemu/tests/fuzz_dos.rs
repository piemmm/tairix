//! Deterministic fuzz harness for TAIRiX DOS, which programs call through
//! INT 21h with any registers, pointing at any bytes, and whose EXEC loads
//! whatever a disk holds. On a booted system, for any sequence of calls:
//!
//! 1. Nothing panics, and every call returns within the slowest disk
//!    retry's time.
//! 2. No call is left in progress: `InDOS` is back to zero.
//! 3. The memory chain still walks from its first block to its last.
//!
//! Calls point only at the harness's own area and free only the blocks it
//! allocated, so what is hostile is what a program may say to DOS, not a
//! program overwriting DOS's own memory, which DOS does not defend. Half
//! the paths, FCBs and packets are well formed but wrong, the rest noise;
//! a run also writes executables with hostile MZ headers and relocations
//! and loads them without running them, and reads and writes the disk's
//! sectors directly, corrupting the volume the calls after it work on. A
//! folder is shared as drive C:, and every call reaches it as readily as
//! the diskette. Runs the fixed smoke sweep under
//! plain `cargo test`; keeps drawing from the same seeded stream until
//! `TAIRIX_FUZZ_BUDGET_SECS` elapses under `cargo xtask fuzz`.

use tairix_fuzzseed::Prng;
use tairix_pcemu::asm::Asm;
use tairix_pcemu::bios::{self, Firmware};
use tairix_pcemu::cpu::{flag, gpr, Bus, Regs, Seg};
use tairix_pcemu::dos::volume::{self, File};
use tairix_pcemu::folder::memhost::MemHost;
use tairix_pcemu::folder::server::Server;
use tairix_pcemu::folder::{DosName, Share};
use tairix_pcemu::machine::{Machine, Template};
use tairix_pcemu::storage::floppy::Floppy;
use tairix_pcemu::storage::MemoryDisk;
use tairix_pcemu::time::{Nanos, NANOS_PER_SECOND};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 2;
/// Calls a run makes.
const CALLS: usize = 120;
/// The longest a call may take: every retry of a failing diskette read.
const LONGEST: Nanos = 30 * NANOS_PER_SECOND;
const SECOND: Nanos = NANOS_PER_SECOND;
/// Where the call stub lies in the shell's segment.
const STUB: u16 = 0x0200;
/// The area calls point into, and its size.
const AREA: u16 = 0x0400;
const AREA_LEN: usize = 0x800;
/// Paths a run names: files and directories that are there and are not,
/// devices, patterns, a root, and names DOS cuts or refuses.
const PATHS: &[&str] = &[
    "A:\\FILE.TXT",
    "A:\\DIR\\INNER.TXT",
    "A:\\DIR",
    "A:\\NEW.DAT",
    "A:\\NOWHERE\\X.TXT",
    "A:\\",
    "",
    "CON",
    "NUL",
    "A:\\????????.???",
    "A:\\*.TXT",
    "B:\\EMPTY.TXT",
    "Z:\\NO.DRV",
    "A:\\DIR\\..\\FILE.TXT",
    "A:\\..\\..",
    "A:\\VERYLONGNAMEISCUT.TEXT",
    "A:\\HOSTILE.EXE",
    "C:\\FILE.TXT",
    "C:\\DIR\\INNER.TXT",
    "C:\\DIR",
    "C:\\NEW.DAT",
    "C:\\LONGNA~1.TEX",
    "C:\\NOWHERE\\X.TXT",
    "C:\\",
    "C:\\*.*",
    "C:\\DIR\\..\\FILE.TXT",
    "C:LOCKED.TXT",
];

/// The folder shared as drive C: a file, a directory with one, a name DOS
/// reaches by its alias, and a file the host keeps from change.
fn folder() -> Share {
    let host = MemHost::new()
        .file("file.txt", &[0x42; 1500])
        .file("DIR/inner.txt", b"inner")
        .file("Long Name.text", b"long")
        .file("locked.txt", b"kept")
        .locked("locked.txt");
    let label = DosName::new(*b"SHARED     ", false).unwrap();
    Share::new(
        label,
        true,
        Box::new(Server::new(host.root(), label, true, 0x0C)),
    )
}

/// The idle shell: it waits with interrupts on, for ever.
fn idle() -> Vec<u8> {
    let mut asm = Asm::new(0x100);
    let here = asm.label();
    asm.sti();
    asm.bind(&here);
    asm.hlt();
    asm.jmp_short(&here);
    asm.finish().unwrap()
}

/// The volume a run boots: the shell, a file, and a directory with one.
fn disk() -> Floppy {
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
    let shell = idle();
    let files = [
        File {
            path: "COMMAND.COM",
            bytes: &shell,
            attributes: 0,
        },
        File {
            path: "FILE.TXT",
            bytes: &[0x41; 1500],
            attributes: 0,
        },
        File {
            path: "DIR\\INNER.TXT",
            bytes: b"inner",
            attributes: 0,
        },
    ];
    let image = volume::volume(&bpb, None, &files, tairix_fat::stamp::Stamp::NONE).unwrap();
    Floppy::raw(Box::new(MemoryDisk::new(image, false).unwrap())).unwrap()
}

struct Run {
    machine: Machine,
    psp: u16,
    handles: Vec<u16>,
    blocks: Vec<u16>,
}

impl Run {
    fn new() -> Self {
        let config = Template::IbmPc.config();
        let mut machine = Machine::new(&config).unwrap();
        Firmware::new(&config, [None, None])
            .unwrap()
            .install(machine.board_mut())
            .unwrap();
        machine.board_mut().insert_floppy(0, disk()).unwrap();
        machine.board_mut().share(0, folder());
        let mut deadline = SECOND;
        loop {
            bios::run_until(&mut machine, deadline);
            let regs = machine.cpu().regs();
            if (0x101..0x104).contains(&regs.ip) && regs.sreg(Seg::Cs) > 0x100 {
                let psp = regs.sreg(Seg::Cs);
                let mut run = Self {
                    machine,
                    psp,
                    handles: Vec::new(),
                    blocks: Vec::new(),
                };
                let shrunk = run.call(|r| {
                    r.gpr[gpr::AX] = 0x4A00;
                    r.gpr[gpr::BX] = 0x1000;
                    r.set_sreg(Seg::Es, psp);
                });
                assert!(!shrunk.flag(flag::CF), "the shell's block shrinks");
                return run;
            }
            assert!(deadline < 30 * SECOND, "TAIRiX DOS booted");
            deadline += SECOND / 2;
        }
    }

    fn put(&mut self, offset: u16, bytes: &[u8]) {
        for (at, &byte) in (offset..).zip(bytes) {
            let linear = u32::from(self.psp) * 16 + u32::from(at);
            self.machine.board_mut().write(linear, byte, 0);
        }
    }

    fn peek(&self, segment: u16, offset: u16) -> u8 {
        self.machine
            .board()
            .peek((u32::from(segment) * 16 + u32::from(offset)) & 0xF_FFFF)
    }

    fn peek16(&self, segment: u16, offset: u16) -> u16 {
        u16::from_le_bytes([
            self.peek(segment, offset),
            self.peek(segment, offset.wrapping_add(1)),
        ])
    }

    /// Makes INT `vector` from the stub, every segment register the
    /// shell's but those `set` changes.
    fn int(&mut self, vector: u8, set: impl FnOnce(&mut Regs)) -> Regs {
        self.put(STUB, &[0xCD, vector, 0xEB, 0xFE]);
        let psp = self.psp;
        let cpu = self.machine.cpu_mut();
        cpu.reset();
        let regs = cpu.regs_mut();
        for seg in [Seg::Cs, Seg::Ss, Seg::Ds, Seg::Es] {
            regs.set_sreg(seg, psp);
        }
        regs.ip = STUB;
        regs.gpr[gpr::SP] = 0xFFF0;
        set(regs);
        regs.set_sreg(Seg::Cs, psp);
        regs.set_sreg(Seg::Ss, psp);
        regs.ip = STUB;
        regs.gpr[gpr::SP] = 0xFFF0;
        let begun = self.machine.now();
        loop {
            let now = self.machine.now();
            bios::run_until(&mut self.machine, now + SECOND / 20);
            let regs = *self.machine.cpu().regs();
            if regs.sreg(Seg::Cs) == psp && regs.ip == STUB + 2 {
                return regs;
            }
            assert!(
                self.machine.now() - begun < LONGEST,
                "INT {vector:02X}h returned: {regs:X?}"
            );
        }
    }

    fn call(&mut self, set: impl FnOnce(&mut Regs)) -> Regs {
        self.int(0x21, set)
    }

    /// Keeps a handle a call opened for the calls after it, unless it is the
    /// console's input, which a read rightly waits on for a key: that one is
    /// closed.
    fn keep(&mut self, handle: u16) {
        let info = self.call(|r| {
            r.gpr[gpr::AX] = 0x4400;
            r.gpr[gpr::BX] = handle;
        });
        if info.gpr[gpr::DX] & 0x81 == 0x81 {
            self.call(|r| {
                r.gpr[gpr::AX] = 0x3E00;
                r.gpr[gpr::BX] = handle;
            });
            return;
        }
        self.handles.push(handle);
    }

    /// `InDOS` is zero and the memory chain walks to its end.
    fn check(&mut self) {
        let indos = self.call(|r| r.gpr[gpr::AX] = 0x3400);
        let count = self.peek(indos.sreg(Seg::Es), indos.gpr[gpr::BX]);
        assert_eq!(count, 0, "no call is left in DOS");
        let lists = self.call(|r| r.gpr[gpr::AX] = 0x5200);
        let mut block = self.peek16(lists.sreg(Seg::Es), lists.gpr[gpr::BX].wrapping_sub(2));
        for _ in 0..0x1000 {
            let kind = self.peek(block, 0);
            assert!(
                matches!(kind, b'M' | b'Z'),
                "the memory chain is intact at {block:04X}"
            );
            if kind == b'Z' {
                return;
            }
            block = block.wrapping_add(self.peek16(block, 3)).wrapping_add(1);
        }
        panic!("the memory chain ends");
    }

    /// Puts an ASCIZ path at `offset`: one of the harness's, or noise.
    fn path(&mut self, rng: &mut Prng, offset: u16) {
        let mut bytes = if rng.below(4) == 0 {
            let len = rng.below(140);
            rng.noise(len)
        } else {
            rng.pick(PATHS).as_bytes().to_vec()
        };
        bytes.push(0);
        self.put(offset, &bytes);
    }

    /// Writes a file of `bytes` through DOS.
    fn write_file(&mut self, name: &str, bytes: &[u8]) {
        let mut path = name.as_bytes().to_vec();
        path.push(0);
        self.put(AREA, &path);
        let made = self.call(|r| {
            r.gpr[gpr::AX] = 0x3C00;
            r.gpr[gpr::CX] = 0;
            r.gpr[gpr::DX] = AREA;
        });
        if made.flag(flag::CF) {
            return;
        }
        let handle = made.gpr[gpr::AX];
        for chunk in bytes.chunks(0x400) {
            self.put(AREA + 0x100, chunk);
            self.call(|r| {
                r.gpr[gpr::AX] = 0x4000;
                r.gpr[gpr::BX] = handle;
                r.gpr[gpr::CX] = u16::try_from(chunk.len()).unwrap();
                r.gpr[gpr::DX] = AREA + 0x100;
            });
        }
        self.call(|r| {
            r.gpr[gpr::AX] = 0x3E00;
            r.gpr[gpr::BX] = handle;
        });
    }

    /// Writes an executable with a hostile MZ header and loads it without
    /// running it; whatever it loaded is given back.
    fn hostile_exec(&mut self, rng: &mut Prng) {
        let len = 32 + rng.below(3000);
        let mut image = rng.noise(len);
        image[..2].copy_from_slice(b"MZ");
        let any = rng.next_u16();
        let pages = u16::try_from(image.len().div_ceil(512)).unwrap();
        let words: [u16; 12] = [
            *rng.pick(&[0u16, 1, 0x1FF, 0x200, any]),
            pages.wrapping_add(*rng.pick(&[0u16, 0, 1, 0xFFFF])),
            *rng.pick(&[0u16, 1, 4, 0x7FFF, any]),
            *rng.pick(&[0u16, 2, 4, 0xFFFF, any]),
            *rng.pick(&[0u16, 0x10, 0x1000, 0xFFFF, any]),
            *rng.pick(&[0u16, 0x10, 0xFFFF, any]),
            rng.next_u16(),
            rng.next_u16(),
            rng.next_u16(),
            rng.next_u16(),
            rng.next_u16(),
            *rng.pick(&[0x1Cu16, 0x40, 0xFFFF, any]),
        ];
        for (n, word) in words.into_iter().enumerate() {
            image[2 + n * 2..4 + n * 2].copy_from_slice(&word.to_le_bytes());
        }
        self.write_file("A:\\HOSTILE.EXE", &image);
        self.put(AREA, b"A:\\HOSTILE.EXE\0");
        let tail = AREA + 0x40;
        self.put(tail, &[0, 0x0D]);
        let psp = self.psp;
        let mut block = [0u8; 18];
        for at in [2usize, 6, 10] {
            block[at..at + 2].copy_from_slice(&tail.to_le_bytes());
            block[at + 2..at + 4].copy_from_slice(&psp.to_le_bytes());
        }
        self.put(AREA + 0x60, &block);
        let loaded = self.call(|r| {
            r.gpr[gpr::AX] = 0x4B01;
            r.gpr[gpr::DX] = AREA;
            r.gpr[gpr::BX] = AREA + 0x60;
        });
        if loaded.flag(flag::CF) {
            return;
        }
        let child = self.call(|r| r.gpr[gpr::AX] = 0x6200).gpr[gpr::BX];
        let environment = self.peek16(child, 0x2C);
        self.call(|r| {
            r.gpr[gpr::AX] = 0x5000;
            r.gpr[gpr::BX] = psp;
        });
        for segment in [environment, child] {
            self.call(|r| {
                r.gpr[gpr::AX] = 0x4900;
                r.set_sreg(Seg::Es, segment);
            });
        }
    }
}

/// The functions a run calls: every one that neither waits for a key, nor
/// ends or starts a program, nor writes where a program chose outside its
/// own memory.
const FUNCTIONS: &[u8] = &[
    0x02, 0x06, 0x09, 0x0B, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x19,
    0x1A, 0x1B, 0x1C, 0x1F, 0x21, 0x22, 0x23, 0x24, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E,
    0x2F, 0x30, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F,
    0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x4D, 0x4E, 0x4F, 0x51, 0x52,
    0x54, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x5B, 0x5D, 0x60, 0x62, 0x63, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6C, 0x70, 0xFF,
];

fn run(rng: &mut Prng) {
    let mut run = Run::new();
    for n in 0..CALLS {
        if rng.below(25) == 0 {
            run.hostile_exec(rng);
            continue;
        }
        if rng.below(40) == 0 {
            let vector = *rng.pick(&[0x25u8, 0x26]);
            let noise = rng.noise(0x400);
            run.put(AREA, &noise);
            let (drive, sector, count) = (
                rng.below(3),
                rng.next_u16() % 800,
                *rng.pick(&[0u16, 1, 2, 0xFFFF]),
            );
            run.int(vector, |r| {
                r.gpr[gpr::AX] = u16::try_from(drive).unwrap();
                r.gpr[gpr::CX] = count;
                r.gpr[gpr::DX] = sector;
                r.gpr[gpr::BX] = AREA;
            });
            continue;
        }
        let ah = *rng.pick(FUNCTIONS);
        let mut area = rng.noise(AREA_LEN);
        if rng.below(2) == 0 {
            area[0x11..0x1C].copy_from_slice(b"????????TXT");
            area[0x01..0x0C].copy_from_slice(b"FILE    TXT");
            area[0] = *rng.pick(&[0, 1, 3]);
        }
        run.put(AREA, &area);
        run.path(rng, AREA + 0x200);
        run.path(rng, AREA + 0x300);
        let handle = match ah {
            0x3F | 0x40 | 0x42 | 0x45 | 0x46 | 0x57 | 0x68 => run
                .handles
                .get(rng.below(run.handles.len() + 1))
                .copied()
                .unwrap_or(20),
            _ => rng.next_u16(),
        };
        let segment = match ah {
            0x49 | 0x4A => run
                .blocks
                .get(rng.below(run.blocks.len() + 1))
                .copied()
                .unwrap_or(1),
            _ => run.psp,
        };
        let pointer = *rng.pick(&[AREA, AREA + 0x200, AREA + 0x300]);
        let al = if rng.below(3) == 0 {
            rng.next_u8()
        } else {
            u8::try_from(rng.below(4)).unwrap()
        };
        let small = rng.next_u16() & 0x7FF;
        let cx = *rng.pick(&[0u16, 1, 0x7F, 0x100, 0x200, small]);
        let dx = match ah {
            0x02 | 0x06 => u16::from(rng.next_u8() | 0x20),
            0x0E | 0x1C | 0x32 | 0x36 | 0x47 => u16::try_from(rng.below(30)).unwrap(),
            _ => pointer,
        };
        let regs = run.call(|r| {
            r.gpr[gpr::AX] = u16::from_le_bytes([al, ah]);
            r.gpr[gpr::BX] = match ah {
                0x44 if matches!(al, 0x04 | 0x05 | 0x08 | 0x09 | 0x0D | 0x0E | 0x0F | 0x11) => {
                    rng.next_u16() & 3
                }
                0x48 => *rng.pick(&[1u16, 0x10, 0x800, 0xFFFF, small]),
                0x67 => u16::try_from(rng.below(60)).unwrap(),
                _ => handle,
            };
            r.gpr[gpr::CX] = cx;
            r.gpr[gpr::DX] = dx;
            r.gpr[gpr::SI] = AREA + 0x200;
            r.gpr[gpr::DI] = AREA + 0x300;
            r.set_sreg(Seg::Es, segment);
        });
        if !regs.flag(flag::CF) {
            match ah {
                0x3C | 0x3D | 0x5A | 0x5B | 0x6C | 0x45 if regs.gpr[gpr::AX] > 4 => {
                    run.keep(regs.gpr[gpr::AX]);
                }
                0x48 => run.blocks.push(regs.gpr[gpr::AX]),
                _ => {}
            }
        }
        if n % 10 == 9 {
            run.check();
        }
    }
    run.check();
}

#[test]
fn hostile_calls_never_panic_hang_or_break_the_memory_chain() {
    let seed = tairix_fuzzseed::start(
        "hostile_calls_never_panic_hang_or_break_the_memory_chain",
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
