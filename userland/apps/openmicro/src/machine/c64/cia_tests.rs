use super::*;

const PAL_MAINS: Mains = Mains {
    clock: 985_248,
    hz: 50,
};

fn chip(model: Model) -> Cia {
    Cia::new(model, PAL_MAINS)
}

/// A chip with timer A's latch and counter at `latch`, written stopped in
/// cycles 1 and 2.
fn with_timer_a(model: Model, latch: u16) -> Cia {
    let mut cia = chip(model);
    let [low, high] = latch.to_le_bytes();
    cia.write(1, reg::TA_LO, low);
    cia.write(2, reg::TA_HI, high);
    cia
}

#[test]
fn reset_leaves_the_timers_stopped_with_their_latches_all_ones() {
    let mut cia = chip(Model::Mos6526);
    assert_eq!(cia.read(5, reg::TA_LO), 0xFF);
    assert_eq!(cia.read(6, reg::TB_HI), 0xFF);
    assert_eq!(cia.read(7, reg::CRA), 0);
    assert_eq!(cia.port_a(), 0xFF, "every line an input, pulled up");
    assert_eq!(cia.read(8, reg::TOD_HOURS), 0x01);
}

#[test]
fn a_started_timer_shows_its_first_count_three_cycles_after_the_write() {
    let mut cia = with_timer_a(Model::Mos6526, 0x10);
    cia.write(20, reg::CRA, cr::START);
    assert_eq!(cia.read(21, reg::TA_LO), 0x10);
    assert_eq!(cia.read(22, reg::TA_LO), 0x10);
    assert_eq!(cia.read(23, reg::TA_LO), 0x0F);
    assert_eq!(cia.read(24, reg::TA_LO), 0x0E);
}

#[test]
fn a_continuous_timer_underflows_every_latch_plus_one_cycles() {
    let mut cia = with_timer_a(Model::Mos8521, 5);
    cia.write(3, reg::ICR, icr::SET | icr::TA);
    cia.write(4, reg::CRA, cr::START);
    cia.advance(11);
    assert!(!cia.irq());
    // The 8521 raises its line at the end of the underflow's cycle.
    cia.advance(12);
    assert!(cia.irq(), "underflow in cycle 11");
    assert_eq!(cia.read(12, reg::ICR), icr::SET | icr::TA);
    cia.advance(17);
    assert!(!cia.irq(), "the read released the line");
    cia.advance(18);
    assert!(cia.irq(), "the next underflow, six cycles on");
}

#[test]
fn the_6526_raises_its_line_a_cycle_later() {
    let mut cia = with_timer_a(Model::Mos6526, 5);
    cia.write(3, reg::ICR, icr::SET | icr::TA);
    cia.write(4, reg::CRA, cr::START);
    cia.advance(12);
    assert!(!cia.irq());
    cia.advance(13);
    assert!(cia.irq());
}

#[test]
fn reading_the_flags_in_the_underflow_s_cycle_keeps_the_line_up() {
    for model in [Model::Mos6526, Model::Mos8521] {
        let mut cia = with_timer_a(model, 5);
        cia.write(3, reg::ICR, icr::SET | icr::TA);
        cia.write(4, reg::CRA, cr::START | cr::ONE_SHOT);
        let flags = cia.read(11, reg::ICR);
        let set = if model == Model::Mos8521 { icr::SET } else { 0 };
        assert_eq!(flags, icr::TA | set, "{model:?}");
        cia.advance(20);
        assert!(!cia.irq(), "{model:?}: the read took the interrupt away");
    }
}

#[test]
fn a_one_shot_timer_stops_at_its_underflow_with_its_latch_reloaded() {
    let mut cia = with_timer_a(Model::Mos6526, 5);
    cia.write(4, reg::CRA, cr::START | cr::ONE_SHOT);
    assert_eq!(cia.read(10, reg::CRA) & cr::START, cr::START);
    assert_eq!(cia.read(12, reg::CRA) & cr::START, 0);
    assert_eq!(cia.read(30, reg::TA_LO), 5);
    assert_eq!(cia.read(31, reg::ICR), icr::TA);
}

#[test]
fn a_forced_load_reaches_the_counter_two_cycles_after_the_write() {
    let mut cia = with_timer_a(Model::Mos6526, 0x100);
    cia.write(4, reg::CRA, cr::START);
    cia.write(10, reg::TA_LO, 0x34);
    cia.write(11, reg::TA_HI, 0x12);
    assert_eq!(
        cia.read(12, reg::TA_HI),
        0x00,
        "running: the latch alone changed"
    );
    cia.write(20, reg::CRA, cr::START | cr::FORCE_LOAD);
    assert_ne!(cia.read(21, reg::TA_HI), 0x12);
    assert_eq!(cia.read(22, reg::TA_LO), 0x34);
    assert_eq!(cia.read(23, reg::TA_LO), 0x34);
    assert_eq!(cia.read(24, reg::TA_LO), 0x33);
    assert_eq!(cia.read(25, reg::CRA), cr::START, "the strobe reads clear");
}

#[test]
fn timer_b_counts_timer_a_s_underflows() {
    let mut cia = with_timer_a(Model::Mos8521, 2);
    cia.write(3, reg::TB_LO, 3);
    cia.write(4, reg::TB_HI, 0);
    cia.write(5, reg::ICR, icr::SET | icr::TB);
    cia.write(6, reg::CRB, cr::START | cr::COUNT_TA);
    cia.write(7, reg::CRA, cr::START);
    // Timer A underflows in cycles 11, 14, 17 and 20; the fourth takes
    // timer B through zero a cycle later.
    cia.advance(21);
    assert!(!cia.irq());
    cia.advance(22);
    assert!(cia.irq());
    assert_eq!(cia.read(22, reg::ICR), icr::SET | icr::TB | icr::TA);
    assert_eq!(cia.read(23, reg::TB_LO), 3);
}

#[test]
fn the_6526_loses_timer_b_s_flag_to_a_read_the_cycle_before() {
    for (model, kept) in [(Model::Mos6526, 0), (Model::Mos8521, icr::TB)] {
        let mut cia = chip(model);
        cia.write(1, reg::TB_LO, 5);
        cia.write(2, reg::TB_HI, 0);
        cia.write(4, reg::CRB, cr::START | cr::ONE_SHOT);
        assert_eq!(cia.read(10, reg::ICR), 0);
        assert_eq!(cia.read(13, reg::ICR), kept, "{model:?}");
    }
}

#[test]
fn unmasking_a_flagged_cause_raises_the_line() {
    let mut cia = with_timer_a(Model::Mos8521, 5);
    cia.write(4, reg::CRA, cr::START | cr::ONE_SHOT);
    cia.advance(30);
    assert!(!cia.irq());
    cia.write(30, reg::ICR, icr::SET | icr::TA);
    cia.advance(31);
    assert!(!cia.irq());
    cia.advance(32);
    assert!(cia.irq(), "a cycle after the write");
}

#[test]
fn flag_s_falling_edge_raises_its_cause() {
    let mut cia = chip(Model::Mos6526);
    cia.set_flag(10, true);
    cia.set_flag(11, false);
    cia.set_flag(12, true);
    assert_eq!(cia.read(20, reg::ICR), icr::FLAG);
    assert_eq!(cia.read(21, reg::ICR), 0);
}

#[test]
fn pb6_toggles_from_high_at_the_start_and_pulses_for_a_cycle() {
    let mut cia = with_timer_a(Model::Mos6526, 5);
    cia.write(4, reg::CRA, cr::START | cr::PB_ON | cr::TOGGLE);
    cia.advance(5);
    assert_eq!(cia.port_b() & 0x40, 0x40);
    cia.advance(11);
    assert_eq!(
        cia.port_b() & 0x40,
        0,
        "toggled by the underflow in cycle 11"
    );
    cia.advance(17);
    assert_eq!(cia.port_b() & 0x40, 0x40);

    let mut cia = with_timer_a(Model::Mos6526, 5);
    cia.write(4, reg::CRA, cr::START | cr::PB_ON);
    cia.advance(10);
    assert_eq!(cia.port_b() & 0x40, 0);
    cia.advance(11);
    assert_eq!(cia.port_b() & 0x40, 0x40);
    cia.advance(12);
    assert_eq!(cia.port_b() & 0x40, 0);
}

#[test]
fn the_ports_drive_their_outputs_and_pull_their_inputs_up() {
    let mut cia = chip(Model::Mos6526);
    cia.write(1, reg::DDRA, 0x0F);
    cia.write(2, reg::PRA, 0x05);
    assert_eq!(cia.port_a(), 0xF5);
    assert_eq!(cia.read(3, reg::PRA), 0xF5);
}

/// The cycle PAL's 50 Hz mains cycle `tick` begins in.
fn mains(tick: u64) -> u64 {
    PAL_MAINS.tick_cycle(tick)
}

/// The time read hours first, latching, tenths last, releasing.
fn time(cia: &mut Cia, at: u64) -> [u8; 4] {
    [
        cia.read(at, reg::TOD_HOURS),
        cia.read(at + 1, reg::TOD_MINUTES),
        cia.read(at + 2, reg::TOD_SECONDS),
        cia.read(at + 3, reg::TOD_TENTHS),
    ]
}

#[test]
fn the_clock_counts_tenths_from_50_hz_and_turns_11_59_into_noon() {
    let mut cia = chip(Model::Mos6526);
    cia.write(1, reg::CRA, cr::TOD_50HZ);
    cia.write(2, reg::TOD_HOURS, 0x11);
    cia.write(3, reg::TOD_MINUTES, 0x59);
    cia.write(4, reg::TOD_SECONDS, 0x59);
    cia.write(5, reg::TOD_TENTHS, 0x09);
    assert_eq!(time(&mut cia, 10), [0x11, 0x59, 0x59, 0x09]);
    assert_eq!(time(&mut cia, mains(5) - 4), [0x11, 0x59, 0x59, 0x09]);
    assert_eq!(time(&mut cia, mains(5) + 1), [PM | 0x12, 0, 0, 0]);
    assert_eq!(time(&mut cia, mains(10) + 1), [PM | 0x12, 0, 0, 1]);
}

#[test]
fn the_60_hz_divider_counts_six_mains_cycles_a_tenth() {
    let mut cia = chip(Model::Mos6526);
    cia.write(5, reg::TOD_TENTHS, 0);
    assert_eq!(time(&mut cia, mains(6) - 4)[3], 0);
    assert_eq!(time(&mut cia, mains(6) + 1)[3], 1);
}

#[test]
fn twelve_flips_the_half_of_the_day_and_one_follows_twelve() {
    let mut cia = chip(Model::Mos6526);
    cia.write(1, reg::CRA, cr::TOD_50HZ);
    cia.write(2, reg::TOD_HOURS, 0x12);
    assert_eq!(cia.read(3, reg::TOD_HOURS), PM | 0x12);
    assert_eq!(cia.read(4, reg::TOD_TENTHS), 0);
    cia.write(5, reg::TOD_HOURS, PM | 0x12);
    cia.write(6, reg::TOD_MINUTES, 0x59);
    cia.write(7, reg::TOD_SECONDS, 0x59);
    cia.write(8, reg::TOD_TENTHS, 0x09);
    assert_eq!(time(&mut cia, mains(5) + 1), [0x01, 0, 0, 0]);
}

#[test]
fn a_read_of_the_hours_holds_the_time_until_the_tenths_are_read() {
    let mut cia = chip(Model::Mos6526);
    cia.write(1, reg::CRA, cr::TOD_50HZ);
    cia.write(2, reg::TOD_TENTHS, 0);
    assert_eq!(cia.read(3, reg::TOD_HOURS), 0x01);
    assert_eq!(cia.read(mains(20) + 1, reg::TOD_TENTHS), 0, "held");
    assert_eq!(cia.read(mains(20) + 2, reg::TOD_TENTHS), 4);
}

#[test]
fn writing_the_hours_stops_the_clock_until_the_tenths_are_written() {
    let mut cia = chip(Model::Mos6526);
    cia.write(1, reg::CRA, cr::TOD_50HZ);
    cia.write(2, reg::TOD_TENTHS, 0);
    cia.write(3, reg::TOD_HOURS, 0x05);
    assert_eq!(time(&mut cia, mains(20) + 1), [0x05, 0, 0, 0]);
    cia.write(mains(20) + 10, reg::TOD_TENTHS, 0);
    assert_eq!(time(&mut cia, mains(25) - 4)[3], 0);
    assert_eq!(time(&mut cia, mains(25) + 1)[3], 1);
}

#[test]
fn the_alarm_raises_its_cause_when_the_time_reaches_it() {
    let mut cia = chip(Model::Mos8521);
    cia.write(1, reg::CRA, cr::TOD_50HZ);
    cia.write(2, reg::CRB, cr::ALARM);
    cia.write(3, reg::TOD_TENTHS, 0x01);
    cia.write(4, reg::TOD_HOURS, 0x01);
    cia.write(5, reg::CRB, 0);
    cia.write(6, reg::ICR, icr::SET | icr::ALARM);
    cia.write(7, reg::TOD_TENTHS, 0);
    assert_eq!(
        cia.next_event(),
        Some(mains(1) + 1),
        "each mains cycle is watched"
    );
    cia.advance(mains(5) + 1);
    assert!(cia.irq(), "the fifth mains cycle reaches 01:00:00.1");
    assert_eq!(cia.read(mains(5) + 1, reg::ICR), icr::SET | icr::ALARM);
}

#[test]
fn an_alarm_matched_only_while_it_is_being_set_still_raises_its_cause() {
    let mut cia = chip(Model::Mos6526);
    cia.write(1, reg::CRB, cr::ALARM);
    // The alarm reads 01:00:00.0, the stopped clock's time, until its
    // tenths are written.
    cia.write(2, reg::TOD_HOURS, 0x01);
    cia.write(3, reg::TOD_TENTHS, 0x05);
    assert_eq!(cia.read(10, reg::ICR), icr::ALARM);
}

#[test]
fn the_serial_register_shifts_a_byte_out_in_sixteen_underflows() {
    let mut cia = with_timer_a(Model::Mos8521, 3);
    cia.write(5, reg::ICR, icr::SET | icr::SDR);
    cia.write(10, reg::CRA, cr::START | cr::SERIAL_OUT);
    cia.write(12, reg::SDR, 0xA5);
    // Timer A underflows from cycle 15 every four cycles; CNT toggles a
    // cycle after each, and the fifteenth toggle, in cycle 72, ends the
    // byte, flagged two cycles later.
    cia.advance(74);
    assert!(!cia.irq());
    cia.advance(75);
    assert!(cia.irq());
    assert_eq!(cia.read(75, reg::ICR), icr::SET | icr::SDR | icr::TA);
}

#[test]
fn the_serial_register_s_cnt_drives_a_timer_counting_cnt() {
    let mut cia = with_timer_a(Model::Mos6526, 3);
    cia.write(3, reg::TB_LO, 0xFF);
    cia.write(4, reg::TB_HI, 0x00);
    cia.write(5, reg::CRB, cr::START | cr::COUNT_CNT);
    cia.write(10, reg::CRA, cr::START | cr::SERIAL_OUT);
    cia.write(12, reg::SDR, 0xFF);
    // Eight rising edges of CNT for the byte.
    assert_eq!(cia.read(200, reg::TB_LO), 0xFF - 8);
}

/// A small deterministic generator for scripts of accesses.
struct Script(u64);

impl Script {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn byte(&mut self) -> u8 {
        self.next().to_le_bytes()[0]
    }
}

/// Writes a script draws from: timer latches and controls, the mask, the
/// clock and the serial register, so every pipeline and skip is crossed.
fn scripted_write(script: &mut Script) -> (u8, u8) {
    let register = [
        reg::TA_LO,
        reg::TA_HI,
        reg::TB_LO,
        reg::TB_HI,
        reg::CRA,
        reg::CRB,
        reg::ICR,
        reg::SDR,
        reg::TOD_TENTHS,
        reg::TOD_HOURS,
    ][usize::try_from(script.below(10)).expect("an index")];
    let mut value = script.byte();
    if matches!(register, reg::TA_HI | reg::TB_HI) {
        value &= 0x01;
    }
    (register, value)
}

#[test]
fn running_many_cycles_at_once_matches_running_them_one_at_a_time() {
    for model in [Model::Mos6526, Model::Mos8521] {
        let mut script = Script(0x9E37_79B9_7F4A_7C15);
        let mut jumped = chip(model);
        let mut stepped = chip(model);
        let mut now = 1;
        for _ in 0..4000 {
            let long = script.below(8) == 0;
            now += 1 + script.below(if long { 60_000 } else { 40 });
            jumped.advance(now);
            while stepped.now < now {
                stepped.advance(stepped.now + 1);
            }
            assert_eq!(jumped, stepped, "{model:?} at cycle {now}");
            assert_eq!(jumped.next_event(), stepped.next_event());
            if script.below(3) == 0 {
                let register = if script.below(2) == 0 {
                    reg::ICR
                } else {
                    reg::TA_LO
                };
                assert_eq!(jumped.read(now, register), stepped.read(now, register));
            } else {
                let (register, value) = scripted_write(&mut script);
                jumped.write(now, register, value);
                stepped.write(now, register, value);
            }
        }
    }
}

#[test]
fn the_line_never_rises_before_the_event_the_chip_names() {
    for model in [Model::Mos6526, Model::Mos8521] {
        let mut script = Script(0x2545_F491_4F6C_DD1D);
        let mut cia = chip(model);
        let mut now = 1;
        for _ in 0..3000 {
            let target = now + 1 + script.below(500);
            while now < target {
                let line = cia.irq();
                let due = cia.next_event().unwrap_or(u64::MAX);
                now += 1;
                cia.advance(now);
                if cia.irq() != line {
                    assert!(
                        due <= now,
                        "{model:?}: the line changed at {now}, the chip named {due}"
                    );
                }
            }
            if script.below(4) == 0 {
                let _ = cia.read(now, reg::ICR);
            } else {
                let (register, value) = scripted_write(&mut script);
                cia.write(now, register, value);
            }
        }
    }
}

#[test]
fn a_snapshot_restores_the_chip_exactly() {
    let mut cia = with_timer_a(Model::Mos8521, 0x40);
    cia.write(3, reg::ICR, icr::SET | icr::TA);
    cia.write(4, reg::CRA, cr::START | cr::SERIAL_OUT);
    cia.write(5, reg::SDR, 0x3C);
    cia.write(6, reg::TOD_TENTHS, 0);
    cia.advance(1000);
    let mut w = snapshot::Writer::new();
    w.put(&cia);
    let body = snapshot::open(&w.finish().expect("fits")).expect("opens");
    let mut r = snapshot::Reader::new(&body);
    r.set_clock(1000).expect("a clock in range");
    assert_eq!(r.get::<Cia>().expect("restores"), cia);
    let mut r = snapshot::Reader::new(&body);
    r.set_clock(999).expect("a clock in range");
    assert!(r.get::<Cia>().is_err(), "a chip at another time is refused");
}
