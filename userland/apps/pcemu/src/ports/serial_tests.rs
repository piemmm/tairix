use super::*;
use crate::ports::mouse::button;
use alloc::vec::Vec;

const US: Nanos = 1_000;
/// 9600 bits a second, and 1200.
const FAST: u16 = 12;
const SLOW: u16 = 96;
/// Line control: eight data bits, seven, and eight with even parity.
const N81: u8 = 0x03;
const N71: u8 = 0x02;
const E81: u8 = 0x1B;

fn set_format(uart: &mut Uart, lcr: u8, divisor: u16, t: Nanos) {
    let [low, high] = divisor.to_le_bytes();
    uart.output(3, 0x80, t);
    uart.output(0, low, t);
    uart.output(1, high, t);
    uart.output(3, lcr, t);
}

fn uart(lcr: u8, divisor: u16) -> Uart {
    let mut uart = Uart::new(Model::Ns16450, Plug::Nothing);
    set_format(&mut uart, lcr, divisor, 0);
    uart
}

/// A device puts `data` on the line at `at`, framed as `format`.
fn send(uart: &mut Uart, at: Nanos, data: u8, format: Format) {
    uart.advance(at);
    uart.incoming = Some(Frame {
        start: at,
        data,
        format,
    });
    uart.frame_started(at);
}

/// Follows the UART's events while it has any, collecting each character
/// it receives with its error bits.
fn collect(uart: &mut Uart) -> Vec<(u8, u8)> {
    let mut heard = Vec::new();
    while let Some(at) = uart.next_event() {
        uart.advance(at);
        let status = uart.input(5, at);
        if status & lsr::DATA_READY != 0 {
            heard.push((uart.input(0, at), status & lsr::ERRORS));
        }
    }
    heard
}

#[test]
fn five_bit_characters_take_one_and_a_half_stop_bits_for_two() {
    let five = Format::from_lcr(0x04, FAST);
    assert_eq!(five.stop, Stop::OneAndHalf);
    assert_eq!(five.character_time(), Hertz::UART.time_of(15 * 8 * 12));
    assert_eq!(Format::from_lcr(0x07, FAST).stop, Stop::Two);
    assert_eq!(
        Format::from_lcr(N81, 0).bit_time(),
        Hertz::UART.time_of(16 * 0x1_0000),
        "a divisor of 0 counts as 65 536"
    );
}

#[test]
fn the_divisor_latch_is_reached_while_dlab_is_set() {
    let mut uart = uart(N81, 0x0180);
    uart.output(3, 0x83, 0);
    assert_eq!((uart.input(0, 0), uart.input(1, 0)), (0x80, 0x01));
    uart.output(3, N81, 0);
    uart.output(1, 0x0F, 0);
    assert_eq!(
        uart.input(1, 0),
        0x0F,
        "the interrupt enable, its high bits zero"
    );
}

#[test]
fn a_character_written_in_loopback_comes_back_at_its_stop_bit() {
    let mut uart = uart(N81, FAST);
    uart.output(4, mcr::LOOP, 0);
    let written = 10 * US;
    uart.output(0, 0x5A, written);
    assert_eq!(
        uart.input(5, written) & (lsr::THR_EMPTY | lsr::EMPTY),
        lsr::THR_EMPTY,
        "straight to the shift register"
    );
    let format = Format::from_lcr(N81, FAST);
    let stop = written + format.half_bit_time() + 9 * format.bit_time();
    assert_eq!(uart.next_event(), Some(stop));
    assert_eq!(uart.input(5, stop - 1) & lsr::DATA_READY, 0);
    assert_eq!(uart.input(5, stop) & lsr::DATA_READY, lsr::DATA_READY);
    assert_eq!(uart.input(0, stop), 0x5A);
    let end = written + format.character_time();
    assert_eq!(uart.input(5, end - 1) & lsr::EMPTY, 0);
    assert_eq!(uart.input(5, end) & lsr::EMPTY, lsr::EMPTY);
}

#[test]
fn the_transmitter_interrupt_comes_as_the_holding_register_empties() {
    let mut uart = uart(N81, FAST);
    uart.output(4, mcr::OUT2, 0);
    uart.output(1, 0x02, 0);
    assert!(uart.irq(), "enabled with the holding register empty");
    assert_eq!(uart.input(2, 0), source::TRANSMIT);
    assert!(!uart.irq(), "reading the identification ends it");
    uart.output(0, b'a', US);
    assert!(uart.irq(), "moved on into the shift register");
    uart.output(0, b'b', 2 * US);
    assert!(!uart.irq(), "held behind the first");
    let end = US + Format::from_lcr(N81, FAST).character_time();
    assert_eq!(uart.next_event(), Some(end));
    uart.advance(end);
    assert!(uart.irq());
    assert_eq!(uart.input(2, end), source::TRANSMIT);
    assert_eq!(uart.input(2, end), 0x01, "nothing else");
}

#[test]
fn out2_gates_the_interrupt_and_loopback_holds_its_pin_inactive() {
    let mut uart = uart(N81, FAST);
    uart.output(1, 0x02, 0);
    assert!(!uart.irq(), "OUT2 low");
    uart.output(4, mcr::OUT2, 0);
    assert!(uart.irq());
    uart.output(4, mcr::OUT2 | mcr::LOOP, 0);
    assert!(!uart.irq());
}

#[test]
fn looped_outputs_drive_the_status_inputs_and_changes_are_reported_once() {
    let mut uart = uart(N81, FAST);
    uart.output(4, mcr::LOOP | mcr::DTR | mcr::OUT1, 0);
    assert_eq!(uart.input(6, 0), 0x60 | 0x02, "DSR and RI up, DSR changed");
    assert_eq!(uart.input(6, 0), 0x60, "reading clears the changes");
    uart.output(4, mcr::LOOP, US);
    assert_eq!(
        uart.input(6, US),
        0x02 | 0x04,
        "DSR fell, RI's trailing edge"
    );
}

#[test]
fn only_the_16450_has_a_scratch_register() {
    let mut old = Uart::new(Model::Ins8250, Plug::Nothing);
    old.output(7, 0x5A, 0);
    assert_eq!(old.input(7, 0), 0xFF);
    let mut new = Uart::new(Model::Ns16450, Plug::Nothing);
    new.output(7, 0x5A, 0);
    assert_eq!(new.input(7, 0), 0x5A);
}

#[test]
fn a_character_not_read_in_time_is_overrun_by_the_next() {
    let mut uart = uart(N81, FAST);
    let format = Format::from_lcr(N81, FAST);
    send(&mut uart, 0, b'a', format);
    let next = format.character_time();
    send(&mut uart, next, b'b', format);
    uart.advance(2 * next);
    let status = uart.input(5, 2 * next);
    assert_eq!(
        status & (lsr::OVERRUN | lsr::DATA_READY),
        lsr::OVERRUN | lsr::DATA_READY
    );
    assert_eq!(uart.input(0, 2 * next), b'b');
}

#[test]
fn a_parity_mismatch_is_flagged_until_the_line_status_is_read() {
    let mut uart = uart(E81, FAST);
    let odd = Format::from_lcr(0x0B, FAST);
    send(&mut uart, 0, 0x01, odd);
    let end = odd.character_time();
    uart.advance(end);
    let flags = lsr::PARITY | lsr::DATA_READY;
    assert_eq!(uart.input(5, end) & flags, flags);
    assert_eq!(uart.input(5, end) & lsr::PARITY, 0);
    assert_eq!(uart.input(0, end), 0x01);
}

#[test]
fn a_line_error_outranks_received_data_in_the_identification() {
    let mut uart = uart(E81, FAST);
    uart.output(1, 0x05, 0);
    send(&mut uart, 0, 0x01, Format::from_lcr(0x0B, FAST));
    let end = Format::from_lcr(E81, FAST).character_time();
    uart.advance(end);
    assert_eq!(uart.input(2, end), source::LINE_STATUS);
    uart.input(5, end);
    assert_eq!(uart.input(2, end), source::RECEIVED);
    uart.input(0, end);
    assert_eq!(uart.input(2, end), 0x01);
}

#[test]
fn the_receiver_checks_only_the_first_stop_bit() {
    let mut uart = uart(0x07, FAST);
    let one = Format::from_lcr(N81, FAST);
    send(&mut uart, 0, b'a', one);
    assert_eq!(collect(&mut uart), [(b'a', 0)]);
    send(&mut uart, one.character_time(), b'b', one);
    assert_eq!(
        collect(&mut uart),
        [(b'b', 0)],
        "its second stop bit not looked for"
    );
}

#[test]
fn a_port_too_fast_for_a_zero_byte_hears_one_break_and_waits_for_the_line_to_mark() {
    let mut uart = uart(N81, FAST);
    let slow = Format::from_lcr(N71, SLOW);
    send(&mut uart, 100 * US, 0x00, slow);
    let fast = Format::from_lcr(N81, FAST);
    let stop = 100 * US + fast.half_bit_time() + 9 * fast.bit_time();
    uart.advance(stop);
    let flags = lsr::DATA_READY | lsr::BREAK | lsr::FRAMING;
    assert_eq!(uart.input(5, stop) & flags, flags);
    assert_eq!(uart.input(0, stop), 0);
    let rise = 100 * US + 8 * slow.bit_time();
    assert_eq!(uart.rx, Rx::Break(rise));
    let later = 100 * US + slow.character_time() + 10 * fast.character_time();
    uart.advance(later);
    assert_eq!(
        uart.input(5, later) & lsr::DATA_READY,
        0,
        "one character for the whole break"
    );
    assert_eq!(uart.rx, Rx::Hunting);
}

#[test]
fn a_start_bit_too_short_for_the_port_is_taken_for_noise() {
    let mut uart = uart(N71, SLOW);
    let fast = Format::from_lcr(N81, FAST);
    send(&mut uart, 0, 0xFF, fast);
    let later = 20 * fast.character_time();
    uart.advance(later);
    assert_eq!(uart.input(5, later) & lsr::DATA_READY, 0);
    assert_eq!(uart.rx, Rx::Hunting);
}

/// A mouse on a port set to `lcr` at 1200 bits a second, powered at 0.
fn mouse_port(lcr: u8) -> Uart {
    let mut uart = Uart::new(Model::Ins8250, Plug::Mouse);
    set_format(&mut uart, lcr, SLOW, 0);
    uart.output(4, mcr::DTR | mcr::RTS | mcr::OUT2, 0);
    uart
}

#[test]
fn the_mouse_answers_m_and_its_byte_interrupts_at_its_stop_bit() {
    let mut uart = mouse_port(N71);
    uart.output(1, 0x01, 0);
    let at = uart.next_event().unwrap();
    uart.advance(at - 1);
    assert!(!uart.irq());
    uart.advance(at);
    assert!(uart.irq());
    assert_eq!(uart.input(0, at), b'M');
    assert!(!uart.irq());
    assert_eq!(uart.next_event(), None, "quiet until it moves");
}

#[test]
fn a_port_set_like_the_mouse_hears_its_packets_whole() {
    let mut uart = mouse_port(N71);
    assert_eq!(collect(&mut uart), [(b'M', 0)]);
    let now = 100_000 * US;
    assert!(uart.mouse(5, -3, button::LEFT, false, now));
    assert_eq!(collect(&mut uart), [(0x6C, 0), (0x05, 0), (0x3D, 0)]);
}

#[test]
fn an_eight_bit_port_hears_the_mouses_stop_bit_as_data_and_frames_badly_back_to_back() {
    let mut uart = mouse_port(N81);
    assert_eq!(
        collect(&mut uart),
        [(0xCD, 0)],
        "idle after it, so framed well"
    );
    uart.mouse(5, -3, button::LEFT, false, 100_000 * US);
    assert_eq!(
        collect(&mut uart),
        [(0xEC, lsr::FRAMING), (0x85, lsr::FRAMING), (0xBD, 0)],
        "each next start bit read as a stop bit, then taken for the start bit it is"
    );
}

#[test]
fn a_port_without_a_mouse_says_so() {
    let mut uart = uart(N71, SLOW);
    assert!(!uart.mouse(1, 1, 0, false, 0));
}
