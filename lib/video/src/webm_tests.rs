//! `WebM` mux and demux tests.
//!
//! Recordings are muxed into a file held in memory and read back. Malformed
//! files are laid out an element at a time with the crate's own field
//! writers, which the first tests hold to the fields they read back.

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_fscopy::Target;
use tairix_fuzzseed::Prng;
use tairix_sound::{InputError, SoundInput, MAX_READ};

use super::ebml::{self, Field, Size, UNKNOWN_SIZE};
use super::{
    id, Codec, DemuxError, Demuxer, Ending, Finished, Frame, Layout, Media, MuxError, Muxer,
    OpusChannels, OpusTrack, Placement, MAX_FRAME_BYTES, MAX_INDEX_POINTS,
};

const MS: u64 = 1_000_000;

const STEREO: OpusTrack = OpusTrack {
    channels: OpusChannels::Stereo,
    pre_skip: 312,
    input_rate: 44_100,
    output_gain: -256,
};

/// A file the muxer writes; past `capacity` bytes a write puts down what fits
/// and is refused, as a full disk refuses it.
#[derive(Default)]
struct Disk {
    bytes: Vec<u8>,
    capacity: Option<usize>,
}

impl Target for Disk {
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Errno> {
        let start = usize::try_from(offset).map_err(|_| Errno::OutOfRange)?;
        let end = start + bytes.len();
        let fits = self.capacity.map_or(end, |capacity| end.min(capacity));
        if fits > start {
            if self.bytes.len() < fits {
                self.bytes.resize(fits, 0);
            }
            self.bytes[start..fits].copy_from_slice(&bytes[..fits - start]);
        }
        if fits < end {
            Err(Errno::NoSpace)
        } else {
            Ok(())
        }
    }

    fn set_len(&mut self, len: u64) -> Result<(), Errno> {
        let len = usize::try_from(len).map_err(|_| Errno::OutOfRange)?;
        self.bytes.resize(len, 0);
        Ok(())
    }
}

/// A frame pushed to the muxer.
#[derive(Clone, Debug)]
struct Pushed {
    video: bool,
    timestamp_ns: u64,
    keyframe: bool,
    payload: Vec<u8>,
}

impl Pushed {
    /// A frame of `len` bytes no other frame shares.
    fn new(rng: &mut Prng, video: bool, timestamp_ns: u64, keyframe: bool, len: usize) -> Self {
        let mut payload = vec![0; len];
        rng.fill(&mut payload);
        Self {
            video,
            timestamp_ns,
            keyframe,
            payload,
        }
    }
}

/// `seconds` of video at irregular times with a keyframe at least every
/// `key_every_ms`, and with `sound`, 20 ms sound packets.
fn recording(seed: u64, seconds: u64, key_every_ms: u64, sound: bool) -> Vec<Pushed> {
    let mut rng = Prng::new(seed);
    let end = seconds * 1000 * MS;
    let mut frames: Vec<Pushed> = Vec::new();
    let (mut at, mut last_key) = (0, 0);
    while at < end {
        let keyframe = frames.is_empty() || at - last_key >= key_every_ms * MS;
        if keyframe {
            last_key = at;
        }
        let len = 1 + rng.below(if keyframe { 2000 } else { 400 });
        frames.push(Pushed::new(&mut rng, true, at, keyframe, len));
        at += 15 * MS + u64::try_from(rng.below(120_000_000)).unwrap();
    }
    if sound {
        for packet in 0..end / (20 * MS) {
            let len = 1 + rng.below(300);
            frames.push(Pushed::new(&mut rng, false, packet * 20 * MS, true, len));
        }
    }
    frames
}

/// The order `frames` reach the muxer: sound as it is captured, video up to
/// `lag_ms` late as its encoding takes, each track in its own order.
fn arrival(frames: &[Pushed], seed: u64, lag_ms: u64) -> Vec<Pushed> {
    let mut rng = Prng::new(seed);
    let lag = usize::try_from(lag_ms * MS).unwrap();
    let mut late = 0;
    let mut timed: Vec<(u64, Pushed)> = Vec::new();
    for frame in frames {
        let at = if frame.video {
            late = u64::max(
                late,
                frame.timestamp_ns + u64::try_from(rng.below(lag)).unwrap(),
            );
            late
        } else {
            frame.timestamp_ns
        };
        timed.push((at, frame.clone()));
    }
    timed.sort_by_key(|(at, _)| *at);
    timed.into_iter().map(|(_, frame)| frame).collect()
}

/// `frames` in the order a file holds them: by timestamp, video first at a
/// tie.
fn in_file_order(frames: &[Pushed]) -> Vec<Pushed> {
    let mut sorted = frames.to_vec();
    sorted.sort_by_key(|frame| (frame.timestamp_ns, !frame.video));
    sorted
}

/// The video keyframes' times among `frames`, to the millisecond.
fn keyframes(frames: &[Pushed]) -> Vec<u64> {
    frames
        .iter()
        .filter(|frame| frame.video && frame.keyframe)
        .map(|frame| frame.timestamp_ns / MS * MS)
        .collect()
}

fn layout(sound: bool, indexed_keyframes: u32) -> Layout {
    Layout {
        width: 320,
        height: 240,
        audio: sound.then_some(STEREO),
        indexed_keyframes,
    }
}

/// Push `frames` in the order given.
fn push_all(muxer: &mut Muxer, frames: &[Pushed], disk: &mut Disk) -> Result<(), MuxError> {
    for frame in frames {
        if frame.video {
            muxer.push_video(disk, frame.timestamp_ns, frame.keyframe, &frame.payload)?;
        } else {
            muxer.push_audio(disk, frame.timestamp_ns, &frame.payload)?;
        }
    }
    Ok(())
}

/// Mux `frames` in the order given into `disk`, ending 20 ms past the last.
fn mux(layout: &Layout, frames: &[Pushed], disk: &mut Disk) -> Result<Finished, MuxError> {
    let mut muxer = Muxer::begin(disk, layout)?;
    push_all(&mut muxer, frames, disk)?;
    let end = frames
        .iter()
        .map(|frame| frame.timestamp_ns)
        .max()
        .unwrap_or(0);
    muxer.finish(disk, end + 20 * MS)
}

/// Every frame `bytes` holds, and the demuxer that read them.
fn demux(bytes: &[u8]) -> (Demuxer, Vec<Frame>) {
    let mut input = bytes;
    let mut demuxer = Demuxer::open(&mut input).expect("opens");
    let mut frames = Vec::new();
    while let Some(frame) = demuxer.next_frame(&mut input).expect("reads") {
        frames.push(frame);
    }
    (demuxer, frames)
}

fn payload_of<'a>(bytes: &'a [u8], frame: &Frame) -> &'a [u8] {
    let start = usize::try_from(frame.offset).unwrap();
    &bytes[start..start + usize::try_from(frame.len).unwrap()]
}

/// Assert `frames`, read from `bytes`, are `expected` to the millisecond.
fn assert_frames(bytes: &[u8], frames: &[Frame], expected: &[Pushed]) {
    assert_eq!(frames.len(), expected.len(), "frames read");
    for (frame, pushed) in frames.iter().zip(expected) {
        assert_eq!(frame.track, if pushed.video { 1 } else { 2 });
        assert_eq!(frame.timestamp_ns, pushed.timestamp_ns / MS * MS);
        assert_eq!(frame.keyframe, pushed.keyframe);
        assert_eq!(frame.discard_padding_ns, 0);
        assert_eq!(payload_of(bytes, frame), &pushed.payload[..]);
    }
}

/// Assert a seek to `target_ns` leads to the latest of `keys` at or before it.
fn assert_seek(bytes: &[u8], demuxer: &mut Demuxer, keys: &[u64], target_ns: u64) {
    let mut input = bytes;
    demuxer.seek(&mut input, target_ns).expect("seeks");
    let frame = demuxer
        .next_frame(&mut input)
        .expect("reads")
        .expect("a frame");
    let latest = keys.iter().rev().find(|&&key| key <= target_ns);
    assert!(frame.track == 1 && frame.keyframe, "a seek to {target_ns}");
    assert_eq!(Some(frame.timestamp_ns), latest.or(keys.first()).copied());
}

/// An element: its ID, and where it starts, its data starts, and it ends.
type Placed = (u32, usize, usize, usize);

/// The elements `bytes[from..to]` holds.
fn children(bytes: &[u8], from: usize, to: usize) -> Vec<Placed> {
    let mut found = Vec::new();
    let mut at = from;
    while at < to {
        let (element, id_len) = ebml::read_id(&bytes[at..], 4).expect("an ID");
        let (size, size_len) = ebml::read_size(&bytes[at + id_len..], 8).expect("a size");
        let data = at + id_len + size_len;
        let end = match size {
            Size::Known(size) => data + usize::try_from(size).unwrap(),
            Size::Unknown => to,
        };
        found.push((element, at, data, end));
        at = end;
    }
    found
}

/// Where a file's Segment data starts, and the elements it holds.
fn segment(bytes: &[u8]) -> (usize, Vec<Placed>) {
    let top = children(bytes, 0, bytes.len());
    let (_, _, data, end) = first(&top, id::SEGMENT);
    (data, children(bytes, data, end))
}

/// The first of `elements` that is a `wanted`.
fn first(elements: &[Placed], wanted: u32) -> Placed {
    *elements
        .iter()
        .find(|placed| placed.0 == wanted)
        .expect("present")
}

#[test]
fn variable_size_integers_read_back_as_written() {
    for len in 1..=8 {
        let widest = (1u64 << (7 * len)) - 2;
        for value in [0, 1, widest / 2, widest] {
            let field = ebml::size_field(value, len).expect("fits");
            assert_eq!(
                ebml::read_size(field.as_slice(), 8),
                Ok((Size::Known(value), len))
            );
        }
        assert!(
            ebml::size_field(widest + 1, len).is_none(),
            "all ones means unknown"
        );
    }
    assert_eq!(ebml::read_size(&UNKNOWN_SIZE, 8), Ok((Size::Unknown, 8)));
    assert_eq!(ebml::read_size(&[0x40], 8), Err(Field::Short));
    assert_eq!(ebml::read_size(&[0x00, 0x81], 8), Err(Field::Bad));
    assert_eq!(
        ebml::read_size(&UNKNOWN_SIZE, 4),
        Err(Field::Bad),
        "wider than allowed"
    );
    let cluster = ebml::id_field(id::CLUSTER);
    assert_eq!(ebml::read_id(cluster.as_slice(), 4), Ok((id::CLUSTER, 4)));
    assert_eq!(ebml::read_id(&[0xEC], 4), Ok((id::VOID, 1)));
    assert_eq!(
        ebml::read_id(&[0x40, 0x7F], 4),
        Ok((0x407F, 2)),
        "127 needs two bytes"
    );
    for refused in [
        &[0x80][..],
        &[0xFF],
        &[0x40, 0x01],
        &[0x40, 0x00],
        &[0x08, 0, 0, 0, 1],
    ] {
        assert_eq!(ebml::read_id(refused, 4), Err(Field::Bad), "{refused:02x?}");
    }
    assert_eq!(ebml::read_signed_vint(&[0xBF]), Ok((0, 1)));
    assert_eq!(ebml::read_signed_vint(&[0x80]), Ok((-63, 1)));
    assert_eq!(ebml::read_signed_vint(&[0x60, 0x00]), Ok((1, 2)));
    assert_eq!(ebml::read_signed_vint(&[0xFF]), Err(Field::Bad));
}

#[test]
fn element_values_read_back_as_written() {
    for value in [0, 1, 0xFF, 0x100, 0x00FF_FFFF, u64::MAX] {
        let field = ebml::uint_field(value);
        assert_eq!(ebml::read_uint(field.as_slice()), Some(value));
        assert_eq!(field.as_slice().len(), ebml::uint_len(value));
    }
    assert_eq!(ebml::uint_len(0), 1);
    assert_eq!(ebml::read_uint(&[]), Some(0));
    assert_eq!(ebml::read_uint(&[0; 9]), None);
    assert_eq!(ebml::read_int(&[0xFF, 0x00]), Some(-256));
    assert_eq!(ebml::read_int(&[0x01, 0x00]), Some(256));
    assert_eq!(ebml::read_int(&[]), Some(0));
    assert_eq!(ebml::read_float(&48_000f32.to_be_bytes()), Some(48_000.0));
    assert_eq!(ebml::read_float(&1.5f64.to_be_bytes()), Some(1.5));
    assert_eq!(ebml::read_float(&[0; 3]), None);
    assert_eq!(
        ebml::read_ascii(&[b'w', b'e', b'b', b'm', 0, 0]),
        Some("webm")
    );
    assert_eq!(
        ebml::read_ascii(&[b'w', 0, b'b']),
        None,
        "text after the padding"
    );
    assert_eq!(
        ebml::read_ascii(&[b'w', 7, b'b']),
        None,
        "a control character"
    );
}

#[test]
fn a_recording_reads_back_frame_for_frame_in_timestamp_order() {
    let frames = recording(1, 25, 10_000, true);
    let mut disk = Disk::default();
    let finished = mux(&layout(true, 16), &arrival(&frames, 2, 300), &mut disk).expect("muxes");
    assert_eq!(finished.index, Placement::Reserved);
    assert!(!finished.cut_short);
    assert_eq!(finished.len, disk.bytes.len() as u64);
    let (demuxer, read) = demux(&disk.bytes);
    assert_frames(&disk.bytes, &read, &in_file_order(&frames));
    assert_eq!(demuxer.ending(), Some(Ending::Complete));
    assert_eq!(demuxer.duration_ns(), Some(finished.duration_ns));
    assert!(demuxer.has_index());
}

#[test]
fn the_tracks_state_vp8_and_opus_as_the_opus_mapping_has_them() {
    let mut disk = Disk::default();
    mux(&layout(true, 4), &recording(3, 1, 10_000, true), &mut disk).expect("muxes");
    let (demuxer, _) = demux(&disk.bytes);
    let [video, sound] = demuxer.tracks() else {
        panic!("two tracks");
    };
    assert_eq!((video.number, video.codec), (1, Codec::Vp8));
    assert_eq!(video.codec_id.as_str(), "V_VP8");
    let size = Media::Video {
        width: 320,
        height: 240,
    };
    assert_eq!(video.media, size);
    assert!(video.codec_private.is_empty());
    let mut head = b"OpusHead".to_vec();
    head.extend_from_slice(&[1, 2]);
    head.extend_from_slice(&312u16.to_le_bytes());
    head.extend_from_slice(&44_100u32.to_le_bytes());
    head.extend_from_slice(&(-256i16).to_le_bytes());
    head.push(0);
    assert_eq!((sound.number, sound.codec), (2, Codec::Opus));
    assert_eq!(sound.codec_id.as_str(), "A_OPUS");
    let rate = Media::Audio {
        rate: 48_000,
        channels: 2,
    };
    assert_eq!(sound.media, rate);
    assert_eq!(sound.codec_private, head);
    assert_eq!(sound.codec_delay_ns, 6_500_000, "312 samples at 48 kHz");
    assert_eq!(sound.seek_pre_roll_ns, 80 * MS);
}

#[test]
fn the_seek_head_names_info_tracks_and_the_index_where_they_are() {
    let mut disk = Disk::default();
    mux(
        &layout(false, 8),
        &recording(4, 21, 10_000, false),
        &mut disk,
    )
    .expect("muxes");
    let bytes = &disk.bytes;
    let (origin, elements) = segment(bytes);
    let (_, _, head_data, head_end) = first(&elements, id::SEEK_HEAD);
    let mut named = Vec::new();
    for (entry, _, data, end) in children(bytes, head_data, head_end) {
        assert_eq!(
            entry,
            id::SEEK,
            "the room kept for the index's entry is filled"
        );
        let fields = children(bytes, data, end);
        let (_, _, target_at, target_end) = first(&fields, id::SEEK_ID);
        let (target, _) = ebml::read_id(&bytes[target_at..target_end], 4).expect("an ID");
        let (_, _, position_at, position_end) = first(&fields, id::SEEK_POSITION);
        let position = ebml::read_uint(&bytes[position_at..position_end]).expect("a position");
        let at = origin + usize::try_from(position).unwrap();
        assert_eq!(
            ebml::read_id(&bytes[at..], 4).map(|(found, _)| found),
            Ok(target)
        );
        named.push(target);
    }
    assert_eq!(named, [id::INFO, id::TRACKS, id::CUES]);
}

#[test]
fn a_seek_lands_on_the_latest_keyframe_at_or_before_its_target() {
    let frames = recording(5, 41, 10_000, true);
    let keys = keyframes(&frames);
    assert!(keys.len() >= 4, "{keys:?}");
    let mut disk = Disk::default();
    mux(&layout(true, 8), &arrival(&frames, 6, 200), &mut disk).expect("muxes");
    let (mut demuxer, _) = demux(&disk.bytes);
    for &key in &keys {
        for target in [key, key + 1, key + 4_000 * MS] {
            assert_seek(&disk.bytes, &mut demuxer, &keys, target);
        }
    }
    assert_seek(&disk.bytes, &mut demuxer, &keys, 0);
    assert_seek(&disk.bytes, &mut demuxer, &keys, u64::MAX);
    assert!(demuxer.has_index());
}

#[test]
fn a_gap_past_a_blocks_reach_opens_a_cluster() {
    let mut rng = Prng::new(7);
    let frames = [
        Pushed::new(&mut rng, true, 0, true, 40),
        Pushed::new(&mut rng, false, 5 * MS, true, 10),
        Pushed::new(&mut rng, false, 40_000 * MS, true, 10),
        Pushed::new(&mut rng, true, 40_001 * MS, false, 30),
        Pushed::new(&mut rng, false, 100_000 * MS, true, 10),
    ];
    let mut disk = Disk::default();
    mux(&layout(true, 2), &frames, &mut disk).expect("muxes");
    let (_, read) = demux(&disk.bytes);
    assert_frames(&disk.bytes, &read, &in_file_order(&frames));
    let (_, elements) = segment(&disk.bytes);
    let clusters = elements
        .iter()
        .filter(|placed| placed.0 == id::CLUSTER)
        .count();
    assert_eq!(
        clusters, 3,
        "a Cluster for each stretch a block's offset reaches"
    );
}

#[test]
fn an_index_outgrowing_its_reservation_follows_the_last_cluster() {
    let frames = recording(8, 61, 10_000, false);
    let keys = keyframes(&frames);
    assert!(keys.len() >= 6, "{keys:?}");
    let mut disk = Disk::default();
    let finished = mux(&layout(false, 2), &frames, &mut disk).expect("muxes");
    assert_eq!(finished.index, Placement::End);
    let (_, elements) = segment(&disk.bytes);
    assert_eq!(elements.last().map(|placed| placed.0), Some(id::CUES));
    let (mut demuxer, read) = demux(&disk.bytes);
    assert_frames(&disk.bytes, &read, &frames);
    assert!(demuxer.has_index());
    for &key in &keys {
        assert_seek(&disk.bytes, &mut demuxer, &keys, key + 10 * MS);
    }
}

#[test]
fn an_index_fitting_nowhere_else_is_thinned_into_the_reservation() {
    let frames = recording(8, 61, 10_000, false);
    let keys = keyframes(&frames);
    let mut whole = Disk::default();
    mux(&layout(false, 2), &frames, &mut whole).expect("muxes");
    let (_, elements) = segment(&whole.bytes);
    let (_, cues_at, _, _) = first(&elements, id::CUES);
    let mut full = Disk {
        bytes: Vec::new(),
        capacity: Some(cues_at),
    };
    let finished = mux(&layout(false, 2), &frames, &mut full).expect("finishes");
    let stride = keys.len().div_ceil(2);
    let thinned = Placement::Thinned {
        stride: u32::try_from(stride).unwrap(),
    };
    assert_eq!(finished.index, thinned);
    assert!(!finished.cut_short, "no frame was lost");
    assert_eq!(full.bytes.len(), cues_at);
    let kept: Vec<u64> = keys.iter().step_by(stride).copied().collect();
    let (mut demuxer, read) = demux(&full.bytes);
    assert_frames(&full.bytes, &read, &frames);
    for &key in &keys {
        assert_seek(&full.bytes, &mut demuxer, &kept, key + MS);
    }
}

#[test]
fn a_reservation_holds_as_many_keyframes_as_it_was_sized_for() {
    let frames = recording(9, 61, 10_000, false);
    let keys = u32::try_from(keyframes(&frames).len()).unwrap();
    let mut disk = Disk::default();
    let finished = mux(&layout(false, keys), &frames, &mut disk).expect("muxes");
    assert_eq!(finished.index, Placement::Reserved);
    let (_, elements) = segment(&disk.bytes);
    let cues = first(&elements, id::CUES);
    let cluster = first(&elements, id::CLUSTER);
    assert!(
        cues.1 < cluster.1,
        "the index is ahead of the first Cluster"
    );
}

#[test]
fn a_truncated_file_yields_every_frame_written_whole() {
    let frames = recording(10, 12, 4_000, true);
    let mut disk = Disk::default();
    mux(&layout(true, 8), &arrival(&frames, 11, 200), &mut disk).expect("muxes");
    let (_, whole) = demux(&disk.bytes);
    let (_, elements) = segment(&disk.bytes);
    let (_, body, _, _) = first(&elements, id::CLUSTER);
    for cut in (body..disk.bytes.len()).step_by(331) {
        let mut input = &disk.bytes[..cut];
        let mut demuxer = Demuxer::open(&mut input).expect("opens");
        let mut read = Vec::new();
        while let Some(frame) = demuxer.next_frame(&mut input).expect("reads what is whole") {
            read.push(frame);
        }
        let complete = whole
            .iter()
            .take_while(|frame| frame.offset + u64::from(frame.len) <= cut as u64)
            .count();
        assert_eq!(read, whole[..complete], "cut at {cut}");
        assert_eq!(demuxer.ending(), Some(Ending::Truncated), "cut at {cut}");
        demuxer
            .seek(&mut input, u64::MAX)
            .expect("seeks within what is left");
    }
}

#[test]
fn the_file_of_a_writer_that_died_plays_as_far_as_it_got() {
    let frames = recording(12, 25, 10_000, true);
    let mut disk = Disk::default();
    let mut muxer = Muxer::begin(&mut disk, &layout(true, 8)).expect("begins");
    push_all(&mut muxer, &arrival(&frames, 13, 300), &mut disk).expect("takes every frame");
    drop(muxer);
    let (mut demuxer, read) = demux(&disk.bytes);
    let expected = in_file_order(&frames);
    assert!(
        read.len() + 64 > expected.len(),
        "only what was held back is missing"
    );
    assert_frames(&disk.bytes, &read, &expected[..read.len()]);
    assert_eq!(demuxer.ending(), Some(Ending::Complete));
    assert!(!demuxer.has_index());
    let keys = keyframes(&frames);
    for &key in &keys {
        assert_seek(&disk.bytes, &mut demuxer, &keys, key + 2 * MS);
    }
}

#[test]
fn a_full_disk_ends_the_file_at_its_last_block_written_whole() {
    let frames = recording(14, 25, 5_000, true);
    let pushes = arrival(&frames, 15, 200);
    let mut whole = Disk::default();
    mux(&layout(true, 16), &pushes, &mut whole).expect("muxes");
    let mut full = Disk {
        bytes: Vec::new(),
        capacity: Some(whole.bytes.len() * 2 / 3),
    };
    let mut muxer = Muxer::begin(&mut full, &layout(true, 16)).expect("begins");
    let refused = push_all(&mut muxer, &pushes, &mut full).expect_err("the disk fills");
    assert_eq!(refused, MuxError::Sink(Errno::NoSpace));
    assert_eq!(
        muxer.push_audio(&mut full, u64::MAX, &[1]),
        Err(MuxError::Stopped)
    );
    let finished = muxer
        .finish(&mut full, u64::MAX)
        .expect("finishes in what is left");
    assert!(finished.cut_short);
    assert_eq!(finished.index, Placement::Reserved);
    assert_eq!(finished.len, full.bytes.len() as u64);
    let (mut demuxer, read) = demux(&full.bytes);
    let expected = in_file_order(&frames);
    assert!(!read.is_empty() && read.len() < expected.len());
    assert_frames(&full.bytes, &read, &expected[..read.len()]);
    assert_eq!(demuxer.ending(), Some(Ending::Complete));
    let last = read.iter().map(|frame| frame.timestamp_ns).max();
    assert_eq!(demuxer.duration_ns(), last, "the duration of what was kept");
    let keys: Vec<u64> = read
        .iter()
        .filter(|frame| frame.track == 1 && frame.keyframe)
        .map(|frame| frame.timestamp_ns)
        .collect();
    for &key in &keys {
        assert_seek(&full.bytes, &mut demuxer, &keys, key);
    }
}

/// A file served a page at a time: a read meeting a page not yet held answers
/// "not yet" and is given every page it met.
struct Paged<'a> {
    file: &'a [u8],
    held: Vec<bool>,
}

const PAGE: usize = 64;

impl<'a> Paged<'a> {
    fn new(file: &'a [u8]) -> Self {
        Self {
            file,
            held: vec![false; file.len().div_ceil(PAGE)],
        }
    }
}

impl SoundInput for Paged<'_> {
    fn len(&self) -> u64 {
        self.file.len() as u64
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, InputError> {
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(self.file.len());
        let end = start.saturating_add(buf.len()).min(self.file.len());
        let pages = start / PAGE..end.div_ceil(PAGE);
        if !self.held[pages.clone()].iter().all(|&held| held) {
            self.held[pages].fill(true);
            return Err(InputError::Unavailable);
        }
        let mut whole = self.file;
        whole.read_at(offset, buf)
    }
}

/// `call`'s answer once the input holds every page it reads.
fn retried<'f, T>(
    input: &mut Paged<'f>,
    mut call: impl FnMut(&mut Paged<'f>) -> Result<T, DemuxError>,
) -> Result<T, DemuxError> {
    loop {
        match call(input) {
            Err(DemuxError::InputUnavailable) => {}
            answered => return answered,
        }
    }
}

#[test]
fn a_paged_input_yields_what_a_direct_read_does() {
    let frames = recording(16, 21, 10_000, true);
    let mut disk = Disk::default();
    mux(&layout(true, 4), &arrival(&frames, 17, 200), &mut disk).expect("muxes");
    let (_, direct) = demux(&disk.bytes);
    let mut input = Paged::new(&disk.bytes);
    let mut demuxer = retried(&mut input, Demuxer::open).expect("opens");
    let mut read = Vec::new();
    while let Some(frame) = retried(&mut input, |input| demuxer.next_frame(input)).expect("reads") {
        read.push(frame);
    }
    assert_eq!(read, direct);
    let target = 15_000 * MS;
    retried(&mut input, |input| demuxer.seek(input, target)).expect("seeks");
    let landed = retried(&mut input, |input| demuxer.next_frame(input)).expect("reads");
    let mut plain = &disk.bytes[..];
    let (mut reference, _) = demux(&disk.bytes);
    reference.seek(&mut plain, target).expect("seeks");
    assert_eq!(landed, reference.next_frame(&mut plain).expect("reads"));
}

#[test]
fn the_muxer_refuses_what_no_file_states_and_goes_on() {
    let mut disk = Disk::default();
    let flat = Layout {
        width: 0,
        ..layout(false, 1)
    };
    assert_eq!(
        Muxer::begin(&mut disk, &flat).err(),
        Some(MuxError::VideoSize)
    );
    let vast = layout(false, MAX_INDEX_POINTS + 1);
    assert_eq!(
        Muxer::begin(&mut disk, &vast).err(),
        Some(MuxError::IndexTooLarge)
    );
    let mut muxer = Muxer::begin(&mut disk, &layout(false, 1)).expect("begins");
    assert_eq!(
        muxer.push_audio(&mut disk, 0, &[1]),
        Err(MuxError::NoAudioTrack)
    );
    assert_eq!(
        muxer.push_video(&mut disk, 0, false, &[1]),
        Err(MuxError::NoKeyframe)
    );
    assert_eq!(
        muxer.push_video(&mut disk, 0, true, &[]),
        Err(MuxError::EmptyFrame)
    );
    let huge = vec![0; usize::try_from(MAX_FRAME_BYTES).unwrap() + 1];
    assert_eq!(
        muxer.push_video(&mut disk, 0, true, &huge),
        Err(MuxError::TooLarge)
    );
    muxer
        .push_video(&mut disk, 100 * MS, true, &[1, 2, 3])
        .expect("takes a keyframe");
    let early = muxer.push_video(&mut disk, 50 * MS, false, &[4]);
    assert_eq!(early, Err(MuxError::OutOfOrder));
    muxer
        .push_video(&mut disk, 100 * MS, false, &[5])
        .expect("takes a frame at the same time");
    muxer.finish(&mut disk, 0).expect("finishes");
    let (_, read) = demux(&disk.bytes);
    let stamps: Vec<(u64, bool)> = read
        .iter()
        .map(|frame| (frame.timestamp_ns, frame.keyframe))
        .collect();
    assert_eq!(stamps, [(100 * MS, true), (100 * MS, false)]);
}

#[test]
fn a_frame_held_past_the_window_is_written_anyway_in_its_tracks_order() {
    let mut rng = Prng::new(19);
    let mut frames = vec![Pushed::new(&mut rng, true, 0, true, 50)];
    for packet in 0..250 {
        frames.push(Pushed::new(&mut rng, false, packet * 20 * MS, true, 20));
    }
    frames.push(Pushed::new(&mut rng, true, 2_000 * MS, false, 50));
    let mut disk = Disk::default();
    let mut muxer = Muxer::begin(&mut disk, &layout(true, 2)).expect("begins");
    push_all(&mut muxer, &frames, &mut disk).expect("takes every frame");
    muxer.finish(&mut disk, 5_000 * MS).expect("finishes");
    let (_, read) = demux(&disk.bytes);
    assert_eq!(read.len(), frames.len());
    let stamps = |track: u64| -> Vec<u64> {
        read.iter()
            .filter(|frame| frame.track == track)
            .map(|frame| frame.timestamp_ns)
            .collect()
    };
    assert_eq!(stamps(1), [0, 2_000 * MS]);
    let sound = stamps(2);
    assert_eq!(sound.len(), 250);
    assert!(sound.windows(2).all(|pair| pair[0] <= pair[1]));
}

/// An element of `tag` holding `data`, its size in the fewest bytes.
fn element(tag: u32, data: &[u8]) -> Vec<u8> {
    let len = data.len() as u64;
    let width = ebml::size_len(len).expect("stated");
    let mut out = ebml::id_field(tag).as_slice().to_vec();
    out.extend_from_slice(ebml::size_field(len, width).expect("fits").as_slice());
    out.extend_from_slice(data);
    out
}

/// An element of `tag` holding `data`, its size left unknown.
fn unsized_element(tag: u32, data: &[u8]) -> Vec<u8> {
    let mut out = ebml::id_field(tag).as_slice().to_vec();
    out.extend_from_slice(&UNKNOWN_SIZE);
    out.extend_from_slice(data);
    out
}

/// An unsigned integer element.
fn number(tag: u32, value: u64) -> Vec<u8> {
    element(tag, ebml::uint_field(value).as_slice())
}

/// An EBML header naming `doc_type`, a reader of `read_version` of it needed.
fn ebml_header(doc_type: &str, read_version: u64) -> Vec<u8> {
    let fields = [
        number(id::DOC_TYPE_READ_VERSION, read_version),
        element(id::DOC_TYPE, doc_type.as_bytes()),
    ];
    element(id::EBML, &fields.concat())
}

/// Info stating a millisecond timeline.
fn info() -> Vec<u8> {
    element(id::INFO, &number(id::TIMESTAMP_SCALE, MS))
}

/// Track 1's entry: VP8 at 64 by 48 pixels.
fn vp8_entry() -> Vec<u8> {
    let picture = [number(id::PIXEL_WIDTH, 64), number(id::PIXEL_HEIGHT, 48)].concat();
    let fields = [
        number(id::TRACK_NUMBER, 1),
        number(id::TRACK_TYPE, 1),
        element(id::CODEC_ID, b"V_VP8"),
        element(id::VIDEO, &picture),
    ];
    element(id::TRACK_ENTRY, &fields.concat())
}

fn vp8_tracks() -> Vec<u8> {
    element(id::TRACKS, &vp8_entry())
}

/// A `WebM` file whose Segment, of unknown size, holds `ahead` and then
/// `body`.
fn laid_out(ahead: &[Vec<u8>], body: &[u8]) -> Vec<u8> {
    let mut data = ahead.concat();
    data.extend_from_slice(body);
    [ebml_header("webm", 2), unsized_element(id::SEGMENT, &data)].concat()
}

/// A file of VP8 track 1 holding `body` after its Info and Tracks.
fn vp8_file(body: &[u8]) -> Vec<u8> {
    laid_out(&[info(), vp8_tracks()], body)
}

/// A `SimpleBlock` of `track` at `relative` ticks, flagged `flags`, its lacing
/// bytes ahead of `payload`.
fn simple_block(track: u8, relative: i16, flags: u8, lacing: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut data = vec![0x80 | track];
    data.extend_from_slice(&relative.to_be_bytes());
    data.push(flags);
    data.extend_from_slice(lacing);
    data.extend_from_slice(payload);
    element(id::SIMPLE_BLOCK, &data)
}

/// A Cluster timed at `tick` holding `children` after its timestamp.
fn cluster(tick: u64, children: &[Vec<u8>]) -> Vec<u8> {
    let mut data = number(id::CLUSTER_TIMESTAMP, tick);
    data.extend_from_slice(&children.concat());
    element(id::CLUSTER, &data)
}

/// What walking `bytes` to the end of its frames meets: the frames, and the
/// refusal that stopped it where one did.
fn walked(bytes: &[u8]) -> (Vec<Frame>, Option<DemuxError>) {
    let mut input = bytes;
    let mut demuxer = match Demuxer::open(&mut input) {
        Ok(demuxer) => demuxer,
        Err(refused) => return (Vec::new(), Some(refused)),
    };
    let mut frames = Vec::new();
    loop {
        match demuxer.next_frame(&mut input) {
            Ok(Some(frame)) => frames.push(frame),
            Ok(None) => return (frames, None),
            Err(refused) => return (frames, Some(refused)),
        }
    }
}

fn refusal(bytes: &[u8]) -> Option<DemuxError> {
    walked(bytes).1
}

#[test]
fn malformed_variable_size_integers_are_refused() {
    assert_eq!(refusal(&[]), Some(DemuxError::NotWebm));
    assert_eq!(
        refusal(&[0x00, 0x00, 0x00, 0x00]),
        Some(DemuxError::NotWebm)
    );
    for bad_id in [&[0x00, 0x81, 0x00][..], &[0xFF, 0x80], &[0x40, 0x01, 0x80]] {
        let file = vp8_file(&cluster(0, &[bad_id.to_vec()]));
        assert_eq!(refusal(&file), Some(DemuxError::BadId), "{bad_id:02x?}");
    }
    let bad_size = vp8_file(&cluster(0, &[vec![0xEC, 0x00, 0x81]]));
    assert_eq!(refusal(&bad_size), Some(DemuxError::BadSize));
    let narrow = [
        number(id::EBML_MAX_SIZE_LENGTH, 4),
        element(id::DOC_TYPE, b"webm"),
    ];
    let segment = unsized_element(id::SEGMENT, &[info(), vp8_tracks()].concat());
    let file = [element(id::EBML, &narrow.concat()), segment].concat();
    assert_eq!(
        refusal(&file),
        Some(DemuxError::BadSize),
        "wider than the header allows"
    );
}

#[test]
fn oversize_and_overrunning_elements_are_refused() {
    let vast_info = element(id::INFO, &vec![0xEC; MAX_READ + 1]);
    let file = laid_out(&[vast_info, vp8_tracks()], &[]);
    assert_eq!(refusal(&file), Some(DemuxError::TooLarge));
    let frame = vec![0; usize::try_from(MAX_FRAME_BYTES).unwrap() + 1];
    let vast_block = cluster(0, &[simple_block(1, 0, 0x80, &[], &frame)]);
    assert_eq!(refusal(&vp8_file(&vast_block)), Some(DemuxError::TooLarge));
    let mut overrun = number(id::CLUSTER_TIMESTAMP, 0);
    overrun.extend_from_slice(&[0xA3, 0x90, 0x81]);
    let body = [element(id::CLUSTER, &overrun), element(id::VOID, &[0; 32])].concat();
    assert_eq!(refusal(&vp8_file(&body)), Some(DemuxError::Overrun));
}

#[test]
fn nesting_is_refused_or_stepped_over_but_never_recursed_into() {
    let mut bomb = Vec::new();
    for _ in 0..1000 {
        bomb = unsized_element(0xC0, &bomb);
    }
    let file = vp8_file(&cluster(0, &[bomb]));
    assert_eq!(refusal(&file), Some(DemuxError::UnknownSize));
    let mut deep = Vec::new();
    for _ in 0..2000 {
        deep = element(0xC0, &deep);
    }
    let block = simple_block(1, 0, 0x80, &[], &[9; 4]);
    let (read, refused) = walked(&vp8_file(&cluster(0, &[deep, block])));
    assert_eq!(refused, None);
    assert_eq!(read.len(), 1, "the block after the nest is read");
}

#[test]
fn timestamps_past_their_range_are_refused() {
    let late = cluster(u64::MAX, &[simple_block(1, 1, 0x80, &[], &[1])]);
    assert_eq!(
        refusal(&vp8_file(&late)),
        Some(DemuxError::TimestampOutOfRange)
    );
    let early = cluster(0, &[simple_block(1, -1, 0x80, &[], &[1])]);
    assert_eq!(
        refusal(&vp8_file(&early)),
        Some(DemuxError::TimestampOutOfRange)
    );
    let vast = cluster(1 << 50, &[simple_block(1, 0, 0x80, &[], &[1])]);
    let refused = refusal(&vp8_file(&vast));
    assert_eq!(
        refused,
        Some(DemuxError::TimestampOutOfRange),
        "past what nanoseconds hold"
    );
    let unscaled = element(id::INFO, &number(id::TIMESTAMP_SCALE, 0));
    let file = laid_out(&[unscaled, vp8_tracks()], &[]);
    assert_eq!(refusal(&file), Some(DemuxError::BadValue));
}

#[test]
fn laced_blocks_yield_every_frame_they_hold() {
    let frames: [&[u8]; 3] = [&[1; 300], &[2; 2], &[3; 5]];
    let xiph = simple_block(1, 0, 0x82, &[2, 255, 45, 2], &frames.concat());
    let laced: [&[u8]; 3] = [&[4], &[5; 3], &[6; 7]];
    let ebml_laced = simple_block(1, 0, 0x86, &[2, 0x81, 0xC1], &laced.concat());
    let fixed = simple_block(1, 0, 0x84, &[3], &[7; 12]);
    let file = vp8_file(&cluster(0, &[xiph, ebml_laced, fixed]));
    let (read, refused) = walked(&file);
    assert_eq!(refused, None);
    let sizes: Vec<u32> = read.iter().map(|frame| frame.len).collect();
    assert_eq!(sizes, [300, 2, 5, 1, 3, 7, 3, 3, 3, 3]);
    let firsts: Vec<u8> = read
        .iter()
        .map(|frame| payload_of(&file, frame)[0])
        .collect();
    assert_eq!(firsts, [1, 2, 3, 4, 5, 6, 7, 7, 7, 7]);
    assert!(read
        .iter()
        .all(|frame| frame.keyframe && frame.timestamp_ns == 0));
    for (lacing, flags) in [(&[2, 255][..], 0x82), (&[2, 0x81, 0x80][..], 0x86)] {
        let short = vp8_file(&cluster(0, &[simple_block(1, 0, flags, lacing, &[0; 3])]));
        assert_eq!(
            refusal(&short),
            Some(DemuxError::BadLacing),
            "{lacing:02x?}"
        );
    }
    let uneven = vp8_file(&cluster(0, &[simple_block(1, 0, 0x84, &[2], &[0; 10])]));
    assert_eq!(refusal(&uneven), Some(DemuxError::BadLacing));
}

#[test]
fn block_groups_carry_their_keyframe_and_padding() {
    let block = element(id::BLOCK, &[0x81, 0, 5, 0, 9, 9]);
    let referenced = [
        block.clone(),
        element(id::REFERENCE_BLOCK, &[0xFF]),
        element(id::DISCARD_PADDING, &[0x01, 0x00]),
    ];
    let alone = [block.clone(), element(id::DISCARD_PADDING, &[0xFF, 0x00])];
    let groups = [
        element(id::BLOCK_GROUP, &referenced.concat()),
        element(id::BLOCK_GROUP, &alone.concat()),
    ];
    let (read, refused) = walked(&vp8_file(&cluster(10, &groups)));
    assert_eq!(refused, None);
    let seen: Vec<(u64, bool, i64)> = read
        .iter()
        .map(|frame| (frame.timestamp_ns, frame.keyframe, frame.discard_padding_ns))
        .collect();
    assert_eq!(seen, [(15 * MS, false, 256), (15 * MS, true, -256)]);
    let empty = element(id::BLOCK_GROUP, &element(id::REFERENCE_BLOCK, &[1]));
    let file = vp8_file(&cluster(0, &[empty]));
    assert_eq!(refusal(&file), Some(DemuxError::Missing));
    let twice = element(id::BLOCK_GROUP, &[block.clone(), block].concat());
    let file = vp8_file(&cluster(0, &[twice]));
    assert_eq!(refusal(&file), Some(DemuxError::Duplicated));
    let flagged = element(id::BLOCK_GROUP, &element(id::BLOCK, &[0x81, 0, 0, 0x80, 1]));
    let file = vp8_file(&cluster(0, &[flagged]));
    assert_eq!(
        refusal(&file),
        Some(DemuxError::BadValue),
        "a group's block has no keyframe bit"
    );
}

#[test]
fn a_damaged_index_is_refused_once_and_seeks_walk_the_file_after() {
    let frames = recording(18, 31, 10_000, false);
    let keys = keyframes(&frames);
    let mut disk = Disk::default();
    mux(&layout(false, 8), &frames, &mut disk).expect("muxes");
    let bytes = &disk.bytes;
    let (_, elements) = segment(bytes);
    let (_, _, cues, cues_end) = first(&elements, id::CUES);
    let (_, _, point, point_end) = first(&children(bytes, cues, cues_end), id::CUE_POINT);
    let positions = first(&children(bytes, point, point_end), id::CUE_TRACK_POSITIONS);
    let fields = children(bytes, positions.2, positions.3);
    let (_, _, at, end) = first(&fields, id::CUE_CLUSTER_POSITION);
    disk.bytes[at..end].fill(0);
    let (mut demuxer, read) = demux(&disk.bytes);
    assert_frames(&disk.bytes, &read, &frames);
    let mut input = &disk.bytes[..];
    assert_eq!(demuxer.seek(&mut input, 0), Err(DemuxError::BadIndex));
    assert!(!demuxer.has_index());
    for &key in &keys {
        assert_seek(&disk.bytes, &mut demuxer, &keys, key + MS);
    }
}

#[test]
fn misplaced_and_repeated_elements_are_refused() {
    let untimed = element(id::CLUSTER, &simple_block(1, 0, 0x80, &[], &[1]));
    assert_eq!(refusal(&vp8_file(&untimed)), Some(DemuxError::Untimed));
    let twice = cluster(0, &[number(id::CLUSTER_TIMESTAMP, 1)]);
    assert_eq!(refusal(&vp8_file(&twice)), Some(DemuxError::Duplicated));
    let nested = cluster(0, &[info()]);
    assert_eq!(refusal(&vp8_file(&nested)), Some(DemuxError::Misplaced));
    let file = laid_out(&[info(), info(), vp8_tracks()], &[]);
    assert_eq!(refusal(&file), Some(DemuxError::Duplicated));
    assert_eq!(
        refusal(&laid_out(&[info()], &[])),
        Some(DemuxError::Missing)
    );
    let segment = unsized_element(id::SEGMENT, &[info(), vp8_tracks()].concat());
    let matroska = [ebml_header("matroska", 2), segment.clone()].concat();
    assert_eq!(refusal(&matroska), Some(DemuxError::NotWebm));
    let newer = [ebml_header("webm", 5), segment].concat();
    assert_eq!(refusal(&newer), Some(DemuxError::UnsupportedVersion));
}

#[test]
fn tracks_that_do_not_hold_together_are_refused() {
    let undeclared = cluster(0, &[simple_block(3, 0, 0x80, &[], &[1])]);
    assert_eq!(
        refusal(&vp8_file(&undeclared)),
        Some(DemuxError::UndeclaredTrack)
    );
    let entry =
        |fields: &[Vec<u8>]| element(id::TRACKS, &element(id::TRACK_ENTRY, &fields.concat()));
    let bare_opus = entry(&[
        number(id::TRACK_NUMBER, 2),
        number(id::TRACK_TYPE, 2),
        element(id::CODEC_ID, b"A_OPUS"),
    ]);
    let file = laid_out(&[info(), bare_opus], &[]);
    assert_eq!(
        refusal(&file),
        Some(DemuxError::BadTrack),
        "Opus without its header"
    );
    let sounding_vp8 = entry(&[
        number(id::TRACK_NUMBER, 1),
        number(id::TRACK_TYPE, 2),
        element(id::CODEC_ID, b"V_VP8"),
    ]);
    assert_eq!(
        refusal(&laid_out(&[info(), sounding_vp8], &[])),
        Some(DemuxError::BadTrack)
    );
    let nameless = entry(&[number(id::TRACK_TYPE, 1), element(id::CODEC_ID, b"V_VP8")]);
    assert_eq!(
        refusal(&laid_out(&[info(), nameless], &[])),
        Some(DemuxError::Missing)
    );
    let doubled = element(id::TRACKS, &[vp8_entry(), vp8_entry()].concat());
    assert_eq!(
        refusal(&laid_out(&[info(), doubled], &[])),
        Some(DemuxError::Duplicated)
    );
}
