//! `thermal-v1`: the cooling service's status (`plans/COOLING.md` CL6).
//!
//! `thermald` owns the machine's fan policy ([`FanPolicy`], set in
//! `system.conf`), reads the processor's sensors and switches the fan's GPIO
//! line. Its one request, [`ThermalRequest::Status`], is ungated: what the
//! processor's temperature is and whether the fan runs are facts any user may
//! read, as every other system shows them.

use crate::driver::gpio::{HeaderLines, Polarity};
use crate::driver::thermal::Millicelsius;
use crate::le::{put_i32, put_u16, put_u32, put_u64, read_i32, read_u16, read_u32, read_u64};
use crate::reply::{decode_status_reply, encode_status_reply, STATUS_REPLY_LEN};
use crate::Errno;

/// Well-known call endpoint of the cooling service (`"THM\0"`).
pub const THERMAL_ENDPOINT: u64 = 0x5448_4D00;

/// How the fan is switched.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum FanMode {
    /// Never: the line is left undriven.
    #[default]
    Off = 0,
    /// Always.
    On = 1,
    /// By the processor's temperature, with hysteresis.
    Automatic = 2,
}

impl FanMode {
    /// Every mode, in the order a chooser offers them.
    pub const ALL: [Self; 3] = [Self::Off, Self::On, Self::Automatic];

    const fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Off),
            1 => Some(Self::On),
            2 => Some(Self::Automatic),
            _ => None,
        }
    }

    /// Whether the mode switches a line at all.
    #[must_use]
    pub const fn drives(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// Whole degrees Celsius a fan may be set to start at.
pub const FAN_START_CELSIUS: core::ops::RangeInclusive<u8> = 40..=85;

/// Whole degrees Celsius a running fan must cool by before it stops.
pub const FAN_HYSTERESIS_CELSIUS: core::ops::RangeInclusive<u8> = 2..=20;

/// The machine's fan policy, every field already in range.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct FanPolicy {
    mode: FanMode,
    line: Option<u8>,
    polarity: Polarity,
    start: u8,
    hysteresis: u8,
}

impl Default for FanPolicy {
    fn default() -> Self {
        Self {
            mode: FanMode::Off,
            line: None,
            polarity: Polarity::ActiveHigh,
            start: 65,
            hysteresis: 10,
        }
    }
}

impl FanPolicy {
    /// The policy these fields state.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a line past the header's width, a start
    /// outside [`FAN_START_CELSIUS`] or a hysteresis outside
    /// [`FAN_HYSTERESIS_CELSIUS`].
    pub fn new(
        mode: FanMode,
        line: Option<u8>,
        polarity: Polarity,
        start: u8,
        hysteresis: u8,
    ) -> Result<Self, Errno> {
        let line_fits =
            line.is_none_or(|line| usize::from(line) < crate::driver::gpio::GPIO_HEADER_LINES_MAX);
        if !line_fits
            || !FAN_START_CELSIUS.contains(&start)
            || !FAN_HYSTERESIS_CELSIUS.contains(&hysteresis)
        {
            return Err(Errno::OutOfRange);
        }
        Ok(Self {
            mode,
            line,
            polarity,
            start,
            hysteresis,
        })
    }

    /// How the fan is switched.
    #[must_use]
    pub const fn mode(&self) -> FanMode {
        self.mode
    }

    /// The header line the fan is wired to, if one is chosen.
    #[must_use]
    pub const fn line(&self) -> Option<u8> {
        self.line
    }

    /// Which level of the line runs the fan.
    #[must_use]
    pub const fn polarity(&self) -> Polarity {
        self.polarity
    }

    /// The temperature, in whole degrees, an automatic fan starts at.
    #[must_use]
    pub const fn start_celsius(&self) -> u8 {
        self.start
    }

    /// The degrees a running automatic fan cools by before it stops.
    #[must_use]
    pub const fn hysteresis_celsius(&self) -> u8 {
        self.hysteresis
    }

    /// The temperature an automatic fan stops at.
    #[must_use]
    pub const fn stop_celsius(&self) -> u8 {
        // The ranges keep the start above the hysteresis.
        self.start - self.hysteresis
    }

    /// Whether an automatic fan in state `running` should run at
    /// `temperature`: it starts at the start temperature, and once running
    /// stops only at the stop temperature.
    #[must_use]
    pub fn runs_at(&self, temperature: Millicelsius, running: bool) -> bool {
        if running {
            temperature > Millicelsius::from_celsius(i16::from(self.stop_celsius()))
        } else {
            temperature >= Millicelsius::from_celsius(i16::from(self.start))
        }
    }
}

/// Why the fan is not doing what its policy asks.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum FanFault {
    /// The policy switches the fan, but names no line.
    NoLine = 1,
    /// The machine has no header whose lines are served.
    NoHeader = 2,
    /// The header would not lend the line: not one it lends, held or routed
    /// elsewhere, or its controller not answering.
    LineUnavailable = 3,
    /// Automatic, but the machine has no processor sensor; the fan runs.
    NoSensor = 4,
    /// Automatic, but the processor's temperature could not be read, or read
    /// what no processor could be at; the fan runs until it can.
    SensorFailed = 5,
}

impl FanFault {
    const fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::NoLine),
            2 => Some(Self::NoHeader),
            3 => Some(Self::LineUnavailable),
            4 => Some(Self::NoSensor),
            5 => Some(Self::SensorFailed),
            _ => None,
        }
    }
}

/// The cooling service's answer: the processor's temperature, the policy in
/// effect, the fan, and the header's lines.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ThermalStatus {
    /// The hottest processor sensor's reading, if any could be taken.
    pub temperature: Option<Millicelsius>,
    /// The policy the service is applying.
    pub policy: FanPolicy,
    /// Whether the fan's line is driven on.
    pub running: bool,
    /// Why the fan is not as the policy asks, if it is not.
    pub fault: Option<FanFault>,
    /// The header's lines as the fan may use them; [`None`] when the machine
    /// has no header served or its lines could not be read.
    pub lines: Option<FanLines>,
}

/// The header's lines as the fan may use them: every line it lends, and those
/// the fan may be put on, free or the fan's already. How the others are held
/// or routed is the hardware inventory's to tell, not the fan's.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct FanLines {
    lent: HeaderLines,
    free: HeaderLines,
}

impl FanLines {
    /// The `lent` lines, of which the fan may take `free`; [`None`] when
    /// `free` names a line not lent.
    #[must_use]
    pub const fn new(lent: HeaderLines, free: HeaderLines) -> Option<Self> {
        if free.mask() & !lent.mask() != 0 {
            return None;
        }
        Some(Self { lent, free })
    }

    /// Every line the header lends.
    #[must_use]
    pub const fn lent(self) -> HeaderLines {
        self.lent
    }

    /// The lines the fan may be put on.
    #[must_use]
    pub const fn free(self) -> HeaderLines {
        self.free
    }

    /// The lent lines something else holds or routes.
    #[must_use]
    pub const fn held(self) -> HeaderLines {
        HeaderLines::from_mask(self.lent.mask() & !self.free.mask())
    }
}

/// Magic opening every request frame (`"THRQ"`).
pub const THERMAL_REQUEST_MAGIC: u32 = u32::from_le_bytes(*b"THRQ");

/// The `thermal-v1` protocol version.
pub const THERMAL_VERSION_V1: u16 = 1;

/// Every request frame's length: magic, version, operation, a reserved byte.
pub const THERMAL_REQUEST_LEN: usize = 8;

/// The status's fixed part after its status word: flags, mode, line,
/// polarity, start, hysteresis, fault, a reserved byte and the temperature.
const STATUS_FIXED_LEN: usize = 12;

/// The [`FanLines`] after the fixed part: the lent mask, then the free one.
const FAN_LINES_LEN: usize = 16;

/// The longest reply: a status with the header's lines.
pub const THERMAL_MAX_REPLY: usize = STATUS_REPLY_LEN + STATUS_FIXED_LEN + FAN_LINES_LEN;

const FLAG_TEMPERATURE: u8 = 1 << 0;
const FLAG_RUNNING: u8 = 1 << 1;
const FLAG_LINES: u8 = 1 << 2;
const FLAGS_KNOWN: u8 = FLAG_TEMPERATURE | FLAG_RUNNING | FLAG_LINES;
/// The line byte that says no line is chosen.
const NO_LINE: u8 = u8::MAX;

/// A request to the cooling service.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ThermalRequest {
    /// Answer the [`ThermalStatus`].
    Status,
}

impl ThermalRequest {
    /// Encode the request into `out`, answering its length.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] if `out` cannot hold it.
    pub fn encode(self, out: &mut [u8]) -> Result<usize, Errno> {
        let frame = out
            .get_mut(..THERMAL_REQUEST_LEN)
            .ok_or(Errno::BufferTooSmall)?;
        frame.fill(0);
        put_u32(frame, 0, THERMAL_REQUEST_MAGIC);
        put_u16(frame, 4, THERMAL_VERSION_V1);
        frame[6] = match self {
            Self::Status => 1,
        };
        Ok(THERMAL_REQUEST_LEN)
    }

    /// Decode a request frame.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] — not exactly a request's length.
    /// * [`Errno::BadMagic`] — a wrong magic or a dirty reserved byte.
    /// * [`Errno::AbiVersionUnsupported`] — not [`THERMAL_VERSION_V1`].
    /// * [`Errno::OutOfRange`] — an unknown operation.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() != THERMAL_REQUEST_LEN {
            return Err(Errno::LengthOutOfRange);
        }
        if read_u32(bytes, 0) != THERMAL_REQUEST_MAGIC || bytes[7] != 0 {
            return Err(Errno::BadMagic);
        }
        if read_u16(bytes, 4) != THERMAL_VERSION_V1 {
            return Err(Errno::AbiVersionUnsupported);
        }
        match bytes[6] {
            1 => Ok(Self::Status),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// Frame the answer to a [`ThermalRequest::Status`].
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `out` cannot hold the reply.
pub fn encode_status(
    out: &mut [u8],
    outcome: Result<&ThermalStatus, Errno>,
) -> Result<usize, Errno> {
    let Ok(status) = outcome else {
        let frame = out
            .get_mut(..STATUS_REPLY_LEN)
            .ok_or(Errno::BufferTooSmall)?;
        frame.copy_from_slice(&encode_status_reply(outcome.map(|_| ())));
        return Ok(STATUS_REPLY_LEN);
    };
    let lines_len = if status.lines.is_some() {
        FAN_LINES_LEN
    } else {
        0
    };
    let len = STATUS_REPLY_LEN + STATUS_FIXED_LEN + lines_len;
    let frame = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
    frame.fill(0);
    frame[..STATUS_REPLY_LEN].copy_from_slice(&encode_status_reply(Ok(())));
    let mut flags = 0;
    if status.temperature.is_some() {
        flags |= FLAG_TEMPERATURE;
    }
    if status.running {
        flags |= FLAG_RUNNING;
    }
    if status.lines.is_some() {
        flags |= FLAG_LINES;
    }
    let policy = status.policy;
    let fixed = &mut frame[STATUS_REPLY_LEN..STATUS_REPLY_LEN + STATUS_FIXED_LEN];
    fixed[..8].copy_from_slice(&[
        flags,
        policy.mode as u8,
        policy.line.unwrap_or(NO_LINE),
        policy.polarity as u8,
        policy.start,
        policy.hysteresis,
        status.fault.map_or(0, |fault| fault as u8),
        0,
    ]);
    put_i32(fixed, 8, status.temperature.map_or(0, |reading| reading.0));
    if let Some(lines) = status.lines {
        let at = STATUS_REPLY_LEN + STATUS_FIXED_LEN;
        put_u64(frame, at, lines.lent.mask());
        put_u64(frame, at + 8, lines.free.mask());
    }
    Ok(len)
}

/// Decode the answer to a [`ThermalRequest::Status`].
///
/// # Errors
///
/// The refusal the reply carries, or [`Errno::BadMagic`] for a frame of the
/// wrong length, a dirty reserved field, an undefined value, or a field out of
/// the policy's range.
pub fn decode_status(bytes: &[u8]) -> Result<ThermalStatus, Errno> {
    if bytes.len() < STATUS_REPLY_LEN {
        return Err(Errno::BadMagic);
    }
    decode_status_reply(bytes)?;
    let fixed = bytes
        .get(STATUS_REPLY_LEN..STATUS_REPLY_LEN + STATUS_FIXED_LEN)
        .ok_or(Errno::BadMagic)?;
    let [flags, mode, line, polarity, start, hysteresis, fault, reserved] =
        [0, 1, 2, 3, 4, 5, 6, 7].map(|at| fixed[at]);
    if flags & !FLAGS_KNOWN != 0 || reserved != 0 {
        return Err(Errno::BadMagic);
    }
    let polarity = Polarity::from_u8(polarity).ok_or(Errno::BadMagic)?;
    let policy = FanPolicy::new(
        FanMode::from_u8(mode).ok_or(Errno::BadMagic)?,
        (line != NO_LINE).then_some(line),
        polarity,
        start,
        hysteresis,
    )
    .map_err(|_| Errno::BadMagic)?;
    let fault = match fault {
        0 => None,
        code => Some(FanFault::from_u8(code).ok_or(Errno::BadMagic)?),
    };
    let reading = read_i32(fixed, 8);
    let temperature = if flags & FLAG_TEMPERATURE == 0 {
        if reading != 0 {
            return Err(Errno::BadMagic);
        }
        None
    } else {
        Some(Millicelsius(reading))
    };
    let rest = &bytes[STATUS_REPLY_LEN + STATUS_FIXED_LEN..];
    let lines = if flags & FLAG_LINES == 0 {
        if !rest.is_empty() {
            return Err(Errno::BadMagic);
        }
        None
    } else {
        if rest.len() != FAN_LINES_LEN {
            return Err(Errno::BadMagic);
        }
        let lent = HeaderLines::from_mask(read_u64(rest, 0));
        let free = HeaderLines::from_mask(read_u64(rest, 8));
        Some(FanLines::new(lent, free).ok_or(Errno::BadMagic)?)
    };
    Ok(ThermalStatus {
        temperature,
        policy,
        running: flags & FLAG_RUNNING != 0,
        fault,
        lines,
    })
}

#[cfg(test)]
#[path = "thermal_ipc_tests.rs"]
mod tests;
