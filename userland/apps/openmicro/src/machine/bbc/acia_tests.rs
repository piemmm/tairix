use alloc::vec::Vec;

use super::acia::{status, Acia};

/// Divide by 16, eight data bits, no parity, one stop bit; receive interrupt
/// on, transmit interrupt off, RTS low.
const DIVIDE_16_8N1: u8 = 0x15;
const RECEIVE_INTERRUPT: u8 = 0x80;
const TRANSMIT_INTERRUPT: u8 = 0x20;

fn running(control: u8) -> Acia {
    let mut acia = Acia::new();
    acia.set_clocks(0, 16 * 9_600, 16 * 9_600);
    acia.write(0, false, control);
    acia
}

#[test]
fn master_reset_holds_the_chip_with_only_the_modem_lines_showing() {
    let mut acia = Acia::new();
    acia.set_dcd(true);
    acia.set_cts(true);
    assert_eq!(acia.read(0, false), status::DCD | status::CTS);
    assert!(!acia.irq());
    acia.write(0, true, 0x41);
    assert_eq!(acia.take_transmitted().len(), 0);
}

#[test]
fn an_idle_transmitter_takes_a_byte_one_bit_time_after_it_is_written() {
    let mut acia = running(DIVIDE_16_8N1);
    assert_ne!(acia.read(100, false) & status::TDRE, 0);
    acia.write(100, true, 0x41);
    // A bit lasts 208⅓ cycles at 9600 baud, counted from the write.
    assert_eq!(acia.next_event(), Some(309));
    assert_eq!(
        acia.read(150, false) & status::TDRE,
        0,
        "still in the data register"
    );
    acia.write(160, true, 0x42);
    assert_ne!(acia.read(309, false) & status::TDRE, 0);
    acia.advance(5_000);
    // Ten bits from edge 1 end at edge 11, 2291⅔ cycles after the write.
    assert_eq!(acia.take_transmitted().collect::<Vec<_>>(), [(2_392, 0x42)]);
}

#[test]
fn the_doubled_sync_byte_is_sent_once_whatever_the_clock_phase() {
    // The OS writes its sync byte twice, 166 cycles apart, from an idle
    // transmitter at the cassette's 1200 baud: one must reach the tape,
    // wherever the clock had got to. Two frames would take 33,334 cycles.
    for idle in (0..1_667).step_by(97) {
        let mut acia = Acia::new();
        acia.set_clocks(0, 16 * 1_200, 16 * 1_200);
        acia.write(0, false, DIVIDE_16_8N1);
        let start = 10_000 + idle;
        acia.write(start, true, 0x2A);
        acia.write(start + 166, true, 0x2A);
        acia.advance(start + 40_000);
        assert_eq!(acia.take_transmitted().len(), 1, "idle for {idle}");
    }
}

#[test]
fn a_byte_waiting_behind_a_frame_follows_it_from_its_last_edge() {
    let mut acia = running(DIVIDE_16_8N1);
    acia.write(100, true, 0x41);
    acia.write(400, true, 0x42);
    assert_eq!(acia.read(401, false) & status::TDRE, 0);
    acia.advance(5_000);
    // The second frame follows from edge 11 to edge 21, 4475 cycles after
    // the first write.
    assert_eq!(
        acia.take_transmitted().collect::<Vec<_>>(),
        [(2_392, 0x41), (4_475, 0x42)]
    );
}

#[test]
fn clear_to_send_high_holds_tdre_low() {
    let mut acia = running(DIVIDE_16_8N1);
    acia.set_cts(true);
    assert_eq!(acia.read(0, false) & status::TDRE, 0);
}

#[test]
fn a_frame_received_fills_the_register_and_interrupts_when_enabled() {
    let mut acia = running(DIVIDE_16_8N1 | RECEIVE_INTERRUPT);
    assert!(!acia.irq());
    acia.receive(500, 0x9C, false, false);
    assert!(acia.irq());
    assert_eq!(
        acia.read(501, false) & (status::RDRF | status::IRQ),
        status::RDRF | status::IRQ
    );
    assert_eq!(acia.read(502, true), 0x9C);
    assert!(!acia.irq());
    assert_eq!(acia.read(503, false) & status::RDRF, 0);
}

#[test]
fn an_overrun_shows_once_the_last_good_frame_has_been_read() {
    let mut acia = running(DIVIDE_16_8N1);
    acia.receive(10, 0x01, false, false);
    acia.receive(20, 0x02, false, false);
    assert_eq!(
        acia.read(30, false) & (status::RDRF | status::OVRN),
        status::RDRF
    );
    assert_eq!(acia.read(31, true), 0x01);
    assert_eq!(
        acia.read(32, false) & (status::RDRF | status::OVRN),
        status::RDRF | status::OVRN
    );
    assert_eq!(acia.read(33, true), 0x01);
    assert_eq!(acia.read(34, false) & (status::RDRF | status::OVRN), 0);
}

#[test]
fn a_seven_bit_frame_loses_its_top_bit_and_errors_are_reported() {
    // Seven data bits, even parity, one stop.
    let mut acia = running(0x09);
    acia.receive(10, 0xC1, true, true);
    assert_eq!(
        acia.read(11, false) & (status::FE | status::PE),
        status::FE | status::PE
    );
    assert_eq!(acia.read(12, true), 0x41);
}

#[test]
fn a_rising_carrier_detect_latches_until_status_then_data_are_read() {
    let mut acia = running(DIVIDE_16_8N1 | RECEIVE_INTERRUPT);
    acia.set_dcd(true);
    acia.set_dcd(false);
    assert!(acia.irq());
    acia.read(0, true);
    assert!(acia.irq(), "reading the data alone does not clear it");
    assert_ne!(acia.read(1, false) & status::DCD, 0);
    acia.read(2, true);
    assert!(!acia.irq());
    assert_eq!(acia.read(3, false) & status::DCD, 0);
}

#[test]
fn a_lost_carrier_makes_the_receive_register_read_empty() {
    let mut acia = running(DIVIDE_16_8N1);
    acia.receive(10, 0x55, false, false);
    acia.set_dcd(true);
    assert_eq!(acia.read(11, false) & status::RDRF, 0);
}

#[test]
fn the_transmitter_interrupts_while_its_register_is_empty() {
    let mut acia = running(DIVIDE_16_8N1 | TRANSMIT_INTERRUPT);
    assert!(acia.irq());
    acia.write(0, true, 1);
    acia.write(1, true, 2);
    assert!(!acia.irq(), "the second byte waits for the shifter");
    acia.advance(2_084);
    assert!(acia.irq());
}

#[test]
fn clear_to_send_high_holds_the_transmit_interrupt_off() {
    let mut acia = running(DIVIDE_16_8N1 | TRANSMIT_INTERRUPT);
    acia.set_cts(true);
    assert!(!acia.irq(), "TDRE is inhibited, so nothing interrupts");
    assert_eq!(acia.read(0, false) & status::IRQ, 0);
    acia.set_cts(false);
    assert!(acia.irq());
}

#[test]
fn rts_is_high_only_for_transmitter_control_10() {
    for (bits, rts) in [(0x00, true), (0x20, true), (0x40, false), (0x60, true)] {
        assert_eq!(running(DIVIDE_16_8N1 | bits).rts(), rts, "{bits:#04x}");
    }
    // Master reset leaves the control bits that drive RTS alone.
    assert!(Acia::new().rts());
}

#[test]
fn the_receiver_reports_the_rate_and_frame_it_samples_at() {
    let acia = running(DIVIDE_16_8N1);
    let (rate, format) = acia.receive_rate();
    assert_eq!(rate, 9_600);
    assert_eq!(
        (format.data_bits, format.parity, format.stop_bits),
        (8, None, 1)
    );
    assert_eq!(format.frame_bits(), 10);
}
