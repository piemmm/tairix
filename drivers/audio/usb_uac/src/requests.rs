//! The class requests the driver makes of its function (USB Audio 1.0 §5.2,
//! 2.0 §5.2), and the values they carry.
//!
//! An entity's controls are addressed through the control interface, an
//! endpoint's through the endpoint; either way a request names exactly one
//! control and reaches no other interface's state.

/// Version 1.0's request codes.
pub mod v1 {
    /// Set a control's current value.
    pub const SET_CUR: u8 = 0x01;
    /// Read a control's current value.
    pub const GET_CUR: u8 = 0x81;
    /// Read a control's least value.
    pub const GET_MIN: u8 = 0x82;
    /// Read a control's greatest value.
    pub const GET_MAX: u8 = 0x83;
    /// Read a control's resolution.
    pub const GET_RES: u8 = 0x84;
}

/// Version 2.0's request codes: one for the current value either way, one
/// for every range at once.
pub mod v2 {
    /// A control's current value.
    pub const CUR: u8 = 0x01;
    /// A control's ranges.
    pub const RANGE: u8 = 0x02;
}

/// Control selectors.
pub mod selector {
    /// A feature unit's mute.
    pub const MUTE: u8 = 0x01;
    /// A feature unit's volume.
    pub const VOLUME: u8 = 0x02;
    /// Version 1.0: an endpoint's sampling frequency.
    pub const SAMPLING_FREQUENCY: u8 = 0x01;
    /// Version 2.0: a clock source's frequency.
    pub const CLOCK_FREQUENCY: u8 = 0x01;
    /// Version 2.0: whether a clock source is running.
    pub const CLOCK_VALID: u8 = 0x02;
    /// Version 2.0: a clock selector's pin.
    pub const CLOCK_SELECTOR: u8 = 0x01;
    /// Version 2.0: a clock multiplier's numerator.
    pub const NUMERATOR: u8 = 0x01;
    /// Version 2.0: a clock multiplier's denominator.
    pub const DENOMINATOR: u8 = 0x02;
}

/// `bmRequestType`: class, to an interface or an endpoint, each way.
const CLASS_INTERFACE_OUT: u8 = 0x21;
const CLASS_INTERFACE_IN: u8 = 0xA1;
const CLASS_ENDPOINT_OUT: u8 = 0x22;
const CLASS_ENDPOINT_IN: u8 = 0xA2;

/// Which way a request's data stage runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    /// The host reads.
    In,
    /// The host writes.
    Out,
}

/// A request to control `selector` on `channel` of entity `entity`, addressed
/// through control interface `interface`, moving `length` bytes.
#[must_use]
pub const fn entity(
    direction: Direction,
    request: u8,
    selector: u8,
    channel: u8,
    entity: u8,
    interface: u8,
    length: u16,
) -> [u8; 8] {
    let request_type = match direction {
        Direction::In => CLASS_INTERFACE_IN,
        Direction::Out => CLASS_INTERFACE_OUT,
    };
    let [low, high] = length.to_le_bytes();
    [
        request_type,
        request,
        channel,
        selector,
        interface,
        entity,
        low,
        high,
    ]
}

/// A request to control `selector` of endpoint `endpoint` (its
/// `bEndpointAddress`), moving `length` bytes.
#[must_use]
pub const fn endpoint(
    direction: Direction,
    request: u8,
    selector: u8,
    endpoint: u8,
    length: u16,
) -> [u8; 8] {
    let request_type = match direction {
        Direction::In => CLASS_ENDPOINT_IN,
        Direction::Out => CLASS_ENDPOINT_OUT,
    };
    let [low, high] = length.to_le_bytes();
    [request_type, request, 0, selector, endpoint, 0, low, high]
}

/// Bytes of a version 1.0 sampling frequency.
pub const FREQUENCY_V1_LEN: u16 = 3;

/// Bytes of a version 2.0 clock frequency.
pub const FREQUENCY_V2_LEN: u16 = 4;

/// Bytes of a volume.
pub const VOLUME_LEN: u16 = 2;

/// The volume that is silence rather than a level (USB Audio 1.0
/// §5.2.2.4.3.2, 2.0 §5.2.5.7.2).
pub const VOLUME_SILENCE: i16 = i16::MIN;

/// A version 1.0 sampling frequency's three bytes.
#[must_use]
pub const fn frequency_v1(hz: u32) -> [u8; 3] {
    let [a, b, c, _] = hz.to_le_bytes();
    [a, b, c]
}

/// A version 1.0 sampling frequency read back: `None` for fewer than three
/// bytes.
#[must_use]
pub fn read_frequency_v1(bytes: &[u8]) -> Option<u32> {
    let bytes = bytes.get(..3)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0]))
}

/// A `u32` read back: `None` for fewer than four bytes.
#[must_use]
pub fn read_u32(bytes: &[u8]) -> Option<u32> {
    let bytes = bytes.get(..4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// An `i16` read back: `None` for fewer than two bytes.
#[must_use]
pub fn read_i16(bytes: &[u8]) -> Option<i16> {
    let bytes = bytes.get(..2)?;
    Some(i16::from_le_bytes([bytes[0], bytes[1]]))
}

/// One range a version 2.0 `RANGE` request answers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Subrange<T> {
    /// The least value.
    pub min: T,
    /// The greatest value.
    pub max: T,
    /// The step between values; zero for a continuous range.
    pub res: T,
}

/// The `index`th subrange of a 4-byte-parameter `RANGE` block — a clock
/// frequency's — or `None` past the block's count or its bytes.
#[must_use]
pub fn subrange_u32(block: &[u8], index: usize) -> Option<Subrange<u32>> {
    let count = usize::from(u16::from_le_bytes([*block.first()?, *block.get(1)?]));
    if index >= count {
        return None;
    }
    let at = 2 + index * 12;
    Some(Subrange {
        min: read_u32(block.get(at..)?)?,
        max: read_u32(block.get(at + 4..)?)?,
        res: read_u32(block.get(at + 8..)?)?,
    })
}

/// The `index`th subrange of a 2-byte-parameter `RANGE` block — a volume's —
/// or `None` past the block's count or its bytes.
#[must_use]
pub fn subrange_i16(block: &[u8], index: usize) -> Option<Subrange<i16>> {
    let count = usize::from(u16::from_le_bytes([*block.first()?, *block.get(1)?]));
    if index >= count {
        return None;
    }
    let at = 2 + index * 6;
    Some(Subrange {
        min: read_i16(block.get(at..)?)?,
        max: read_i16(block.get(at + 2..)?)?,
        res: read_i16(block.get(at + 4..)?)?,
    })
}

/// Bytes of a `RANGE` block holding `count` subranges of `width`-byte
/// parameters.
#[must_use]
pub const fn range_len(count: u16, width: u16) -> u16 {
    2 + count * 3 * width
}

/// A volume in 1/256 dB as hundredths of a decibel, rounded down: a range
/// reported with it ends at levels [`volume_at_least`] reaches at or above,
/// where rounding up would put the device's own extremes out of reach.
#[must_use]
pub fn millibel(volume: i16) -> i32 {
    (i32::from(volume) * 100).div_euclid(256)
}

/// The quietest volume in `min..=max`, on its `res` grid from `min`, that is
/// at least `millibel` hundredths of a decibel: rounded to the step above, so
/// a mixer setting the rest in software never has to amplify.
#[must_use]
pub fn volume_at_least(millibel: i32, min: i16, max: i16, res: i16) -> i16 {
    // The smallest 1/256 dB value at or above the request.
    let wanted = (i64::from(millibel) * 256).div_euclid(100)
        + i64::from((i64::from(millibel) * 256).rem_euclid(100) != 0);
    let (min, max) = (i64::from(min), i64::from(max));
    let res = i64::from(res).max(1);
    let steps = (wanted - min).max(0).div_euclid(res)
        + i64::from((wanted - min).max(0).rem_euclid(res) != 0);
    let value = (min + steps * res).min(max);
    i16::try_from(value).unwrap_or(i16::MAX)
}

#[cfg(test)]
#[path = "requests_tests.rs"]
mod tests;
