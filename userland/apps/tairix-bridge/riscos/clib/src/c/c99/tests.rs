//! Chunks 5, 7 and 8 held to the answers RISC OS 5.30 gave the maths
//! probes, each call made through the chunk's own entry with the client's
//! FPSR as a C program starts with it.

extern crate std;

use tairix_riscos_arm::fpa::Exception;

use crate::c::math::LARGE_ARGUMENT;
use crate::c::testing::replay::{disagreements, within_ulps, Call};

const REAL: &str = include_str!("tests/real.txt");
const COMPLEX: &str = include_str!("tests/complex.txt");

/// Where `acosh` and `asinh` inherit a square root FPEmulator gives places
/// off correct rounding, where the FPA's is correctly rounded by design: the
/// recorded arguments' bits, and how many places apart the answers are.
const FPEMULATOR_PLACES_OFF: [(u64, u64); 5] = [
    (0x3FF8_0000_0000_0000, 1),
    (0x403E_0000_0000_0000, 1),
    (0x3FF0_0068_DB8B_AC71, 2),
    (0x3FFC_0000_0000_0000, 1),
    (0x3FEF_FFFF_CA50_1ACB, 1),
];

/// How far the gamma and error functions' answers may lie from genuine's,
/// whose approximations are not known: `lgamma`'s by 2·10⁻¹² of the answer
/// or of one, `tgamma`'s by 5·10⁻¹⁰ of the answer, the floats' by 10⁻⁵ of
/// theirs, `lgammaf`'s or of one, and `erf` and `erfc`'s by 10⁻¹³ of the
/// answer. A logarithm of Γ errs absolutely by Γ's relative error, so the
/// logarithms are held to one where their answers are smaller.
const LGAMMA_SPREAD: f64 = 2e-12;
const TGAMMA_SPREAD: f64 = 5e-10;
const FLOAT_GAMMA_SPREAD: f64 = 1e-5;
const ERROR_FUNCTION_SPREAD: f64 = 1e-13;
/// A float's last place, in a double's; `erfcf` is two from genuine's.
const FLOAT_PLACE: u64 = 1 << 29;
const ERFCF_PLACES: u64 = 2 * FLOAT_PLACE;

/// The known differences: `acosh` and `asinh` at [`FPEMULATOR_PLACES_OFF`],
/// and the gamma and error functions' values by their spreads.
fn excused(call: &Call, key: &str, want: u64, got: u64) -> bool {
    if key != "F0" {
        return false;
    }
    let spread = |spread: f64, floor: f64| {
        let (want, got) = (f64::from_bits(want), f64::from_bits(got));
        (want - got).abs() <= spread * want.abs().max(floor)
    };
    match (call.chunk, call.entry) {
        (7, 7 | 9) => {
            let x = call.x().to_bits();
            FPEMULATOR_PLACES_OFF
                .iter()
                .any(|&(at, places)| at == x && within_ulps(want, got, places))
        }
        (7, 44) => spread(LGAMMA_SPREAD, 1.0),
        (7, 46) => spread(TGAMMA_SPREAD, 0.0),
        (7, 45) => spread(FLOAT_GAMMA_SPREAD, 1.0),
        (7, 47) => spread(FLOAT_GAMMA_SPREAD, 0.0),
        (7, 40 | 42) => spread(ERROR_FUNCTION_SPREAD, 0.0),
        (7, 43) => within_ulps(want, got, ERFCF_PLACES),
        _ => false,
    }
}

#[test]
fn the_real_functions_answer_as_genuine_s() {
    let found = disagreements(REAL, excused);
    assert!(
        found.is_empty(),
        "{} disagree:\n{}",
        found.len(),
        found.join("\n")
    );
}

/// `erf` and `erfc` of numbers so small that the series' later terms lie
/// below extended's range: inexact, with no underflow, as the answers are
/// normal.
const SMALL_ERROR_FUNCTIONS: &str = "\
7.40 R=3379B604,AAACA626,@E,@F -> F0=337D030322A4BC8D errno=0 trap=0 flags=10
7.40 R=2B2BFF2E,E48E0530,@E,@F -> F0=2B2F974CD8B00F7E errno=0 trap=0 flags=10
7.40 R=A0CA2FE7,6A3F9475,@E,@F -> F0=A0CD8C8BAAA00773 errno=0 trap=0 flags=10
7.42 R=3379B604,AAACA626,@E,@F -> F0=3FF0000000000000 errno=0 trap=0 flags=10
7.42 R=A0CA2FE7,6A3F9475,@E,@F -> F0=3FF0000000000000 errno=0 trap=0 flags=10
";

#[test]
fn an_error_function_of_a_small_number_raises_no_underflow() {
    let found = disagreements(SMALL_ERROR_FUNCTIONS, |_, _, _, _| false);
    assert!(found.is_empty(), "{}", found.join("\n"));
}

/// Two places of extended precision at one, 2⁻⁶²: how far FPEmulator's
/// extended arithmetic, where ours rounds correctly, moves a logarithm of a
/// magnitude within a rounding of one.
const EXTENDED_PLACES_AT_ONE: f64 = 2.168_404_344_971_009e-19;
/// How far FPEmulator's circular functions near a multiple of π/2 move
/// `cpow`'s parts, as a fraction of the answer's larger part: 2⁻⁶⁰.
const CPOW_SPREAD: f64 = 8.673_617_379_884_035e-19;
/// The complex functions' `cosh` and `sinh` past one, while `e^−|x|` still
/// reaches the last place, whose evaluation there is not derived: within two
/// places.
const HYPERBOLIC_PLACES: u64 = 2;

/// A chunk 8 function's argument: R0 to R3's doubles, or a float function's
/// two raw singles.
fn argument(call: &Call) -> (f64, f64) {
    if call.entry % 2 == 1 {
        (call.single(0), call.single(1))
    } else {
        (call.double(0), call.double(2))
    }
}

/// Chunk 8's known differences: a NaN's sign and payload, which C leaves
/// unspecified, but for the functions that copy or negate one; the division
/// helpers' NaNs for a NaN operand, where genuine's answer is not one;
/// `cproj`, which answers nothing on genuine; `cacosh(±0 ± i0)`, C99's `+0`
/// where genuine's real part is a rounding's 1.37·10⁻¹⁶; the inverse
/// functions' logarithm parts within [`EXTENDED_PLACES_AT_ONE`], their signs
/// alike; the circular and hyperbolic functions within
/// [`HYPERBOLIC_PLACES`] past one; `cpow` within
/// [`CPOW_SPREAD`]; and `ctanh` and `ctan`'s underflow, which genuine does not
/// flag where both parts are nonzero numbers.
fn complex_excused(call: &Call, key: &str, want: u64, got: u64) -> bool {
    let (x, y) = argument(call);
    let part = key == "F0" || key == "F1";
    let (wanted, ours) = (f64::from_bits(want), f64::from_bits(got));
    if part && wanted.is_nan() && ours.is_nan() && !matches!(call.entry, 42..=45 | 48 | 49) {
        return true;
    }
    let hyperbolic_places = |argument: f64| {
        (1.0..LARGE_ARGUMENT).contains(&argument.abs()) && within_ulps(want, got, HYPERBOLIC_PLACES)
    };
    let logarithm_within = || {
        (wanted - ours).abs() <= EXTENDED_PLACES_AT_ONE
            && wanted.is_sign_negative() == ours.is_sign_negative()
    };
    match call.entry {
        2..=5 => part && ours.is_nan() && call.operands().iter().any(|operand| operand.is_nan()),
        46 | 47 => part,
        18 | 19 if key == "F0" && x == 0.0 && y == 0.0 => true,
        6..=11 => key == "F1" && logarithm_within(),
        18..=23 => key == "F0" && logarithm_within(),
        12..=15 => part && hyperbolic_places(y),
        24..=27 => part && hyperbolic_places(x),
        16 | 17 | 28 | 29 => {
            key == "flags" && got == want | UNDERFLOW && x.is_normal() && y.is_normal()
        }
        36 | 37 => {
            let magnitude = |key| {
                call.answer(key)
                    .map_or(0.0, |bits| f64::from_bits(bits).abs())
            };
            part && (wanted - ours).abs() <= CPOW_SPREAD * magnitude("F0").max(magnitude("F1"))
        }
        _ => false,
    }
}

/// The underflow flag, as a record's flags hold it.
const UNDERFLOW: u64 = Exception::Underflow.flag() as u64;

#[test]
fn the_complex_functions_answer_as_genuine_s() {
    let found = disagreements(COMPLEX, complex_excused);
    assert!(
        found.is_empty(),
        "{} disagree:\n{}",
        found.len(),
        found.join("\n")
    );
}

/// Where chunk 8 answers as C99 gives rather than as RISC OS 5.30 does:
/// `cproj`'s projection, `cacosh(±0 ± i0)`'s `+0 ± iπ/2`, inexact as
/// genuine's working raises it, and a division helper's NaNs for a NaN
/// operand.
const DOCUMENTED: &str = "\
8.46 R=7FF00000,00000000,BFF00000,00000000 -> F0=7FF0000000000000 F1=8000000000000000 errno=0 trap=0 flags=00
8.46 R=3FF00000,00000000,7FF00000,00000000 -> F0=7FF0000000000000 F1=0000000000000000 errno=0 trap=0 flags=00
8.46 R=7FF80000,00000000,FFF00000,00000000 -> F0=7FF0000000000000 F1=8000000000000000 errno=0 trap=0 flags=00
8.46 R=40000000,00000000,C0080000,00000000 -> F0=4000000000000000 F1=C008000000000000 errno=0 trap=0 flags=00
8.47 R=FF800000,3F800000,@E,@F -> F0=7FF0000000000000 F1=0000000000000000 errno=0 trap=0 flags=00
8.18 R=00000000,00000000,00000000,00000000 -> F0=0000000000000000 F1=3FF921FB54442D18 errno=0 trap=0 flags=10
8.18 R=80000000,00000000,80000000,00000000 -> F0=0000000000000000 F1=BFF921FB54442D18 errno=0 trap=0 flags=10
8.19 R=00000000,80000000,@E,@F -> F0=0000000000000000 F1=BFF921FB60000000 errno=0 trap=0 flags=10
8.2 R=00000000,00000000,00000000,00000000 F=7FF8000000000000,0000000000000000,3FF0000000000000,3FF0000000000000 -> F0=7FF8000000000000 F1=7FF8000000000000 errno=0 trap=0 flags=00
8.4 R=00000000,00000000,00000000,00000000 F=7FF8000000000000,0000000000000000,3FF0000000000000,3FF0000000000000 -> F0=7FF8000000000000 F1=7FF8000000000000 errno=0 trap=0 flags=00
";

#[test]
fn where_genuine_departs_from_c99_the_library_answers_as_c99_gives() {
    let found = disagreements(DOCUMENTED, |_, _, _, _| false);
    assert!(
        found.is_empty(),
        "{} disagree:\n{}",
        found.len(),
        found.join("\n")
    );
}
