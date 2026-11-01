use core::sync::atomic::{AtomicBool, AtomicU32};

use tairix_abi::time::MonotonicClock;
use tairix_spirv::Component;

use super::{put_raw, raw_at};
use crate::software::shader::exec::{Env, Machine, Placement, Space};
use crate::software::shader::ir::{Image, ImageOp, Num, Program, Reg, LANES};
use crate::software::texel::Srgb;
use crate::software::Meter;

struct Stopped;

impl MonotonicClock for Stopped {
    fn now_ns(&self) -> u64 {
        0
    }
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

#[test]
fn a_projective_coordinate_s_implicit_derivatives_are_of_the_projected_point() {
    // Each lane of the quad holds (u q, v q, q), its own q.
    let points = [
        (0.25, 0.5, 2.0),
        (0.5, 0.5, 4.0),
        (0.25, 0.75, 8.0),
        (0.5, 0.75, 2.0),
    ];
    let mut planes = alloc::vec![0u32; 3 * LANES];
    for (lane, &(u, v, q)) in points.iter().enumerate() {
        for (word, value) in [u * q, v * q, q].into_iter().enumerate() {
            planes[word * LANES + lane] = f32::to_bits(value);
        }
    }
    let program = Program::new([4, 1, 1], Reg::varying(0));
    let srgb = Srgb::new();
    let env = Env {
        program: &program,
        bindings: &[],
        push: &[],
        srgb: &srgb,
        subgroup_size: 4,
        groups: [1, 1, 1],
    };
    let place = Placement {
        live: 0b1111,
        local: [0; LANES],
        group_id: [[0; 3]; LANES],
        group_slot: [0; LANES],
        subgroup: [(0, 4); LANES],
    };
    let mut live = 0b1111;
    let space = Space {
        planes: &mut planes,
        uniforms: &mut [],
        private: &mut [],
        shared: &mut [],
        masks: &mut [],
        pending: &mut [],
        live: &mut live,
        workgroup: &mut [],
    };
    let stop = AtomicBool::new(false);
    let mut meter = Meter {
        stop: &stop,
        clock: &Stopped,
        deadline: u64::MAX,
        unchecked: 0,
        read: false,
    };
    let machine = Machine::new(&env, &place, space, &mut meter);
    let image = Image {
        op: ImageOp::Sample {
            projective: true,
            dref: false,
        },
        binding: 0,
        sampler: None,
        handle: Reg::varying(8),
        coord: Reg::varying(0),
        coords: 3,
        coord_num: Num::F32,
        operands: crate::software::shader::ir::ImageOperands::default(),
        component: Component::Float,
        dst: Reg::varying(9),
    };
    let (along_x, along_y) = machine.quad_grads(&image, 0, (2, true));
    assert_eq!(&along_x[..2], &[0.25, 0.0], "from lane 0 to lane 1");
    assert_eq!(&along_y[..2], &[0.0, 0.25], "from lane 0 to lane 2");
}
