//! The sound walks — the devices, and the streams a principal may see — and
//! the one row each is printed as.
//!
//! The audio service's own records reach a tool through the System
//! Information API, which scopes the streams: a principal's own are open,
//! another's need `CAP_SYSINFO_GLOBAL`. Each walk is the generic
//! [`walk_records`](crate::list) loop.

use alloc::format;
use alloc::string::String;

use tairix_abi::audio::{
    AudioDeviceDescriptor, StreamDescriptor, AUDIO_DEVICE_RECORD_LEN, AUDIO_STREAM_RECORD_LEN,
};
use tairix_abi::driver::audio::StreamDirection;
use tairix_abi::sysinfo::SysinfoQueryId;
use tairix_abi::Errno;

use crate::list::{walk_records, ListError, WalkStep};
use crate::transport::Transport;

/// Records requested per sound page: as many of the wider of the two records
/// as one reply holds, so a walk costs as few calls as it can.
pub const AUDIO_PAGE: u16 =
    tairix_abi::reply_page(if AUDIO_DEVICE_RECORD_LEN > AUDIO_STREAM_RECORD_LEN {
        AUDIO_DEVICE_RECORD_LEN
    } else {
        AUDIO_STREAM_RECORD_LEN
    });

/// Which streams a walk reads.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StreamScope {
    /// The caller's own ([`SysinfoQueryId::SELF_AUDIO_STREAMS`]).
    Own,
    /// Every principal's ([`SysinfoQueryId::GLOBAL_AUDIO_STREAMS`]).
    Every,
}

/// Page through every sound device ([`SysinfoQueryId::AUDIO_DEVICES`]) and
/// hand each decoded [`AudioDeviceDescriptor`] to `sink`, sinks first.
///
/// # Errors
///
/// * [`ListError::Call`] — the transport failed, no audio service answered,
///   or the reply was structurally invalid.
/// * [`ListError::Sink`] — `sink` refused a record; the walk stops there.
pub fn for_each_audio_device(
    transport: &dyn Transport,
    sink: impl FnMut(&AudioDeviceDescriptor) -> Result<WalkStep, Errno>,
) -> Result<(), ListError> {
    walk_records(
        transport,
        SysinfoQueryId::AUDIO_DEVICES,
        AUDIO_DEVICE_RECORD_LEN,
        AUDIO_PAGE,
        AudioDeviceDescriptor::from_le_bytes,
        sink,
    )
}

/// Page through the sound streams `scope` names and hand each decoded
/// [`StreamDescriptor`] to `sink`, ascending by stream.
///
/// # Errors
///
/// As [`for_each_audio_device`], and a refusal of [`StreamScope::Every`] to
/// a caller without `CAP_SYSINFO_GLOBAL`.
pub fn for_each_audio_stream(
    transport: &dyn Transport,
    scope: StreamScope,
    sink: impl FnMut(&StreamDescriptor) -> Result<WalkStep, Errno>,
) -> Result<(), ListError> {
    let query = match scope {
        StreamScope::Own => SysinfoQueryId::SELF_AUDIO_STREAMS,
        StreamScope::Every => SysinfoQueryId::GLOBAL_AUDIO_STREAMS,
    };
    walk_records(
        transport,
        query,
        AUDIO_STREAM_RECORD_LEN,
        AUDIO_PAGE,
        StreamDescriptor::from_le_bytes,
        sink,
    )
}

/// The columns a [`render_audio_device`] row fills.
pub const AUDIO_DEVICE_HEADER: &str =
    "device  id    default  level     muted  clock-hz       lost  location            name";

/// One sound device's row. A device not clocking has no rate to state.
#[must_use]
pub fn render_audio_device(device: &AudioDeviceDescriptor) -> String {
    let kind = match device.direction {
        StreamDirection::Playback => "sink",
        StreamDirection::Capture => "source",
    };
    let clock = if device.clock_millihertz == 0 {
        String::from("-")
    } else {
        format!(
            "{}.{:03}",
            device.clock_millihertz / 1_000,
            device.clock_millihertz % 1_000
        )
    };
    let level = format!("{}", device.level);
    format!(
        "{kind:<6}  {:<4}  {:<7}  {level:<8}  {:<5}  {clock:<13}  {:<4}  {}  {}",
        device.device_id,
        if device.default.is_default() {
            "yes"
        } else {
            "no"
        },
        if device.muted { "yes" } else { "no" },
        device.lost_frames,
        device.location,
        device.name.as_str(),
    )
}

/// The columns a [`render_audio_stream`] row fills.
pub const AUDIO_STREAM_HEADER: &str =
    "stream  device  direction  role           state          position      xruns  uid     pid     app";

/// One sound stream's row.
#[must_use]
pub fn render_audio_stream(stream: &StreamDescriptor) -> String {
    let direction = match stream.direction {
        StreamDirection::Playback => "playback",
        StreamDirection::Capture => "capture",
    };
    format!(
        "{:<6}  {:<6}  {direction:<9}  {:<13}  {:<13}  {:<12}  {:<5}  {:<6}  {:<6}  {}",
        stream.stream_id,
        stream.device_id,
        stream.role.name(),
        stream.state.name(),
        stream.position.get(),
        stream.xruns,
        stream.owner_uid,
        stream.owner_pid,
        stream.owner_app.as_ref().map_or("-", |app| app.as_str()),
    )
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use tairix_abi::audio::{
        AudioGain, AudioLocation, ControlAccess, DefaultChoice, StreamRole, StreamState,
    };
    use tairix_abi::driver::audio::{
        AudioName, ChannelMap, Frames, JackState, Rate, RateSupport, SampleFormats,
    };

    use super::*;

    /// Where each column starts: the first byte, and each byte after a run
    /// of two or more spaces.
    fn column_starts(text: &str) -> Vec<usize> {
        let bytes = text.as_bytes();
        (0..bytes.len())
            .filter(|&at| {
                bytes[at] != b' ' && (at == 0 || (at >= 2 && bytes[at - 2..at] == *b"  "))
            })
            .collect()
    }

    #[test]
    fn each_row_sits_under_its_header() {
        let device = AudioDeviceDescriptor {
            device_id: 3,
            direction: StreamDirection::Capture,
            jack: JackState::Present,
            default: DefaultChoice::Inherited,
            formats: SampleFormats::EMPTY,
            channel_map: ChannelMap::STEREO,
            rates: RateSupport::Continuous {
                min: Rate::HZ_48000,
                max: Rate::HZ_48000,
            },
            gain: None,
            name: AudioName::new("Microphone").expect("a short name"),
            location: AudioLocation::new(0x51e7, 1).expect("a place"),
            level: AudioGain::new(-650).expect("attenuation"),
            muted: false,
            own_level: false,
            access: ControlAccess::Shown,
            clock_millihertz: 47_999_500,
            lost_frames: 12,
        };
        let row = render_audio_device(&device);
        assert_eq!(
            column_starts(&row),
            column_starts(AUDIO_DEVICE_HEADER),
            "{row}"
        );
        assert!(row.starts_with("source  3 "), "{row}");
        assert!(row.contains("47999.500"), "{row}");
        assert!(row.ends_with("00000000000051e7.1  Microphone"), "{row}");

        let stream = StreamDescriptor {
            stream_id: 21,
            device_id: 3,
            direction: StreamDirection::Capture,
            role: StreamRole::Communication,
            state: StreamState::SeatInactive,
            position: Frames::new(480),
            xruns: 1,
            xrun_frames: 64,
            owner_uid: 1_000,
            owner_pid: 77,
            owner_app: None,
        };
        let row = render_audio_stream(&stream);
        assert_eq!(
            column_starts(&row),
            column_starts(AUDIO_STREAM_HEADER),
            "the widest names still fit their columns: {row}"
        );
        assert!(row.contains("  communication  seat-inactive  "), "{row}");
        assert!(
            row.ends_with("  -"),
            "an anonymous stream names no app: {row}"
        );
    }
}
