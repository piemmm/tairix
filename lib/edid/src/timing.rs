//! The detailed descriptor that states a video timing in full, the tables'
//! spelling of one, and the picture aspect a CTA-861 video code carries.

use tairix_abi::driver::display::{Axis, Polarity, Scan, Timing};

/// A picture aspect ratio a CTA-861 video code carries.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Aspect {
    /// 4:3.
    FourThree,
    /// 16:9.
    SixteenNine,
    /// 64:27.
    SixtyFourTwentySeven,
    /// 256:135.
    TwoFiftySixOneThirtyFive,
}

/// A timing in the published tables' terms: the clock, each axis as active,
/// sync start, sync end and total, the polarities, the scan, and the
/// repetition. A table entry whose figures go backwards fails to compile.
pub(crate) const fn t(
    clock_khz: u32,
    horizontal: [u16; 4],
    vertical: [u16; 4],
    hsync: Polarity,
    vsync: Polarity,
    scan: Scan,
    repetition: u8,
) -> Timing {
    Timing {
        clock_khz,
        horizontal: axis(horizontal),
        vertical: axis(vertical),
        hsync,
        vsync,
        scan,
        repetition,
    }
}

const fn axis([active, sync_start, sync_end, total]: [u16; 4]) -> Axis {
    Axis {
        active,
        front: sync_start - active,
        sync: sync_end - sync_start,
        back: total - sync_end,
    }
}

/// Bytes of a detailed timing descriptor.
pub(crate) const DETAILED_LEN: usize = 18;

/// The physical size a detailed timing states for the picture, in mm; zero
/// where it states none.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Size {
    /// Width.
    pub width_mm: u16,
    /// Height.
    pub height_mm: u16,
}

/// The timing a detailed timing descriptor states and the picture size with
/// it, or `None` for a descriptor that states no timing or an impossible one.
///
/// An interlaced timing's vertical figures are a field's; they are doubled
/// into the frame's, the second field's extra half line making the total odd.
pub(crate) fn detailed(d: &[u8; DETAILED_LEN]) -> Option<(Timing, Size)> {
    let clock = u16::from_le_bytes([d[0], d[1]]);
    if clock == 0 {
        return None;
    }
    let high = |byte: u8, shift: u8, mask: u8| u16::from((byte >> shift) & mask);
    let h_active = u16::from(d[2]) | high(d[4], 4, 0xF) << 8;
    let h_blank = u16::from(d[3]) | high(d[4], 0, 0xF) << 8;
    let v_active = u16::from(d[5]) | high(d[7], 4, 0xF) << 8;
    let v_blank = u16::from(d[6]) | high(d[7], 0, 0xF) << 8;
    let h_front = u16::from(d[8]) | high(d[11], 6, 0x3) << 8;
    let h_sync = u16::from(d[9]) | high(d[11], 4, 0x3) << 8;
    let v_front = high(d[10], 4, 0xF) | high(d[11], 2, 0x3) << 4;
    let v_sync = high(d[10], 0, 0xF) | high(d[11], 0, 0x3) << 4;
    let size = Size {
        width_mm: u16::from(d[12]) | high(d[14], 4, 0xF) << 8,
        height_mm: u16::from(d[13]) | high(d[14], 0, 0xF) << 8,
    };
    let h_back = h_blank.checked_sub(h_front)?.checked_sub(h_sync)?;
    let v_back = v_blank.checked_sub(v_front)?.checked_sub(v_sync)?;
    if h_active == 0 || v_active == 0 || h_sync == 0 || v_sync == 0 {
        return None;
    }
    let flags = d[17];
    let polarity = |bit: u8| {
        if flags & bit != 0 {
            Polarity::Positive
        } else {
            Polarity::Negative
        }
    };
    let (scan, vertical) = if flags & 0x80 == 0 {
        (
            Scan::Progressive,
            Axis {
                active: v_active,
                front: v_front,
                sync: v_sync,
                back: v_back,
            },
        )
    } else {
        (
            Scan::Interlaced,
            Axis {
                active: v_active * 2,
                front: v_front * 2,
                sync: v_sync * 2,
                back: v_back * 2 + 1,
            },
        )
    };
    Some((
        Timing {
            clock_khz: u32::from(clock) * 10,
            horizontal: Axis {
                active: h_active,
                front: h_front,
                sync: h_sync,
                back: h_back,
            },
            vertical,
            hsync: polarity(0x02),
            vsync: polarity(0x04),
            scan,
            repetition: 1,
        },
        size,
    ))
}
