extern crate std;

use tairix_fuzzseed::Prng;
use tairix_riscos_kernel::module::Code;
use tairix_softfloat::from_fpa_words;

use super::*;
use crate::c::statics::at;
use crate::c::testing::replay::{disagreements, within_ulps, Call};
use crate::c::testing::{Client, FREE, STATICS};

mod recorded;

use recorded::{ARGUMENTS, DENSE, FREXP, LDEXP, MODF, ONE, PAIRS, TWO};

/// `entry` called with `arguments`, `errno` nought first: F0's bits after,
/// and `errno`.
fn called(client: &mut Client, entry: Code, arguments: &[u32]) -> (u64, u32) {
    client.put(STATICS + at::ERRNO, &[0; 4]);
    client.call(entry, arguments);
    let bits = client
        .session
        .kernel
        .os()
        .fpa_double(0)
        .expect("F0 a double");
    (bits, client.word(STATICS + at::ERRNO))
}

/// Where our answer is only within an ulp of genuine's: `cosh` and `sinh`
/// past one while `e^−|x|` still reaches the answer's last place, whose
/// evaluation there is not derived; and the recorded FPA instructions whose
/// FPEmulator answer is a place off correct rounding, where the FPA's is
/// rounded correctly by design.
fn within_an_ulp(name: &str, x: f64) -> bool {
    let hyperbolic = matches!(name, "cosh" | "sinh") && x.abs() > 1.0 && x.abs() < LARGE_ARGUMENT;
    hyperbolic || (name == "cos" && x.abs().to_bits() == 1e5f64.to_bits())
}

/// Where `ours` is not genuine's answer, what differs.
fn disagreement(
    name: &str,
    x: f64,
    ours: (u64, u32),
    genuine: (u64, u32),
) -> Option<std::string::String> {
    let close = within_an_ulp(name, x) && ours.0.abs_diff(genuine.0) <= 1;
    let agreed = ours.1 == genuine.1 && (close || ours.0 == genuine.0);
    (!agreed).then(|| {
        std::format!(
            "{name}({x:e}): {:#018X} errno {} for {:#018X} errno {}",
            ours.0,
            ours.1,
            genuine.0,
            genuine.1
        )
    })
}

/// Genuine's answers to the later probes, flags and traps among them.
const GENUINE: &str = include_str!("tests/genuine.txt");
/// Genuine's answers with the client's underflow and inexact traps enabled.
const TRAPS: &str = include_str!("tests/traps.txt");

/// The known differences, by entry: [`within_an_ulp`]'s, and `fmod` of a
/// subnormal by a normal, which is that subnormal where genuine's is not.
fn excused(call: &Call, key: &str, want: u64, got: u64) -> bool {
    if (call.chunk, call.entry) == (2, 153) {
        let x = call.x();
        return key == "F0" && x.is_subnormal() && call.double(2).is_normal() && got == x.to_bits();
    }
    let name = match (call.chunk, call.entry) {
        (2, 136) => "cos",
        (2, 139) => "cosh",
        (2, 140) => "sinh",
        _ => return false,
    };
    key == "F0" && within_an_ulp(name, call.x()) && within_ulps(want, got, 1)
}

#[test]
fn every_function_answers_and_flags_as_genuine_s_records_say() {
    let found: std::vec::Vec<_> = [GENUINE, TRAPS]
        .into_iter()
        .flat_map(|records| disagreements(records, excused))
        .collect();
    assert!(
        found.is_empty(),
        "{} disagree:\n{}",
        found.len(),
        found.join("\n")
    );
}

#[test]
fn every_function_of_one_double_answers_as_genuine_does() {
    let mut client = Client::new();
    let mut disagreements = std::vec::Vec::new();
    for (name, entry, answers) in ONE {
        for (&x, genuine) in ARGUMENTS.iter().zip(answers) {
            let ours = called(&mut client, entry, &fpa_words(x));
            disagreements.extend(disagreement(name, f64::from_bits(x), ours, genuine));
        }
    }
    assert!(disagreements.is_empty(), "{disagreements:#?}");
}

#[test]
fn every_function_of_two_doubles_answers_as_genuine_does() {
    let mut client = Client::new();
    for (name, entry, answers) in TWO {
        for (&(a, b), genuine) in PAIRS.iter().zip(answers) {
            let [a_high, a_low] = fpa_words(a);
            let [b_high, b_low] = fpa_words(b);
            let ours = called(&mut client, entry, &[a_high, a_low, b_high, b_low]);
            let (a, b) = (f64::from_bits(a), f64::from_bits(b));
            assert_eq!(ours, genuine, "{name}({a}, {b})");
        }
    }
}

#[test]
fn frexp_modf_and_ldexp_answer_as_genuine_does() {
    let mut client = Client::new();
    for (x, fraction, power) in FREXP {
        let [high, low] = fpa_words(x);
        let (ours, errno) = called(&mut client, frexp, &[high, low, FREE]);
        assert_eq!(
            (ours, client.word(FREE).cast_signed(), errno),
            (fraction, power, 0),
            "frexp({x:#X})"
        );
    }
    for (x, fraction, whole) in MODF {
        let [high, low] = fpa_words(x);
        let (ours, errno) = called(&mut client, modf, &[high, low, FREE]);
        let stored = from_fpa_words([client.word(FREE), client.word(FREE + 4)]);
        assert_eq!((ours, stored, errno), (fraction, whole, 0), "modf({x:#X})");
    }
    for (x, power, value, errno) in LDEXP {
        let [high, low] = fpa_words(x);
        let ours = called(&mut client, ldexp, &[high, low, power.cast_unsigned()]);
        assert_eq!(ours, (value, errno), "ldexp({x:#X}, {power})");
    }
}

#[test]
fn the_hyperbolic_functions_answer_as_genuine_does_over_a_dense_spread() {
    let mut client = Client::new();
    let mut disagreements = std::vec::Vec::new();
    for (x, cosh_bits, sinh_bits, tanh_bits) in DENSE {
        let argument = fpa_words(x);
        let x = f64::from_bits(x);
        for (name, entry, genuine) in [
            ("cosh", cosh as Code, cosh_bits),
            ("sinh", sinh, sinh_bits),
            ("tanh", tanh, tanh_bits),
        ] {
            let ours = called(&mut client, entry, &argument);
            disagreements.extend(disagreement(name, x, ours, (genuine, 0)));
        }
    }
    assert!(disagreements.is_empty(), "{disagreements:#?}");
}

/// A finite double from anywhere in the range, subnormals and both signs
/// included.
fn any_finite(prng: &mut Prng) -> f64 {
    loop {
        let x = f64::from_bits(prng.next_u64());
        if x.is_finite() {
            return x;
        }
    }
}

/// Two to `power`, subnormal ones included.
fn power_of_two(power: i32) -> f64 {
    let bits = if power >= -1022 {
        u64::from((power + 1023).cast_unsigned()) << 52
    } else {
        1 << (power + 1074)
    };
    f64::from_bits(bits)
}

/// `x` times two to `power`, rounded by one host multiplication: after an
/// exact one where two to `power` is no double, and none where that would
/// round.
fn reference_scaled(x: f64, power: i32) -> Option<f64> {
    let last = power.clamp(-1074, 1023);
    let step = power - last;
    if !(-1022..=1023).contains(&step) {
        return None;
    }
    let first = x * power_of_two(step);
    let exact = first.is_normal() && (first * power_of_two(-step)).to_bits() == x.to_bits();
    exact.then(|| first * power_of_two(last))
}

#[test]
fn ldexp_rounds_once_however_far_it_scales() {
    let mut prng = Prng::new(tairix_fuzzseed::start(
        "ldexp_rounds_once",
        "TAIRIX_CLIB_LDEXP_SEED",
    ));
    let mut checked = 0;
    while checked < 20_000 {
        let x = any_finite(&mut prng);
        let power = i32::try_from(prng.below(4400)).expect("small") - 2200;
        let Some(reference) = reference_scaled(x, power) else {
            continue;
        };
        let ours = scaled(&mut Unit::own(), x, power).expect("no trap on the library's own unit");
        assert_eq!(ours.to_bits(), reference.to_bits(), "{x:e} × 2^{power}");
        checked += 1;
    }
}

#[test]
fn frexp_takes_every_finite_double_apart_exactly() {
    let mut prng = Prng::new(tairix_fuzzseed::start(
        "frexp_exact",
        "TAIRIX_CLIB_FREXP_SEED",
    ));
    let least = f64::from_bits(1);
    assert_eq!(fraction_and_power(least), (0.5, -1073));
    let edges = [
        least,
        -least,
        f64::MIN_POSITIVE,
        f64::MIN_POSITIVE - least,
        f64::MAX,
        f64::MIN,
    ];
    for x in edges
        .into_iter()
        .chain((0..20_000).map(|_| any_finite(&mut prng)))
    {
        let (fraction, power) = fraction_and_power(x);
        if x == 0.0 {
            assert_eq!((fraction.to_bits(), power), (x.to_bits(), 0));
            continue;
        }
        assert!(
            (0.5..1.0).contains(&fraction.abs()),
            "{x:e}'s fraction {fraction}"
        );
        let whole = reference_scaled(fraction, power).map(f64::to_bits);
        assert_eq!(whole, Some(x.to_bits()), "{x:e}");
    }
}

#[test]
fn fmod_and_the_roundings_are_the_host_s_exact_answers() {
    let mut prng = Prng::new(tairix_fuzzseed::start(
        "fmod_exact",
        "TAIRIX_CLIB_FMOD_SEED",
    ));
    let mut client = Client::new();
    for _ in 0..5_000 {
        let (x, y) = (any_finite(&mut prng), any_finite(&mut prng));
        let [x_high, x_low] = fpa_words(x.to_bits());
        if y != 0.0 {
            let [y_high, y_low] = fpa_words(y.to_bits());
            let ours = called(&mut client, fmod, &[x_high, x_low, y_high, y_low]);
            assert_eq!(ours, ((x % y).to_bits(), 0), "fmod({x:e}, {y:e})");
        }
        for (entry, exact) in [(ceil as Code, x.ceil()), (floor as Code, x.floor())] {
            assert_eq!(
                called(&mut client, entry, &[x_high, x_low]),
                (exact.to_bits(), 0),
                "{x:e}"
            );
        }
        let (fraction, errno) = called(&mut client, modf, &[x_high, x_low, FREE]);
        let whole = f64::from_bits(from_fpa_words([client.word(FREE), client.word(FREE + 4)]));
        assert_eq!(
            (whole.to_bits(), errno),
            (x.trunc().to_bits(), 0),
            "modf({x:e})'s whole part"
        );
        assert_eq!(
            fraction,
            (x - x.trunc()).to_bits(),
            "modf({x:e})'s fraction"
        );
    }
}

#[test]
fn the_errors_are_the_exceptions_the_work_raised() {
    let mut client = Client::new();
    let words = |x: f64| fpa_words(x.to_bits());
    let cases: [(Code, f64, f64, u32); 10] = [
        (exp, 1e5, HUGE_VAL, ERANGE),
        (exp, -1e5, 0.0, ERANGE),
        (exp, f64::INFINITY, f64::INFINITY, 0),
        (sinh, -1e5, -HUGE_VAL, ERANGE),
        (sinh, f64::NEG_INFINITY, f64::NEG_INFINITY, 0),
        (sqrt, -1.0, -HUGE_VAL, EDOM),
        (sqrt, f64::NAN, f64::NAN, 0),
        (log, 0.0, -HUGE_VAL, ERANGE),
        (log, f64::INFINITY, f64::INFINITY, 0),
        (tanh, f64::INFINITY, 1.0, 0),
    ];
    for (entry, x, want, errno) in cases {
        assert_eq!(
            called(&mut client, entry, &words(x)),
            (want.to_bits(), errno),
            "{x:e}"
        );
    }
    let [high, low] = words(1.0);
    assert_eq!(
        called(&mut client, ldexp, &[high, low, 5000]),
        (HUGE_VAL.to_bits(), ERANGE)
    );
    assert_eq!(
        called(&mut client, ldexp, &[high, low, (-5000i32).cast_unsigned()]),
        (0, ERANGE)
    );
}

#[test]
fn the_flags_some_functions_raise_are_left_the_client_s() {
    let mut client = Client::new();
    let half = fpa_words(0.5f64.to_bits());
    for (entry, left) in [
        (log as Code, true),
        (sqrt, true),
        (atan, true),
        (ceil, true),
        (cosh, true),
        (exp, false),
        (sin, false),
        (acos, false),
    ] {
        client.session.kernel.os().set_fpsr(0);
        called(&mut client, entry, &half);
        let flags = client.session.kernel.os().fpsr() & 0x1F;
        assert_eq!(
            flags,
            if left { Exception::Inexact.flag() } else { 0 },
            "{entry:p}"
        );
    }
    client.session.kernel.os().set_fpsr(0);
    called(&mut client, log, &fpa_words(0.0f64.to_bits()));
    assert_eq!(
        client.session.kernel.os().fpsr() & 0x1F,
        0,
        "an error leaves no flag"
    );
}
