//! Fuzz harness for the sound-file decoders: untrusted AU and WAVE bytes.
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
//!
//! Inputs are arbitrary bytes, and structured AU, WAVE and FLAC files — every
//! encoding and codec the decoders name, chunks in random order, every FLAC
//! construct the encoder's core can be told to emit — then mutated.

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
    probe, sniff, DecodeError, DecodeLimits, InputError, PcmSource, SoundFormat, SoundInput,
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

/// Every frame `source` gives, read through `input`, asking again after a
/// call that met missing bytes.
fn drain(source: &mut PcmSource, input: &mut impl SoundInput) -> Result<Vec<u8>, DecodeError> {
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
            Err(err) => return Err(err),
        };
        if written == 0 {
            return Ok(all);
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
        let decoded = drain(&mut source, &mut input);
        Some((source, decoded))
    });
    let bound = match sniff(bytes) {
        Some(SoundFormat::Flac | SoundFormat::Ogg) => flac_peak(bytes.len()),
        _ if bytes.starts_with(b"ID3") => flac_peak(bytes.len()),
        _ => PEAK_BOUND,
    };
    assert!(
        metering.peak <= bound,
        "a decode held {} bytes past a bound of {bound}",
        metering.peak
    );
    let Some((mut source, direct)) = outcome else {
        return;
    };
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
        drain(&mut reopened, &mut cache),
        direct,
        "a decode through the cache differs"
    );
    let (true, Some(frames), Ok(direct)) = (source.info().seekable, source.info().frames, direct)
    else {
        return;
    };
    let target = rng.next_u64() % (frames + 1);
    source.seek(target).expect("a stated frame is reachable");
    let frame = source.info().frame_bytes();
    let mut block = vec![0u8; 8 * frame];
    let mut input = bytes;
    let written = source
        .next_block(&mut input, &mut block)
        .expect("the decode read it once");
    let at = usize::try_from(target).expect("small") * frame;
    assert_eq!(
        direct.get(at..at + written * frame),
        Some(&block[..written * frame]),
        "a block entered at {target} differs"
    );
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
            let signature: &[u8] = rng.pick(&[b".snd".as_slice(), b"RIFF", b"RF64"]);
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
