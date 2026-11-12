use super::super::run_until;
use super::super::tests::{
    booted, call, mask_every_interrupt, poke, runs_end_where_asked, start, stopped_at,
};
use crate::board::ParallelPort;
use crate::cpu::{Bus, Regs};
use crate::machine::{Machine, Template};
use crate::ports::parallel::Device;
use crate::time::{Nanos, NANOS_PER_MILLISECOND as MS, NANOS_PER_SECOND};

const SECOND: Nanos = NANOS_PER_SECOND;

fn word(m: &Machine, at: u32) -> u16 {
    u16::from_le_bytes([m.board().peek(at), m.board().peek(at + 1)])
}

fn ticks(m: &Machine) -> u16 {
    word(m, 0x46C)
}

/// Starts INT `vector` with the registers `set` gives and runs `span` of
/// it; returns where the program halts once it returns.
fn begin(
    m: &mut Machine,
    vector: u8,
    set: impl FnOnce(&mut Regs),
    span: Nanos,
) -> core::ops::Range<u16> {
    let end = start(
        m,
        |asm| {
            asm.sti();
            asm.int(vector);
        },
        set,
    );
    let now = m.now();
    run_until(m, now + span);
    end
}

/// Runs a started call to its end, within `limit`.
fn finish(m: &mut Machine, end: &core::ops::Range<u16>, limit: Nanos) -> Regs {
    let begun = m.now();
    while !stopped_at(m, 0, end.clone()) {
        assert!(m.now() - begun < limit, "the call returned");
        let now = m.now();
        run_until(m, now + MS);
    }
    *m.cpu().regs()
}

fn port(m: &mut Machine, port: u16) -> u8 {
    let at = m.board().cycles_at(m.now());
    m.board_mut().input(port, at)
}

fn set_port(m: &mut Machine, port: u16, value: u8) {
    let at = m.board().cycles_at(m.now());
    m.board_mut().output(port, value, at);
}

#[test]
fn the_post_lists_the_serial_ports_and_finds_the_game_port() {
    let m = booted(Template::TurboXt, None);
    assert_eq!(
        [
            word(&m, 0x400),
            word(&m, 0x402),
            word(&m, 0x404),
            word(&m, 0x406)
        ],
        [0x3F8, 0x2F8, 0, 0]
    );
    let equipment = word(&m, 0x410);
    assert_eq!((equipment >> 9) & 7, 2, "two serial ports");
    assert_eq!(equipment & 0x1000, 0x1000, "a game port");
}

#[test]
fn int_17h_prints_a_byte_and_reports_the_printer() {
    let mut m = booted(Template::IbmXt, None);
    let printed = call(&mut m, 0x17, |r| r.set16(0, 0x0041));
    assert_eq!(printed.get8(4), 0x10, "busy with the byte just strobed in");
    let mut out = [0; 4];
    assert_eq!(m.board_mut().take_printed(0, &mut out), 1);
    assert_eq!(out[0], b'A');
    let status = call(&mut m, 0x17, |r| r.set16(0, 0x0200));
    assert_eq!(status.get8(4), 0x90, "not busy, selected");
    let reset = call(&mut m, 0x17, |r| r.set16(0, 0x0100));
    assert_eq!(reset.get8(4), 0x90);
}

#[test]
fn int_17h_times_out_on_a_printer_that_never_takes_the_byte() {
    let mut config = Template::IbmXt.config();
    config.ports.parallel[0] = Some(ParallelPort {
        base: 0x378,
        device: Device::Nothing,
    });
    let mut m = crate::bios::machine(&config, [None, None]).unwrap();
    m.board_mut()
        .insert_floppy(0, super::super::tests::boot_disk(super::super::tests::stop))
        .unwrap();
    run_until(&mut m, 3 * SECOND);
    poke(&mut m, 0x478, &[1]);
    let before = (ticks(&m), m.now());
    let timed_out = call(&mut m, 0x17, |r| r.set16(0, 0x0058));
    assert_eq!(
        timed_out.get8(4),
        0x31,
        "timed out, paper out and selected floating"
    );
    assert_eq!(timed_out.get8(0), 0x58, "the byte still in AL");
    let spent = m.now() - before.1;
    assert!(
        (SECOND / 2..SECOND / 2 + 100 * MS).contains(&spent),
        "{spent} ns"
    );
    let counted = ticks(&m).wrapping_sub(before.0);
    assert!(
        (8..=10).contains(&counted),
        "the timer served meanwhile: {counted}"
    );
}

#[test]
fn int_17h_and_int_14h_change_nothing_for_a_port_the_post_did_not_find() {
    let mut m = booted(Template::IbmXt, None);
    let printer = call(&mut m, 0x17, |r| {
        r.set16(0, 0x0041);
        r.set16(2, 1);
    });
    assert_eq!(printer.get16(0), 0x0041);
    let serial = call(&mut m, 0x14, |r| {
        r.set16(0, 0x0300);
        r.set16(2, 1);
    });
    assert_eq!(serial.get16(0), 0x0300);
}

#[test]
fn int_14h_sets_the_rate_and_framing_and_reports_the_lines() {
    let mut m = booted(Template::IbmXt, None);
    let set = call(&mut m, 0x14, |r| r.set16(0, 0x0083));
    assert_eq!(set.get8(4), 0x60, "both transmitter registers empty");
    assert_eq!(set.get8(0), 0x00, "no modem line up");
    assert_eq!(
        port(&mut m, 0x3FB),
        0x03,
        "eight data bits, no parity, one stop bit"
    );
    set_port(&mut m, 0x3FB, 0x83);
    assert_eq!(
        (port(&mut m, 0x3F8), port(&mut m, 0x3F9)),
        (0x60, 0x00),
        "1200 bits a second"
    );
    assert_eq!(port(&mut m, 0x3F9) & 0x0F, 0, "interrupts off");
}

#[test]
fn int_14h_times_out_waiting_for_a_modem_a_mouse_is_not() {
    let mut m = booted(Template::IbmXt, None);
    poke(&mut m, 0x47C, &[1]);
    let sent = call(&mut m, 0x14, |r| r.set16(0, 0x0178));
    assert_eq!(sent.get8(4), 0x80, "timed out, no DSR or CTS");
    assert_eq!(sent.get8(0), 0x78, "the character still in AL");
    let received = call(&mut m, 0x14, |r| r.set16(0, 0x0200));
    assert_eq!(received.get8(4) & 0x80, 0x80);
}

#[test]
fn a_serial_wait_times_out_a_run_at_a_time_and_no_run_passes_its_end() {
    let mut m = booted(Template::IbmXt, None);
    poke(&mut m, 0x47C, &[3]);
    let end = start(&mut m, |asm| asm.int(0x14), |r| r.set16(0, 0x0178));
    mask_every_interrupt(&mut m);
    assert!(
        runs_end_where_asked(&mut m, &end, MS, 20_000),
        "the send ended"
    );
    assert_eq!(m.cpu().regs().get8(4) & 0x80, 0x80, "timed out");
}

#[test]
fn int_14h_sends_once_dsr_and_cts_come_and_receives_once_a_character_does() {
    // COM2, with nothing plugged in to answer the port's own lines.
    let mut m = booted(Template::TurboXt, None);
    let com2 = |r: &mut Regs, ax| {
        r.set16(0, ax);
        r.set16(2, 1);
    };
    call(&mut m, 0x14, |r| com2(r, 0x00E3));
    let end = begin(&mut m, 0x14, |r| com2(r, 0x0153), 50 * MS);
    assert!(!stopped_at(&m, 0, end.clone()), "waiting for the modem");
    // Looped back, the UART's own DTR and RTS come back as DSR and CTS.
    set_port(&mut m, 0x2FC, 0x13);
    let sent = finish(&mut m, &end, SECOND);
    assert_eq!(sent.get8(4) & 0x80, 0, "not timed out");
    assert_eq!(sent.get8(4) & 0x20, 0x20, "the holding register was empty");
    let now = m.now();
    run_until(&mut m, now + 2 * MS);
    assert_eq!(port(&mut m, 0x2F8), b'S', "it went out, and came back");
    let end = begin(&mut m, 0x14, |r| com2(r, 0x0200), 10 * MS);
    assert!(!stopped_at(&m, 0, end.clone()), "waiting for DSR");
    set_port(&mut m, 0x2FC, 0x11);
    set_port(&mut m, 0x2F8, b'z');
    let got = finish(&mut m, &end, SECOND);
    assert_eq!(
        (got.get8(0), got.get8(4)),
        (b'z', 0),
        "the character, without errors"
    );
}

#[test]
fn a_mouse_powered_by_a_send_garbles_into_a_port_set_faster_than_it() {
    let mut m = booted(Template::IbmXt, None);
    call(&mut m, 0x14, |r| r.set16(0, 0x00E3));
    poke(&mut m, 0x47C, &[1]);
    call(&mut m, 0x14, |r| r.set16(0, 0x0153));
    let status = port(&mut m, 0x3FD);
    assert_eq!(
        status & 0x03,
        0x03,
        "its M arrived as several characters, overrunning"
    );
}
