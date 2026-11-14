use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::machine::{CLOCK, ROM_SIZE};
use crate::media::tape::{Tape, Tone};

/// An OS ROM whose reset vector enters `code` at `&C000`.
fn os_with(code: &[u8]) -> Vec<u8> {
    let mut rom = vec![0xEA; ROM_SIZE];
    rom[..code.len()].copy_from_slice(code);
    for vector in [0x3FFA, 0x3FFC, 0x3FFE] {
        rom[vector] = 0x00;
        rom[vector + 1] = 0xC0;
    }
    rom
}

/// A ROM image whose first byte is `first`.
fn rom_marked(first: u8) -> Vec<u8> {
    let mut rom = vec![0; ROM_SIZE];
    rom[0] = first;
    rom
}

/// `code` followed by a jump to itself, so the machine idles once it ends.
fn ending(code: &[u8]) -> Vec<u8> {
    let mut all = code.to_vec();
    let at = 0xC000 + u16::try_from(all.len()).unwrap();
    all.push(0x4C);
    all.extend_from_slice(&at.to_le_bytes());
    all
}

#[test]
fn basic_and_the_keyboard_can_be_left_only_for_another_of_8_to_15() {
    // LDA #10: STA &FE05; LDA #3: STA &FE05; LDA &8000: STA &70;
    // LDA #12: STA &FE05; LDA #3: STA &FE05; LDA &8000: STA &71
    let code = ending(&[
        0xA9, 10, 0x8D, 0x05, 0xFE, 0xA9, 3, 0x8D, 0x05, 0xFE, 0xAD, 0x00, 0x80, 0x85, 0x70, 0xA9,
        12, 0x8D, 0x05, 0xFE, 0xA9, 3, 0x8D, 0x05, 0xFE, 0xAD, 0x00, 0x80, 0x85, 0x71,
    ]);
    let mut machine = Machine::new(&os_with(&code)).unwrap();
    machine.fit_basic(&rom_marked(0xBA)).unwrap();
    machine.fit_plus1();
    machine.insert_rom(3, &rom_marked(0x33)).unwrap();
    machine.insert_rom(PLUS1_ROM, &rom_marked(0x12)).unwrap();
    machine.run_until(2_000);
    assert_eq!(machine.peek(0x70), 0xBA, "3 refused while BASIC is paged");
    assert_eq!(machine.peek(0x71), 0x33, "taken once 12 was");
}

#[test]
fn only_the_plus_1_takes_cartridges_and_its_own_rom() {
    let mut machine = Machine::new(&os_with(&[])).unwrap();
    assert_eq!(
        machine.insert_rom(PLUS1_ROM, &rom_marked(1)),
        Err(RomError::BadSlot)
    );
    machine.fit_plus1();
    for slot in [0, 1, 2, 3, PLUS1_ROM] {
        assert_eq!(machine.insert_rom(slot, &rom_marked(1)), Ok(()), "{slot}");
    }
    for slot in [4, 8, 10, 13, 16] {
        assert_eq!(
            machine.insert_rom(slot, &rom_marked(1)),
            Err(RomError::BadSlot),
            "{slot}"
        );
    }
}

#[test]
fn the_keyboard_reads_through_rom_8_a_line_to_each_low_address_bit() {
    // LDA #8: STA &FE05; LDA &BFFE: STA &70; LDA &9FFF: STA &71
    let code = ending(&[
        0xA9, 8, 0x8D, 0x05, 0xFE, 0xAD, 0xFE, 0xBF, 0x85, 0x70, 0xAD, 0xFF, 0x9F, 0x85, 0x71,
    ]);
    let mut machine = Machine::new(&os_with(&code)).unwrap();
    machine.set_key(Key::RIGHT, true);
    machine.set_key(Key::SHIFT, true);
    machine.run_until(1_000);
    assert_eq!(machine.peek(0x70), 0x01, "line 0: cursor right");
    assert_eq!(machine.peek(0x71), 0x08, "line 13: SHIFT");
}

/// The cycles an access to `addr` takes, begun at `at`.
fn access_cycles(board: &mut Board, at: u64, addr: u16) -> u64 {
    board.now = at;
    board.next_event = u64::MAX;
    board.read(addr);
    board.now - at
}

#[test]
fn ram_is_held_off_by_the_display_and_the_1_mhz_parts_stretch() {
    let mut board = Board::new(Rom::of(&os_with(&[])).unwrap());
    let in_window = 10 * LINE_CYCLES + 30;
    assert_eq!(
        access_cycles(&mut board, in_window, 0x1000),
        104 - 30 + 1,
        "mode 0"
    );
    assert_eq!(
        access_cycles(&mut board, in_window, 0xC100),
        1,
        "the OS at 2 MHz"
    );
    assert_eq!(
        access_cycles(&mut board, in_window, 0xFE04),
        1,
        "the ULA at 2 MHz"
    );
    assert_eq!(
        access_cycles(&mut board, in_window, 0xFC00),
        2,
        "FRED at 1 MHz"
    );
    assert_eq!(
        access_cycles(&mut board, in_window + 1, 0xFD00),
        3,
        "JIM from an odd cycle"
    );
    board.video.write_mode(6);
    assert_eq!(
        access_cycles(&mut board, in_window, 0x1000),
        2,
        "mode 6 RAM at 1 MHz"
    );
    assert_eq!(access_cycles(&mut board, 300 * LINE_CYCLES + 1, 0x1000), 3);
}

#[test]
fn rtc_and_display_end_each_rise_once_a_frame_and_hold_until_cleared() {
    let mut machine = Machine::new(&os_with(&ending(&[]))).unwrap();
    machine.run_until(RTC_AT - 4);
    assert_eq!(machine.board.status & irq::RTC, 0);
    machine.run_until(RTC_AT + 4);
    assert_ne!(machine.board.status & irq::RTC, 0);
    assert_eq!(machine.board.status & irq::DISPLAY_END, 0);
    assert!(!machine.board.irq, "nothing enabled");
    machine.run_until(DISPLAY_END_AT + 4);
    assert_ne!(machine.board.status & irq::DISPLAY_END, 0);
    assert_eq!(
        DISPLAY_END_AT - RTC_AT,
        155 * LINE_CYCLES,
        "about ten milliseconds"
    );
    let board = &mut machine.board;
    board.io_write(sheila::STATUS, irq::RTC);
    assert!(board.irq, "an enabled condition present");
    let status = board.io_read(sheila::STATUS);
    assert_eq!(
        status & (irq::SET | irq::MASTER | irq::POWER_ON),
        irq::SET | irq::MASTER | irq::POWER_ON
    );
    assert_eq!(
        board.io_read(sheila::STATUS) & irq::POWER_ON,
        0,
        "the first read clears power-on"
    );
    board.io_write(sheila::PAGING, paging::CLEAR_RTC);
    assert_eq!(board.status & irq::RTC, 0);
    assert!(!board.irq);
    assert_ne!(board.status & irq::DISPLAY_END, 0, "the other stands");
    board.io_write(sheila::PAGING, paging::CLEAR_DISPLAY_END);
    machine.run_until(FRAME_CYCLES + RTC_AT + 4);
    assert_eq!(
        machine.board.status & irq::CONDITIONS,
        irq::RTC,
        "once a frame"
    );
}

#[test]
fn a_write_only_register_reads_as_the_os_beneath() {
    let mut os = os_with(&[]);
    os[0x3E07] = 0x5A;
    os[0x3EA9] = 0xA5;
    let mut board = Board::new(Rom::of(&os).unwrap());
    assert_eq!(board.io_read(0xFE07), 0x5A);
    assert_eq!(
        board.io_read(0xFEA9),
        0xA5,
        "a palette register's image, in the page's repeats"
    );
}

/// A tape: a second of carrier, then `bytes` framed 8N1 at the Electron's
/// rate, then silence.
fn tape_of(bytes: &[u8]) -> Tape {
    let mut tape = Tape::default();
    tape.push(Tone::High, CLOCK).unwrap();
    for &byte in bytes {
        tape.push(Tone::Low, BIT_CYCLES).unwrap();
        for k in 0..8 {
            let tone = if byte >> k & 1 != 0 {
                Tone::High
            } else {
                Tone::Low
            };
            tape.push(tone, BIT_CYCLES).unwrap();
        }
        tape.push(Tone::High, BIT_CYCLES).unwrap();
    }
    tape.push(Tone::Silence, CLOCK).unwrap();
    tape
}

#[test]
fn the_ula_hears_the_carrier_and_receives_each_byte_from_the_tape() {
    let mut machine = Machine::new(&os_with(&ending(&[]))).unwrap();
    machine.insert_tape(tape_of(b"E"));
    machine.board.io_write(sheila::CONTROL, control::MOTOR);
    machine.run_until(CLOCK / 2);
    assert_ne!(
        machine.board.status & irq::HIGH_TONE,
        0,
        "the carrier heard"
    );
    assert_eq!(machine.board.status & irq::RECEIVE_FULL, 0);
    machine.run_until(CLOCK + 12 * BIT_CYCLES);
    assert_ne!(machine.board.status & irq::RECEIVE_FULL, 0);
    assert_eq!(machine.board.io_read(sheila::CASSETTE), b'E');
    assert_eq!(
        machine.board.status & irq::RECEIVE_FULL,
        0,
        "a read empties it"
    );
    assert!(machine.tape_motor());
}

#[test]
fn bytes_sent_in_cassette_output_are_recorded_and_empty_the_register_once_gone() {
    let mut machine = Machine::new(&os_with(&ending(&[]))).unwrap();
    machine.insert_tape(Tape::default());
    machine.set_tape_record(true);
    machine
        .board
        .io_write(sheila::CONTROL, control::MOTOR | control::CASSETTE_OUTPUT);
    assert_ne!(
        machine.board.status & irq::TRANSMIT_EMPTY,
        0,
        "empty on entering output"
    );
    machine.run_until(CLOCK / 10);
    machine.board.io_write(sheila::CASSETTE, b'Z');
    machine.board.io_write(sheila::CASSETTE, b'X');
    assert_eq!(machine.board.status & irq::TRANSMIT_EMPTY, 0);
    machine.run_until(CLOCK / 10 + FRAME + 10);
    assert_eq!(
        machine.board.status & irq::TRANSMIT_EMPTY,
        0,
        "the second still to go"
    );
    machine.run_until(CLOCK / 10 + 2 * FRAME + 10);
    assert_ne!(machine.board.status & irq::TRANSMIT_EMPTY, 0);
    machine.run_until(CLOCK / 5);
    machine.board.io_write(sheila::CONTROL, 0);
    let recorded = machine.eject_tape().unwrap();
    // Played back, the recording holds the two bytes.
    let mut playback = Machine::new(&os_with(&ending(&[]))).unwrap();
    playback.insert_tape(recorded);
    playback.board.io_write(sheila::CONTROL, control::MOTOR);
    let mut heard = Vec::new();
    for slice in 1..=40 {
        playback.run_until(slice * CLOCK / 100);
        if playback.board.status & irq::RECEIVE_FULL != 0 {
            heard.push(playback.board.io_read(sheila::CASSETTE));
        }
    }
    assert_eq!(heard, b"ZX");
}

#[test]
fn the_plus_1_ports_answer_in_fred_and_nothing_does_without_it() {
    let mut machine = Machine::new(&os_with(&ending(&[]))).unwrap();
    machine.board.bus = 0x77;
    assert_eq!(
        machine.board.io_read(plus1::STATUS),
        0x77,
        "the floating bus"
    );
    machine.fit_plus1();
    machine.set_analogue(1, 0x8000);
    machine.set_fire_buttons(0x02);
    machine.board.io_write(plus1::ADC, 0x05);
    let status = machine.board.io_read(plus1::STATUS);
    assert_eq!(
        status & plus1::status::CONVERTING,
        plus1::status::CONVERTING
    );
    assert_eq!(status & plus1::status::FIRE_2, 0, "the second button held");
    machine.run_until(machine.cycles() + plus1::CONVERSION + 10);
    assert_eq!(
        machine.board.io_read(plus1::STATUS) & plus1::status::CONVERTING,
        0
    );
    assert_eq!(machine.board.io_read(plus1::ADC), 0x80);
    machine.board.io_write(plus1::PRINTER, b'P');
    assert_eq!(machine.take_printer_output().collect::<Vec<_>>(), b"P");
}

/// A machine running a loop that writes the screen and turns the palette
/// over, with the Plus 1 and BASIC fitted and a tape playing.
fn busy_machine() -> Machine {
    // loop: INC &3000,X: INX: STX &FE08: LDA &FC70: JMP loop
    let code = [
        0xFE, 0x00, 0x30, 0xE8, 0x8E, 0x08, 0xFE, 0xAD, 0x70, 0xFC, 0x4C, 0x00, 0xC0,
    ];
    let mut machine = Machine::new(&os_with(&code)).unwrap();
    machine.fit_basic(&rom_marked(0xBA)).unwrap();
    machine.fit_plus1();
    machine.insert_tape(tape_of(b"SNAP"));
    machine
        .board
        .io_write(sheila::CONTROL, control::MOTOR | control::SOUND | 0x08);
    machine.board.io_write(sheila::COUNTER, 40);
    machine.board.io_write(plus1::ADC, 0x04);
    machine
}

#[test]
fn a_restored_machine_runs_on_as_the_one_it_was_taken_from() {
    let mut original = busy_machine();
    original.run_until(CLOCK / 3 + 17);
    let snapshot = original.snapshot().unwrap();
    let mut copy = busy_machine();
    copy.restore(&snapshot).unwrap();
    for machine in [&mut original, &mut copy] {
        machine.take_audio();
        machine.run_until(CLOCK);
    }
    assert_eq!(original.cycles(), copy.cycles());
    assert_eq!(original.snapshot().unwrap(), copy.snapshot().unwrap());
    assert_eq!(original.video().shown(), copy.video().shown());
    assert_eq!(
        original.take_audio().collect::<Vec<_>>(),
        copy.take_audio().collect::<Vec<_>>()
    );
}

#[test]
fn a_snapshot_restores_only_onto_an_electron_fitted_as_it_was() {
    let mut original = busy_machine();
    original.run_until(CLOCK / 10);
    let snapshot = original.snapshot().unwrap();
    let mut bare = Machine::new(&os_with(&[0x4C, 0x00, 0xC0])).unwrap();
    assert_eq!(
        bare.restore(&snapshot),
        Err(SnapshotError::Fitting),
        "another OS"
    );
    let mut no_plus1 = busy_machine();
    no_plus1.board.plus1 = None;
    assert_eq!(no_plus1.restore(&snapshot), Err(SnapshotError::Fitting));
    let mut model_b = crate::machine::bbc::Machine::new(&os_with(&[])).unwrap();
    assert_eq!(model_b.restore(&snapshot), Err(SnapshotError::Model));
    let theirs = model_b.snapshot().unwrap();
    assert_eq!(original.restore(&theirs), Err(SnapshotError::Model));
}

/// The regression: a write to &FE01, which holds nothing, was taken for a
/// palette register below the first, and the subtraction overflowed.
#[test]
fn a_write_to_every_sheila_address_is_taken_or_ignored() {
    // LDX #0: loop STA &FE00,X: INX: BNE loop, A walking every value.
    let code = ending(&[0xA2, 0x00, 0x9D, 0x00, 0xFE, 0x69, 0x3B, 0xE8, 0xD0, 0xF8]);
    let mut machine = Machine::new(&os_with(&code)).expect("a 16 KiB OS");
    machine.run_until(CLOCK / 10);
    assert!(machine.cycles() >= CLOCK / 10);
}
