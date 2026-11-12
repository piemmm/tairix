use alloc::vec;
use alloc::vec::Vec;

use super::fdc::PhysicalDrive;
use super::{rom_image, Machine, RomError, ROM_SIZE};

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

/// Run until the CPU has executed `steps` instructions after reset.
fn run_steps(machine: &mut Machine, steps: usize) {
    for _ in 0..=steps {
        machine.cpu.step(&mut machine.board);
    }
}

/// Run from reset until the CPU passes the end of a `len`-byte program at
/// `&C000`.
fn run_program(machine: &mut Machine, len: usize) {
    let end = 0xC000 + u16::try_from(len).expect("a program fits the OS ROM");
    machine.cpu.step(&mut machine.board);
    while machine.cpu.pc() < end {
        machine.cpu.step(&mut machine.board);
    }
}

#[test]
fn power_on_leaves_no_interrupt_flag_raised() {
    let machine = Machine::new(&os_with(&[])).expect("a 16 KiB OS");
    assert_eq!(machine.system_via().interrupts(), (0, 0));
    assert_eq!(machine.user_via().interrupts(), (0, 0));
}

#[test]
fn a_short_rom_is_mirrored_across_its_sixteen_kilobytes() {
    let mut image = vec![0; 0x2000];
    image[0] = 0x42;
    let mirrored = rom_image(&image).expect("8 KiB is a ROM size");
    assert_eq!(mirrored.len(), ROM_SIZE);
    assert_eq!(mirrored[0x2000], 0x42);
    assert_eq!(rom_image(&[0; 0x3000]), Err(RomError::BadSize));
}

#[test]
fn rom_select_pages_the_chosen_slot_into_the_sideways_window() {
    // LDA #5: STA &FE30: LDA &8000: STA &70
    let code = [0xA9, 0x05, 0x8D, 0x30, 0xFE, 0xAD, 0x00, 0x80, 0x85, 0x70];
    let mut machine = Machine::new(&os_with(&code)).expect("a 16 KiB OS");
    let mut five = vec![0; ROM_SIZE];
    five[0] = 0x55;
    machine.insert_rom(5, &five).expect("slot 5 exists");
    machine
        .insert_rom(4, &vec![0x44; ROM_SIZE])
        .expect("slot 4 exists");
    run_steps(&mut machine, 4);
    assert_eq!(machine.peek(0x70), 0x55);
}

#[test]
fn an_empty_socket_reads_the_floating_bus() {
    // LDA &8000 with slot 0 empty: the last byte on the bus was the operand's
    // high byte.
    let code = [0xAD, 0x00, 0x80, 0x85, 0x70];
    let mut machine = Machine::new(&os_with(&code)).expect("a 16 KiB OS");
    run_steps(&mut machine, 2);
    assert_eq!(machine.peek(0x70), 0x80);
}

#[test]
fn a_one_megahertz_access_stretches_to_the_next_coincident_edge() {
    // LDA &FE40 (System VIA) from an even and an odd starting cycle, and
    // LDA &FE20 (the 2 MHz video ULA) for comparison.
    let code = [0xAD, 0x40, 0xFE, 0xEA, 0xAD, 0x40, 0xFE, 0xAD, 0x20, 0xFE];
    let mut machine = Machine::new(&os_with(&code)).expect("a 16 KiB OS");
    run_steps(&mut machine, 0);
    assert_eq!(machine.cycles(), 7, "reset");
    machine.cpu.step(&mut machine.board);
    // Opcode and operands at 7, 8, 9; the access starts at 10 (even): two cycles.
    assert_eq!(machine.cycles(), 12);
    machine.cpu.step(&mut machine.board);
    assert_eq!(machine.cycles(), 14, "NOP");
    machine.cpu.step(&mut machine.board);
    // Operands at 14, 15, 16; the access starts at 17 (odd): three cycles.
    assert_eq!(machine.cycles(), 20);
    machine.cpu.step(&mut machine.board);
    assert_eq!(machine.cycles(), 24, "the video ULA is not stretched");
}

#[test]
fn the_acia_answers_its_status_register_with_no_interrupt_pending() {
    // An absent 6850 would leave the floating bus's &FE there, whose bit 7
    // reads as an interrupt the MOS would service for ever.
    let code = [0xAD, 0x08, 0xFE, 0x85, 0x70];
    let mut machine = Machine::new(&os_with(&code)).expect("a 16 KiB OS");
    run_steps(&mut machine, 2);
    // Held in master reset with no tape playing, only DCD, the lost
    // carrier, reads set.
    assert_eq!(machine.peek(0x70), 0x04);
}

/// Select keyboard column `column` with the keyboard scanned by the CPU, and
/// store the System VIA's interrupt flags at &70.
fn select_column(column: u8) -> Vec<u8> {
    vec![
        0xA9, 0x7F, 0x8D, 0x43, 0xFE, // DDRA = &7F: PA0-6 drive the keyboard
        0xA9, 0x03, 0x8D, 0x40, 0xFE, // latch bit 3 low: the CPU scans
        0xA9, 0x04, 0x8D, 0x4C, 0xFE, // CA2 on its rising edge
        0xA9, 0x01, 0x8D, 0x4D, 0xFE, // clear CA2's flag
        0xA9, column, 0x8D, 0x4F, 0xFE, // select the column
        0xAD, 0x4D, 0xFE, 0x85, 0x70, // read the flags
    ]
}

#[test]
fn selecting_a_column_with_a_key_down_raises_ca2_while_the_cpu_scans() {
    use super::keyboard::Key;
    let code = select_column(Key::P.column());
    let mut machine = Machine::new(&os_with(&code)).expect("an OS");
    machine.set_key(Key::P, true);
    run_program(&mut machine, code.len());
    assert_ne!(
        machine.peek(0x70) & 0x01,
        0,
        "CA2 flagged column {}",
        Key::P.column()
    );

    let code = select_column(Key::Q.column());
    let mut machine = Machine::new(&os_with(&code)).expect("an OS");
    machine.set_key(Key::P, true);
    run_program(&mut machine, code.len());
    assert_eq!(
        machine.peek(0x70) & 0x01,
        0,
        "no key in column {}",
        Key::Q.column()
    );
}

#[test]
fn a_key_reads_on_pa7_where_the_cpu_selects_it() {
    use super::keyboard::Key;
    let selector = (Key::P.row() << 4) | Key::P.column();
    let mut code = select_column(selector);
    code.extend_from_slice(&[0xAD, 0x4F, 0xFE, 0x85, 0x71]);
    let mut machine = Machine::new(&os_with(&code)).expect("an OS");
    machine.set_key(Key::P, true);
    run_program(&mut machine, code.len());
    assert_ne!(machine.peek(0x71) & 0x80, 0, "P reads as down");
}

#[test]
fn break_resets_the_disc_controller_and_its_drive_lines() {
    // LDA #&3A: STA &FE80: LDA #&23: STA &FE81: LDA #&48: STA &FE81: loading
    // drive 0's head through the drive output register.
    let code = [
        0xA9, 0x3A, 0x8D, 0x80, 0xFE, 0xA9, 0x23, 0x8D, 0x81, 0xFE, 0xA9, 0x48, 0x8D, 0x81, 0xFE,
    ];
    let mut machine = Machine::new(&os_with(&code)).expect("a 16 KiB OS");
    run_program(&mut machine, code.len());
    assert!(machine.drive_active(PhysicalDrive::Zero));
    machine.set_break(true);
    machine.set_break(false);
    assert!(!machine.drive_active(PhysicalDrive::Zero));
}

#[test]
fn each_conversion_s_end_raises_cb1_however_the_handler_orders_its_work() {
    use crate::rom::asm::{abs, imm, Asm};
    // Start a conversion, take its CB1 flag, clear the flag before starting
    // the next conversion, and count the next end of conversion.
    let mut a = Asm::new(0xC000, ROM_SIZE);
    a.sei();
    a.lda(imm(0x04));
    a.sta(abs(0xFE4C));
    a.lda(imm(0x7F));
    a.sta(abs(0xFE4E));
    a.lda(imm(0x0B));
    a.sta(abs(0xFEC0));
    a.here("first");
    a.lda(abs(0xFE4D));
    a.and(imm(0x10));
    a.beq("first");
    a.lda(imm(0x10));
    a.sta(abs(0xFE4D));
    for register in [0xFEC0, 0xFEC2, 0xFEC1] {
        a.lda(abs(register));
    }
    a.lda(imm(0x0A));
    a.sta(abs(0xFEC0));
    a.ldx(imm(0));
    a.ldy(imm(0));
    a.here("second");
    a.lda(abs(0xFE4D));
    a.and(imm(0x10));
    a.bne("ended");
    a.dex();
    a.bne("second");
    a.dey();
    a.bne("second");
    a.lda(imm(0xEE));
    a.sta(abs(0x0070));
    a.here("halt");
    a.jmp("halt");
    a.here("ended");
    a.lda(imm(0x01));
    a.sta(abs(0x0070));
    a.jmp("halt");
    a.org(0xFFFC);
    a.word(0xC000);
    let image = a.finish().expect("the test ROM assembles");
    let mut machine = Machine::new(&image.bytes).expect("a 16 KiB OS");
    machine.run_until(2_000_000);
    assert_eq!(
        machine.peek(0x0070),
        0x01,
        "the second conversion's end raised CB1"
    );
}

/// The User VIA's register `register` on the bus.
fn user_via(register: u8) -> u16 {
    super::sheila::USER_VIA + u16::from(register)
}

/// The regression: an acknowledge's end was checked against its start plus
/// its width before either was bounded, so one at the top of the range
/// overflowed.
#[test]
fn a_restored_printer_acknowledge_at_the_top_of_time_is_refused() {
    use crate::snapshot::{Reader, SnapshotError, Writer};
    let printer = super::Printer {
        output: Vec::new(),
        strobe: true,
        ack: Some((u64::MAX - super::PRINTER_ACK_WIDTH, u64::MAX)),
    };
    let mut w = Writer::new();
    w.put(&printer);
    let body = crate::snapshot::open(&w.finish().unwrap()).unwrap();
    assert_eq!(
        Reader::new(&body).get::<super::Printer>(),
        Err(SnapshotError::Malformed)
    );
}

/// The regression: a strobe pulse that ended between two writes was not
/// seen to end, so the second write's strobe fell from low to low and its
/// byte never reached the printer.
#[test]
fn two_bytes_strobed_in_quick_succession_both_reach_the_printer() {
    use super::via::reg;
    let mut machine = Machine::new(&os_with(&[])).expect("a 16 KiB OS");
    let board = &mut machine.board;
    board.io_write(user_via(reg::DDRA), 0xFF);
    // CA2 pulses low for a cycle on each write to port A.
    board.io_write(user_via(reg::PCR), 0x0A);
    let first = board.now;
    board.io_write(user_via(reg::ORA), b'A');
    for _ in 0..4 {
        board.end_cycle();
    }
    board.io_write(user_via(reg::ORA), b'B');
    assert!(
        board.now - first < super::PRINTER_ACK_DELAY,
        "before any acknowledge"
    );
    assert_eq!(machine.take_printer_output().collect::<Vec<_>>(), b"AB");
}

/// The regression: the printer saw CA2 before its acknowledge ended a
/// handshake, so a snapshot taken as the acknowledge ended kept the strobe
/// low under a VIA driving CA2 high, and the machine restored from it saved
/// itself otherwise.
#[test]
fn a_snapshot_taken_as_the_printer_acknowledges_restores_exactly() {
    use super::via::reg;
    let os = os_with(&[]);
    let mut machine = Machine::new(&os).expect("a 16 KiB OS");
    machine.board.io_write(user_via(reg::DDRA), 0xFF);
    // CA2 handshakes: low on a write to port A, high at CA1's rising edge.
    machine.board.io_write(user_via(reg::PCR), 0x09);
    machine.board.io_write(user_via(reg::ORA), b'A');
    for _ in 0..super::PRINTER_ACK_DELAY + super::PRINTER_ACK_WIDTH + 4 {
        machine.board.end_cycle();
        let taken = machine.snapshot().expect("a machine fits a snapshot");
        let mut copy = Machine::new(&os).expect("a 16 KiB OS");
        copy.restore(&taken)
            .expect("an undamaged snapshot restores");
        let now = machine.board.now;
        assert!(copy.snapshot().expect("fits") == taken, "at cycle {now}");
    }
    assert!(machine.user_via().ca2(), "the handshake ended");
}

/// The regression: the User VIA and the 8271 were reset only as BREAK was
/// released, so while it was held a command ran on and the drive light
/// stayed lit.
#[test]
fn break_holds_the_chips_on_its_line_in_reset_while_it_is_down() {
    let code = [
        0xA9, 0x3A, 0x8D, 0x80, 0xFE, 0xA9, 0x23, 0x8D, 0x81, 0xFE, 0xA9, 0x48, 0x8D, 0x81, 0xFE,
    ];
    let mut machine = Machine::new(&os_with(&code)).expect("a 16 KiB OS");
    run_program(&mut machine, code.len());
    machine
        .board
        .io_write(user_via(super::via::reg::IER), 0x80 | 0x02);
    assert!(machine.drive_active(PhysicalDrive::Zero));
    machine.set_break(true);
    assert!(!machine.drive_active(PhysicalDrive::Zero), "held in reset");
    assert_eq!(machine.user_via().interrupts().1, 0);
    let (held, pc) = (machine.cycles(), machine.cpu().pc());
    machine.run_until(held + 100_000);
    assert!(machine.cycles() >= held + 100_000, "time runs on");
    assert_eq!(machine.cpu().pc(), pc, "the CPU waits");
    machine.set_break(false);
    assert!(!machine.drive_active(PhysicalDrive::Zero));
}

/// A tape of `carrier` cycles of carrier, then a few bytes.
fn tape(carrier: u16) -> crate::media::tape::Tape {
    let mut uef = b"UEF File!\0\x0A\x00".to_vec();
    for (id, body) in [(0x0110u16, &carrier.to_le_bytes()[..]), (0x0100, b"HELLO")] {
        uef.extend_from_slice(&id.to_le_bytes());
        uef.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
        uef.extend_from_slice(body);
    }
    crate::media::tape::Tape::decode(&uef).expect("the UEF decodes")
}

/// A machine with `tape` playing into the 6850 at 1200 baud.
fn listening(tape: crate::media::tape::Tape) -> Machine {
    use super::{serial, sheila};
    let mut machine = Machine::new(&os_with(&[])).expect("a 16 KiB OS");
    machine.insert_tape(tape);
    let rate = 1 | (1 << serial::RECEIVE_SHIFT);
    machine
        .board
        .io_write(sheila::SERIAL_ULA, serial::MOTOR | rate);
    // Divide by 64, eight bits, no parity, one stop bit.
    machine.board.io_write(sheila::ACIA_CONTROL, 0x16);
    machine
}

/// The regression: the 6850 was read before the tape was run to the access,
/// so a frame finishing in the very cycle of a status read was missed by it.
#[test]
fn a_frame_the_tape_finishes_in_the_cycle_of_a_read_is_there_to_read() {
    use super::acia::status;
    // A frame that lands on an odd cycle, where an access to the 6850 can.
    let found = (2000..2016).find_map(|carrier| {
        let mut probe = listening(tape(carrier));
        (0..4_000_000)
            .find_map(|_| {
                probe.board.end_cycle();
                let landed =
                    probe.board.acia.clone().read(probe.board.now, false) & status::RDRF != 0;
                landed.then_some((carrier, probe.board.now - 1))
            })
            .filter(|&(_, at)| at % 2 == 1)
    });
    let (carrier, at) = found.expect("a frame lands on an odd cycle");
    let mut machine = listening(tape(carrier));
    while machine.board.now < at - 1 {
        machine.board.end_cycle();
    }
    let read = machine.board.io_read(super::sheila::ACIA_CONTROL);
    assert_eq!(
        machine.board.now - 1,
        at,
        "read in the cycle the frame lands"
    );
    assert_ne!(read & status::RDRF, 0);
}

/// The regression: a restored board was taken on trust where its devices
/// meet, so a device's event already past or a 6850 clocked otherwise than
/// the serial ULA routes it was restored.
#[test]
fn a_board_is_restorable_only_with_its_events_to_come_and_its_serial_line_wired() {
    let mut machine = listening(tape(2000));
    machine.run_until(10_000);
    assert!(machine.board.restorable());
    let mut late = listening(tape(2000));
    late.run_until(10_000);
    let now = late.board.now;
    late.board.printer.ack = Some((now - 20, now - 10));
    assert!(!late.board.restorable(), "an acknowledge already past");
    let mut rewired = listening(tape(2000));
    rewired.run_until(10_000);
    rewired.board.serial_ula.write(0x00);
    assert!(
        !rewired.board.restorable(),
        "the motor and clocks the ULA no longer sets"
    );
}

#[test]
fn the_machine_hands_out_its_sound_and_stops_keeping_it_when_asked() {
    let mut machine = Machine::new(&os_with(&[])).expect("a 16 KiB OS");
    machine.run_until(80_000);
    let taken = machine.take_audio().count();
    assert_eq!(taken, 10_000, "a sample each update");
    assert_eq!(machine.take_audio().count(), 0, "taken");
    machine.set_sound_output(false);
    machine.run_until(160_000);
    assert_eq!(machine.take_audio().count(), 0);
    machine.set_sound_output(true);
    machine.run_until(240_000);
    assert_eq!(machine.take_audio().count(), 10_000);
}

#[test]
fn the_machine_hands_out_the_rows_its_picture_changed() {
    use super::sheila;
    let mut machine = Machine::new(&os_with(&[])).expect("a 16 KiB OS");
    // Mode 7, with text on its first row.
    let crtc = [
        0x3F, 0x28, 0x33, 0x24, 0x1E, 0x02, 0x19, 0x1C, 0x93, 0x12, 0x72, 0x13, 0x28, 0x00,
    ];
    for (register, value) in (0..).zip(crtc) {
        machine.board.io_write(sheila::CRTC_ADDRESS, register);
        machine.board.io_write(sheila::CRTC_DATA, value);
    }
    machine.board.io_write(sheila::VIDEO_CONTROL, 0x4B);
    machine.board.ram[0x7C00..0x7C05].copy_from_slice(b"HELLO");
    machine.run_until(200_000);
    let rows = machine.take_dirty_rows().rows().count();
    assert!(rows > 0, "the text was drawn");
    machine.run_until(300_000);
    assert_eq!(
        machine.take_dirty_rows().rows().count(),
        0,
        "a still picture"
    );
}

#[test]
fn releasing_every_key_lifts_each_one_held() {
    use super::keyboard::Key;
    let mut machine = Machine::new(&os_with(&[])).expect("a 16 KiB OS");
    for key in [Key::A, Key::SHIFT, Key::RETURN] {
        machine.set_key(key, true);
    }
    let down =
        |machine: &Machine, key: Key| machine.board.keyboard.scan((key.row() << 4) | key.column());
    assert!(down(&machine, Key::A));
    machine.release_keys();
    assert!([Key::A, Key::SHIFT, Key::RETURN]
        .iter()
        .all(|&key| !down(&machine, key)));
}

#[test]
fn a_disc_comes_back_out_as_it_was_written_and_saving_it_clears_its_marks() {
    use crate::media::disc::{Disc, ImageFormat};
    let mut machine = Machine::new(&os_with(&[])).expect("a 16 KiB OS");
    let mut disc = Disc::decode(&[0x11; 2560], ImageFormat::Ssd, false).unwrap();
    disc.track_mut(0, 1).unwrap();
    machine.insert_disc(PhysicalDrive::One, disc.clone());
    assert!(machine.disc(PhysicalDrive::One).is_some_and(Disc::is_dirty));
    machine.mark_disc_saved(PhysicalDrive::One);
    assert!(machine
        .disc(PhysicalDrive::One)
        .is_some_and(|disc| !disc.is_dirty()));
    let out = machine
        .eject_disc(PhysicalDrive::One)
        .expect("the disc was in");
    assert_eq!(out.encode().image, disc.encode().image);
    assert!(machine.disc(PhysicalDrive::One).is_none());
    assert_eq!(machine.eject_disc(PhysicalDrive::Zero), None);
}

#[test]
fn the_deck_reports_its_motor_its_place_and_its_record_key() {
    let mut machine = listening(tape(2000));
    assert!(machine.tape_motor());
    let start = machine.tape_position();
    machine.run_until(100_000);
    assert!(
        machine.tape_position() >= start + 99_000,
        "it plays while the motor runs"
    );
    assert_eq!(machine.tape_record(), (false, false));
    machine.set_tape_record(true);
    assert_eq!(machine.tape_record(), (true, false));
    machine.board.io_write(super::sheila::SERIAL_ULA, 0x00);
    assert!(!machine.tape_motor());
}

#[test]
fn the_lock_lights_follow_the_latch_and_the_bus_monitor_records_io() {
    use super::latch;
    let mut machine = Machine::new(&os_with(&[])).expect("a 16 KiB OS");
    machine.board.latch = latch::SHIFT_LOCK_LED;
    assert_eq!(machine.lock_leds(), (true, false), "lit when low");
    machine.monitor_io(true);
    machine.board.io_write(super::sheila::ROMSEL, 0x03);
    let read = machine.board.io_read(super::sheila::ACIA_CONTROL);
    let trace: Vec<_> = machine.take_io_trace().collect();
    assert_eq!(trace.len(), 2);
    assert_eq!(
        (trace[0].addr, trace[0].value, trace[0].write),
        (super::sheila::ROMSEL, 0x03, true)
    );
    assert_eq!(
        (trace[1].addr, trace[1].value, trace[1].write),
        (super::sheila::ACIA_CONTROL, read, false)
    );
    assert!(trace[0].cycle < trace[1].cycle);
    assert_eq!(machine.take_io_trace().count(), 0, "taken");
    machine.monitor_io(false);
    machine.board.io_write(super::sheila::ROMSEL, 0x04);
    assert_eq!(machine.take_io_trace().count(), 0, "off");
}

#[test]
fn what_the_6850_sends_down_rs423_is_handed_out() {
    use super::{serial, sheila};
    let mut machine = Machine::new(&os_with(&[])).expect("a 16 KiB OS");
    machine.board.io_write(sheila::SERIAL_ULA, serial::RS423);
    machine.board.io_write(sheila::ACIA_CONTROL, 0x16);
    machine.board.io_write(sheila::ACIA_DATA, b'Z');
    machine.run_until(100_000);
    assert_eq!(machine.take_rs423_output().collect::<Vec<_>>(), b"Z");
    assert_eq!(machine.take_rs423_output().count(), 0, "taken");
}
