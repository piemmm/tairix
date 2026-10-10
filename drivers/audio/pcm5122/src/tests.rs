//! The driver against a model of the part's register file: page 0, a reset
//! that restores the defaults, single-register transfers, and an analogue
//! mute that takes a few reads to settle.

use std::cell::{Cell, RefCell};
use std::vec::Vec;

use tairix_abi::driver::audio::Rate;
use tairix_abi::driver::codec::{ClockInversion, Codec, DaiFormat, DaiLink};
use tairix_abi::driver::i2c::I2cPort;
use tairix_abi::{Delay, DriverError};

use super::{
    Pcm5122, ANALOG_MUTE_DET, BCKP, BCLK_LRCLK_CFG, DCAS, ERROR_DETECT, I2S_1, I2S_2, IDCH, MUTE,
    PLL_REF, POWER, RESET, RQML, RQMR, RQST, RSTR, SREF, SREF_BCK, VOLUME_LEFT, VOLUME_RIGHT,
};

/// Register defaults the reset restores, as the datasheet gives them for the
/// ones the driver touches.
fn defaults() -> [u8; 128] {
    let mut registers = [0u8; 128];
    registers[usize::from(I2S_1)] = 0x02;
    registers[usize::from(VOLUME_LEFT)] = 0x30;
    registers[usize::from(VOLUME_RIGHT)] = 0x30;
    registers[usize::from(BCLK_LRCLK_CFG)] = 0x31;
    registers[usize::from(ERROR_DETECT)] = DCAS;
    registers
}

struct Part {
    registers: RefCell<[u8; 128]>,
    writes: RefCell<Vec<(u8, u8)>>,
    /// Reads of the analogue mute left before a requested mute settles.
    settling: Cell<u32>,
    settle_reads: u32,
}

impl Part {
    fn new(settle_reads: u32) -> Self {
        Self {
            registers: RefCell::new(defaults()),
            writes: RefCell::new(Vec::new()),
            settling: Cell::new(0),
            settle_reads,
        }
    }

    fn get(&self, register: u8) -> u8 {
        self.registers.borrow()[usize::from(register)]
    }
}

impl I2cPort for Part {
    fn transfer(&self, write: &[u8], read: &mut [u8]) -> Result<(), DriverError> {
        let (&register, values) = write.split_first().ok_or(DriverError::OutOfRange)?;
        assert!(register < 0x80, "page 0 holds registers below 128");
        assert!(values.len() <= 1, "one register a transfer");
        if let Some(&value) = values.first() {
            self.writes.borrow_mut().push((register, value));
            match register {
                0 => assert_eq!(value, 0, "page 0"),
                RESET if value & RSTR != 0 => *self.registers.borrow_mut() = defaults(),
                MUTE if value & (RQML | RQMR) != 0 => self.settling.set(self.settle_reads),
                _ => {}
            }
            if register != RESET {
                self.registers.borrow_mut()[usize::from(register)] = value;
            }
        }
        if let Some(out) = read.first_mut() {
            *out = if register == ANALOG_MUTE_DET {
                let left = self.settling.get();
                self.settling.set(left.saturating_sub(1));
                let live = self.get(POWER) & RQST == 0 && self.get(MUTE) == 0;
                if live || left > 0 {
                    0b11
                } else {
                    0
                }
            } else {
                self.get(register)
            };
        }
        Ok(())
    }
}

#[derive(Default)]
struct Pauses(Cell<u32>);

impl Delay for &Pauses {
    fn delay_us(&self, _us: u32) {
        self.0.set(self.0.get() + 1);
    }
    fn now_us(&self) -> u64 {
        u64::from(self.0.get())
    }
}

fn link(format: DaiFormat, codec_clocks: bool) -> DaiLink {
    DaiLink {
        format,
        codec_drives_bit_clock: codec_clocks,
        codec_drives_frame_clock: codec_clocks,
        inversion: ClockInversion::Normal,
        cpu_dai: 0,
        codec_dai: 0,
    }
}

fn rate(hz: u32) -> Rate {
    Rate::new(hz).expect("a rate")
}

#[test]
fn bring_up_resets_the_part_to_follow_the_bit_clock_muted_in_standby() {
    let part = Part::new(0);
    let pauses = Pauses::default();
    Pcm5122::new(&part, &pauses).bring_up().expect("brought up");
    assert_eq!(
        part.writes.borrow()[..3],
        [(0, 0), (RESET, 0x11), (RESET, 0)]
    );
    assert_ne!(part.get(POWER) & RQST, 0, "standby");
    assert_eq!(part.get(BCLK_LRCLK_CFG), 0, "both clocks are inputs");
    assert_eq!(part.get(PLL_REF) & SREF, SREF_BCK);
    assert_ne!(
        part.get(ERROR_DETECT) & IDCH,
        0,
        "no system clock is no error"
    );
    assert_eq!(part.get(MUTE), RQML | RQMR);
}

#[test]
fn each_framing_and_width_is_programmed_as_the_part_names_it() {
    let part = Part::new(0);
    let pauses = Pauses::default();
    let mut codec = Pcm5122::new(&part, &pauses);
    codec.bring_up().expect("brought up");
    for (format, width, i2s_1, offset) in [
        (DaiFormat::I2s, 16, 0x00, 0),
        (DaiFormat::I2s, 32, 0x03, 0),
        (DaiFormat::LeftJustified, 24, 0x32, 0),
        (DaiFormat::RightJustified, 20, 0x21, 0),
        (DaiFormat::DspA, 32, 0x13, 1),
        (DaiFormat::DspB, 16, 0x10, 0),
    ] {
        codec
            .configure(&link(format, false), rate(48_000), width)
            .expect("configured");
        assert_eq!(part.get(I2S_1), i2s_1, "{format:?} {width}");
        assert_eq!(part.get(I2S_2), offset);
        assert_eq!(
            part.get(ERROR_DETECT) & DCAS,
            0,
            "dividers follow the rates"
        );
    }
}

#[test]
fn a_rate_it_cannot_derive_a_width_no_slot_carries_or_a_clock_it_would_drive_is_refused() {
    let part = Part::new(0);
    let pauses = Pauses::default();
    let mut codec = Pcm5122::new(&part, &pauses);
    for (link, rate, width) in [
        (link(DaiFormat::I2s, false), rate(47_000), 32),
        (link(DaiFormat::I2s, false), rate(48_000), 18),
        (link(DaiFormat::I2s, true), rate(48_000), 32),
        (
            DaiLink {
                inversion: ClockInversion::FrameClock,
                ..link(DaiFormat::I2s, false)
            },
            rate(48_000),
            32,
        ),
    ] {
        assert_eq!(
            codec.configure(&link, rate, width),
            Err(DriverError::Unsupported)
        );
    }
    assert!(part.writes.borrow().is_empty(), "nothing reached the part");
}

#[test]
fn an_inverted_bit_clock_is_sampled_on_its_falling_edge_and_a_normal_one_restored() {
    let part = Part::new(0);
    let pauses = Pauses::default();
    let mut codec = Pcm5122::new(&part, &pauses);
    codec.bring_up().expect("brought up");
    let inverted = DaiLink {
        inversion: ClockInversion::BitClock,
        ..link(DaiFormat::I2s, false)
    };
    codec
        .configure(&inverted, rate(48_000), 32)
        .expect("configured");
    assert_eq!(
        part.get(BCLK_LRCLK_CFG),
        BCKP,
        "inverted, both clocks inputs"
    );
    codec
        .configure(&link(DaiFormat::I2s, false), rate(48_000), 32)
        .expect("configured");
    assert_eq!(part.get(BCLK_LRCLK_CFG), 0);
}

#[test]
fn the_gain_is_the_step_at_or_above_the_one_asked_within_the_range() {
    let part = Part::new(0);
    let pauses = Pauses::default();
    let mut codec = Pcm5122::new(&part, &pauses);
    for (asked, set, register) in [
        (0, 0, 0x30),
        (-625, -600, 60),
        (-650, -650, 61),
        (3_000, 2_400, 0),
        (-20_000, -10_300, 254),
    ] {
        assert_eq!(codec.set_gain(asked, false), Ok(set), "{asked}");
        assert_eq!(part.get(VOLUME_LEFT), register);
        assert_eq!(part.get(VOLUME_RIGHT), register);
    }
}

#[test]
fn the_output_sounds_only_while_started_and_unmuted() {
    let part = Part::new(3);
    let pauses = Pauses::default();
    let mut codec = Pcm5122::new(&part, &pauses);
    codec.bring_up().expect("brought up");
    codec.set_gain(0, false).expect("gain");
    assert_eq!(part.get(MUTE), RQML | RQMR, "not started");
    codec.start().expect("started");
    assert_eq!(part.get(MUTE), 0);
    assert_eq!(part.get(POWER) & RQST, 0, "out of standby");
    codec.set_gain(0, true).expect("muted");
    assert_eq!(part.get(MUTE), RQML | RQMR);
    codec.set_gain(0, false).expect("unmuted");
    assert_eq!(part.get(MUTE), 0);
}

#[test]
fn stopping_lets_the_soft_mute_settle_before_standby() {
    let part = Part::new(3);
    let pauses = Pauses::default();
    let mut codec = Pcm5122::new(&part, &pauses);
    codec.bring_up().expect("brought up");
    codec.start().expect("started");
    part.writes.borrow_mut().clear();
    codec.stop().expect("stopped");
    let writes = part.writes.borrow();
    assert_eq!(writes.first(), Some(&(MUTE, RQML | RQMR)), "muted first");
    assert_eq!(writes.last().map(|&(register, _)| register), Some(POWER));
    assert_eq!(
        pauses.0.get(),
        3,
        "waited out the ramp, parked between reads"
    );
    assert_ne!(part.get(POWER) & RQST, 0);
}

#[test]
fn a_mute_that_never_settles_is_waited_for_no_longer_than_its_budget() {
    let part = Part::new(u32::MAX);
    let pauses = Pauses::default();
    let mut codec = Pcm5122::new(&part, &pauses);
    codec.start().expect("started");
    codec.stop().expect("stopped regardless");
    assert_eq!(pauses.0.get(), super::MUTE_SETTLE_READS);
    assert_ne!(part.get(POWER) & RQST, 0, "still put in standby");
}
