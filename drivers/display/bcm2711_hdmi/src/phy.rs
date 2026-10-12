//! The HDMI transmitter's PHY: its PLL set for a TMDS rate and its lanes'
//! drive strengths, as Linux's `vc5_hdmi_phy_init` programs the BCM2711's.

use tairix_abi::driver::mmio::RegisterBlock;
use tairix_abi::DriverError;

use crate::regs::{field, phy, rm};

/// The reference oscillator the PHY's PLL multiplies.
const OSCILLATOR_HZ: u64 = 54_000_000;

/// The band the PLL's oscillator runs in.
const VCO_MIN_HZ: u64 = 3_000_000_000;
const VCO_HIGH_HZ: u64 = 4_500_000_000;

/// How the PHY's lanes are wired to the connector's: the clock lane and the
/// three data lanes, each by the PHY lane it sends.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LaneMap {
    /// The PHY lane sending each of data lanes 0, 1 and 2, then the clock.
    pub lanes: [u8; 4],
}

/// The PHY lane carrying the clock in every wiring.
const LANE_CLOCK: u8 = 3;

/// One lane's drive: pre-emphasis, main driver, its resistor and its
/// termination.
#[derive(Copy, Clone)]
struct Drive {
    preemphasis: u32,
    main_driver: u32,
    resistor: u32,
    termination: u32,
}

const fn drive(preemphasis: u32, main_driver: u32, resistor: u32, termination: u32) -> Drive {
    Drive {
        preemphasis,
        main_driver,
        resistor,
        termination,
    }
}

/// The data lanes' and the clock lane's drive for TMDS rates up to the bound,
/// Linux's `vc5_hdmi_phy_settings`.
const DRIVES: [(u64, Drive, Drive); 7] = [
    (50_000_000, drive(0, 0x0A, 0x12, 0), drive(0, 0x0A, 0x18, 0)),
    (75_000_000, drive(0, 0x09, 0x12, 0), drive(0, 0x0C, 0x18, 3)),
    (
        165_000_000,
        drive(0, 0x09, 0x12, 0),
        drive(0, 0x0C, 0x18, 3),
    ),
    (
        250_000_000,
        drive(0, 0x0F, 0x12, 1),
        drive(0, 0x0C, 0x18, 3),
    ),
    (
        340_000_000,
        drive(2, 0x0D, 0x12, 1),
        drive(0, 0x0C, 0x18, 0xF),
    ),
    (
        450_000_000,
        drive(0, 0x1B, 0x12, 0xF),
        drive(0, 0x0A, 0x12, 0xF),
    ),
    (
        600_000_000,
        drive(0, 0x1C, 0x12, 0xF),
        drive(0, 0x0B, 0x13, 0xF),
    ),
];

/// The drives for `tmds_hz`: the band it falls in, else the fastest.
fn drives(tmds_hz: u64) -> (Drive, Drive) {
    let band = DRIVES
        .iter()
        .find(|(top, _, _)| tmds_hz <= *top)
        .unwrap_or(&DRIVES[DRIVES.len() - 1]);
    (band.1, band.2)
}

/// The PLL's oscillator frequency for `tmds_hz` — ten bits a character, times
/// the smallest divider bringing it into band — and that divider; [`None`]
/// for a rate the divider's eight bits cannot bring there.
fn vco(tmds_hz: u64) -> Option<(u64, u32)> {
    let bit_hz = tmds_hz.checked_mul(10).filter(|&hz| hz != 0)?;
    let divider = u8::try_from(VCO_MIN_HZ.div_ceil(bit_hz).max(1)).ok()?;
    Some((bit_hz * u64::from(divider), u32::from(divider)))
}

/// The charge pump current for an oscillator at `vco_hz`.
const fn charge_pump(vco_hz: u64) -> u32 {
    if vco_hz < 3_700_000_000 {
        0x1C
    } else {
        0x18
    }
}

/// The oscillator's gain at `vco_hz`.
const fn vco_gain(vco_hz: u64) -> u32 {
    match vco_hz {
        0..3_350_000_000 => 0xF,
        3_350_000_000..3_700_000_000 => 0xC,
        3_700_000_000..4_050_000_000 => 0x6,
        4_050_000_000..4_800_000_000 => 0x5,
        4_800_000_000..5_200_000_000 => 0x7,
        _ => 0x2,
    }
}

/// The rate manager's offset for `vco_hz`: twice it over the reference, in
/// 9.22 fixed point.
fn rm_offset(vco_hz: u64) -> u32 {
    let offset = (((u128::from(vco_hz) * 2) << 22) / u128::from(OSCILLATOR_HZ)) >> 2;
    u32::try_from(offset).unwrap_or(u32::MAX)
}

fn modify(
    regs: &dyn RegisterBlock,
    offset: usize,
    clear: u32,
    set: u32,
) -> Result<(), DriverError> {
    let value = regs.read32(offset)?;
    regs.write32(offset, value & !clear | set)
}

/// Hold the PHY in reset, powered down: the PLL's resets are active low, so
/// setting the lanes' alone holds every part.
pub(crate) fn reset(phy_regs: &dyn RegisterBlock) -> Result<(), DriverError> {
    phy_regs.write32(phy::RESET_CTL, phy::RESET_LANES)?;
    phy_regs.write32(phy::POWERDOWN_CTL, phy::POWERDOWN_ALL)
}

/// Bring the PHY up sending TMDS at `tmds_hz` over the lanes `map` wires, its
/// clock at a quarter of the character rate for a scrambled link.
///
/// # Errors
///
/// [`DriverError::OutOfRange`] for a rate the PLL cannot make, or the window's
/// refusal.
pub(crate) fn init(
    phy_regs: &dyn RegisterBlock,
    rm_regs: &dyn RegisterBlock,
    tmds_hz: u64,
    scrambled: bool,
    map: LaneMap,
) -> Result<(), DriverError> {
    let (vco_hz, divider) = vco(tmds_hz).ok_or(DriverError::OutOfRange)?;
    reset(phy_regs)?;
    phy_regs.write32(phy::POWERDOWN_CTL, phy::POWERDOWN_RNDGEN)?;
    modify(phy_regs, phy::RESET_CTL, phy::RESET_LANES, 0)?;
    modify(
        rm_regs,
        rm::CONTROL,
        0,
        rm::CONTROL_FREEZE_COUNTERS | rm::CONTROL_LOAD_INTEGRATOR | rm::CONTROL_FREE_RUN,
    )?;
    modify(phy_regs, phy::PLL_CALIBRATION_1, phy::CALIBRATION_LIMIT, 0)?;
    modify(phy_regs, phy::PLL_CALIBRATION_2, phy::CALIBRATION_LIMIT, 0)?;
    rm_regs.write32(
        rm::OFFSET,
        field(rm_offset(vco_hz), 0, 31) | rm::OFFSET_ONLY,
    )?;
    phy_regs.write32(phy::CLK_DIV, field(divider, 8, 8))?;
    phy_regs.write32(
        phy::PLL_CALIBRATION_4,
        field(0xE14, 16, 16) | field(0xE147, 0, 16),
    )?;
    phy_regs.write32(
        phy::PLL_CTL_0,
        phy::PLL_ENA_VCO_CLK
            | phy::PLL_VCO_CONT_EN
            | phy::PLL_MASH11_MODE
            | field(u32::from(vco_hz > VCO_HIGH_HZ), 9, 1),
    )?;
    modify(
        phy_regs,
        phy::PLL_CTL_1,
        0,
        phy::PLL_FREQ_DOUBLER_ENABLE | field(3, 11, 2) | field(1, 14, 2) | field(0x8A, 16, 12),
    )?;
    modify(rm_regs, rm::FORMAT, 0, field(2, 24, 2))?;
    modify(phy_regs, phy::PLL_CFG, 0, field(1, 0, 4))?;
    phy_regs.write32(phy::TMDS_CLK_WORD_SEL, if scrambled { 3 } else { 0 })?;
    phy_regs.write32(
        phy::CTL_3,
        field(charge_pump(vco_hz), 0, 6)
            | field(1, 8, 2)
            | field(1, 10, 2)
            | field(3, 6, 2)
            | field(4, 17, 3)
            | field(6, 12, 5),
    )?;
    let (data, clock) = drives(tmds_hz);
    let lane = |at: usize| {
        if map.lanes[at] == LANE_CLOCK {
            clock
        } else {
            data
        }
    };
    let [lane0, lane1, lane2, lane_ck] = [lane(0), lane(1), lane(2), lane(3)];
    phy_regs.write32(
        phy::CTL_0,
        field(lane0.preemphasis, 13, 3)
            | field(lane0.main_driver, 8, 5)
            | field(lane1.preemphasis, 21, 3)
            | field(lane1.main_driver, 16, 5)
            | field(lane2.preemphasis, 29, 3)
            | field(lane2.main_driver, 24, 5)
            | field(lane_ck.preemphasis, 5, 3)
            | field(lane_ck.main_driver, 0, 5),
    )?;
    modify(
        phy_regs,
        phy::CTL_1,
        0,
        field(lane0.resistor, 5, 5)
            | field(lane1.resistor, 10, 5)
            | field(lane2.resistor, 15, 5)
            | field(lane_ck.resistor, 0, 5),
    )?;
    phy_regs.write32(
        phy::CTL_2,
        field(lane0.termination, 4, 4)
            | field(lane1.termination, 8, 4)
            | field(lane2.termination, 12, 4)
            | field(lane_ck.termination, 0, 4)
            | field(vco_gain(vco_hz), 16, 4),
    )?;
    let [l0, l1, l2, lck] = map.lanes.map(u32::from);
    phy_regs.write32(
        phy::CHANNEL_SWAP,
        field(l0, 0, 2) | field(l1, 4, 2) | field(l2, 8, 2) | field(lck, 12, 2),
    )?;
    modify(phy_regs, phy::RESET_CTL, phy::RESET_PLL, 0)?;
    modify(phy_regs, phy::RESET_CTL, 0, phy::RESET_PLL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_oscillator_runs_in_band_for_every_rate() {
        assert_eq!(vco(148_500_000), Some((4_455_000_000, 3)), "1080p");
        assert_eq!(vco(594_000_000), Some((5_940_000_000, 1)), "4K at 60 Hz");
        assert_eq!(vco(25_175_000), Some((3_021_000_000, 12)), "640x480");
        assert_eq!(
            vco(150_000_000),
            Some((3_000_000_000, 2)),
            "the band's edge"
        );
        for tmds_hz in [25_000_000, 74_250_000, 297_000_000, 600_000_000] {
            assert!(vco(tmds_hz).is_some_and(|(vco_hz, _)| vco_hz >= VCO_MIN_HZ));
        }
    }

    #[test]
    fn a_rate_the_divider_cannot_bring_into_band_is_refused() {
        assert_eq!(vco(0), None);
        assert_eq!(vco(1_000_000), None, "three hundred tens");
        assert_eq!(vco(u64::MAX), None);
    }

    #[test]
    fn the_drive_follows_the_rate_band() {
        let (data, clock) = drives(148_500_000);
        assert_eq!((data.main_driver, clock.termination), (0x09, 3));
        let (data, _) = drives(594_000_000);
        assert_eq!(data.main_driver, 0x1C);
        let (data, _) = drives(900_000_000);
        assert_eq!(data.main_driver, 0x1C, "past the table, its fastest band");
    }

    #[test]
    fn the_rate_manager_offset_is_twice_the_oscillator_over_the_reference() {
        // 4.455 GHz * 2 / 54 MHz = 165, in 9.22 fixed point less two bits.
        assert_eq!(rm_offset(4_455_000_000), 165 << 20);
    }
}
