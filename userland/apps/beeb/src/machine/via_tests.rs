use super::{irq, reg, Via};

/// The cycle a CPU access to the chip lands on in 1 MHz tick `t`.
const fn at(t: u64) -> u64 {
    2 * t + 1
}

fn start_t1(via: &mut Via, tick: u64, latch: u16, acr: u8) {
    let [low, high] = latch.to_le_bytes();
    via.write(at(tick), reg::ACR, acr);
    via.write(at(tick), reg::T1L_L, low);
    via.write(at(tick), reg::T1C_H, high);
}

/// Timer 1's counter at `tick`; reading T1C-L clears its flag, so callers
/// read a clone where the flag still matters.
fn t1(via: &mut Via, tick: u64) -> u16 {
    let high = via.read(at(tick), reg::T1C_H);
    let low = via.read(at(tick), reg::T1C_L);
    u16::from_le_bytes([low, high])
}

#[test]
fn a_one_shot_timer_counts_down_then_interrupts_one_and_a_half_ticks_after_zero() {
    let mut via = Via::new();
    via.write(at(0), reg::IER, irq::ANY | irq::T1);
    start_t1(&mut via, 10, 5, 0x00);
    let mut seen = alloc::vec::Vec::new();
    for tick in 11..=17 {
        let mut probe = via.clone();
        seen.push(t1(&mut probe, tick));
    }
    assert_eq!(seen, [5, 4, 3, 2, 1, 0, 0xFFFF]);
    // Written in tick 10: the flag rises at the start of tick 10 + 5 + 2.
    let mut early = via.clone();
    early.advance(2 * 17 - 1);
    assert!(!early.irq(), "not yet at the end of tick 16");
    via.advance(2 * 17);
    assert!(via.irq(), "raised at the first half of tick 17");
}

#[test]
fn a_one_shot_timer_reloads_but_interrupts_only_once() {
    let mut via = Via::new();
    start_t1(&mut via, 0, 3, 0x00);
    via.advance(at(5));
    let (flags, _) = via.interrupts();
    assert_ne!(flags & irq::T1, 0);
    via.write(at(6), reg::IFR, irq::T1);
    // The counter has reloaded from the latch.
    let mut probe = via.clone();
    assert_eq!(t1(&mut probe, 7), 2);
    via.advance(at(40));
    let (flags, _) = via.interrupts();
    assert_eq!(
        flags & irq::T1,
        0,
        "no second interrupt without a new start"
    );
}

#[test]
fn a_free_running_timer_interrupts_every_latch_plus_two_ticks() {
    let mut via = Via::new();
    start_t1(&mut via, 0, 8, 0x40);
    let mut rises = alloc::vec::Vec::new();
    for tick in 1..60u64 {
        via.advance(2 * tick);
        let (flags, _) = via.interrupts();
        if flags & irq::T1 != 0 {
            rises.push(tick);
            via.write(at(tick), reg::IFR, irq::T1);
        }
    }
    assert_eq!(rises, [10, 20, 30, 40, 50]);
}

#[test]
fn timer_1_drives_pb7_as_a_one_shot_pulse_or_a_square_wave() {
    let mut via = Via::new();
    start_t1(&mut via, 0, 4, 0x80);
    via.advance(at(2));
    assert_eq!(via.port_b() & 0x80, 0, "low from the start");
    via.advance(at(7));
    assert_ne!(via.port_b() & 0x80, 0, "high from time-out");

    let mut via = Via::new();
    start_t1(&mut via, 0, 4, 0xC0);
    let levels: alloc::vec::Vec<bool> = (1..20)
        .map(|tick| {
            via.advance(at(tick));
            via.port_b() & 0x80 != 0
        })
        .collect();
    let flips = levels.windows(2).filter(|pair| pair[0] != pair[1]).count();
    assert_eq!(flips, 3, "{levels:?}");
}

#[test]
fn timer_2_does_not_reload_and_interrupts_once() {
    let mut via = Via::new();
    via.write(at(0), reg::T2C_L, 3);
    via.write(at(0), reg::T2C_H, 0);
    let read = |via: &mut Via, tick: u64| {
        let low = via.read(at(tick), reg::T2C_L);
        let high = via.read(at(tick), reg::T2C_H);
        u16::from_le_bytes([low, high])
    };
    let mut probe = via.clone();
    assert_eq!(read(&mut probe, 1), 3);
    let mut probe = via.clone();
    assert_eq!(read(&mut probe, 5), 0xFFFF);
    let mut probe = via.clone();
    assert_eq!(read(&mut probe, 7), 0xFFFD);
    via.advance(2 * 5);
    let (flags, _) = via.interrupts();
    assert_ne!(flags & irq::T2, 0);
    via.write(at(5), reg::IFR, irq::T2);
    via.advance(at(0x2_0000));
    let (flags, _) = via.interrupts();
    assert_eq!(flags & irq::T2, 0);
}

#[test]
fn timer_2_counts_pulses_on_pb6() {
    let mut via = Via::new();
    via.write(at(0), reg::ACR, 0x20);
    via.write(at(0), reg::T2C_L, 2);
    via.write(at(0), reg::T2C_H, 0);
    for pulse in 0..3u64 {
        via.set_port_b_pins(at(10 + pulse * 4), 0xBF);
        via.set_port_b_pins(at(12 + pulse * 4), 0xFF);
    }
    let (flags, _) = via.interrupts();
    assert_ne!(flags & irq::T2, 0, "past zero on the third pulse");
    assert_eq!(via.read(at(30), reg::T2C_H), 0xFF);
}

#[test]
fn the_flag_register_summarises_only_enabled_flags_and_the_enable_register_reads_bit_7_set() {
    let mut via = Via::new();
    via.set_ca1(at(0), false);
    via.write(at(1), reg::PCR, 0x01);
    via.set_ca1(at(2), true);
    assert_eq!(
        via.read(at(3), reg::IFR),
        irq::CA1,
        "flagged but not enabled"
    );
    via.write(at(4), reg::IER, irq::ANY | irq::CA1);
    assert_eq!(via.read(at(5), reg::IFR), irq::ANY | irq::CA1);
    assert_eq!(via.read(at(5), reg::IER), irq::ANY | irq::CA1);
    via.write(at(6), reg::IER, irq::CA1);
    assert_eq!(via.read(at(7), reg::IER), irq::ANY);
}

#[test]
fn a_ca1_edge_latches_port_a_and_reading_port_a_clears_it() {
    let mut via = Via::new();
    via.write(at(0), reg::ACR, 0x01);
    via.write(at(0), reg::PCR, 0x00);
    via.set_port_a_pins(0x5A);
    via.set_ca1(at(1), false);
    via.set_port_a_pins(0x00);
    assert_eq!(
        via.read(at(2), reg::ORA),
        0x5A,
        "latched at the falling edge"
    );
    let (flags, _) = via.interrupts();
    assert_eq!(flags & irq::CA1, 0);
}

#[test]
fn an_independent_ca2_flag_survives_a_port_a_read() {
    let mut via = Via::new();
    via.write(at(0), reg::PCR, 0x02);
    via.set_ca2(at(1), false);
    via.read(at(2), reg::ORA);
    let (flags, _) = via.interrupts();
    assert_ne!(flags & irq::CA2, 0);
    via.write(at(3), reg::PCR, 0x00);
    via.set_ca2(at(4), true);
    via.set_ca2(at(5), false);
    via.read(at(6), reg::ORA);
    let (flags, _) = via.interrupts();
    assert_eq!(flags & irq::CA2, 0);
}

#[test]
fn pulse_mode_strobes_ca2_low_for_one_tick_on_a_port_a_write() {
    let mut via = Via::new();
    via.write(at(0), reg::PCR, 0x0A);
    via.write(at(1), reg::ORA, 0x41);
    assert!(!via.ca2());
    via.advance(at(2));
    assert!(via.ca2());
}

#[test]
fn handshake_mode_holds_ca2_low_until_ca1_answers() {
    let mut via = Via::new();
    via.write(at(0), reg::PCR, 0x08);
    via.write(at(1), reg::ORA, 0x41);
    via.advance(at(20));
    assert!(!via.ca2());
    via.set_ca1(at(21), false);
    assert!(via.ca2());
}

#[test]
fn reading_port_b_returns_the_output_register_for_output_pins() {
    let mut via = Via::new();
    via.write(at(0), reg::DDRB, 0x0F);
    via.write(at(0), reg::ORB, 0x0A);
    via.set_port_b_pins(at(1), 0x30);
    assert_eq!(via.read(at(2), reg::ORB), 0x3A);
}

#[test]
fn the_system_clock_shifts_eight_bits_in_over_eight_ticks() {
    let mut via = Via::new();
    via.write(at(0), reg::ACR, 0x08);
    via.set_cb2(at(0), true);
    via.read(at(1), reg::SR);
    via.advance(at(8));
    let (flags, _) = via.interrupts();
    assert_eq!(flags & irq::SR, 0, "seven shifts in");
    via.advance(at(9));
    let (flags, _) = via.interrupts();
    assert_ne!(flags & irq::SR, 0);
    assert_eq!(via.read(at(10), reg::SR), 0xFF);
}

#[test]
fn free_running_shift_out_recirculates_the_byte() {
    let mut via = Via::new();
    via.write(at(0), reg::T2C_L, 0);
    via.write(at(0), reg::ACR, 0x10);
    via.write(at(0), reg::SR, 0x81);
    // One shift every 2 × (0 + 2) ticks; 8 × 4 ticks is a whole byte.
    via.advance(at(32));
    assert_eq!(via.read(at(33), reg::SR), 0x81);
}

#[test]
fn reset_clears_the_registers_and_keeps_the_timers_counting() {
    let mut via = Via::new();
    via.write(at(0), reg::DDRA, 0xFF);
    start_t1(&mut via, 0, 1000, 0x40);
    via.write(at(0), reg::IER, irq::ANY | irq::T1);
    via.reset(at(100));
    assert_eq!(via.read(at(101), reg::DDRA), 0);
    assert_eq!(via.read(at(101), reg::IER), irq::ANY);
    let mut probe = via.clone();
    let counter = t1(&mut probe, 102);
    assert!(counter < 1000 && counter > 800, "{counter}");
}

fn restored(via: &Via, clock: u64) -> Result<Via, crate::snapshot::SnapshotError> {
    let mut w = crate::snapshot::Writer::new();
    w.put(via);
    let body = crate::snapshot::open(&w.finish().unwrap()).unwrap();
    let mut r = crate::snapshot::Reader::new(&body);
    r.set_clock(clock).unwrap();
    r.get()
}

/// The regression: the shift register's window was reckoned from the chip's
/// time before that time was checked, so a time near the top of the range
/// overflowed, and a build that checks overflow aborted.
#[test]
fn a_restored_chip_is_held_to_the_clock_before_anything_is_reckoned_from_its_time() {
    let mut via = Via::new();
    via.acr = 0x04;
    via.sr.next = Some(u64::MAX);
    via.now = u64::MAX - 1;
    assert_eq!(
        restored(&via, 100).err(),
        Some(crate::snapshot::SnapshotError::Malformed)
    );
    let mut behind = Via::new();
    behind.advance(at(10));
    assert!(restored(&behind, at(10)).is_ok());
    assert!(
        restored(&behind, at(20)).is_err(),
        "a chip left behind the clock"
    );
}

/// The regression: RES clears the ACR but timer 2 was not re-anchored, so a
/// timer counting pulses jumped by every tick since it started counting them.
#[test]
fn reset_lets_a_pulse_counting_timer_2_count_on_from_where_it_stood() {
    let mut via = Via::new();
    via.write(at(0), reg::ACR, 0x20);
    via.write(at(0), reg::T2C_L, 0xE8);
    via.write(at(0), reg::T2C_H, 0x03);
    via.advance(at(100_000));
    via.reset(at(100_000));
    let read = |via: &mut Via, tick| {
        let low = via.read(at(tick), reg::T2C_L);
        u16::from_le_bytes([low, via.read(at(tick), reg::T2C_H)])
    };
    let counted = read(&mut via, 100_010);
    assert!((985..=1000).contains(&counted), "{counted}");
}

#[test]
fn ca2_awaits_its_active_edge_only_while_its_flag_is_down() {
    let mut via = Via::new();
    via.write(at(0), reg::PCR, 0x04);
    assert_eq!(via.ca2_edge_awaited(), Some(true), "an input, rising");
    via.set_ca2(at(1), false);
    via.set_ca2(at(1), true);
    assert_eq!(via.ca2_edge_awaited(), None, "its flag is up");
    via.write(at(2), reg::IFR, irq::CA2);
    assert_eq!(via.ca2_edge_awaited(), Some(true));
    via.sync_ca2(false);
    assert_eq!(
        via.interrupts().0 & irq::CA2,
        0,
        "a quiet move raises nothing"
    );
    via.write(at(3), reg::PCR, 0x0C);
    assert_eq!(via.ca2_edge_awaited(), None, "an output");
}
