//! Deterministic fuzz harness for normalization and SASLprep.
//!
//! Text from anywhere reaches both — a name to compare, a password typed — so
//! the harness holds them to what the standards guarantee of any input: they
//! never panic, a buffer [`MAX_DECOMPOSITION`] times the input's length never
//! overflows and a shorter one is said to, every form is idempotent, the
//! forms agree with one another (UAX #15 §1.2), and a prepared string
//! prepares to itself.
//!
//! The text is drawn from the characters normalization works on — combining
//! marks of every class, Hangul jamo and syllables, characters that
//! decompose, right-to-left letters, spaces, controls, code points no release
//! assigns — so its paths are reached by construction. A plain `cargo test`
//! runs the [`SMOKE_ITERATIONS`] sweep once from a fresh, logged seed; `cargo
//! xtask fuzz` extends it to a wall-clock budget.

use tairix_fuzzseed::Prng;
use tairix_unicode::{
    normalize, saslprep, Form, Overflow, SaslprepError, Unassigned, MAX_DECOMPOSITION,
};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 20_000;

/// The kinds of character the text is drawn from, as inclusive ranges.
const KINDS: &[(u32, u32)] = &[
    (0x20, 0x7e),
    (0x00, 0x1f),
    (0xa0, 0xff),
    (0x300, 0x36f),
    (0x591, 0x5c7),
    (0x5d0, 0x5ea),
    (0x620, 0x65f),
    (0x900, 0x97f),
    (0xf71, 0xf84),
    (0x1100, 0x11ff),
    (0x1dc0, 0x1dff),
    (0x200b, 0x200f),
    (0x2160, 0x2188),
    (0x3099, 0x309a),
    (0xac00, 0xd7a3),
    (0xf900, 0xfaff),
    (0xfb00, 0xfb4f),
    (0xfdfa, 0xfdfa),
    (0xff00, 0xffef),
    (0x1d15e, 0x1d1c0),
    (0x1f100, 0x1f1ff),
    (0x2f800, 0x2fa1d),
    (0xe0000, 0xe007f),
    (0x0, 0x10_ffff),
];

const FORMS: [Form; 4] = [Form::Nfd, Form::Nfc, Form::Nfkd, Form::Nfkc];

fn character(rng: &mut Prng) -> char {
    let &(first, last) = rng.pick(KINDS);
    let span = usize::try_from(last - first).unwrap_or(0);
    let offset = u32::try_from(rng.at_most(span)).unwrap_or(0);
    char::from_u32(first + offset).unwrap_or(char::REPLACEMENT_CHARACTER)
}

/// `text` in `form`, written into a buffer as long as any normalization of
/// it needs.
fn normalized(text: &[char], form: Form) -> Vec<char> {
    let mut out = vec!['\0'; text.len() * MAX_DECOMPOSITION];
    let len = normalize(text.iter().copied(), form, &mut out)
        .unwrap_or_else(|_| panic!("{form:?} of {text:?} overflowed its bound"));
    out.truncate(len);
    out
}

fn prepared(text: &str) -> Result<String, SaslprepError> {
    let mut out = vec!['\0'; text.chars().count() * MAX_DECOMPOSITION];
    let len = saslprep(text, Unassigned::Allowed, &mut out)?;
    Ok(out[..len].iter().collect())
}

#[test]
fn normalization_and_saslprep_hold_for_any_text() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "normalization_and_saslprep_hold_for_any_text",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let len = rng.at_most(24);
        let text: Vec<char> = (0..len).map(|_| character(&mut rng)).collect();

        let [decomposed, composed, flattened, folded] = FORMS.map(|form| normalized(&text, form));
        for (form, once) in FORMS
            .into_iter()
            .zip([&decomposed, &composed, &flattened, &folded])
        {
            assert_eq!(&normalized(once, form), once, "{form:?} is idempotent");
        }
        assert_eq!(
            normalized(&composed, Form::Nfd),
            decomposed,
            "NFD(NFC) is NFD"
        );
        assert_eq!(
            normalized(&decomposed, Form::Nfc),
            composed,
            "NFC(NFD) is NFC"
        );
        assert_eq!(
            normalized(&flattened, Form::Nfkc),
            folded,
            "NFKC(NFKD) is NFKC"
        );
        assert_eq!(
            normalized(&composed, Form::Nfkd),
            flattened,
            "NFKD(NFC) is NFKD"
        );

        // A buffer too short is said to be, never written past.
        let form = *rng.pick(&FORMS);
        let longest = normalized(&text, form).len();
        if longest > 0 {
            let mut short = vec!['\0'; rng.below(longest)];
            assert_eq!(
                normalize(text.iter().copied(), form, &mut short),
                Err(Overflow)
            );
        }

        let typed: String = text.iter().collect();
        match prepared(&typed) {
            Ok(once) => assert_eq!(prepared(&once), Ok(once.clone()), "SASLprep is idempotent"),
            Err(error) => assert_ne!(error, SaslprepError::Overflow, "{typed:?} overflowed"),
        }

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
