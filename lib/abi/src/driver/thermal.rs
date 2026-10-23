//! `thermal-sensor-v1`: a temperature sensor's channel (`plans/COOLING.md`
//! CL4).
//!
//! A sensor driver binds its node's [`THERMAL_SENSOR_ENDPOINTS`] id, gated on
//! [`CapabilityId::SYSINFO_HW`](crate::CapabilityId::SYSINFO_HW), then
//! publishes a [`THERMAL_SENSOR_NODE_COMPATIBLE`] child node carrying the
//! endpoint and the [`ThermalZone`] it measures, so a consumer finds it in the
//! hardware tree only once it serves. Its one request, [`SensorOp::Read`],
//! answers the temperature in [`Millicelsius`].

use crate::hwtree::NodeEndpointBlock;
use crate::le::{put_i32, put_u16, put_u32, read_i32, read_u16, read_u32};
use crate::reply::{decode_status_reply, encode_status_reply, STATUS_REPLY_LEN};
use crate::Errno;

/// The endpoints temperature sensors serve, indexed by the sensor's node id.
pub const THERMAL_SENSOR_ENDPOINTS: NodeEndpointBlock = NodeEndpointBlock::tagged(*b"TS");

/// `compatible` of the channel node a sensor driver publishes once it serves.
pub const THERMAL_SENSOR_NODE_COMPATIBLE: &[u8] = b"tairix,thermal-sensor";

/// A temperature in thousandths of a degree Celsius.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Millicelsius(pub i32);

impl Millicelsius {
    /// `celsius` whole degrees.
    #[must_use]
    pub fn from_celsius(celsius: i16) -> Self {
        Self(i32::from(celsius) * 1_000)
    }

    /// The temperature in tenths of a degree, rounded half away from zero.
    #[must_use]
    pub const fn tenths(self) -> i32 {
        let half = if self.0 < 0 { -50 } else { 50 };
        (self.0 + half) / 100
    }
}

/// What a sensor measures.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ThermalZone {
    /// The processor: its die, or the system-on-chip that carries it.
    Processor = 1,
}

impl ThermalZone {
    /// The value a [`HwProperty::ThermalZone`](crate::HwProperty::ThermalZone)
    /// fact carries.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self as u64
    }

    /// Inverse of [`Self::as_u64`]; [`None`] for a zone this ABI does not
    /// define.
    #[must_use]
    pub const fn from_u64(value: u64) -> Option<Self> {
        match value {
            1 => Some(Self::Processor),
            _ => None,
        }
    }
}

/// The linear conversion a thermal zone states from a sensor's raw code to
/// [`Millicelsius`]: `slope × code + offset`, the device tree's
/// `coefficients`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ThermalCalibration {
    /// Millidegrees per raw count.
    pub slope: i32,
    /// Millidegrees at a raw count of zero.
    pub offset: i32,
}

impl ThermalCalibration {
    /// The temperature `code` reads as, or [`None`] past what a
    /// [`Millicelsius`] holds.
    #[must_use]
    pub fn convert(self, code: u32) -> Option<Millicelsius> {
        let milli = i64::from(self.slope)
            .checked_mul(i64::from(code))?
            .checked_add(i64::from(self.offset))?;
        i32::try_from(milli).ok().map(Millicelsius)
    }

    /// The calibration as one property word: the slope's bits above the
    /// offset's.
    #[must_use]
    pub fn to_word(self) -> u64 {
        let mut word = [0u8; 8];
        word[..4].copy_from_slice(&self.offset.to_le_bytes());
        word[4..].copy_from_slice(&self.slope.to_le_bytes());
        u64::from_le_bytes(word)
    }

    /// Inverse of [`Self::to_word`].
    #[must_use]
    pub fn from_word(word: u64) -> Self {
        let bytes = word.to_le_bytes();
        Self {
            slope: read_i32(&bytes, 4),
            offset: read_i32(&bytes, 0),
        }
    }
}

/// A temperature sensor, as its driver reads it.
pub trait ThermalSensor {
    /// The current temperature.
    ///
    /// # Errors
    ///
    /// [`Errno::NoData`] while the hardware holds no valid conversion, or
    /// [`Errno::DeviceFault`] for a register that could not be read.
    fn read(&mut self) -> Result<Millicelsius, Errno>;
}

/// Magic opening every request frame (`"TSRQ"`).
pub const SENSOR_REQUEST_MAGIC: u32 = u32::from_le_bytes(*b"TSRQ");

/// The `thermal-sensor-v1` protocol version.
pub const SENSOR_VERSION_V1: u16 = 1;

/// Every request frame's length: magic, version, operation, a reserved byte.
pub const SENSOR_REQUEST_LEN: usize = 8;

/// Every reply frame's length: the status word and the temperature.
pub const SENSOR_REPLY_LEN: usize = STATUS_REPLY_LEN + 4;

/// The operation a request carries.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SensorOp {
    /// Answer the current temperature.
    Read = 1,
}

/// Frame a [`SensorOp::Read`] into `out`, answering its length.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold it.
pub fn encode_read(out: &mut [u8]) -> Result<usize, Errno> {
    let frame = out
        .get_mut(..SENSOR_REQUEST_LEN)
        .ok_or(Errno::BufferTooSmall)?;
    frame.fill(0);
    put_u32(frame, 0, SENSOR_REQUEST_MAGIC);
    put_u16(frame, 4, SENSOR_VERSION_V1);
    frame[6] = SensorOp::Read as u8;
    Ok(SENSOR_REQUEST_LEN)
}

/// Decode a request frame.
///
/// # Errors
///
/// * [`Errno::LengthOutOfRange`] — not exactly a request's length.
/// * [`Errno::BadMagic`] — a wrong magic or a dirty reserved byte.
/// * [`Errno::AbiVersionUnsupported`] — not [`SENSOR_VERSION_V1`].
/// * [`Errno::OutOfRange`] — an unknown operation.
pub fn decode_request(bytes: &[u8]) -> Result<SensorOp, Errno> {
    if bytes.len() != SENSOR_REQUEST_LEN {
        return Err(Errno::LengthOutOfRange);
    }
    if read_u32(bytes, 0) != SENSOR_REQUEST_MAGIC || bytes[7] != 0 {
        return Err(Errno::BadMagic);
    }
    if read_u16(bytes, 4) != SENSOR_VERSION_V1 {
        return Err(Errno::AbiVersionUnsupported);
    }
    match bytes[6] {
        1 => Ok(SensorOp::Read),
        _ => Err(Errno::OutOfRange),
    }
}

/// Frame the answer to a read: the temperature, or why there is none.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold a reply.
pub fn encode_reading(
    out: &mut [u8],
    outcome: Result<Millicelsius, Errno>,
) -> Result<usize, Errno> {
    let frame = out
        .get_mut(..SENSOR_REPLY_LEN)
        .ok_or(Errno::BufferTooSmall)?;
    frame[..STATUS_REPLY_LEN].copy_from_slice(&encode_status_reply(outcome.map(|_| ())));
    put_i32(
        frame,
        STATUS_REPLY_LEN,
        outcome.map_or(0, |reading| reading.0),
    );
    Ok(SENSOR_REPLY_LEN)
}

/// Decode the answer to a read.
///
/// # Errors
///
/// The refusal the reply carries, or [`Errno::BadMagic`] for a frame of the
/// wrong length or a refusal that carries a temperature.
pub fn decode_reading(bytes: &[u8]) -> Result<Millicelsius, Errno> {
    if bytes.len() != SENSOR_REPLY_LEN {
        return Err(Errno::BadMagic);
    }
    let reading = read_i32(bytes, STATUS_REPLY_LEN);
    match decode_status_reply(bytes) {
        Ok(()) => Ok(Millicelsius(reading)),
        Err(_) if reading != 0 => Err(Errno::BadMagic),
        Err(refusal) => Err(refusal),
    }
}

/// Answer `request` from `sensor` into `out`, answering the reply's length:
/// the one transform every sensor driver's endpoint applies, so a malformed
/// request is refused the same way by each.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold a reply.
pub fn serve_request<S: ThermalSensor + ?Sized>(
    sensor: &mut S,
    request: &[u8],
    out: &mut [u8],
) -> Result<usize, Errno> {
    let outcome = decode_request(request).and_then(|SensorOp::Read| sensor.read());
    encode_reading(out, outcome)
}

#[cfg(test)]
#[path = "thermal_tests.rs"]
mod tests;
