use core::sync::atomic::AtomicBool;

use tairix_abi::time::MonotonicClock;
use tairix_spirv::Component;

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
        helpers: 0,
        local: [0; LANES],
        group_id: [[0; 3]; LANES],
        group_slot: [0; LANES],
        subgroup: [(0, 4); LANES],
    };
    let (mut live, mut killed) = (0b1111, 0);
    let space = Space {
        planes: &mut planes,
        uniforms: &mut [],
        private: &mut [],
        shared: &mut [],
        masks: &mut [],
        pending: &mut [],
        live: &mut live,
        killed: &mut killed,
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
