use alloc::vec;
use alloc::vec::Vec;

use super::fdc::PhysicalDrive;
use super::keyboard::Key;
use super::{Machine, ROM_SIZE};
use crate::media::disc::{Disc, ImageFormat};
use crate::media::tape::Tape;
use crate::snapshot::SnapshotError;

/// A program keeping most of the machine busy: mode 7 on the 6845 and ULA,
/// the keyboard autoscanning, a tone, System VIA timer 1 interrupting, the
/// 6850 reading a playing tape, the 8271 seeking, and conversion after
/// conversion; its main loop writes the screen. Timer 1's handler counts in
/// `&71`, the 8271's in `&72`.
const PROGRAM: [u8; 149] = [
    0x78, 0xA2, 0xFF, 0x9A, 0xA2, 0x00, 0x8E, 0x00, 0xFE, 0xBD, 0x00, 0xC1, 0x8D, 0x01, 0xFE, 0xE8,
    0xE0, 0x0E, 0xD0, 0xF2, 0xA9, 0x4B, 0x8D, 0x20, 0xFE, 0xA9, 0xFF, 0x8D, 0x43, 0xFE, 0xA9, 0x0F,
    0x8D, 0x42, 0xFE, 0xA9, 0x0B, 0x8D, 0x40, 0xFE, 0xA9, 0x8F, 0x20, 0x77, 0xC0, 0xA9, 0x3F, 0x20,
    0x77, 0xC0, 0xA9, 0x90, 0x20, 0x77, 0xC0, 0xA9, 0x40, 0x8D, 0x4B, 0xFE, 0xA9, 0x10, 0x8D, 0x44,
    0xFE, 0xA9, 0x27, 0x8D, 0x45, 0xFE, 0xA9, 0xC0, 0x8D, 0x4E, 0xFE, 0xA9, 0x03, 0x8D, 0x08, 0xFE,
    0xA9, 0x15, 0x8D, 0x08, 0xFE, 0xA9, 0x85, 0x8D, 0x10, 0xFE, 0xA9, 0x69, 0x8D, 0x80, 0xFE, 0xA9,
    0x05, 0x8D, 0x81, 0xFE, 0x58, 0xE6, 0x70, 0xA5, 0x70, 0x8D, 0x00, 0x7C, 0x8D, 0xC0, 0xFE, 0xAD,
    0x09, 0xFE, 0x85, 0x73, 0x4C, 0x65, 0xC0, 0x8D, 0x4F, 0xFE, 0xA9, 0x00, 0x8D, 0x40, 0xFE, 0xA9,
    0x08, 0x8D, 0x40, 0xFE, 0x60, 0x48, 0xAD, 0x44, 0xFE, 0xE6, 0x71, 0x68, 0x40, 0x48, 0xAD, 0x81,
    0xFE, 0xE6, 0x72, 0x68, 0x40,
];
/// The program's IRQ and NMI handlers.
const IRQ: u16 = 0xC085;
const NMI: u16 = 0xC08D;
/// Mode 7's 6845 registers 0–13, which the program loads from `&C100`.
const MODE_7: [u8; 14] = [
    0x3F, 0x28, 0x33, 0x24, 0x1E, 0x02, 0x19, 0x1B, 0x93, 0x12, 0x72, 0x13, 0x28, 0x00,
];

fn os() -> Vec<u8> {
    let mut rom = vec![0xEA; ROM_SIZE];
    rom[..PROGRAM.len()].copy_from_slice(&PROGRAM);
    rom[0x100..0x100 + MODE_7.len()].copy_from_slice(&MODE_7);
    for (vector, target) in [(0x3FFA, NMI), (0x3FFC, 0xC000), (0x3FFE, IRQ)] {
        rom[vector..vector + 2].copy_from_slice(&target.to_le_bytes());
    }
    rom
}

/// A UEF tape: carrier, then framed bytes, then silence, three times over.
fn tape() -> Tape {
    let mut uef = b"UEF File!\0\x0A\x00".to_vec();
    let mut chunk = |id: u16, body: &[u8]| {
        uef.extend_from_slice(&id.to_le_bytes());
        uef.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
        uef.extend_from_slice(body);
    };
    for block in 0u8..3 {
        chunk(0x0110, &240u16.to_le_bytes());
        chunk(
            0x0100,
            &(0..=255).map(|byte: u8| byte ^ block).collect::<Vec<_>>(),
        );
        chunk(0x0112, &600u16.to_le_bytes());
    }
    Tape::decode(&uef).unwrap()
}

fn disc() -> Disc {
    let image: Vec<u8> = (0..40 * 2560)
        .map(|at: usize| u8::try_from(at * 7 % 251).unwrap())
        .collect();
    Disc::decode(&image, ImageFormat::Ssd, false).unwrap()
}

/// The machine running the program with a disc, a tape, a key held, the
/// joysticks off-centre and the light pen on the screen.
fn busy() -> Machine {
    let mut machine = Machine::new(&os()).unwrap();
    machine.insert_ram(4).unwrap();
    machine.insert_rom(7, &[0x77; 0x2000]).unwrap();
    machine.insert_disc(PhysicalDrive::Zero, disc());
    machine.insert_tape(tape());
    machine.set_key(Key::A, true);
    machine.set_analogue(1, 0x1234);
    machine.set_analogue(2, 0xFEDC);
    machine.set_light_pen(Some((300, 120)));
    machine
}

#[test]
fn a_restored_machine_runs_on_exactly_as_the_one_it_was_taken_from() {
    let mut original = busy();
    let end = 1_000_000;
    let mut taken = Vec::new();
    // Early ones catch the 8271 mid-seek and the 6850 mid-setup.
    for at in [
        1_201, 3_001, 9_973, 20_011, 41_999, 241_667, 483_334, 725_001,
    ] {
        original.run_until(at);
        taken.push(original.snapshot().unwrap());
    }
    original.run_until(end);
    let expected = original.snapshot().unwrap();
    assert!(original.peek(0x71) > 0, "timer 1 interrupted");
    assert_eq!(original.peek(0x72), 1, "the seek finished");
    assert_ne!(original.peek(0x73), 0, "the tape played");
    for snapshot in &taken {
        let mut copy = Machine::new(&os()).unwrap();
        copy.insert_ram(4).unwrap();
        copy.insert_rom(7, &[0x77; 0x2000]).unwrap();
        copy.restore(snapshot).unwrap();
        assert_eq!(copy.snapshot().unwrap(), *snapshot, "restored as taken");
        copy.run_until(end);
        assert_eq!(copy.cycles(), original.cycles());
        assert!(
            copy.snapshot().unwrap() == expected,
            "diverged after restoring the snapshot at {}",
            copy.cycles()
        );
        assert_eq!(copy.video().shown(), original.video().shown());
        assert_eq!(
            copy.disc(PhysicalDrive::Zero),
            original.disc(PhysicalDrive::Zero)
        );
    }
}

#[test]
fn a_snapshot_restores_only_onto_the_machine_it_was_fitted_for() {
    let mut machine = busy();
    machine.run_until(400_000);
    let snapshot = machine.snapshot().unwrap();

    let mut other_os = os();
    other_os[0x2000] ^= 1;
    let fitted = |os: &[u8], fit: &dyn Fn(&mut Machine)| {
        let mut machine = Machine::new(os).unwrap();
        fit(&mut machine);
        let before = machine.snapshot().unwrap();
        let result = machine.restore(&snapshot);
        if result.is_err() {
            assert_eq!(
                machine.snapshot().unwrap(),
                before,
                "a refused snapshot changes nothing"
            );
        }
        result
    };
    let same = |machine: &mut Machine| {
        machine.insert_ram(4).unwrap();
        machine.insert_rom(7, &[0x77; 0x2000]).unwrap();
    };
    assert_eq!(fitted(&os(), &same), Ok(()));
    assert_eq!(
        fitted(&other_os, &same),
        Err(SnapshotError::Fitting),
        "another OS"
    );
    let other_rom = |machine: &mut Machine| {
        machine.insert_ram(4).unwrap();
        machine.insert_rom(7, &[0x76; 0x2000]).unwrap();
    };
    assert_eq!(
        fitted(&os(), &other_rom),
        Err(SnapshotError::Fitting),
        "another sideways ROM"
    );
    let no_ram = |machine: &mut Machine| {
        machine.insert_rom(7, &[0x77; 0x2000]).unwrap();
    };
    assert_eq!(
        fitted(&os(), &no_ram),
        Err(SnapshotError::Fitting),
        "no sideways RAM"
    );
    let mirrored = |machine: &mut Machine| {
        machine.insert_ram(4).unwrap();
        machine.insert_rom(7, &[0x77; 0x4000]).unwrap();
    };
    assert_eq!(
        fitted(&os(), &mirrored),
        Ok(()),
        "a ROM is its mirrored image"
    );
}

#[test]
fn a_damaged_snapshot_is_refused_and_changes_nothing() {
    let mut machine = busy();
    machine.run_until(250_000);
    let snapshot = machine.snapshot().unwrap();
    let body = crate::snapshot::open(&snapshot).unwrap();
    let reseal = |body: &[u8]| {
        let mut w = crate::snapshot::Writer::new();
        w.raw(body);
        w.finish().unwrap()
    };
    assert_eq!(reseal(&body), snapshot);
    let mut target = busy();
    let before = target.snapshot().unwrap();
    // Each section's tag and length, which must be refused, and bytes spread
    // through it, which may hold another machine's state.
    let mut places = Vec::new();
    let mut at = 0;
    while at < body.len() {
        let len =
            usize::try_from(u32::from_le_bytes(body[at + 4..at + 8].try_into().unwrap())).unwrap();
        places.extend([(at, true), (at + 4, true)]);
        places.extend(
            (0..32)
                .map(|k| (at + 8 + k * len / 32, false))
                .filter(|&(place, _)| place < at + 8 + len),
        );
        at += 8 + len;
    }
    let mut refused = 0;
    for &(place, structure) in &places {
        let mut damaged = body.clone();
        damaged[place] ^= 0xA5;
        if target.restore(&reseal(&damaged)).is_err() {
            refused += 1;
            assert_eq!(
                target.snapshot().unwrap(),
                before,
                "refused at byte {place}"
            );
        } else {
            assert!(!structure, "a damaged tag or length at {place} was taken");
            target = busy();
        }
    }
    assert!(refused > places.len() / 3, "{refused} of {}", places.len());
    assert_eq!(
        target.restore(&reseal(&body[..body.len() - 1])),
        Err(SnapshotError::Truncated)
    );
    let mut longer = body.clone();
    longer.push(0);
    assert_eq!(
        target.restore(&reseal(&longer)),
        Err(SnapshotError::Malformed)
    );
    assert_eq!(target.snapshot().unwrap(), before);
}
