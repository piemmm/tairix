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

use super::fixture::{keyframe, Block, Writer};
use super::{decode, inverse_dct, inverse_walsh, probe, BLOCK_COEFFS, START_CODE};
use crate::{DecodeError, DecodeLimits, RGBA_BYTES};
use tairix_fuzzseed::Prng;

/// Generous enough that no fixture here is refused for its size.
fn limits() -> DecodeLimits {
    DecodeLimits::new(256, 256, 256 * 256, 1 << 16)
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
    let image = decode(bytes, &limits()).expect("a valid keyframe decodes");
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
    let image = decode(&bytes, &limits()).expect("a valid keyframe decodes");
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
        let image = decode(&bytes, &limits()).expect("a valid keyframe decodes");
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
    assert_eq!(probe(&bytes), Err(DecodeError::WebpLossyInterframe));
}

#[test]
fn a_profile_the_format_does_not_define_is_refused() {
    let mut bytes = keyframe(16, 16, Block::flat(0, 0));
    bytes[0] |= 0b0000_1110;
    assert_eq!(probe(&bytes), Err(DecodeError::WebpLossyUnsupportedProfile));
}

#[test]
fn a_missing_start_code_is_refused() {
    let mut bytes = keyframe(16, 16, Block::flat(0, 0));
    bytes[4] ^= 0xFF;
    assert_eq!(probe(&bytes), Err(DecodeError::WebpLossyBadStartCode));
}

#[test]
fn a_zero_sided_picture_is_refused() {
    let mut bytes = keyframe(16, 16, Block::flat(0, 0));
    bytes[6] = 0;
    bytes[7] = 0;
    assert_eq!(probe(&bytes), Err(DecodeError::WebpLossyInvalidGeometry));
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
    assert_eq!(
        decode(&bytes, &limits()),
        Err(DecodeError::WebpLossyReservedColourSpace)
    );
}

#[test]
fn a_truncated_bitstream_is_refused_rather_than_completed() {
    let bytes = keyframe(16, 16, Block::flat(0, 0));
    for cut in 0..bytes.len() {
        // Every prefix must refuse or decode; none may panic, and none may
        // hand out a picture read from bytes that are not there.
        let _ = decode(&bytes[..cut], &limits());
    }
    assert!(decode(&bytes[..UNCOMPRESSED_ONLY], &limits()).is_err());
}

/// A prefix holding only the uncompressed header, whose partition is empty.
const UNCOMPRESSED_ONLY: usize = 10;

#[test]
fn a_geometry_past_the_callers_limit_is_refused_before_anything_is_reserved() {
    let bytes = keyframe(64, 64, Block::flat(0, 0));
    let tight = DecodeLimits::new(32, 32, 32 * 32, 0);
    assert_eq!(decode(&bytes, &tight), Err(DecodeError::WidthExceedsLimit));
}

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

/// What the whole-frame decoder this one replaced made of [`drawn_cases`], in
/// order: a decode a macroblock row at a time must hand back the same pixels.
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
    0xd3ca_2451_b4f5_7593,
    0xe7fb_06e3_8aa7_ad61,
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
fn a_row_at_a_time_decode_is_the_whole_frame_decode_to_the_bit() {
    for ((seed, (width, height), filter, partitions), expected) in
        drawn_cases().into_iter().zip(DRAWN)
    {
        let mut rng = Prng::new(seed);
        let bytes = super::fixture::drawn_keyframe(width, height, filter, partitions, &mut rng);
        let image = decode(&bytes, &limits()).expect("a drawn keyframe decodes");
        assert_eq!(fnv(image.pixels()), expected, "seed {seed:#x}");
        let mut streamed = Vec::new();
        super::decode_rows(&bytes, &limits(), |row| {
            streamed.extend_from_slice(row);
            Ok(())
        })
        .expect("and streams");
        assert_eq!(
            streamed,
            image.pixels(),
            "seed {seed:#x} streams the same rows"
        );
    }
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
        let bytes = super::fixture::drawn_keyframe(48, height, filter, 1, &mut Prng::new(1));
        let size = super::first_partition(&bytes).expect("a first partition");
        let head = &bytes[super::UNCOMPRESSED_HEADER..][..size];
        let header = super::read_header(&mut super::Bool::new(head), 48, height).expect("a header");
        let frame = super::Frame::new(&header).expect("a frame");
        let planes = [
            &frame.luma,
            &frame.blue_chroma,
            &frame.red_chroma,
            &frame.window.luma,
            &frame.window.blue_chroma,
            &frame.window.red_chroma,
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
