//! The audio function's entity graph, as its control interface's
//! class-specific descriptors state it (USB Audio 1.0 §4.3.2, 2.0 §4.7).
//!
//! Every entity names its sources, so a signal path is found by walking back
//! from one end toward the other. Each walk visits an entity at most once, so
//! a cyclic graph a hostile device describes ends rather than spins.

use alloc::vec::Vec;

use tairix_abi::DriverError;
use tairix_usb::descriptor::{
    descriptors, Malformed, DESC_TYPE_INTERFACE, INTERFACE_DESCRIPTOR_LEN,
};

/// `bDescriptorType` of a class-specific interface descriptor.
pub const CS_INTERFACE: u8 = 0x24;

/// `wTerminalType` of the USB streaming terminal, the host's end of a path.
pub const TERMINAL_USB_STREAMING: u16 = 0x0101;

/// Which revision of the class the function implements.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Version {
    /// USB Audio 1.0: rates on each streaming format, set per endpoint.
    One,
    /// USB Audio 2.0: rates on clock entities, set per clock source.
    Two,
}

/// A channel cluster: how many channels, and the spatial positions stated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cluster {
    /// `bNrChannels`.
    pub channels: u8,
    /// `wChannelConfig` / `bmChannelConfig`: bit `n` set for the `n`th
    /// predefined position, those channels first in bit order.
    pub config: u32,
}

/// What a feature unit lets the host set on one channel.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FeatureControls {
    /// Mute.
    pub mute: bool,
    /// Volume.
    pub volume: bool,
}

impl FeatureControls {
    /// A version 1.0 control bitmap: bit 0 mute, bit 1 volume.
    const fn from_v1(bits: u32) -> Self {
        Self {
            mute: bits & 0b01 != 0,
            volume: bits & 0b10 != 0,
        }
    }

    /// A version 2.0 control bitmap, two bits a control: only a control the
    /// host may program (`0b11`) is one it can set.
    const fn from_v2(bits: u32) -> Self {
        Self {
            mute: bits & 0b11 == 0b11,
            volume: (bits >> 2) & 0b11 == 0b11,
        }
    }
}

/// One entity of the function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entity {
    /// `bTerminalID`, `bUnitID` or `bClockID`.
    pub id: u8,
    /// What it is.
    pub kind: EntityKind,
}

/// What an entity is, and the links and controls the driver uses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntityKind {
    /// A signal enters the function here.
    InputTerminal {
        /// `wTerminalType`.
        terminal_type: u16,
        /// The cluster it emits.
        cluster: Cluster,
        /// Version 2.0's clock entity; `0` in 1.0.
        clock: u8,
    },
    /// A signal leaves the function here.
    OutputTerminal {
        /// `wTerminalType`.
        terminal_type: u16,
        /// `bSourceID`.
        source: u8,
        /// Version 2.0's clock entity; `0` in 1.0.
        clock: u8,
    },
    /// Volume and mute, among others.
    Feature {
        /// `bSourceID`.
        source: u8,
        /// The controls on the master channel.
        master: FeatureControls,
        /// The controls on each logical channel, from channel 1.
        channels: Vec<FeatureControls>,
    },
    /// A mixer, selector, processing, extension, effect or rate-converter
    /// unit: on a signal path, offering nothing the driver sets.
    Unit {
        /// Its input pins' sources.
        sources: Vec<u8>,
    },
    /// A clock the function runs a sampling frequency from.
    ClockSource {
        /// The host may set its frequency.
        programmable: bool,
        /// The host may read whether it is valid.
        validity: bool,
    },
    /// A choice among clocks.
    ClockSelector {
        /// Its input pins' sources, pin 1 first.
        sources: Vec<u8>,
        /// The host may choose the pin.
        programmable: bool,
    },
    /// A clock derived from another by a ratio.
    ClockMultiplier {
        /// `bCSourceID`.
        source: u8,
    },
}

impl EntityKind {
    /// The entities a signal arrives at this one from.
    fn signal_sources(&self) -> &[u8] {
        match self {
            Self::OutputTerminal { source, .. } | Self::Feature { source, .. } => {
                core::slice::from_ref(source)
            }
            Self::Unit { sources } => sources,
            _ => &[],
        }
    }
}

/// Every entity of one function, in descriptor order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Topology {
    /// The revision its header states.
    pub version: Version,
    /// The interfaces a 1.0 header lists as its streaming interfaces;
    /// empty for 2.0, whose association descriptor groups them instead.
    pub streaming: Vec<u8>,
    entities: Vec<Entity>,
}

/// The descriptor subtypes both revisions share.
mod subtype {
    pub const HEADER: u8 = 0x01;
    pub const INPUT_TERMINAL: u8 = 0x02;
    pub const OUTPUT_TERMINAL: u8 = 0x03;
    pub const MIXER_UNIT: u8 = 0x04;
    pub const SELECTOR_UNIT: u8 = 0x05;
    pub const FEATURE_UNIT: u8 = 0x06;
}

/// Version 1.0's subtypes past those shared.
mod v1 {
    pub const PROCESSING_UNIT: u8 = 0x07;
    pub const EXTENSION_UNIT: u8 = 0x08;
}

/// Version 2.0's subtypes past those shared.
mod v2 {
    pub const EFFECT_UNIT: u8 = 0x07;
    pub const PROCESSING_UNIT: u8 = 0x08;
    pub const EXTENSION_UNIT: u8 = 0x09;
    pub const CLOCK_SOURCE: u8 = 0x0A;
    pub const CLOCK_SELECTOR: u8 = 0x0B;
    pub const CLOCK_MULTIPLIER: u8 = 0x0C;
    pub const SAMPLE_RATE_CONVERTER: u8 = 0x0D;
}

/// `bcdADC` of each revision.
const BCD_ADC_1_0: u16 = 0x0100;
const BCD_ADC_2_0: u16 = 0x0200;

impl Topology {
    /// The topology interface `control`'s default setting states in
    /// configuration descriptor `config`.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] for a malformed configuration, a descriptor
    /// shorter than its subtype requires, no header or two, a revision the
    /// header names that is neither 1.0 nor 2.0, an entity id of zero, or two
    /// entities sharing an id.
    pub fn parse(config: &[u8], control: u8) -> Result<Self, DriverError> {
        let mut version = None;
        let mut streaming = Vec::new();
        let mut entities: Vec<Entity> = Vec::new();
        let header = tairix_usb::descriptor::ConfigurationHeader::decode(config)
            .map_err(|Malformed| DriverError::BadMagic)?;
        let mut inside = false;
        for descriptor in descriptors(config.get(header.length..).unwrap_or_default()) {
            let descriptor = descriptor.map_err(|Malformed| DriverError::BadMagic)?;
            if descriptor[1] == DESC_TYPE_INTERFACE {
                if descriptor.len() < INTERFACE_DESCRIPTOR_LEN {
                    return Err(DriverError::BadMagic);
                }
                inside = descriptor[2] == control && descriptor[3] == 0;
                continue;
            }
            if !inside || descriptor[1] != CS_INTERFACE || descriptor.len() < 3 {
                continue;
            }
            if descriptor[2] == subtype::HEADER {
                if version.is_some() {
                    return Err(DriverError::BadMagic);
                }
                let parsed = parse_header(descriptor)?;
                version = Some(parsed.0);
                streaming = parsed.1;
                continue;
            }
            let version = version.ok_or(DriverError::BadMagic)?;
            let Some(entity) = parse_entity(version, descriptor)? else {
                continue;
            };
            if entity.id == 0 || entities.iter().any(|known| known.id == entity.id) {
                return Err(DriverError::BadMagic);
            }
            entities
                .try_reserve(1)
                .map_err(|_| DriverError::OutOfMemory)?;
            entities.push(entity);
        }
        Ok(Self {
            version: version.ok_or(DriverError::BadMagic)?,
            streaming,
            entities,
        })
    }

    /// The entity `id` names.
    #[must_use]
    pub fn entity(&self, id: u8) -> Option<&Entity> {
        self.entities.iter().find(|entity| entity.id == id)
    }

    /// The signal path from terminal `streaming`, the host's end, to the
    /// function's other end: the ids it passes through, `streaming` first, or
    /// `None` when no path joins it to a terminal of the outside world.
    ///
    /// A playback path runs from an input terminal to an output terminal, so
    /// it is found back from each output terminal in turn; a capture path
    /// ends at `streaming` and is found back from it.
    #[must_use]
    pub fn path_from(&self, streaming: u8) -> Option<Vec<u8>> {
        match &self.entity(streaming)?.kind {
            EntityKind::InputTerminal { .. } => self.entities.iter().find_map(|entity| {
                let EntityKind::OutputTerminal { terminal_type, .. } = entity.kind else {
                    return None;
                };
                if terminal_type == TERMINAL_USB_STREAMING {
                    return None;
                }
                let mut path = self.back(entity.id, &|candidate| candidate.id == streaming)?;
                path.reverse();
                Some(path)
            }),
            EntityKind::OutputTerminal { .. } => self.back(streaming, &|candidate| {
                matches!(candidate.kind, EntityKind::InputTerminal { terminal_type, .. }
                    if terminal_type != TERMINAL_USB_STREAMING)
            }),
            _ => None,
        }
    }

    /// The ids from `from` back along signal sources to the first entity
    /// `found` accepts, `from` first.
    fn back(&self, from: u8, found: &dyn Fn(&Entity) -> bool) -> Option<Vec<u8>> {
        let mut visited = [false; 256];
        let mut path = Vec::new();
        self.descend(from, found, &mut visited, &mut path)
            .then_some(path)
    }

    fn descend(
        &self,
        id: u8,
        found: &dyn Fn(&Entity) -> bool,
        visited: &mut [bool; 256],
        path: &mut Vec<u8>,
    ) -> bool {
        let Some(entity) = self.entity(id) else {
            return false;
        };
        if core::mem::replace(&mut visited[usize::from(id)], true) || path.try_reserve(1).is_err() {
            return false;
        }
        path.push(id);
        if found(entity) {
            return true;
        }
        for &source in entity.kind.signal_sources() {
            if self.descend(source, found, visited, path) {
                return true;
            }
        }
        path.pop();
        false
    }

    /// The clock path from clock entity `clock` to the source it runs from,
    /// `clock` first, choosing pin `pin(selector)` at each selector, or
    /// `None` when it reaches no clock source.
    #[must_use]
    pub fn clock_path(&self, clock: u8, pin: &dyn Fn(u8) -> Option<u8>) -> Option<Vec<u8>> {
        let mut visited = [false; 256];
        let mut path = Vec::new();
        let mut at = clock;
        loop {
            if core::mem::replace(&mut visited[usize::from(at)], true) {
                return None;
            }
            path.try_reserve(1).ok()?;
            path.push(at);
            at = match &self.entity(at)?.kind {
                EntityKind::ClockSource { .. } => return Some(path),
                EntityKind::ClockMultiplier { source } => *source,
                EntityKind::ClockSelector { sources, .. } => {
                    let chosen = pin(at)?;
                    *sources.get(usize::from(chosen.checked_sub(1)?))?
                }
                _ => return None,
            };
        }
    }

    /// Every entity, in descriptor order.
    pub fn entities(&self) -> impl Iterator<Item = &Entity> {
        self.entities.iter()
    }
}

/// A header's revision, and a 1.0 header's streaming interfaces.
fn parse_header(descriptor: &[u8]) -> Result<(Version, Vec<u8>), DriverError> {
    let bcd = u16::from_le_bytes([
        *descriptor.get(3).ok_or(DriverError::BadMagic)?,
        *descriptor.get(4).ok_or(DriverError::BadMagic)?,
    ]);
    match bcd {
        BCD_ADC_1_0 => {
            let count = usize::from(*descriptor.get(7).ok_or(DriverError::BadMagic)?);
            let listed = descriptor.get(8..8 + count).ok_or(DriverError::BadMagic)?;
            let mut interfaces = Vec::new();
            interfaces
                .try_reserve_exact(count)
                .map_err(|_| DriverError::OutOfMemory)?;
            interfaces.extend_from_slice(listed);
            Ok((Version::One, interfaces))
        }
        BCD_ADC_2_0 => Ok((Version::Two, Vec::new())),
        _ => Err(DriverError::BadMagic),
    }
}

/// Bytes `range` of `descriptor`, or a malformation.
fn field(descriptor: &[u8], range: core::ops::Range<usize>) -> Result<&[u8], DriverError> {
    descriptor.get(range).ok_or(DriverError::BadMagic)
}

/// Byte `at` of `descriptor`, or a malformation.
fn byte(descriptor: &[u8], at: usize) -> Result<u8, DriverError> {
    descriptor.get(at).copied().ok_or(DriverError::BadMagic)
}

/// The little-endian `u16` at `at`.
fn word(descriptor: &[u8], at: usize) -> Result<u16, DriverError> {
    let bytes = field(descriptor, at..at + 2)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

/// The little-endian `u32` at `at`.
fn dword(descriptor: &[u8], at: usize) -> Result<u32, DriverError> {
    let bytes = field(descriptor, at..at + 4)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// `count` source ids starting at `at`.
fn sources(descriptor: &[u8], at: usize, count: u8) -> Result<Vec<u8>, DriverError> {
    let listed = field(descriptor, at..at + usize::from(count))?;
    let mut ids = Vec::new();
    ids.try_reserve_exact(listed.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    ids.extend_from_slice(listed);
    Ok(ids)
}

/// One class-specific descriptor as an entity; `None` for a subtype that is
/// none.
fn parse_entity(version: Version, d: &[u8]) -> Result<Option<Entity>, DriverError> {
    let id = byte(d, 3)?;
    let kind = match (version, d[2]) {
        (Version::One, subtype::INPUT_TERMINAL) => EntityKind::InputTerminal {
            terminal_type: word(d, 4)?,
            cluster: Cluster {
                channels: byte(d, 7)?,
                config: u32::from(word(d, 8)?),
            },
            clock: 0,
        },
        (Version::Two, subtype::INPUT_TERMINAL) => EntityKind::InputTerminal {
            terminal_type: word(d, 4)?,
            cluster: Cluster {
                channels: byte(d, 8)?,
                config: dword(d, 9)?,
            },
            clock: byte(d, 7)?,
        },
        (Version::One, subtype::OUTPUT_TERMINAL) => EntityKind::OutputTerminal {
            terminal_type: word(d, 4)?,
            source: byte(d, 7)?,
            clock: 0,
        },
        (Version::Two, subtype::OUTPUT_TERMINAL) => EntityKind::OutputTerminal {
            terminal_type: word(d, 4)?,
            source: byte(d, 7)?,
            clock: byte(d, 8)?,
        },
        (_, subtype::FEATURE_UNIT) => parse_feature(version, d)?,
        (_, subtype::MIXER_UNIT | subtype::SELECTOR_UNIT) => EntityKind::Unit {
            sources: sources(d, 5, byte(d, 4)?)?,
        },
        (Version::One, v1::PROCESSING_UNIT | v1::EXTENSION_UNIT)
        | (Version::Two, v2::PROCESSING_UNIT | v2::EXTENSION_UNIT) => EntityKind::Unit {
            sources: sources(d, 7, byte(d, 6)?)?,
        },
        (Version::Two, v2::EFFECT_UNIT) => EntityKind::Unit {
            sources: sources(d, 6, 1)?,
        },
        (Version::Two, v2::SAMPLE_RATE_CONVERTER) => EntityKind::Unit {
            sources: sources(d, 4, 1)?,
        },
        (Version::Two, v2::CLOCK_SOURCE) => {
            let controls = byte(d, 5)?;
            EntityKind::ClockSource {
                programmable: controls & 0b11 == 0b11,
                validity: (controls >> 2) & 0b01 != 0,
            }
        }
        (Version::Two, v2::CLOCK_SELECTOR) => {
            let pins = byte(d, 4)?;
            let controls = byte(d, 5 + usize::from(pins))?;
            EntityKind::ClockSelector {
                sources: sources(d, 5, pins)?,
                programmable: controls & 0b11 == 0b11,
            }
        }
        (Version::Two, v2::CLOCK_MULTIPLIER) => EntityKind::ClockMultiplier {
            source: byte(d, 4)?,
        },
        _ => return Ok(None),
    };
    Ok(Some(Entity { id, kind }))
}

/// A feature unit: one control bitmap for the master channel, then one for
/// each logical channel.
fn parse_feature(version: Version, d: &[u8]) -> Result<EntityKind, DriverError> {
    let source = byte(d, 4)?;
    // 1.0 states each bitmap's width; 2.0 fixes it at four bytes. Both close
    // with a string index.
    let (first, width) = match version {
        Version::One => (6, usize::from(byte(d, 5)?)),
        Version::Two => (5, 4),
    };
    if width == 0 || width > 4 {
        return Err(DriverError::BadMagic);
    }
    let bitmaps = d
        .len()
        .checked_sub(first + 1)
        .filter(|span| *span >= width && *span % width == 0)
        .ok_or(DriverError::BadMagic)?
        / width;
    let read = |index: usize| -> u32 {
        let at = first + index * width;
        d[at..at + width]
            .iter()
            .rev()
            .fold(0, |bits, &b| (bits << 8) | u32::from(b))
    };
    let decode = |bits| match version {
        Version::One => FeatureControls::from_v1(bits),
        Version::Two => FeatureControls::from_v2(bits),
    };
    let mut channels = Vec::new();
    channels
        .try_reserve_exact(bitmaps - 1)
        .map_err(|_| DriverError::OutOfMemory)?;
    channels.extend((1..bitmaps).map(|index| decode(read(index))));
    Ok(EntityKind::Feature {
        source,
        master: decode(read(0)),
        channels,
    })
}

/// What to call a terminal of `terminal_type` in a user interface.
#[must_use]
pub fn terminal_name(terminal_type: u16) -> &'static str {
    match terminal_type {
        0x0301 => "Speaker",
        0x0302 => "Headphones",
        0x0303 => "Head-mounted display",
        0x0304 => "Desktop speaker",
        0x0305 => "Room speaker",
        0x0306 => "Communication speaker",
        0x0307 => "Low-frequency speaker",
        0x0201 => "Microphone",
        0x0202 => "Desktop microphone",
        0x0203 => "Personal microphone",
        0x0204 => "Omnidirectional microphone",
        0x0205 => "Microphone array",
        0x0206 => "Processing microphone array",
        0x0401 => "Handset",
        0x0402 => "Headset",
        0x0403..=0x0405 => "Speakerphone",
        0x0500..=0x05FF => "Telephone line",
        0x0601 => "Analog connector",
        0x0602 => "Digital interface",
        0x0603 => "Line",
        0x0605 => "S/PDIF",
        0x0606 | 0x0607 => "IEEE 1394",
        _ if terminal_type & 0xFF00 == 0x0300 => "Output",
        _ => "Input",
    }
}

#[cfg(test)]
#[path = "topology_tests.rs"]
mod tests;
