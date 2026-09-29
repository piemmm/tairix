//! The card registers the driver negotiates from: the SD Configuration
//! Register (`ACMD51`) and the switch-function status (`CMD6`), laid out as
//! the SD Physical Layer Simplified Specification's "SCR register" and
//! "Switch Function Status" tables give them, most significant bit first.

/// Bytes of the SCR.
pub const SCR_BYTES: usize = 8;

/// Bytes of a switch-function status block.
pub const SWITCH_STATUS_BYTES: usize = 64;

/// What the SCR says about the card.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Scr {
    /// The card answers `CMD6` (SD 1.10 and later).
    pub switch_function: bool,
    /// The card drives four data lines.
    pub four_bit: bool,
    /// The card takes `CMD23` ahead of a multi-block transfer.
    pub set_block_count: bool,
}

/// Decode the SCR, or `None` for a structure version other than 1.0, whose
/// fields cannot be trusted to mean what 1.0's do.
#[must_use]
pub fn decode_scr(bytes: &[u8; SCR_BYTES]) -> Option<Scr> {
    let scr = u64::from_be_bytes(*bytes);
    if (scr >> 60) & 0xF != 0 {
        return None;
    }
    let sd_spec = (scr >> 56) & 0xF;
    let sd_spec3 = (scr >> 47) & 1 != 0;
    Some(Scr {
        switch_function: sd_spec >= 1,
        four_bit: (scr >> 50) & 1 != 0,
        // CMD_SUPPORT is defined from SD 3.00; an older card's bits are
        // reserved, so only an SD_SPEC3 card's are read.
        set_block_count: sd_spec3 && (scr >> 33) & 1 != 0,
    })
}

/// The groups of a switch-function status the driver reads.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SwitchStatus {
    /// Access modes (function group 1) the card supports, bit `n` for
    /// function `n`.
    pub access_modes: u16,
    /// Current limits (function group 4) the card supports.
    pub current_limits: u16,
    /// The access mode a switch selected (or would select); `0xF` when it
    /// could not.
    pub access_mode: u8,
    /// The current limit a switch selected; `0xF` when it could not.
    pub current_limit: u8,
}

/// Decode a switch-function status block.
#[must_use]
pub fn decode_switch_status(bytes: &[u8; SWITCH_STATUS_BYTES]) -> SwitchStatus {
    SwitchStatus {
        access_modes: u16::from_be_bytes([bytes[12], bytes[13]]),
        current_limits: u16::from_be_bytes([bytes[6], bytes[7]]),
        access_mode: bytes[16] & 0xF,
        current_limit: bytes[15] >> 4,
    }
}

/// A `CMD6` argument: query (`set == false`) or switch to `access_mode` in
/// group 1 and `current_limit` in group 4, leaving every other group as it is
/// (`0xF` in a group changes nothing).
#[must_use]
pub const fn switch_argument(set: bool, access_mode: u8, current_limit: u8) -> u32 {
    let mode = if set { 1 << 31 } else { 0 };
    mode | 0x00FF_0FF0 | ((current_limit as u32 & 0xF) << 12) | (access_mode as u32 & 0xF)
}

/// The value a [`switch_argument`] group takes to leave the group as it is.
pub const KEEP: u8 = 0xF;

#[cfg(test)]
mod tests {
    use super::*;

    /// An SD 3.0x card's SCR: 4-bit and 1-bit buses, `SD_SPEC3`, `CMD23`.
    const SCR_SD3_CMD23: [u8; 8] = [0x02, 0x35, 0x80, 0x02, 0, 0, 0, 0];

    #[test]
    fn an_sd3_scr_offers_cmd6_the_4bit_bus_and_cmd23() {
        assert_eq!(
            decode_scr(&SCR_SD3_CMD23),
            Some(Scr {
                switch_function: true,
                four_bit: true,
                set_block_count: true
            })
        );
    }

    #[test]
    fn cmd23_support_is_read_only_from_an_sd3_card() {
        let mut scr = SCR_SD3_CMD23;
        scr[2] &= !0x80;
        assert!(!decode_scr(&scr).expect("v1.0").set_block_count);
    }

    #[test]
    fn an_sd_1_0_card_answers_no_cmd6() {
        let scr = [0x00, 0x05, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            decode_scr(&scr),
            Some(Scr {
                switch_function: false,
                four_bit: true,
                set_block_count: false
            })
        );
    }

    #[test]
    fn an_unknown_scr_structure_is_refused() {
        let mut scr = SCR_SD3_CMD23;
        scr[0] |= 0x10;
        assert_eq!(decode_scr(&scr), None);
    }

    #[test]
    fn the_switch_status_groups_are_read_at_their_bit_positions() {
        let mut status = [0u8; SWITCH_STATUS_BYTES];
        status[7] = 0x0F; // group 4: 200, 400, 600, 800 mA
        status[13] = 0x17; // group 1: SDR12, SDR25, SDR50, DDR50
        status[15] = 0x10; // group 4 result: 400 mA
        status[16] = 0x04; // group 1 result: DDR50
        assert_eq!(
            decode_switch_status(&status),
            SwitchStatus {
                access_modes: 0x17,
                current_limits: 0x0F,
                access_mode: 4,
                current_limit: 1
            }
        );
    }

    #[test]
    fn a_switch_argument_changes_only_the_groups_it_names() {
        assert_eq!(switch_argument(false, KEEP, KEEP), 0x00FF_FFFF);
        assert_eq!(switch_argument(true, 1, KEEP), 0x80FF_FFF1);
        assert_eq!(switch_argument(true, KEEP, 2), 0x80FF_2FFF);
    }
}
