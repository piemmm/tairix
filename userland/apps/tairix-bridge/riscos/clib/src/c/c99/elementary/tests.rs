use super::*;

/// `t = 2x²` as the error functions work it, in F4, with one in F2.
fn twice_the_square(e: &mut Extended<'_>, x: f64) {
    e.constant(1, x);
    e.constant(2, 1.0);
    e.constant(7, 2.0);
    e.op(FpaDyadicOp::Muf, 3, 1, 1).expect("x²");
    e.op(FpaDyadicOp::Muf, 4, 3, 7).expect("2x²");
}

#[test]
fn laplace_s_fraction_is_started_deep_enough_for_every_argument_it_serves() {
    // Extended carries eleven places more than a double.
    let place = f64::EPSILON / 2048.0;
    let mut upto = EXPONENTIAL_UNDERFLOWS;
    for (from, depth) in FRACTION_DEPTHS {
        for step in 0..32u8 {
            let x = from + (upto - from) * f64::from(step) / 32.0;
            let mut unit = Unit::own();
            let e = &mut Extended(&mut unit);
            twice_the_square(e, x);
            laplace_fraction(e, depth).expect("the fraction");
            e.mono(FpaMonadicOp::Mvf, 0, 5).expect("kept");
            laplace_fraction(e, 4 * depth).expect("the deeper fraction");
            e.op(FpaDyadicOp::Suf, 6, 0, 5).expect("the difference");
            let difference = e.answer(6).expect("the difference");
            let limit = e.answer(5).expect("the limit");
            assert!(
                difference.abs() <= limit * place,
                "{x}: {difference:e} off {limit}"
            );
        }
        upto = from;
    }
}

#[test]
fn each_argument_takes_the_depth_from_the_nearest_argument_below_it() {
    assert_eq!(fraction_depth(SERIES_BELOW), 63);
    assert_eq!(fraction_depth(2.999), 63);
    assert_eq!(fraction_depth(3.0), 47);
    assert_eq!(fraction_depth(9.5), 20);
    assert_eq!(fraction_depth(EXPONENTIAL_UNDERFLOWS), 13);
}
