//! Deterministic fuzz harness for the disk controllers, which a guest
//! program drives through their ports with any values at any times. For any
//! sequence of port writes and reads, DMA programming, media changes and
//! waits:
//!
//! 1. Neither controller panics, and advancing time always ends.
//! 2. Advancing to a time leaves nothing due at or before it.
//!
//! Each run starts with a well-formed read on each controller, so the data
//! paths are reached by construction, then drives them with hostile traffic.
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from the
//! same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

mod support;

use support::Flat;
use tairix_fuzzseed::Prng;
use tairix_pcemu::chips::dma::{channel_setup, Dma};
use tairix_pcemu::storage::fdc::{DriveKind, Fdc, Interface};
use tairix_pcemu::storage::floppy::Floppy;
use tairix_pcemu::storage::hdc::{DiskType, Hdc, LowLevel, Recording};
use tairix_pcemu::storage::{MemoryDisk, SECTOR};
use tairix_pcemu::time::{Hertz, Nanos};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 60;
/// Port operations a run makes after its prologue.
const OPERATIONS: usize = 300;
/// The most events a run follows when it waits for the controllers.
const EVENTS: usize = 20_000;
const MS: Nanos = 1_000_000;

const DISK: DiskType = DiskType {
    cylinders: 8,
    heads: 2,
    track_to_track: 3 * MS,
    average_seek: 20 * MS,
};

struct Machine {
    fdc: Fdc,
    hdc: Hdc,
    dma: Dma,
    memory: Flat,
    now: Nanos,
    results: u64,
}

impl Machine {
    fn new(rng: &mut Prng) -> Self {
        let kinds = [
            DriveKind::Dd525,
            DriveKind::Hd525,
            DriveKind::Dd35,
            DriveKind::Hd35,
        ];
        let interface = *rng.pick(&[Interface::Standard, Interface::HighDensity]);
        let mut fdc = Fdc::new(
            interface,
            [Some(*rng.pick(&kinds)), Some(*rng.pick(&kinds)), None, None],
        );
        fdc.insert(0, floppy(rng), 0).unwrap();
        let rate = Hertz::whole(954_545).unwrap();
        let (recording, per_track) = *rng.pick(&[(Recording::Mfm, 17), (Recording::Rll, 26)]);
        let mut hdc = Hdc::new(recording, rng.next_u8(), rate);
        let sectors = usize::from(DISK.cylinders) * usize::from(DISK.heads) * per_track;
        let image = MemoryDisk::new(rng.noise(sectors * SECTOR), rng.below(4) == 0).unwrap();
        let format = LowLevel {
            interleave: u8::try_from(rng.at_most(30)).unwrap(),
            bad_tracks: Vec::new(),
        };
        hdc.attach(0, DISK, Box::new(image), format).unwrap();
        Self {
            fdc,
            hdc,
            dma: Dma::new(),
            memory: Flat(vec![0; 1 << 20]),
            now: 0,
            results: 0,
        }
    }

    fn out(&mut self, port: u16, value: u8) {
        match port {
            0x3F0..=0x3F7 => {
                self.fdc
                    .output(port, value, self.now, &mut self.dma, &mut self.memory);
            }
            0x320..=0x323 => {
                self.hdc
                    .output(port, value, self.now, &mut self.dma, &mut self.memory);
            }
            _ => {
                self.dma.write_port(port, value);
                self.hdc.advance(self.now, &mut self.dma, &mut self.memory);
            }
        }
    }

    fn input(&mut self, port: u16) -> Option<u8> {
        match port {
            0x3F0..=0x3F7 => self
                .fdc
                .input(port, self.now, &mut self.dma, &mut self.memory),
            _ => self
                .hdc
                .input(port, self.now, &mut self.dma, &mut self.memory),
        }
    }

    /// Moves time to `t` and checks both controllers left nothing due.
    fn wait_until(&mut self, t: Nanos) {
        self.now = self.now.max(t);
        self.fdc.advance(self.now, &mut self.dma, &mut self.memory);
        self.hdc.advance(self.now, &mut self.dma, &mut self.memory);
        assert!(
            self.fdc.next_event().is_none_or(|at| at > self.now),
            "the floppy controller left work due"
        );
        assert!(
            self.hdc.next_event().is_none_or(|at| at > self.now),
            "the fixed-disk controller left work due"
        );
    }

    /// Follows the controllers' events until neither has any, or the run's
    /// bound on them.
    fn settle(&mut self) {
        for _ in 0..EVENTS {
            let next = match (self.fdc.next_event(), self.hdc.next_event()) {
                (Some(a), Some(b)) => a.min(b),
                (a, b) => match a.or(b) {
                    Some(at) => at,
                    None => return,
                },
            };
            self.wait_until(next);
            self.collect();
        }
    }

    /// Reads whatever results are ready, as a driver would.
    fn collect(&mut self) {
        while self.input(0x3F4).is_some_and(|msr| msr & 0xF0 == 0xD0) {
            self.input(0x3F5);
            self.results += 1;
        }
        if self
            .input(0x321)
            .is_some_and(|status| status & 0x0F == 0x0F)
        {
            self.input(0x320);
            self.results += 1;
        }
    }

    /// A well-formed read on each controller: the floppy after a reset and
    /// SPECIFY, the fixed disk after INITIALIZE.
    fn prologue(&mut self) {
        self.out(0x3F2, 0x1C);
        self.out(0x3F7, 0x02);
        self.wait_until(5 * MS);
        for byte in [0x03, 0xDF, 0x02] {
            self.out(0x3F5, byte);
        }
        self.program(2, 0x46, 512);
        self.wait_until(450 * MS);
        for byte in [0x46, 0x00, 0, 0, 1, 2, 9, 0x2A, 0xFF] {
            self.out(0x3F5, byte);
        }
        self.out(0x323, 0x02);
        self.out(0x322, 0);
        for byte in [0x0C, 0, 0, 0, 0, 0] {
            self.out(0x320, byte);
        }
        self.settle();
        for byte in [0, 8, 2, 0, 0, 0, 0, 11] {
            self.out(0x320, byte);
        }
        self.settle();
        self.program(3, 0x47, 512);
        self.out(0x323, 0x03);
        self.out(0x322, 0);
        for byte in [0x08, 0x00, 0x00, 0x00, 0x01, 0x05] {
            self.out(0x320, byte);
        }
        self.settle();
    }

    /// Channel `channel` set to move `bytes` at `4000h` in `mode`.
    fn program(&mut self, channel: usize, mode: u8, bytes: u16) {
        for (port, value) in channel_setup(channel, mode, 0x4000, bytes - 1) {
            self.out(port, value);
        }
    }
}

fn floppy(rng: &mut Prng) -> Floppy {
    let kib = *rng.pick(&[160, 180, 320, 360, 720, 1200, 1440]);
    let image = MemoryDisk::new(rng.noise(kib * 1024), rng.below(3) == 0).unwrap();
    Floppy::raw(Box::new(image)).unwrap()
}

/// A random command for the floppy controller: a valid opcode with random
/// option bits and parameters, or any byte at all.
fn floppy_command(rng: &mut Prng) -> Vec<u8> {
    let op = *rng.pick(&[
        0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0C, 0x0D, 0x0F, 0x11, 0x19, 0x1D,
    ]);
    let first = if rng.below(4) == 0 {
        rng.next_u8()
    } else {
        op | (rng.next_u8() & 0xE0)
    };
    let mut bytes = vec![first];
    bytes.extend((0..8).map(|_| rng.edge_biased_u8()));
    bytes
}

fn fixed_command(rng: &mut Prng) -> [u8; 6] {
    let op = *rng.pick(&[
        0x00, 0x01, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0xE0,
        0xE3, 0xE4, 0xE5, 0xE6,
    ]);
    let first = if rng.below(6) == 0 { rng.next_u8() } else { op };
    let small = |rng: &mut Prng, most| u8::try_from(rng.at_most(most)).unwrap();
    [
        first,
        rng.next_u8() & 0x3F,
        rng.next_u8() & 0x4F,
        small(rng, 9),
        small(rng, 3),
        rng.next_u8(),
    ]
}

fn run(rng: &mut Prng) -> u64 {
    let mut machine = Machine::new(rng);
    machine.prologue();
    for _ in 0..OPERATIONS {
        match rng.below(10) {
            0 => {
                for byte in floppy_command(rng) {
                    machine.out(0x3F5, byte);
                }
            }
            1 => {
                machine.out(0x322, 0);
                for byte in fixed_command(rng) {
                    machine.out(0x320, byte);
                }
            }
            2 => {
                let port = 0x3F0 + u16::try_from(rng.below(8)).unwrap();
                machine.out(port, rng.edge_biased_u8());
            }
            3 => {
                let port = 0x320 + u16::try_from(rng.below(4)).unwrap();
                machine.out(port, rng.edge_biased_u8());
            }
            4 => {
                let port = u16::try_from(rng.below(16)).unwrap();
                machine.out(port, rng.edge_biased_u8());
            }
            5 => {
                let port = *rng.pick(&[0x3F4, 0x3F5, 0x3F7, 0x320, 0x321, 0x322]);
                machine.input(port);
            }
            6 => {
                let floppy = floppy(rng);
                let drive = rng.below(3);
                let _ = machine.fdc.insert(drive, floppy, machine.now);
            }
            7 => {
                machine.fdc.eject(rng.below(5), machine.now);
            }
            8 => {
                let t = machine.now + u64::try_from(rng.at_most(400)).unwrap() * 100_000;
                machine.wait_until(t);
                machine.collect();
            }
            _ => machine.settle(),
        }
    }
    machine.settle();
    machine.results
}

#[test]
fn hostile_port_traffic_never_panics_or_hangs() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_port_traffic_never_panics_or_hangs",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut results = 0;
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            results += run(&mut rng);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
    assert!(
        results >= 3 * SMOKE_ITERATIONS,
        "every run's prologue reached three results"
    );
}
