//! Opening a file: its EBML header, its Segment's header, and the elements
//! ahead of the first Cluster.

use alloc::vec::Vec;

use tairix_inline::ArrayString;
use tairix_sound::SoundInput;
use tairix_util::{fallible, mathf};

use super::super::ebml::{self, Size, MAX_ID_LEN, MAX_SIZE_LEN};
use super::super::id;
use super::super::{
    AUDIO_TYPE, DEFAULT_TIMESTAMP_SCALE, DOC_TYPE, MAX_TRACKS, OPUS_CODEC, VIDEO_TYPE, VP8_CODEC,
};
use super::{
    known, once, read_head, slurp, uint, Children, Codec, DemuxError, Media, Opened, Segment,
    Track, Widths,
};

/// The newest EBML reading rules this reader follows.
const NEWEST_EBML_READ: u64 = 1;

/// The newest Matroska version a file may require of its reader: version 4
/// adds only elements this reader interprets or steps over.
const NEWEST_DOC_READ: u64 = 4;

/// Most elements ahead of the Segment, and ahead of the first Cluster.
const MAX_HEAD_ELEMENTS: usize = 64;

/// Matroska's sampling frequency where an audio track states none.
const DEFAULT_RATE: u32 = 8000;

/// 2^64: the first double past every `u64`.
const TWO_TO_THE_64: f64 = 18_446_744_073_709_551_616.0;

/// What the elements ahead of the first Cluster state.
#[derive(Default)]
struct Ahead {
    /// Info's timestamp scale, and its duration in ticks.
    info: Option<(u64, Option<f64>)>,
    tracks: Option<Vec<Track>>,
    /// Where the seek head says the index is.
    seek_cues: Option<u64>,
    /// Where the walk met an index.
    walked_cues: Option<u64>,
}

/// Open the file `input` holds, answering what later calls need and where
/// its index is, where it names or holds one.
pub(super) fn open(
    input: &mut (impl SoundInput + ?Sized),
    scratch: &mut Vec<u8>,
) -> Result<(Opened, Option<u64>), DemuxError> {
    let (widths, header_end) = read_ebml_header(input, scratch)?;
    let segment = read_segment(input, header_end, widths)?;
    let (ahead, body) = read_ahead(input, &segment, widths, scratch)?;
    let (scale, duration) = ahead.info.ok_or(DemuxError::Missing)?;
    let tracks = ahead.tracks.ok_or(DemuxError::Missing)?;
    let duration_ns = duration
        .map(|ticks| nanoseconds(ticks, scale))
        .transpose()?;
    let seek_track = tracks
        .iter()
        .find(|track| matches!(track.media, Media::Video { .. }))
        .or_else(|| tracks.first())
        .map_or(0, |track| track.number);
    let opened = Opened {
        widths,
        segment,
        scale,
        duration_ns,
        tracks,
        body,
        seek_track,
    };
    Ok((opened, ahead.seek_cues.or(ahead.walked_cues)))
}

/// Read the EBML header, answering the widths it allows and where it ends.
fn read_ebml_header(
    input: &mut (impl SoundInput + ?Sized),
    scratch: &mut Vec<u8>,
) -> Result<(Widths, u64), DemuxError> {
    let file_len = input.len();
    let head = match read_head(input, 0, file_len, Widths::WIDEST) {
        Ok(Some(head)) if head.id == id::EBML => head,
        Ok(_) | Err(DemuxError::BadId | DemuxError::BadSize) => return Err(DemuxError::NotWebm),
        Err(refused) => return Err(refused),
    };
    let size = known(head.size)?;
    let mut widths = Widths::WIDEST;
    let mut doc_type = None;
    for child in Children::new(slurp(input, head.len, size, scratch)?, Widths::WIDEST) {
        let (element, value) = child?;
        match element {
            id::EBML_READ_VERSION => supported(uint(value)?, NEWEST_EBML_READ)?,
            id::EBML_MAX_ID_LENGTH => widths.id = width_bound(value, MAX_ID_LEN)?,
            id::EBML_MAX_SIZE_LENGTH => widths.size = width_bound(value, MAX_SIZE_LEN)?,
            id::DOC_TYPE => once(&mut doc_type, text(value)? == DOC_TYPE)?,
            id::DOC_TYPE_READ_VERSION => supported(uint(value)?, NEWEST_DOC_READ)?,
            _ => {}
        }
    }
    if doc_type != Some(true) {
        return Err(DemuxError::NotWebm);
    }
    Ok((widths, head.len + size))
}

/// Refuse a version newer than `newest`.
fn supported(version: u64, newest: u64) -> Result<(), DemuxError> {
    if version > newest {
        Err(DemuxError::UnsupportedVersion)
    } else {
        Ok(())
    }
}

/// The widest field an EBML header allows, refused past what this reader
/// reads.
fn width_bound(value: &[u8], widest: usize) -> Result<usize, DemuxError> {
    match usize::try_from(uint(value)?) {
        Ok(0) => Err(DemuxError::BadValue),
        Ok(width) if width <= widest => Ok(width),
        _ => Err(DemuxError::UnsupportedVersion),
    }
}

/// A string element's text.
fn text(value: &[u8]) -> Result<&str, DemuxError> {
    ebml::read_ascii(value).ok_or(DemuxError::BadValue)
}

/// Read the Segment's header from `at`, stepping over a `Void` or `CRC-32`
/// element ahead of it.
fn read_segment(
    input: &mut (impl SoundInput + ?Sized),
    mut at: u64,
    widths: Widths,
) -> Result<Segment, DemuxError> {
    let file_len = input.len();
    for _ in 0..MAX_HEAD_ELEMENTS {
        let head = read_head(input, at, file_len, widths)?.ok_or(DemuxError::Truncated)?;
        let data = at.checked_add(head.len).ok_or(DemuxError::Overrun)?;
        match head.id {
            id::SEGMENT => {
                let end = match head.size {
                    Size::Known(size) => Some(data.checked_add(size).ok_or(DemuxError::Overrun)?),
                    Size::Unknown => None,
                };
                return Ok(Segment { data, end });
            }
            id::VOID | id::CRC32 => {
                at = data
                    .checked_add(known(head.size)?)
                    .ok_or(DemuxError::Overrun)?;
            }
            _ => return Err(DemuxError::NotWebm),
        }
    }
    Err(DemuxError::TooMany)
}

/// Walk the Segment's elements ahead of its first Cluster, answering what
/// they state and where the walk stopped.
fn read_ahead(
    input: &mut (impl SoundInput + ?Sized),
    segment: &Segment,
    widths: Widths,
    scratch: &mut Vec<u8>,
) -> Result<(Ahead, u64), DemuxError> {
    let file_len = input.len();
    let limit = segment.limit(file_len);
    let mut ahead = Ahead::default();
    let mut at = segment.data;
    for _ in 0..MAX_HEAD_ELEMENTS {
        let found = if at < limit {
            read_head(input, at, limit, widths)?
        } else {
            None
        };
        let Some(head) =
            found.filter(|head| !matches!(head.id, id::CLUSTER | id::EBML | id::SEGMENT))
        else {
            return Ok((ahead, at));
        };
        let data = at.checked_add(head.len).ok_or(DemuxError::Overrun)?;
        let size = known(head.size)?;
        let end = data.checked_add(size).ok_or(DemuxError::Overrun)?;
        if segment.end.is_some_and(|segment_end| end > segment_end) {
            return Err(DemuxError::Overrun);
        }
        match head.id {
            id::INFO => {
                let info = read_info(slurp(input, data, size, scratch)?, widths)?;
                once(&mut ahead.info, info)?;
            }
            id::TRACKS => {
                let tracks = read_tracks(slurp(input, data, size, scratch)?, widths)?;
                once(&mut ahead.tracks, tracks)?;
            }
            id::SEEK_HEAD if ahead.seek_cues.is_none() => {
                ahead.seek_cues = seek_cues(slurp(input, data, size, scratch)?, widths, segment)?;
            }
            id::CUES => {
                ahead.walked_cues.get_or_insert(at);
            }
            _ => {}
        }
        if end > file_len {
            return Ok((ahead, at));
        }
        at = end;
    }
    Err(DemuxError::TooMany)
}

/// The timeline's tick in nanoseconds and the duration in ticks that Info
/// states.
fn read_info(data: &[u8], widths: Widths) -> Result<(u64, Option<f64>), DemuxError> {
    let mut scale = None;
    let mut duration = None;
    for child in Children::new(data, widths) {
        let (element, value) = child?;
        match element {
            id::TIMESTAMP_SCALE => once(&mut scale, uint(value)?)?,
            id::DURATION => {
                let ticks = ebml::read_float(value).ok_or(DemuxError::BadValue)?;
                once(&mut duration, ticks)?;
            }
            _ => {}
        }
    }
    let scale = scale.unwrap_or(DEFAULT_TIMESTAMP_SCALE);
    if scale == 0 {
        return Err(DemuxError::BadValue);
    }
    Ok((scale, duration))
}

/// `ticks` of `scale` nanoseconds each, to the nearest nanosecond.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "a duration rounded to whole nanoseconds, held below 2^64 first"
)]
fn nanoseconds(ticks: f64, scale: u64) -> Result<u64, DemuxError> {
    if !ticks.is_finite() || ticks < 0.0 {
        return Err(DemuxError::BadValue);
    }
    let ns = mathf::round(ticks * scale as f64);
    if ns < TWO_TO_THE_64 {
        Ok(ns as u64)
    } else {
        Err(DemuxError::TimestampOutOfRange)
    }
}

/// The tracks the Tracks element declares, each number declared once.
fn read_tracks(data: &[u8], widths: Widths) -> Result<Vec<Track>, DemuxError> {
    let mut tracks: Vec<Track> = Vec::new();
    for child in Children::new(data, widths) {
        let (element, value) = child?;
        if element != id::TRACK_ENTRY {
            continue;
        }
        if tracks.len() == MAX_TRACKS {
            return Err(DemuxError::TooMany);
        }
        let track = read_track(value, widths)?;
        if tracks.iter().any(|known| known.number == track.number) {
            return Err(DemuxError::Duplicated);
        }
        if !fallible::push(&mut tracks, track) {
            return Err(DemuxError::OutOfMemory);
        }
    }
    if tracks.is_empty() {
        return Err(DemuxError::Missing);
    }
    Ok(tracks)
}

/// What a track entry's children state.
#[derive(Default)]
struct Entry<'a> {
    number: Option<u64>,
    kind: Option<u64>,
    codec_id: Option<&'a str>,
    private: Option<&'a [u8]>,
    delay: Option<u64>,
    pre_roll: Option<u64>,
    video: Option<Media>,
    audio: Option<Media>,
    /// Its frames are compressed or encrypted.
    encoded: bool,
}

impl Entry<'_> {
    /// The track the entry describes, refused where it does not hold together.
    fn track(self) -> Result<Track, DemuxError> {
        let number = self.number.ok_or(DemuxError::Missing)?;
        let kind = self.kind.ok_or(DemuxError::Missing)?;
        let codec_id = self.codec_id.ok_or(DemuxError::Missing)?;
        let kind = u8::try_from(kind)
            .ok()
            .filter(|&kind| kind > 0)
            .ok_or(DemuxError::BadValue)?;
        if number == 0 {
            return Err(DemuxError::BadValue);
        }
        let media = match kind {
            VIDEO_TYPE => self.video.ok_or(DemuxError::Missing)?,
            AUDIO_TYPE => self.audio.unwrap_or(Media::Audio {
                rate: DEFAULT_RATE,
                channels: 1,
            }),
            other => Media::Other(other),
        };
        let private = self.private.unwrap_or_default();
        let named = match (codec_id, media) {
            (VP8_CODEC, Media::Video { .. }) => Codec::Vp8,
            (OPUS_CODEC, Media::Audio { .. }) if !private.is_empty() => Codec::Opus,
            (VP8_CODEC | OPUS_CODEC, _) => return Err(DemuxError::BadTrack),
            _ => Codec::Other,
        };
        let mut codec_private = Vec::new();
        if !fallible::reserve(&mut codec_private, private.len()) {
            return Err(DemuxError::OutOfMemory);
        }
        codec_private.extend_from_slice(private);
        Ok(Track {
            number,
            codec: if self.encoded { Codec::Other } else { named },
            codec_id: ArrayString::from_str_truncating(codec_id),
            media,
            codec_private,
            codec_delay_ns: self.delay.unwrap_or(0),
            seek_pre_roll_ns: self.pre_roll.unwrap_or(0),
        })
    }
}

/// The track one track entry declares.
fn read_track(data: &[u8], widths: Widths) -> Result<Track, DemuxError> {
    let mut entry = Entry::default();
    for child in Children::new(data, widths) {
        let (element, value) = child?;
        match element {
            id::TRACK_NUMBER => once(&mut entry.number, uint(value)?)?,
            id::TRACK_TYPE => once(&mut entry.kind, uint(value)?)?,
            id::CODEC_ID => once(&mut entry.codec_id, text(value)?)?,
            id::CODEC_PRIVATE => once(&mut entry.private, value)?,
            id::CODEC_DELAY => once(&mut entry.delay, uint(value)?)?,
            id::SEEK_PRE_ROLL => once(&mut entry.pre_roll, uint(value)?)?,
            id::VIDEO => once(&mut entry.video, read_video(value, widths)?)?,
            id::AUDIO => once(&mut entry.audio, read_audio(value, widths)?)?,
            id::CONTENT_ENCODINGS => entry.encoded = true,
            _ => {}
        }
    }
    entry.track()
}

/// The picture size a video track states.
fn read_video(data: &[u8], widths: Widths) -> Result<Media, DemuxError> {
    let mut width = None;
    let mut height = None;
    for child in Children::new(data, widths) {
        let (element, value) = child?;
        match element {
            id::PIXEL_WIDTH => once(&mut width, dimension(value)?)?,
            id::PIXEL_HEIGHT => once(&mut height, dimension(value)?)?,
            _ => {}
        }
    }
    Ok(Media::Video {
        width: width.ok_or(DemuxError::Missing)?,
        height: height.ok_or(DemuxError::Missing)?,
    })
}

/// A picture's width or height: more than zero pixels, and within `u32`.
fn dimension(value: &[u8]) -> Result<u32, DemuxError> {
    u32::try_from(uint(value)?)
        .ok()
        .filter(|&side| side > 0)
        .ok_or(DemuxError::BadValue)
}

/// The rate and channels an audio track states, Matroska's defaults where it
/// states none.
fn read_audio(data: &[u8], widths: Widths) -> Result<Media, DemuxError> {
    let mut rate = None;
    let mut channels = None;
    for child in Children::new(data, widths) {
        let (element, value) = child?;
        match element {
            id::SAMPLING_FREQUENCY => {
                let frequency = ebml::read_float(value).ok_or(DemuxError::BadValue)?;
                once(&mut rate, hertz(frequency)?)?;
            }
            id::CHANNELS => {
                let count = u8::try_from(uint(value)?)
                    .ok()
                    .filter(|&count| count > 0)
                    .ok_or(DemuxError::BadValue)?;
                once(&mut channels, count)?;
            }
            _ => {}
        }
    }
    Ok(Media::Audio {
        rate: rate.unwrap_or(DEFAULT_RATE),
        channels: channels.unwrap_or(1),
    })
}

/// A sampling frequency as whole hertz.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a whole number of hertz, held within `u32` first"
)]
fn hertz(rate: f64) -> Result<u32, DemuxError> {
    let whole = (1.0..=f64::from(u32::MAX)).contains(&rate) && mathf::floor(rate) >= rate;
    if whole {
        Ok(rate as u32)
    } else {
        Err(DemuxError::BadValue)
    }
}

/// Where the seek head says the index is, if it names one.
fn seek_cues(data: &[u8], widths: Widths, segment: &Segment) -> Result<Option<u64>, DemuxError> {
    for entry in Children::new(data, widths) {
        let (element, fields) = entry?;
        if element != id::SEEK {
            continue;
        }
        let mut target = None;
        let mut position = None;
        for field in Children::new(fields, widths) {
            let (field_id, value) = field?;
            match field_id {
                id::SEEK_ID => once(&mut target, seek_id(value)?)?,
                id::SEEK_POSITION => once(&mut position, uint(value)?)?,
                _ => {}
            }
        }
        if target == Some(id::CUES) {
            let position = position.ok_or(DemuxError::Missing)?;
            return segment
                .data
                .checked_add(position)
                .map(Some)
                .ok_or(DemuxError::BadValue);
        }
    }
    Ok(None)
}

/// The element ID a seek entry names: one ID filling its data.
fn seek_id(value: &[u8]) -> Result<u32, DemuxError> {
    match ebml::read_id(value, MAX_ID_LEN) {
        Ok((target, len)) if len == value.len() => Ok(target),
        _ => Err(DemuxError::BadValue),
    }
}
