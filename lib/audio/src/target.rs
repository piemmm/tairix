//! `audio:` references naming a sink or a source (`plans/ALIAS.md`): the
//! spelling a program is told which device to open a stream on.
//!
//! `audio:sink/default` and `audio:source/default` are the policy-selected
//! defaults, which the stream ABI spells as device zero; `audio:sink/<id>`
//! names the device `Enumerate` reported with that identity for this boot;
//! `audio:sink/<location>` names a device by where it is, across boots — the
//! spelling persistent configuration keeps. Nothing else is an audio target:
//! a guard, a facet or a query names no device the service has, so it is
//! refused rather than ignored.

use core::fmt;
use core::num::NonZeroU32;

use tairix_abi::audio::{AudioDeviceDescriptor, AudioLocation};
use tairix_abi::driver::audio::StreamDirection;
use tairix_resref::{KnownNamespace, RefError};

/// Which device of a direction a stream is opened on.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum AudioDevice {
    /// Whatever the machine's policy selects at open.
    Default,
    /// The device `Enumerate` reported with this identity, this boot.
    Id(NonZeroU32),
    /// The device at this location, whenever it is there.
    At(AudioLocation),
}

/// A sink or a source, as an `audio:` reference names it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct AudioTarget {
    /// Whether it plays or records.
    pub direction: StreamDirection,
    /// Which device.
    pub device: AudioDevice,
}

/// Why text is not an audio target.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TargetError {
    /// Not a resource reference at all.
    Malformed(RefError),
    /// A reference in another namespace.
    NotAudio,
    /// An `audio:` reference that names no sink or source.
    NoSuchTarget,
}

impl fmt::Display for TargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(_) => f.write_str("not a resource reference"),
            Self::NotAudio => f.write_str("not an audio: reference"),
            Self::NoSuchTarget => f.write_str("names no sink or source"),
        }
    }
}

impl AudioTarget {
    /// The machine's default sink.
    pub const DEFAULT_SINK: Self = Self {
        direction: StreamDirection::Playback,
        device: AudioDevice::Default,
    };

    /// Read `text` as an audio target.
    ///
    /// # Errors
    ///
    /// [`TargetError`] for anything but `audio:sink/…` or `audio:source/…`
    /// naming `default` or a device identity in canonical decimal.
    pub fn parse(text: &str) -> Result<Self, TargetError> {
        let reference = tairix_resref::parse(text).map_err(TargetError::Malformed)?;
        if reference.namespace().known() != Some(KnownNamespace::Audio) {
            return Err(TargetError::NotAudio);
        }
        if reference.guard().is_some()
            || reference.facet().is_some()
            || !reference.params().is_empty()
        {
            return Err(TargetError::NoSuchTarget);
        }
        let [kind, device] = reference.selector() else {
            return Err(TargetError::NoSuchTarget);
        };
        let direction = match kind.as_str() {
            "sink" => StreamDirection::Playback,
            "source" => StreamDirection::Capture,
            _ => return Err(TargetError::NoSuchTarget),
        };
        let device = match device.as_str() {
            "default" => AudioDevice::Default,
            other => match AudioLocation::parse(other) {
                Ok(place) => AudioDevice::At(place),
                Err(_) => AudioDevice::Id(canonical_id(other).ok_or(TargetError::NoSuchTarget)?),
            },
        };
        Ok(Self { direction, device })
    }

    /// The target naming the device `descriptor` describes by its identity
    /// for this boot.
    #[must_use]
    pub fn of(descriptor: &AudioDeviceDescriptor) -> Self {
        Self {
            direction: descriptor.direction,
            device: NonZeroU32::new(descriptor.device_id)
                .map_or(AudioDevice::Default, AudioDevice::Id),
        }
    }

    /// The target naming the device `descriptor` describes by where it is:
    /// the one to keep, because it names the same device next boot.
    #[must_use]
    pub fn at(descriptor: &AudioDeviceDescriptor) -> Self {
        Self {
            direction: descriptor.direction,
            device: AudioDevice::At(descriptor.location),
        }
    }

    /// The device among `devices` this target names now: its direction's
    /// default, the one with its identity, or the one at its location.
    #[must_use]
    pub fn resolve(self, devices: &[AudioDeviceDescriptor]) -> Option<&AudioDeviceDescriptor> {
        devices
            .iter()
            .filter(|device| device.direction == self.direction)
            .find(|device| match self.device {
                AudioDevice::Default => device.default.is_default(),
                AudioDevice::Id(id) => device.device_id == id.get(),
                AudioDevice::At(place) => device.location == place,
            })
    }

    /// The `OpenParams::device_id` a stream on this target opens with, where
    /// the target says it without asking the service: [`None`] for a
    /// location, which only [`resolve`](Self::resolve) turns into an
    /// identity.
    #[must_use]
    pub const fn device_id(self) -> Option<u32> {
        match self.device {
            AudioDevice::Default => Some(0),
            AudioDevice::Id(id) => Some(id.get()),
            AudioDevice::At(_) => None,
        }
    }
}

impl fmt::Display for AudioTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.direction {
            StreamDirection::Playback => "sink",
            StreamDirection::Capture => "source",
        };
        match self.device {
            AudioDevice::Default => write!(f, "audio:{kind}/default"),
            AudioDevice::Id(id) => write!(f, "audio:{kind}/{id}"),
            AudioDevice::At(place) => write!(f, "audio:{kind}/{place}"),
        }
    }
}

/// A nonzero identity spelled with no sign and no leading zero, so each
/// device has exactly one spelling.
fn canonical_id(digits: &str) -> Option<NonZeroU32> {
    let bytes = digits.as_bytes();
    if bytes
        .first()
        .is_none_or(|first| !(b'1'..=b'9').contains(first))
        || !bytes.iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
#[path = "target_tests.rs"]
mod tests;
