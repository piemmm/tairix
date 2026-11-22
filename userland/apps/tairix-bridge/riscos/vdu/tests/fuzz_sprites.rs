//! Deterministic fuzz harness for sprites: every `OS_SpriteOp` reason with
//! each naming over areas whose blocks and sprites are salted, scale factors,
//! matrices, parallelograms and tables pointing anywhere, output switched to
//! a sprite and drawn on there, the JPEG SWIs over JPEGs with bytes changed,
//! `PLOT` &E8–&EF, `VDU 23,27` and the SpriteUtils commands.
//!
//! The invariants, checked after every call:
//!
//! * nothing panics, and every refusal is a value;
//! * every error block a call returns can be read back;
//! * the drivers still answer a mode variable afterwards.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

mod common;

use common::{answers, drivers};
use tairix_fuzzseed::Prng;
use tairix_image::{encode_jpeg, JpegOptions, Picture};
use tairix_riscos_filing::session::Session;
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::os::{Fail, Regs};
use tairix_riscos_kernel::swi::os::{CHANGE_DYNAMIC_AREA, CLI, SPRITE_OP, WRITE_C};
use tairix_riscos_kernel::swi::X;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_RUNS: u64 = 2;
/// Calls made in one run.
const CALLS: usize = 120;
/// The user area, its size, and the blocks hostile calls point at after it.
const AREA: u32 = APPLICATION;
const AREA_SIZE: u32 = 0x8000;
const BLOCKS: u32 = APPLICATION + AREA_SIZE;
const BLOCKS_SIZE: u32 = 0x1000;
const JPEG_AT: u32 = BLOCKS + BLOCKS_SIZE;
/// R0's area value for a user area by name.
const USER_AREA: u32 = 0x100;
/// The reasons that make an area's sprites.
const INITIALISE: u32 = 0x09;
const CREATE: u32 = 0x0F;
const CREATE_MASK: u32 = 0x1D;
/// SpriteExtend's JPEG SWI chunk.
const JPEG_CHUNK: u32 = 0x4_9980;
/// The sprite names salted in the blocks, each twelve bytes apart.
const NAMES: [&[u8]; 4] = [b"a\0", b"bee\0", b"c0123456789a\0", b"none\0"];

/// Sprites made in a fresh area: a name, a width and height, a mode.
const MADE: [(&[u8], u32, u32, u32); 3] = [
    (b"a\0", 8, 8, 28),
    (b"bee\0", 5, 3, 12),
    (b"c0123456789a\0", 4, 4, 0x3016_80B5),
];

fn op(session: &mut Session, regs: &mut Regs) -> Result<(), Fail> {
    session.kernel.os().perform(X | SPRITE_OP, regs)
}

/// A session with a user area of three sprites, two masked, and the system
/// area grown and given the same; the blocks after the area salted with
/// names, factors and words; a JPEG with bytes changed after them.
fn boot(rng: &mut Prng) -> Session {
    let mut session = drivers(b"yyyyyyyy");
    let os = &mut session.kernel.os();
    let _ = os.write_word(AREA, AREA_SIZE);
    let _ = os.write_word(AREA + 8, 16);
    let mut blocks = vec![0u8; BLOCKS_SIZE as usize];
    rng.fill(&mut blocks);
    for (i, name) in NAMES.iter().enumerate() {
        blocks[i * 16..i * 16 + name.len()].copy_from_slice(name);
    }
    for (i, word) in [1u32, 2, 3, 1, 0x1_0000, 0, 0, 0x1_0000, 0x10_0000, 0x8_0000]
        .iter()
        .enumerate()
    {
        blocks[64 + i * 4..68 + i * 4].copy_from_slice(&word.to_le_bytes());
    }
    let _ = os.write_bytes(BLOCKS, &blocks);
    let mut grow = Regs::new([3, 0x4000, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let _ = os.perform(X | CHANGE_DYNAMIC_AREA, &mut grow);
    for naming in [USER_AREA, 0] {
        let mut init = Regs::new([naming | INITIALISE, AREA, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let _ = op(&mut session, &mut init);
        for (k, (name, width, height, mode)) in MADE.iter().enumerate() {
            let at = BLOCKS + 0xF00 + 16 * u32::try_from(k).unwrap_or(0);
            let _ = session.kernel.os().write_bytes(at, name);
            let mut create = Regs::new([
                naming | CREATE,
                AREA,
                at,
                u32::from(k == 1),
                *width,
                *height,
                *mode,
                0,
                0,
                0,
                0,
                0,
                0,
            ]);
            let _ = op(&mut session, &mut create);
            if k != 2 {
                let mut mask =
                    Regs::new([naming | CREATE_MASK, AREA, at, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                let _ = op(&mut session, &mut mask);
            }
        }
    }
    let mut salt = vec![0u8; 64];
    rng.fill(&mut salt);
    if rng.below(3) == 0 {
        let offset = 16 + u32::try_from(rng.below(0x200)).unwrap_or(0);
        let _ = session
            .kernel
            .os()
            .write_bytes(AREA + offset, &salt[..=rng.below(8)]);
    }
    let mut pixels = vec![0u8; 8 * 8 * 4];
    rng.fill(&mut pixels);
    let jpeg = Picture::rgba(8, 8, pixels)
        .ok()
        .and_then(|picture| encode_jpeg(&picture, JpegOptions::new(75, [0, 0, 0]).ok()?).ok())
        .unwrap_or_default();
    let mut jpeg = jpeg;
    for _ in 0..rng.below(4) {
        if jpeg.is_empty() {
            break;
        }
        let at = rng.below(jpeg.len());
        jpeg[at] = u8::try_from(rng.below(256)).unwrap_or(0);
    }
    let _ = session.kernel.os().write_bytes(JPEG_AT, &jpeg);
    session
}

/// A hostile register value: small numbers, coordinates at the range's
/// ends, pointers into the area, the salted blocks, or nowhere.
fn hostile(rng: &mut Prng) -> u32 {
    let into = BLOCKS + u32::try_from(rng.below(BLOCKS_SIZE as usize)).unwrap_or(0);
    let area = AREA + u32::try_from(rng.below(0x400)).unwrap_or(0);
    let edge = *rng.pick(&[-32768i32, -1, 0, 1, 639, 1279, 32767]);
    let small = u32::try_from(rng.below(300)).unwrap_or(0);
    let anything = rng.next_u32();
    *rng.pick(&[
        0,
        1,
        small,
        u32::MAX,
        0x8000_0000,
        into,
        area,
        area | 2,
        edge.cast_unsigned(),
        BLOCKS + 64,
        anything,
    ])
}

/// One hostile call, its outcome a value.
fn call(session: &mut Session, rng: &mut Prng) {
    let mut regs = Regs::new([0; 13]);
    for register in &mut regs.r {
        *register = hostile(rng);
    }
    let name = BLOCKS + 16 * u32::try_from(rng.below(4)).unwrap_or(0);
    let result = match rng.below(6) {
        0..=2 => {
            let naming = *rng.pick(&[0, 0x100, 0x200]);
            regs.r[0] = naming | u32::try_from(rng.below(71)).unwrap_or(0);
            regs.r[1] = AREA;
            regs.r[2] = if naming == 0x200 { AREA + 16 } else { name };
            op(session, &mut regs)
        }
        3 => {
            regs.r[0] = if rng.below(2) == 0 {
                JPEG_AT
            } else {
                regs.r[0]
            };
            regs.r[1] = if rng.below(2) == 0 {
                JPEG_AT
            } else {
                regs.r[1]
            };
            let offset = u32::try_from(rng.below(8)).unwrap_or(0);
            session
                .kernel
                .os()
                .perform(X | JPEG_CHUNK | offset, &mut regs)
        }
        4 => {
            let bytes: Vec<u8> = match rng.below(3) {
                0 => vec![
                    25,
                    0xE8 + u8::try_from(rng.below(8)).unwrap_or(0),
                    0x40,
                    0,
                    0x40,
                    0,
                ],
                1 => vec![
                    23,
                    27,
                    u8::try_from(rng.below(3)).unwrap_or(0),
                    u8::try_from(rng.below(256)).unwrap_or(0),
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                ],
                _ => vec![25, 4, 0, 1, 0, 1, 25, 0x65, 0x40, 0, 0x30, 0],
            };
            let mut outcome = Ok(());
            for byte in bytes {
                let mut write = Regs::new([u32::from(byte), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                outcome = session.kernel.os().perform(X | WRITE_C, &mut write);
                if outcome.is_err() {
                    break;
                }
            }
            outcome
        }
        _ => {
            let commands: [&[u8]; 8] = [
                b"SList\0",
                b"SInfo\0",
                b"SChoose a\0",
                b"SCopy a z\0",
                b"SRename bee y\0",
                b"SDelete z nope\0",
                b"SFlipY a\0",
                b"SGet q\0",
            ];
            let command = *rng.pick(&commands);
            let _ = session.kernel.os().write_bytes(BLOCKS + 0xE00, command);
            let mut cli = Regs::new([BLOCKS + 0xE00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            session.kernel.os().perform(X | CLI, &mut cli)
        }
    };
    if let Err(Fail::Error(block)) = result {
        let _ = session.kernel.os().read_word(block);
    }
}

#[test]
fn hostile_sprite_calls_leave_the_drivers_answering() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_sprite_calls_leave_the_drivers_answering",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        let mut session = boot(&mut rng);
        for step in 0..CALLS {
            call(&mut session, &mut rng);
            assert!(
                answers(&mut session),
                "OS_ReadModeVariable after step {step}"
            );
        }
        runs += 1;
    }
}
