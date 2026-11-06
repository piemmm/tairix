//! Encoder tests: every frame reads back, through the stream decoder, as the
//! encoder's own reconstruction, bit for bit; static content stays exact; each
//! quality holds its floor; and a refused encode changes nothing.

extern crate std;

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use tairix_fuzzseed::meter::{metered, refusing_after};
use tairix_fuzzseed::{first_difference, Prng};
use tairix_parallel::{JobRunner, Reversed, Threaded, SERIAL};

use super::{
    Changed, Encoder, Modes, Partition, Picture, Quality, Shape, TokenRows, Written, NO_LEVELS,
};
use crate::vp8::forward::{forward_dct, forward_walsh};
use crate::vp8::inter::Prediction;
use crate::vp8::motion::{Motion, Mv};
use crate::vp8::B_PRED;
use crate::vp8::{
    colour, decode_rows, inverse_dct, inverse_walsh, probe, Decoded, Decoder, Nonzero, Planes, Tag,
    Vp8Error, DEFAULT_COEFF_PROBS, MAX_PARTITIONS,
};

/// The size the quality tests' pictures are drawn at.
const WIDTH: usize = 96;
const HEIGHT: usize = 64;

/// Every quality, coarsest first.
const QUALITIES: [Quality; 3] = [Quality::Compact, Quality::Standard, Quality::Fine];

/// `length` as a picture's side or coordinate.
fn side(length: usize) -> u32 {
    u32::try_from(length).expect("a side")
}

/// The region `width`×`height` from (`x`, `y`).
const fn region(x: u32, y: u32, width: u32, height: u32) -> Changed {
    Changed {
        x,
        y,
        width,
        height,
    }
}

/// The whole of a `width`×`height` picture, as a changed region.
fn whole(width: usize, height: usize) -> Changed {
    region(0, 0, side(width), side(height))
}

/// A packed `width`×`height` picture, each pixel what `paint` makes of its
/// column and row.
fn picture(width: usize, height: usize, paint: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let [red, green, blue] = paint(x, y);
            rgba.extend_from_slice(&[red, green, blue, u8::MAX]);
        }
    }
    rgba
}

/// Four flat fills meeting on edges the chroma samples share.
fn flat(x: usize, y: usize) -> [u8; 3] {
    match (x < 36, y < 20) {
        (true, true) => [240, 240, 240],
        (true, false) => [200, 40, 40],
        (false, true) => [30, 30, 30],
        (false, false) => [40, 90, 200],
    }
}

/// Dark glyph-like strokes on a light ground, set in cells as text is.
fn text(x: usize, y: usize) -> [u8; 3] {
    let cell = x / 7 + (y / 12) * 97;
    let strokes = cell.wrapping_mul(2_654_435_761) & 0xF_FFFF;
    let (across, down) = (x % 7, y % 12);
    let inked = down < 9 && across < 5 && (strokes >> (across + 5 * (down / 3))) & 1 == 1;
    if inked {
        [20, 20, 20]
    } else {
        [245, 245, 245]
    }
}

/// Three ramps across the quality tests' picture, a channel each, repeating
/// past it.
fn gradient(x: usize, y: usize) -> [u8; 3] {
    let ramp =
        |at: usize, span: usize| u8::try_from(at % (span + 1) * 255 / span).expect("a level");
    [
        ramp(x, WIDTH - 1),
        ramp(y, HEIGHT - 1),
        ramp(x + y, WIDTH + HEIGHT - 2),
    ]
}

/// Screen content scrolled `shift` pixels left: bands of text between bands
/// of ramps, so a stream exercises motion, residuals and intra prediction.
fn scene(x: usize, y: usize, shift: usize) -> [u8; 3] {
    if (y / 8).is_multiple_of(3) {
        gradient(x + shift, y)
    } else {
        text(x + shift, y)
    }
}

/// The RGB channels' peak signal-to-noise ratio, in decibels, of `made`
/// against `wanted`, two packed pictures; infinite where they are the same.
#[allow(
    clippy::cast_precision_loss,
    reason = "a test's measure of error, in floating point"
)]
fn psnr(made: &[u8], wanted: &[u8]) -> f64 {
    let mut squared = 0u64;
    let mut samples = 0u64;
    for (made, wanted) in made
        .as_chunks::<4>()
        .0
        .iter()
        .zip(wanted.as_chunks::<4>().0)
    {
        for (&a, &b) in made.iter().zip(wanted).take(3) {
            squared += u64::from(a.abs_diff(b)).pow(2);
            samples += 1;
        }
    }
    if squared == 0 {
        return f64::INFINITY;
    }
    10.0 * (255.0 * 255.0 * samples as f64 / squared as f64).log10()
}

/// The picture `decoded` shows, packed.
fn shown(decoded: &Decoded<'_>) -> Vec<u8> {
    let width = usize::try_from(decoded.width()).expect("a width");
    let height = usize::try_from(decoded.height()).expect("a height");
    let mut rgba = vec![0; width * height * 4];
    decoded
        .rgba_rows(0..height, &mut rgba, width * 4)
        .expect("every row");
    rgba
}

/// Every sample of `picture`'s planes, borders and padding included.
fn planes(picture: &Planes) -> [&[u8]; 3] {
    [
        &picture.luma.samples,
        &picture.blue_chroma.samples,
        &picture.red_chroma.samples,
    ]
}

/// The pictures `picture`'s planes hold. The border and the spare columns are
/// left out: a frame writes them before it reads them, so a reference's say
/// nothing of its picture.
fn pictures(picture: &Planes) -> [Vec<u8>; 3] {
    [&picture.luma, &picture.blue_chroma, &picture.red_chroma].map(|plane| {
        let rows = plane.samples.chunks_exact(plane.stride).skip(1);
        rows.flat_map(|row| &row[1..=plane.width])
            .copied()
            .collect()
    })
}

/// Assert the picture a decoder holds after frame `frame` is, sample for
/// sample, the reference the encoder codes the next frame against.
fn assert_same_reference(decoded: &Decoded<'_>, encoder: &Encoder, frame: usize) {
    let read = pictures(decoded.picture).into_iter();
    for (plane, (held, kept)) in read.zip(pictures(&encoder.reference)).enumerate() {
        assert_eq!(
            first_difference(&held, &kept),
            None,
            "frame {frame}, plane {plane}"
        );
    }
}

/// What one frame came to: whether it is a keyframe, its bytes, and the
/// picture a decoder shows.
struct Coded {
    keyframe: bool,
    len: usize,
    shown: Vec<u8>,
}

/// Lanes the twins of a stream's encoder code on.
const TWIN_LANES: usize = 4;

/// An encoder on the calling thread alone, its twins on lanes run last first
/// and on real threads, and the decoder its frames are read back with.
struct Stream {
    encoder: Encoder,
    twins: [Encoder; 2],
    reversed: Reversed,
    decoder: Decoder,
    size: (u32, u32),
    stride: usize,
    frames: usize,
    out: Vec<u8>,
    twin_out: Vec<u8>,
}

impl Stream {
    /// A stream of packed `width`×`height` pictures coded at `quality`.
    fn new(width: usize, height: usize, quality: Quality) -> Self {
        let size = (side(width), side(height));
        let make = |lanes| Encoder::new(size.0, size.1, quality, lanes).expect("an encoder");
        Self {
            encoder: make(1),
            twins: [make(TWIN_LANES), make(TWIN_LANES)],
            reversed: Reversed::new(TWIN_LANES),
            decoder: Decoder::new(),
            size,
            stride: width * 4,
            frames: 0,
            out: Vec::new(),
            twin_out: Vec::new(),
        }
    }

    /// Encode the packed picture `rgba` and read the frame back: the twins
    /// must code the same bytes, the decoder must hold what the encoder does,
    /// and a keyframe must read alike through the keyframe decoder.
    fn round_trip(&mut self, rgba: &[u8], changed: &[Changed], key: bool) -> Coded {
        let encoded = self
            .encoder
            .encode(rgba, self.stride, changed, key, &mut self.out, &SERIAL)
            .expect("an encoded frame");
        let threaded = Threaded::new(TWIN_LANES);
        let runners: [&dyn JobRunner; 2] = [&self.reversed, &threaded];
        for (twin, runner) in self.twins.iter_mut().zip(runners) {
            let coded = twin.encode(rgba, self.stride, changed, key, &mut self.twin_out, runner);
            assert_eq!(coded, Ok(encoded), "frame {}", self.frames);
            let differs = first_difference(&self.twin_out, &self.out);
            assert_eq!(
                differs, None,
                "frame {}: the bytes depend on the runner",
                self.frames
            );
        }
        let decoded = self
            .decoder
            .decode(&self.out)
            .expect("a frame the decoder reads");
        assert_eq!(
            decoded.keyframe(),
            encoded.keyframe,
            "frame {}",
            self.frames
        );
        assert!(decoded.shown(), "frame {}", self.frames);
        assert_same_reference(&decoded, &self.encoder, self.frames);
        let shown = shown(&decoded);
        if encoded.keyframe {
            assert_eq!(probe(&self.out), Ok(self.size));
            let mut rows = Vec::new();
            decode_rows::<Vp8Error>(&self.out, |row| {
                rows.extend(row.iter().flatten());
                Ok(())
            })
            .expect("a keyframe the row decoder reads");
            assert_eq!(
                first_difference(&rows, &shown),
                None,
                "frame {}",
                self.frames
            );
        }
        self.frames += 1;
        Coded {
            keyframe: encoded.keyframe,
            len: self.out.len(),
            shown,
        }
    }
}

#[test]
fn every_frame_reads_back_as_the_encoders_reconstruction() {
    for (width, height) in [(1, 1), (17, 9), (48, 40)] {
        for quality in QUALITIES {
            let mut stream = Stream::new(width, height, quality);
            for frame in 0..6 {
                let rgba = picture(width, height, |x, y| scene(x, y, 3 * frame));
                let coded = stream.round_trip(&rgba, &[whole(width, height)], frame == 3);
                assert_eq!(coded.keyframe, frame == 0 || frame == 3);
            }
        }
    }
}

#[test]
fn a_picture_a_pixel_past_full_hd_reads_back() {
    let (width, height) = (1921, 1081);
    let mut stream = Stream::new(width, height, Quality::default());
    stream.round_trip(&picture(width, height, |x, y| scene(x, y, 0)), &[], false);
    let moved = |x: usize, y: usize| (900..1300).contains(&x) && (500..700).contains(&y);
    let next = picture(width, height, |x, y| {
        scene(x, y, if moved(x, y) { 5 } else { 0 })
    });
    let coded = stream.round_trip(&next, &[region(900, 500, 400, 200)], false);
    assert!(!coded.keyframe);
}

#[test]
fn thirty_frames_of_moving_content_stay_in_step() {
    let (width, height) = (80, 48);
    let mut stream = Stream::new(width, height, Quality::Standard);
    for frame in 0..30 {
        let rgba = picture(width, height, |x, y| scene(x, y, 2 * frame));
        let coded = stream.round_trip(&rgba, &[whole(width, height)], false);
        assert_eq!(coded.keyframe, frame == 0);
        let fidelity = psnr(&coded.shown, &rgba);
        assert!(fidelity >= 28.0, "frame {frame}: {fidelity:.2} dB");
    }
}

#[test]
fn a_thousand_static_frames_stay_exactly_the_first() {
    let (width, height) = (48, 32);
    let rgba = picture(width, height, gradient);
    let mut stream = Stream::new(width, height, Quality::Standard);
    stream.round_trip(&rgba, &[], false);
    let first = pictures(&stream.encoder.reference);
    for frame in 1..1000 {
        let coded = stream.round_trip(&rgba, &[], false);
        assert!(
            !coded.keyframe && coded.len < 64,
            "frame {frame}: {} bytes",
            coded.len
        );
        let now = pictures(&stream.encoder.reference);
        for (plane, (now, then)) in now.iter().zip(&first).enumerate() {
            assert_eq!(
                first_difference(now, then),
                None,
                "frame {frame}, plane {plane}"
            );
        }
    }
}

/// A picture a quality test codes: its name, its painter, and the floors in
/// decibels it holds at Compact, Standard and Fine.
type Content = (&'static str, fn(usize, usize) -> [u8; 3], [f64; 3]);

#[test]
fn each_quality_holds_its_floor_and_costs_in_its_order() {
    let contents: [Content; 3] = [
        ("flat", flat, [34.0, 39.0, 44.0]),
        ("text", text, [26.0, 31.0, 35.0]),
        ("gradient", gradient, [31.0, 33.0, 37.0]),
    ];
    for (name, paint, floors) in contents {
        let rgba = picture(WIDTH, HEIGHT, paint);
        let mut sizes = [0; 3];
        for ((quality, floor), size) in QUALITIES.into_iter().zip(floors).zip(&mut sizes) {
            let coded = Stream::new(WIDTH, HEIGHT, quality).round_trip(&rgba, &[], false);
            let fidelity = psnr(&coded.shown, &rgba);
            assert!(fidelity >= floor, "{name} at {quality:?}: {fidelity:.2} dB");
            *size = coded.len;
        }
        let ordered = sizes[0] < sizes[1] && sizes[1] < sizes[2];
        assert!(name == "flat" || ordered, "{name}: {sizes:?} bytes");
    }
}

#[test]
fn only_a_keyframe_predicts_a_subblock_at_a_time() {
    let (width, height) = (96, 64);
    let subblocked = |encoder: &Encoder| {
        encoder
            .coding
            .decisions
            .iter()
            .filter(|decided| matches!(decided.prediction, Prediction::Intra { ymode: B_PRED, .. }))
            .count()
    };
    let mut stream = Stream::new(width, height, Quality::Standard);
    stream.round_trip(&picture(width, height, text), &[], false);
    assert!(
        subblocked(&stream.encoder) > 0,
        "text is predicted a subblock at a time"
    );
    let changed = picture(width, height, |x, y| text(x + 40, y + 24));
    let coded = stream.round_trip(&changed, &[whole(width, height)], false);
    assert!(!coded.keyframe);
    assert_eq!(subblocked(&stream.encoder), 0);
}

#[test]
fn keyframes_fall_where_they_are_asked_for() {
    let (width, height) = (32, 32);
    let rgba = picture(width, height, text);
    let mut stream = Stream::new(width, height, Quality::Standard);
    for frame in 0..12 {
        let asked = frame % 4 == 2;
        let coded = stream.round_trip(&rgba, &[], asked);
        assert_eq!(coded.keyframe, frame == 0 || asked, "frame {frame}");
    }
}

#[test]
fn changed_regions_are_held_to_the_picture() {
    let (width, height) = (48, 32);
    let mut stream = Stream::new(width, height, Quality::Standard);
    stream.round_trip(&picture(width, height, text), &[], false);
    let before = planes(&stream.encoder.reference).map(<[u8]>::to_vec);
    // Everything changes, but the regions named reach past the picture, and
    // of them only the top of the last macroblock column lies in it.
    let changed = [
        region(40, 0, u32::MAX, 8),
        region(48, 0, 16, 16),
        region(0, 32, 48, 16),
        region(u32::MAX, u32::MAX, u32::MAX, u32::MAX),
    ];
    stream.round_trip(&picture(width, height, gradient), &changed, false);
    let held = &stream.encoder.reference;
    let luma = &held.luma;
    let coded =
        (0..16).any(|y| (32..48).any(|x| luma.samples[luma.at(x, y)] != before[0][luma.at(x, y)]));
    assert!(coded, "the macroblock a region touches kept the last frame");
    // That macroblock's loop filter reaches three samples into its left
    // neighbour; nothing else moves.
    let blocks = [
        (&held.luma, 16),
        (&held.blue_chroma, 8),
        (&held.red_chroma, 8),
    ];
    for ((plane, block), was) in blocks.into_iter().zip(&before) {
        for y in 0..2 * block {
            for x in (0..3 * block).filter(|&x| x < 2 * block - 3 || y >= block) {
                let at = plane.at(x, y);
                assert_eq!(
                    plane.samples[at], was[at],
                    "({x}, {y}) under {block}-sample blocks"
                );
            }
        }
    }
}

#[test]
fn a_refused_encode_changes_nothing() {
    let (width, height) = (40, 24);
    let changed = [whole(width, height)];
    let mut refused = Encoder::new(side(width), side(height), Quality::Standard, 1).expect("one");
    let mut twin = Encoder::new(side(width), side(height), Quality::Standard, 1).expect("a twin");
    for frame in 0..4 {
        let rgba = picture(width, height, |x, y| scene(x, y, 3 * frame));
        let mut grants = 0;
        let coded = loop {
            let mut out = Vec::new();
            let outcome = refusing_after(grants, || {
                refused.encode(&rgba, width * 4, &changed, false, &mut out, &SERIAL)
            });
            match outcome {
                Ok(_) => break out,
                Err(error) => {
                    assert_eq!(error, Vp8Error::OutOfMemory, "frame {frame}");
                    assert!(out.is_empty(), "a refused frame left bytes behind");
                    grants += 1;
                }
            }
        };
        assert!(grants > 0, "frame {frame} was never refused");
        let mut expected = Vec::new();
        twin.encode(&rgba, width * 4, &changed, false, &mut expected, &SERIAL)
            .expect("a frame");
        assert_eq!(coded, expected, "frame {frame} after {grants} refusals");
    }
}

#[test]
fn a_size_or_a_layout_the_picture_does_not_have_is_refused() {
    for (width, height) in [(0, 16), (16, 0), (16_384, 16), (16, 16_384), (u32::MAX, 1)] {
        let made = Encoder::new(width, height, Quality::Standard, 1);
        assert_eq!(
            made.err(),
            Some(Vp8Error::InvalidGeometry),
            "{width}×{height}"
        );
        assert_eq!(Encoder::peak_bytes(width, height, 1), 0);
    }
    assert!(Encoder::new(16_383, 1, Quality::Compact, 1).is_ok());
    let rgba = picture(16, 8, text);
    let mut encoder = Encoder::new(16, 8, Quality::Standard, 1).expect("an encoder");
    let mut out = Vec::new();
    let short = &rgba[..rgba.len() - 1];
    for (bytes, stride) in [(&rgba[..], 63), (short, 64), (&rgba[..], usize::MAX)] {
        let refused = encoder.encode(bytes, stride, &[], false, &mut out, &SERIAL);
        assert_eq!(
            refused,
            Err(Vp8Error::InvalidGeometry),
            "{stride}-byte rows"
        );
    }
    // Padding between rows is read past, and the last row needs none.
    let mut padded = Vec::new();
    for row in rgba.chunks(64) {
        padded.extend_from_slice(row);
        padded.extend_from_slice(&[0xA5; 16]);
    }
    padded.truncate(padded.len() - 16);
    let coded = encoder.encode(&padded, 80, &[], false, &mut out, &SERIAL);
    assert_eq!(coded.map(|coded| coded.keyframe), Ok(true));
    let mut twin = Encoder::new(16, 8, Quality::Standard, 1).expect("a twin");
    let mut packed = Vec::new();
    twin.encode(&rgba, 64, &[], false, &mut packed, &SERIAL)
        .expect("a packed picture");
    assert_eq!(out, packed);
}

#[test]
fn an_encoder_holds_its_forecast_and_a_frame_allocates_only_its_output() {
    for (width, height) in [(1, 1), (17, 9), (100, 37)] {
        for lanes in [1, 3] {
            let size = (side(width), side(height));
            let (made, metering) = metered(|| Encoder::new(size.0, size.1, Quality::Fine, lanes));
            let mut encoder = made.expect("an encoder");
            let forecast = Encoder::peak_bytes(size.0, size.1, lanes);
            let case = format!("{width}×{height} on {lanes} lanes");
            assert_eq!(u64::try_from(metering.peak).ok(), Some(forecast), "{case}");
            assert_eq!(metering.retained, metering.peak, "{case}");
            let rgba = picture(width, height, |x, y| scene(x, y, 0));
            let mut out = Vec::with_capacity(1 << 16);
            let runner = Reversed::new(lanes);
            for key in [true, false] {
                let changed = [whole(width, height)];
                let (coded, metering) =
                    metered(|| encoder.encode(&rgba, width * 4, &changed, key, &mut out, &runner));
                assert_eq!(coded.map(|coded| coded.keyframe), Ok(key));
                assert_eq!(metering.allocations, 0, "{case}, keyframe {key}");
            }
        }
    }
}

/// An arch over every `2 * half` samples, nought at each end and `half`
/// squared at its crown: curved, so no intra mode predicts it.
fn arch(at: usize, half: usize) -> usize {
    let phase = at % (2 * half);
    phase * (2 * half - phase)
}

/// Smooth swells of three periods, every block its own within a search's
/// reach, so a picture's error against itself moved falls toward no move.
fn swells(x: usize, y: usize) -> [u8; 3] {
    let level = |phase: usize| {
        let across = arch(x.wrapping_add(phase), 23) / 8;
        let swell = across + arch(y, 29) / 10 + arch(x.wrapping_add(y), 37) / 16;
        u8::try_from(10 + swell).unwrap_or(u8::MAX)
    };
    [level(0), level(7), level(13)]
}

/// Motion is searched from where a macroblock lies in the frame, not in the
/// band it is coded in: a picture moved by whole pixels is coded, in every
/// row, as moved by exactly that.
#[test]
fn a_moved_picture_is_coded_as_moved_by_its_shift_in_every_row() {
    let (width, height, across, down) = (96, 80, 3, 2);
    let mut stream = Stream::new(width, height, Quality::Fine);
    stream.round_trip(&picture(width, height, swells), &[], false);
    let moved = |x: usize, y: usize| swells(x.wrapping_sub(across), y.wrapping_sub(down));
    stream.round_trip(
        &picture(width, height, moved),
        &[whole(width, height)],
        false,
    );
    let shift = Mv {
        x: -8 * i16::try_from(across).expect("a small shift"),
        y: -8 * i16::try_from(down).expect("a small shift"),
    };
    let columns = width.div_ceil(16);
    for row in 1..height.div_ceil(16) - 1 {
        for column in 1..columns - 1 {
            let decided = stream.encoder.coding.decisions[row * columns + column];
            let motion = match decided.prediction {
                Prediction::Inter { motion, .. } => Some(motion),
                Prediction::Intra { .. } => None,
            };
            assert_eq!(
                motion,
                Some(Motion::Whole(shift)),
                "macroblock {column}, {row}"
            );
        }
    }
}

/// Enough coded macroblocks over enough rows split a frame's tokens into
/// eight partitions, which read back and code alike on every runner.
#[test]
fn a_large_frame_writes_its_tokens_as_eight_partitions() {
    let (width, height) = (640, 416);
    let mut stream = Stream::new(width, height, Quality::Standard);
    stream.round_trip(&picture(width, height, |x, y| scene(x, y, 0)), &[], false);
    let written = stream
        .encoder
        .coding
        .partitions
        .iter()
        .filter(|part| !part.bytes.is_empty());
    assert_eq!(written.count(), MAX_PARTITIONS);
    let rgba = picture(width, height, |x, y| scene(x, y, 4));
    stream.round_trip(&rgba, &[whole(width, height)], false);
}

/// A partition whose tokens outgrow the room it was made with is written
/// into the frame itself, the frame the same bytes as where it fit.
#[test]
fn a_partition_outgrowing_its_room_is_written_into_the_frame_alike() {
    let (width, height) = (640, 416);
    let mut stream = Stream::new(width, height, Quality::Standard);
    stream.encoder.coding.partitions[3].bytes = Vec::with_capacity(32);
    stream.round_trip(&picture(width, height, |x, y| scene(x, y, 0)), &[], false);
    assert!(stream.encoder.coding.partitions[3].overflowed);
    assert!(!stream.twins[0].coding.partitions[3].overflowed);
}

/// A token partition longer than its three-byte size names is answered as
/// such, so the frame is written as one partition, which needs no size.
#[test]
fn a_token_partition_past_its_size_asks_for_one_partition() {
    let tokens = TokenRows {
        columns: 1,
        rows: 2,
        levels: &[NO_LEVELS; 2],
        shapes: &[Shape::default(); 2],
        contexts: &[Nonzero::default(); 2],
        probs: &DEFAULT_COEFF_PROBS,
        partitions: 2,
    };
    let mut first = Partition::new(1, 0).expect("a partition");
    let mut last = Partition::new(1, 0).expect("a partition");
    first.bytes = vec![0; 1 << 24];
    let mut out = vec![1, 2, 3];
    let parts = [(0, &mut first), (1, &mut last)];
    assert_eq!(
        tokens.append(parts.into_iter(), &mut out),
        Ok(Written::Unsized)
    );
    assert!(out.is_empty());
    first.bytes.truncate((1 << 24) - 1);
    let mut out = vec![1, 2, 3];
    let parts = [(0, &mut first), (1, &mut last)];
    assert_eq!(
        tokens.append(parts.into_iter(), &mut out),
        Ok(Written::Whole)
    );
    assert_eq!(out[..6], [1, 2, 3, 0xFF, 0xFF, 0xFF]);
    assert_eq!(out.len(), 6 + (1 << 24) - 1);
}

/// However many lanes an encoder has and however wide the runner, a frame is
/// the same bytes.
#[test]
fn any_lanes_on_any_runner_code_the_same_bytes() {
    let (width, height) = (80, 64);
    let size = (side(width), side(height));
    let frames = [
        (picture(width, height, |x, y| scene(x, y, 0)), vec![], false),
        (
            picture(width, height, |x, y| scene(x, y, 2)),
            vec![whole(width, height)],
            false,
        ),
        (
            picture(width, height, |x, y| scene(x, y, 5)),
            vec![region(20, 30, 30, 20)],
            false,
        ),
        (picture(width, height, |x, y| scene(x, y, 5)), vec![], true),
    ];
    let mut alone = Encoder::new(size.0, size.1, Quality::Standard, 1).expect("an encoder");
    let reversed = Reversed::new(5);
    let cases: [(usize, &dyn JobRunner); 3] = [
        (2, &Threaded::new(6)),
        (6, &Threaded::new(2)),
        (3, &reversed),
    ];
    let mut others: Vec<Encoder> = cases
        .iter()
        .map(|&(lanes, _)| Encoder::new(size.0, size.1, Quality::Standard, lanes).expect("one"))
        .collect();
    let (mut expected, mut coded) = (Vec::new(), Vec::new());
    for (frame, (rgba, changed, key)) in frames.iter().enumerate() {
        alone
            .encode(rgba, width * 4, changed, *key, &mut expected, &SERIAL)
            .expect("a frame");
        for (other, &(lanes, runner)) in others.iter_mut().zip(&cases) {
            other
                .encode(rgba, width * 4, changed, *key, &mut coded, runner)
                .expect("a frame");
            assert_eq!(
                first_difference(&coded, &expected),
                None,
                "frame {frame}, {lanes} lanes"
            );
        }
    }
}

/// The frames the crate documentation's timings are taken from.
#[test]
fn a_full_hd_keyframe_and_a_still_inter_frame_allocate_nothing() {
    let (width, height) = (1920, 1080);
    let rgba = picture(width, height, |x, y| scene(x, y, 0));
    let mut encoder =
        Encoder::new(side(width), side(height), Quality::default(), 1).expect("an encoder");
    let mut out = Vec::with_capacity(8 << 20);
    for key in [true, false] {
        let (coded, metering) =
            metered(|| encoder.encode(&rgba, width * 4, &[], key, &mut out, &SERIAL));
        assert_eq!(coded.map(|coded| coded.keyframe), Ok(key));
        assert_eq!(metering.allocations, 0);
    }
}

#[test]
fn each_tier_of_modes_reads_back_as_the_encoders_reconstruction() {
    let (width, height) = (40, 24);
    for modes in [Modes::Chosen, Modes::Whole, Modes::Cheapest] {
        let mut encoder =
            Encoder::new(side(width), side(height), Quality::Standard, 1).expect("an encoder");
        let mut decoder = Decoder::new();
        let mut out = Vec::new();
        for frame in 0..3 {
            let rgba = picture(width, height, |x, y| scene(x, y, 4 * frame));
            let held = Picture::new(&rgba, width * 4, encoder.geometry).expect("a picture");
            encoder
                .coding
                .touch(&[whole(width, height)], encoder.geometry);
            let fitted = encoder.code(held, frame == 0, modes, &mut out, &SERIAL);
            assert_eq!(fitted, Ok(true), "{modes:?}");
            let read = decoder.decode(&out).expect("a frame the decoder reads");
            assert_eq!(read.keyframe(), frame == 0);
            assert_same_reference(&read, &encoder, frame);
        }
    }
}

#[test]
fn seeded_random_streams_read_back_as_the_encoders_reconstruction() {
    let name = "seeded_random_streams_read_back_as_the_encoders_reconstruction";
    let mut rng = Prng::new(tairix_fuzzseed::start(name, tairix_fuzzseed::FUZZ_SEED_ENV));
    for _ in 0..6 {
        let (width, height) = (1 + rng.below(56), 1 + rng.below(40));
        let mut stream = Stream::new(width, height, *rng.pick(&QUALITIES));
        let mut rgba = rng.noise(width * height * 4);
        let mut changed = whole(width, height);
        for _ in 0..4 {
            stream.round_trip(&rgba, &[changed], rng.below(5) == 0);
            let (left, top) = (rng.below(width), rng.below(height));
            let (across, down) = (1 + rng.below(width - left), 1 + rng.below(height - top));
            for row in top..top + down {
                let start = (row * width + left) * 4;
                rng.fill(&mut rgba[start..start + across * 4]);
            }
            changed = region(side(left), side(top), side(across), side(down));
        }
    }
}

#[test]
fn greys_carry_no_chroma_and_colours_come_back_within_two_levels() {
    for level in 0..=u8::MAX {
        assert_eq!(
            colour::chroma([u32::from(level) * 4; 3]),
            [128, 128],
            "grey {level}"
        );
    }
    let mut rng = Prng::new(0x5EED_3720);
    for _ in 0..20_000 {
        let pixel = [rng.next_u8(), rng.next_u8(), rng.next_u8()];
        let [blue, red] = colour::chroma(pixel.map(|channel| u32::from(channel) * 4));
        let back = colour::rgba(colour::luma(pixel), blue, red);
        for (&made, &wanted) in back.iter().zip(&pixel) {
            assert!(
                made.abs_diff(wanted) <= 2,
                "{pixel:?} came back as {back:?}"
            );
        }
    }
}

#[test]
fn the_forward_transforms_come_back_through_the_inverse_within_a_level() {
    let mut rng = Prng::new(0x5EED_3721);
    let mut drawn = |reach: i32| {
        let span = usize::try_from(2 * reach + 1).expect("a span");
        i32::try_from(rng.below(span)).expect("a value") - reach
    };
    for _ in 0..5_000 {
        let residual: [i32; 16] = core::array::from_fn(|_| drawn(255));
        let coefficients = forward_dct(&residual).map(|value| i16::try_from(value).expect("one"));
        for (&made, &wanted) in inverse_dct(&coefficients).iter().zip(&residual) {
            assert!((made - wanted).abs() <= 1, "{residual:?}");
        }
        let dc: [i32; 16] = core::array::from_fn(|_| drawn(2040));
        let walsh = forward_walsh(&dc).map(|value| i16::try_from(value).expect("one"));
        for (&made, &wanted) in inverse_walsh(&walsh).iter().zip(&dc) {
            assert!((i32::from(made) - wanted).abs() <= 1, "{dc:?}");
        }
    }
}

#[test]
fn a_tag_reads_back_as_written_and_refuses_a_partition_past_its_bits() {
    let tags = [
        (true, true, 0),
        (false, true, (1 << 19) - 1),
        (true, false, 1234),
    ];
    for (keyframe, shown, first_partition) in tags {
        let tag = Tag {
            keyframe,
            version: 0,
            shown,
            first_partition,
        };
        let bytes = tag.bytes().expect("a tag's bytes");
        assert_eq!(Tag::read(&bytes), Ok(tag));
    }
    let past = Tag {
        keyframe: true,
        version: 0,
        shown: true,
        first_partition: 1 << 19,
    };
    assert_eq!(past.bytes(), None);
}
