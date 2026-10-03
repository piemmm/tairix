use super::{Density, DensityUnit, Stated};

#[test]
fn a_density_refuses_a_zero_part() {
    assert!(Density::new((0, 1), (72, 1), DensityUnit::Inch).is_none());
    assert!(Density::new((72, 0), (72, 1), DensityUnit::Inch).is_none());
    assert!(Density::whole(72, 0, DensityUnit::Inch).is_none());
    let held = Density::new((300, 2), (75, 1), DensityUnit::Centimetre).expect("valid");
    assert_eq!(
        (held.across(), held.down(), held.unit()),
        ((300, 2), (75, 1), DensityUnit::Centimetre)
    );
}

#[test]
fn a_density_converts_between_lengths_rounding_to_the_nearest() {
    let inch = Density::whole(72, 96, DensityUnit::Inch).expect("valid");
    assert_eq!(inch.whole_in(DensityUnit::Metre), Some((2835, 3780)));
    assert_eq!(inch.whole_in(DensityUnit::Centimetre), Some((28, 38)));
    assert_eq!(inch.whole_in(DensityUnit::Inch), Some((72, 96)));
    let metre = Density::whole(2835, 2835, DensityUnit::Metre).expect("valid");
    assert_eq!(metre.whole_in(DensityUnit::Inch), Some((72, 72)));
    let fraction = Density::new((1441, 5), (72, 1), DensityUnit::Inch).expect("valid");
    assert_eq!(fraction.whole_in(DensityUnit::Inch), Some((288, 72)));
}

#[test]
fn a_shape_and_a_length_do_not_convert() {
    let shape = Density::whole(2, 1, DensityUnit::Aspect).expect("valid");
    assert_eq!(shape.whole_in(DensityUnit::Metre), None);
    assert_eq!(
        Density::whole(72, 72, DensityUnit::Inch)
            .expect("valid")
            .whole_in(DensityUnit::Aspect),
        None
    );
}

#[test]
fn a_shape_keeps_its_proportion_in_its_smallest_terms() {
    let shape = Density::new((4, 3), (2, 3), DensityUnit::Aspect).expect("valid");
    assert_eq!(shape.whole_in(DensityUnit::Aspect), Some((2, 1)));
}

#[test]
fn a_figure_that_rounds_to_nothing_or_past_u32_is_refused() {
    let tiny = Density::whole(1, 1, DensityUnit::Metre).expect("valid");
    assert_eq!(tiny.whole_in(DensityUnit::Centimetre), None);
    let huge = Density::whole(u32::MAX, 1, DensityUnit::Inch).expect("valid");
    assert_eq!(huge.whole_in(DensityUnit::Metre), None);
}

#[test]
fn a_density_restates_exactly_in_another_length() {
    let metre = Density::whole(2835, 3780, DensityUnit::Metre).expect("valid");
    let centimetre = metre.exact_in(DensityUnit::Centimetre).expect("exact");
    assert_eq!(
        (centimetre.across(), centimetre.down()),
        ((567, 20), (189, 5))
    );
    assert_eq!(centimetre.whole_in(DensityUnit::Metre), Some((2835, 3780)));
    let inch = Density::whole(72, 72, DensityUnit::Inch).expect("valid");
    let restated = inch.exact_in(DensityUnit::Metre).expect("exact");
    assert_eq!(restated.across(), (360_000, 127));
    assert_eq!(inch.exact_in(DensityUnit::Aspect), None);
    let shape = Density::new((4, 1), (8, 1), DensityUnit::Aspect).expect("valid");
    assert_eq!(shape.exact_in(DensityUnit::Aspect), Some(shape));
    let huge = Density::whole(u32::MAX, 1, DensityUnit::Inch).expect("valid");
    assert_eq!(huge.exact_in(DensityUnit::Metre), None, "past u32");
}

#[test]
fn a_shape_is_the_proportion_whatever_the_unit() {
    let physical = Density::whole(300, 600, DensityUnit::Inch).expect("valid");
    assert_eq!(physical.shape(), Some((1, 2)));
}

#[test]
fn square_pixels_with_no_unit_state_no_density() {
    assert_eq!(
        Stated::of((3, 1), (6, 2), Some(DensityUnit::Aspect)),
        Stated::Kept(None)
    );
    assert_eq!(
        Stated::of((3, 1), (6, 2), Some(DensityUnit::Inch)),
        Stated::Kept(Density::new((3, 1), (6, 2), DensityUnit::Inch))
    );
    assert_eq!(
        Stated::of((0, 1), (6, 2), Some(DensityUnit::Inch)),
        Stated::Unkept
    );
    assert_eq!(Stated::of((3, 1), (6, 2), None), Stated::Unkept);
}
