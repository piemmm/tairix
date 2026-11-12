use super::*;
use alloc::vec;

const US: Nanos = 1_000;

fn mixer() -> Mixer {
    Mixer::new(48_000).unwrap()
}

/// Writes `byte` and latches it with a Select In pulse of a few
/// microseconds.
fn latch(port: &mut Parallel, byte: u8, t: Nanos, mixer: &mut Mixer) {
    port.output(0, byte, t, mixer);
    port.output(2, control::INIT | control::SELECT_IN, t + US, mixer);
    port.output(2, control::INIT, t + 3 * US, mixer);
}

#[test]
fn nothing_plugged_in_floats_high() {
    let mut mixer = mixer();
    let mut port = Parallel::new(Device::Nothing);
    port.output(0, 0xAA, 0, &mut mixer);
    assert_eq!(
        port.input(0, 0, &mut mixer),
        0xAA,
        "the data latch reads back"
    );
    assert_eq!(port.input(1, 0, &mut mixer), 0x7F);
    port.output(2, 0x15, 0, &mut mixer);
    assert_eq!(port.input(2, 0, &mut mixer), 0xF5);
}

#[test]
fn the_covox_follows_the_data_lines() {
    let mut mixer = mixer();
    let mut port = Parallel::new(Device::Covox);
    port.output(0, 0xFF, 1_000 * US, &mut mixer);
    assert_eq!(port.heard, 0xFF);
    port.output(0, 0x00, 2_000 * US, &mut mixer);
    assert_eq!(port.heard, 0x00);
    assert_eq!(port.input(1, 2_000 * US, &mut mixer), 0x7F);
}

#[test]
fn the_sound_source_latches_on_a_pulse_and_plays_at_7_khz() {
    let mut mixer = mixer();
    let mut port = Parallel::new(Device::SoundSource);
    latch(&mut port, 0xC0, 100 * US, &mut mixer);
    assert_eq!(port.source.len, 1);
    assert_eq!(port.heard, 0, "not before its clock");
    port.advance(100 * US + Hertz::SOUND_SOURCE.time_of(1), &mut mixer);
    assert_eq!(port.source.len, 0);
    assert_eq!(port.heard, 0xC0);
}

#[test]
fn a_full_fifo_shows_on_ack_while_init_is_high() {
    let mut mixer = mixer();
    let mut port = Parallel::new(Device::SoundSource);
    let start = 1_000 * US;
    for n in 0..20 {
        latch(&mut port, 0x80, start + n * 5 * US, &mut mixer);
    }
    assert_eq!(
        port.input(1, start + 100 * US, &mut mixer) & status::ACK,
        status::ACK
    );
    port.output(2, 0, start + 101 * US, &mut mixer);
    assert_eq!(
        port.input(1, start + 102 * US, &mut mixer) & status::ACK,
        0,
        "Init powers the flag"
    );
    port.output(2, control::INIT, start + 103 * US, &mut mixer);
    let drained = start + 103 * US + Hertz::SOUND_SOURCE.time_of(1);
    assert_eq!(
        port.input(1, drained, &mut mixer) & status::ACK,
        0,
        "room for one"
    );
}

#[test]
fn held_low_it_powers_down_and_forgets_its_fifo() {
    let mut mixer = mixer();
    let mut port = Parallel::new(Device::SoundSource);
    latch(&mut port, 0xF0, 10 * US, &mut mixer);
    port.output(2, control::INIT | control::SELECT_IN, 20 * US, &mut mixer);
    port.advance(20 * US + 2 * POWER_DOWN, &mut mixer);
    assert!(!port.source.powered);
    assert_eq!(port.source.len, 0);
    port.output(0, 0x11, 30 * US + 2 * POWER_DOWN, &mut mixer);
    port.output(2, control::INIT, 31 * US + 2 * POWER_DOWN, &mut mixer);
    assert!(port.source.powered, "powered up again");
    assert_eq!(
        port.source.len, 0,
        "the edge that powers it latches nothing"
    );
}

/// Hands the printer `byte` with a Strobe pulse at `t`, as INT 17h does.
fn strobe(port: &mut Parallel, byte: u8, t: Nanos, mixer: &mut Mixer) {
    port.output(0, byte, t, mixer);
    port.output(
        2,
        control::INIT | control::SELECT_IN | control::STROBE,
        t,
        mixer,
    );
    port.output(2, control::INIT | control::SELECT_IN, t + US, mixer);
}

#[test]
fn the_printer_takes_a_byte_on_strobe_and_is_busy_until_it_acknowledges() {
    let mut mixer = mixer();
    let mut port = Parallel::new(Device::Printer);
    assert_eq!(port.input(1, 0, &mut mixer), 0xDF, "ready and selected");
    strobe(&mut port, b'A', 100 * US, &mut mixer);
    assert_eq!(port.input(1, 100 * US, &mut mixer), 0x5F, "busy");
    let ack = 100 * US + PRINT_TIME;
    assert_eq!(port.input(1, ack - 1, &mut mixer), 0x5F);
    assert_eq!(port.input(1, ack, &mut mixer), 0x9F, "ready, Ack low");
    assert_eq!(port.input(1, ack + ACK_PULSE, &mut mixer), 0xDF);
    let mut out = [0; 4];
    assert_eq!(port.take_printed(&mut out), 1);
    assert_eq!(out[0], b'A');
}

#[test]
fn a_strobe_while_busy_is_lost_as_the_printer_ignores_it() {
    let mut mixer = mixer();
    let mut port = Parallel::new(Device::Printer);
    strobe(&mut port, b'A', 0, &mut mixer);
    strobe(&mut port, b'B', 2 * US, &mut mixer);
    strobe(&mut port, b'C', PRINT_TIME, &mut mixer);
    let mut out = [0; 4];
    assert_eq!(port.take_printed(&mut out), 2);
    assert_eq!(&out[..2], b"AC");
}

#[test]
fn init_held_low_resets_the_printer_and_ignores_strobes() {
    let mut mixer = mixer();
    let mut port = Parallel::new(Device::Printer);
    strobe(&mut port, b'A', 0, &mut mixer);
    port.output(2, control::SELECT_IN, 2 * US, &mut mixer);
    assert_eq!(port.input(1, 2 * US, &mut mixer), 0xDF, "no longer busy");
    port.output(2, control::SELECT_IN | control::STROBE, 3 * US, &mut mixer);
    port.output(2, control::INIT | control::SELECT_IN, 4 * US, &mut mixer);
    let mut out = [0; 4];
    assert_eq!(port.take_printed(&mut out), 1, "what it took stays printed");
}

#[test]
fn the_interrupt_follows_ack_while_the_control_register_enables_it() {
    let mut mixer = mixer();
    let mut port = Parallel::new(Device::Printer);
    strobe(&mut port, b'A', 0, &mut mixer);
    let ack = PRINT_TIME;
    assert!(!port.irq(ack), "disabled");
    assert_eq!(port.next_event(0), None);
    let enabled = control::INIT | control::SELECT_IN | control::IRQ;
    port.output(2, enabled, 2 * US, &mut mixer);
    assert_eq!(port.next_event(2 * US), Some(ack));
    assert!(!port.irq(ack - 1));
    assert!(port.irq(ack));
    assert_eq!(port.next_event(ack), Some(ack + ACK_PULSE));
    assert!(!port.irq(ack + ACK_PULSE));
    assert_eq!(port.next_event(ack + ACK_PULSE), None);
    assert_eq!(port.input(2, ack, &mut mixer), 0xE0 | enabled);
}

#[test]
fn a_full_printer_stays_busy_until_its_output_is_drained() {
    let mut mixer = mixer();
    let mut port = Parallel::new(Device::Printer);
    let mut t = 0;
    for n in 0..PAPER {
        strobe(&mut port, u8::try_from(n % 251).unwrap(), t, &mut mixer);
        t += PRINT_TIME + ACK_PULSE;
    }
    assert_eq!(port.input(1, t, &mut mixer) & status::NOT_BUSY, 0, "full");
    strobe(&mut port, 0xEE, t, &mut mixer);
    let mut out = vec![0; PAPER + 1];
    assert_eq!(port.take_printed(&mut out[..1]), 1);
    assert_eq!(out[0], 0, "the first byte, in order");
    assert_eq!(
        port.input(1, t + US, &mut mixer) & status::NOT_BUSY,
        status::NOT_BUSY,
        "room again"
    );
    assert_eq!(
        port.take_printed(&mut out),
        PAPER - 1,
        "the refused byte was never taken"
    );
    assert_eq!(out[PAPER - 2], u8::try_from((PAPER - 1) % 251).unwrap());
}
