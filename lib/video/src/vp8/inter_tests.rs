//! Inter-frame macroblock syntax tests.
//!
//! Every bitstream is written with the format's own boolean encoder and tree
//! writer, and every expected vector, count and probability context is worked
//! out here from the format's rules rather than taken from the module.

use alloc::vec;
use alloc::vec::Vec;

use super::{
    find_near, read_macroblock, read_mv, read_row, InterMode, MbInfo, Neighbours, Partitioning,
    Prediction, Proposed, Reach, ZERO,
};
use crate::vp8::motion::{Motion, Mv};
use crate::vp8::writer::{write_mv, write_tree, Writer};
use crate::vp8::{
    Bool, Buffer, BufferUpdate, Buffers, FilterDeltas, Header, InterHeader, Probabilities, Quant,
    Segmentation, BMODE_PROBS, BMODE_TREE, B_PRED, DEFAULT_MV_PROBS, MODE_CONTEXTS, MV_PROBS,
    MV_REF_TREE, SPLITS, SPLIT_PROBS, SPLIT_TREE, SUB_MV_REF_PROBS, SUB_MV_REF_TREE, UV_MODE_TREE,
    YMODE_TREE,
};
use tairix_fuzzseed::Prng;

/// An inter frame's header carrying nothing from the stream: no segmentation,
/// no skipping, the default probabilities and even reference probabilities.
fn frame() -> Header {
    Header {
        width: 64,
        height: 64,
        simple_filter: false,
        filter_level: 0,
        sharpness: 0,
        segmentation: Segmentation::none(),
        deltas: FilterDeltas::none(),
        quant: Quant {
            index: 0,
            y_dc: 0,
            y2_dc: 0,
            y2_ac: 0,
            uv_dc: 0,
            uv_ac: 0,
        },
        partitions: 1,
        probs: Probabilities::DEFAULT,
        skip_probability: None,
        refresh_entropy: true,
        inter: Some(InterHeader {
            buffers: biases(false, false),
            refresh_last: true,
            prob_intra: 128,
            prob_last: 128,
            prob_golden: 128,
        }),
    }
}

/// The inter frame's own fields of `header`.
fn inter_fields(header: &Header) -> &InterHeader {
    header.inter.as_ref().expect("an inter header")
}

/// Buffers whose golden and alternate vectors point as `golden` and
/// `alternate` say.
fn biases(golden: bool, alternate: bool) -> Buffers {
    Buffers {
        golden: BufferUpdate::Keep,
        alternate: BufferUpdate::Keep,
        sign_bias_golden: golden,
        sign_bias_alternate: alternate,
    }
}

fn mv(x: i16, y: i16) -> Mv {
    Mv { x, y }
}

/// A macroblock predicting from `reference` by one vector throughout.
fn moving(reference: Buffer, x: i16, y: i16) -> MbInfo {
    MbInfo {
        segment: 0,
        skip: false,
        prediction: Prediction::Inter {
            reference,
            mode: InterMode::New,
            motion: Motion::Whole(mv(x, y)),
        },
    }
}

/// A macroblock predicting from the last frame by `mvs`, one per subblock.
fn split(mvs: [Mv; 16]) -> MbInfo {
    MbInfo {
        segment: 0,
        skip: false,
        prediction: Prediction::Inter {
            reference: Buffer::Last,
            mode: InterMode::Split(Partitioning::Each),
            motion: Motion::Split(mvs),
        },
    }
}

/// A macroblock predicted from its own frame.
fn intra() -> MbInfo {
    MbInfo {
        segment: 0,
        skip: false,
        prediction: Prediction::Intra {
            ymode: 0,
            uvmode: 0,
            bmodes: [0; 16],
        },
    }
}

fn around<'a>(
    above: Option<&'a MbInfo>,
    left: Option<&'a MbInfo>,
    above_left: Option<&'a MbInfo>,
) -> Neighbours<'a> {
    Neighbours {
        above,
        left,
        above_left,
    }
}

/// What a near search should find.
fn found(best: Mv, nearest: Mv, near: Mv, counts: [usize; 4]) -> Proposed {
    Proposed {
        best,
        nearest,
        near,
        counts,
    }
}

/// The mode tree's probabilities for neighbours weighing `counts`: each step's
/// from the row its own count picks.
fn mode_probs(counts: [usize; 4]) -> [u8; 4] {
    core::array::from_fn(|step| MODE_CONTEXTS[counts[step]][step])
}

/// Open an inter macroblock predicting from the last frame.
fn open_inter(writer: &mut Writer, inter: &InterHeader) {
    writer.bit(inter.prob_intra, true);
    writer.bit(inter.prob_last, false);
}

#[test]
fn a_vector_reads_back_short_and_long_of_either_sign_doubled() {
    let values: [i16; 16] = [
        0, 1, -1, 7, -7, 8, -8, 15, -15, 16, -16, 100, -513, 512, 1023, -1023,
    ];
    let skewed = [[1; MV_PROBS], [255; MV_PROBS]];
    for probs in [DEFAULT_MV_PROBS, skewed] {
        let mut writer = Writer::new();
        for (&row, &column) in values.iter().zip(values.iter().rev()) {
            write_mv(&mut writer, &probs, (row, column));
        }
        let bytes = writer.finish();
        let mut reader = Bool::new(&bytes);
        for (&row, &column) in values.iter().zip(values.iter().rev()) {
            let read = read_mv(&mut reader, &probs);
            assert_eq!(read, mv(2 * column, 2 * row), "({row}, {column})");
        }
        assert!(!reader.exhausted());
    }
}

#[test]
fn the_near_search_weighs_counts_and_orders_its_neighbours_vectors() {
    let buffers = biases(false, false);
    let (a, b) = (mv(6, -4), mv(-10, 2));
    let (one, other) = (moving(Buffer::Last, 6, -4), moving(Buffer::Last, -10, 2));
    let (still, own, splits) = (moving(Buffer::Last, 0, 0), intra(), split([a; 16]));
    let cases = [
        (around(None, None, None), found(ZERO, ZERO, ZERO, [0; 4])),
        (
            around(Some(&own), Some(&own), Some(&own)),
            found(ZERO, ZERO, ZERO, [0; 4]),
        ),
        (
            around(Some(&one), Some(&one), Some(&own)),
            found(a, a, ZERO, [0, 4, 0, 0]),
        ),
        // A third distinct vector agreeing with the first strengthens it.
        (
            around(Some(&one), Some(&other), Some(&one)),
            found(a, a, b, [0, 3, 2, 0]),
        ),
        (
            around(Some(&still), Some(&still), Some(&still)),
            found(ZERO, ZERO, ZERO, [5, 0, 0, 0]),
        ),
        // The stronger of the two comes first.
        (
            around(Some(&one), Some(&other), Some(&other)),
            found(b, b, a, [0, 3, 2, 0]),
        ),
        // No motion weighing more leaves the best vector none.
        (
            around(Some(&still), Some(&still), Some(&one)),
            found(ZERO, a, ZERO, [4, 1, 0, 0]),
        ),
        // A split neighbour lends its last subblock's vector, and counts as split.
        (
            around(Some(&splits), Some(&splits), Some(&splits)),
            found(a, a, ZERO, [0, 5, 0, 5]),
        ),
    ];
    for (index, (neighbours, expected)) in cases.into_iter().enumerate() {
        assert_eq!(
            find_near(neighbours, Buffer::Last, &buffers),
            expected,
            "case {index}"
        );
    }
    assert_eq!(
        found(ZERO, ZERO, ZERO, [0, 3, 2, 5]).mode_probs(),
        [7, 56, 57, 28]
    );
}

#[test]
fn a_neighbour_whose_reference_points_the_other_way_lends_its_vector_reversed() {
    let buffers = biases(true, false);
    let golden = moving(Buffer::Golden, 6, -4);
    let alternate = moving(Buffer::Alternate, 6, -4);
    let from_golden = around(Some(&golden), None, None);
    assert_eq!(
        find_near(from_golden, Buffer::Last, &buffers).nearest,
        mv(-6, 4)
    );
    assert_eq!(
        find_near(from_golden, Buffer::Golden, &buffers).nearest,
        mv(6, -4)
    );
    assert_eq!(
        find_near(from_golden, Buffer::Alternate, &buffers).nearest,
        mv(-6, 4)
    );
    let from_alternate = around(Some(&alternate), None, None);
    assert_eq!(
        find_near(from_alternate, Buffer::Golden, &buffers).nearest,
        mv(-6, 4)
    );
    assert_eq!(
        find_near(from_alternate, Buffer::Last, &buffers).nearest,
        mv(6, -4)
    );
    // Reversed before it is compared, a golden vector agrees with its opposite
    // from the last frame.
    let last = moving(Buffer::Last, -6, 4);
    let agreeing = around(Some(&golden), Some(&last), None);
    assert_eq!(
        find_near(agreeing, Buffer::Last, &buffers),
        found(mv(-6, 4), mv(-6, 4), ZERO, [0, 4, 0, 0])
    );
}

#[test]
fn vectors_are_held_to_a_macroblock_past_the_frames_edges() {
    // The top-left macroblock of a frame two macroblocks square may point
    // sixteen pixels past the top and left edges, and past the right and
    // bottom ones by the macroblock beside it and sixteen pixels more.
    let corner = Reach::new(0, 2, 0, 2);
    assert_eq!(
        (corner.left, corner.right, corner.top, corner.bottom),
        (-128, 256, -128, 256)
    );
    assert_eq!(mv(-300, 300).clamped(corner), mv(-128, 256));
    assert_eq!(mv(100, -100).clamped(corner), mv(100, -100));
    let opposite = Reach::new(1, 2, 1, 2);
    assert_eq!(
        (opposite.left, opposite.right, opposite.top, opposite.bottom),
        (-256, 128, -256, 128)
    );
    assert_eq!(mv(i16::MAX, i16::MIN).clamped(opposite), mv(128, -256));
}

#[test]
fn each_whole_macroblock_mode_reads_its_vector_clamped() {
    let header = frame();
    let inter = inter_fields(&header);
    // The far vector above is the nearest and the best, and both it and the
    // near one to the left are held to a lone macroblock's reach.
    let (above, left) = (moving(Buffer::Last, 900, -6), moving(Buffer::Last, 4, 2));
    let neighbours = around(Some(&above), Some(&left), None);
    let reach = Reach::new(0, 1, 0, 1);
    let mut writer = Writer::new();
    for code in [1, 2, 0, 3] {
        open_inter(&mut writer, inter);
        write_tree(&mut writer, &MV_REF_TREE, &mode_probs([0, 2, 2, 0]), code);
        if code == 3 {
            write_mv(&mut writer, &DEFAULT_MV_PROBS, (3, -5));
        }
    }
    let bytes = writer.finish();
    let mut reader = Bool::new(&bytes);
    let expected = [
        (InterMode::Nearest, mv(128, -6)),
        (InterMode::Near, mv(4, 2)),
        (InterMode::Zero, ZERO),
        (InterMode::New, mv(118, 0)),
    ];
    for (mode, vector) in expected {
        let read = read_macroblock(&mut reader, &header, inter, neighbours, reach, 0);
        let motion = Motion::Whole(vector);
        let prediction = Prediction::Inter {
            reference: Buffer::Last,
            mode,
            motion,
        };
        assert_eq!(read.prediction, prediction, "{mode:?}");
    }
    assert!(!reader.exhausted());
}

#[test]
fn a_macroblock_names_its_reference_and_its_intra_modes_without_context() {
    let header = frame();
    let inter = inter_fields(&header);
    let (none, reach) = (around(None, None, None), Reach::new(0, 1, 0, 1));
    let mut writer = Writer::new();
    for golden in [false, true] {
        writer.bit(inter.prob_intra, true);
        writer.bit(inter.prob_last, true);
        writer.bit(inter.prob_golden, golden);
        write_tree(&mut writer, &MV_REF_TREE, &mode_probs([0; 4]), 0);
    }
    writer.bit(inter.prob_intra, false);
    write_tree(
        &mut writer,
        &YMODE_TREE,
        &header.probs.ymode,
        usize::from(B_PRED),
    );
    for bmode in 0..16 {
        write_tree(&mut writer, &BMODE_TREE, &BMODE_PROBS, bmode % 10);
    }
    write_tree(&mut writer, &UV_MODE_TREE, &header.probs.uvmode, 3);
    writer.bit(inter.prob_intra, false);
    write_tree(&mut writer, &YMODE_TREE, &header.probs.ymode, 2);
    write_tree(&mut writer, &UV_MODE_TREE, &header.probs.uvmode, 1);
    let bytes = writer.finish();
    let mut reader = Bool::new(&bytes);
    for reference in [Buffer::Golden, Buffer::Alternate] {
        let read = read_macroblock(&mut reader, &header, inter, none, reach, 0);
        let prediction = Prediction::Inter {
            reference,
            mode: InterMode::Zero,
            motion: Motion::Whole(ZERO),
        };
        assert_eq!(read.prediction, prediction);
    }
    let bmodes = core::array::from_fn(|index| u8::try_from(index % 10).expect("a mode"));
    let read = read_macroblock(&mut reader, &header, inter, none, reach, 0);
    let subblocks = Prediction::Intra {
        ymode: B_PRED,
        uvmode: 3,
        bmodes,
    };
    assert_eq!(read.prediction, subblocks);
    let read = read_macroblock(&mut reader, &header, inter, none, reach, 0);
    let whole = Prediction::Intra {
        ymode: 2,
        uvmode: 1,
        bmodes: [0; 16],
    };
    assert_eq!(read.prediction, whole);
    assert!(!reader.exhausted());
}

/// The part-vector probabilities for a part whose left and above neighbours
/// move by `left` and `above`: by whether they agree and are zero.
fn part_probs(left: Mv, above: Mv) -> [u8; 3] {
    let context = match (left == above, left == ZERO, above == ZERO) {
        (true, true, _) => 4,
        (true, false, _) => 3,
        (false, _, true) => 2,
        (false, true, false) => 1,
        (false, false, false) => 0,
    };
    SUB_MV_REF_PROBS[context]
}

/// Write a split macroblock's partitioning `code` and its parts' choices — a
/// part-vector code each and, for a new vector, its delta in quarter pixels —
/// beside neighbours moving by `left` and `above` throughout, answering the
/// vector the format gives each subblock.
fn write_split(
    writer: &mut Writer,
    code: u8,
    choices: &[(usize, (i16, i16))],
    (left, above, best): (Mv, Mv, Mv),
) -> [Mv; 16] {
    write_tree(writer, &SPLIT_TREE, &SPLIT_PROBS, usize::from(code));
    let owners = SPLITS[usize::from(code)];
    let mut mvs = [ZERO; 16];
    for (part, &(choice, delta)) in choices.iter().enumerate() {
        let first = owners
            .iter()
            .position(|&owner| usize::from(owner) == part)
            .expect("a part's first subblock");
        let from_left = if first.is_multiple_of(4) {
            left
        } else {
            mvs[first - 1]
        };
        let from_above = if first < 4 { above } else { mvs[first - 4] };
        write_tree(
            writer,
            &SUB_MV_REF_TREE,
            &part_probs(from_left, from_above),
            choice,
        );
        let vector = match choice {
            0 => from_left,
            1 => from_above,
            2 => ZERO,
            _ => {
                write_mv(writer, &DEFAULT_MV_PROBS, delta);
                mv(best.x + 2 * delta.1, best.y + 2 * delta.0)
            }
        };
        for (slot, &owner) in mvs.iter_mut().zip(&owners) {
            if usize::from(owner) == part {
                *slot = vector;
            }
        }
    }
    mvs
}

/// Every subblock's own choice, each kind of part vector several times.
const EACH: [(usize, (i16, i16)); 16] = [
    (3, (1, -1)),
    (0, (0, 0)),
    (1, (0, 0)),
    (2, (0, 0)),
    (3, (-4, 8)),
    (1, (0, 0)),
    (0, (0, 0)),
    (3, (100, -100)),
    (2, (0, 0)),
    (3, (7, 7)),
    (1, (0, 0)),
    (0, (0, 0)),
    (0, (0, 0)),
    (2, (0, 0)),
    (3, (-1023, 1023)),
    (1, (0, 0)),
];

/// A split's encoding, its partitioning, and the vectors it should give.
type SplitCase = (u8, Partitioning, &'static [(usize, (i16, i16))]);

#[test]
fn every_partitioning_reads_each_parts_vector_from_its_neighbours_or_anew() {
    let header = frame();
    let inter = inter_fields(&header);
    let (left, above) = (moving(Buffer::Last, 10, 20), moving(Buffer::Last, -30, 40));
    let neighbours = around(Some(&above), Some(&left), None);
    // The vector above comes first, so it is the best a new one is coded against.
    let vectors = (mv(10, 20), mv(-30, 40), mv(-30, 40));
    let cases: [SplitCase; 4] = [
        (0, Partitioning::TopBottom, &[(0, (0, 0)), (3, (1, 2))]),
        (1, Partitioning::LeftRight, &[(1, (0, 0)), (2, (0, 0))]),
        (
            2,
            Partitioning::Quarters,
            &[(3, (-2, 0)), (0, (0, 0)), (1, (0, 0)), (2, (0, 0))],
        ),
        (3, Partitioning::Each, &EACH),
    ];
    let mut writer = Writer::new();
    let mut expected = Vec::new();
    for (code, partitioning, choices) in cases {
        open_inter(&mut writer, inter);
        write_tree(&mut writer, &MV_REF_TREE, &mode_probs([0, 2, 2, 0]), 4);
        expected.push((
            partitioning,
            write_split(&mut writer, code, choices, vectors),
        ));
    }
    // Worked through by hand: the halves take the left vector and a new one,
    // the above vector and none; the quarters a new one three times and none.
    let quartered = mv(-30, 36);
    assert_eq!(
        (expected[0].1[7], expected[0].1[8]),
        (mv(10, 20), mv(-26, 42))
    );
    assert_eq!((expected[1].1[1], expected[1].1[2]), (mv(-30, 40), ZERO));
    let quarters = [0, 2, 8, 10].map(|block| expected[2].1[block]);
    assert_eq!(quarters, [quartered, quartered, quartered, ZERO]);
    let bytes = writer.finish();
    let mut reader = Bool::new(&bytes);
    let reach = Reach::new(5, 10, 5, 10);
    for (partitioning, mvs) in expected {
        let read = read_macroblock(&mut reader, &header, inter, neighbours, reach, 0);
        let prediction = Prediction::Inter {
            reference: Buffer::Last,
            mode: InterMode::Split(partitioning),
            motion: Motion::Split(mvs),
        };
        assert_eq!(read.prediction, prediction, "{partitioning:?}");
    }
    assert!(!reader.exhausted());
}

#[test]
fn a_row_reads_each_macroblock_against_those_before_it_and_above() {
    let mut header = frame();
    header.segmentation = Segmentation {
        enabled: true,
        update_map: true,
        probs: [128; 3],
        ..Segmentation::none()
    };
    header.skip_probability = Some(128);
    let inter = inter_fields(&header);
    let above = [moving(Buffer::Last, 8, 8), intra()];
    let mut writer = Writer::new();
    // Segment two, skipped, and the nearest vector: the one above, weighing two.
    for bit in [true, false, true] {
        writer.flag(bit);
    }
    open_inter(&mut writer, inter);
    write_tree(&mut writer, &MV_REF_TREE, &mode_probs([0, 2, 0, 0]), 1);
    // Segment one, coded, and the nearest vector again: the intra macroblock
    // above lends none, and those to the left and above-left the same, weighing
    // three.
    for bit in [false, true, false] {
        writer.flag(bit);
    }
    open_inter(&mut writer, inter);
    write_tree(&mut writer, &MV_REF_TREE, &mode_probs([0, 3, 0, 0]), 1);
    let bytes = writer.finish();
    let mut row = [intra(); 2];
    read_row(
        &mut Bool::new(&bytes),
        &header,
        inter,
        &above,
        &mut row,
        (1, 2),
    );
    let nearest = Prediction::Inter {
        reference: Buffer::Last,
        mode: InterMode::Nearest,
        motion: Motion::Whole(mv(8, 8)),
    };
    assert_eq!(row.map(|mb| (mb.segment, mb.skip)), [(2, true), (1, false)]);
    assert_eq!(row.map(|mb| mb.prediction), [nearest; 2]);
}

#[test]
fn a_row_keeps_the_segments_a_frame_leaves_be() {
    let header = frame();
    let inter = inter_fields(&header);
    let mut writer = Writer::new();
    // No motion twice, the second beside the first's zero vector.
    for counts in [[0; 4], [2, 0, 0, 0]] {
        open_inter(&mut writer, inter);
        write_tree(&mut writer, &MV_REF_TREE, &mode_probs(counts), 0);
    }
    let bytes = writer.finish();
    let mut row = [intra(); 2];
    row[0].segment = 3;
    row[1].segment = 1;
    read_row(
        &mut Bool::new(&bytes),
        &header,
        inter,
        &[],
        &mut row,
        (0, 1),
    );
    let still = Prediction::Inter {
        reference: Buffer::Last,
        mode: InterMode::Zero,
        motion: Motion::Whole(ZERO),
    };
    assert_eq!(
        row.map(|mb| (mb.segment, mb.prediction)),
        [(3, still), (1, still)]
    );
}

#[test]
fn rows_read_from_drawn_bits_name_only_what_the_format_has() {
    let mut rng = Prng::new(0x1D_E5_71_00);
    for _ in 0..64 {
        let mut header = frame();
        header.segmentation = Segmentation {
            enabled: true,
            update_map: true,
            probs: [rng.next_u8(), rng.next_u8(), rng.next_u8()],
            ..Segmentation::none()
        };
        header.skip_probability = Some(rng.next_u8());
        let mut inter = inter_fields(&header).clone();
        inter.prob_intra = rng.next_u8();
        inter.prob_last = rng.next_u8();
        inter.prob_golden = rng.next_u8();
        let bytes: Vec<u8> = (0..512).map(|_| rng.next_u8()).collect();
        let mut reader = Bool::new(&bytes);
        let mut above: Vec<MbInfo> = Vec::new();
        for index in 0..4 {
            let mut row = vec![intra(); 5];
            read_row(&mut reader, &header, &inter, &above, &mut row, (index, 4));
            for mb in &row {
                assert!(mb.segment < 4);
                if let Prediction::Intra {
                    ymode,
                    uvmode,
                    bmodes,
                } = mb.prediction
                {
                    assert!(ymode <= B_PRED && uvmode <= 3);
                    assert!(bmodes.iter().all(|&mode| mode <= 9));
                }
            }
            above = row;
        }
    }
}
