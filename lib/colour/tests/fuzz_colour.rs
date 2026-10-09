//! Deterministic fuzz harness for the colour notation and coordinates.
//!
//! Hex digits arrive from untrusted SVG assets and settings documents, and
//! coordinates from CSS `hsl()` numbers of any size, so:
//!
//! 1. [`parse_hex`] never panics on any text, and a spelling it accepts names
//!    a form of exactly that many digits whose colour, written back, reads
//!    back the same.
//! 2. [`Hsv`] and [`Hsl`] are total over every raw coordinate, and the colour
//!    either names comes back unchanged through its own coordinates.
//! 3. [`Hue::from_degrees_f64`] and [`Fraction::from_f64`] are total over
//!    every `f64`, `NaN` and the infinities included.
//! 4. [`Cmyk`] brings every ink back as a colour that comes back through its
//!    own inks, and the measured spaces — [`Lab`], [`Lch`], [`Illuminant`] —
//!    are total over every `f64`, a value outside sRGB answering clipped.
//!
//! The fixed sweep runs under plain `cargo test`; under `cargo xtask fuzz`
//! the same seeded stream keeps being drawn until the budget elapses.

use std::string::{String, ToString};

use tairix_colour::{parse_hex, Cmyk, Fraction, HexForm, Hsl, Hsv, Hue, Illuminant, Lab, Lch};
use tairix_fuzzseed::Prng;

/// Fixed-iteration sweep run when no budget is set.
const SMOKE_ITERATIONS: u64 = 5_000;

/// Characters a hex spelling is drawn from: mostly digits, so accepted
/// spellings are common, with the near misses that must be refused.
const ALPHABET: &[u8] = b"0123456789abcdefABCDEF#+- gGxX\xc3\xa9";

/// Draw from `rng` until the budget runs out, a sweep at a time.
fn fuzz(name: &str, mut each: impl FnMut(&mut Prng)) {
    let mut rng = Prng::new(tairix_fuzzseed::start(name, tairix_fuzzseed::FUZZ_SEED_ENV));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            each(&mut rng);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}

#[test]
fn any_text_is_read_as_a_colour_or_refused_and_what_is_read_reads_back() {
    let mut bytes = Vec::new();
    fuzz("fuzz_colour::hex", |rng| {
        bytes.clear();
        for _ in 0..rng.below(10) {
            bytes.push(*rng.pick(ALPHABET));
        }
        let text = String::from_utf8_lossy(&bytes);
        let Some((colour, form)) = parse_hex(&text) else {
            return;
        };
        let digits = match form {
            HexForm::Short => 3,
            HexForm::ShortAlpha => 4,
            HexForm::Long => 6,
            HexForm::LongAlpha => 8,
        };
        assert_eq!(text.len(), digits, "{text:?}");
        assert!(form.has_alpha() || colour.is_opaque());
        let written = colour.hex().to_string();
        assert_eq!(
            parse_hex(&written).map(|(read, _)| read),
            Some(colour),
            "{text:?}"
        );
    });
}

#[test]
fn every_coordinate_names_a_colour_that_comes_back_through_its_own() {
    fuzz("fuzz_colour::coordinates", |rng| {
        let hue = Hue::from_steps(rng.next_u32());
        let (a, b) = (
            Fraction::from_raw(rng.next_u16()),
            Fraction::from_raw(rng.next_u16()),
        );
        let hsv = Hsv::new(hue, a, b);
        let rgb = hsv.to_rgb();
        assert_eq!(Hsv::from_rgb(rgb, hsv).to_rgb(), rgb, "{hsv:?}");
        let hsl = Hsl::new(hue, a, b);
        let rgb = hsl.to_rgb();
        assert_eq!(Hsl::from_rgb(rgb, hsl).to_rgb(), rgb, "{hsl:?}");
    });
}

#[test]
fn every_css_number_is_held_to_the_circle_and_the_unit() {
    fuzz("fuzz_colour::numbers", |rng| {
        let number = f64::from_bits(rng.next_u64());
        assert!(
            Hue::from_degrees_f64(number).steps() < Hue::TURN,
            "{number}"
        );
        let fraction = Fraction::from_f64(number);
        if number.is_nan() || number <= 0.0 {
            assert_eq!(fraction, Fraction::NONE, "{number}");
        }
        if number >= 1.0 {
            assert_eq!(fraction, Fraction::ALL, "{number}");
        }
    });
}

#[test]
fn every_ink_names_a_colour_that_comes_back_through_its_own_inks() {
    fuzz("fuzz_colour::cmyk", |rng| {
        let mut ink = || Fraction::from_raw(rng.next_u16());
        let inks = Cmyk::new(ink(), ink(), ink(), ink());
        let rgb = inks.to_rgb();
        assert_eq!(Cmyk::from_rgb(rgb).to_rgb(), rgb, "{inks:?}");
    });
}

#[test]
fn every_measured_number_lands_in_srgb_or_is_refused() {
    fuzz("fuzz_colour::measured", |rng| {
        let mut number = || f64::from_bits(rng.next_u64());
        let lab = Lab {
            l: number(),
            a: number(),
            b: number(),
        };
        let _ = lab.to_rgb();
        let lch = Lch {
            l: number(),
            c: number(),
            h: number(),
        };
        let _ = lch.to_rgb();
        let light = Illuminant::new(number(), number());
        let _ = light.white();
        let _ = Illuminant::of_linear([number(), number(), number()]);
    });
}
