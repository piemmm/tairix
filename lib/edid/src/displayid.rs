//! A `DisplayID` section carried in an EDID extension: the detailed timings
//! its type I and type VII blocks state.

use crate::edid::{Edid, Mode, BLOCK_LEN};
use tairix_abi::driver::display::{Axis, Polarity, Scan, Timing};

/// The extension's tag.
pub(crate) const TAG: u8 = 0x70;

/// Where the section's data blocks start: after the extension tag, the
/// version, the section's length, its product type and its extension count.
const DATA_START: usize = 5;

const TYPE_I_TIMING: u8 = 0x03;
const TYPE_VII_TIMING: u8 = 0x22;

/// Bytes of one type I or type VII detailed timing.
const TIMING_LEN: usize = 20;

/// Read `block`, an extension carrying a `DisplayID` section, into `edid`. A
/// section whose own checksum is wrong, or that runs past the block, is
/// skipped.
pub(crate) fn read(block: &[u8; BLOCK_LEN], edid: &mut Edid) {
    let end = DATA_START + usize::from(block[2]);
    // The section's checksum byte follows it, and the block's own ends it.
    let Some(section) = block.get(1..=end).filter(|_| end < BLOCK_LEN - 1) else {
        return;
    };
    if section
        .iter()
        .fold(0u8, |sum, byte| sum.wrapping_add(*byte))
        != 0
    {
        return;
    }
    let mut at = DATA_START;
    while let Some(&[tag, _revision, len]) = block.get(at..at + 3).filter(|_| at + 3 <= end) {
        let Some(payload) = block
            .get(at + 3..at + 3 + usize::from(len))
            .filter(|payload| at + 3 + payload.len() <= end)
        else {
            return;
        };
        let unit_khz = match tag {
            TYPE_I_TIMING => Some(10),
            TYPE_VII_TIMING => Some(1),
            _ => None,
        };
        if let Some(unit_khz) = unit_khz {
            let (timings, _) = payload.as_chunks::<TIMING_LEN>();
            for (timing, preferred) in timings.iter().filter_map(|t| timing(t, unit_khz)) {
                edid.add(Mode {
                    timing,
                    code: None,
                    preferred,
                });
            }
        }
        at += 3 + payload.len();
    }
}

/// The timing a type I or type VII descriptor states, its clock counted in
/// `unit_khz`, and whether it is preferred; `None` for an impossible one.
fn timing(d: &[u8; TIMING_LEN], unit_khz: u32) -> Option<(Timing, bool)> {
    let clock = (u32::from(d[0]) | u32::from(d[1]) << 8 | u32::from(d[2]) << 16) + 1;
    let field = |at: usize, mask: u8| u32::from(d[at]) | u32::from(d[at + 1] & mask) << 8;
    // Each field is stored one less than it is.
    let narrow = |value: u32| u16::try_from(value + 1).ok();
    let h_active = narrow(field(4, 0xFF))?;
    let h_blank = narrow(field(6, 0xFF))?;
    let h_front = narrow(field(8, 0x7F))?;
    let h_sync = narrow(field(10, 0xFF))?;
    let v_active = narrow(field(12, 0xFF))?;
    let v_blank = narrow(field(14, 0xFF))?;
    let v_front = narrow(field(16, 0x7F))?;
    let v_sync = narrow(field(18, 0xFF))?;
    let polarity = |byte: u8| {
        if byte & 0x80 != 0 {
            Polarity::Positive
        } else {
            Polarity::Negative
        }
    };
    let timing = Timing {
        clock_khz: clock.checked_mul(unit_khz)?,
        horizontal: Axis {
            active: h_active,
            front: h_front,
            sync: h_sync,
            back: h_blank.checked_sub(h_front)?.checked_sub(h_sync)?,
        },
        vertical: Axis {
            active: v_active,
            front: v_front,
            sync: v_sync,
            back: v_blank.checked_sub(v_front)?.checked_sub(v_sync)?,
        },
        hsync: polarity(d[9]),
        vsync: polarity(d[17]),
        scan: if d[3] & 0x10 != 0 {
            Scan::Interlaced
        } else {
            Scan::Progressive
        },
        repetition: 1,
    };
    Some((timing, d[3] & 0x80 != 0))
}
