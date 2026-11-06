//! VP8 keyframe decoder tests.
//!
//! Every bitstream is written here rather than shipped. The boolean encoder
//! is the reference *encoder* from the format's own specification, not the
//! inverse of the decoder beside it, so a round trip tests the decoder
//! against the format; the tree writer finds each leaf's path from the same
//! tree arrays the decoder walks, which is the one thing a fixture cannot
//! usefully re-derive.

use alloc::vec;
use alloc::vec::Vec;

use super::fixture::{keyframe, Block};
use super::writer::{write_header, write_tree_from, Writer};
use super::{
    inverse_dct, inverse_walsh, probe, read_block, read_header, Bool, Buffer, BufferUpdate,
    Buffers, Edges, FilterDeltas, Header, InterHeader, Probabilities, Quant, Rgba8, Segmentation,
    StreamState, Strength, Tag, Vp8Error, BLOCK_COEFFS, COEFF_BANDS, COEFF_TREE, DCT_0, DCT_CAT1,
    DCT_EOB, DEFAULT_COEFF_PROBS, MV_LONG, MV_SIGN, START_CODE, TREE_AFTER_EOB,
};
use tairix_fuzzseed::Prng;

/// Bytes in one decoded pixel.
const RGBA_BYTES: usize = core::mem::size_of::<Rgba8>();

/// A keyframe decoded whole: its declared size and its rows end to end.
#[derive(Debug, PartialEq)]
struct Decoded {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Decoded {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn pixels(&self) -> &[u8] {
        &self.pixels
    }
}

/// `bytes`, a keyframe, decoded whole through the row decoder.
fn decode(bytes: &[u8]) -> Result<Decoded, Vp8Error> {
    let (width, height) = probe(bytes)?;
    let mut pixels = Vec::new();
    super::decode_rows(bytes, |row: &[Rgba8]| {
        pixels.extend_from_slice(row.as_flattened());
        Ok::<(), Vp8Error>(())
    })?;
    Ok(Decoded {
        width,
        height,
        pixels,
    })
}

/// Generous enough that no fixture here is refused for its size.

#[test]
fn what_the_loop_filter_does_at_a_macroblock_is_held_in_two_bytes() {
    // A row of them is held for the loop filter, one per sixteen pixels of
    // width.
    assert!(core::mem::size_of::<Edges>() <= 2);
}

#[test]
fn the_boolean_coder_round_trips_every_probability() {
    let mut rng = Prng::new(0x5EED_1234_ABCD_0001);
    for _ in 0..200 {
        let count = 1 + rng.below(400);
        let choices: Vec<(u8, bool)> = (0..count)
            .map(|_| (rng.next_u8(), rng.next_u64() & 1 == 1))
            .collect();
        let mut writer = Writer::new();
        for &(probability, value) in &choices {
            writer.bit(probability, value);
        }
        let bytes = writer.finish();
        let mut reader = super::Bool::new(&bytes);
        for (index, &(probability, value)) in choices.iter().enumerate() {
            assert_eq!(
                reader.bit(probability) != 0,
                value,
                "bit {index} of {count} disagrees"
            );
        }
        assert!(!reader.exhausted(), "the flush pads the lookahead");
    }
}

/// The one colour every pixel of a decoded fixture must be.
fn uniform(bytes: &[u8], width: u32, height: u32) -> [u8; RGBA_BYTES] {
    let image = decode(bytes).expect("a valid keyframe decodes");
    assert_eq!((image.width(), image.height()), (width, height));
    let pixels = image.pixels().as_chunks::<RGBA_BYTES>().0;
    let first = pixels[0];
    for (index, pixel) in pixels.iter().enumerate() {
        assert_eq!(*pixel, first, "pixel {index} breaks the uniform picture");
    }
    first
}

#[test]
fn a_flat_keyframe_predicts_the_midpoint_and_converts_to_grey() {
    // With no macroblock above or to the left, the average prediction has
    // nothing to average and the format fills the block with 128; the
    // conversion then carries that to 130 in each channel.
    let bytes = keyframe(16, 16, Block::flat(0, 0));
    assert_eq!(uniform(&bytes, 16, 16), [130, 130, 130, 255]);
}

#[test]
fn a_probe_reads_the_geometry_without_decoding() {
    let bytes = keyframe(16, 16, Block::flat(0, 0));
    assert_eq!(probe(&bytes), Ok((16, 16)));
}

#[test]
fn a_picture_smaller_than_a_macroblock_is_cropped_to_its_own_size() {
    let bytes = keyframe(5, 3, Block::flat(0, 0));
    assert_eq!(uniform(&bytes, 5, 3), [130, 130, 130, 255]);
}

#[test]
fn the_three_directional_modes_predict_from_the_edges_the_format_invents() {
    // On the top-left macroblock the above row reads 127 and the left
    // column 129, so the vertical, horizontal, and averaging modes each
    // reach a different luma and none of them agree.
    let mut seen = Vec::new();
    for mode in [0usize, 1, 2] {
        let bytes = keyframe(16, 16, Block::flat(mode, mode));
        seen.push(uniform(&bytes, 16, 16));
    }
    assert_ne!(seen[0], seen[1]);
    assert_ne!(seen[1], seen[2]);
    assert_ne!(seen[0], seen[2]);
}

#[test]
fn the_true_motion_mode_decodes_to_a_uniform_picture_at_the_frame_corner() {
    // Its prediction is left + above - corner, and at the frame corner all
    // three are the invented edge values, so the result is flat.
    let bytes = keyframe(16, 16, Block::flat(3, 3));
    let _ = uniform(&bytes, 16, 16);
}

#[test]
fn subblock_prediction_reconstructs_each_block_from_the_one_before() {
    // Every subblock averages the four samples above and the four to its
    // left. The top row averages the invented 127 above against a
    // reconstructed 128, giving 128; every later row averages a
    // reconstructed 128 or 129 against the invented 129, giving 129. So the
    // picture is not flat, and where it steps is exactly what proves the
    // sixteen subblocks were predicted and reconstructed in scan order.
    let bytes = keyframe(16, 16, Block::subblocks(0));
    let image = decode(&bytes).expect("a valid keyframe decodes");
    let pixels = image.pixels().as_chunks::<RGBA_BYTES>().0;
    for row in 0..16 {
        let expected = if row < 4 {
            [130, 130, 130, 255]
        } else {
            [132, 132, 132, 255]
        };
        for column in 0..16 {
            assert_eq!(pixels[row * 16 + column], expected, "at {column},{row}");
        }
    }
}

#[test]
fn every_subblock_mode_decodes_to_a_picture() {
    // Ten modes, each of which reads a different set of the samples around
    // its subblock; none may refuse or reach outside the frame.
    for bmode in 0..10 {
        let bytes = keyframe(16, 16, Block::subblocks(bmode));
        let image = decode(&bytes).expect("a valid keyframe decodes");
        assert_eq!(image.pixels().len(), 16 * 16 * RGBA_BYTES, "mode {bmode}");
    }
}

#[test]
fn a_luma_dc_coefficient_moves_the_whole_macroblock() {
    // The Walsh-Hadamard transform spreads the luma DC block's one
    // coefficient across all sixteen luma blocks, so a token of four lifts
    // every sample by one and the conversion carries that to 132.
    let mut block = Block::flat(0, 0);
    block.y2_dc = 4;
    let bytes = keyframe(16, 16, block);
    assert_eq!(uniform(&bytes, 16, 16), [132, 132, 132, 255]);
}

#[test]
fn an_interframe_is_refused_rather_than_predicted_from_nothing() {
    let mut bytes = keyframe(16, 16, Block::flat(0, 0));
    bytes[0] |= 1;
    assert_eq!(probe(&bytes), Err(Vp8Error::Interframe));
    let refused = super::decode_rows(&bytes, |_: &[Rgba8]| Ok::<(), Vp8Error>(()));
    assert_eq!(refused, Err(Vp8Error::Interframe));
}

#[test]
fn a_profile_the_format_does_not_define_is_refused() {
    let mut bytes = keyframe(16, 16, Block::flat(0, 0));
    bytes[0] |= 0b0000_1110;
    assert_eq!(probe(&bytes), Err(Vp8Error::UnsupportedProfile));
}

#[test]
fn a_missing_start_code_is_refused() {
    let mut bytes = keyframe(16, 16, Block::flat(0, 0));
    bytes[4] ^= 0xFF;
    assert_eq!(probe(&bytes), Err(Vp8Error::BadStartCode));
}

#[test]
fn a_zero_sided_picture_is_refused() {
    let mut bytes = keyframe(16, 16, Block::flat(0, 0));
    bytes[6] = 0;
    bytes[7] = 0;
    assert_eq!(probe(&bytes), Err(Vp8Error::InvalidGeometry));
}

#[test]
fn a_reserved_colour_space_is_refused_rather_than_guessed() {
    // The first bit of the compressed header selects the colour space, and
    // its one reserved value names a space this decoder cannot convert.
    let mut head = Writer::new();
    head.flag(true);
    let first_part = head.finish();
    let mut bytes = vec![
        u8::try_from((u32::try_from(first_part.len()).expect("small") << 5) & 0xFF)
            .expect("one byte"),
        0,
        0,
    ];
    bytes.extend_from_slice(&START_CODE);
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(&first_part);
    assert_eq!(decode(&bytes), Err(Vp8Error::ReservedColourSpace));
}

#[test]
fn a_truncated_bitstream_is_refused_rather_than_completed() {
    let bytes = keyframe(16, 16, Block::flat(0, 0));
    for cut in 0..bytes.len() {
        // Every prefix must refuse or decode; none may panic, and none may
        // hand out a picture read from bytes that are not there.
        let _ = decode(&bytes[..cut]);
    }
    assert!(decode(&bytes[..UNCOMPRESSED_ONLY]).is_err());
}

/// A prefix holding only the uncompressed header, whose partition is empty.
const UNCOMPRESSED_ONLY: usize = 10;

#[test]
fn the_direct_current_shortcut_matches_the_whole_inverse_transform() {
    // A block whose only non-zero coefficient is its first has a constant
    // residual, and the reconstruction takes that shortcut; it must agree
    // with the transform it skips at every value.
    for value in [-2048i16, -257, -8, -1, 0, 1, 8, 257, 2047] {
        let mut coeffs = [0i16; BLOCK_COEFFS];
        coeffs[0] = value;
        let residual = inverse_dct(&coeffs);
        let flat = (i32::from(value) + 4) >> 3;
        for (index, sample) in residual.iter().enumerate() {
            assert_eq!(*sample, flat, "coefficient {value}, sample {index}");
        }
    }
}

#[test]
fn the_inverse_transforms_leave_an_empty_block_empty() {
    let empty = [0i16; BLOCK_COEFFS];
    assert_eq!(inverse_dct(&empty), [0i32; BLOCK_COEFFS]);
    assert_eq!(inverse_walsh(&empty), empty);
}

#[test]
fn the_walsh_transform_spreads_one_coefficient_over_every_output() {
    for value in [8i16, 32, 256, -32] {
        let mut coeffs = [0i16; BLOCK_COEFFS];
        coeffs[0] = value;
        let spread = inverse_walsh(&coeffs);
        let expected = i16::try_from((i32::from(value) + 3) >> 3).expect("in range");
        assert_eq!(spread, [expected; BLOCK_COEFFS], "coefficient {value}");
    }
}

#[test]
fn the_inverse_transforms_are_linear_in_their_input() {
    // Doubling every coefficient doubles the residual, within the rounding
    // the format's own arithmetic performs.
    let mut coeffs = [0i16; BLOCK_COEFFS];
    for (index, value) in coeffs.iter_mut().enumerate() {
        *value = i16::try_from(index * 16).expect("in range");
    }
    let single = inverse_dct(&coeffs);
    let mut doubled = coeffs;
    for value in &mut doubled {
        *value *= 2;
    }
    let twice = inverse_dct(&doubled);
    for (one, two) in single.iter().zip(&twice) {
        assert!((two - 2 * one).abs() <= 1, "{one} doubled is {two}");
    }
}

/// A corrupt stream can dequantise to coefficients whose second transform
/// pass would multiply past 32 bits. The transform must answer a bounded
/// residual — one the sample addition cannot overflow either — rather than
/// trap. Found by `fuzz_image`'s mutated-WebP harness.
#[test]
fn the_inverse_dct_bounds_a_corrupt_streams_coefficients() {
    for extreme in [i16::MAX, i16::MIN] {
        let residual = inverse_dct(&[extreme; BLOCK_COEFFS]);
        for value in residual {
            // Every sample the decoder adds this to is a `u8`, so the
            // bound has to leave room for one.
            assert!(
                value.checked_add(i32::from(u8::MAX)).is_some(),
                "residual {value} would overflow the sample addition"
            );
        }
    }
}

/// What the decoder makes of [`drawn_cases`], in order, pinned so a change to
/// any decode shows.
const DRAWN: [u64; 90] = [
    0x5f8e_5d47_8d69_db24,
    0xa847_9b65_51cb_01d8,
    0xd772_1b40_9d4f_ce1f,
    0xc634_0ce5_02b6_125c,
    0xd099_d4f1_4041_7d2e,
    0xbc08_3561_43c3_9caa,
    0x3b6b_a66d_59fc_8629,
    0x9e11_f2c6_4ffc_9bd1,
    0xc1a3_bddf_e670_3f5e,
    0xc402_5fbc_7a64_882f,
    0xfd48_5085_96ce_52a9,
    0x8426_a53d_c771_0569,
    0xc063_0ba5_98b3_ac65,
    0xb506_0631_9011_7726,
    0xc538_aaf3_074e_5c4f,
    0x632a_e843_bb4e_95f1,
    0x5ba3_ffbb_7734_1706,
    0x4da8_c320_2d40_f561,
    0xd31d_8d79_4ca8_9488,
    0x8727_ae67_33af_01cc,
    0xace5_2974_3bd3_d04c,
    0xa5bc_6673_d92d_ecb7,
    0x8c5a_53b0_a111_4ddc,
    0xd543_26aa_1ce5_5929,
    0xf736_a9b0_a12d_abad,
    0xa371_5fbc_1232_71b6,
    0x9716_85a8_2546_8cf4,
    0x6be8_f9f3_77b4_1545,
    0xce36_9052_2d07_d141,
    0xadee_3752_4300_bf35,
    0x32ff_41d7_adc4_5d15,
    0x9dce_fd00_2124_d274,
    0xeec8_4fa8_1e23_4221,
    0x9879_6779_16bf_e325,
    0xe79a_a3c7_cc74_daec,
    0x5dc6_7e01_130d_4ae3,
    0x7656_85a6_cb2e_35cd,
    0xb314_7be2_22f2_9ef7,
    0x9f41_5b0e_7548_6f25,
    0x4d5e_bd29_50b0_57ee,
    0x9039_7927_20e1_c307,
    0x4584_a5b1_b9f7_9325,
    0x5a61_a6fe_2099_1e29,
    0x613b_94b9_6553_e093,
    0xaa24_aa44_90af_641a,
    0x6135_0578_e3a4_df34,
    0x7862_94dc_f420_3930,
    0xf89c_14fa_1950_4a54,
    0x6952_44ad_7420_0fde,
    0x8b01_4870_ec6d_4074,
    0x1aed_7007_d619_ef4d,
    0xf41e_6283_cace_509f,
    0x6e9c_2e49_99df_801b,
    0x5e3b_56de_de4b_3f6f,
    0xb058_46bd_bcf8_99a7,
    0xa6ef_9b92_9015_b7a4,
    0xa1b1_59b3_d8a3_fb41,
    0x8981_1cc9_dcd2_63b9,
    0x05f8_0eb6_4cf0_eb98,
    0xad68_5eb2_81ff_9532,
    0x4a14_0b6c_43d3_1b25,
    0x7cf0_f626_1035_8125,
    0xd69a_d8ca_2e10_f325,
    0x06a7_1c98_f777_3725,
    0x85bc_3a51_c8a3_13a9,
    0x8992_8e1a_9ab3_243d,
    0xbac9_f6e9_9b55_0725,
    0xc933_ac49_ee26_d645,
    0xfc53_1846_00b4_2b25,
    0xec7d_2177_5501_3b91,
    0xbac9_f6e9_9b55_0725,
    0x307e_0982_34d4_90a1,
    0x3947_4ec2_0764_ab65,
    0xbec2_05f4_7986_6325,
    0xe050_4884_d3b7_feb1,
    0x4cfd_1656_8b2f_6294,
    0x9e83_a2af_b49f_c7af,
    0x74d8_515a_13df_ca9a,
    0x2c0b_fecb_29a6_9b2f,
    0x281d_71ed_6bd9_0efa,
    0x7571_cc04_4ffb_6641,
    0x7bd8_3b99_d800_5583,
    0xf346_1132_0efc_4089,
    0x99ab_daa9_22f6_c24d,
    0x601c_519b_687a_1a3d,
    0x6be2_62be_d0d2_41eb,
    0x52fa_d8d0_6c5d_f71c,
    0x2814_c5e0_967a_6211,
    0x4d28_7414_52ee_59af,
    0xb3ab_55a9_31b1_0523,
];

#[test]
fn drawn_keyframes_decode_to_the_pictures_pinned() {
    let hashes: Vec<u64> = drawn_cases()
        .into_iter()
        .map(|(seed, (width, height), filter, partitions)| {
            let mut rng = Prng::new(seed);
            let bytes =
                super::fixture::drawn_keyframe(width, height, filter, partitions, &mut || {
                    rng.next_u64()
                });
            fnv(decode(&bytes).expect("a drawn keyframe decodes").pixels())
        })
        .collect();
    assert!(
        hashes == DRAWN,
        "the drawn keyframes now decode to {hashes:#x?}"
    );
}

/// A frame holds one macroblock row and the rows its filter and colours still
/// need, however tall the picture, and no more than its forecast says.
#[test]
fn a_frame_holds_a_window_of_rows_however_tall_the_picture() {
    let held = |height: u32| {
        let filter = super::fixture::Filter {
            simple: false,
            level: 20,
            sharpness: 0,
        };
        let mut rng = Prng::new(1);
        let bytes = super::fixture::drawn_keyframe(48, height, filter, 1, &mut || rng.next_u64());
        let size = Tag::read(&bytes).expect("a tag").first_partition;
        let head = &bytes[super::UNCOMPRESSED_HEADER..][..size];
        let header = read_header(
            &mut Bool::new(head),
            true,
            48,
            height,
            &mut StreamState::new(),
        )
        .expect("a header");
        let frame = super::Frame::new(&header).expect("a frame");
        let planes = [
            &frame.current.luma,
            &frame.current.blue_chroma,
            &frame.current.red_chroma,
            &frame.window.planes.luma,
            &frame.window.planes.blue_chroma,
            &frame.window.planes.red_chroma,
        ];
        planes
            .iter()
            .map(|plane| plane.samples.len())
            .sum::<usize>() as u64
    };
    assert_eq!(held(16), held(4096));
    assert!(held(4096) <= super::frame_peak_bytes(48));
}

/// A 64-bit FNV-1a hash, so a picture's every byte is pinned in one number.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn drawn_cases() -> Vec<(u64, (u32, u32), super::fixture::Filter, usize)> {
    use super::fixture::Filter;
    let filters = [
        Filter {
            simple: false,
            level: 0,
            sharpness: 0,
        },
        Filter {
            simple: false,
            level: 20,
            sharpness: 0,
        },
        Filter {
            simple: false,
            level: 63,
            sharpness: 5,
        },
        Filter {
            simple: true,
            level: 30,
            sharpness: 2,
        },
        Filter {
            simple: true,
            level: 63,
            sharpness: 7,
        },
    ];
    let sizes = [(40, 37), (33, 50), (64, 16), (17, 49), (16, 16), (1, 33)];
    let mut cases = Vec::new();
    let mut seed = 0x1D_E5_70_00u64;
    for size in sizes {
        for filter in filters {
            for partitions in [1, 2, 4] {
                seed += 1;
                cases.push((seed, size, filter, partitions));
            }
        }
    }
    cases
}

/// A stream part-way through, carrying values none of which are the defaults,
/// so a header that keeps one is told from one that resets it.
fn carried() -> StreamState {
    let mut state = StreamState::new();
    state.probs.coeff[1][2][0][3] = 77;
    state.probs.ymode = [90, 91, 92, 93];
    state.probs.uvmode = [60, 61, 62];
    state.probs.mv[1][4] = 40;
    state.segmentation = Segmentation {
        enabled: true,
        update_map: false,
        absolute: true,
        quant: [5, -5, 9, 0],
        filter: [1, 2, 3, 4],
        probs: [255; 3],
    };
    state.deltas = FilterDeltas {
        enabled: true,
        reference: [2, 0, -2, -4],
        mode: [4, -2, 2, 4],
    };
    state
}

/// An inter header changing something in every part of what [`carried`]
/// carries, its buffers taking `golden` and `alternate`.
fn changing(golden: BufferUpdate, alternate: BufferUpdate) -> Header {
    let mut probs = carried().probs;
    probs.coeff[0][1][2][3] = 200;
    probs.coeff[3][7][2][10] = 1;
    probs.ymode = [10, 20, 30, 40];
    probs.mv[0][MV_SIGN] = 1;
    probs.mv[1][MV_LONG + 9] = 254;
    Header {
        width: 48,
        height: 32,
        simple_filter: true,
        filter_level: 41,
        sharpness: 3,
        segmentation: Segmentation {
            enabled: true,
            update_map: true,
            absolute: false,
            quant: [-12, 0, 33, 127],
            filter: [-63, 5, 0, 0],
            probs: [30, 255, 200],
        },
        deltas: FilterDeltas {
            enabled: true,
            reference: [2, 9, -2, -4],
            mode: [4, -2, 2, -63],
        },
        quant: Quant {
            index: 99,
            y_dc: -15,
            y2_dc: 0,
            y2_ac: 7,
            uv_dc: 1,
            uv_ac: -1,
        },
        partitions: 4,
        probs,
        skip_probability: Some(201),
        refresh_entropy: true,
        inter: Some(InterHeader {
            buffers: Buffers {
                golden,
                alternate,
                sign_bias_golden: true,
                sign_bias_alternate: false,
            },
            refresh_last: false,
            prob_intra: 63,
            prob_last: 255,
            prob_golden: 2,
        }),
    }
}

#[test]
fn an_inter_header_reads_back_every_field_it_was_written_with() {
    let updates = [
        (
            BufferUpdate::CopyOf(Buffer::Alternate),
            BufferUpdate::Refresh,
        ),
        (BufferUpdate::Keep, BufferUpdate::CopyOf(Buffer::Last)),
        (BufferUpdate::Refresh, BufferUpdate::CopyOf(Buffer::Golden)),
        (BufferUpdate::CopyOf(Buffer::Last), BufferUpdate::Keep),
    ];
    for (golden, alternate) in updates {
        let expected = changing(golden, alternate);
        let mut writer = Writer::new();
        write_header(&mut writer, &expected, &carried());
        let bytes = writer.finish();
        let mut reader = Bool::new(&bytes);
        let mut state = carried();
        let header = read_header(&mut reader, false, 48, 32, &mut state).expect("an inter header");
        assert_eq!(
            header, expected,
            "golden {golden:?}, alternate {alternate:?}"
        );
        assert!(!reader.exhausted());
        assert_eq!(
            state.probs, expected.probs,
            "refreshed probabilities carry on"
        );
        assert_eq!(state.segmentation, expected.segmentation);
        assert_eq!(state.deltas, expected.deltas);
    }
}

#[test]
fn a_header_not_refreshing_its_probabilities_leaves_the_streams_as_they_were() {
    let mut expected = changing(BufferUpdate::Keep, BufferUpdate::Keep);
    expected.refresh_entropy = false;
    let mut writer = Writer::new();
    write_header(&mut writer, &expected, &carried());
    let mut state = carried();
    let header = read_header(&mut Bool::new(&writer.finish()), false, 48, 32, &mut state)
        .expect("an inter header");
    assert_eq!(
        header.probs, expected.probs,
        "the frame decodes with its updates"
    );
    assert_eq!(
        state.probs,
        carried().probs,
        "and the stream goes on without them"
    );
    assert_eq!(
        state.segmentation, expected.segmentation,
        "features carry on regardless"
    );
    assert_eq!(state.deltas, expected.deltas);
}

#[test]
fn what_a_header_does_not_replace_the_stream_keeps() {
    let from = carried();
    let mut expected = changing(BufferUpdate::Keep, BufferUpdate::Refresh);
    expected.segmentation = Segmentation {
        update_map: false,
        ..from.segmentation.clone()
    };
    expected.deltas = from.deltas.clone();
    expected.probs = from.probs.clone();
    let mut state = from.clone();
    let mut writer = Writer::new();
    write_header(&mut writer, &expected, &from);
    let header = read_header(&mut Bool::new(&writer.finish()), false, 48, 32, &mut state)
        .expect("an inter header");
    assert_eq!(header, expected);
    // Nor does a frame with segmentation and deltas off lose them for a later one.
    expected.segmentation.enabled = false;
    expected.deltas.enabled = false;
    let mut writer = Writer::new();
    write_header(&mut writer, &expected, &state);
    let header = read_header(&mut Bool::new(&writer.finish()), false, 48, 32, &mut state)
        .expect("an inter header");
    assert_eq!(header, expected);
    assert_eq!(state.segmentation.quant, from.segmentation.quant);
    assert_eq!(state.deltas.mode, from.deltas.mode);
}

#[test]
fn a_keyframe_resets_what_the_stream_carries() {
    let bytes = keyframe(16, 16, Block::flat(0, 0));
    let size = Tag::read(&bytes).expect("a tag").first_partition;
    let head = &bytes[super::UNCOMPRESSED_HEADER..][..size];
    let mut state = carried();
    let header =
        read_header(&mut Bool::new(head), true, 16, 16, &mut state).expect("a keyframe header");
    assert_eq!(header.probs, Probabilities::DEFAULT);
    assert_eq!(header.segmentation, Segmentation::none());
    assert_eq!(header.deltas, FilterDeltas::none());
    assert_eq!(header.inter, None);
    assert_eq!(state, StreamState::new());
}

#[test]
fn a_reserved_buffer_copy_is_refused_and_the_stream_kept() {
    let mut writer = Writer::new();
    // No segmentation, loop filter or deltas, one partition and a quantiser of
    // nought; then a golden buffer not refreshed, copied from what nothing is.
    writer.flag(false);
    writer.flag(false);
    writer.literal(0, 6);
    writer.literal(0, 3);
    writer.flag(false);
    writer.literal(0, 2);
    writer.literal(0, 7);
    for _ in 0..5 {
        writer.flag(false);
    }
    writer.flag(false);
    writer.flag(true);
    writer.literal(3, 2);
    let bytes = writer.finish();
    let mut state = carried();
    assert_eq!(
        read_header(&mut Bool::new(&bytes), false, 16, 16, &mut state),
        Err(Vp8Error::ReservedBufferCopy)
    );
    assert_eq!(state, carried());
}

/// The level a segment's own value sets is held to the filter's range before
/// the reference and mode deltas adjust it, as the reference decoder holds it.
#[test]
fn a_segments_filter_level_is_held_to_range_before_any_delta() {
    let mut segmentation = Segmentation::none();
    segmentation.enabled = true;
    segmentation.filter = [40, -40, 0, 0];
    assert_eq!(segmentation.filter_level(60, 0), 63);
    assert_eq!(segmentation.filter_level(20, 1), 0);
    segmentation.absolute = true;
    segmentation.filter = [-9, 0, 0, 0];
    assert_eq!(segmentation.filter_level(60, 0), 0);
}

#[test]
fn a_frame_without_segmentation_ignores_the_features_it_carries() {
    let mut segmentation = carried().segmentation;
    assert_eq!(segmentation.quant_index(50, 1), 0);
    segmentation.enabled = false;
    assert_eq!(segmentation.quant_index(50, 1), 50);
    assert_eq!(segmentation.filter_level(30, 1), 30);
}

/// A coefficient of the third large category, 11 to 18, carries three extra
/// bits: read with a fourth, it took a bit of the token after it, and every
/// later token was read out of step.
#[test]
fn a_third_category_coefficient_reads_three_extra_bits() {
    // The category's own probabilities, from the specification.
    const CATEGORY_THREE: [u8; 3] = [173, 148, 140];
    let plane = &DEFAULT_COEFF_PROBS[3];
    let band = |scan: usize| usize::from(COEFF_BANDS[scan]);
    for magnitude in 11..=18u8 {
        let mut writer = Writer::new();
        let token = usize::from(DCT_CAT1 + 2);
        write_tree_from(&mut writer, &COEFF_TREE, &plane[band(0)][0], 0, token);
        for (bit, &probability) in CATEGORY_THREE.iter().enumerate() {
            writer.bit(probability, ((magnitude - 11) >> (2 - bit)) & 1 != 0);
        }
        writer.flag(false);
        // A one after it, under the context a large coefficient leaves, then
        // the end of the block under a one's.
        write_tree_from(&mut writer, &COEFF_TREE, &plane[band(1)][2], 0, 1);
        writer.flag(true);
        let end = usize::from(DCT_EOB);
        write_tree_from(&mut writer, &COEFF_TREE, &plane[band(2)][1], 0, end);
        let bytes = writer.finish();
        let mut coeffs = [0i16; BLOCK_COEFFS];
        let mut reader = Bool::new(&bytes);
        assert!(read_block(
            &mut reader,
            &DEFAULT_COEFF_PROBS,
            3,
            0,
            0,
            (1, 1),
            &mut coeffs
        ));
        let mut expected = [0i16; BLOCK_COEFFS];
        expected[0] = i16::from(magnitude);
        expected[1] = -1;
        assert_eq!(coeffs, expected, "magnitude {magnitude}");
    }
}

/// A block whose tokens run to its end as zeros coded past its first
/// position, as the reference decoder's end-of-block test has it, though no
/// coefficient of it is non-zero; one ending at once did not.
#[test]
fn a_block_of_zeros_running_to_its_end_reads_as_coded() {
    let plane = &DEFAULT_COEFF_PROBS[0];
    let band = |scan: usize| usize::from(COEFF_BANDS[scan]);
    let mut writer = Writer::new();
    for scan in 1..BLOCK_COEFFS {
        let start = if scan == 1 { 0 } else { TREE_AFTER_EOB };
        let zero = usize::from(DCT_0);
        write_tree_from(&mut writer, &COEFF_TREE, &plane[band(scan)][0], start, zero);
    }
    let end = usize::from(DCT_EOB);
    write_tree_from(&mut writer, &COEFF_TREE, &plane[band(1)][0], 0, end);
    let bytes = writer.finish();
    let mut reader = Bool::new(&bytes);
    let mut coeffs = [0i16; BLOCK_COEFFS];
    assert!(read_block(
        &mut reader,
        &DEFAULT_COEFF_PROBS,
        0,
        0,
        1,
        (1, 1),
        &mut coeffs
    ));
    assert_eq!(coeffs, [0; BLOCK_COEFFS]);
    assert!(!read_block(
        &mut reader,
        &DEFAULT_COEFF_PROBS,
        0,
        0,
        1,
        (1, 1),
        &mut coeffs
    ));
    assert!(!reader.exhausted());
}

/// An inter frame counts fewer of its edges as sharp features than a
/// keyframe filtered at the same level does.
#[test]
fn an_inter_frame_keeps_fewer_edges_sharp_than_a_keyframe() {
    let thresholds = |keyframe: bool| {
        [14, 15, 19, 20, 39, 40, 63].map(|level| Strength::new(level, 0, keyframe, true).hev)
    };
    assert_eq!(thresholds(true), [0, 1, 1, 1, 1, 2, 2]);
    assert_eq!(thresholds(false), [0, 1, 1, 2, 2, 3, 3]);
}
