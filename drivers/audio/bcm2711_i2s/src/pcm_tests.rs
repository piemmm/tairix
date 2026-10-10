//! The block against the model of its control register. The framings are
//! checked against the register values Linux's `bcm2835-i2s` computes for the
//! same link.

use tairix_abi::driver::codec::{ClockInversion, DaiFormat, DaiLink};
use tairix_abi::{DriverError, RegisterBlock};

use super::model::Block;
use super::{
    Framing, Pcm, CS, CS_DMAEN, CS_EN, CS_STBY, CS_SYNC, CS_TXCLR, CS_TXERR, CS_TXON, CS_TXTHR,
    DREQ, INTEN, INTSTC, MODE, RXC, TXC, WINDOW_LEN,
};

/// A link in `format`, the codec driving the bit clock when `bit` and the
/// frame clock when `frame`.
fn link(format: DaiFormat, bit: bool, frame: bool, inversion: ClockInversion) -> DaiLink {
    DaiLink {
        format,
        codec_drives_bit_clock: bit,
        codec_drives_frame_clock: frame,
        inversion,
        cpu_dai: 0,
        codec_dai: 0,
    }
}

/// Check `link` at `width` frames as Linux programs it: `mode` in `MODE_A`
/// and `channels` in `TXC_A`.
fn check(link: &DaiLink, width: u8, mode: u32, channels: u32) {
    let framing = Framing::new(link, width).expect("a framing");
    assert_eq!(
        framing.mode, mode,
        "{link:?} {width}: mode {:#x}",
        framing.mode
    );
    assert_eq!(
        framing.channels, channels,
        "{link:?} {width}: channels {:#x}",
        framing.channels
    );
    assert_eq!(framing.frame_bits(), 2 * u32::from(width));
}

#[test]
fn each_framing_width_and_clock_side_is_the_one_linux_programs() {
    use ClockInversion::Normal;
    // The flags say whether the codec drives the bit clock, then the frame
    // clock.
    check(
        &link(DaiFormat::I2s, false, false, Normal),
        32,
        0x0050_FC20,
        0xC018_C218,
    );
    check(
        &link(DaiFormat::I2s, false, false, Normal),
        16,
        0x0150_7C10,
        0x4018_4118,
    );
    check(
        &link(DaiFormat::LeftJustified, true, true, Normal),
        24,
        0x00E0_BC18,
        0xC000_C180,
    );
    check(
        &link(DaiFormat::RightJustified, false, true, Normal),
        20,
        0x0060_9C14,
        0x400C_414C,
    );
    check(
        &link(DaiFormat::DspA, true, false, Normal),
        32,
        0x00C0_FC01,
        0xC018_C218,
    );
    check(
        &link(DaiFormat::DspB, false, false, Normal),
        32,
        0x0040_FC01,
        0xC008_C208,
    );
}

#[test]
fn each_inversion_is_the_one_linux_programs() {
    use ClockInversion::{BitClock, Both, FrameClock};
    check(
        &link(DaiFormat::I2s, false, false, BitClock),
        32,
        0x0010_FC20,
        0xC018_C218,
    );
    check(
        &link(DaiFormat::I2s, false, false, FrameClock),
        32,
        0x0040_FC20,
        0xC018_C218,
    );
    check(
        &link(DaiFormat::LeftJustified, false, false, FrameClock),
        32,
        0x0050_FC20,
        0xC008_C208,
    );
    check(
        &link(DaiFormat::LeftJustified, false, false, Both),
        32,
        0x0010_FC20,
        0xC008_C208,
    );
}

#[test]
fn a_width_codec_v1_does_not_name_is_refused() {
    for width in [8, 18, 33] {
        assert_eq!(
            Framing::new(
                &link(DaiFormat::I2s, false, false, ClockInversion::Normal),
                width
            ),
            Err(DriverError::Unsupported)
        );
    }
}

#[test]
fn enabling_masks_the_interrupts_and_adopts_the_sync_bit_as_it_reads() {
    let block = Block::new(true, CS_SYNC);
    let mut pcm = Pcm::new(&block).expect("a window");
    pcm.enable().expect("enabled");
    assert_eq!(block.last(INTEN), Some(0));
    assert_eq!(block.last(INTSTC), Some(0xF));
    assert_eq!(block.last(CS), Some(CS_SYNC | CS_EN | CS_STBY), "no toggle");
}

#[test]
fn framing_writes_the_control_registers_with_transmit_off() {
    let block = Block::new(true, 0);
    let mut pcm = Pcm::new(&block).expect("a window");
    pcm.enable().expect("enabled");
    pcm.transmit(true).expect("on");
    block.clear_writes();
    let framing = Framing::new(
        &link(DaiFormat::I2s, false, false, ClockInversion::Normal),
        32,
    )
    .expect("a framing");
    pcm.frame(&framing).expect("framed");
    let writes = block.writes();
    assert_eq!(writes[0], (CS, CS_EN | CS_STBY), "transmit off first");
    assert_eq!(
        writes[1..5],
        [
            (MODE, framing.mode),
            (RXC, 0),
            (TXC, framing.channels),
            (DREQ, 0x1000_3000),
        ]
    );
    assert_eq!(
        writes[5],
        (CS, CS_EN | CS_STBY | CS_TXTHR | CS_DMAEN),
        "DMA requests on last"
    );
}

#[test]
fn a_clear_turns_transmit_off_and_returns_once_two_bit_clocks_have_passed() {
    let block = Block::new(true, 0);
    let mut pcm = Pcm::new(&block).expect("a window");
    pcm.enable().expect("enabled");
    pcm.transmit(true).expect("on");
    pcm.clear().expect("cleared");
    assert_eq!(
        block.cleared.get(),
        1,
        "the clear took effect before it returned"
    );
    let clear = block.last(CS).expect("written");
    assert_eq!(clear & (CS_TXCLR | CS_TXON), CS_TXCLR);
    assert_eq!(clear & CS_SYNC, CS_SYNC, "the sync bit toggled");
    // A second clear toggles it back.
    pcm.clear().expect("cleared");
    assert_eq!(block.last(CS).map(|cs| cs & CS_SYNC), Some(0));
    assert_eq!(block.cleared.get(), 2);
}

#[test]
fn a_clear_with_no_bit_clock_to_complete_it_fails_within_its_budget() {
    let block = Block::new(false, 0);
    let mut pcm = Pcm::new(&block).expect("a window");
    assert_eq!(pcm.clear(), Err(DriverError::DeviceFault));
    assert_eq!(block.cleared.get(), 0);
}

#[test]
fn transmitting_clears_the_latched_error_and_stopping_leaves_the_rest() {
    let block = Block::new(true, 0);
    let mut pcm = Pcm::new(&block).expect("a window");
    pcm.enable().expect("enabled");
    pcm.transmit(true).expect("on");
    assert_eq!(block.last(CS), Some(CS_EN | CS_STBY | CS_TXON | CS_TXERR));
    pcm.transmit(false).expect("off");
    assert_eq!(block.last(CS), Some(CS_EN | CS_STBY));
}

#[test]
fn a_window_short_of_the_registers_is_refused() {
    struct Short;
    impl RegisterBlock for Short {
        fn read32(&self, _offset: usize) -> Result<u32, DriverError> {
            Ok(0)
        }
        fn write32(&self, _offset: usize, _value: u32) -> Result<(), DriverError> {
            Ok(())
        }
        fn block_len(&self) -> usize {
            WINDOW_LEN - 4
        }
    }
    assert!(matches!(
        Pcm::new(&Short),
        Err(DriverError::LengthOutOfRange)
    ));
}
