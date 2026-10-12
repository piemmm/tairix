//! Fuzz harness for the sound-file decoders: untrusted AU, WAVE, FLAC, Ogg
//! and MPEG audio bytes.
//!
//! Invariants, for every input:
//!
//! 1. Opening, probing, seeking and reading every block never panics.
//! 2. A stream never writes more frames than it states, and every block is
//!    whole frames.
//! 3. A decode holds no more memory than its block, the format's fixed
//!    bounds and the metadata limits allow.
//! 4. Decoding through an input that answers "not yet" at random, each call
//!    asked again once the bytes are supplied, yields exactly what a direct
//!    decode does: a call that meets missing bytes changes nothing.
//! 5. The metadata a decode keeps is within the limits.
//!
//! 6. A FLAC stream the encoder wrote, unmutated, decodes to exactly the
//!    samples it was given, natively and in Ogg.
//! 7. Every link of a chained file holds the invariants above, read link
//!    after link as a player reads them.
//!
//! Inputs are arbitrary bytes, and structured AU, WAVE, FLAC and MPEG audio
//! files — every encoding and codec the decoders name, chunks in random
//! order, every FLAC construct the encoder's core can be told to emit, MPEG
//! streams in every layer and family that lean on Layer III's reservoir —
//! and libvorbis's and libopus's own streams chained, interleaved and
//! corrupted beneath resealed pages, then mutated.

#![allow(
    clippy::cast_possible_truncation,
    reason = "the generators narrow lengths they keep small"
)]

use tairix_fuzzseed::meter::{metered, Metered};
use tairix_fuzzseed::Prng;
use tairix_sound::flac_encode::{
    BlockCode, Blocking, Codes, Frame, Options, Params, Partition, Predictor, RateCode, Residual,
    Stereo, Subframe, Writer,
};
use tairix_sound::{
    probe, sniff, DecodeError, DecodeLimits, Encoding, InputError, PcmSource, SoundFormat,
    SoundInput,
};

#[global_allocator]
static ALLOC: Metered = Metered;

/// Fixed-iteration sweep run when no budget is set.
const SMOKE_ITERATIONS: u64 = 300;

const LIMITS: DecodeLimits = DecodeLimits::new(8, 1024, 64);

/// Frames a block holds.
const BLOCK_FRAMES: usize = 512;

/// The most a decode may hold: two blocks of the widest frame, two ADPCM
/// blocks of the largest alignment, the format chunk twice over, the
/// metadata limits with room for their containers, and slack for the
/// allocator's rounding.
const PEAK_BOUND: usize =
    2 * BLOCK_FRAMES * 32 + 2 * 65_535 + 2 * (18 + 65_535) + 4 * 1024 + 64 * 32 + 65_536;

/// The most a FLAC decode may hold: the samples of the largest block in
/// every channel, a window, a packet and two Ogg pages no larger than the
/// file, and the metadata limits with slack.
const fn flac_peak(file: usize) -> usize {
    65_535 * 8 * 8 + 3 * file + 2 * (27 + 255 + 255 * 255) + 4 * 1024 + 65_536
}

/// The most a Vorbis decode may hold: a setup at its bounds — 2^18 entries,
/// each a length while read and a packed codeword after, 2^19 vector values,
/// a fast lookup for each of 256 books, two Bark maps for each of 64 floors
/// — the setup packet and the largest audio packet, the block buffers of
/// eight channels, two pages, and the metadata limits with slack.
const fn vorbis_peak(file: usize) -> usize {
    (1 << 18) * 9
        + (1 << 19) * 4
        + 256 * 4096
        + 64 * 2 * 4096 * 2
        + 256 * 1024
        + 4096 * 8 * 36
        + 8 * 4096 * 4 * 4
        + 3 * file
        + 2 * (27 + 255 + 255 * 255)
        + 4 * 1024
        + 4 * 65_536
}

/// The most an Opus decode may hold: the state of the decoder of each of
/// eight streams and the frame scratch they share, the streams' decoded
/// samples, the largest packet's samples in eight channels, the largest
/// packet held while read and the one it outgrew, two pages, and the
/// metadata limits with slack.
const fn opus_peak(file: usize) -> usize {
    8 * OPUS_STREAM_STATE
        + OPUS_SHARED_SCRATCH
        + 2 * 5760 * 4
        + 5760 * 8 * 4
        + 2 * 61_440
        + 3 * file
        + 2 * (27 + 255 + 255 * 255)
        + 4 * 1024
        + 65_536
}

/// The heap one Opus stream's decoder keeps between packets, stereo, with
/// slack: about 40 KiB measured, CELT's history and SILK's state.
const OPUS_STREAM_STATE: usize = 48 * 1024;

/// The frame scratch a packet's streams share, with slack: about 68 KiB
/// measured, most of it CELT's transforms.
const OPUS_SHARED_SCRATCH: usize = 80 * 1024;

/// The most links of a chained file one case reads.
const MAX_LINKS: usize = 8;

/// Every frame of every link `source` gives, read through `input`, asking
/// again after a call that met missing bytes; and how the decode ended.
fn drain_links(
    source: &mut PcmSource,
    input: &mut impl SoundInput,
) -> (Vec<u8>, Result<(), DecodeError>) {
    let (mut all, mut end) = drain(source, input);
    for _ in 1..MAX_LINKS {
        if end.is_err() {
            break;
        }
        let link = source.link();
        let moved = loop {
            match source.next_link(input) {
                Err(DecodeError::InputUnavailable) => {
                    assert_eq!(source.link(), link, "a miss moved the link");
                }
                other => break other,
            }
        };
        match moved {
            Ok(true) => {
                assert_eq!(source.link().index, link.index + 1);
                assert!(source.link().offset > link.offset, "a link behind the last");
                assert_eq!(source.position(), 0);
                assert!(
                    source.metadata().within(&LIMITS),
                    "metadata past the limits"
                );
                let (more, ended) = drain(source, input);
                all.extend_from_slice(&more);
                end = ended;
            }
            Ok(false) => break,
            Err(err) => {
                end = Err(err);
                break;
            }
        }
    }
    (all, end)
}

/// Every frame `source` gives of its link, read through `input`, asking
/// again after a call that met missing bytes; and how the decode ended.
fn drain(
    source: &mut PcmSource,
    input: &mut impl SoundInput,
) -> (Vec<u8>, Result<(), DecodeError>) {
    let frame = source.info().frame_bytes();
    let mut block = vec![0u8; BLOCK_FRAMES * frame];
    let mut all = Vec::new();
    let stated = source.info().frames;
    loop {
        let position = source.position();
        let written = match source.next_block(input, &mut block) {
            Ok(written) => written,
            Err(DecodeError::InputUnavailable) => {
                assert_eq!(source.position(), position, "a miss moved the stream");
                continue;
            }
            Err(err) => return (all, Err(err)),
        };
        if written == 0 {
            return (all, Ok(()));
        }
        assert!(written <= BLOCK_FRAMES);
        all.extend_from_slice(&block[..written * frame]);
        if let Some(stated) = stated {
            assert!(
                source.position() <= stated,
                "more frames than the stream states"
            );
        }
    }
}

/// Bytes a supplied page holds.
const PAGE: usize = 64;

/// An input that holds some pages of its file from the start and is given
/// each page a read misses, as the sandbox's worker is: the read that missed
/// answers "not yet", and the same read succeeds when asked again.
struct Cache<'a> {
    file: &'a [u8],
    held: Vec<bool>,
}

impl<'a> Cache<'a> {
    fn new(file: &'a [u8], rng: &mut Prng) -> Self {
        let held = (0..file.len().div_ceil(PAGE))
            .map(|_| rng.below(2) == 0)
            .collect();
        Self { file, held }
    }
}

impl SoundInput for Cache<'_> {
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
            for page in pages {
                self.held[page] = true;
            }
            return Err(InputError::Unavailable);
        }
        let mut file = self.file;
        file.read_at(offset, buf)
    }
}

/// Every invariant over `bytes`.
fn exercise(bytes: &[u8], rng: &mut Prng) {
    let mut input = bytes;
    let probed = probe(&mut input);
    let (outcome, metering) = metered(|| {
        let mut input = bytes;
        let mut source = PcmSource::open(&mut input, &LIMITS).ok()?;
        let decoded = drain_links(&mut source, &mut input);
        Some((source, decoded))
    });
    let bound = match sniff(bytes) {
        Some(SoundFormat::Ogg) => flac_peak(bytes.len())
            .max(vorbis_peak(bytes.len()))
            .max(opus_peak(bytes.len())),
        Some(SoundFormat::Flac) => flac_peak(bytes.len()),
        _ if bytes.starts_with(b"ID3") => flac_peak(bytes.len()),
        _ => PEAK_BOUND,
    };
    // What the harness kept of the output is its own, not the decode's: the
    // buffer, and the one it outgrew while doubling.
    let kept = outcome
        .as_ref()
        .map_or(0, |(_, (decoded, _))| decoded.capacity() * 3 / 2);
    assert!(
        metering.peak <= bound + kept,
        "a decode held {} bytes past a bound of {bound} and {kept} of output",
        metering.peak
    );
    let Some((_, direct)) = outcome else {
        return;
    };
    let mut input = bytes;
    let mut source = PcmSource::open(&mut input, &LIMITS).expect("it opened once");
    assert_eq!(
        probed.as_ref().ok(),
        Some(source.info()),
        "the probe states what the open does"
    );
    assert!(
        source.metadata().within(&LIMITS),
        "metadata past the limits"
    );
    let mut cache = Cache::new(bytes, rng);
    let mut reopened = loop {
        match PcmSource::open(&mut cache, &LIMITS) {
            Err(DecodeError::InputUnavailable) => {}
            other => break other.expect("the file opens through the cache"),
        }
    };
    assert_eq!(
        reopened.metadata(),
        source.metadata(),
        "the cache changed the metadata"
    );
    assert_eq!(
        drain_links(&mut reopened, &mut cache),
        direct,
        "a decode through the cache differs"
    );
    let (direct, first_end) = drain(&mut source, &mut input);
    let (true, Some(frames), Ok(())) = (source.info().seekable, source.info().frames, first_end)
    else {
        return;
    };
    let mut source = PcmSource::open(&mut input, &LIMITS).expect("it opened once");
    let target = rng.next_u64() % (frames + 1);
    source.seek(target).expect("a stated frame is reachable");
    let frame = source.info().frame_bytes();
    let mut block = vec![0u8; 8 * frame];
    let mut input = bytes;
    let written = source
        .next_block(&mut input, &mut block)
        .expect("the decode read it once");
    let at = usize::try_from(target).expect("small") * frame;
    let entered = direct.get(at..at + written * frame);
    // A seek into Opus decodes on from a pre-roll, so its samples converge
    // on the decode from the start rather than repeat it.
    if source.info().encoding == Encoding::Opus {
        assert_eq!(
            entered.map(<[u8]>::len),
            Some(written * frame),
            "a block entered at {target} is misplaced"
        );
    } else {
        assert_eq!(
            entered,
            Some(&block[..written * frame]),
            "a block entered at {target} differs"
        );
    }
}

/// A plausible AU file, its fields drawn from what the format defines.
fn au(rng: &mut Prng) -> Vec<u8> {
    let encoding = *rng.pick(&[
        1u32, 2, 3, 4, 5, 6, 7, 11, 12, 13, 14, 23, 24, 25, 26, 27, 8, 21, 99,
    ]);
    let rate = *rng.pick(&[8000u32, 16_000, 44_100, 48_000, 96_000, 1, 4_000_000]);
    let channels = *rng.pick(&[1u32, 1, 2, 2, 3, 4, 6, 8, 9, 0]);
    let annotation = rng.below(40);
    let data = rng.below(4000);
    let declared = match rng.below(4) {
        0 => u32::MAX,
        1 => rng.next_u32(),
        _ => data as u32,
    };
    let mut file = b".snd".to_vec();
    for field in [24 + annotation as u32, declared, encoding, rate, channels] {
        file.extend(field.to_be_bytes());
    }
    let mut body = vec![0u8; annotation + data];
    rng.fill(&mut body);
    file.extend(body);
    file
}

fn chunk(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend((body.len() as u32).to_le_bytes());
    out.extend(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
    out
}

/// A plausible `fmt ` chunk: a tag the decoder names or one it does not,
/// with fields mostly in step with it.
fn fmt(rng: &mut Prng) -> Vec<u8> {
    let tag = *rng.pick(&[1u16, 1, 3, 6, 7, 0x11, 0x11, 2, 2, 0xFFFE, 0x55, 0x31, 0x99]);
    let channels: u16 = *rng.pick(&[1, 1, 2, 2, 6, 0, 9]);
    let bits: u16 = match tag {
        0x11 | 2 => 4,
        3 => *rng.pick(&[32, 64, 16]),
        6 | 7 => 8,
        _ => *rng.pick(&[8, 12, 16, 24, 32, 33, 0]),
    };
    let align = match tag {
        0x11 | 2 => *rng.pick(&[256u16, 512, 1024, 2048, 7]),
        _ => (bits.div_ceil(8) * channels).max(1),
    };
    let mut body = Vec::new();
    body.extend(tag.to_le_bytes());
    body.extend(channels.to_le_bytes());
    body.extend((*rng.pick(&[8000u32, 22_050, 44_100, 48_000, 2])).to_le_bytes());
    body.extend(0u32.to_le_bytes());
    body.extend(align.to_le_bytes());
    body.extend(bits.to_le_bytes());
    let per_block = |header: u16, factor: u16, plus: u16| {
        let ch = channels.max(1);
        align.saturating_sub(header * ch) * factor / ch + plus
    };
    let extension: Vec<u8> = match tag {
        0x11 => per_block(4, 2, 1).to_le_bytes().to_vec(),
        2 => {
            let mut ext = per_block(7, 2, 2).to_le_bytes().to_vec();
            let pairs = *rng.pick(&[7u16, 7, 1, 0, 300]);
            ext.extend(pairs.to_le_bytes());
            for _ in 0..pairs.min(300) {
                ext.extend(rng.next_u32().to_le_bytes());
            }
            ext
        }
        0xFFFE => {
            let mut ext = bits.to_le_bytes().to_vec();
            ext.extend((*rng.pick(&[0u32, 0x3, 0x4, 0x3F, 0x63F, 0x40])).to_le_bytes());
            ext.extend((*rng.pick(&[1u16, 3, 6, 7, 2])).to_le_bytes());
            ext.extend([
                0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
            ]);
            ext
        }
        _ => Vec::new(),
    };
    if !extension.is_empty() || rng.below(3) == 0 {
        body.extend((extension.len() as u16).to_le_bytes());
        body.extend(extension);
    }
    chunk(b"fmt ", &body)
}

/// A plausible WAVE file: its chunks in random order.
fn wave(rng: &mut Prng) -> Vec<u8> {
    let sixty_four = rng.below(4) == 0;
    let mut chunks = Vec::new();
    let mut data = vec![0u8; rng.below(6000)];
    rng.fill(&mut data);
    let mut data_chunk = chunk(b"data", &data);
    if sixty_four {
        let mut ds64 = 0u64.to_le_bytes().to_vec();
        ds64.extend((data.len() as u64).to_le_bytes());
        ds64.extend(rng.next_u64().to_le_bytes());
        ds64.extend(0u32.to_le_bytes());
        chunks.push(chunk(b"ds64", &ds64));
        data_chunk[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    }
    let mut rest = vec![fmt(rng), data_chunk];
    for _ in 0..rng.below(4) {
        let mut body = vec![0u8; rng.below(80)];
        rng.fill(&mut body);
        let id = *rng.pick(&[b"LIST", b"cue ", b"smpl", b"fact", b"junk"]);
        if id == b"LIST" && body.len() >= 4 {
            body[..4].copy_from_slice(b"INFO");
        }
        rest.push(chunk(id, &body));
    }
    if rng.below(2) == 0 {
        rest.swap(0, 1);
    }
    chunks.extend(rest);
    let body: Vec<u8> = chunks.concat();
    let mut file = (if sixty_four { b"RF64" } else { b"RIFF" }).to_vec();
    file.extend((body.len() as u32 + 4).to_le_bytes());
    file.extend(b"WAVE");
    file.extend(body);
    file
}

/// Bits laid most significant first.
#[derive(Default)]
struct Bits {
    bytes: Vec<u8>,
    len: usize,
}

impl Bits {
    fn put(&mut self, value: u32, count: u32) {
        for bit in (0..count).rev() {
            if self.len.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if (value >> bit) & 1 == 1 {
                let last = self.bytes.len() - 1;
                self.bytes[last] |= 0x80 >> (self.len % 8);
            }
            self.len += 1;
        }
    }
}

/// MPEG audio's frame check, x^16 + x^15 + x^2 + 1 over the first `bits`
/// bits of `data`, carried on from `crc`.
fn mpeg_crc(mut crc: u16, data: &[u8], bits: usize) -> u16 {
    for at in 0..bits {
        let bit = u16::from(data[at / 8] >> (7 - at % 8)) & 1;
        let top = crc >> 15;
        crc <<= 1;
        if top ^ bit == 1 {
            crc ^= 0x8005;
        }
    }
    crc
}

/// The reflected CRC-16 LAME guards its tag with.
fn lame_crc(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |mut crc, &byte| {
        crc ^= u16::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xA001
            };
        }
        crc
    })
}

/// ISO/IEC 11172-3 and 13818-3's bitrates in kbit/s, by index: MPEG-1's
/// Layers I, II and III, then the lower rates' Layer I and Layers II and III.
const MPEG_BITRATES: [[u32; 15]; 5] = [
    [
        0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
    ],
    [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
    ],
    [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ],
    [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
    ],
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
];

/// Sample rates by index, for MPEG-1, MPEG-2 and MPEG-2.5.
const MPEG_RATES: [[u32; 3]; 3] = [
    [44_100, 48_000, 32_000],
    [22_050, 24_000, 16_000],
    [11_025, 12_000, 8_000],
];

/// One MPEG audio frame's shape, as its header codes it.
#[derive(Copy, Clone)]
struct MpegShape {
    /// 0 for MPEG-1, 1 for MPEG-2, 2 for MPEG-2.5.
    family: usize,
    /// 1, 2 or 3.
    layer: u32,
    protected: bool,
    bitrate: usize,
    rate: usize,
    mode: u32,
    extension: u32,
}

impl MpegShape {
    fn header(self, padded: bool) -> [u8; 4] {
        let version = [3u8, 2, 0][self.family];
        let layer = [3u8, 2, 1][self.layer as usize - 1];
        [
            0xFF,
            0xE0 | version << 3 | layer << 1 | u8::from(!self.protected),
            (self.bitrate as u8) << 4 | (self.rate as u8) << 2 | u8::from(padded) << 1,
            (self.mode as u8) << 6 | (self.extension as u8) << 4,
        ]
    }

    fn channels(self) -> usize {
        if self.mode == 3 {
            1
        } else {
            2
        }
    }

    fn len(self, padded: bool) -> usize {
        let row = match (self.family, self.layer) {
            (0, layer) => layer as usize - 1,
            (_, 1) => 3,
            _ => 4,
        };
        let bits = MPEG_BITRATES[row][self.bitrate] * 1000;
        let rate = MPEG_RATES[self.family][self.rate];
        let padding = usize::from(padded);
        match (self.layer, self.family) {
            (1, _) => (12 * bits / rate) as usize * 4 + 4 * padding,
            (3, 1 | 2) => (72 * bits / rate) as usize + padding,
            _ => (144 * bits / rate) as usize + padding,
        }
    }

    fn side_info_len(self) -> usize {
        match (self.family == 0, self.channels()) {
            (true, 1) | (false, 2) => 17,
            (true, _) => 32,
            (false, _) => 9,
        }
    }

    fn data_start(self) -> usize {
        let side = if self.layer == 3 {
            self.side_info_len()
        } else {
            0
        };
        4 + 2 * usize::from(self.protected) + side
    }
}

/// A plausible MPEG audio frame shape: any family, Layers I, II or III at a
/// stated bitrate, any mode; protected sometimes.
fn mpeg_shape(rng: &mut Prng) -> MpegShape {
    let layer = *rng.pick(&[1u32, 2, 3, 3, 3]);
    MpegShape {
        family: rng.below(3),
        layer,
        // Layer II's allocation tables are not restated here, so its CRC
        // cannot be: its frames go unprotected.
        protected: layer != 2 && rng.below(4) == 0,
        bitrate: 1 + rng.below(14),
        rate: rng.below(3),
        mode: rng.below(4) as u32,
        extension: rng.below(4) as u32,
    }
}

/// Layer I's audio data within `budget` bits — subbands allocated sparsely,
/// their scalefactors and samples drawn at random within range — and the
/// bits its CRC covers.
fn layer1_data(rng: &mut Prng, shape: MpegShape, budget: usize) -> (Bits, usize) {
    let channels = shape.channels();
    let bound = if shape.mode == 1 {
        4 + 4 * shape.extension as usize
    } else {
        32
    };
    let mut allocation = [[0u32; 2]; 32];
    for (subband, allocation) in allocation.iter_mut().enumerate() {
        for channel in 0..channels {
            allocation[channel] = if channel == 1 && subband >= bound {
                allocation[0]
            } else if rng.below(6) == 0 {
                1 + rng.below(14) as u32
            } else {
                0
            };
        }
    }
    let codes: usize = allocation
        .iter()
        .enumerate()
        .map(|(subband, allocation)| {
            let carried = if subband >= bound { 1 } else { channels };
            allocation[..carried]
                .iter()
                .map(|&a| if a == 0 { 0 } else { a as usize + 1 })
                .sum::<usize>()
        })
        .sum();
    let factors = allocation
        .iter()
        .flat_map(|a| &a[..channels])
        .filter(|&&a| a != 0)
        .count();
    let allocation_bits = 4 * (bound.min(32) * channels + 32usize.saturating_sub(bound));
    if allocation_bits + 6 * factors + 12 * codes > budget {
        allocation = [[0; 2]; 32];
    }
    let mut bits = Bits::default();
    for (subband, allocation) in allocation.iter().enumerate() {
        let carried = if subband >= bound { 1 } else { channels };
        for &a in &allocation[..carried] {
            bits.put(a, 4);
        }
    }
    let protected = bits.len;
    for allocation in &allocation {
        for _ in allocation[..channels].iter().filter(|&&a| a != 0) {
            bits.put(rng.below(63) as u32, 6);
        }
    }
    for _ in 0..12 {
        for (subband, allocation) in allocation.iter().enumerate() {
            let carried = if subband >= bound { 1 } else { channels };
            for &a in allocation[..carried].iter().filter(|&&a| a != 0) {
                let steps = (1u32 << (a + 1)) - 1;
                bits.put(rng.next_u32() % steps, a + 1);
            }
        }
    }
    (bits, protected)
}

/// One Layer III granule's lines in its count1 region under table B: a
/// random number of quadruples, each its inverted four bits and the signs
/// of its set ones, within `budget` bits.
fn count1(rng: &mut Prng, bits: &mut Bits, budget: usize) -> u32 {
    let start = bits.len;
    for _ in 0..rng.below((budget / 8).min(144) + 1) {
        let value = rng.below(16) as u32;
        bits.put(15 - value, 4);
        for _ in 0..value.count_ones() {
            bits.put(rng.below(2) as u32, 1);
        }
    }
    (bits.len - start) as u32
}

/// Layer III side information for granules of `lengths` bits: every block
/// shape, unscaled, the big values empty and the count1 region under table B.
fn layer3_side(rng: &mut Prng, shape: MpegShape, begin: usize, lengths: &[[u32; 2]; 2]) -> Vec<u8> {
    let lsf = shape.family != 0;
    let channels = shape.channels();
    let mut bits = Bits::default();
    bits.put(begin as u32, if lsf { 8 } else { 9 });
    bits.put(
        0,
        match (lsf, channels) {
            (false, 1) => 5,
            (false, _) => 3,
            (true, 1) => 1,
            (true, _) => 2,
        },
    );
    if !lsf {
        bits.put(0, 4 * channels as u32);
    }
    for granule in lengths.iter().take(if lsf { 1 } else { 2 }) {
        for &length in granule.iter().take(channels) {
            bits.put(length, 12);
            bits.put(0, 9);
            bits.put(100 + rng.below(110) as u32, 8);
            bits.put(0, if lsf { 9 } else { 4 });
            if rng.below(2) == 0 {
                bits.put(1, 1);
                bits.put(1 + rng.below(3) as u32, 2);
                bits.put(rng.below(2) as u32, 1);
                bits.put(0, 10);
                bits.put(rng.below(512) as u32, 9);
            } else {
                bits.put(0, 1);
                bits.put(0, 15);
                bits.put(rng.below(128) as u32, 7);
            }
            if !lsf {
                bits.put(rng.below(2) as u32, 1);
            }
            bits.put(rng.below(2) as u32, 1);
            bits.put(1, 1);
        }
    }
    bits.bytes
}

/// Seal a protected frame with its CRC over the header's last two bytes and
/// the first `bits` bits after the CRC field.
fn seal(bytes: &mut [u8], shape: MpegShape, bits: usize) {
    if shape.protected {
        let bits = bits.min((bytes.len() - 6) * 8);
        let crc = mpeg_crc(mpeg_crc(0xFFFF, &bytes[2..4], 16), &bytes[6..], bits);
        bytes[4..6].copy_from_slice(&crc.to_be_bytes());
    }
}

/// A Layer I frame's samples, or a Layer II frame's silence.
fn layer12_frame(rng: &mut Prng, shape: MpegShape, padded: bool) -> Vec<u8> {
    let (len, start) = (shape.len(padded), shape.data_start());
    let mut bytes = shape.header(padded).to_vec();
    bytes.resize(len, 0);
    if shape.layer == 1 {
        // A frame too short for even its allocations is cut, and refused.
        let (bits, protected) = layer1_data(rng, shape, (len - start) * 8);
        let fits = bits.bytes.len().min(len - start);
        bytes[start..start + fits].copy_from_slice(&bits.bytes[..fits]);
        seal(&mut bytes, shape, protected);
    }
    bytes
}

/// Layer III frames of `frames`' shapes, each frame's main data laid into
/// the reservoir as far back as `reach` bytes, so frames lean on the ones
/// before them.
fn layer3_frames(rng: &mut Prng, frames: &[(MpegShape, bool)], reach: usize) -> Vec<Vec<u8>> {
    let mut coded = Vec::new();
    for &(shape, padded) in frames {
        let area = shape.len(padded) - shape.data_start();
        let granules = if shape.family == 0 { 2 } else { 1 };
        let parts = granules * shape.channels();
        let mut data = Bits::default();
        let mut lengths = [[0u32; 2]; 2];
        for granule in lengths.iter_mut().take(granules) {
            for length in granule.iter_mut().take(shape.channels()) {
                *length = count1(rng, &mut data, area * 8 / parts);
            }
        }
        coded.push((data.bytes, lengths, area));
    }
    let mut main = vec![0u8; coded.iter().map(|(_, _, area)| area).sum()];
    let mut begins = Vec::new();
    let (mut area_start, mut end) = (0usize, 0usize);
    for (data, _, area) in &coded {
        let start = end.max(area_start.saturating_sub(reach));
        main[start..start + data.len()].copy_from_slice(data);
        begins.push(area_start - start);
        end = start + data.len();
        area_start += area;
    }
    let mut area_start = 0;
    let mut out = Vec::new();
    for ((&(shape, padded), (_, lengths, area)), &begin) in frames.iter().zip(&coded).zip(&begins) {
        let mut bytes = shape.header(padded).to_vec();
        bytes.resize(shape.data_start() - shape.side_info_len(), 0);
        bytes.extend(layer3_side(rng, shape, begin, lengths));
        bytes.extend_from_slice(&main[area_start..area_start + area]);
        seal(&mut bytes, shape, shape.side_info_len() * 8);
        out.push(bytes);
        area_start += area;
    }
    out
}

/// A plausible MPEG audio stream: frames whose headers agree and whose
/// audio is mostly decodable — Layer I's samples, Layer II's silence,
/// Layer III's count1 region in every block shape and joint stereo, leaning
/// on its reservoir — sometimes behind a gapless tag frame, between ID3 and
/// APE tags, with bytes between frames or a frame of another mode.
fn mpeg(rng: &mut Prng) -> Vec<u8> {
    let first = mpeg_shape(rng);
    let mut frames = Vec::new();
    for at in 0..=rng.below(40) {
        let mut shape = first;
        if at > 0 && rng.below(12) == 0 {
            shape.mode = rng.below(4) as u32;
        }
        let padded = rng.below(2) == 0;
        if shape.len(padded) <= shape.data_start() {
            break;
        }
        frames.push((shape, padded));
    }
    let bodies = if first.layer == 3 {
        let reach = *rng.pick(&[0usize, 64, if first.family == 0 { 511 } else { 255 }]);
        layer3_frames(rng, &frames, reach)
    } else {
        frames
            .iter()
            .map(|&(shape, padded)| layer12_frame(rng, shape, padded))
            .collect()
    };
    let mut file = Vec::new();
    if rng.below(4) == 0 {
        let body = rng.below(200);
        file.extend(b"ID3\x04\x00\x00\x00\x00");
        file.extend([(body >> 7) as u8 & 0x7F, body as u8 & 0x7F]);
        let mut tag = vec![0u8; body];
        rng.fill(&mut tag);
        file.extend(tag);
    }
    if first.layer == 3 && !frames.is_empty() && rng.below(3) == 0 {
        file.extend(mpeg_tag_frame(rng, first, frames.len() as u32));
    }
    for body in bodies {
        if rng.below(30) == 0 {
            let mut junk = vec![0u8; rng.below(20)];
            rng.fill(&mut junk);
            file.extend(junk);
        }
        file.extend(body);
    }
    if rng.below(4) == 0 {
        let mut ape = vec![0u8; 32];
        ape[..8].copy_from_slice(b"APETAGEX");
        ape[12..16].copy_from_slice(&32u32.to_le_bytes());
        file.extend(ape);
    }
    if rng.below(4) == 0 {
        let mut id3v1 = vec![0u8; 128];
        id3v1[..3].copy_from_slice(b"TAG");
        file.extend(id3v1);
    }
    file
}

/// A Layer III tag frame of `shape` stating `frames`, with LAME's extension
/// stating a delay and padding, its CRC holding or not.
fn mpeg_tag_frame(rng: &mut Prng, shape: MpegShape, frames: u32) -> Vec<u8> {
    let len = shape.len(false);
    let mut frame = shape.header(false).to_vec();
    frame.resize(shape.data_start(), 0);
    frame.extend(if rng.below(2) == 0 { b"Xing" } else { b"Info" });
    frame.extend(1u32.to_be_bytes());
    frame.extend(frames.to_be_bytes());
    let at = frame.len();
    let mut lame = [0u8; 36];
    lame[..4].copy_from_slice(b"LAME");
    let (delay, padding) = (rng.below(4096), rng.below(4096));
    lame[21] = (delay >> 4) as u8;
    lame[22] = ((delay & 0xF) << 4) as u8 | (padding >> 8) as u8;
    lame[23] = padding as u8;
    frame.extend(lame);
    let crc = lame_crc(&frame[..at + 34]) ^ u16::from(rng.below(4) == 0);
    frame[at + 34..at + 36].copy_from_slice(&crc.to_be_bytes());
    if frame.len() > len {
        return Vec::new();
    }
    frame.resize(len, 0);
    seal(&mut frame, shape, shape.side_info_len() * 8);
    frame
}

/// Samples of `bits` for `frames` frames of `channels`: a random walk, so a
/// predictor has something to find, or plain noise.
fn samples(rng: &mut Prng, frames: usize, channels: usize, bits: u8) -> Vec<Vec<i32>> {
    let top = (1i64 << (bits - 1)) - 1;
    let noise = rng.below(2) == 0;
    (0..channels)
        .map(|_| {
            let mut value = 0i64;
            (0..frames)
                .map(|_| {
                    value = if noise {
                        i64::from(rng.next_u32().cast_signed()) >> (32 - bits)
                    } else {
                        (value + i64::from(rng.next_u8()) - 128).clamp(-top - 1, top)
                    };
                    value as i32
                })
                .collect()
        })
        .collect()
}

/// A residual coding for a block of `block` and a predictor of `order`, its
/// partitioning and parameters drawn at random within what the block allows.
fn residual(rng: &mut Prng, block: usize, order: usize) -> Residual {
    let wide = rng.below(3) == 0;
    let mut partition_order = 0u8;
    while partition_order < 8
        && rng.below(2) == 0
        && block.is_multiple_of(1 << (partition_order + 1))
        && block >> (partition_order + 1) >= order
    {
        partition_order += 1;
    }
    let top = if wide { 30 } else { 14 };
    let partitions = (0..1usize << partition_order)
        .map(|_| {
            if rng.below(6) == 0 {
                Partition::Escape(rng.below(32) as u8)
            } else {
                Partition::Rice(rng.below(top + 1) as u8)
            }
        })
        .collect();
    Residual {
        wide,
        order: partition_order,
        partitions,
    }
}

/// A subframe construct drawn at random for a block of `block`.
fn subframe(rng: &mut Prng, block: usize) -> Subframe {
    let longest = block.saturating_sub(1);
    let (predictor, order) = match rng.below(5) {
        0 => (Predictor::Constant, 0),
        1 => (Predictor::Verbatim, 0),
        2 => {
            let order = rng.below(5.min(longest + 1)) as u8;
            (Predictor::Fixed(order), usize::from(order))
        }
        _ if longest > 0 => {
            let order = 1 + rng.below(32.min(longest));
            let precision = 1 + rng.below(15) as u8;
            let limit = 1i32 << (precision - 1);
            let coefficients = (0..order)
                .map(|_| {
                    (i64::from(rng.next_u32()) % (2 * i64::from(limit)) - i64::from(limit)) as i32
                })
                .collect();
            let shift = rng.below(16) as u8;
            (
                Predictor::Lpc {
                    coefficients,
                    precision,
                    shift,
                },
                order,
            )
        }
        _ => (Predictor::Verbatim, 0),
    };
    Subframe {
        predictor,
        wasted: if rng.below(4) == 0 {
            rng.below(4) as u8
        } else {
            0
        },
        residual: residual(rng, block, order),
    }
}

/// A well-formed cuesheet: tracks at the stream's start, each with index
/// points numbered as the format requires, then the lead-out.
fn cuesheet(rng: &mut Prng) -> Vec<u8> {
    let cd = rng.below(2) == 0;
    let mut out = vec![0u8; 128];
    out[..13].copy_from_slice(b"0123456789012");
    out.extend(rng.next_u64().to_be_bytes());
    out.push(if cd { 0x80 } else { 0 });
    out.extend([0u8; 258]);
    let tracks = 1 + rng.below(4);
    out.push((tracks + 1) as u8);
    for track in 1..=tracks {
        out.extend(0u64.to_be_bytes());
        out.push(track as u8);
        out.extend([0u8; 12 + 1 + 13]);
        let points = 1 + rng.below(3);
        out.push(points as u8);
        let first = rng.below(2);
        for point in 0..points {
            out.extend(0u64.to_be_bytes());
            out.push((first + point) as u8);
            out.extend([0u8; 3]);
        }
    }
    out.extend(0u64.to_be_bytes());
    out.push(if cd { 170 } else { 255 });
    out.extend([0u8; 12 + 1 + 13]);
    out.push(0);
    out
}

/// A well-formed picture block holding random picture bytes.
fn picture(rng: &mut Prng) -> Vec<u8> {
    let mut out = (rng.below(21) as u32).to_be_bytes().to_vec();
    for text in [&b"image/png"[..], b"a cover"] {
        out.extend((text.len() as u32).to_be_bytes());
        out.extend(text);
    }
    for _ in 0..4 {
        out.extend(rng.next_u32().to_be_bytes());
    }
    let mut data = vec![0u8; rng.below(400)];
    rng.fill(&mut data);
    out.extend((data.len() as u32).to_be_bytes());
    out.extend(data);
    out
}

/// Draw the metadata a stream carries: a Vorbis comment whose channel mask
/// usually matches `channels`, a seek table, padding, and well-formed or
/// opaque blocks of the other kinds.
fn metadata(writer: &mut Writer, rng: &mut Prng, channels: u8) {
    if rng.below(2) == 0 {
        let matching = match channels {
            1 => 0x4,
            2 => 0x3,
            3 => 0x7,
            6 => 0x3F,
            _ => 0x63F,
        };
        let mask = if rng.below(4) == 0 {
            *rng.pick(&[0x3u32, 0x4, 0x3F, 0x63F, 0x8, 0x1_0000])
        } else {
            matching
        };
        let mask = format!("WAVEFORMATEXTENSIBLE_CHANNEL_MASK=0x{mask:x}");
        writer.comments(
            "fuzz",
            &[
                "TITLE=t",
                "artist=a",
                &mask,
                "LONG_NAME_THAT_HAS_NO_READING_HERE_AT_ALL=x",
            ],
        );
    }
    if rng.below(3) == 0 {
        writer.seek_points(1 + rng.next_u64() % 5000, rng.below(3) as u32);
    }
    if rng.below(3) == 0 {
        writer.padding(rng.below(100) as u32);
    }
    for _ in 0..rng.below(4) {
        match rng.below(4) {
            0 => writer.block(5, &cuesheet(rng)),
            1 => writer.block(6, &picture(rng)),
            kind => {
                let mut data = vec![0u8; 4 + rng.below(300)];
                rng.fill(&mut data);
                writer.block(
                    if kind == 2 {
                        2
                    } else {
                        *rng.pick(&[7u8, 64, 126])
                    },
                    &data,
                );
            }
        }
    }
}

/// A FLAC stream the encoder writes, its every construct drawn at random:
/// where the core cannot code a frame as drawn, the chooser codes it. The
/// samples it holds, interleaved, come with it.
fn flac(rng: &mut Prng) -> (Vec<u8>, Vec<i32>) {
    let params = Params {
        rate: *rng.pick(&[
            8_000u32, 11_025, 22_050, 32_000, 44_100, 48_000, 96_000, 192_000, 655_350, 1_000,
        ]),
        channels: *rng.pick(&[1u8, 1, 2, 2, 2, 3, 6, 8]),
        bits: *rng.pick(&[4u8, 5, 8, 12, 16, 17, 20, 24, 31, 32]),
    };
    let blocking = if rng.below(3) == 0 {
        Blocking::Variable { min: 16, max: 400 }
    } else {
        Blocking::Fixed(*rng.pick(&[16u32, 64, 192, 256, 333]))
    };
    let Ok(mut writer) = Writer::new(params, blocking) else {
        return (Vec::new(), Vec::new());
    };
    metadata(&mut writer, rng, params.channels);
    let block = match blocking {
        Blocking::Fixed(block) => block as usize,
        Blocking::Variable { .. } => 0,
    };
    let channels = usize::from(params.channels);
    let mut interleaved = Vec::new();
    for frame in 0..=rng.below(4) {
        let size = if block == 0 {
            16 + rng.below(385)
        } else if frame > 0 && rng.below(4) == 0 {
            1 + rng.below(block)
        } else {
            block
        };
        let signal = samples(rng, size, channels, params.bits);
        let slices: Vec<&[i32]> = signal.iter().map(Vec::as_slice).collect();
        let stereo = if channels == 2 {
            *rng.pick(&[
                Stereo::Independent,
                Stereo::LeftSide,
                Stereo::SideRight,
                Stereo::MidSide,
            ])
        } else {
            Stereo::Independent
        };
        let drawn = Frame {
            stereo,
            subframes: (0..channels).map(|_| subframe(rng, size)).collect(),
            codes: Codes {
                rate: *rng.pick(&[RateCode::Compact, RateCode::StreamInfo, RateCode::Hertz]),
                width_from_streaminfo: rng.below(3) == 0,
                block: *rng.pick(&[BlockCode::Compact, BlockCode::Word]),
            },
        };
        let written = writer.frame(&slices, &drawn).or_else(|_| {
            let quick = Options {
                block: 0,
                max_lpc_order: 2,
                max_partition_order: 3,
            };
            let chosen = tairix_sound::flac_encode::choose(params, &slices, quick);
            writer.frame(&slices, &chosen)
        });
        if written.is_err() {
            break;
        }
        for index in 0..size {
            for channel in &signal {
                interleaved.push(channel[index]);
            }
        }
        if size < block {
            break;
        }
    }
    let file = if rng.below(4) == 0 {
        writer.finish_ogg(rng.next_u32(), 64 + rng.below(2000))
    } else {
        writer.finish()
    };
    (file, interleaved)
}

/// The PCM a decode writes for `interleaved` samples of `bits`.
fn pcm(interleaved: &[i32], bits: u8) -> Vec<u8> {
    let (bytes, shift) = match bits {
        0..=8 => (1, 8 - u32::from(bits)),
        9..=16 => (2, 16 - u32::from(bits)),
        17..=24 => (3, 24 - u32::from(bits)),
        _ => (4, 32 - u32::from(bits)),
    };
    let mut out = Vec::new();
    for &sample in interleaved {
        let placed = (i64::from(sample) << shift).to_le_bytes();
        out.extend_from_slice(&placed[..bytes]);
        if bytes == 1 {
            let last = out.len() - 1;
            out[last] ^= 0x80;
        }
    }
    out
}

fn mutate(bytes: &mut [u8], rng: &mut Prng) {
    if bytes.is_empty() {
        return;
    }
    for _ in 0..rng.at_most(6) {
        let at = rng.below(bytes.len());
        bytes[at] ^= rng.next_u8();
    }
}

fn sweep(test: &str, mut case: impl FnMut(&mut Prng)) {
    let mut rng = Prng::new(tairix_fuzzseed::start(test, tairix_fuzzseed::FUZZ_SEED_ENV));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            case(&mut rng);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}

#[test]
fn arbitrary_bytes_never_panic() {
    sweep("arbitrary_bytes_never_panic", |rng| {
        let mut bytes = vec![0u8; rng.below(400)];
        rng.fill(&mut bytes);
        if rng.below(2) == 0 && bytes.len() >= 12 {
            let signature: &[u8] =
                rng.pick(&[b".snd".as_slice(), b"RIFF", b"RF64", b"\xFF\xFB\x90\x40"]);
            bytes[..4].copy_from_slice(signature);
            bytes[8..12].copy_from_slice(b"WAVE");
        }
        exercise(&bytes, rng);
    });
}

#[test]
fn structured_au_files_hold_every_invariant() {
    sweep("structured_au_files_hold_every_invariant", |rng| {
        let mut file = au(rng);
        if rng.below(2) == 0 {
            mutate(&mut file, rng);
        }
        if rng.below(4) == 0 {
            file.truncate(rng.below(file.len() + 1));
        }
        exercise(&file, rng);
    });
}

#[test]
fn structured_wave_files_hold_every_invariant() {
    sweep("structured_wave_files_hold_every_invariant", |rng| {
        let mut file = wave(rng);
        if rng.below(2) == 0 {
            mutate(&mut file, rng);
        }
        if rng.below(4) == 0 {
            file.truncate(rng.below(file.len() + 1));
        }
        exercise(&file, rng);
    });
}

#[test]
fn structured_mpeg_streams_hold_every_invariant() {
    sweep("structured_mpeg_streams_hold_every_invariant", |rng| {
        let mut file = mpeg(rng);
        if rng.below(2) == 0 {
            mutate(&mut file, rng);
        }
        if rng.below(4) == 0 {
            file.truncate(rng.below(file.len() + 1));
        }
        exercise(&file, rng);
    });
}

#[test]
fn structured_flac_files_hold_every_invariant() {
    sweep("structured_flac_files_hold_every_invariant", |rng| {
        let (mut file, interleaved) = flac(rng);
        if file.is_empty() {
            return;
        }
        let mutated = rng.below(2) == 0;
        if mutated {
            mutate(&mut file, rng);
        }
        if rng.below(4) == 0 {
            file.truncate(rng.below(file.len() + 1));
        } else if !mutated {
            round_trips(&file, &interleaved);
        }
        exercise(&file, rng);
    });
}

/// libvorbis's own streams: stereo with its block switching, mono at 8 kHz,
/// and 5.1.
const VORBIS: [&[u8]; 3] = [
    include_bytes!("vectors/vorbis-stereo.ogg"),
    include_bytes!("vectors/vorbis-mono.ogg"),
    include_bytes!("vectors/vorbis-surround.ogg"),
];

/// The Ogg CRC (RFC 3533, section 6): polynomial 0x04C11DB7, unreflected,
/// no final inversion.
fn ogg_crc(bytes: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &byte in bytes {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                crc << 1 ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// The pages of `file`, each whole, as far as they hold together.
fn ogg_pages(file: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 27 <= file.len() {
        let segments = usize::from(file[at + 26]);
        let Some(lacing) = file.get(at + 27..at + 27 + segments) else {
            break;
        };
        let len = 27 + segments + lacing.iter().map(|&len| usize::from(len)).sum::<usize>();
        let Some(page) = file.get(at..at + len) else {
            break;
        };
        out.push(page.to_vec());
        at += len;
    }
    out
}

/// `page` with its CRC made good again.
fn reseal(page: &mut [u8]) {
    page[22..26].fill(0);
    let crc = ogg_crc(page);
    page[22..26].copy_from_slice(&crc.to_le_bytes());
}

/// libopus's own streams: every mode, bandwidth and frame size, the switches
/// between modes, redundancy, discontinuous transmission, padding, 5.1, 6.1
/// and 7.1.
const OPUS: [&[u8]; 9] = [
    include_bytes!("vectors/opus-celt.opus"),
    include_bytes!("vectors/opus-silk.opus"),
    include_bytes!("vectors/opus-hybrid.opus"),
    include_bytes!("vectors/opus-switching.opus"),
    include_bytes!("vectors/opus-cbr.opus"),
    include_bytes!("vectors/opus-dtx.opus"),
    include_bytes!("vectors/opus-surround.opus"),
    include_bytes!("vectors/opus-octophonic.opus"),
    include_bytes!("vectors/opus-heptaphonic.opus"),
];

/// The pages of one of `streams`, mostly under a serial of their own so
/// that links chained or interleaved from them are well formed.
fn library_pages(rng: &mut Prng, streams: &[&[u8]]) -> Vec<Vec<u8>> {
    let mut pages = ogg_pages(rng.pick(streams));
    if rng.below(4) != 0 {
        let serial = rng.next_u32().to_le_bytes();
        for page in &mut pages {
            page[14..18].copy_from_slice(&serial);
            reseal(page);
        }
    }
    pages
}

/// An Ogg file built from a codec library's `streams`: one, a chain of them
/// — sometimes two sharing a serial, against the format's rule — or two
/// interleaved, then corrupted within pages whose CRCs are made good, so the
/// damage reaches the codec rather than the container's check.
fn library_streams(rng: &mut Prng, streams: &[&[u8]]) -> Vec<u8> {
    let links = 1 + rng.below(3);
    let mut pages: Vec<Vec<u8>> = Vec::new();
    if rng.below(4) == 0 {
        let first = library_pages(rng, streams);
        let second = library_pages(rng, streams);
        let (mut a, mut b) = (first.into_iter(), second.into_iter());
        loop {
            match (a.next(), b.next()) {
                (None, None) => break,
                (x, y) => pages.extend(x.into_iter().chain(y)),
            }
        }
    } else {
        for _ in 0..links {
            pages.extend(library_pages(rng, streams));
        }
    }
    for _ in 0..rng.below(4) {
        let page = rng.below(pages.len());
        let body = 27 + usize::from(pages[page][26]);
        if pages[page].len() > body {
            let at = body + rng.below(pages[page].len() - body);
            pages[page][at] ^= rng.next_u8() | 1;
            reseal(&mut pages[page]);
        }
    }
    if rng.below(6) == 0 && pages.len() > 2 {
        pages.remove(1 + rng.below(pages.len() - 1));
    }
    pages.concat()
}

#[test]
fn structured_vorbis_streams_hold_every_invariant() {
    sweep("structured_vorbis_streams_hold_every_invariant", |rng| {
        let mut file = library_streams(rng, &VORBIS);
        if rng.below(4) == 0 {
            mutate(&mut file, rng);
        }
        if rng.below(5) == 0 {
            file.truncate(rng.below(file.len() + 1));
        }
        exercise(&file, rng);
    });
}

/// Opus links, sometimes chained or interleaved with Vorbis ones.
#[test]
fn structured_opus_streams_hold_every_invariant() {
    sweep("structured_opus_streams_hold_every_invariant", |rng| {
        let mixed: Vec<&[u8]> = OPUS.iter().chain(&VORBIS).copied().collect();
        let streams = if rng.below(4) == 0 {
            &mixed[..]
        } else {
            &OPUS[..]
        };
        let mut file = library_streams(rng, streams);
        if rng.below(4) == 0 {
            mutate(&mut file, rng);
        }
        if rng.below(5) == 0 {
            file.truncate(rng.below(file.len() + 1));
        }
        exercise(&file, rng);
    });
}

/// An unmutated stream decodes to what it was written from, unless its rate
/// or channels have no reading in the PCM vocabulary.
fn round_trips(file: &[u8], interleaved: &[i32]) {
    let mut input = file;
    let mut source = match PcmSource::open(&mut input, &LIMITS) {
        Ok(source) => source,
        Err(
            DecodeError::ChannelLayoutUnsupported
            | DecodeError::FlacChannelMask
            | DecodeError::RateOutOfRange,
        ) => return,
        Err(err) => panic!("a stream the encoder wrote is refused: {err:?}"),
    };
    let frame = source.info().frame_bytes();
    let mut block = vec![0u8; BLOCK_FRAMES * frame];
    let mut all = Vec::new();
    loop {
        match source.next_block(&mut input, &mut block) {
            Ok(0) => break,
            Ok(written) => all.extend_from_slice(&block[..written * frame]),
            Err(err) => panic!("a stream the encoder wrote fails: {err:?}"),
        }
    }
    let width = stream_bits(file);
    assert_eq!(
        all,
        pcm(interleaved, width),
        "the samples written are the samples read"
    );
}

/// The sample width a stream's `STREAMINFO` states.
fn stream_bits(file: &[u8]) -> u8 {
    let info = if file.starts_with(b"fLaC") {
        &file[8..]
    } else {
        &file[28 + 17..]
    };
    ((u64::from_be_bytes(info[10..18].try_into().expect("eight bytes")) >> 36 & 0x1F) + 1) as u8
}
