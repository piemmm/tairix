use tairix_colour::{linear_to_srgb, srgb_to_linear};
use tairix_spirv::Component;
use tairix_util::mathf::single;

use core::sync::atomic::AtomicU32;

use super::{
    decode, encode, put_raw, raw_at, small_float, to_shared_exponent, to_small_float, Plane, Srgb,
};
use crate::format::{Aspects, Format};

fn float(value: f32) -> u32 {
    value.to_bits()
}

fn bits(values: [f32; 4]) -> [u32; 4] {
    values.map(f32::to_bits)
}

fn plane_of(format: Format) -> Plane {
    let aspects = format.aspects();
    if aspects.contains(Aspects::DEPTH) {
        Plane::Depth
    } else if aspects.contains(Aspects::STENCIL) {
        Plane::Stencil
    } else {
        Plane::Colour
    }
}

/// `raw` made a texel `format` stores as written: within its bytes, its
/// floats finite and positive, and no signed normalised channel at its
/// minimum code, which reads as minus one as the code above it does.
fn storable(format: Format, raw: [u32; 4]) -> [u32; 4] {
    let mut kept = match format {
        Format::Depth32FloatStencil8 | Format::Depth24PlusStencil8 | Format::Depth32Float => {
            [raw[0] & 0x3f7f_ffff, 0, 0, 0]
        }
        Format::Rg11B10Float | Format::Rgb9E5Float => [raw[0] & 0x7bdf_7bdf, 0, 0, 0],
        Format::R16Float | Format::Rg16Float | Format::Rgba16Float => raw.map(|w| w & 0x7bff_7bff),
        Format::R32Float | Format::Rg32Float | Format::Rgba32Float => raw.map(|w| w & 0x3fff_ffff),
        Format::R8Snorm | Format::Rg8Snorm | Format::Rgba8Snorm => raw.map(|w| {
            let bytes = w.to_le_bytes().map(|b| if b == 0x80 { 0x81 } else { b });
            u32::from_le_bytes(bytes)
        }),
        Format::R16Snorm | Format::Rg16Snorm | Format::Rgba16Snorm => raw.map(|w| {
            let halves = [w & 0xffff, w >> 16].map(|h| if h == 0x8000 { 0x8001 } else { h });
            halves[0] | halves[1] << 16
        }),
        _ => raw,
    };
    let bits = format.texel_bytes() * 8;
    for (index, word) in kept.iter_mut().enumerate() {
        let first = u32::try_from(index).unwrap_or(0) * 32;
        if first >= bits {
            *word = 0;
        } else if bits - first < 32 {
            *word &= (1 << (bits - first)) - 1;
        }
    }
    kept
}

#[test]
fn every_stored_texel_reads_back_as_it_was_written() {
    let srgb = Srgb::new();
    for format in Format::ALL {
        let plane = plane_of(format);
        for seed in 0u32..256 {
            let raw: [u32; 4] = core::array::from_fn(|index| {
                let turn = u32::try_from(index).unwrap_or(0) * 7;
                seed.wrapping_mul(0x9e37_79b9).rotate_left(turn) ^ seed << 24
            });
            let kept = storable(format, raw);
            let read = decode(format, plane, kept, &srgb);
            let written = encode(format, plane, read, &srgb);
            if format == Format::Rgb9E5Float {
                let back = decode(format, plane, written, &srgb);
                assert_eq!(back, read, "{format:?} changed {kept:x?}");
            } else {
                assert_eq!(written, kept, "{format:?} changed {kept:x?}");
            }
        }
    }
}

#[test]
fn a_missing_channel_reads_as_zero_and_a_missing_alpha_as_one() {
    let srgb = Srgb::new();
    let red = decode(Format::R8Unorm, Plane::Colour, [255, 0, 0, 0], &srgb);
    assert_eq!(red, [float(1.0), 0, 0, float(1.0)]);
    let int = decode(
        Format::Rg32Sint,
        Plane::Colour,
        [7, (-3i32).cast_unsigned(), 0, 0],
        &srgb,
    );
    assert_eq!(int, [7, (-3i32).cast_unsigned(), 0, 1]);
    assert_eq!(Format::Rg32Sint.component(), Component::Sint);
}

#[test]
fn normalised_channels_convert_as_the_portable_rules_give() {
    let srgb = Srgb::new();
    let snorm = decode(
        Format::Rgba8Snorm,
        Plane::Colour,
        [0x8081_7f00, 0, 0, 0],
        &srgb,
    );
    assert_eq!(
        snorm,
        bits([0.0, 1.0, -1.0, -1.0]),
        "-128 reads as -1, as -127 does"
    );
    let unorm = encode(
        Format::Rgba8Unorm,
        Plane::Colour,
        [float(0.5), float(2.0), float(-1.0), float(f32::NAN)],
        &srgb,
    );
    assert_eq!(
        unorm,
        [0x0000_ff80, 0, 0, 0],
        "clamped, rounded, and NaN as zero"
    );
    let signed = encode(
        Format::Rg16Snorm,
        Plane::Colour,
        [float(-0.5), float(1.5), 0, 0],
        &srgb,
    );
    assert_eq!(
        signed,
        [0x7fff_c000, 0, 0, 0],
        "half away from zero: -0.5 is -16384"
    );
    let packed = encode(
        Format::Rgb10A2Unorm,
        Plane::Colour,
        [float(1.0), 0, float(0.5), float(0.4)],
        &srgb,
    );
    assert_eq!(packed, [0x3ff | (512 << 20) | (1 << 30), 0, 0, 0]);
}

#[test]
fn blue_first_formats_swap_red_and_blue() {
    let srgb = Srgb::new();
    let bgra = decode(
        Format::Bgra8Unorm,
        Plane::Colour,
        [0xff00_00ff, 0, 0, 0],
        &srgb,
    );
    assert_eq!(bgra, bits([0.0, 0.0, 1.0, 1.0]));
    let stored = encode(
        Format::Bgra8Unorm,
        Plane::Colour,
        [float(1.0), 0, 0, float(1.0)],
        &srgb,
    );
    assert_eq!(stored, [0xff00_0000 | 0x00ff_0000, 0, 0, 0]);
}

#[test]
fn the_srgb_tables_are_the_colour_crates_transfer() {
    let srgb = Srgb::new();
    for code in 0u32..256 {
        let share = f64::from(code) / 255.0;
        let expected = single(srgb_to_linear(share));
        let read = decode(
            Format::Rgba8UnormSrgb,
            Plane::Colour,
            [code, 0, 0, 0],
            &srgb,
        );
        assert_eq!(read[0], expected.to_bits(), "code {code}");
        let back = encode(Format::Rgba8UnormSrgb, Plane::Colour, read, &srgb);
        assert_eq!(back[0] & 0xff, code, "code {code} round trips");
    }
    for step in 0u32..=1000 {
        let linear = f64::from(step) / 1000.0;
        let rounded = (linear_to_srgb(linear) * 255.0 + 0.5).floor();
        let code = encode(
            Format::Rgba8UnormSrgb,
            Plane::Colour,
            [single(linear).to_bits(), 0, 0, 0],
            &srgb,
        );
        assert!(
            (f64::from(code[0] & 0xff) - rounded).abs() <= 1.0,
            "linear {linear} encoded {code:?}, the transfer says {rounded}"
        );
    }
    let alpha = encode(
        Format::Rgba8UnormSrgb,
        Plane::Colour,
        [0, 0, 0, float(0.5)],
        &srgb,
    );
    assert_eq!(alpha[0] >> 24, 128, "alpha is linear");
}

#[test]
fn small_floats_round_to_nearest_and_saturate_finite_values() {
    assert_eq!(to_small_float(1.0, 6), 15 << 6);
    assert_eq!(to_small_float(-1.0, 6), 0, "negative is zero");
    assert_eq!(
        to_small_float(1.0e9, 6),
        (31 << 6) - 1,
        "finite past the largest is the largest"
    );
    assert_eq!(to_small_float(f32::INFINITY, 5), 31 << 5);
    assert_eq!(to_small_float(f32::NAN, 5) >> 5, 31);
    assert_ne!(to_small_float(f32::NAN, 5) & 31, 0);
    for bits in 0u32..(31 << 6) {
        assert_eq!(to_small_float(small_float(bits, 6), 6), bits, "{bits:#x}");
    }
    for bits in 0u32..(31 << 5) {
        assert_eq!(to_small_float(small_float(bits, 5), 5), bits, "{bits:#x}");
    }
}

#[test]
fn a_shared_exponent_keeps_the_largest_channel_in_nine_bits() {
    let srgb = Srgb::new();
    let packed = to_shared_exponent([float(1.0), float(0.5), float(0.25), 0]);
    let read = decode(Format::Rgb9E5Float, Plane::Colour, [packed, 0, 0, 0], &srgb);
    assert_eq!(read, bits([1.0, 0.5, 0.25, 1.0]));
    let saturated = to_shared_exponent([float(1.0e9), float(-2.0), float(f32::NAN), 0]);
    let read = decode(
        Format::Rgb9E5Float,
        Plane::Colour,
        [saturated, 0, 0, 0],
        &srgb,
    );
    assert_eq!(read, bits([65408.0, 0.0, 0.0, 1.0]));
    let carried = to_shared_exponent([float(511.9), 0, 0, 0]);
    let read = decode(
        Format::Rgb9E5Float,
        Plane::Colour,
        [carried, 0, 0, 0],
        &srgb,
    );
    assert_eq!(
        read[0],
        512.0f32.to_bits(),
        "rounding into the tenth bit raises the exponent"
    );
}

#[test]
fn depth_and_stencil_planes_are_stored_apart() {
    let srgb = Srgb::new();
    let depth = encode(
        Format::Depth32FloatStencil8,
        Plane::Depth,
        [float(0.25), 0, 0, 0],
        &srgb,
    );
    let stencil = encode(
        Format::Depth32FloatStencil8,
        Plane::Stencil,
        [0x1ab, 0, 0, 0],
        &srgb,
    );
    let texel = [depth[0] | stencil[0], depth[1] | stencil[1], 0, 0];
    assert_eq!(
        decode(Format::Depth32FloatStencil8, Plane::Depth, texel, &srgb)[0],
        float(0.25)
    );
    assert_eq!(
        decode(Format::Depth32FloatStencil8, Plane::Stencil, texel, &srgb),
        [0xab, 0, 0, 1]
    );
    let short = encode(
        Format::Depth16Unorm,
        Plane::Depth,
        [float(0.5), 0, 0, 0],
        &srgb,
    );
    assert_eq!(short[0], 32768);
}

#[test]
fn a_texel_s_bytes_are_read_at_any_alignment_and_nothing_past_them() {
    let stored: alloc::vec::Vec<u32> = (0..8u32).map(|k| 0x0403_0201 + k * 0x1010_1010).collect();
    let words: alloc::vec::Vec<AtomicU32> = stored.iter().map(|&w| AtomicU32::new(w)).collect();
    let bytes: alloc::vec::Vec<u8> = stored.iter().flat_map(|w| w.to_le_bytes()).collect();
    for byte in 0..20u64 {
        for len in 1..=16u64 {
            let mut expected = [0u32; 4];
            for k in 0..len {
                let at = usize::try_from(byte + k).expect("small");
                let value = u32::from(bytes.get(at).copied().unwrap_or(0));
                expected[usize::try_from(k / 4).expect("small")] |= value << (k % 4 * 8);
            }
            assert_eq!(
                raw_at(&words, byte, len),
                expected,
                "{len} bytes at byte {byte}"
            );
        }
    }
}

#[test]
fn a_texel_s_bytes_are_written_at_any_alignment_and_nothing_else() {
    let raw = [0x4433_2211, 0x8877_6655, 0xccbb_aa99, 0x00ff_eedd];
    let bytes_of: alloc::vec::Vec<u8> = raw.iter().flat_map(|w: &u32| w.to_le_bytes()).collect();
    for byte in 0..20u64 {
        for len in 1..=16u64 {
            let before: alloc::vec::Vec<u32> = (0..10u32).map(|k| 0x5a5a_5a5a ^ k).collect();
            let words: alloc::vec::Vec<AtomicU32> =
                before.iter().map(|&w| AtomicU32::new(w)).collect();
            put_raw(&words, byte, len, raw);
            let after: alloc::vec::Vec<u8> = words
                .iter()
                .flat_map(|w| w.load(core::sync::atomic::Ordering::Relaxed).to_le_bytes())
                .collect();
            let old: alloc::vec::Vec<u8> = before.iter().flat_map(|w| w.to_le_bytes()).collect();
            for (at, (&now, &was)) in after.iter().zip(&old).enumerate() {
                let at = at as u64;
                let expected = if (byte..byte + len).contains(&at) {
                    bytes_of[usize::try_from(at - byte).expect("small")]
                } else {
                    was
                };
                assert_eq!(now, expected, "byte {at} of {len} written at {byte}");
            }
        }
    }
}
