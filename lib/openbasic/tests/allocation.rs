//! The tokenisers, listers and program images under the heap's refusals and
//! against its growth.
//!
//! Every request each makes is refused in turn: each refusal is answered as
//! `NoMemory`, never an abort, and nothing outlives the answer. A buffer
//! built a piece at a time grows by doubling, so its requests are
//! logarithmic in what it holds, not one a piece.

use tairix_fuzzseed::meter::{metered, refused_at_each, refusing_after, Metered};
use tairix_openbasic::keyword::word;
use tairix_openbasic::list::{Lister, Options};
use tairix_openbasic::program::{self, Line, MOST_TEXT};
use tairix_openbasic::tokenise::{tokenise, Start};
use tairix_openbasic::{lineref, Dialect, Error};

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
        refused_at_each(
            || tokenise(dialect, typed, Start::Line, &mut Vec::new()),
            refused,
        );
        let mut tokens = Vec::new();
        tokenise(dialect, typed, Start::Line, &mut tokens).expect("room");
        let text = &tokens[1 + lineref::ENCODED..];
        refused_at_each(|| listed(dialect, text), refused);
    }
    // Only an `ELSE` the stored text changes is copied.
    let otherwise = [b' ', b' ', word::ELSE.token, b' ', b'X'];
    refused_at_each(
        || program::stored(Dialect::BasicV, &otherwise, Options::new(1)),
        refused,
    );
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

#[test]
fn a_refused_line_leaves_the_listing_and_its_buffer_as_they_were() {
    let typed = b"10FOR I=1 TO 10:REPEAT:PRINT \"loops\"";
    let next = [word::PRINT.token];
    for dialect in DIALECTS {
        let mut tokens = Vec::new();
        tokenise(dialect, typed, Start::Line, &mut tokens).expect("room");
        let text = &tokens[1 + lineref::ENCODED..];
        let both = |lister: &mut Lister, out: &mut Vec<u8>| {
            lister.line(10, text, out)?;
            lister.line(20, &next, out)
        };
        let mut whole = b"kept".to_vec();
        both(&mut Lister::new(dialect, Options::new(LISTO)), &mut whole).expect("room");
        let ((), taken) = metered(|| {
            let mut out = b"kept".to_vec();
            Lister::new(dialect, Options::new(LISTO))
                .line(10, text, &mut out)
                .expect("room");
        });
        for granted in 0..taken.allocations {
            let mut lister = Lister::new(dialect, Options::new(LISTO));
            let mut out = b"kept".to_vec();
            if refusing_after(granted, || lister.line(10, text, &mut out)).is_err() {
                assert_eq!(out, b"kept", "{dialect:?}: the buffer as it was");
                both(&mut lister, &mut out).expect("room");
                assert_eq!(out, whole, "{dialect:?}: the listing as it was");
            }
        }
    }
}

#[test]
fn a_line_refused_its_room_leaves_the_buffer_it_was_to_extend_as_it_was() {
    // Line numbers outgrow the room their text is first given.
    let mut typed = b"10ON X GOTO 1".to_vec();
    typed.extend(",1".repeat(60).bytes());
    let typed = &typed[..];
    for dialect in DIALECTS {
        let (answer, taken) = metered(|| {
            let mut out = Vec::new();
            tokenise(dialect, typed, Start::Line, &mut out).map(|_| out.len())
        });
        assert!(answer.is_ok() && taken.allocations > 1, "{taken:?}");
        for granted in 0..taken.allocations {
            let mut out = Vec::with_capacity(4);
            out.extend_from_slice(b"kept");
            let answer =
                refusing_after(granted, || tokenise(dialect, typed, Start::Line, &mut out));
            if answer.is_err() {
                assert_eq!(out, b"kept", "{dialect:?} at request {granted}");
            }
        }
    }
}
