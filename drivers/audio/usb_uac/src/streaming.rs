//! The function's streaming interfaces: which way each carries samples, and
//! the encoding, channel layout, rates and endpoints each alternate setting
//! offers (USB Audio 1.0 §4.5 and 2.0 §4.9; USB Audio Data Formats 1.0 §2.2
//! and 2.0 §2.3.1).
//!
//! A setting the audio vocabulary cannot carry exactly — an encoding it has
//! no word for, a channel position it does not name, a rate outside the range
//! any converter runs at — is left out rather than approximated, and an
//! interface left with no setting is no endpoint at all.

use alloc::vec::Vec;

use tairix_abi::driver::audio::{
    ChannelMap, ChannelPosition, Rate, RateSet, RateSupport, SampleFormat, StreamDirection,
    MAX_CHANNELS, MAX_DEVICE_RATES,
};
use tairix_abi::DriverError;
use tairix_usb::alternate::{alternate_setting, association_of};
use tairix_usb::descriptor::{
    descriptors, ConfigurationHeader, Malformed, DESC_TYPE_ENDPOINT, DESC_TYPE_INTERFACE,
    INTERFACE_DESCRIPTOR_LEN,
};
use tairix_usb::periodic::{EndpointDescriptor, IsoUsage, TransferKind};

use crate::topology::{
    Cluster, EntityKind, Topology, Version, CS_INTERFACE, TERMINAL_USB_STREAMING,
};

/// `bDescriptorType` of a class-specific endpoint descriptor.
const CS_ENDPOINT: u8 = 0x25;

/// Class-specific interface subtypes of a streaming interface.
const AS_GENERAL: u8 = 0x01;
const FORMAT_TYPE: u8 = 0x02;

/// `bFormatType` of the PCM-family formats.
const FORMAT_TYPE_I: u8 = 0x01;

/// `EP_GENERAL`, a class-specific endpoint descriptor's subtype.
const EP_GENERAL: u8 = 0x01;

/// A version 1.0 `EP_GENERAL`'s `bmAttributes` bit: the endpoint's sampling
/// frequency is set through it.
const SAMPLING_FREQUENCY_CONTROL: u8 = 0x01;

/// Version 1.0 `wFormatTag`s of the Type I encodings the vocabulary carries.
mod tag {
    pub const PCM: u16 = 0x0001;
    pub const PCM8: u16 = 0x0002;
    pub const IEEE_FLOAT: u16 = 0x0003;
}

/// Version 2.0 `bmFormats` bits of the same encodings.
mod formats {
    pub const PCM: u32 = 1 << 0;
    pub const PCM8: u32 = 1 << 1;
    pub const IEEE_FLOAT: u32 = 1 << 2;
}

/// The Type I encodings, by family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Encoding {
    Pcm,
    Pcm8,
    Float,
}

/// One streaming interface: one endpoint of the device.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamingInterface {
    /// `bInterfaceNumber`.
    pub interface: u8,
    /// Which way its samples flow.
    pub direction: StreamDirection,
    /// The USB streaming terminal its settings link to.
    pub terminal: u8,
    /// The settings it offers that the vocabulary carries, in descriptor
    /// order.
    pub formats: Vec<AltFormat>,
}

/// What one alternate setting streams.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AltFormat {
    /// `bAlternateSetting`.
    pub alternate: u8,
    /// The sample encoding.
    pub format: SampleFormat,
    /// The channel layout, in interleave order.
    pub channel_map: ChannelMap,
    /// Where its rates come from.
    pub rates: AltRates,
    /// The isochronous endpoint carrying its samples.
    pub data: EndpointDescriptor,
    /// The explicit feedback endpoint pacing an asynchronous OUT endpoint.
    pub feedback: Option<EndpointDescriptor>,
    /// Version 1.0: the sampling frequency is set on the data endpoint.
    pub frequency_control: bool,
}

impl AltFormat {
    /// Bytes one frame occupies on the wire.
    #[must_use]
    pub fn frame_bytes(&self) -> usize {
        self.format.bytes_per_sample() * usize::from(self.channel_map.channels())
    }
}

/// Where a setting's rates come from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AltRates {
    /// Version 1.0: the rates its format descriptor lists.
    Listed(RateSupport),
    /// Version 2.0: whatever its terminal's clock runs at.
    Clock,
}

/// The streaming interfaces of the function whose control interface is
/// `control`, with every setting each offers that the vocabulary carries.
///
/// Version 1.0 names them in its header; version 2.0 groups them with the
/// control interface in an interface association, which it requires.
///
/// # Errors
///
/// [`DriverError::BadMagic`] for a malformed configuration or setting, or a
/// version 2.0 function no association groups; [`DriverError::OutOfMemory`].
pub fn streaming_interfaces(
    config: &[u8],
    topology: &Topology,
    control: u8,
) -> Result<Vec<StreamingInterface>, DriverError> {
    let mut numbers: Vec<u8> = Vec::new();
    match topology.version {
        Version::One => {
            numbers
                .try_reserve_exact(topology.streaming.len())
                .map_err(|_| DriverError::OutOfMemory)?;
            numbers.extend(topology.streaming.iter().filter(|&&n| n != control));
        }
        Version::Two => {
            let association = association_of(config, control)?.ok_or(DriverError::BadMagic)?;
            for offset in 0..association.count {
                let number = association
                    .first
                    .checked_add(offset)
                    .ok_or(DriverError::BadMagic)?;
                if number != control {
                    numbers
                        .try_reserve(1)
                        .map_err(|_| DriverError::OutOfMemory)?;
                    numbers.push(number);
                }
            }
        }
    }
    let mut interfaces = Vec::new();
    for number in numbers {
        if let Some(interface) = streaming_interface(config, topology, number)? {
            interfaces
                .try_reserve(1)
                .map_err(|_| DriverError::OutOfMemory)?;
            interfaces.push(interface);
        }
    }
    Ok(interfaces)
}

/// Interface `number` as a streaming interface, or `None` when no setting of
/// it streams anything the vocabulary carries.
fn streaming_interface(
    config: &[u8],
    topology: &Topology,
    number: u8,
) -> Result<Option<StreamingInterface>, DriverError> {
    let mut found: Option<StreamingInterface> = None;
    for alternate in alternates(config, number)? {
        let Some((terminal, format)) = alt_format(config, topology, number, alternate)? else {
            continue;
        };
        let direction = if format.data.is_in() {
            StreamDirection::Capture
        } else {
            StreamDirection::Playback
        };
        let interface = found.get_or_insert_with(|| StreamingInterface {
            interface: number,
            direction,
            terminal,
            formats: Vec::new(),
        });
        // Every setting of one interface links one terminal one way.
        if interface.direction != direction || interface.terminal != terminal {
            return Err(DriverError::BadMagic);
        }
        interface
            .formats
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        interface.formats.push(format);
    }
    Ok(found)
}

/// Every non-default alternate setting interface `number` states.
fn alternates(config: &[u8], number: u8) -> Result<Vec<u8>, DriverError> {
    let mut out = Vec::new();
    for descriptor in descriptors(body(config)?) {
        let descriptor = descriptor.map_err(|Malformed| DriverError::BadMagic)?;
        if descriptor[1] == DESC_TYPE_INTERFACE {
            if descriptor.len() < INTERFACE_DESCRIPTOR_LEN {
                return Err(DriverError::BadMagic);
            }
            if descriptor[2] == number && descriptor[3] != 0 && !out.contains(&descriptor[3]) {
                out.try_reserve(1).map_err(|_| DriverError::OutOfMemory)?;
                out.push(descriptor[3]);
            }
        }
    }
    Ok(out)
}

fn body(config: &[u8]) -> Result<&[u8], DriverError> {
    let header = ConfigurationHeader::decode(config).map_err(|Malformed| DriverError::BadMagic)?;
    config.get(header.length..).ok_or(DriverError::BadMagic)
}

/// The class-specific descriptors one setting carries.
#[derive(Default)]
struct SettingDescriptors<'a> {
    general: Option<&'a [u8]>,
    format: Option<&'a [u8]>,
    /// Each `EP_GENERAL`, with the endpoint it follows.
    endpoint_general: Vec<(u8, &'a [u8])>,
}

fn setting_descriptors(
    config: &[u8],
    number: u8,
    alternate: u8,
) -> Result<SettingDescriptors<'_>, DriverError> {
    let mut found = SettingDescriptors::default();
    let mut inside = false;
    let mut last_endpoint = None;
    for descriptor in descriptors(body(config)?) {
        let descriptor = descriptor.map_err(|Malformed| DriverError::BadMagic)?;
        match descriptor[1] {
            DESC_TYPE_INTERFACE => {
                if descriptor.len() < INTERFACE_DESCRIPTOR_LEN {
                    return Err(DriverError::BadMagic);
                }
                inside = descriptor[2] == number && descriptor[3] == alternate;
                last_endpoint = None;
            }
            DESC_TYPE_ENDPOINT if inside => last_endpoint = descriptor.get(2).copied(),
            CS_INTERFACE if inside && descriptor.len() >= 3 => {
                let slot = match descriptor[2] {
                    AS_GENERAL => &mut found.general,
                    FORMAT_TYPE => &mut found.format,
                    _ => continue,
                };
                if slot.replace(descriptor).is_some() {
                    return Err(DriverError::BadMagic);
                }
            }
            CS_ENDPOINT if inside && descriptor.len() >= 4 && descriptor[2] == EP_GENERAL => {
                if let Some(address) = last_endpoint {
                    found
                        .endpoint_general
                        .try_reserve(1)
                        .map_err(|_| DriverError::OutOfMemory)?;
                    found.endpoint_general.push((address, descriptor));
                }
            }
            _ => {}
        }
    }
    Ok(found)
}

/// Setting `alternate` of interface `number`: the terminal it links and the
/// format it streams, or `None` for a setting that streams nothing the
/// vocabulary carries.
fn alt_format(
    config: &[u8],
    topology: &Topology,
    number: u8,
    alternate: u8,
) -> Result<Option<(u8, AltFormat)>, DriverError> {
    let setting = alternate_setting(config, number, alternate)?;
    let found = setting_descriptors(config, number, alternate)?;
    let (Some(general), Some(format)) = (found.general, found.format) else {
        return Ok(None);
    };
    // A version 1.0 synchronisation endpoint often states data usage, so the
    // endpoint another one names as its `bSynchAddress` is never the data.
    let synchronises = |address: u8| {
        setting
            .endpoints()
            .any(|other| other.address != address && other.synch_address == address)
    };
    let Some(data) = setting.endpoints().copied().find(|endpoint| {
        matches!(
            endpoint.kind,
            TransferKind::Isochronous {
                usage: IsoUsage::Data | IsoUsage::ImplicitFeedbackData,
                ..
            }
        ) && !synchronises(endpoint.address)
    }) else {
        return Ok(None);
    };
    let feedback = setting.endpoints().copied().find(|endpoint| {
        !data.is_in()
            && endpoint.is_in()
            && endpoint.is_isochronous()
            && (matches!(
                endpoint.kind,
                TransferKind::Isochronous {
                    usage: IsoUsage::Feedback,
                    ..
                }
            ) || endpoint.address == data.synch_address)
    });
    let decoded = match topology.version {
        Version::One => decode_v1(general, format),
        Version::Two => decode_v2(general, format),
    }?;
    let Some(decoded) = decoded else {
        return Ok(None);
    };
    // The terminal the setting links is the host's end of a path: a playback
    // stream enters at an input terminal, a capture stream leaves at an
    // output terminal.
    let linked = topology.entity(decoded.terminal).map(|entity| &entity.kind);
    let streaming_end = match linked {
        Some(EntityKind::InputTerminal { terminal_type, .. }) => {
            *terminal_type == TERMINAL_USB_STREAMING && !data.is_in()
        }
        Some(EntityKind::OutputTerminal { terminal_type, .. }) => {
            *terminal_type == TERMINAL_USB_STREAMING && data.is_in()
        }
        _ => false,
    };
    if !streaming_end {
        return Ok(None);
    }
    let cluster = match decoded.cluster {
        Some(cluster) => cluster,
        None => v1_cluster(topology, decoded.terminal, decoded.channels),
    };
    let Some(channel_map) = channel_map(cluster) else {
        return Ok(None);
    };
    let frequency_control = found.endpoint_general.iter().any(|&(address, general)| {
        address == data.address && general[3] & SAMPLING_FREQUENCY_CONTROL != 0
    });
    Ok(Some((
        decoded.terminal,
        AltFormat {
            alternate,
            format: decoded.format,
            channel_map,
            rates: decoded.rates,
            data,
            feedback,
            frequency_control: topology.version == Version::One && frequency_control,
        },
    )))
}

/// What a setting's general and format descriptors say.
struct Decoded {
    terminal: u8,
    format: SampleFormat,
    channels: u8,
    /// Version 2.0 states its cluster here; 1.0 states only the count.
    cluster: Option<Cluster>,
    rates: AltRates,
}

fn decode_v1(general: &[u8], format: &[u8]) -> Result<Option<Decoded>, DriverError> {
    if general.len() < 7 || format.len() < 8 {
        return Err(DriverError::BadMagic);
    }
    let encoding = match u16::from_le_bytes([general[5], general[6]]) {
        tag::PCM => Encoding::Pcm,
        tag::PCM8 => Encoding::Pcm8,
        tag::IEEE_FLOAT => Encoding::Float,
        _ => return Ok(None),
    };
    if format[3] != FORMAT_TYPE_I {
        return Ok(None);
    }
    let (channels, subframe, bits) = (format[4], format[5], format[6]);
    let Some(sample) = sample_format(encoding, subframe, bits) else {
        return Ok(None);
    };
    let Some(rates) = listed_rates(format)? else {
        return Ok(None);
    };
    Ok(Some(Decoded {
        terminal: general[3],
        format: sample,
        channels,
        cluster: None,
        rates: AltRates::Listed(rates),
    }))
}

fn decode_v2(general: &[u8], format: &[u8]) -> Result<Option<Decoded>, DriverError> {
    if general.len() < 16 || format.len() < 6 {
        return Err(DriverError::BadMagic);
    }
    if general[5] != FORMAT_TYPE_I || format[3] != FORMAT_TYPE_I {
        return Ok(None);
    }
    let stated = u32::from_le_bytes([general[6], general[7], general[8], general[9]]);
    let (subslot, bits) = (format[4], format[5]);
    // The first encoding the setting states that the vocabulary carries at
    // its slot size, PCM before the others.
    let Some(sample) = [
        (formats::PCM, Encoding::Pcm),
        (formats::PCM8, Encoding::Pcm8),
        (formats::IEEE_FLOAT, Encoding::Float),
    ]
    .into_iter()
    .filter(|&(bit, _)| stated & bit != 0)
    .find_map(|(_, encoding)| sample_format(encoding, subslot, bits)) else {
        return Ok(None);
    };
    let channels = general[10];
    Ok(Some(Decoded {
        terminal: general[3],
        format: sample,
        channels,
        cluster: Some(Cluster {
            channels,
            config: u32::from_le_bytes([general[11], general[12], general[13], general[14]]),
        }),
        rates: AltRates::Clock,
    }))
}

/// The vocabulary's word for `encoding` in `slot`-byte subslots carrying
/// `bits` significant bits.
///
/// A sample is left-justified in its subslot, its unused low bits zero, so a
/// 24-bit sample in a 4-byte subslot is a 32-bit sample whose low byte the
/// device ignores — never one sign-extended into the low three bytes.
fn sample_format(encoding: Encoding, slot: u8, bits: u8) -> Option<SampleFormat> {
    if bits == 0 || u32::from(bits) > u32::from(slot) * 8 {
        return None;
    }
    match (encoding, slot) {
        (Encoding::Pcm, 2) => Some(SampleFormat::S16),
        (Encoding::Pcm, 3) => Some(SampleFormat::S24),
        (Encoding::Pcm, 4) => Some(SampleFormat::S32),
        (Encoding::Pcm8, 1) => Some(SampleFormat::U8),
        (Encoding::Float, 4) if bits == 32 => Some(SampleFormat::F32),
        _ => None,
    }
}

/// A version 1.0 Type I format descriptor's rates: a continuous range, or a
/// list. `None` when none lies within the range any converter runs at.
fn listed_rates(format: &[u8]) -> Result<Option<RateSupport>, DriverError> {
    let count = usize::from(format[7]);
    let frequency = |index: usize| -> Result<u32, DriverError> {
        let at = 8 + index * 3;
        let bytes = format.get(at..at + 3).ok_or(DriverError::BadMagic)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0]))
    };
    if count == 0 {
        let (lower, upper) = (frequency(0)?, frequency(1)?);
        let clamp = |hz: u32| hz.clamp(Rate::MIN_HZ, Rate::MAX_HZ);
        if lower > upper || upper < Rate::MIN_HZ || lower > Rate::MAX_HZ {
            return Ok(None);
        }
        let (Ok(min), Ok(max)) = (Rate::new(clamp(lower)), Rate::new(clamp(upper))) else {
            return Ok(None);
        };
        return Ok(Some(RateSupport::Continuous { min, max }));
    }
    let mut rates = [Rate::HZ_48000; MAX_DEVICE_RATES];
    let mut held = 0usize;
    let mut listed = Vec::new();
    listed
        .try_reserve_exact(count)
        .map_err(|_| DriverError::OutOfMemory)?;
    for index in 0..count {
        if let Ok(rate) = Rate::new(frequency(index)?) {
            listed.push(rate);
        }
    }
    listed.sort_unstable();
    listed.dedup();
    // A device listing more rates than the contract carries keeps its
    // lowest, the ones a converter is most likely to be asked for.
    for rate in listed.into_iter().take(MAX_DEVICE_RATES) {
        rates[held] = rate;
        held += 1;
    }
    if held == 0 {
        return Ok(None);
    }
    Ok(RateSet::new(&rates[..held]).ok().map(RateSupport::Discrete))
}

/// A version 1.0 stream's cluster: its count, with the positions of the
/// terminal at the outside world's end of its path where the counts agree,
/// and none stated where they do not.
fn v1_cluster(topology: &Topology, terminal: u8, channels: u8) -> Cluster {
    let stated = |id: u8| match topology.entity(id).map(|entity| &entity.kind) {
        Some(EntityKind::InputTerminal { cluster, .. }) if cluster.channels == channels => {
            Some(cluster.config)
        }
        _ => None,
    };
    let config = stated(terminal)
        .or_else(|| {
            topology
                .path_from(terminal)
                .and_then(|path| path.last().copied())
                .and_then(stated)
        })
        .unwrap_or(0);
    Cluster { channels, config }
}

/// The positions a cluster's spatial bits name, in bit order: front left,
/// right and centre, low frequency, rear left and right, then side left and
/// right. Both revisions number these alike.
const POSITION_BITS: [(u32, ChannelPosition); 8] = [
    (0, ChannelPosition::FrontLeft),
    (1, ChannelPosition::FrontRight),
    (2, ChannelPosition::FrontCentre),
    (3, ChannelPosition::LowFrequency),
    (4, ChannelPosition::RearLeft),
    (5, ChannelPosition::RearRight),
    (9, ChannelPosition::SideLeft),
    (10, ChannelPosition::SideRight),
];

/// The channel layout `cluster` describes, or `None` when the vocabulary
/// cannot describe it: a position it does not name, positions for fewer or
/// more channels than the cluster carries, or a count with no conventional
/// reading and no positions.
#[must_use]
pub fn channel_map(cluster: Cluster) -> Option<ChannelMap> {
    let channels = usize::from(cluster.channels);
    if channels == 1 {
        return Some(ChannelMap::MONO);
    }
    if cluster.config == 0 {
        return ChannelMap::conventional(cluster.channels);
    }
    if channels == 0 || channels > MAX_CHANNELS {
        return None;
    }
    let named = POSITION_BITS
        .iter()
        .fold(0u32, |bits, &(bit, _)| bits | 1 << bit);
    if cluster.config & !named != 0 || cluster.config.count_ones() as usize != channels {
        return None;
    }
    let mut positions = [ChannelPosition::Mono; MAX_CHANNELS];
    for (slot, (_, position)) in POSITION_BITS
        .iter()
        .filter(|&&(bit, _)| cluster.config & 1 << bit != 0)
        .enumerate()
    {
        positions[slot] = *position;
    }
    ChannelMap::new(&positions[..channels]).ok()
}

#[cfg(test)]
#[path = "streaming_tests.rs"]
mod tests;
