//! The stencil stage: each operation as named, the comparison and the writes
//! through their masks, and each outcome its own operation.

use super::{Outcome, StencilFace, StencilOp};
use crate::depth::CompareOp;

fn every(op: StencilOp) -> StencilFace {
    StencilFace {
        fail: op,
        pass: op,
        depth_fail: op,
        reference: 0x5a,
        ..StencilFace::KEEP
    }
}

#[test]
fn every_operation_does_what_it_is_named() {
    let cases = [
        (StencilOp::Keep, [0, 1, 254, 255]),
        (StencilOp::Zero, [0, 0, 0, 0]),
        (StencilOp::Replace, [0x5a, 0x5a, 0x5a, 0x5a]),
        (StencilOp::IncrementAndClamp, [1, 2, 255, 255]),
        (StencilOp::DecrementAndClamp, [0, 0, 253, 254]),
        (StencilOp::Invert, [255, 254, 1, 0]),
        (StencilOp::IncrementAndWrap, [1, 2, 255, 0]),
        (StencilOp::DecrementAndWrap, [255, 0, 253, 254]),
    ];
    for (op, expected) in cases {
        let face = every(op);
        let written = [0u8, 1, 254, 255].map(|held| face.write(Outcome::Passed, held));
        assert_eq!(written, expected, "{op:?}");
    }
}

#[test]
fn a_write_changes_only_the_bits_its_mask_names() {
    let face = StencilFace {
        write_mask: 0x0f,
        ..every(StencilOp::Replace)
    };
    assert_eq!(face.write(Outcome::Passed, 0xf0), 0xfa);
    let none = StencilFace {
        write_mask: 0,
        ..every(StencilOp::Invert)
    };
    assert_eq!(none.write(Outcome::Passed, 0x33), 0x33);
}

#[test]
fn the_comparison_reads_the_reference_and_what_is_held_through_its_mask() {
    let face = StencilFace {
        compare: CompareOp::Equal,
        compare_mask: 0x0f,
        reference: 0x1a,
        ..StencilFace::KEEP
    };
    assert!(face.passes(0xfa));
    assert!(!face.passes(0x1b));
    let less = StencilFace {
        compare: CompareOp::Less,
        compare_mask: u8::MAX,
        reference: 3,
        ..StencilFace::KEEP
    };
    assert!(
        less.passes(4) && !less.passes(3),
        "the reference is the value compared"
    );
    assert!(StencilFace::KEEP.passes(0x77));
}

#[test]
fn each_outcome_takes_its_own_operation() {
    let face = StencilFace {
        fail: StencilOp::Zero,
        depth_fail: StencilOp::Invert,
        pass: StencilOp::IncrementAndWrap,
        ..StencilFace::KEEP
    };
    assert_eq!(face.write(Outcome::StencilFailed, 9), 0);
    assert_eq!(face.write(Outcome::DepthFailed, 9), !9);
    assert_eq!(face.write(Outcome::Passed, 9), 10);
}
