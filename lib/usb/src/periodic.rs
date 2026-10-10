//! Periodic endpoints: what an endpoint descriptor says, the service interval
//! and per-interval budget it means at a bus speed, and the arithmetic a
//! periodic stream runs on — explicit feedback and exact packet pacing
//! (`plans/SOUND.md` SND6).
//!
//! A periodic endpoint moves a fixed budget every service interval whether or
//! not anyone asked, so everything here is arithmetic on intervals rather
//! than on queue depths. It is pure and allocation-free, shared by the host
//! controller (which schedules the intervals) and the class driver (which
//! decides what each one carries).

use tairix_abi::usb_urb::UsbSpeed;
use tairix_abi::DriverError;

use crate::descriptor::{
    DESC_TYPE_ENDPOINT, ENDPOINT_ADDR_DIR_IN, ENDPOINT_ADDR_NUMBER_MASK, ENDPOINT_ATTR_BULK,
    ENDPOINT_ATTR_INTERRUPT, ENDPOINT_ATTR_ISOCHRONOUS, ENDPOINT_ATTR_TYPE_MASK,
    ENDPOINT_DESCRIPTOR_LEN, ENDPOINT_MAX_PACKET_MASK, ENDPOINT_TRANSACTIONS_SHIFT,
    SS_ENDPOINT_COMPANION_LEN,
};

/// How an isochronous endpoint keeps time with the host (USB 2.0 §5.12.4.1,
/// `bmAttributes` bits 3:2).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IsoSync {
    /// No synchronisation stated.
    None,
    /// The device runs its own clock and reports the rate it wants through a
    /// feedback endpoint (explicit or implicit).
    Asynchronous,
    /// The device locks to whatever rate the host delivers.
    Adaptive,
    /// The device runs from the bus's frame clock.
    Synchronous,
}

/// What an isochronous endpoint's packets carry (`bmAttributes` bits 5:4).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IsoUsage {
    /// Data.
    Data,
    /// Explicit feedback: the rate a sibling data endpoint should run at.
    Feedback,
    /// Data whose own packet sizes pace a sibling OUT endpoint.
    ImplicitFeedbackData,
}

/// An endpoint's transfer type and, for isochronous, its timing.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TransferKind {
    /// A control endpoint other than endpoint zero.
    Control,
    /// Isochronous.
    Isochronous {
        /// How it keeps time.
        sync: IsoSync,
        /// What it carries.
        usage: IsoUsage,
    },
    /// Bulk.
    Bulk,
    /// Interrupt.
    Interrupt,
}

/// A `SuperSpeed` endpoint companion's fields (USB 3.2 §9.6.7).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SsCompanion {
    /// Packets one burst carries, less one.
    pub max_burst: u8,
    /// `bmAttributes`: for an isochronous endpoint, bits 1:0 are Mult — bursts
    /// per service interval, less one.
    pub attributes: u8,
    /// The bytes one service interval moves.
    pub bytes_per_interval: u16,
}

impl SsCompanion {
    /// Decode a companion descriptor.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] for one shorter than its fixed length.
    pub fn decode(descriptor: &[u8]) -> Result<Self, DriverError> {
        if descriptor.len() < SS_ENDPOINT_COMPANION_LEN {
            return Err(DriverError::BadMagic);
        }
        Ok(Self {
            max_burst: descriptor[2],
            attributes: descriptor[3],
            bytes_per_interval: u16::from_le_bytes([descriptor[4], descriptor[5]]),
        })
    }
}

/// Byte length of the audio-class 1.0 endpoint descriptor, which appends
/// `bRefresh` and `bSynchAddress` to the standard seven (USB Audio 1.0
/// §4.6.1.1).
const AUDIO_ENDPOINT_DESCRIPTOR_LEN: usize = 9;

/// One endpoint descriptor, every field read and none assumed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EndpointDescriptor {
    /// `bEndpointAddress`.
    pub address: u8,
    /// The transfer type.
    pub kind: TransferKind,
    /// `wMaxPacketSize` bits 0:10.
    pub max_packet: u16,
    /// `wMaxPacketSize` bits 11:12: a high-speed periodic endpoint's
    /// additional transactions per microframe.
    pub transactions: u8,
    /// `bInterval`, in the units its speed and type give it.
    pub interval: u8,
    /// An audio-class 1.0 feedback endpoint's `bRefresh` (its report period as
    /// a power of two frames); `0` on a standard descriptor.
    pub refresh: u8,
    /// An audio-class 1.0 data endpoint's `bSynchAddress`, the feedback
    /// endpoint that paces it; `0` when it names none.
    pub synch_address: u8,
    /// The `SuperSpeed` companion that followed it, if any.
    pub companion: Option<SsCompanion>,
}

impl EndpointDescriptor {
    /// Decode an endpoint descriptor (the standard seven bytes, or the
    /// audio-class nine).
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] for a descriptor that is not an endpoint
    /// descriptor, is shorter than one, or names endpoint zero or sets an
    /// address bit no endpoint has.
    pub fn decode(descriptor: &[u8]) -> Result<Self, DriverError> {
        if descriptor.len() < ENDPOINT_DESCRIPTOR_LEN || descriptor[1] != DESC_TYPE_ENDPOINT {
            return Err(DriverError::BadMagic);
        }
        let address = descriptor[2];
        if address & ENDPOINT_ADDR_NUMBER_MASK == 0 || address & 0x70 != 0 {
            return Err(DriverError::BadMagic);
        }
        let attributes = descriptor[3];
        let kind = match attributes & ENDPOINT_ATTR_TYPE_MASK {
            ENDPOINT_ATTR_ISOCHRONOUS => TransferKind::Isochronous {
                sync: match attributes >> 2 & 0b11 {
                    0 => IsoSync::None,
                    1 => IsoSync::Asynchronous,
                    2 => IsoSync::Adaptive,
                    _ => IsoSync::Synchronous,
                },
                usage: match attributes >> 4 & 0b11 {
                    0 => IsoUsage::Data,
                    1 => IsoUsage::Feedback,
                    2 => IsoUsage::ImplicitFeedbackData,
                    _ => return Err(DriverError::BadMagic),
                },
            },
            ENDPOINT_ATTR_BULK => TransferKind::Bulk,
            ENDPOINT_ATTR_INTERRUPT => TransferKind::Interrupt,
            _ => TransferKind::Control,
        };
        let packet = u16::from_le_bytes([descriptor[4], descriptor[5]]);
        let (refresh, synch_address) = if descriptor.len() >= AUDIO_ENDPOINT_DESCRIPTOR_LEN {
            (descriptor[7], descriptor[8])
        } else {
            (0, 0)
        };
        Ok(Self {
            address,
            kind,
            max_packet: packet & ENDPOINT_MAX_PACKET_MASK,
            transactions: descriptor[5] >> ENDPOINT_TRANSACTIONS_SHIFT & 0b11,
            interval: descriptor[6],
            refresh,
            synch_address,
            companion: None,
        })
    }

    /// The endpoint number.
    #[must_use]
    pub const fn number(&self) -> u8 {
        self.address & ENDPOINT_ADDR_NUMBER_MASK
    }

    /// Whether data moves device → host.
    #[must_use]
    pub const fn is_in(&self) -> bool {
        self.address & ENDPOINT_ADDR_DIR_IN != 0
    }

    /// The Device Context Index the controller knows it by (xHCI §4.5.1).
    #[must_use]
    pub const fn dci(&self) -> u8 {
        self.number() * 2 + self.is_in() as u8
    }

    /// Whether this is an isochronous endpoint.
    #[must_use]
    pub const fn is_isochronous(&self) -> bool {
        matches!(self.kind, TransferKind::Isochronous { .. })
    }
}

/// The longest service interval the controller can express: `2^15`
/// microframes, 4.096 s (xHCI §6.2.3.6).
const MAX_INTERVAL_EXPONENT: u8 = 15;

/// The period between two of a periodic endpoint's service intervals, a
/// power of two microframes (125 µs).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ServiceInterval {
    exponent: u8,
}

impl ServiceInterval {
    /// An isochronous endpoint's interval: `2^(bInterval-1)` frames at full
    /// speed, `2^(bInterval-1)` microframes at high speed and above (USB 2.0
    /// §9.6.6).
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] at low speed, which has no isochronous
    ///   transfers.
    /// * [`DriverError::OutOfRange`] for a `bInterval` outside `1..=16`, or one
    ///   longer than the controller can schedule.
    pub fn isochronous(speed: UsbSpeed, b_interval: u8) -> Result<Self, DriverError> {
        if !(1..=16).contains(&b_interval) {
            return Err(DriverError::OutOfRange);
        }
        let exponent = match speed {
            UsbSpeed::Low => return Err(DriverError::Unsupported),
            // Whole frames are eight microframes.
            UsbSpeed::Full => b_interval - 1 + 3,
            UsbSpeed::High | UsbSpeed::Super => b_interval - 1,
        };
        if exponent > MAX_INTERVAL_EXPONENT {
            return Err(DriverError::OutOfRange);
        }
        Ok(Self { exponent })
    }

    /// An interrupt endpoint's interval. High speed and above state it as an
    /// exponent like isochronous; full and low speed state a linear count of
    /// frames, reduced to the power of two at or below it and held to the
    /// 1–128 ms the periodic schedule takes. A `bInterval` of zero reads as
    /// one, so a malformed descriptor is scheduled rather than refused.
    #[must_use]
    pub fn interrupt(speed: UsbSpeed, b_interval: u8) -> Self {
        let b_interval = b_interval.max(1);
        let exponent = if speed.has_microframes() {
            (b_interval - 1).min(MAX_INTERVAL_EXPONENT)
        } else {
            let microframes = u32::from(b_interval).saturating_mul(8);
            let exponent = u32::BITS - 1 - microframes.leading_zeros();
            // `exponent` is at most `log2(255 * 8)`, so it fits a byte.
            exponent.clamp(3, 10) as u8
        };
        Self { exponent }
    }

    /// The interval as the controller's context field states it.
    #[must_use]
    pub const fn exponent(self) -> u8 {
        self.exponent
    }

    /// Microframes per interval.
    #[must_use]
    pub const fn microframes(self) -> u32 {
        1 << self.exponent
    }
}

/// The largest isochronous packet at full speed (USB 2.0 §5.6.3).
const FS_ISO_MAX_PACKET: u16 = 1023;

/// The largest isochronous packet at high speed and above.
const HS_ISO_MAX_PACKET: u16 = 1024;

/// The most additional transactions a high-speed periodic endpoint moves per
/// microframe (USB 2.0 §5.9.1).
const HS_MAX_ADDITIONAL_TRANSACTIONS: u8 = 2;

/// The most packets a `SuperSpeed` isochronous burst carries, less one.
const SS_ISO_MAX_BURST: u8 = 15;

/// The most bursts a `SuperSpeed` isochronous interval carries, less one.
const SS_ISO_MAX_MULT: u8 = 2;

/// What one service interval of a periodic endpoint may move, in the terms
/// the controller's endpoint context states it (xHCI §6.2.3).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PeriodicBudget {
    /// Max Packet Size.
    pub max_packet: u16,
    /// Max Burst Size: packets per burst, less one.
    pub max_burst: u8,
    /// Mult: bursts per interval, less one (`SuperSpeed` isochronous only).
    pub mult: u8,
    /// Max ESIT Payload: the bytes one interval moves.
    pub max_esit_payload: u32,
}

impl PeriodicBudget {
    /// An isochronous endpoint's budget at `speed`.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] at low speed.
    /// * [`DriverError::BadMagic`] for a packet size of zero or past the
    ///   speed's, additional transactions below high speed, a `SuperSpeed`
    ///   endpoint with no companion, a reserved Mult, or a stated interval
    ///   payload of zero or past what its bursts can carry.
    pub fn isochronous(
        endpoint: &EndpointDescriptor,
        speed: UsbSpeed,
    ) -> Result<Self, DriverError> {
        let packet = endpoint.max_packet;
        match speed {
            UsbSpeed::Low => Err(DriverError::Unsupported),
            UsbSpeed::Full => {
                if packet == 0 || packet > FS_ISO_MAX_PACKET || endpoint.transactions != 0 {
                    return Err(DriverError::BadMagic);
                }
                Ok(Self {
                    max_packet: packet,
                    max_burst: 0,
                    mult: 0,
                    max_esit_payload: u32::from(packet),
                })
            }
            UsbSpeed::High => {
                if packet == 0
                    || packet > HS_ISO_MAX_PACKET
                    || endpoint.transactions > HS_MAX_ADDITIONAL_TRANSACTIONS
                {
                    return Err(DriverError::BadMagic);
                }
                Ok(Self {
                    max_packet: packet,
                    max_burst: endpoint.transactions,
                    mult: 0,
                    max_esit_payload: u32::from(packet) * (u32::from(endpoint.transactions) + 1),
                })
            }
            UsbSpeed::Super => {
                let companion = endpoint.companion.ok_or(DriverError::BadMagic)?;
                let mult = companion.attributes & 0b11;
                if packet == 0
                    || packet > HS_ISO_MAX_PACKET
                    || companion.max_burst > SS_ISO_MAX_BURST
                    || mult > SS_ISO_MAX_MULT
                {
                    return Err(DriverError::BadMagic);
                }
                let most = u32::from(packet)
                    * (u32::from(companion.max_burst) + 1)
                    * (u32::from(mult) + 1);
                let stated = u32::from(companion.bytes_per_interval);
                if stated == 0 || stated > most {
                    return Err(DriverError::BadMagic);
                }
                Ok(Self {
                    max_packet: packet,
                    max_burst: companion.max_burst,
                    mult,
                    max_esit_payload: stated,
                })
            }
        }
    }

    /// Packets an isochronous transfer descriptor of `bytes` moves, less one
    /// burst's worth: the TD's Transfer Burst Count and Last Burst Packet
    /// Count (xHCI §4.11.2.3). A zero-length TD is one packet.
    ///
    /// Within the interval's budget at most three bursts of sixteen packets
    /// move, so both counts fit their two- and four-bit fields; `bytes` past
    /// the budget saturates rather than wrapping into a smaller TD.
    #[must_use]
    pub fn burst_counts(self, bytes: u32) -> (u8, u8) {
        let packet = u32::from(self.max_packet.max(1));
        let packets = bytes.div_ceil(packet).max(1);
        let burst = u32::from(self.max_burst) + 1;
        let tbc = u8::try_from(packets.div_ceil(burst) - 1).unwrap_or(u8::MAX);
        let tlbpc = match packets % burst {
            0 => self.max_burst,
            rest => u8::try_from(rest - 1).unwrap_or(u8::MAX),
        };
        (tbc, tlbpc)
    }
}

/// Q16.16 scale of every rate here.
const Q16: u64 = 1 << 16;

/// Microframes in one frame.
const MICROFRAMES_PER_FRAME: u64 = 8;

/// Microframes in one second.
const MICROFRAMES_PER_SECOND: u64 = 8000;

/// A rate in sample frames per 1 ms bus frame, Q16.16.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct FeedbackRate(u64);

impl FeedbackRate {
    /// `rate_hz` frames per second as frames per bus frame.
    #[must_use]
    pub const fn from_hz(rate_hz: u32) -> Self {
        Self(rate_hz as u64 * Q16 / 1000)
    }

    /// Sample frames per bus frame, Q16.16.
    #[must_use]
    pub const fn per_frame_q16(self) -> u64 {
        self.0
    }

    /// Sample frames per second, in millihertz.
    #[must_use]
    pub const fn millihertz(self) -> u64 {
        self.0 * 1_000_000 / Q16
    }

    /// Whether `self` lies within an eighth of `nominal` — the window a real
    /// clock's correction stays in (Linux `snd_usb_handle_sync_urb` holds the
    /// same bound).
    const fn near(self, nominal: Self) -> bool {
        let tolerance = nominal.0 / 8;
        self.0 + tolerance >= nominal.0 && self.0 <= nominal.0 + tolerance
    }
}

/// Feedback interpretations tried beyond the specification's own: the value
/// scaled by `2^shift`. Devices in the field send high-speed's format at full
/// speed and the reverse; trying a fixed, small set of shifts against the
/// nominal rate reads them all with no table of devices.
const FEEDBACK_SHIFTS: [i8; 8] = [-4, -3, -2, -1, 1, 2, 3, 4];

/// Reads an explicit feedback endpoint's reports (USB 2.0 §5.12.4.2): 10.14
/// sample frames per frame in three bytes at full speed, 16.16 per microframe
/// in four at high speed and above.
///
/// The first report that lies within an eighth of the nominal rate under the
/// specification's format, or under one of a few fixed shifts of it, fixes
/// how every later report is read. A report outside that window afterwards is
/// refused rather than followed: a correction of more than an eighth is not a
/// clock drifting but a value read wrongly.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FeedbackDecoder {
    speed: UsbSpeed,
    nominal: FeedbackRate,
    shift: Option<i8>,
}

impl FeedbackDecoder {
    /// A decoder for a stream running nominally at `rate_hz` on a `speed`
    /// bus.
    #[must_use]
    pub const fn new(speed: UsbSpeed, rate_hz: u32) -> Self {
        Self {
            speed,
            nominal: FeedbackRate::from_hz(rate_hz),
            shift: None,
        }
    }

    /// The rate the stream runs at before any report.
    #[must_use]
    pub const fn nominal(&self) -> FeedbackRate {
        self.nominal
    }

    /// Read one report, answering the rate it asks for, or `None` for a report
    /// too short to hold a value or outside the nominal window.
    pub fn decode(&mut self, report: &[u8]) -> Option<FeedbackRate> {
        let width = if self.speed.has_microframes() { 4 } else { 3 };
        let bytes = report.get(..width.min(report.len()))?;
        if bytes.len() < 3 {
            return None;
        }
        let mut raw = [0u8; 8];
        raw[..bytes.len()].copy_from_slice(bytes);
        let value = u64::from_le_bytes(raw);
        let per_frame = if self.speed.has_microframes() {
            value * MICROFRAMES_PER_FRAME
        } else {
            value << 2
        };
        let read = |shift: i8| {
            FeedbackRate(if shift < 0 {
                per_frame >> shift.unsigned_abs()
            } else {
                per_frame << shift
            })
        };
        if let Some(shift) = self.shift {
            let rate = read(shift);
            return rate.near(self.nominal).then_some(rate);
        }
        let shift = core::iter::once(0)
            .chain(FEEDBACK_SHIFTS)
            .find(|&shift| read(shift).near(self.nominal))?;
        self.shift = Some(shift);
        Some(read(shift))
    }

    /// The rate an implicit-feedback device asked for by sending `frames`
    /// sample frames over `microframes` of its data endpoint's intervals, or
    /// `None` over no time at all or outside the nominal window — the same
    /// bound an explicit report is held to.
    #[must_use]
    pub fn implicit(&self, frames: u64, microframes: u64) -> Option<FeedbackRate> {
        let per_frame = frames
            .checked_mul(Q16 * MICROFRAMES_PER_FRAME)?
            .checked_div(microframes)?;
        let rate = FeedbackRate(per_frame);
        rate.near(self.nominal).then_some(rate)
    }
}

/// Spreads a rate over service intervals as whole sample frames, exactly.
///
/// Each interval carries the whole frames the accumulated rate has reached
/// and keeps the remainder, so `44 100 Hz` over 1 ms intervals is forty-four
/// frames nine times and forty-five the tenth, and an hour of intervals ends
/// on precisely the frame the rate says — the remainder is an integer, never
/// a float accumulating error.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PacketPacer {
    step: u64,
    denominator: u64,
    accumulated: u64,
}

impl PacketPacer {
    /// Pace `rate_hz` over intervals of `interval_microframes`.
    #[must_use]
    pub const fn nominal(rate_hz: u32, interval_microframes: u32) -> Self {
        Self {
            step: rate_hz as u64 * interval_microframes as u64,
            denominator: MICROFRAMES_PER_SECOND,
            accumulated: 0,
        }
    }

    /// Follow `rate` from the next interval on, carrying the fraction already
    /// accumulated across so a correction never drops or invents a frame.
    pub fn follow(&mut self, rate: FeedbackRate, interval_microframes: u32) {
        let denominator = MICROFRAMES_PER_FRAME * Q16;
        // The fraction is below one frame, so rescaling it cannot overflow.
        self.accumulated = self.accumulated * denominator / self.denominator;
        self.step = rate.per_frame_q16() * u64::from(interval_microframes);
        self.denominator = denominator;
    }

    /// The frames the next interval carries.
    pub fn next_frames(&mut self) -> u32 {
        self.accumulated += self.step;
        let frames = self.accumulated / self.denominator;
        self.accumulated -= frames * self.denominator;
        // A rate is at most the ABI's 768 kHz, so an interval of at most
        // 4.096 s carries well under `u32::MAX` frames.
        u32::try_from(frames).unwrap_or(u32::MAX)
    }

    /// The frames the next `intervals` intervals carry together, in one
    /// step: what that many calls to [`Self::next_frames`] would sum to.
    pub fn advance(&mut self, intervals: u32) -> u64 {
        let total = u128::from(self.accumulated) + u128::from(self.step) * u128::from(intervals);
        let denominator = u128::from(self.denominator);
        let frames = total / denominator;
        // The remainder is below the denominator, which is a `u64`.
        self.accumulated = u64::try_from(total - frames * denominator).unwrap_or(0);
        u64::try_from(frames).unwrap_or(u64::MAX)
    }

    /// The most frames any interval carries at the current rate.
    #[must_use]
    pub fn max_frames(&self) -> u32 {
        u32::try_from(self.step.div_ceil(self.denominator)).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
#[path = "periodic_tests.rs"]
mod tests;
