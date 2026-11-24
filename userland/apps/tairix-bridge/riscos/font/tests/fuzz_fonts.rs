//! Deterministic fuzz harness for the Font Manager: guest fonts on
//! `Font$Path` — metrics, outlines, encodings and bitmap files — made whole
//! and then damaged as a guest may damage them, found under hostile
//! identifiers, measured and painted with hostile strings, colours, blocks
//! and matrices, in screens of every depth, through every SWI with hostile
//! registers. No word reaches the kernel's own memory: the Font Manager
//! writes where it is pointed, as RISC OS's does, and a write there would
//! break the machine rather than the module.
//!
//! The invariants, checked after every call:
//!
//! * nothing panics, and every refusal is a value, a call the host stops
//!   after a few of its words among them;
//! * every error block a call returns can be read back;
//! * the drivers still answer a mode variable afterwards;
//! * the module, re-initialised after a run, holds none of the RMA it took.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz` extends it to a wall-clock
//! budget.

#[path = "../../vdu/tests/common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::sync::LazyLock;

use common::{answers, rma_used, select_depth, KernelMemory, HOSTILE_CALL_WORDS};
use tairix_fscopy::fixture::Tree;
use tairix_fuzzseed::Prng;
use tairix_riscos_filing::session::{
    command, modules_with, session, Session, Setup, DISC, SLOT_BYTES,
};
use tairix_riscos_font::build::{
    bitmap_file, old_bitmaps, Character, Element, Font, Pixels, Point,
};
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::{Fail, Regs};
use tairix_riscos_kernel::swi::chunk::FONT;
use tairix_riscos_kernel::swi::{font, X};

const SMOKE_RUNS: u64 = 3;
const CALLS: usize = 160;
/// The application space salted with blocks hostile calls read.
const SALTED: u32 = 0x8000;
const BLOCK: u32 = 512;
/// The SWIs the Font Manager has.
const SWIS: usize = 42;

static MODULES: LazyLock<Vec<&'static Builtin>> = LazyLock::new(|| {
    modules_with(&[
        &tairix_riscos_vdu::MODULES,
        &tairix_riscos_draw::MODULES,
        &tairix_riscos_font::MODULES,
    ])
});

/// The fonts a run's library holds outlines for, named for what each is.
const FONTS: [&str; 4] = ["Boxes", "Curves", "Parts", "Strokes"];

/// A font of a few glyphs of each kind: boxes, curves, composites, strokes.
fn font(name: &str, rng: &mut Prng) -> Font {
    let mut characters = BTreeMap::new();
    for code in [32u8, b'A', b'B', b'C', b'a', b'b', 200] {
        let size = 50 + i32::try_from(rng.below(900)).unwrap_or(0);
        let fill = match name {
            "Curves" => vec![
                Element::Move(Point::new(0, 0)),
                Element::Curve(
                    Point::new(size, 0),
                    Point::new(size, size),
                    Point::new(0, size),
                ),
            ],
            "Strokes" => Vec::new(),
            _ => vec![
                Element::Move(Point::new(0, -size / 4)),
                Element::Line(Point::new(size, 0)),
                Element::Line(Point::new(size, size)),
                Element::Line(Point::new(0, size)),
            ],
        };
        let strokes = if name == "Strokes" {
            vec![
                Element::Move(Point::new(0, 0)),
                Element::Line(Point::new(size, size)),
                Element::Line(Point::new(size, 0)),
            ]
        } else {
            Vec::new()
        };
        let inclusions = if name == "Parts" && code == b'C' {
            vec![(b'A', Point::new(10, 0)), (b'B', Point::new(size, 20))]
        } else {
            Vec::new()
        };
        characters.insert(
            code,
            Character {
                advance: i16::try_from(size).unwrap_or(500),
                fill: if code == 32 { Vec::new() } else { fill },
                strokes,
                inclusions,
            },
        );
    }
    Font {
        name: name.as_bytes().to_vec(),
        design_size: *rng.pick(&[1000, 2048, 1, 40_000]),
        characters,
        kerns: vec![(b'A', b'B', -120), (b'B', b'A', 40)],
    }
}

/// `bytes` damaged as a guest may: bits flipped, cut short, words made
/// extreme, spliced with noise, or left whole.
fn damaged(rng: &mut Prng, mut bytes: Vec<u8>) -> Vec<u8> {
    match rng.below(6) {
        0 => bytes,
        1 => {
            for _ in 0..=rng.below(8) {
                if !bytes.is_empty() {
                    let at = rng.below(bytes.len());
                    bytes[at] ^= 1 << rng.below(8);
                }
            }
            bytes
        }
        2 => {
            let keep = rng.below(bytes.len() + 1);
            bytes.truncate(keep);
            bytes
        }
        3 => {
            for _ in 0..=rng.below(4) {
                if bytes.len() >= 4 {
                    let at = rng.below(bytes.len() - 3) & !3;
                    let word = *rng.pick(&[0u32, u32::MAX, 0x7FFF_FFFF, 0x8000_0000, 0xFFFF]);
                    bytes[at..at + 4].copy_from_slice(&word.to_le_bytes());
                }
            }
            bytes
        }
        4 => {
            let at = rng.below(bytes.len() + 1);
            let length = rng.below(64);
            let noise = rng.noise(length);
            bytes.splice(at..at, noise);
            bytes
        }
        _ => {
            let length = rng.below(600);
            rng.noise(length)
        }
    }
}

/// An encoding file: names for each code, some missing, some unknown.
fn encoding(rng: &mut Prng) -> Vec<u8> {
    let names = [
        "space", "A", "B", "C", "a", "b", ".notdef", "uni0041", "nonesuch", "",
    ];
    let mut out = b"%%RISCOS_Alphabet 101\n".to_vec();
    for _ in 0..rng.below(300) {
        out.push(b'/');
        out.extend_from_slice(rng.pick(&names).as_bytes());
        out.push(*rng.pick(b" \n\t"));
    }
    out
}

/// The disc a run serves: a library of the fonts, each file damaged or
/// not, an encoding of its own beside one, and an outlines file naming
/// another font.
fn disc(rng: &mut Prng) -> Tree {
    let library = format!("{DISC}/Lib");
    let mut tree = Tree::new().dir(&library);
    for name in FONTS {
        let made = font(name, rng);
        let (metrics, outlines) = (made.metrics(), made.outlines());
        tree = tree
            .file(
                &format!("{library}/{name}/IntMetric0,ff6"),
                &damaged(rng, metrics),
            )
            .file(
                &format!("{library}/{name}/Outlines0,ff6"),
                &damaged(rng, outlines),
            );
    }
    let whole = Font {
        design_size: 1000,
        ..font("Whole", rng)
    };
    tree = tree
        .file(&format!("{library}/Whole/IntMetric0,ff6"), &whole.metrics())
        .file(&format!("{library}/Whole/Outlines0,ff6"), &whole.outlines());
    let symbol = font("Symbol", rng);
    let old = old_bitmaps(12, (90, 45), &pixels(rng));
    let old = damaged(rng, old);
    let mut files = Vec::new();
    for (four, points) in [(true, 256), (false, 256), (true, 192), (true, 192)] {
        let made = bitmaps(rng, four, points);
        files.push(damaged(rng, made));
    }
    tree.file(
        &format!("{library}/Symbol/IntMetrics,ff6"),
        &damaged(rng, symbol.metrics()),
    )
    .file(
        &format!("{library}/Symbol/Outlines,ff6"),
        &damaged(rng, symbol.outlines()),
    )
    .file(&format!("{library}/Symbol/Encoding,fff"), &encoding(rng))
    .file(
        &format!("{library}/Named/IntMetric0,ff6"),
        &font("Named", rng).metrics(),
    )
    .file(
        &format!("{library}/Named/Outlines0,ff6"),
        b"Boxes\\M65536 0 16384 65536 0 0\r",
    )
    .file(
        &format!("{library}/Old/IntMetric0,ff6"),
        &font("Old", rng).metrics(),
    )
    .file(&format!("{library}/Old/x90y45,ff6"), &old)
    .file(
        &format!("{library}/Bitmapped/IntMetric0,ff6"),
        &font("Bitmapped", rng).metrics(),
    )
    .file(&format!("{library}/Bitmapped/f320x320,ff6"), &files[0])
    .file(&format!("{library}/Bitmapped/b320x320,ff6"), &files[1])
    .file(&format!("{library}/Bitmapped/f240x240,ff6"), &files[2])
    .file(&format!("{library}/Bitmapped/f240x120,ff6"), &files[3])
}

/// A glyph of random levels at `A`, a few pixels either way of the pen.
fn pixels(rng: &mut Prng) -> Pixels {
    let width = 1 + rng.below(12);
    let rows = 1 + rng.below(12);
    Pixels {
        code: b'A',
        corner: (
            i32::try_from(rng.below(8)).unwrap_or(0) - 4,
            i32::try_from(rng.below(8)).unwrap_or(0) - 4,
        ),
        width,
        levels: (0..width * rows).map(|_| rng.next_u8() % 16).collect(),
    }
}

/// A bitmap file of a few random glyphs at `points` and 90 dots an inch.
fn bitmaps(rng: &mut Prng, four: bool, points: i32) -> Vec<u8> {
    let glyphs: Vec<Pixels> = [b'A', b'B', b' ', 200]
        .into_iter()
        .map(|code| Pixels {
            code,
            ..pixels(rng)
        })
        .collect();
    bitmap_file(b"Bitmapped", four, points, 90, &glyphs).expect("small glyphs fit")
}

/// A font identifier: one of the library's, with hostile fields.
fn identifier(rng: &mut Prng) -> Vec<u8> {
    let mut name = rng
        .pick(&[
            "Boxes",
            "Curves",
            "Parts",
            "Strokes",
            "Symbol",
            "Named",
            "Old",
            "Bitmapped",
            "Nonesuch",
            "Homerton.Medium",
            "",
        ])
        .as_bytes()
        .to_vec();
    for _ in 0..rng.below(3) {
        name.extend_from_slice(rng.pick(&[
            &b"\\ELatin1"[..],
            b"\\EUTF8",
            b"\\ESymbol",
            b"\\E",
            b"\\M65536 0 0 65536 0 0",
            b"\\M0 65536 -65536 0 100 -200",
            b"\\M1 2 3",
            b"\\FBoxes",
            b"\\f1 Name",
        ]));
    }
    name.push(0);
    name
}

/// A string to measure or paint: characters, every control sequence, its
/// parameters wild, UTF-8 runs broken, a terminator or none.
fn text(rng: &mut Prng, handles: &[u32]) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..rng.below(24) {
        match rng.below(12) {
            0 => out.extend_from_slice(&[9, rng.next_u8(), rng.next_u8(), rng.next_u8()]),
            1 => out.extend_from_slice(&[11, rng.next_u8(), rng.next_u8(), rng.next_u8()]),
            2 => out.extend_from_slice(&[17, rng.next_u8()]),
            3 => out.extend_from_slice(&[18, rng.next_u8(), rng.next_u8(), rng.next_u8()]),
            4 => out.extend_from_slice(&[
                19,
                rng.next_u8(),
                rng.next_u8(),
                rng.next_u8(),
                rng.next_u8(),
                rng.next_u8(),
                rng.next_u8(),
                rng.next_u8() % 16,
            ]),
            5 => out.extend_from_slice(&[21, b'c', b'o', 1]),
            6 => out.extend_from_slice(&[25, rng.next_u8(), rng.next_u8()]),
            7 => out.extend_from_slice(&[26, u8::try_from(*rng.pick(handles)).unwrap_or(1)]),
            8 => {
                out.push(*rng.pick(&[27, 28]));
                out.extend((0..28).map(|_| rng.next_u8()));
            }
            9 => out.extend_from_slice(rng.pick(&[
                &[0xE2, 0x82, 0xAC][..],
                &[0xC0, 0x80],
                &[0xFF],
                &[0xF0, 0x9F],
            ])),
            10 => out.push(*rng.pick(&[1, 7, 31, 127])),
            _ => out.extend_from_slice(b"AB C"),
        }
    }
    out.push(*rng.pick(&[0, 10, 13, 0]));
    out
}

/// A hostile register: a block, a pointer anywhere in the salted space, a
/// flag combination, or anything that does not reach the kernel's memory.
fn hostile(rng: &mut Prng, kernel: &KernelMemory) -> u32 {
    loop {
        let block =
            APPLICATION + BLOCK * u32::try_from(rng.below((SALTED / BLOCK) as usize)).unwrap_or(0);
        let into = APPLICATION + u32::try_from(rng.below(SALTED as usize)).unwrap_or(0);
        let anything = rng.next_u32();
        let value = *rng.pick(&[
            0,
            1,
            2,
            14,
            0x100,
            0x20_0000,
            block,
            block,
            into,
            u32::MAX,
            0x6575_7254,
            anything,
        ]);
        if !kernel.holds(value) {
            return value;
        }
    }
}

/// A session over a run's disc, its application space salted with strings,
/// identifiers, blocks and noise; the fonts found.
fn boot(rng: &mut Prng) -> (Session, KernelMemory, Vec<u32>, u32) {
    let tree = disc(rng);
    let mut s = session(&Setup {
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules: MODULES.as_slice(),
        bytes: SLOT_BYTES,
        keys: &[],
        disc: Some(&tree),
    });
    let kernel = KernelMemory::of(&mut s);
    let held = rma_used(&mut s);
    command(
        &mut s.kernel.os(),
        "Set Font$Path HostFS:$.Lib.,Resources:$.Fonts.",
    )
    .expect("Font$Path");
    let mut handles = Vec::new();
    for _ in 0..6 {
        let name = identifier(rng);
        let at = APPLICATION + SALTED;
        let _ = s.kernel.os().write_bytes(at, &name);
        let size = *rng.pick(&[192u32, 1, 640, 6000, 0x7FFF_FFFF]);
        let mut regs = Regs::new([0, at, size, size, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        if s.kernel
            .os()
            .perform(X | font::FIND_FONT, &mut regs)
            .is_ok()
        {
            handles.push(regs.r[0]);
        }
    }
    handles.push(1);
    for n in 0..SALTED / BLOCK {
        let base = APPLICATION + BLOCK * n;
        let fill: Vec<u8> = match rng.below(4) {
            0 => text(rng, &handles),
            1 => identifier(rng),
            2 => (0..BLOCK)
                .flat_map(|_| hostile(rng, &kernel).to_le_bytes())
                .take(BLOCK as usize)
                .collect(),
            _ => rng.noise(BLOCK as usize),
        };
        let _ = s
            .kernel
            .os()
            .write_bytes(base, &fill[..fill.len().min(BLOCK as usize)]);
    }
    (s, kernel, handles, held)
}

/// What a refused call said.
fn refusal(s: &mut Session, failed: Fail) -> String {
    let Fail::Error(block) = failed else {
        return format!("{failed:?}");
    };
    let mut os = s.kernel.os();
    let number = os.read_word(block).unwrap_or(0);
    let text = os
        .read_until(block + 4, 200, |b| b == 0)
        .map(|t| t.0)
        .unwrap_or_default();
    format!("{number:#x} {}", String::from_utf8_lossy(&text))
}

/// `swi` with `regs`, which must succeed.
fn perform(s: &mut Session, swi: u32, regs: &mut Regs, what: &str) {
    if let Err(failed) = s.kernel.os().perform(X | swi, regs) {
        panic!("{what}: {}", refusal(s, failed));
    }
}

/// A sound font found, measured and painted: the calls reach the module.
fn reaches_fonts(s: &mut Session) {
    let at = APPLICATION + SALTED;
    s.kernel
        .os()
        .write_bytes(at, b"Whole\0AB C\0")
        .expect("written");
    let mut find = Regs::new([0, at, 192, 192, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    perform(s, font::FIND_FONT, &mut find, "found");
    let handle = find.r[0];
    let mut colours = Regs::new([handle, 0, 1, 6, 0xFFFF_FF00, 0, 0, 0, 0, 0, 0, 0, 0]);
    perform(s, font::SET_PALETTE, &mut colours, "coloured");
    let mut scan = Regs::new([
        handle,
        at + 6,
        0x100,
        0x7FFF_FFFF,
        0x7FFF_FFFF,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);
    perform(s, font::SCAN_STRING, &mut scan, "measured");
    assert_eq!(scan.r[1], at + 10, "the whole string measured");
    let mut paint = Regs::new([
        handle,
        at + 6,
        0x100,
        100_000,
        160_000,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);
    perform(s, font::PAINT, &mut paint, "painted");
    let mut made = Regs::new([0, handle, 256, 256, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    perform(s, font::MAKE_BITMAP, &mut made, "bitmaps made");
    perform(s, font::PAINT, &mut paint, "painted from the bitmaps");
    let buffer = APPLICATION + SALTED + 0x100;
    s.kernel
        .os()
        .write_bytes(buffer, &[0, 0, 0, 0, 0, 0x10, 0, 0])
        .expect("an empty buffer");
    let mut switched = Regs::new([0, buffer, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    perform(s, font::SWITCH_OUTPUT_TO_BUFFER, &mut switched, "switched");
    let mut rgb = Regs::new([handle, 0xFFFF_FF00, 0, 14, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    perform(
        s,
        tairix_riscos_kernel::swi::colour_trans::SET_FONT_COLOURS,
        &mut rgb,
        "RGB colours",
    );
    perform(s, font::PAINT, &mut paint, "painted into the buffer");
    let mut back = Regs::new([0; 13]);
    perform(s, font::SWITCH_OUTPUT_TO_BUFFER, &mut back, "switched back");
    assert!(back.r[1] > buffer, "the buffer holds the paint's objects");
}

fn call(s: &mut Session, rng: &mut Prng, kernel: &KernelMemory, handles: &[u32]) {
    s.allow_words(Some(HOSTILE_CALL_WORDS));
    if rng.below(24) == 0 {
        let _ = select_depth(s, u32::try_from(rng.below(6)).unwrap_or(0));
        return;
    }
    let mut regs = Regs::new([0; 13]);
    for register in &mut regs.r[..8] {
        *register = hostile(rng, kernel);
    }
    if rng.below(2) == 0 {
        regs.r[0] = *rng.pick(handles);
    }
    let swi = FONT + u32::try_from(rng.below(SWIS)).unwrap_or(0);
    let result = s.kernel.os().perform(X | swi, &mut regs);
    if let Err(Fail::Error(block)) = result {
        assert!(
            s.kernel.os().read_word(block).is_ok(),
            "the error block at {block:#x} can be read"
        );
    }
}

#[test]
fn hostile_fonts_and_calls_leave_the_drivers_answering() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_fonts_and_calls_leave_the_drivers_answering",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        let (mut s, kernel, handles, held) = boot(&mut rng);
        reaches_fonts(&mut s);
        for step in 0..CALLS {
            call(&mut s, &mut rng, &kernel, &handles);
            assert!(answers(&mut s), "OS_ReadModeVariable after step {step}");
        }
        s.allow_words(None);
        if let Err(failed) = command(&mut s.kernel.os(), "RMReInit FontManager") {
            panic!("reinitialised: {}", refusal(&mut s, failed));
        }
        assert_eq!(rma_used(&mut s), held, "all it took given back");
        runs += 1;
    }
}
