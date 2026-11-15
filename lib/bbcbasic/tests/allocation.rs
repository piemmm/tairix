//! The tokenisers, listers and program images under the heap's refusals and
//! against its growth.
//!
//! Every request each makes is refused in turn: each refusal is answered as
//! `NoMemory`, never an abort, and nothing outlives the answer. A buffer
//! built a piece at a time grows by doubling, so its requests are
//! logarithmic in what it holds, not one a piece.

use tairix_bbcbasic::keyword::word;
use tairix_bbcbasic::list::{Lister, Options};
use tairix_bbcbasic::program::{self, Line, MOST_TEXT};
use tairix_bbcbasic::tokenise::{tokenise, Start};
use tairix_bbcbasic::{lineref, Dialect, Error};
use tairix_fuzzseed::meter::{metered, refused_at_each, Metered};

#[global_allocator]
static ALLOCATOR: Metered = Metered;

const DIALECTS: [Dialect; 2] = [Dialect::BasicII, Dialect::BasicV];

/// Every `LISTO` option but BASIC V's stop at a line reference.
const LISTO: u8 = 0x1F & !Options::NO_NUMBERS;

/// `text` listed as line 10 into a fresh buffer.
fn listed(dialect: Dialect, text: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    Lister::new(dialect, Options::new(LISTO)).line(10, text, &mut out)?;
    Ok(out)
}

#[test]
fn every_refusal_is_answered_as_no_memory() {
    let refused = |error: &Error| *error == Error::NoMemory;
    let typed = b"10FOR I=1 TO 10:PRINT \"A\";I:IF I THEN GOTO 20:NEXT:REM done";
    for dialect in DIALECTS {
        refused_at_each(|| tokenise(dialect, typed, Start::Line), refused);
        let tokens = tokenise(dialect, typed, Start::Line).expect("room").bytes;
        let text = &tokens[1 + lineref::ENCODED..];
        refused_at_each(|| listed(dialect, text), refused);
        refused_at_each(
            || program::stored(dialect, b"  ELSE  ", Options::new(1)),
            refused,
        );
    }
    let lines: Vec<Line<'_>> = (1..=40)
        .map(|number| Line {
            number,
            text: b"PRINT",
        })
        .collect();
    refused_at_each(|| program::image(lines.iter().copied()), refused);
}

#[test]
fn a_buffer_built_a_piece_at_a_time_grows_by_doubling() {
    let text = [word::PRINT.token];
    let lines: Vec<Line<'_>> = (1..=4000)
        .map(|number| Line {
            number,
            text: &text,
        })
        .collect();
    let (image, metering) = metered(|| program::image(lines.iter().copied()));
    assert_eq!(image.map(|image| image.len()), Ok(4000 * 5 + 2));
    assert!(
        metering.allocations <= 32,
        "{} requests for 4000 lines",
        metering.allocations
    );

    let tokens = [word::PRINT.token; MOST_TEXT];
    for dialect in DIALECTS {
        let (out, metering) = metered(|| listed(dialect, &tokens));
        assert!(out.is_ok_and(|out| out.len() > 5 * MOST_TEXT));
        assert!(
            metering.allocations <= 16,
            "{dialect:?}: {} requests",
            metering.allocations
        );
    }
}
