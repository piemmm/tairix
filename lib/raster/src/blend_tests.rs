//! The blend stage: every factor and operation as stated, values and factors
//! held to a normalised range, minimum and maximum unscaled, and every logic
//! operation's truth table.

use super::{Blend, BlendFactor, BlendInputs, BlendOp, BlendRange, LogicOp};

const INPUTS: BlendInputs = BlendInputs {
    source: [0.25, 0.5, 0.75, 0.5],
    source1: [0.1, 0.2, 0.3, 0.4],
    destination: [0.8, 0.6, 0.4, 0.2],
    constant: [0.3, 0.6, 0.9, 0.7],
};

fn equation(source: BlendFactor, destination: BlendFactor, op: BlendOp) -> Blend {
    Blend {
        source_color: source,
        destination_color: destination,
        color: op,
        source_alpha: source,
        destination_alpha: destination,
        alpha: op,
    }
}

fn close(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(b).all(|(a, b)| (a - b).abs() <= 1e-6)
}

#[test]
fn premultiplied_over_is_the_source_and_what_its_alpha_leaves() {
    let out = Blend::OVER.apply(BlendRange::Unsigned, INPUTS);
    let expected = core::array::from_fn(|i| INPUTS.source[i] + INPUTS.destination[i] * 0.5);
    assert!(close(out, expected), "{out:?}");
}

#[test]
fn every_factor_scales_by_what_it_names() {
    let (s, s1, d, c) = (
        INPUTS.source,
        INPUTS.source1,
        INPUTS.destination,
        INPUTS.constant,
    );
    let cases: [(BlendFactor, [f32; 4]); 19] = [
        (BlendFactor::Zero, [0.0; 4]),
        (BlendFactor::One, [1.0; 4]),
        (BlendFactor::SourceColor, s),
        (BlendFactor::OneMinusSourceColor, s.map(|v| 1.0 - v)),
        (BlendFactor::DestinationColor, d),
        (BlendFactor::OneMinusDestinationColor, d.map(|v| 1.0 - v)),
        (BlendFactor::SourceAlpha, [s[3]; 4]),
        (BlendFactor::OneMinusSourceAlpha, [1.0 - s[3]; 4]),
        (BlendFactor::DestinationAlpha, [d[3]; 4]),
        (BlendFactor::OneMinusDestinationAlpha, [1.0 - d[3]; 4]),
        (BlendFactor::ConstantColor, c),
        (BlendFactor::OneMinusConstantColor, c.map(|v| 1.0 - v)),
        (BlendFactor::ConstantAlpha, [c[3]; 4]),
        (BlendFactor::OneMinusConstantAlpha, [1.0 - c[3]; 4]),
        (BlendFactor::SourceAlphaSaturate, [0.5, 0.5, 0.5, 1.0]),
        (BlendFactor::Source1Color, s1),
        (BlendFactor::OneMinusSource1Color, s1.map(|v| 1.0 - v)),
        (BlendFactor::Source1Alpha, [s1[3]; 4]),
        (BlendFactor::OneMinusSource1Alpha, [1.0 - s1[3]; 4]),
    ];
    for (factor, scale) in cases {
        let out =
            equation(factor, BlendFactor::Zero, BlendOp::Add).apply(BlendRange::Unbounded, INPUTS);
        let expected = core::array::from_fn(|i| s[i] * scale[i]);
        assert!(close(out, expected), "{factor:?}: {out:?}");
        assert_eq!(
            equation(factor, BlendFactor::Zero, BlendOp::Add).reads_second_source(),
            matches!(
                factor,
                BlendFactor::Source1Color
                    | BlendFactor::OneMinusSource1Color
                    | BlendFactor::Source1Alpha
                    | BlendFactor::OneMinusSource1Alpha
            )
        );
    }
}

#[test]
fn every_operation_combines_as_it_says_and_the_extremes_are_unscaled() {
    let (s, d) = (INPUTS.source, INPUTS.destination);
    let scaled = |op| {
        equation(BlendFactor::One, BlendFactor::DestinationAlpha, op)
            .apply(BlendRange::Unbounded, INPUTS)
    };
    assert!(close(
        scaled(BlendOp::Add),
        core::array::from_fn(|i| s[i] + d[i] * d[3])
    ));
    assert!(close(
        scaled(BlendOp::Subtract),
        core::array::from_fn(|i| s[i] - d[i] * d[3])
    ));
    assert!(close(
        scaled(BlendOp::ReverseSubtract),
        core::array::from_fn(|i| d[i] * d[3] - s[i])
    ));
    assert!(close(
        scaled(BlendOp::Min),
        core::array::from_fn(|i| if s[i] < d[i] { s[i] } else { d[i] })
    ));
    assert!(close(
        scaled(BlendOp::Max),
        core::array::from_fn(|i| if s[i] > d[i] { s[i] } else { d[i] })
    ));
}

#[test]
fn a_normalised_attachment_holds_values_and_factors_to_its_range() {
    let wild = BlendInputs {
        source: [1.5, -0.5, 0.5, 2.0],
        destination: [-2.0, 3.0, 0.5, -1.0],
        ..INPUTS
    };
    let add = equation(BlendFactor::One, BlendFactor::One, BlendOp::Add);
    assert!(close(
        add.apply(BlendRange::Unsigned, wild),
        [1.0, 1.0, 1.0, 1.0]
    ));
    assert!(close(
        add.apply(BlendRange::Signed, wild),
        [0.0, 0.5, 1.0, 0.0]
    ));
    assert!(close(
        add.apply(BlendRange::Unbounded, wild),
        [-0.5, 2.5, 1.0, 1.0]
    ));
    // One less a signed value reaches two, which the factor's range holds to one.
    let held = equation(
        BlendFactor::OneMinusSourceColor,
        BlendFactor::Zero,
        BlendOp::Add,
    );
    let out = held.apply(
        BlendRange::Signed,
        BlendInputs {
            source: [-1.0, -1.0, -1.0, -1.0],
            ..INPUTS
        },
    );
    assert!(close(out, [-1.0; 4]), "{out:?}");
}

#[test]
fn every_logic_operation_has_its_truth_table() {
    let (s, d) = (0b1100u32, 0b1010u32);
    let low = |value: u32| value & 0b1111;
    let cases = [
        (LogicOp::Clear, 0b0000),
        (LogicOp::And, 0b1000),
        (LogicOp::AndReverse, 0b0100),
        (LogicOp::Copy, 0b1100),
        (LogicOp::AndInverted, 0b0010),
        (LogicOp::NoOp, 0b1010),
        (LogicOp::Xor, 0b0110),
        (LogicOp::Or, 0b1110),
        (LogicOp::Nor, 0b0001),
        (LogicOp::Equivalent, 0b1001),
        (LogicOp::Invert, 0b0101),
        (LogicOp::OrReverse, 0b1101),
        (LogicOp::CopyInverted, 0b0011),
        (LogicOp::OrInverted, 0b1011),
        (LogicOp::Nand, 0b0111),
        (LogicOp::Set, 0b1111),
    ];
    for (op, table) in cases {
        assert_eq!(low(op.apply(s, d)), table, "{op:?}");
    }
}
