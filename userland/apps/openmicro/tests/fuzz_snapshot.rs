//! Deterministic fuzz harness for machine snapshots, and for the machines
//! under hostile guest code: the Model B, and the Electron with its Plus 1.
//!
//! A snapshot is a stranger's file and a guest program a stranger's code.
//! The invariants:
//!
//! * a machine running a program that drives every device register at random
//!   never panics or hangs;
//! * a snapshot of it restores to exactly the state it was taken from, and
//!   the two run on alike;
//! * restoring arbitrary bytes or a damaged snapshot never panics, a refused
//!   one changes nothing, and a machine restored from an accepted one runs
//!   on without panicking.
//!
//! Damage lands in each section's own fields as often as in the bulk of RAM,
//! discs and pictures, by mutating one section and keeping its length true,
//! and as values at the edges of a field's range, where a time or count
//! reckoned from unchecked overflows.
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

use std::ops::Range;

use tairix_fuzzseed::Prng;
use tairix_openmicro::machine::bbc::fdc::PhysicalDrive;
use tairix_openmicro::machine::electron::{CARTRIDGE_ROMS, PLUS1_ROM};
use tairix_openmicro::machine::{
    bbc, electron, Machine, Model, ANALOGUE_CHANNELS, IO_BASE, IO_END, ROM_SIZE,
};
use tairix_openmicro::media::disc::{Disc, ImageFormat, TRACK_BYTES};
use tairix_openmicro::media::tape::Tape;
use tairix_openmicro::snapshot::{self, Writer};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 16;
/// Device accesses in a drawn program.
const ACCESSES: usize = 96;
/// The Model B's device registers a guest reaches, by base and count.
const MODEL_B_DEVICES: &[(u16, u16)] = &[
    (0xFE00, 2),
    (0xFE08, 2),
    (0xFE10, 1),
    (0xFE20, 2),
    (0xFE40, 16),
    (0xFE60, 16),
    (0xFE80, 8),
    (0xFEC0, 4),
];
/// The Electron's: the ULA, which answers throughout SHEILA, and the Plus
/// 1's ports.
const ELECTRON_DEVICES: &[(u16, u16)] = &[(0xFE00, 16), (0xFEF0, 16), (0xFC70, 3)];
/// Where the program's interrupt handlers return from.
const RTI_AT: usize = 0x3F00;

/// An OS ROM that reads and writes `model`'s device registers at random,
/// for ever.
fn program(rng: &mut Prng, model: Model) -> Vec<u8> {
    let devices = match model {
        Model::ModelB => MODEL_B_DEVICES,
        Model::Electron => ELECTRON_DEVICES,
    };
    let mut rom = vec![0xEA; ROM_SIZE];
    let mut code = vec![0x78];
    for _ in 0..ACCESSES {
        // Now and then any address of the I/O pages, so every decode is met.
        let (base, count) = if rng.below(8) == 0 {
            (IO_BASE, IO_END - IO_BASE)
        } else {
            *rng.pick(devices)
        };
        let [low, high] =
            (base + u16::try_from(rng.below(usize::from(count))).expect("small")).to_le_bytes();
        if rng.below(4) == 0 {
            code.extend_from_slice(&[0xAD, low, high]);
        } else {
            code.extend_from_slice(&[0xA9, rng.edge_biased_u8(), 0x8D, low, high]);
        }
    }
    if rng.below(2) == 0 {
        code.push(0x58);
    }
    code.extend_from_slice(&[0x4C, 0x01, 0xC0]);
    rom[..code.len()].copy_from_slice(&code);
    rom[RTI_AT] = 0x40;
    let handler = (0xC000 + u16::try_from(RTI_AT).expect("in the ROM")).to_le_bytes();
    for vector in [0x3FFA, 0x3FFE] {
        rom[vector..vector + 2].copy_from_slice(&handler);
    }
    rom[0x3FFC..0x3FFE].copy_from_slice(&[0x00, 0xC0]);
    rom
}

fn uef(rng: &mut Prng) -> Tape {
    let mut out = b"UEF File!\0\x0A\x00".to_vec();
    for _ in 0..4 {
        let mut chunk = |id: u16, body: &[u8]| {
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&u32::try_from(body.len()).expect("small").to_le_bytes());
            out.extend_from_slice(body);
        };
        chunk(0x0110, &(1 + rng.next_u16() % 600).to_le_bytes());
        let len = 1 + rng.at_most(64);
        chunk(0x0100, &rng.noise(len));
    }
    Tape::decode(&out).expect("a well-formed UEF")
}

/// The machine the snapshots are restored onto: the program; on the Model B
/// sideways RAM in slot 4 and a ROM in slot 7, on the Electron BASIC's
/// sockets, the Plus 1 with its ROM and a cartridge.
fn fitted(model: Model, os: &[u8]) -> Machine {
    match model {
        Model::ModelB => {
            let mut machine = bbc::Machine::new(os).expect("a 16 KiB OS");
            machine.insert_ram(4).expect("slot 4");
            machine.insert_rom(7, &[0x77; 0x2000]).expect("slot 7");
            Machine::ModelB(Box::new(machine))
        }
        Model::Electron => {
            let mut machine = electron::Machine::new(os).expect("a 16 KiB OS");
            machine.fit_basic(&[0x60; 0x2000]).expect("BASIC's sockets");
            machine.fit_plus1();
            machine
                .insert_rom(PLUS1_ROM, &[0x77; 0x2000])
                .expect("slot 12");
            machine
                .insert_rom(CARTRIDGE_ROMS[0][0], &[0x55; 0x4000])
                .expect("a cartridge");
            Machine::Electron(Box::new(machine))
        }
    }
}

/// A running machine with media in, keys and joysticks held, and on the
/// Model B the pen out.
fn running(rng: &mut Prng, model: Model, os: &[u8]) -> Machine {
    let mut machine = fitted(model, os);
    machine.insert_tape(uef(rng));
    machine.set_tape_record(rng.below(2) == 0);
    let channel = rng.below(ANALOGUE_CHANNELS);
    match &mut machine {
        Machine::ModelB(machine) => {
            let len = rng.at_most(40 * TRACK_BYTES);
            if let Ok(disc) = Disc::decode(&rng.noise(len), ImageFormat::Dsd, rng.below(2) == 0) {
                machine.insert_disc(*rng.pick(&PhysicalDrive::ALL), disc);
            }
            for _ in 0..rng.below(3) {
                machine.set_key(*rng.pick(bbc::keyboard::Key::ALL), true);
            }
            machine.set_analogue(channel, rng.next_u16());
            machine.set_fire_buttons(rng.next_u8());
            machine.set_light_pen(Some((rng.below(896), rng.below(600))));
        }
        Machine::Electron(machine) => {
            for _ in 0..rng.below(3) {
                machine.set_key(*rng.pick(electron::keyboard::Key::ALL), true);
            }
            machine.set_analogue(channel, rng.next_u16());
            machine.set_fire_buttons(rng.next_u8());
        }
    }
    machine.run_until(u64::try_from(10_000 + rng.below(300_000)).expect("small"));
    machine
}

fn sealed(body: &[u8]) -> Vec<u8> {
    let mut w = Writer::new();
    w.raw(body);
    w.finish().expect("a body within the limit")
}

/// Where each section's own bytes lie.
fn sections(body: &[u8]) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(len) = body.get(at + 4..at + 8) {
        let len = usize::try_from(u32::from_le_bytes(len.try_into().expect("four bytes")))
            .expect("small");
        found.push(at + 8..at + 8 + len);
        at += 8 + len;
    }
    found
}

/// Overwrite up to eight bytes of `bytes` from `at` with a value at an edge
/// of their width's range.
fn extreme(rng: &mut Prng, bytes: &mut [u8], at: usize) {
    let width = *rng.pick(&[1usize, 2, 4, 8]);
    let max = u64::MAX >> (64 - 8 * width);
    let value = match rng.below(5) {
        0 => 0,
        1 => 1,
        2 => max,
        3 => max - u64::try_from(rng.at_most(16)).expect("small"),
        _ => 1 << rng.below(8 * width),
    };
    for (byte, shift) in bytes.iter_mut().skip(at).take(width).zip((0..).step_by(8)) {
        *byte = u8::try_from(value >> shift & 0xFF).expect("a byte");
    }
}

/// `body` damaged: anywhere; in one section's first or last bytes or
/// throughout, its length rewritten to cover what the damage left; or with
/// values at the edges of their ranges written into one section's fields.
fn damaged(rng: &mut Prng, body: &[u8]) -> Vec<u8> {
    let sections = sections(body);
    if rng.below(5) == 0 || sections.is_empty() {
        return rng.mutated(body);
    }
    let section = rng.pick(&sections).clone();
    if rng.below(4) == 0 && !section.is_empty() {
        let mut out = body.to_vec();
        for _ in 0..=rng.below(4) {
            let at = section.start + rng.below(section.len());
            extreme(rng, &mut out[..section.end], at);
        }
        return out;
    }
    let window = match rng.below(3) {
        0 => section.start..(section.start + 256).min(section.end),
        1 => section.end.saturating_sub(256).max(section.start)..section.end,
        _ => section.clone(),
    };
    let replaced = rng.mutated(&body[window.clone()]);
    let len = section.len() - window.len() + replaced.len();
    let mut out = body[..window.start].to_vec();
    out.extend_from_slice(&replaced);
    out.extend_from_slice(&body[window.end..]);
    out[section.start - 4..section.start]
        .copy_from_slice(&u32::try_from(len).expect("small").to_le_bytes());
    out
}

/// Restore `bytes` onto a fresh machine: a refused snapshot must leave it as
/// it was, and an accepted one run on.
fn restore_or_refuse(rng: &mut Prng, model: Model, os: &[u8], bytes: &[u8]) {
    let mut target = fitted(model, os);
    let before = target.snapshot().expect("fits");
    if target.restore(bytes).is_ok() {
        let on = target.cycles() + u64::try_from(1 + rng.below(50_000)).expect("small");
        target.run_until(on);
    } else {
        assert!(
            target.snapshot().expect("fits") == before,
            "a refused snapshot changed the machine"
        );
    }
}

#[test]
fn snapshots_restore_exactly_and_damage_is_refused_or_runs_safely() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "snapshots_restore_exactly_and_damage_is_refused_or_runs_safely",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let model = *rng.pick(&Model::ALL);
        let os = program(&mut rng, model);
        let mut original = running(&mut rng, model, &os);
        let taken = original.snapshot().expect("a machine fits a snapshot");
        let mut copy = fitted(model, &os);
        copy.restore(&taken)
            .expect("an undamaged snapshot restores");
        assert!(copy.snapshot().expect("fits") == taken, "restored as taken");
        let on = original.cycles() + u64::try_from(1 + rng.below(100_000)).expect("small");
        original.run_until(on);
        copy.run_until(on);
        assert!(
            original.snapshot() == copy.snapshot(),
            "the two run on alike"
        );

        let body = snapshot::open(&taken).expect("our own container opens");
        for _ in 0..4 {
            let bytes = sealed(&damaged(&mut rng, &body));
            restore_or_refuse(&mut rng, model, &os, &bytes);
        }
        let mutated = rng.mutated(&taken);
        restore_or_refuse(&mut rng, model, &os, &mutated);
        let other = Model::ALL
            .into_iter()
            .find(|&other| other != model)
            .expect("two models");
        assert!(
            fitted(other, &program(&mut rng, other))
                .restore(&taken)
                .is_err(),
            "a snapshot restores onto its own model alone"
        );
        let len = rng.at_most(4096);
        assert!(fitted(model, &os).restore(&rng.noise(len)).is_err());

        iteration += 1;
        match deadline {
            None if iteration >= SMOKE_ITERATIONS => break,
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            _ => {}
        }
    }
}
