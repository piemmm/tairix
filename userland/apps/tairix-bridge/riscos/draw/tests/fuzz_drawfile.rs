//! Deterministic fuzz harness for DrawFile: Draw files built of every
//! object type with hostile fields — sizes past what holds them or too
//! small, font tables twice or naming nothing, text in fonts no table names,
//! sprites of any mode, groups and tagged objects nesting past where any
//! walk's stack follows them, text areas
//! with any commands — or a sound file with bytes changed, rendered,
//! measured and their fonts declared under hostile flags, matrices, clips
//! and lengths, and `*Render` with hostile arguments.
//!
//! The invariants, checked after every call:
//!
//! * nothing panics, and every refusal is a value, a call the host stops
//!   after a few of its words among them;
//! * every error block a call returns can be read back;
//! * the drivers still answer a mode variable afterwards;
//! * the modules a render reaches — DrawFile, Draw and the colour modules,
//!   whose caches its sprites fill — re-initialised after a run, hold none
//!   of the RMA they took.

#[path = "../../vdu/tests/common/mod.rs"]
mod common;

use std::sync::LazyLock;

use common::{answers, command, drivers_with, rma_used, select_depth, HOSTILE_CALL_WORDS};
use tairix_drawing::riscos::format::{
    CLOSE, CLOSE_WITH_GAP, FONT_TABLE, GROUP, JPEG, LINE, MOVE, OPTIONS, PATH, SPRITE, TAGGED,
    TEXT, TEXT_AREA, TEXT_COLUMN, TRANSFORMED_SPRITE, TRANSFORMED_TEXT,
};
use tairix_fuzzseed::Prng;
use tairix_riscos_filing::session::{modules_with, Session};
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::{Fail, Regs};
use tairix_riscos_kernel::swi::chunk::DRAW_FILE;
use tairix_riscos_kernel::swi::X;

const SMOKE_RUNS: u64 = 4;
const CALLS: usize = 120;
/// Where the file is built, and where the call's blocks are.
const FILE_AT: u32 = APPLICATION;
const BLOCKS_AT: u32 = APPLICATION + 0x1_0000;

static MODULES: LazyLock<Vec<&'static Builtin>> =
    LazyLock::new(|| modules_with(&[&tairix_riscos_vdu::MODULES, &tairix_riscos_draw::MODULES]));

fn words(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// An object of `kind` around `body`, its size most often right.
fn object(rng: &mut Prng, kind: u32, body: &[u8]) -> Vec<u8> {
    let right = u32::try_from(24 + body.len()).unwrap_or(u32::MAX);
    let size = match rng.below(12) {
        0 => rng.next_u32(),
        1 => right.wrapping_sub(4),
        2 => *rng.pick(&[0, 4, 8, 12]),
        _ => right,
    };
    let box_ = [
        rng.next_u32() % 90_000,
        rng.next_u32() % 70_000,
        rng.next_u32() % 90_000,
        rng.next_u32() % 70_000,
    ];
    let mut bytes = words(&[kind, size]);
    bytes.extend(words(&box_));
    bytes.extend(body);
    bytes
}

fn padded(mut bytes: Vec<u8>) -> Vec<u8> {
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
    bytes
}

/// A coordinate on the paper.
fn point(rng: &mut Prng) -> u32 {
    rng.next_u32() % 90_000
}

/// A path of a triangle, its last element a close or any.
fn path_object(rng: &mut Prng) -> Vec<u8> {
    let mut body = words(&[
        rng.next_u32(),
        *rng.pick(&[u32::MAX, 0xFF00]),
        rng.next_u32() % 4000,
        rng.next_u32(),
    ]);
    let (x0, y0, x1, y1) = (point(rng), point(rng), point(rng), point(rng));
    let last = *rng.pick(&[CLOSE, CLOSE_WITH_GAP, LINE, 9]);
    body.extend(words(&[MOVE, x0, y0, LINE, x1, y0, LINE, x1, y1, last, 0]));
    object(rng, PATH, &body)
}

/// A font table naming up to three fonts, a number now and then repeated
/// or past the range.
fn font_table(rng: &mut Prng) -> Vec<u8> {
    let mut table = words(&[FONT_TABLE, 0]);
    for n in 1..=u8::try_from(rng.below(4)).unwrap_or(0) {
        table.push(*rng.pick(&[n, 0xFF, 7]));
        table.extend(b"Homerton.Medium\0");
    }
    let mut table = padded(table);
    let size = u32::try_from(table.len()).unwrap_or(0);
    table[4..8].copy_from_slice(&size.to_le_bytes());
    table
}

/// Text in a font a table may not name, transformed or not.
fn text_object(rng: &mut Prng) -> Vec<u8> {
    let font = *rng.pick(&[0, 1, 2, 7, 0xFF]);
    let mut body = words(&[
        rng.next_u32(),
        0xFFFF_FF00,
        font,
        7680,
        7680,
        point(rng),
        point(rng),
    ]);
    body.extend(b"Hello\0");
    let kind = if rng.below(2) == 0 {
        TEXT
    } else {
        TRANSFORMED_TEXT
    };
    if kind == TRANSFORMED_TEXT {
        let mut transformed = words(&[0x1_0000, 0, 0, 0x1_0000, point(rng), point(rng), 0]);
        transformed.extend(body);
        body = transformed;
    }
    object(rng, kind, &padded(body))
}

/// A sprite of any mode, transformed or not.
fn sprite_object(rng: &mut Prng) -> Vec<u8> {
    let anything = rng.next_u32();
    let mode = *rng.pick(&[28, 27, 15, 99, anything]);
    let mut sprite = words(&[60, 0x7370, 0, 0, 0, 3, 0, 31, 44, 44, mode]);
    sprite.extend(words(&[0x0403_0201; 4]));
    let kind = *rng.pick(&[SPRITE, TRANSFORMED_SPRITE]);
    object(rng, kind, &sprite)
}

/// A text area of one column whose text holds any of its commands.
fn text_area_object(rng: &mut Prng) -> Vec<u8> {
    let text: &[u8] = rng.pick(&[
        &b"\\! 1\nHello"[..],
        b"Hello",
        b"\\! 2\nx",
        b"\\! 1\n\\9x",
        b"\\! 1\n\\Qx",
        b"\\F1 Trinity 12\\1x",
    ]);
    let mut body = object(rng, TEXT_COLUMN, &[]);
    body.extend(words(&[0, 0, 0, 0, 0xFFFF_FF00]));
    body.extend(text);
    body.push(0);
    object(rng, TEXT_AREA, &padded(body))
}

/// An object of a kind chosen at random, nesting no deeper than `depth`.
fn any_object(rng: &mut Prng, depth: u32) -> Vec<u8> {
    match rng.below(if depth == 0 { 7 } else { 9 }) {
        0 => path_object(rng),
        1 => font_table(rng),
        2 | 3 => text_object(rng),
        4 => sprite_object(rng),
        5 => text_area_object(rng),
        6 => {
            let kind = *rng.pick(&[JPEG, OPTIONS, 99, 0x1234_5678]);
            object(
                rng,
                kind,
                &words(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]),
            )
        }
        7 => {
            let mut body = b"group name  ".to_vec();
            for _ in 0..rng.below(4) {
                body.extend(any_object(rng, depth - 1));
            }
            object(rng, GROUP, &body)
        }
        _ => {
            let mut body = words(&[rng.next_u32()]);
            body.extend(any_object(rng, depth - 1));
            body.extend(b"tail");
            object(rng, TAGGED, &body)
        }
    }
}

/// Groups and tagged objects mixed at random, nesting past where any walk's
/// stack follows them, around an object of any kind.
fn deep_chain(rng: &mut Prng) -> Vec<u8> {
    let mut inner = any_object(rng, 0);
    for _ in 0..120 + rng.below(140) {
        let (kind, mut body) = if rng.below(2) == 0 {
            (GROUP, b"deep        ".to_vec())
        } else {
            (TAGGED, words(&[rng.next_u32()]))
        };
        body.extend(&inner);
        let size = u32::try_from(24 + body.len()).unwrap_or(u32::MAX);
        inner = words(&[kind, size, 0, 0, 90_000, 70_000]);
        inner.extend(body);
    }
    inner
}

/// A Draw file of hostile objects, or now and then not one at all.
fn file(rng: &mut Prng) -> Vec<u8> {
    let mut bytes = b"Draw".to_vec();
    bytes.extend(words(&[*rng.pick(&[201, 200, 202]), rng.next_u32() % 3]));
    bytes.extend(b"Fuzz        ");
    bytes.extend(words(&[rng.next_u32() % 90_000, 0, 90_000, 70_000]));
    for _ in 0..=rng.below(8) {
        bytes.extend(any_object(rng, 3));
    }
    if rng.below(16) == 0 {
        bytes.extend(deep_chain(rng));
    }
    for _ in 0..rng.below(3) {
        let at = rng.below(bytes.len());
        bytes[at] = u8::try_from(rng.next_u32() & 0xFF).unwrap_or(0);
    }
    if rng.below(16) == 0 {
        bytes.truncate(rng.below(bytes.len()));
    }
    bytes
}

fn call(session: &mut Session, rng: &mut Prng) {
    session.allow_words(Some(HOSTILE_CALL_WORDS));
    let bytes = file(rng);
    let _ = session.kernel.os().write_bytes(FILE_AT, &bytes);
    let blocks: Vec<u32> = (0..16)
        .map(|_| *rng.pick(&[0, 0x1_0000, 0x8000, u32::MAX, 100, 300]))
        .collect();
    let _ = session.kernel.os().write_bytes(BLOCKS_AT, &words(&blocks));
    let length = u32::try_from(bytes.len()).unwrap_or(0);
    let mut regs = Regs::new([0; 13]);
    let anything = rng.next_u32();
    regs.r[0] = *rng.pick(&[0, 1, 2, 3, 4, 7, anything]);
    regs.r[1] = FILE_AT;
    regs.r[2] = *rng.pick(&[
        length,
        length,
        length.saturating_sub(3),
        length + 40,
        3,
        u32::MAX,
    ]);
    regs.r[3] = *rng.pick(&[0, BLOCKS_AT, BLOCKS_AT + 4]);
    regs.r[4] = *rng.pick(&[0, BLOCKS_AT + 8, BLOCKS_AT + 16]);
    regs.r[5] = *rng.pick(&[0, 128, 1, u32::MAX]);
    let swi = DRAW_FILE + u32::try_from(rng.below(4)).unwrap_or(0);
    let result = if rng.below(10) == 0 {
        let _ = session.kernel.os().write_bytes(FILE_AT, &bytes);
        command(
            session,
            rng.pick(&[
                "Render",
                "Render -file x",
                "Render x 1 2 3",
                "Render x -flatness",
                "Render x 1 0 0 1 2 3 4",
            ]),
        )
    } else {
        session.kernel.os().perform(X | swi, &mut regs)
    };
    if let Err(Fail::Error(block)) = result {
        assert!(
            session.kernel.os().read_word(block).is_ok(),
            "the error block at {block:#x} can be read"
        );
    }
}

/// A sound file of a filled path rendered and measured: the calls reach
/// the module.
fn reaches_drawfile(session: &mut Session) {
    let mut bytes = b"Draw".to_vec();
    bytes.extend(words(&[201, 0]));
    bytes.extend(b"Fuzz        ");
    bytes.extend(words(&[0, 0, 9000, 9000]));
    bytes.extend(words(&[
        2,
        24 + 16 + 44,
        0,
        0,
        9000,
        9000,
        0xFF00,
        u32::MAX,
        0,
        0,
    ]));
    bytes.extend(words(&[2, 0, 0, 8, 9000, 0, 8, 9000, 9000, 5, 0]));
    session
        .kernel
        .os()
        .write_bytes(FILE_AT, &bytes)
        .expect("written");
    let length = u32::try_from(bytes.len()).unwrap_or(0);
    let mut render = Regs::new([0, FILE_AT, length, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    session
        .kernel
        .os()
        .perform(X | DRAW_FILE, &mut render)
        .expect("rendered");
    let mut bbox = Regs::new([0, FILE_AT, length, 0, BLOCKS_AT, 0, 0, 0, 0, 0, 0, 0, 0]);
    session
        .kernel
        .os()
        .perform(X | (DRAW_FILE + 1), &mut bbox)
        .expect("measured");
    assert_eq!(session.kernel.os().read_word(BLOCKS_AT + 8), Ok(9000));
}

#[test]
fn hostile_draw_files_leave_the_drivers_answering() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_draw_files_leave_the_drivers_answering",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        let mut session = drivers_with(MODULES.as_slice(), b"");
        let held = rma_used(&mut session);
        reaches_drawfile(&mut session);
        for step in 0..CALLS {
            call(&mut session, &mut rng);
            assert!(
                answers(&mut session),
                "OS_ReadModeVariable after step {step}"
            );
        }
        session.allow_words(None);
        for module in [
            "DrawFile",
            "Draw",
            "ColourTrans",
            "InverseTable",
            "BlendTable",
        ] {
            command(&mut session, &format!("RMReInit {module}")).expect("reinitialised");
        }
        assert_eq!(rma_used(&mut session), held, "all they took given back");
        runs += 1;
    }
}

/// A sprite of no palette plotted into a screen of one leaves its 32K table
/// with ColourTrans, which keeps it for later calls: why a run's end
/// re-initialises the colour modules too.
#[test]
fn a_sprite_rendered_leaves_its_32k_table_with_colourtrans() {
    const TRUE_COLOUR: u32 = (6 << 27) | (90 << 14) | (90 << 1) | 1;
    let mut session = drivers_with(MODULES.as_slice(), b"");
    select_depth(&mut session, 3).expect("256 colours");
    let held = rma_used(&mut session);
    let mut bytes = b"Draw".to_vec();
    bytes.extend(words(&[201, 0]));
    bytes.extend(b"Sprite      ");
    bytes.extend(words(&[0, 0, 9000, 9000, 5, 24 + 60, 0, 0, 9000, 9000]));
    bytes.extend(words(&[60, 0x7370, 0, 0, 0, 3, 0, 31, 44, 44, TRUE_COLOUR]));
    bytes.extend(words(&[0x00FF_8040; 4]));
    session
        .kernel
        .os()
        .write_bytes(FILE_AT, &bytes)
        .expect("written");
    let length = u32::try_from(bytes.len()).unwrap_or(0);
    let mut render = Regs::new([0, FILE_AT, length, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    session
        .kernel
        .os()
        .perform(X | DRAW_FILE, &mut render)
        .expect("rendered");
    for module in ["DrawFile", "Draw"] {
        command(&mut session, &format!("RMReInit {module}")).expect("reinitialised");
    }
    assert!(rma_used(&mut session) > held, "the table is ColourTrans's");
    command(&mut session, "RMReInit ColourTrans").expect("reinitialised");
    assert_eq!(rma_used(&mut session), held);
}

/// A file of a filled path inside `depth` levels of `kind`, a group or a
/// tagged object, each level's header written outermost first.
fn nested(depth: u32, kind: u32) -> Vec<u8> {
    let mut leaf = words(&[2, 24 + 16 + 44, 0, 0, 9000, 9000, 0xFF00, u32::MAX, 0, 0]);
    leaf.extend(words(&[2, 0, 0, 8, 9000, 0, 8, 9000, 9000, 5, 0]));
    let level: &[u8] = if kind == GROUP {
        b"Nested      "
    } else {
        &[0x34, 0x12, 0, 0]
    };
    let overhead = 24 + u32::try_from(level.len()).unwrap_or(0);
    let leaf_bytes = u32::try_from(leaf.len()).unwrap_or(0);
    let mut bytes = b"Draw".to_vec();
    bytes.extend(words(&[201, 0]));
    bytes.extend(b"Nesting     ");
    bytes.extend(words(&[0, 0, 9000, 9000]));
    for outside in 0..depth {
        bytes.extend(words(&[
            kind,
            overhead * (depth - outside) + leaf_bytes,
            0,
            0,
            9000,
            9000,
        ]));
        bytes.extend(level);
    }
    bytes.extend(leaf);
    bytes
}

/// Each call nests as deep as genuine RISC OS 5.30's DrawFile did from
/// BASIC, and one level deeper aborts as its supervisor stack's overflow
/// does; however deep a file nests, no walk follows it further.
#[test]
fn nesting_past_risc_os_s_stack_aborts_as_its_overflow_does() {
    let deepest = [
        (DRAW_FILE, GROUP, 131),
        (DRAW_FILE, TAGGED, 140),
        (DRAW_FILE + 1, GROUP, 162),
        (DRAW_FILE + 1, TAGGED, 176),
        (DRAW_FILE + 2, GROUP, 156),
        (DRAW_FILE + 2, TAGGED, 162),
    ];
    for (swi, kind, depth) in deepest {
        for (levels, aborts) in [(depth, false), (depth + 1, true), (20_000, true)] {
            let mut session = drivers_with(MODULES.as_slice(), b"");
            let bytes = nested(levels, kind);
            session
                .kernel
                .os()
                .write_bytes(FILE_AT, &bytes)
                .expect("written");
            let length = u32::try_from(bytes.len()).unwrap_or(0);
            let mut regs = Regs::new([0, FILE_AT, length, 0, BLOCKS_AT, 0, 0, 0, 0, 0, 0, 0, 0]);
            let done = session.kernel.os().perform(X | swi, &mut regs);
            assert_eq!(
                matches!(done, Err(Fail::Unwind(_))),
                aborts,
                "SWI {swi:#x}, kind {kind}, {levels} deep: {done:?}"
            );
            if !aborts {
                assert!(
                    done.is_ok(),
                    "SWI {swi:#x}, kind {kind}, {levels} deep: {done:?}"
                );
            }
        }
    }
}
