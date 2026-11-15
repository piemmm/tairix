//! Deterministic fuzz harness for BBC BASIC's tokenisers, listers and program
//! images, in both dialects.
//!
//! A program is a stranger's file and a typed line a stranger's text. The
//! invariants:
//!
//! * tokenising any bytes from any start never fails but for memory, stops
//!   at the line's end, and gives at most four bytes for each it reads, a
//!   digit's line reference being the most a byte becomes;
//! * listing any bytes under any `LISTO` never fails but for memory or the
//!   reference BASIC V stops at, and gives at most a fixed amount for each
//!   byte, however deep the structures before it: no indentation grows
//!   without bound;
//! * reading any image never panics, ends at the first line that does not
//!   chain, and an image of the lines read back is the image they came from;
//! * a stored line keeps within a line's text.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

use tairix_bbcbasic::basic5::STATEMENT;
use tairix_bbcbasic::keyword::word;
use tairix_bbcbasic::list::{Lister, Options};
use tairix_bbcbasic::program::{self, Line, MOST_TEXT};
use tairix_bbcbasic::tokenise::{tokenise, Start};
use tairix_bbcbasic::{basic2, basic5, lineref, Dialect, Error};
use tairix_fuzzseed::Prng;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 400;
/// The longest text drawn.
const MOST_DRAWN: usize = 300;
/// The most a listed line takes before its text: its number, a space and
/// the most an indent shows.
const LISTED_BEFORE: usize = 5 + 1 + 255 + 1;
/// The most a byte of text lists as: a split statement's colon, new line,
/// number's room, space and indent.
const LISTED_EACH: usize = 1 + 1 + 5 + 1 + 255;

const DIALECTS: [Dialect; 2] = [Dialect::BasicII, Dialect::BasicV];
const STARTS: [Start; 3] = [Start::Line, Start::Statement, Start::Expression];

/// Text a tokeniser meets: keywords whole and abbreviated, digits, quotes,
/// colons and anything else, a CR or LF now and then.
fn drawn_text(rng: &mut Prng) -> Vec<u8> {
    let mut text = Vec::new();
    let length = rng.at_most(MOST_DRAWN);
    while text.len() < length {
        match rng.below(8) {
            0 | 1 => {
                let name = rng.pick(&basic5::KEYWORDS).word.name.as_bytes();
                text.extend_from_slice(name);
            }
            2 => {
                let name = rng.pick(&basic2::KEYWORDS).word.name.as_bytes();
                text.extend_from_slice(&name[..=rng.below(name.len())]);
                text.push(b'.');
            }
            3 => text.extend((0..=rng.below(6)).map(|_| b'0' + (rng.next_u8() % 10))),
            4 => text.push(*rng.pick(b"\":, &*.()")),
            5 => text.push(*rng.pick(b"\r\n")),
            _ => text.push(rng.next_u8()),
        }
    }
    text
}

/// A line's tokenised bytes, drawn to reach a lister's corners: tokens of
/// both kinds, references, structures opened and closed in quantity,
/// quotes, colons and `REM`.
fn drawn_tokens(rng: &mut Prng) -> Vec<u8> {
    let mut text = Vec::new();
    let length = rng.at_most(MOST_DRAWN);
    while text.len() < length {
        match rng.below(8) {
            0 => {
                text.push(lineref::MARKER);
                text.extend(lineref::encode(rng.next_u16()));
            }
            1 => text.extend(
                [
                    word::FOR.token,
                    word::REPEAT.token,
                    word::NEXT.token,
                    word::UNTIL.token,
                ]
                .repeat(1 + rng.below(4))
                .iter()
                .take(1 + rng.below(16)),
            ),
            2 => {
                let any = rng.next_u8();
                text.extend([STATEMENT, *rng.pick(&[0x95, 0x8E, any])]);
            }
            3 => text.push(*rng.pick(&basic5::PREFIXES)),
            4 => text.push(*rng.pick(&[b'"', b':', word::REM.token, word::THEN.token])),
            _ => text.push(rng.next_u8()),
        }
    }
    text
}

/// The bytes `text` is tokenised from: up to the line's end in `dialect`.
fn line_end(dialect: Dialect, text: &[u8]) -> usize {
    text.iter()
        .position(|&b| b == b'\r' || (dialect == Dialect::BasicV && b == b'\n'))
        .unwrap_or(text.len())
}

fn check_tokenise(text: &[u8]) {
    for dialect in DIALECTS {
        for start in STARTS {
            let tokenised = tokenise(dialect, text, start).expect("memory");
            let read = line_end(dialect, text);
            assert!(
                tokenised.bytes.len() <= 4 * read,
                "{dialect:?} {start:?}: {text:02X?}"
            );
            assert!(!tokenised.bytes.contains(&b'\r'), "a CR ends the line");
            if dialect == Dialect::BasicV {
                assert!(!tokenised.bytes.contains(&b'\n'), "as BASIC V's LF does");
            }
        }
    }
}

fn check_list(rng: &mut Prng, lines: &[Vec<u8>]) {
    for dialect in DIALECTS {
        let listo = u8::try_from(rng.below(32)).expect("a LISTO");
        let mut lister = Lister::new(dialect, Options::new(listo));
        for text in lines {
            let mut out = Vec::new();
            match lister.line(rng.next_u16(), text, &mut out) {
                Ok(()) => {}
                Err(Error::ReferenceListed) => {
                    assert_eq!(dialect, Dialect::BasicV);
                    assert!(text.contains(&lineref::MARKER));
                }
                Err(error) => panic!("{dialect:?} listed {text:02X?}: {error:?}"),
            }
            let bound = LISTED_BEFORE + LISTED_EACH * text.len();
            assert!(
                out.len() <= bound,
                "{dialect:?} LISTO {listo}: {} bytes",
                out.len()
            );
            assert_eq!(out.last(), Some(&b'\n'), "each listed line ends");
        }
    }
}

/// An image of drawn lines, then damaged as a stranger's file might be.
fn drawn_image(rng: &mut Prng) -> Vec<u8> {
    let mut number = 0u16;
    let mut lines = Vec::new();
    for _ in 0..rng.below(12) {
        number = number.saturating_add(1 + rng.next_u16() % 2000);
        if number >= 0xFF00 {
            break;
        }
        let length = rng.at_most(MOST_TEXT);
        lines.push((number, rng.noise(length)));
    }
    let image = program::image(lines.iter().map(|(number, text)| Line {
        number: *number,
        text,
    }))
    .expect("drawn in order and within a line");
    if rng.below(2) == 0 {
        rng.mutated(&image)
    } else {
        image
    }
}

fn check_image(image: &[u8]) {
    let read: Vec<Result<Line<'_>, Error>> = program::lines(image).collect();
    assert!(
        read.len() <= image.len() / 4 + 1,
        "a line takes four bytes at least"
    );
    let (last, kept) = match read.split_last() {
        Some((Err(error), kept)) => (Some(*error), kept),
        _ => (None, read.as_slice()),
    };
    assert!(matches!(last, None | Some(Error::BadProgram)));
    let kept: Vec<Line<'_>> = kept
        .iter()
        .map(|line| *line.as_ref().expect("before the end"))
        .collect();
    assert!(kept.iter().all(|line| line.text.len() <= MOST_TEXT));
    if last.is_none() {
        if let Ok(again) = program::image(kept.iter().copied()) {
            assert_eq!(
                again,
                image[..again.len()],
                "an image of its lines is itself"
            );
        }
    }
}

fn check_stored(text: &[u8]) {
    for dialect in DIALECTS {
        for listo in [0, 1] {
            match program::stored(dialect, text, Options::new(listo)) {
                Ok(stored) => assert!(stored.len() <= MOST_TEXT && stored.len() <= text.len()),
                Err(error) => assert_eq!(error, Error::LineTooLong),
            }
        }
    }
}

#[test]
fn tokenising_listing_and_reading_any_bytes_stays_bounded() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "tokenising_listing_and_reading_any_bytes_stays_bounded",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let text = drawn_text(&mut rng);
        check_tokenise(&text);
        check_tokenise(&rng.mutated(&text));
        check_stored(&text);

        let lines: Vec<Vec<u8>> = (0..=rng.below(8)).map(|_| drawn_tokens(&mut rng)).collect();
        check_list(&mut rng, &lines);
        let typed: Vec<Vec<u8>> = (0..=rng.below(4))
            .map(|_| {
                let dialect = *rng.pick(&DIALECTS);
                tokenise(dialect, &drawn_text(&mut rng), Start::Statement)
                    .expect("memory")
                    .bytes
            })
            .collect();
        check_list(&mut rng, &typed);

        check_image(&drawn_image(&mut rng));
        let length = rng.at_most(512);
        check_image(&rng.noise(length));

        iteration += 1;
        match deadline {
            None if iteration >= SMOKE_ITERATIONS => break,
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            _ => {}
        }
    }
}
