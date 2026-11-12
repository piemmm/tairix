use super::*;
use alloc::vec::Vec;
use tairix_fuzzseed::Prng;

/// Programs counter `n` with a control word and a two-byte count at `tick`.
fn program(pit: &mut Pit, n: u8, mode: u8, count: u16, tick: u64) {
    pit.write(3, (n << 6) | 0x30 | (mode << 1), tick);
    write_count(pit, n, count, tick);
}

fn write_count(pit: &mut Pit, n: u8, count: u16, tick: u64) {
    pit.write(n, crate::bits::lo(count), tick);
    pit.write(n, crate::bits::hi(count), tick);
}

fn latched(pit: &mut Pit, n: u8, tick: u64) -> u16 {
    pit.write(3, n << 6, tick);
    let low = pit.read(n, tick);
    crate::bits::word(low, pit.read(n, tick))
}

/// The 8253 as its data sheet describes it, one clock at a time: the oracle
/// the lazy model is held to.
struct Stepped {
    mode: u8,
    cr: u32,
    /// The counting element, from its first load.
    ce: Option<u32>,
    out: bool,
    load_at: Option<u64>,
    first_of_half: bool,
    strobed: bool,
}

impl Stepped {
    fn new(mode: u8) -> Self {
        Self {
            mode,
            cr: 0,
            ce: None,
            out: mode != 0,
            load_at: None,
            first_of_half: true,
            strobed: false,
        }
    }

    fn write(&mut self, count: u32, tick: u64) {
        self.cr = count;
        if self.mode == 0 {
            self.out = false;
        }
        if !(self.ce.is_some() && matches!(self.mode, 2 | 3)) {
            self.load_at = Some(tick + 1);
        }
    }

    /// The clock edge that begins `tick`.
    fn clock(&mut self, tick: u64) {
        if self.load_at == Some(tick) {
            self.load_at = None;
            self.ce = Some(self.cr);
            self.first_of_half = true;
            self.strobed = false;
            self.out = match self.mode {
                0 => false,
                2 => self.cr != 1,
                _ => true,
            };
            return;
        }
        let Some(ce) = self.ce else { return };
        let next = self.step(ce);
        self.ce = Some(next);
    }

    /// One decrement of the counting element `ce`, returning its new value.
    fn step(&mut self, ce: u32) -> u32 {
        match self.mode {
            0 => {
                let next = (ce + 0xFFFF) & 0xFFFF;
                if next == 0 {
                    self.out = true;
                }
                next
            }
            2 => {
                let next = if ce == 1 { self.cr } else { ce - 1 };
                self.out = next != 1;
                next
            }
            3 => {
                let step = if self.first_of_half && ce % 2 == 1 {
                    if self.out {
                        1
                    } else {
                        3
                    }
                } else {
                    2
                };
                self.first_of_half = false;
                if ce <= step {
                    self.out = !self.out;
                    self.first_of_half = true;
                    self.cr
                } else {
                    ce - step
                }
            }
            _ => {
                let next = (ce + 0xFFFF) & 0xFFFF;
                self.out = next != 0 || self.strobed;
                if next == 0 {
                    self.strobed = true;
                }
                next
            }
        }
    }

    fn count(&self) -> u16 {
        u16::try_from(self.ce.unwrap_or(0) & 0xFFFF).unwrap()
    }
}

#[test]
fn the_lazy_counter_matches_the_data_sheet_clock_by_clock() {
    let mut rng = Prng::new(0x8253);
    for round in 0..200 {
        let mode = [0u8, 2, 3, 4][round % 4];
        let mut pit = Pit::new();
        let mut oracle = Stepped::new(mode);
        // Counts of one are illegal in modes 2 and 3.
        let count = |rng: &mut Prng| 2 + u32::from(rng.next_u8() % 40);
        let first = count(&mut rng);
        pit.write(3, 0x30 | (mode << 1), 0);
        write_count(&mut pit, 0, u16::try_from(first).unwrap(), 0);
        oracle.write(first, 0);
        for tick in 1..600u64 {
            oracle.clock(tick);
            if rng.next_u8() < 6 {
                let next = count(&mut rng);
                write_count(&mut pit, 0, u16::try_from(next).unwrap(), tick);
                oracle.write(next, tick);
            }
            assert_eq!(pit.out(0, tick), oracle.out, "mode {mode} output at {tick}");
            assert_eq!(
                latched(&mut pit, 0, tick),
                oracle.count(),
                "mode {mode} count at {tick}"
            );
        }
    }
}

#[test]
fn edges_and_rise_counts_agree_with_the_output() {
    let mut rng = Prng::new(0x40);
    for round in 0..120 {
        let mode = [0u8, 2, 3, 4][round % 4];
        let mut pit = Pit::new();
        program(&mut pit, 0, mode, u16::from(rng.next_u8() % 30 + 1), 0);
        let reprogram = 50 + u64::from(rng.next_u8());
        write_count(&mut pit, 0, u16::from(rng.next_u8() % 30 + 2), reprogram);
        let outs: Vec<bool> = (0..800).map(|t| pit.out(0, t)).collect();
        let rose =
            |t: u64| outs[usize::try_from(t).unwrap()] && !outs[usize::try_from(t - 1).unwrap()];
        for a in (reprogram..700).step_by(7) {
            let expected = (a + 1..800).find(|&t| rose(t));
            assert_eq!(
                pit.next_rise(0, a),
                expected,
                "mode {mode} next rise after {a}"
            );
            let b = a + u64::from(rng.next_u8() % 200);
            if b < 800 {
                let rises = (a + 1..=b).filter(|&t| rose(t)).count();
                assert_eq!(
                    pit.rises_between(0, a, b),
                    u64::try_from(rises).unwrap(),
                    "mode {mode} rises in ({a}, {b}]"
                );
            }
        }
    }
}

#[test]
fn the_bios_timer_rises_every_65536_ticks() {
    let mut pit = Pit::new();
    program(&mut pit, 0, 3, 0, 100);
    assert!(pit.out(0, 101));
    assert!(!pit.out(0, 101 + 32_768));
    assert_eq!(pit.next_rise(0, 101), Some(101 + 65_536));
    assert_eq!(pit.rises_between(0, 0, 101 + 65_536 * 1000), 1000);
}

#[test]
fn refresh_rate_counts_rises_without_walking_them() {
    let mut pit = Pit::new();
    program(&mut pit, 1, 2, 18, 0);
    let day = 1_193_182 * 86_400;
    assert_eq!(pit.rises_between(1, 0, day), (day - 1) / 18);
}

#[test]
fn mode_three_takes_a_new_count_at_the_half_cycle() {
    let mut pit = Pit::new();
    program(&mut pit, 2, 3, 100, 0);
    // High from 1 to 50; the new count lands on the fall at 51.
    write_count(&mut pit, 2, 10, 20);
    assert!(pit.out(2, 50));
    assert!(
        !pit.out(2, 51),
        "the current half finishes at the old count"
    );
    assert!(!pit.out(2, 55));
    assert!(pit.out(2, 56), "then the new count's low half, five ticks");
    assert!(!pit.out(2, 61));
}

#[test]
fn the_gate_pauses_mode_zero_and_restarts_mode_two() {
    let mut pit = Pit::new();
    program(&mut pit, 2, 0, 100, 0);
    pit.set_gate(2, false, 31);
    assert_eq!(latched(&mut pit, 2, 500), 70);
    pit.set_gate(2, true, 500);
    assert!(!pit.out(2, 569));
    assert!(pit.out(2, 570), "seventy more ticks to terminal count");

    program(&mut pit, 1, 2, 10, 0);
    assert!(!pit.out(1, 10));
    pit.set_gate(1, false, 15);
    assert!(pit.out(1, 19), "gate low holds the output high");
    pit.set_gate(1, true, 40);
    assert!(
        !pit.out(1, 50),
        "a rising gate reloads: low pulse nine ticks after the load at 41"
    );
    assert!(pit.out(1, 49));
}

#[test]
fn bcd_counts_decimally() {
    let mut pit = Pit::new();
    pit.write(3, 0x31, 0);
    write_count(&mut pit, 0, 0x0100, 0);
    assert_eq!(latched(&mut pit, 0, 2), 0x0099);
    assert_eq!(latched(&mut pit, 0, 101), 0x0000);
    assert_eq!(
        latched(&mut pit, 0, 102),
        0x9999,
        "BCD wraps at ten thousand"
    );
}

#[test]
fn a_latch_holds_until_both_bytes_are_read() {
    let mut pit = Pit::new();
    program(&mut pit, 0, 2, 1000, 0);
    pit.write(3, 0x00, 11);
    assert_eq!(pit.read(0, 400), crate::bits::lo(990));
    pit.write(3, 0x00, 401);
    assert_eq!(
        pit.read(0, 500),
        crate::bits::hi(990),
        "a second latch command is ignored"
    );
    let low = pit.read(0, 600);
    assert_eq!(
        crate::bits::word(low, pit.read(0, 600)),
        401,
        "599 ticks into a period of 1000"
    );
}

#[test]
fn byte_access_modes_load_one_byte() {
    let mut pit = Pit::new();
    pit.write(3, 0x20 | (2 << 1), 0);
    pit.write(0, 0x02, 0);
    assert_eq!(pit.read(0, 1), 0x02, "MSB-only reads the high byte");
    pit.write(3, 0x10 | (2 << 1), 0);
    pit.write(0, 0x40, 0);
    assert_eq!(pit.read(0, 2), 0x3F);
}

#[test]
fn the_read_back_command_does_not_exist() {
    let mut pit = Pit::new();
    program(&mut pit, 0, 2, 50, 0);
    pit.write(3, 0xC2, 10);
    assert_eq!(latched(&mut pit, 0, 10), 41);
}

#[test]
fn mode_one_is_a_retriggerable_one_shot() {
    let mut pit = Pit::new();
    pit.set_gate(2, false, 0);
    program(&mut pit, 2, 1, 50, 0);
    assert!(pit.out(2, 100), "high until the gate triggers it");
    pit.set_gate(2, true, 100);
    assert!(pit.out(2, 100));
    assert!(!pit.out(2, 101), "low from the tick after the trigger");
    pit.set_gate(2, false, 120);
    pit.set_gate(2, true, 130);
    assert!(!pit.out(2, 180), "a second trigger restarts the count");
    assert!(pit.out(2, 181));
    write_count(&mut pit, 2, 10, 200);
    assert!(pit.out(2, 205), "a new count waits for the next trigger");
}

#[test]
fn mode_five_strobes_once_after_its_trigger() {
    let mut pit = Pit::new();
    pit.set_gate(2, false, 0);
    program(&mut pit, 2, 5, 20, 0);
    pit.set_gate(2, true, 10);
    let lows: Vec<u64> = (0..200).filter(|&t| !pit.out(2, t)).collect();
    assert_eq!(
        lows,
        [31],
        "one tick low at terminal count, twenty after the load"
    );
    assert_eq!(pit.rises_between(2, 0, 200), 1);
}
