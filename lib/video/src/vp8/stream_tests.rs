//! VP8 stream decoder tests.
//!
//! Every stream is written with the bitstream fixture, and every expected
//! picture is worked out here: a shift sample by sample, the pictures the
//! references were left holding, or the decoder's own predictors and
//! transforms run over coefficients dequantised here.

use alloc::vec;
use alloc::vec::Vec;

use tairix_fuzzseed::meter::{metered, refusing_after, Metered};
use tairix_fuzzseed::Prng;

use super::{stream_peak_bytes, Decoded, Decoder, Geometry};
use crate::vp8::fixture::{
    drawn_inter_frame, drawn_keyframe, inter_frame, Filter, InterFrame, Movement, Predicted,
    Reference, Update, Written,
};
use crate::vp8::inter::{InterMode, MbInfo, Partitioning, Prediction};
use crate::vp8::motion::{predict_inter, Interpolation, Motion, Mv};
use crate::vp8::{
    decode_rows, filter_level, Buffer, DeltaIndex, FilterDeltas, Header, Plane, Planes,
    Probabilities, Quant, Rgba8, Segmentation, Tag, Vp8Error, B_PRED, SPLITS, TAG_BYTES, ZIGZAG,
};

/// Metered, so a stream can be held to its forecast and refused memory.
#[global_allocator]
static ALLOC: Metered = Metered;

/// The tag bit of a frame the stream shows.
const SHOWN: u8 = 1 << 4;

/// A shown keyframe of a `width`×`height` stream drawn from `seed`, its loop
/// filter at `level`.
fn keyframe(seed: u64, (width, height): (u32, u32), level: u32) -> Vec<u8> {
    let filter = Filter {
        simple: false,
        level,
        sharpness: 0,
    };
    let mut rng = Prng::new(seed);
    let mut bytes = drawn_keyframe(width, height, filter, 2, &mut || rng.next_u64());
    bytes[0] |= SHOWN;
    bytes
}

/// The macroblocks of a `width`×`height` stream.
fn macroblocks((width, height): (u32, u32)) -> usize {
    usize::try_from(width.div_ceil(16) * height.div_ceil(16)).expect("a count")
}

/// `count` still, skipped macroblocks predicting from `reference`.
fn still(reference: Reference, count: usize) -> Vec<Written> {
    let block = Written {
        predicted: Predicted::Inter {
            reference,
            motion: Movement::Still,
        },
        residual: None,
    };
    vec![block; count]
}

/// A residual of only a luma DC coefficient, `dc`, which lifts the whole
/// macroblock's luma.
fn lifted(dc: i16) -> [[i16; 16]; 25] {
    let mut blocks = [[0; 16]; 25];
    blocks[24][0] = dc;
    blocks
}

/// Every plane a decoded picture holds, borders and all.
fn planes(decoded: &Decoded<'_>) -> [Vec<u8>; 3] {
    let picture = decoded.picture;
    [&picture.luma, &picture.blue_chroma, &picture.red_chroma].map(|plane| plane.samples.clone())
}

/// `value` as a coordinate.
fn signed(value: usize) -> isize {
    isize::try_from(value).expect("a coordinate")
}

/// A picture's samples within its whole macroblocks, its borders left out.
fn inside(picture: &Planes, geometry: Geometry) -> [Vec<u8>; 3] {
    let (width, height) = geometry.luma();
    let samples = |plane: &Plane, (across, down): (usize, usize)| -> Vec<u8> {
        (0..down)
            .flat_map(|y| (0..across).map(move |x| plane.get(signed(x), signed(y))))
            .collect()
    };
    [
        samples(&picture.luma, (width, height)),
        samples(&picture.blue_chroma, (width / 2, height / 2)),
        samples(&picture.red_chroma, (width / 2, height / 2)),
    ]
}

/// A copy of `picture`, which outlives the decode that lent it.
fn copied(picture: &Planes) -> Planes {
    let copy = |plane: &Plane| Plane {
        samples: plane.samples.clone(),
        stride: plane.stride,
        width: plane.width,
    };
    Planes {
        luma: copy(&picture.luma),
        blue_chroma: copy(&picture.blue_chroma),
        red_chroma: copy(&picture.red_chroma),
    }
}

/// A decoded picture's rows, opaque RGBA8 end to end.
fn rgba(decoded: &Decoded<'_>) -> Vec<u8> {
    let line = usize::try_from(decoded.width()).expect("a width") * 4;
    let height = usize::try_from(decoded.height()).expect("a height");
    let mut out = vec![0; line * height];
    decoded
        .rgba_rows(0..height, &mut out, line)
        .expect("every row");
    out
}

/// `(x, y)` quarter pixels as a vector.
fn mv((x, y): (i16, i16)) -> Mv {
    Mv { x: 2 * x, y: 2 * y }
}

/// An inter macroblock predicting from `reference` by `motion`, as the model
/// reads one.
fn inter(reference: Buffer, motion: Motion) -> MbInfo {
    let mode = match motion {
        Motion::Whole(_) => InterMode::New,
        Motion::Split(_) => InterMode::Split(Partitioning::Each),
    };
    MbInfo {
        segment: 0,
        skip: false,
        prediction: Prediction::Inter {
            reference,
            mode,
            motion,
        },
    }
}

/// An intra macroblock, as the model reads one.
fn intra(ymode: u8, bmode: u8, uvmode: u8) -> MbInfo {
    let bmodes = [if ymode == B_PRED { bmode } else { 0 }; 16];
    MbInfo {
        segment: 0,
        skip: false,
        prediction: Prediction::Intra {
            ymode,
            uvmode,
            bmodes,
        },
    }
}

/// Whether a macroblock predicted as `predicted` codes a luma DC block.
fn codes_luma_dc(predicted: Predicted) -> bool {
    match predicted {
        Predicted::Intra { ymode, .. } => ymode != B_PRED,
        Predicted::Inter { motion, .. } => !matches!(motion, Movement::Split { .. }),
    }
}

/// A residual drawn from `rng` for a macroblock coding a luma DC block where
/// `has_y2` says: a few coefficients a block, of magnitudes reaching every
/// token category.
fn residual(rng: &mut Prng, has_y2: bool) -> [[i16; 16]; 25] {
    const MAGNITUDES: [i16; 10] = [1, 2, 3, 4, 6, 9, 15, 27, 50, 300];
    let mut blocks = [[0i16; 16]; 25];
    for (index, block) in blocks.iter_mut().enumerate() {
        if index == 24 && !has_y2 {
            continue;
        }
        let first = usize::from(has_y2 && index < 16);
        for coefficient in block.iter_mut().skip(first) {
            if rng.below(5) == 0 {
                let magnitude = *rng.pick(&MAGNITUDES);
                *coefficient = if rng.next_u64() & 1 == 0 {
                    magnitude
                } else {
                    -magnitude
                };
            }
        }
    }
    blocks
}

/// A quantiser of index `index` and no deltas.
fn quant(index: i32) -> Quant {
    Quant {
        index,
        y_dc: 0,
        y2_dc: 0,
        y2_ac: 0,
        uv_dc: 0,
        uv_ac: 0,
    }
}

/// The coefficients `residual` codes, dequantised at `quantiser` and put in
/// place as a decoder holds them.
fn dequantised(
    residual: Option<&[[i16; 16]; 25]>,
    has_y2: bool,
    quantiser: u32,
) -> [[i16; 16]; 25] {
    let index = i32::try_from(quantiser).expect("a quantiser index");
    let factors = quant(index).factors(index);
    let mut coeffs = [[0i16; 16]; 25];
    let Some(blocks) = residual else {
        return coeffs;
    };
    for (index, (block, out)) in blocks.iter().zip(&mut coeffs).enumerate() {
        let (dc, ac) = match index {
            24 if has_y2 => (factors.y2_dc, factors.y2_ac),
            24 => continue,
            16..=23 => (factors.uv_dc, factors.uv_ac),
            _ => (factors.y_dc, factors.y_ac),
        };
        let first = usize::from(has_y2 && index < 16);
        for (scan, &value) in block.iter().enumerate().skip(first) {
            let factor = if scan == 0 { dc } else { ac };
            let product = i32::from(value) * factor;
            out[usize::from(ZIGZAG[scan])] = i16::try_from(product).expect("a coefficient");
        }
    }
    coeffs
}

/// What a frame of `written` macroblocks, read as `decided`, makes of
/// `reference` with `interpolation` before any loop filter: each predicted
/// and its residual added, through the decoder's own predictors and
/// transforms.
fn modelled(
    reference: &Planes,
    geometry: Geometry,
    (written, chosen): (&[Written], &[MbInfo]),
    quantiser: u32,
    interpolation: Interpolation,
) -> Planes {
    let (width, height) = geometry.luma();
    let views = reference.view(width, height).expect("a reference");
    let mut model = geometry.picture().expect("a picture");
    for (index, (block, decision)) in written.iter().zip(chosen).enumerate() {
        let (column, row) = (index % geometry.columns, index / geometry.columns);
        let origin = (signed(column * 16), signed(row * 16));
        let has_y2 = decision.has_y2();
        let mut coeffs = dequantised(block.residual.as_ref(), has_y2, quantiser);
        match decision.prediction {
            Prediction::Intra {
                ymode,
                uvmode,
                bmodes,
            } => {
                let wanted = (ymode, &bmodes, uvmode);
                model.reconstruct_intra(wanted, origin, row > 0, &mut coeffs);
            }
            Prediction::Inter { motion, .. } => {
                let planes = [
                    &mut model.luma,
                    &mut model.blue_chroma,
                    &mut model.red_chroma,
                ];
                predict_inter(
                    &views,
                    &motion,
                    interpolation,
                    (column, row),
                    planes,
                    origin.1,
                );
                model.add_macroblock_residual(origin, has_y2, &mut coeffs);
            }
        }
        if column + 1 == geometry.columns {
            model.luma.extend_row(origin.1 + 15);
        }
    }
    model
}

#[test]
fn a_stream_opens_with_a_keyframe() {
    let size = (16, 16);
    let next = inter_frame(16, 16, &InterFrame::default(), &still(Reference::Last, 1));
    let mut stream = Decoder::new();
    assert_eq!(stream.decode(&next).err(), Some(Vp8Error::Interframe));
    let decoded = stream.decode(&keyframe(1, size, 0)).expect("a keyframe");
    assert!(decoded.keyframe() && decoded.shown());
    assert_eq!((decoded.width(), decoded.height()), size);
    let decoded = stream.decode(&next).expect("an inter frame");
    assert!(!decoded.keyframe() && decoded.shown());
}

/// A frame of skipped macroblocks standing still reproduces its reference's
/// macroblocks exactly. Its borders are not compared: they hold the unfiltered
/// rows its own intra prediction would read, which a filtered reference's copy
/// does not share.
#[test]
fn still_skipped_frames_reproduce_their_reference_exactly() {
    for (size, level) in [((48, 32), 0), ((40, 24), 30), ((17, 33), 63)] {
        let geometry = Geometry::new(size.0, size.1);
        let mut stream = Decoder::new();
        let key = stream
            .decode(&keyframe(2, size, level))
            .expect("a keyframe");
        let expected = inside(key.picture, geometry);
        let count = macroblocks(size);
        for reference in [Reference::Last, Reference::Golden, Reference::Alternate] {
            let blocks = still(reference, count);
            let frame = inter_frame(size.0, size.1, &InterFrame::default(), &blocks);
            let decoded = stream.decode(&frame).expect("a still frame");
            let made = inside(decoded.picture, geometry);
            assert_eq!(made, expected, "{size:?} from {reference:?}");
        }
    }
}

/// A keyframe decoded whole into a stream's picture is the picture the
/// keyframe decoder hands over a row at a time.
#[test]
fn a_keyframes_rows_are_the_row_decoders_to_the_bit() {
    let filters = [
        (false, 0, 0),
        (false, 20, 0),
        (false, 63, 5),
        (true, 30, 2),
        (true, 63, 7),
    ];
    let sizes = [(40, 37), (33, 50), (64, 16), (17, 49), (16, 16), (1, 33)];
    let mut seed = 0x5EED_0C00_u64;
    for (width, height) in sizes {
        for (simple, level, sharpness) in filters {
            for partitions in [1, 2, 8] {
                seed += 1;
                let mut rng = Prng::new(seed);
                let filter = Filter {
                    simple,
                    level,
                    sharpness,
                };
                let bytes =
                    drawn_keyframe(width, height, filter, partitions, &mut || rng.next_u64());
                let mut rows = Vec::new();
                decode_rows(&bytes, |row: &[Rgba8]| {
                    rows.extend_from_slice(row.as_flattened());
                    Ok::<(), Vp8Error>(())
                })
                .expect("a drawn keyframe");
                let mut stream = Decoder::new();
                let decoded = stream.decode(&bytes).expect("a drawn keyframe");
                assert_eq!(rgba(&decoded), rows, "seed {seed:#x}");
            }
        }
    }
}

/// A whole-pixel vector moves the reference's samples by its length, every
/// sample it reaches past the reference's whole macroblocks its nearest edge's.
#[test]
fn a_whole_pixel_vector_moves_the_picture_repeating_its_edges() {
    let size = (40, 24);
    let geometry = Geometry::new(size.0, size.1);
    let (width, height) = geometry.luma();
    let mut stream = Decoder::new();
    let key = keyframe(3, size, 20);
    let reference = inside(stream.decode(&key).expect("a keyframe").picture, geometry);
    let kept = InterFrame {
        refresh_last: false,
        ..InterFrame::default()
    };
    for (dx, dy) in [(8, 4), (-6, 10), (-40, -36), (2, -2)] {
        let moving = Written {
            predicted: Predicted::Inter {
                reference: Reference::Last,
                motion: Movement::New(4 * dx, 4 * dy),
            },
            residual: None,
        };
        let bytes = inter_frame(size.0, size.1, &kept, &vec![moving; macroblocks(size)]);
        let moved = inside(
            stream.decode(&bytes).expect("a moving frame").picture,
            geometry,
        );
        for (plane, scale) in [(0, 1), (1, 2), (2, 2)] {
            let (across, down) = (width / scale, height / scale);
            let from = |at: usize, by: i16, extent: usize| {
                let moved = signed(at) + isize::from(by) / signed(scale);
                usize::try_from(moved.clamp(0, signed(extent) - 1)).expect("inside")
            };
            let expected: Vec<u8> = (0..down)
                .flat_map(|y| (0..across).map(move |x| (x, y)))
                .map(|(x, y)| reference[plane][from(y, dy, down) * across + from(x, dx, across)])
                .collect();
            assert_eq!(moved[plane], expected, "by ({dx}, {dy}), plane {plane}");
        }
    }
}

/// New, nearest and near vectors and the intra macroblocks beside them, each
/// with a residual, reconstruct as the predictors and transforms make them
/// of the coefficients coded, whichever interpolation the version names.
#[test]
fn vectors_and_residuals_reconstruct_through_the_predictors_and_transforms() {
    let size = (48, 32);
    let geometry = Geometry::new(size.0, size.1);
    let (a, b) = ((5, -3), (-7, 9));
    let from_last = |motion| Predicted::Inter {
        reference: Reference::Last,
        motion,
    };
    let predicted = [
        from_last(Movement::New(a.0, a.1)),
        from_last(Movement::Nearest),
        Predicted::Intra {
            ymode: 0,
            bmode: 0,
            uvmode: 0,
        },
        from_last(Movement::New(b.0, b.1)),
        from_last(Movement::Near),
        Predicted::Intra {
            ymode: B_PRED,
            bmode: 4,
            uvmode: 3,
        },
    ];
    // The nearest is the vector to its left; below that one and beside the
    // second, the near is the vector to its left again.
    let chosen = [
        inter(Buffer::Last, Motion::Whole(mv(a))),
        inter(Buffer::Last, Motion::Whole(mv(a))),
        intra(0, 0, 0),
        inter(Buffer::Last, Motion::Whole(mv(b))),
        inter(Buffer::Last, Motion::Whole(mv(b))),
        intra(B_PRED, 4, 3),
    ];
    let mut rng = Prng::new(0x5EED_0E00);
    let written: Vec<Written> = predicted
        .iter()
        .map(|&predicted| Written {
            predicted,
            residual: Some(residual(&mut rng, codes_luma_dc(predicted))),
        })
        .collect();
    let frame = InterFrame {
        quantiser: 20,
        ..InterFrame::default()
    };
    let coded = inter_frame(size.0, size.1, &frame, &written);
    for version in 0..4u8 {
        let mut bytes = coded.clone();
        bytes[0] = (bytes[0] & !0b1110) | (version << 1);
        let interpolation = Interpolation::of_version(u32::from(version)).expect("a version");
        let mut stream = Decoder::new();
        let key = keyframe(5, size, 10);
        let reference = copied(stream.decode(&key).expect("a keyframe").picture);
        let model = modelled(&reference, geometry, (&written, &chosen), 20, interpolation);
        let decoded = stream.decode(&bytes).expect("an inter frame");
        let made = inside(decoded.picture, geometry);
        assert_eq!(made, inside(&model, geometry), "version {version}");
    }
}

/// Split macroblocks of every partitioning, one of them skipped, move each
/// part by its own vector, whether coded anew or taken from beside it.
#[test]
fn split_macroblocks_move_each_part_by_its_own_vector() {
    let size = (32, 32);
    let geometry = Geometry::new(size.0, size.1);
    let parts = |list: &[(i16, i16)]| {
        let mut parts = [(0, 0); 16];
        parts[..list.len()].copy_from_slice(list);
        parts
    };
    let each = [
        (4, 2),
        (4, 2),
        (-3, 1),
        (0, 0),
        (6, -5),
        (6, -5),
        (1, 1),
        (2, 3),
        (-8, 4),
        (0, 0),
        (4, 4),
        (5, 5),
        (-1, -1),
        (7, 7),
        (0, 2),
        (3, 3),
    ];
    let splits = [
        (3, parts(&each)),
        (0, parts(&[(4, 2), (-6, 2)])),
        (1, parts(&[(0, 0), (9, -9)])),
        (2, parts(&[(4, 4), (4, 4), (-2, 6), (1, -7)])),
    ];
    let mut rng = Prng::new(0x5EED_0F00);
    let written: Vec<Written> = splits
        .iter()
        .enumerate()
        .map(|(index, &(partitioning, parts))| Written {
            predicted: Predicted::Inter {
                reference: Reference::Last,
                motion: Movement::Split {
                    partitioning,
                    parts,
                },
            },
            // The second is skipped, which leaves the luma DC block's
            // context to pass it by.
            residual: (index != 1).then(|| residual(&mut rng, false)),
        })
        .collect();
    let chosen: Vec<MbInfo> = splits
        .iter()
        .map(|&(partitioning, parts)| {
            let owners = SPLITS[usize::from(partitioning)];
            let mvs = core::array::from_fn(|block| mv(parts[usize::from(owners[block])]));
            inter(Buffer::Last, Motion::Split(mvs))
        })
        .collect();
    let frame = InterFrame {
        quantiser: 30,
        ..InterFrame::default()
    };
    let bytes = inter_frame(size.0, size.1, &frame, &written);
    let mut stream = Decoder::new();
    let key = keyframe(6, size, 0);
    let reference = copied(stream.decode(&key).expect("a keyframe").picture);
    let model = modelled(
        &reference,
        geometry,
        (&written, &chosen),
        30,
        Interpolation::SixTap,
    );
    let decoded = stream.decode(&bytes).expect("a split frame");
    assert_eq!(inside(decoded.picture, geometry), inside(&model, geometry));
}

/// What `decoder` shows of `reference` for a still frame of a `size` stream
/// that leaves the references be.
fn showing(stream: &mut Decoder, size: (u32, u32), reference: Reference) -> [Vec<u8>; 3] {
    let kept = InterFrame {
        refresh_last: false,
        ..InterFrame::default()
    };
    let frame = inter_frame(size.0, size.1, &kept, &still(reference, macroblocks(size)));
    planes(&stream.decode(&frame).expect("a still frame"))
}

#[test]
fn golden_and_alternate_take_their_copies_before_the_refreshes() {
    let size = (32, 32);
    let count = macroblocks(size);
    let lift = |dc: i16| {
        let block = Written {
            predicted: Predicted::Inter {
                reference: Reference::Last,
                motion: Movement::Still,
            },
            residual: Some(lifted(dc)),
        };
        vec![block; count]
    };
    let write = |frame: InterFrame, blocks: &[Written]| inter_frame(size.0, size.1, &frame, blocks);
    let kept = InterFrame {
        refresh_last: false,
        ..InterFrame::default()
    };
    let mut stream = Decoder::new();
    let key = planes(&stream.decode(&keyframe(7, size, 0)).expect("a keyframe"));
    let into_golden = InterFrame {
        golden: Update::Refreshed,
        ..kept
    };
    let golden = planes(
        &stream
            .decode(&write(into_golden, &lift(40)))
            .expect("golden"),
    );
    let last = planes(
        &stream
            .decode(&write(InterFrame::default(), &lift(-40)))
            .expect("last"),
    );
    assert!(golden != key && last != key && golden != last);
    assert_eq!(showing(&mut stream, size, Reference::Last), last);
    assert_eq!(showing(&mut stream, size, Reference::Golden), golden);
    assert_eq!(showing(&mut stream, size, Reference::Alternate), key);
    // Each copies the other: the alternate takes the golden first, then the
    // golden the alternate as just copied, so both hold the golden.
    let swapped = InterFrame {
        golden: Update::CopiedFromOther,
        alternate: Update::CopiedFromOther,
        ..kept
    };
    stream
        .decode(&write(swapped, &still(Reference::Last, count)))
        .expect("the copies");
    assert_eq!(showing(&mut stream, size, Reference::Alternate), golden);
    assert_eq!(showing(&mut stream, size, Reference::Golden), golden);
    // A copy of the last beside a refresh.
    let refreshing = InterFrame {
        golden: Update::Refreshed,
        alternate: Update::CopiedFromLast,
        ..kept
    };
    let fresh = planes(
        &stream
            .decode(&write(refreshing, &lift(20)))
            .expect("a refresh"),
    );
    assert_eq!(showing(&mut stream, size, Reference::Golden), fresh);
    assert_eq!(showing(&mut stream, size, Reference::Alternate), last);
    assert_eq!(showing(&mut stream, size, Reference::Last), last);
}

/// A neighbour predicting from a frame whose vectors point the other way
/// lends its vector reversed, and one whose point the same way as is.
#[test]
fn a_neighbour_against_the_sign_bias_lends_its_vector_reversed() {
    let size = (32, 16);
    let geometry = Geometry::new(size.0, size.1);
    let vector = (6, -4);
    let written = [
        Written {
            predicted: Predicted::Inter {
                reference: Reference::Golden,
                motion: Movement::New(vector.0, vector.1),
            },
            residual: None,
        },
        Written {
            predicted: Predicted::Inter {
                reference: Reference::Last,
                motion: Movement::Nearest,
            },
            residual: None,
        },
    ];
    for bias in [false, true] {
        let lent = if bias {
            mv((-vector.0, -vector.1))
        } else {
            mv(vector)
        };
        let chosen = [
            inter(Buffer::Golden, Motion::Whole(mv(vector))),
            inter(Buffer::Last, Motion::Whole(lent)),
        ];
        let frame = InterFrame {
            sign_bias: [bias, false],
            ..InterFrame::default()
        };
        let bytes = inter_frame(size.0, size.1, &frame, &written);
        let mut stream = Decoder::new();
        let key = keyframe(8, size, 0);
        let reference = copied(stream.decode(&key).expect("a keyframe").picture);
        let model = modelled(
            &reference,
            geometry,
            (&written, &chosen),
            0,
            Interpolation::SixTap,
        );
        let decoded = stream.decode(&bytes).expect("an inter frame");
        assert_eq!(
            inside(decoded.picture, geometry),
            inside(&model, geometry),
            "bias {bias}"
        );
    }
}

#[test]
fn an_unshown_frame_is_decoded_and_predicted_from() {
    let size = (16, 16);
    let mut stream = Decoder::new();
    stream.decode(&keyframe(9, size, 0)).expect("a keyframe");
    let hidden = InterFrame {
        shown: false,
        ..InterFrame::default()
    };
    let lifting = [Written {
        predicted: Predicted::Inter {
            reference: Reference::Last,
            motion: Movement::Still,
        },
        residual: Some(lifted(30)),
    }];
    let decoded = stream
        .decode(&inter_frame(16, 16, &hidden, &lifting))
        .expect("an unshown frame");
    assert!(!decoded.shown());
    let made = planes(&decoded);
    let after = inter_frame(16, 16, &InterFrame::default(), &still(Reference::Last, 1));
    let decoded = stream.decode(&after).expect("a still frame");
    assert!(decoded.shown());
    assert_eq!(planes(&decoded), made);
}

/// A frame refused part-way — cut short, of a reserved version, or naming a
/// partition it does not hold — leaves its references, segment map and
/// carried probabilities as they were, and the next frame decodes as though
/// it never came.
#[test]
fn a_refused_frame_leaves_the_stream_as_it_was() {
    let size = (32, 32);
    let key = keyframe(10, size, 20);
    let mut rng = Prng::new(0x5EED_1000);
    let moving: Vec<Written> = [(-4, 5), (-1, 5), (2, 5), (5, 5)]
        .iter()
        .map(|&(x, y)| Written {
            predicted: Predicted::Inter {
                reference: Reference::Last,
                motion: Movement::New(x, y),
            },
            residual: Some(residual(&mut rng, true)),
        })
        .collect();
    let into_golden = InterFrame {
        quantiser: 30,
        golden: Update::Refreshed,
        ..InterFrame::default()
    };
    let next = inter_frame(size.0, size.1, &into_golden, &moving);
    let after = inter_frame(
        size.0,
        size.1,
        &InterFrame::default(),
        &still(Reference::Golden, 4),
    );
    let mut baseline = Decoder::new();
    baseline.decode(&key).expect("a keyframe");
    let expected_next = planes(&baseline.decode(&next).expect("the frame"));
    let expected_after = planes(&baseline.decode(&after).expect("the frame after"));
    let goes_on = |stream: &mut Decoder, case: usize| {
        let decoded = planes(&stream.decode(&next).expect("the whole frame"));
        assert_eq!(decoded, expected_next, "case {case}");
        let decoded = planes(&stream.decode(&after).expect("the frame after"));
        assert_eq!(decoded, expected_after, "case {case}");
    };
    for cut in 0..next.len() {
        let mut stream = Decoder::new();
        stream.decode(&key).expect("a keyframe");
        let outcome = stream.decode(&next[..cut]).map(|decoded| planes(&decoded));
        match outcome {
            // Only the padding that closes the last partition was cut.
            Ok(decoded) => {
                assert_eq!(decoded, expected_next, "cut at {cut}");
                let decoded = planes(&stream.decode(&after).expect("the frame after"));
                assert_eq!(decoded, expected_after, "cut at {cut}");
            }
            Err(_) => goes_on(&mut stream, cut),
        }
    }
    let mut reserved = next.clone();
    reserved[0] |= 0b1110;
    let drawn = drawn_inter_frame(size.0, size.1, &into_golden, 2, &mut || rng.next_u64());
    let first = Tag::read(&drawn).expect("a tag").first_partition;
    let mut overlong = drawn;
    overlong[TAG_BYTES + first..TAG_BYTES + first + 3].fill(0xFF);
    let refused = [
        (reserved, Vp8Error::UnsupportedProfile),
        (overlong, Vp8Error::InvalidPartitions),
    ];
    for (case, (bad, refusal)) in refused.into_iter().enumerate() {
        let mut stream = Decoder::new();
        stream.decode(&key).expect("a keyframe");
        assert_eq!(stream.decode(&bad).err(), Some(refusal), "case {case}");
        goes_on(&mut stream, case);
    }
}

#[test]
fn a_keyframe_changes_the_size_and_a_refused_one_keeps_the_old() {
    let (small, wide, tall) = ((16, 16), (48, 32), (64, 80));
    let mut stream = Decoder::new();
    stream.decode(&keyframe(11, small, 0)).expect("a keyframe");
    let still_small = inter_frame(16, 16, &InterFrame::default(), &still(Reference::Last, 1));
    stream.decode(&still_small).expect("an inter frame");
    let decoded = stream
        .decode(&keyframe(12, wide, 0))
        .expect("a wider keyframe");
    assert_eq!((decoded.width(), decoded.height()), wide);
    let expected = planes(&decoded);
    let blocks = still(Reference::Golden, macroblocks(wide));
    let still_wide = inter_frame(wide.0, wide.1, &InterFrame::default(), &blocks);
    assert_eq!(
        planes(&stream.decode(&still_wide).expect("the new size")),
        expected
    );
    let taller = keyframe(13, tall, 0);
    assert!(stream.decode(&taller[..taller.len() / 2]).is_err());
    // Its partitions' readers, the walk's eight contexts and maps, then its
    // picture's three planes.
    for granted in 0..12 {
        let refused = refusing_after(granted, || stream.decode(&taller).err());
        assert_eq!(refused, Some(Vp8Error::OutOfMemory), "{granted} granted");
    }
    let decoded = stream.decode(&still_wide).expect("the old size goes on");
    assert_eq!((decoded.width(), decoded.height()), wide);
    assert_eq!(planes(&decoded), expected);
}

/// A header filtering at `level`, with `segmentation` and `deltas`.
fn filtered(level: i32, segmentation: Segmentation, deltas: FilterDeltas) -> Header {
    Header {
        width: 16,
        height: 16,
        simple_filter: false,
        filter_level: level,
        sharpness: 0,
        segmentation,
        deltas,
        quant: quant(0),
        partitions: 1,
        probs: Probabilities::DEFAULT,
        skip_probability: None,
        refresh_entropy: true,
        inter: None,
    }
}

#[test]
fn a_macroblocks_filter_level_takes_its_segments_its_references_and_its_modes_deltas() {
    let mut classes = vec![
        (intra(0, 0, 0), 0, None),
        (intra(3, 0, 1), 0, None),
        (intra(B_PRED, 5, 0), 0, Some(0)),
    ];
    let modes = [
        (InterMode::Zero, 1),
        (InterMode::Nearest, 2),
        (InterMode::Near, 2),
        (InterMode::New, 2),
        (InterMode::Split(Partitioning::Quarters), 3),
    ];
    for (reference, reference_index) in [
        (Buffer::Last, 1),
        (Buffer::Golden, 2),
        (Buffer::Alternate, 3),
    ] {
        for (mode, mode_index) in modes {
            let prediction = Prediction::Inter {
                reference,
                mode,
                motion: Motion::Whole(Mv::default()),
            };
            let chosen = MbInfo {
                segment: 0,
                skip: false,
                prediction,
            };
            classes.push((chosen, reference_index, Some(mode_index)));
        }
    }
    let (reference_deltas, mode_deltas) = ([5, -9, 17, -40], [11, -3, 8, -63]);
    let features = [
        ([0; 4], false),
        ([30, -30, 63, -63], false),
        ([10, 40, 63, 0], true),
    ];
    for (filter, absolute) in features {
        for (enabled, with_deltas) in [(false, false), (false, true), (true, false), (true, true)] {
            for level in [0, 20, 63] {
                let segmentation = Segmentation {
                    enabled,
                    update_map: false,
                    absolute,
                    quant: [0; 4],
                    filter,
                    probs: [255; 3],
                };
                let adjustments = FilterDeltas {
                    enabled: with_deltas,
                    reference: reference_deltas,
                    mode: mode_deltas,
                };
                let header = filtered(level, segmentation, adjustments);
                for segment in 0..4u8 {
                    let own = filter[usize::from(segment)];
                    let segment_level = match (enabled, absolute) {
                        (false, _) => level,
                        (true, true) => own.clamp(0, 63),
                        (true, false) => (level + own).clamp(0, 63),
                    };
                    for &(chosen, reference_index, mode_index) in &classes {
                        let found = chosen.filter_deltas();
                        let index = DeltaIndex {
                            reference: reference_index,
                            mode: mode_index,
                        };
                        assert_eq!(found, index);
                        let adjusted = if with_deltas {
                            let by_mode = mode_index.map_or(0, |at| mode_deltas[at]);
                            segment_level + reference_deltas[reference_index] + by_mode
                        } else {
                            segment_level
                        };
                        let made = filter_level(&header, segment, found);
                        assert_eq!(
                            made,
                            adjusted.clamp(0, 63),
                            "{index:?} in {segment} of {filter:?}"
                        );
                    }
                }
            }
        }
    }
}

/// An inter frame of a `size` stream drawn from `rng`, leaving the golden
/// frame as `golden` says and becoming the last where `refresh_last` does,
/// over `partitions` residual partitions.
fn drawn(
    rng: &mut Prng,
    size: (u32, u32),
    (golden, refresh_last): (Update, bool),
    partitions: usize,
) -> Vec<u8> {
    let frame = InterFrame {
        golden,
        refresh_last,
        ..InterFrame::default()
    };
    drawn_inter_frame(size.0, size.1, &frame, partitions, &mut || rng.next_u64())
}

/// Decoding into a fourth picture over eight partitions, beside three
/// references in pictures of their own, is the most a stream holds, and
/// every byte of it is in the forecast.
#[test]
fn a_stream_holds_every_byte_its_forecast_says_and_no_more() {
    let size = (40, 24);
    let bound = stream_peak_bytes(size.0, size.1).expect("a forecast");
    let mut rng = Prng::new(0x5EED_1300);
    let frames = [
        keyframe(14, size, 10),
        drawn(&mut rng, size, (Update::Kept, true), 1),
        drawn(&mut rng, size, (Update::Refreshed, false), 2),
        drawn(&mut rng, size, (Update::Kept, true), 8),
    ];
    let mut stream = Decoder::new();
    let ((), metering) = metered(|| {
        for frame in &frames {
            stream.decode(frame).expect("a drawn frame");
        }
    });
    assert_eq!(u64::try_from(metering.peak).expect("a size"), bound);
    // A keyframe changing the size holds no more than the new size's
    // forecast beyond what the stream held.
    let wider = (64, 48);
    let changing = keyframe(15, wider, 0);
    let ((), metering) = metered(|| {
        stream.decode(&changing).expect("a wider keyframe");
    });
    let forecast = stream_peak_bytes(wider.0, wider.1).expect("a forecast");
    assert!(u64::try_from(metering.peak).expect("a size") <= forecast);
    assert_eq!(stream_peak_bytes(0, 16), None);
    assert_eq!(stream_peak_bytes(16, 1 << 14), None);
}

#[test]
fn a_frame_refused_its_memory_leaves_the_stream_as_it_was() {
    let size = (32, 16);
    let mut rng = Prng::new(0x5EED_1400);
    let frames = [
        keyframe(16, size, 0),
        drawn(&mut rng, size, (Update::Kept, true), 1),
        drawn(&mut rng, size, (Update::Refreshed, false), 1),
        drawn(&mut rng, size, (Update::Kept, true), 4),
        drawn(&mut rng, size, (Update::Kept, true), 1),
    ];
    let mut baseline = Decoder::new();
    let expected: Vec<[Vec<u8>; 3]> = frames
        .iter()
        .map(|frame| planes(&baseline.decode(frame).expect("a drawn frame")))
        .collect();
    let mut stream = Decoder::new();
    for frame in &frames[..3] {
        stream.decode(frame).expect("a drawn frame");
    }
    // The fourth asks for its partitions' readers, then a picture of its own.
    for granted in 0..4 {
        let refused = refusing_after(granted, || stream.decode(&frames[3]).err());
        assert_eq!(refused, Some(Vp8Error::OutOfMemory), "{granted} granted");
    }
    for (frame, expected) in frames[3..].iter().zip(&expected[3..]) {
        assert_eq!(
            &planes(&stream.decode(frame).expect("a drawn frame")),
            expected
        );
    }
}

#[test]
fn rgba_rows_refuse_rows_or_room_the_picture_does_not_have() {
    let mut stream = Decoder::new();
    let decoded = stream.decode(&keyframe(17, (5, 3), 0)).expect("a keyframe");
    let whole = rgba(&decoded);
    let mut out = vec![0u8; 64];
    let refused = Err(Vp8Error::InvalidGeometry);
    assert_eq!(decoded.rgba_rows(0..4, &mut out, 20), refused);
    assert_eq!(decoded.rgba_rows(0..1, &mut out, 19), refused);
    assert_eq!(decoded.rgba_rows(0..3, &mut out[..59], 20), refused);
    assert!(
        out.iter().all(|&byte| byte == 0),
        "nothing is written on a refusal"
    );
    assert_eq!(decoded.rgba_rows(1..1, &mut [], 20), Ok(()));
    assert_eq!(decoded.rgba_rows(1..3, &mut out, 32), Ok(()));
    assert_eq!(&out[..20], &whole[20..40]);
    assert!(
        out[20..32].iter().all(|&byte| byte == 0),
        "the stride's slack is left be"
    );
    assert_eq!(&out[32..52], &whole[40..60]);
}

/// Drawn inter frames — every mode, vector, partitioning and residual the
/// bits can name — decode alike in two decoders, and one cut short or with a
/// bit flipped is read or refused, and refused, leaves the stream to read
/// the frame whole as it would have.
#[test]
fn drawn_inter_frames_decode_alike_and_a_damaged_one_is_read_or_refused() {
    let mut rng = Prng::new(0x5EED_1500);
    let updates = [
        Update::Kept,
        Update::Refreshed,
        Update::CopiedFromLast,
        Update::CopiedFromOther,
    ];
    for size in [(16, 16), (40, 24), (33, 17)] {
        let mut frames = vec![keyframe(rng.next_u64(), size, 20)];
        for _ in 0..5 {
            let frame = InterFrame {
                filter: Filter {
                    simple: rng.next_u64() & 1 == 1,
                    level: u32::from(rng.next_u8() % 64),
                    sharpness: u32::from(rng.next_u8() % 8),
                },
                quantiser: u32::from(rng.next_u8() % 128),
                golden: *rng.pick(&updates),
                alternate: *rng.pick(&updates),
                sign_bias: [rng.next_u64() & 1 == 1, rng.next_u64() & 1 == 1],
                refresh_last: rng.next_u64() & 1 == 1,
                shown: true,
            };
            let partitions = *rng.pick(&[1, 2, 4, 8]);
            frames.push(drawn_inter_frame(
                size.0,
                size.1,
                &frame,
                partitions,
                &mut || rng.next_u64(),
            ));
        }
        let (mut first, mut second) = (Decoder::new(), Decoder::new());
        let mut expected = Vec::new();
        for frame in &frames {
            let one = planes(&first.decode(frame).expect("a drawn frame"));
            assert_eq!(planes(&second.decode(frame).expect("a drawn frame")), one);
            expected.push(one);
        }
        for (index, frame) in frames.iter().enumerate().skip(1) {
            for _ in 0..8 {
                let mut stream = Decoder::new();
                for before in &frames[..index] {
                    stream.decode(before).expect("a drawn frame");
                }
                let mut bad = frame.clone();
                if rng.next_u64() & 1 == 0 {
                    bad.truncate(rng.below(bad.len()));
                } else {
                    let at = rng.below(bad.len());
                    bad[at] ^= 1 << rng.below(8);
                }
                if stream.decode(&bad).is_err() {
                    let decoded = planes(&stream.decode(frame).expect("the frame whole"));
                    assert_eq!(decoded, expected[index], "{size:?}, frame {index}");
                }
            }
        }
    }
}
