use crate::{Cmyk, Fraction, Rgb};

#[test]
fn every_colour_survives_cyan_magenta_yellow_and_key() {
    for packed in 0..=0x00FF_FFFFu32 {
        let [_, r, g, b] = packed.to_be_bytes();
        let rgb = Rgb::new(r, g, b);
        assert_eq!(Cmyk::from_rgb(rgb).to_rgb(), rgb, "{rgb:?}");
    }
}

#[test]
fn the_key_takes_all_the_black_a_colour_holds() {
    assert_eq!(
        Cmyk::from_rgb(Rgb::BLACK),
        Cmyk::new(
            Fraction::NONE,
            Fraction::NONE,
            Fraction::NONE,
            Fraction::ALL
        )
    );
    assert_eq!(Cmyk::from_rgb(Rgb::WHITE), Cmyk::default());
    let red = Cmyk::from_rgb(Rgb::new(255, 0, 0));
    assert_eq!(
        (red.cyan, red.magenta, red.yellow, red.key),
        (Fraction::NONE, Fraction::ALL, Fraction::ALL, Fraction::NONE)
    );
    // A grey is black ink alone, however light.
    let grey = Cmyk::from_rgb(Rgb::new(128, 128, 128));
    assert_eq!(
        (grey.cyan, grey.magenta, grey.yellow),
        (Fraction::NONE, Fraction::NONE, Fraction::NONE)
    );
    assert_eq!(grey.key.percent(), 50);
}

#[test]
fn full_key_is_black_whatever_the_other_inks() {
    let inks = Cmyk::new(
        Fraction::from_percent(20),
        Fraction::ALL,
        Fraction::NONE,
        Fraction::ALL,
    );
    assert_eq!(inks.to_rgb(), Rgb::BLACK);
}
