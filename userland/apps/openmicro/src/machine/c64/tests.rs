use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::machine::c64::memory::{BASIC_BYTES, CHARACTER_BYTES, KERNAL_BYTES};

/// A KERNAL image holding `program` at $E000, every vector pointing at it
/// and `handler` at $F000 for IRQ.
fn kernal(program: &[u8], handler: &[u8]) -> Vec<u8> {
    let mut image = vec![0xEA; KERNAL_BYTES];
    image[..program.len()].copy_from_slice(program);
    image[0x1000..0x1000 + handler.len()].copy_from_slice(handler);
    image[0x1FFA..].copy_from_slice(&[0x00, 0xE0, 0x00, 0xE0, 0x00, 0xF0]);
    image
}

fn machine(program: &[u8], handler: &[u8]) -> Machine {
    let basic = vec![0xBA; BASIC_BYTES];
    let character = vec![0xC4; CHARACTER_BYTES];
    let kernal = kernal(program, handler);
    Machine::new(
        Config {
            standard: Standard::Pal,
            sid: sid::Model::Mos6581,
        },
        Roms {
            basic: &basic,
            kernal: &kernal,
            character: &character,
        },
    )
    .expect("the images fit")
}

/// `JMP` to the instruction itself, at $E000 + `at`.
fn halt(at: u16) -> [u8; 3] {
    let [low, high] = (0xE000 + at).to_le_bytes();
    [0x4C, low, high]
}

#[test]
fn the_reset_vector_starts_the_kernal_and_ram_takes_its_writes() {
    let mut program = vec![0xA9, 0x41, 0x8D, 0x00, 0x04];
    program.extend(halt(5));
    let mut machine = machine(&program, &[]);
    machine.run_until(100);
    assert_eq!(machine.ram(0x0400), 0x41);
    assert_eq!(machine.cpu().pc(), 0xE005);
}

#[test]
fn the_bus_monitor_records_the_chips_accesses_but_colour_ram_while_asked() {
    let mut program = vec![
        0xA9, 0x3F, 0x8D, 0x02, 0xDD, // LDA #$3F : STA $DD02
        0xAD, 0x0D, 0xDC, // LDA $DC0D
        0x8D, 0x00, 0xD8, // STA $D800, colour RAM
        0x8D, 0x00, 0x04, // STA $0400
    ];
    program.extend(halt(14));
    let mut unasked = machine(&program, &[]);
    unasked.run_until(100);
    assert_eq!(
        unasked.take_io_trace().count(),
        0,
        "nothing recorded unasked"
    );
    let mut machine = machine(&program, &[]);
    machine.monitor_io(true);
    machine.run_until(100);
    let trace: Vec<IoAccess> = machine.take_io_trace().collect();
    let seen: Vec<(u16, u8, bool)> = trace
        .iter()
        .map(|access| (access.addr, access.value, access.write))
        .collect();
    assert_eq!(seen, [(0xDD02, 0x3F, true), (0xDC0D, 0x00, false)]);
    assert!(
        trace[0].cycle < trace[1].cycle,
        "each at the cycle the chip saw it"
    );
    assert_eq!(machine.take_io_trace().count(), 0, "taken once");
}

#[test]
fn the_port_banks_the_kernal_out_to_the_ram_beneath() {
    let mut program = vec![
        0xA9, 0x37, 0x85, 0x01, // LDA #$37 : STA $01, before the lines are outputs
        0xA9, 0x2F, 0x85, 0x00, // LDA #$2F : STA $00
        0xA9, 0x55, 0x8D, 0x00, 0xA0, // LDA #$55 : STA $A000 (into RAM)
        0xAD, 0x00, 0xA0, 0x8D, 0x00, 0x04, // LDA $A000 : STA $0400
        0xA9, 0x36, 0x85, 0x01, // LDA #$36 : STA $01 (BASIC out)
        0xAD, 0x00, 0xA0, 0x8D, 0x01, 0x04, // LDA $A000 : STA $0401
    ];
    let end = u16::try_from(program.len()).expect("short");
    program.extend(halt(end));
    let mut machine = machine(&program, &[]);
    machine.run_until(200);
    assert_eq!(machine.ram(0x0400), 0xBA, "BASIC's byte while it is in");
    assert_eq!(machine.ram(0x0401), 0x55, "RAM's once it is out");
}

#[test]
fn cia1_reads_the_keyboard_s_column_on_its_rows() {
    let mut program = vec![
        0xA9, 0xFF, 0x8D, 0x02, 0xDC, // DDRA = $FF
        0xA9, 0x00, 0x8D, 0x03, 0xDC, // DDRB = $00
        0xA9, 0xFE, 0x8D, 0x00, 0xDC, // column 0
        0xAD, 0x01, 0xDC, 0x8D, 0x00, 0x04, // rows → $0400
    ];
    let end = u16::try_from(program.len()).expect("short");
    program.extend(halt(end));
    let mut machine = machine(&program, &[]);
    machine.set_key(Key::RETURN, true);
    machine.set_key(Key::A, true);
    machine.run_until(200);
    assert_eq!(machine.ram(0x0400), !0x02, "RETURN alone in column 0");
}

#[test]
fn a_raster_interrupt_reaches_the_cpu() {
    let mut program = vec![
        0xA9, 0x00, 0x8D, 0x11, 0xD0, // CR1 = 0 (no bad lines, raster bit 8 clear)
        0xA9, 0x40, 0x8D, 0x12, 0xD0, // compare line $40
        0xA9, 0x01, 0x8D, 0x1A, 0xD0, // enable the raster interrupt
        0x58, // CLI
    ];
    let end = u16::try_from(program.len()).expect("short");
    program.extend(halt(end));
    let handler = [
        0xAD, 0x12, 0xD0, 0x8D, 0x00, 0x04, // the raster line → $0400
        0xA9, 0x01, 0x8D, 0x19, 0xD0, // acknowledge
        0x40, // RTI
    ];
    let mut machine = machine(&program, &handler);
    machine.run_until(312 * 63);
    assert_eq!(machine.ram(0x0400), 0x40);
}

#[test]
fn bad_lines_stall_the_cpu_for_forty_cycles_a_row() {
    // INC $0400,X counts loop passes; X stays 0.
    let mut program = vec![0xA2, 0x00, 0xA9, 0x00, 0x8D, 0x11, 0xD0];
    let lp = u16::try_from(program.len()).expect("short");
    program.extend([0xEE, 0x00, 0x04]);
    let [low, high] = (0xE000 + lp).to_le_bytes();
    program.extend([0x4C, low, high]);
    let passes = |cr1: u8| {
        let mut program = program.clone();
        program[3] = cr1;
        let mut machine = machine(&program, &[]);
        machine.run_until(312 * 63 * 3);
        machine.ram(0x0400)
    };
    let quiet = passes(0x0B);
    let busy = passes(0x1B);
    assert!(
        busy < quiet,
        "{busy} passes with bad lines, {quiet} without"
    );
}

#[test]
fn a_program_loads_into_memory_as_load_leaves_it() {
    use crate::media::prg::Program;
    let mut program = vec![0xA9, 0x01, 0x85, 0x2B, 0xA9, 0x08, 0x85, 0x2C];
    program.extend(halt(8));
    let mut machine = machine(&program, &[]);
    machine.run_until(100);
    machine.load_into_memory(&Program {
        address: 0x0801,
        bytes: vec![1, 2, 3, 4],
    });
    assert_eq!((machine.ram(0x0801), machine.ram(0x0804)), (1, 4));
    let pointer =
        |machine: &Machine, at: u16| u16::from_le_bytes([machine.ram(at), machine.ram(at + 1)]);
    assert_eq!(pointer(&machine, zero_page::LOAD_END), 0x0805);
    assert_eq!(
        pointer(&machine, zero_page::VARTAB),
        0x0805,
        "loaded at BASIC's start"
    );
    machine.load_into_memory(&Program {
        address: 0xC000,
        bytes: vec![9],
    });
    assert_eq!(pointer(&machine, zero_page::LOAD_END), 0xC001);
    assert_eq!(
        pointer(&machine, zero_page::VARTAB),
        0x0805,
        "elsewhere, BASIC's pointers stand"
    );
}

#[test]
fn a_cartridge_s_roml_answers_at_8000() {
    use crate::media::crt::{decode, tests::image};
    let mut program = vec![0xAD, 0x00, 0x80, 0x8D, 0x00, 0x04];
    program.extend(halt(6));
    let mut machine = machine(&program, &[]);
    let crt = decode(&image(0, 0, 1, &[(0, 0x8000, &[0x5A; 0x2000])])).expect("a CRT");
    machine.insert_cartridge(&crt).expect("a generic cartridge");
    machine.run_until(200);
    assert_eq!(machine.ram(0x0400), 0x5A);
    machine.remove_cartridge();
    machine.run_until(400);
    assert_eq!(
        machine.ram(0x0400),
        0x00,
        "RAM again beneath, never written"
    );
}

#[test]
fn an_ultimax_cartridge_replaces_the_kernal_and_its_vectors() {
    use crate::media::crt::{decode, tests::image};
    let mut rom = vec![0xEA; 0x2000];
    // LDA #$C3 : STA $0400 : JMP *
    rom[..8].copy_from_slice(&[0xA9, 0xC3, 0x8D, 0x00, 0x04, 0x4C, 0x05, 0xE0]);
    rom[0x1FFC..0x1FFE].copy_from_slice(&[0x00, 0xE0]);
    let mut machine = machine(&halt(0), &[]);
    let crt = decode(&image(0, 1, 0, &[(0, 0xE000, &rom)])).expect("a CRT");
    machine
        .insert_cartridge(&crt)
        .expect("an Ultimax cartridge");
    machine.run_until(200);
    assert_eq!(machine.ram(0x0400), 0xC3);
}

#[test]
fn the_letter_case_follows_the_character_set_the_vic_ii_shows() {
    use crate::media::crt::{decode, tests::image};
    let showing = |d018: u8, port: u8| {
        let mut program = vec![
            0xA9, 0x03, 0x8D, 0x02, 0xDD, // CIA2's DDRA: the bank's lines out
            0xA9, port, 0x8D, 0x00, 0xDD, // the bank, inverted
            0xA9, d018, 0x8D, 0x18, 0xD0, // the VIC-II's memory register
        ];
        program.extend(halt(15));
        program
    };
    let case = |program: &[u8]| {
        let mut machine = machine(program, &[]);
        machine.run_until(200);
        machine.letter_case()
    };
    assert_eq!(
        case(&showing(0x14, 0x03)),
        LetterCase::CapitalsOnly,
        "the upper-case set"
    );
    assert_eq!(
        case(&showing(0x16, 0x03)),
        LetterCase::Small,
        "the lower-case set"
    );
    assert_eq!(
        case(&showing(0x16, 0x02)),
        LetterCase::CapitalsOnly,
        "bank 1, with RAM where bank 0 has the ROM"
    );
    assert_eq!(
        case(&showing(0x1E, 0x03)),
        LetterCase::CapitalsOnly,
        "a set of a program's own"
    );
    let mut rom = vec![0xEA; 0x2000];
    let program = showing(0x16, 0x03);
    rom[..program.len()].copy_from_slice(&program);
    rom[0x1FFC..0x1FFE].copy_from_slice(&[0x00, 0xE0]);
    let mut ultimax = machine(&halt(0), &[]);
    let crt = decode(&image(0, 1, 0, &[(0, 0xE000, &rom)])).expect("a CRT");
    ultimax
        .insert_cartridge(&crt)
        .expect("an Ultimax cartridge");
    ultimax.run_until(200);
    assert_eq!(
        ultimax.letter_case(),
        LetterCase::CapitalsOnly,
        "no character ROM in the Ultimax configuration"
    );
}

#[test]
fn restore_pulses_nmi_once_a_press_however_long_it_is_held() {
    // Each NMI, like the reset, runs the count again.
    let mut program = vec![0xEE, 0x00, 0x04];
    program.extend(halt(3));
    let mut machine = machine(&program, &[]);
    machine.run_until(100);
    assert_eq!(machine.ram(0x0400), 1, "the reset's pass");
    let pulse = restore_pulse(Standard::Pal);
    let pressed = machine.board.now;
    machine.set_restore(true);
    machine.run_until(pressed + pulse / 2);
    assert!(machine.board.nmi, "low through the pulse");
    machine.set_restore(false);
    machine.set_restore(true);
    machine.run_until(pressed + pulse + 1);
    assert!(!machine.board.nmi, "let go, a press within the pulse aside");
    machine.run_until(pressed + 4000);
    assert!(!machine.board.nmi, "the key held down holds nothing");
    assert_eq!(machine.ram(0x0400), 2, "one NMI");
    machine.set_restore(false);
    machine.set_restore(true);
    machine.run_until(pressed + 8000);
    assert_eq!(machine.ram(0x0400), 3, "another press, another NMI");
}

#[test]
fn a_snapshot_of_restore_s_pulse_past_one_period_is_refused() {
    use crate::snapshot::SnapshotError;
    let mut machine = busy();
    machine.board.restore_until = machine.board.now + restore_pulse(Standard::Pal) + 1;
    let snapshot = machine.snapshot().expect("taken");
    let mut other = shipped(Standard::Pal, 1);
    assert_eq!(other.restore(&snapshot), Err(SnapshotError::Malformed));
}

#[test]
fn the_deck_follows_the_port_from_power_on_and_through_a_reset() {
    // The motor and write lines set high, then driven: DDR $2F.
    let mut program = vec![0xA9, 0x3F, 0x85, 0x01, 0xA9, 0x2F, 0x85, 0x00];
    program.extend(halt(8));
    let mut machine = machine(&program, &[]);
    let deck = |machine: &Machine| {
        let deck = &machine.board.datasette;
        (deck.motor(), deck.write_line())
    };
    assert_eq!(deck(&machine), (true, false), "every line an input");
    machine.run_until(100);
    assert_eq!(deck(&machine), (false, true), "both driven high");
    machine.reset();
    assert_eq!(deck(&machine), (true, false), "the port's lines let go");
}

#[test]
fn a_snapshot_whose_deck_disagrees_with_the_port_is_refused() {
    use crate::snapshot::SnapshotError;
    for flip in [
        |deck: &mut Datasette, now: u64| deck.set_motor(now, !deck.motor()),
        |deck: &mut Datasette, now: u64| deck.set_write(now, !deck.write_line()),
    ] {
        let mut machine = busy();
        let now = machine.board.now;
        flip(&mut machine.board.datasette, now);
        let snapshot = machine.snapshot().expect("taken");
        let mut other = shipped(Standard::Pal, 1);
        assert_eq!(other.restore(&snapshot), Err(SnapshotError::Malformed));
    }
}

/// A machine of `standard` with the shipped ROMs and the first `drives` of
/// its two drives.
fn shipped(standard: Standard, drives: usize) -> Machine {
    use crate::firmware::{
        C1541_DOS_IMAGE, C64_BASIC_IMAGE, C64_CHARACTER_IMAGE, C64_KERNAL_IMAGE,
    };
    let mut machine = Machine::new(
        Config {
            standard,
            sid: sid::Model::Mos6581,
        },
        Roms {
            basic: C64_BASIC_IMAGE,
            kernal: C64_KERNAL_IMAGE,
            character: C64_CHARACTER_IMAGE,
        },
    )
    .expect("the ROMs fit");
    for unit in Unit::ALL.into_iter().take(drives) {
        machine
            .fit_drive(unit, C1541_DOS_IMAGE)
            .expect("the DOS fits");
    }
    machine
}

/// A machine with a disk in its drive and a tape playing, run a while, a
/// press of RESTORE's pulse still on NMI.
fn busy() -> Machine {
    let mut machine = shipped(Standard::Pal, 1);
    let disk = crate::media::c1541::d64::formatted(b"SNAP", *b"SN");
    let surface = crate::media::c1541::d64::decode(&disk).expect("a D64");
    machine
        .drive_mut(Unit::Eight)
        .expect("a drive")
        .insert(surface);
    let mut image = Vec::from(&b"C64-TAPE-RAW"[..]);
    image.extend([1, 0, 0, 0, 64, 0, 0, 0]);
    image.extend([0x30; 64]);
    machine.insert_tape(crate::media::tap::decode(&image).expect("a TAP"));
    machine.press_play(false);
    machine.set_key(keyboard::Key::A, true);
    machine.run_until(Standard::Pal.clock() / 3 + 7);
    machine.set_restore(true);
    machine.run_until(Standard::Pal.clock() / 3 + 17);
    assert!(machine.board.now < machine.board.restore_until, "in flight");
    machine
}

#[test]
fn a_restored_machine_runs_on_exactly_as_the_one_it_was_taken_from() {
    let mut original = busy();
    let snapshot = original.snapshot().expect("taken");
    let mut restored = shipped(Standard::Pal, 1);
    assert_eq!(restored.restore(&snapshot), Ok(()));
    let until = original.cycles() + 150_000;
    for machine in [&mut original, &mut restored] {
        machine.set_key(keyboard::Key::A, false);
        machine.run_until(until);
    }
    assert_eq!(original.cycles(), restored.cycles());
    assert_eq!(original.shown(), restored.shown(), "the same picture");
    assert_eq!(
        original.snapshot(),
        restored.snapshot(),
        "every part of both alike"
    );
}

#[test]
fn a_snapshot_of_a_machine_built_or_fitted_otherwise_is_refused_unchanged() {
    use crate::snapshot::SnapshotError;
    let snapshot = busy().snapshot().expect("taken");
    for (mut other, refused) in [
        (shipped(Standard::Ntsc, 1), SnapshotError::Fitting),
        (shipped(Standard::Pal, 0), SnapshotError::Fitting),
        (shipped(Standard::Pal, 2), SnapshotError::Fitting),
    ] {
        other.run_until(1000);
        let before = other.snapshot().expect("taken");
        assert_eq!(other.restore(&snapshot), Err(refused));
        assert_eq!(other.snapshot(), Ok(before), "nothing changed");
    }
    let mut kernal = machine(&halt(0), &[]);
    assert_eq!(
        kernal.restore(&snapshot),
        Err(SnapshotError::Fitting),
        "other ROMs"
    );
    let mut same = shipped(Standard::Pal, 1);
    assert!(
        same.restore(&snapshot[..snapshot.len() - 3]).is_err(),
        "cut short"
    );
    let bbc = {
        let mut model_b =
            crate::machine::bbc::Machine::new(crate::firmware::MOS_IMAGE).expect("the MOS fits");
        model_b.snapshot().expect("taken")
    };
    assert_eq!(same.restore(&bbc), Err(SnapshotError::Model));
}
