//! The session the crate's tests measure in: the Font Manager, the VDU
//! drivers, Draw and DrawFile and the filing system, and a HostFS disc on
//! `Font$Path` holding the first-party fonts RISC OS 5.30 was probed with,
//! so every number the tests hold the Font Manager to is one RISC OS 5.30
//! gave for these fonts.

extern crate std;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::vec::Vec;
use std::sync::LazyLock;

use tairix_colour::Rgb;
use tairix_fscopy::fixture::Tree;
use tairix_riscos_filing::session::{
    command, modules_with, session, Session, Setup, DISC, SLOT_BYTES,
};
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::Regs;
use tairix_riscos_kernel::swi::font::flag::HANDLE;
use tairix_riscos_kernel::swi::X;

use crate::build::{
    filled as character, rectangle as rect, Element, Font, Point, SESSION_MODULES, SET_PATH,
};
pub(crate) use crate::build::{BLOCK, TEXT};
use crate::module::call::PAINT;

static MODULES: LazyLock<Vec<&'static Builtin>> = LazyLock::new(|| modules_with(&SESSION_MODULES));

/// Where a test writes a matrix.
pub(crate) const MATRIX: u32 = 0xA100;
/// Where the probes painted: 125 pixels across and 200 up in mode 28.
pub(crate) const AT: [u32; 2] = [100_000, 160_000];
/// White and red, as RISC OS colour words.
pub(crate) const WHITE: u32 = Rgb::WHITE.riscos_word();
pub(crate) const RED: u32 = Rgb::new(u8::MAX, 0, 0).riscos_word();

/// The font the layout probes measured: awkward advances and kerns at the
/// base encoding 0 places of space, A to E and i.
pub(crate) fn scan() -> Font {
    let mut c = BTreeMap::new();
    c.insert(32, character(250, Vec::new()));
    c.insert(b'A', character(700, rect(50, 0, 650, 700)));
    c.insert(
        b'B',
        character(
            650,
            alloc::vec![
                Element::Move(Point::new(60, 0)),
                Element::Curve(
                    Point::new(300, 0),
                    Point::new(600, 300),
                    Point::new(600, 700)
                ),
                Element::Line(Point::new(60, 700)),
            ],
        ),
    );
    c.insert(b'C', character(333, rect(30, -200, 300, 500)));
    c.insert(b'D', character(1, rect(0, 0, 1, 1)));
    c.insert(b'E', character(999, rect(-100, 100, 1100, 900)));
    c.insert(b'i', character(278, rect(80, 0, 200, 500)));
    Font {
        name: b"Scan".to_vec(),
        design_size: 1000,
        characters: c,
        kerns: alloc::vec![(b'A', b'B', -100), (b'B', b'A', 37), (b'A', b'A', -1)],
    }
}

/// Its companion of other advances.
pub(crate) fn scan2() -> Font {
    let mut c = BTreeMap::new();
    c.insert(32, character(300, Vec::new()));
    c.insert(b'A', character(500, rect(0, 0, 500, 500)));
    c.insert(b'B', character(777, rect(0, 0, 700, 800)));
    Font {
        name: b"Scan2".to_vec(),
        design_size: 1000,
        characters: c,
        kerns: Vec::new(),
    }
}

/// An `x90y45` file of one size, `points` at `dots` across and up, holding
/// character `code` as a `width` pixel wide glyph from `corner`, its levels
/// from the bottom row up.
pub(crate) fn old_bitmaps(
    points: u8,
    dots: (u8, u8),
    code: u8,
    corner: (i8, i8),
    width: u8,
    levels: &[u8],
) -> Vec<u8> {
    let width = usize::from(width);
    let top_first: Vec<u8> = levels.chunks(width).rev().flatten().copied().collect();
    let glyph = crate::build::Pixels {
        code,
        corner: (i32::from(corner.0), i32::from(corner.1)),
        width,
        levels: top_first,
    };
    crate::build::old_bitmaps(points, dots, &glyph)
}

/// The glyph the old bitmaps font holds at 16 points and 90 dots an inch:
/// `A` three pixels by four from (1, 0), levels 1 to 12 from its bottom
/// left, as the bitmap probes had it.
pub(crate) const OLD_A: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

/// Every pixel of the screen made `value`, a byte at a time.
pub(crate) fn fill(s: &mut Session, value: u8) {
    let block = BLOCK + 0x300;
    write_words(s, block, &[148, 7, -1]);
    let mut os = s.kernel.os();
    let mut regs = tairix_riscos_kernel::os::Regs::default();
    regs.r[..2].copy_from_slice(&[block, block + 0x20]);
    os.perform(
        tairix_riscos_kernel::swi::X | tairix_riscos_kernel::swi::os::READ_VDU_VARIABLES,
        &mut regs,
    )
    .expect("the variables");
    let start = os.read_word(block + 0x20).expect("the start");
    let size = os.read_word(block + 0x24).expect("the size");
    let line = alloc::vec![value; 4096];
    let mut done = 0;
    while done < size {
        let here = (size - done).min(4096);
        os.write_bytes(start + done, &line[..here as usize])
            .expect("the screen");
        done += here;
    }
}

/// `bytes` saved as the font file `name`.
pub(crate) fn save(s: &mut Session, name: &str, bytes: &[u8]) {
    let mut os = s.kernel.os();
    crate::scan::save(
        &mut os,
        name.as_bytes(),
        tairix_riscos_kernel::filing::filetype::FONT,
        bytes,
    )
    .expect("saved");
}

/// A font of glyphs at `codes`, each advancing `base` plus its code.
fn numbered(name: &str, codes: &[u8], base: i16) -> Font {
    let mut c = BTreeMap::new();
    for &code in codes {
        let fill = if code == 32 {
            Vec::new()
        } else {
            rect(10, 0, 100, 400)
        };
        c.insert(code, character(base + i16::from(code), fill));
    }
    Font {
        name: name.as_bytes().to_vec(),
        design_size: 1000,
        characters: c,
        kerns: Vec::new(),
    }
}

/// `/name`s for each code to the last listed, eight to a line.
fn names(list: &[(usize, &str)]) -> Vec<u8> {
    let final_code = list.iter().map(|e| e.0).max().unwrap_or(0);
    let mut out = alloc::string::String::new();
    for i in 0..=final_code {
        out.push('/');
        out.push_str(list.iter().find(|e| e.0 == i).map_or(".notdef", |e| e.1));
        out.push(if i % 8 == 7 { '\n' } else { ' ' });
    }
    out.push('\n');
    out.into_bytes()
}

/// The disc: `Lib`, a font library of the fonts above, and of fonts whose
/// data files are found as the UCS Font Manager finds them.
fn disc() -> Tree {
    let library = format!("{DISC}/Lib");
    let low = [32, 33, 34, 35];
    let fonts = [
        ("Scan", "IntMetric0", "Outlines0", scan()),
        ("Scan2", "IntMetric0", "Outlines0", scan2()),
        ("Pix", "IntMetric0", "Outlines0", crate::build::pix()),
        ("Cv", "IntMetric0", "Outlines0", crate::build::cv()),
        ("Lay", "IntMetric0", "Outlines0", crate::build::lay()),
        (
            "SymEnc",
            "IntMetrics",
            "Outlines",
            numbered("SymEnc", &low, 2000),
        ),
        (
            "SymNoEnc",
            "IntMetrics",
            "Outlines",
            numbered("SymNoEnc", &low, 1000),
        ),
        (
            "Base1F",
            "IntMetric1",
            "Outlines1",
            numbered("Base1F", &low, 4000),
        ),
        (
            "Base32F",
            "IntMetri32",
            "Outlines32",
            numbered("Base32F", &low, 3000),
        ),
        (
            "TwoSets",
            "IntMetric0",
            "Outlines0",
            numbered("TwoSets", &[32, 65, 66, 67], 5000),
        ),
        (
            "TwoSets",
            "IntMetrics",
            "Outlines",
            numbered("TwoSets", &low, 6000),
        ),
    ];
    let mut tree = Tree::new().dir(&library);
    for (dir, metrics, outlines, font) in fonts {
        tree = tree
            .file(&format!("{library}/{dir}/{metrics},ff6"), &font.metrics())
            .file(&format!("{library}/{dir}/{outlines},ff6"), &font.outlines());
    }
    tree.file(
        &format!("{library}/SymEnc/Encoding,fff"),
        &names(&[(32, "space"), (33, "B"), (34, "A"), (35, "C")]),
    )
    .file(
        &format!("{library}/Encodings/.Base1,fff"),
        &names(&[(32, "space"), (33, "C"), (34, "B"), (35, "A")]),
    )
    .file(
        &format!("{library}/NoOut/IntMetric0,ff6"),
        &scan().metrics(),
    )
    .file(
        &format!("{library}/Old/IntMetric0,ff6"),
        &crate::build::pix().metrics(),
    )
    .file(
        &format!("{library}/Old/x90y45,ff6"),
        &old_bitmaps(16, (90, 90), b'A', (1, 0), 3, &OLD_A),
    )
    .file(
        &format!("{library}/Bits/IntMetric0,ff6"),
        &crate::build::pix().metrics(),
    )
    .file(&format!("{library}/BadMet/IntMetric0,ff6"), &[0x55; 30])
    .file(
        &format!("{library}/BadMet/Outlines0,ff6"),
        &scan().outlines(),
    )
    .file(
        &format!("{library}/BadOut/IntMetric0,ff6"),
        &scan().metrics(),
    )
    .file(&format!("{library}/BadOut/Outlines0,ff6"), &[0x33; 300])
    .file(
        &format!("{library}/RedirNone/IntMetric0,ff6"),
        &scan().metrics(),
    )
    .file(&format!("{library}/RedirNone/Outlines0,ff6"), b"Nonesuch\r")
}

/// A session on the disc, `Font$Path` its library then ResourceFS's fonts.
pub(crate) fn fonts() -> Session {
    let tree = disc();
    let mut s = session(&Setup {
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules: MODULES.as_slice(),
        bytes: SLOT_BYTES,
        keys: &[],
        disc: Some(&tree),
    });
    command(&mut s.kernel.os(), SET_PATH).expect("Font$Path");
    s
}

/// Font Manager call `offset` with `low` its first registers: its
/// registers, or its error's number.
pub(crate) fn call(s: &mut Session, offset: u32, low: &[u32]) -> Result<Regs, u32> {
    crate::build::call(&mut s.kernel.os(), offset, low)
}

/// `Font_Paint` of `text` in `handle` with `flags` at `x` and `y`, R5 and R6
/// as given; its error's number where it fails.
pub(crate) fn font_paint(
    s: &mut Session,
    handle: u32,
    text: &[u8],
    flags: u32,
    [x, y]: [u32; 2],
    [r5, r6]: [u32; 2],
) -> Result<(), u32> {
    let text = write(s, TEXT, text);
    call(s, PAINT, &[handle, text, HANDLE | flags, x, y, r5, r6]).map(drop)
}

/// Screen mode `number` selected.
pub(crate) fn mode(s: &mut Session, number: u32) {
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[0, number]);
    s.kernel
        .os()
        .perform(X | tairix_riscos_kernel::swi::os::SCREEN_MODE, &mut regs)
        .expect("the mode");
}

/// A 640 by 480 mode of `log2_bpp` bits a pixel selected.
pub(crate) fn deep_mode(s: &mut Session, log2_bpp: i32) {
    let selector = BLOCK + 0x400;
    write_words(s, selector, &[1, 640, 480, log2_bpp, -1, -1]);
    mode(s, selector);
}

/// The screen's memory: its start, its bytes a line, its bits a pixel as a
/// power of two.
fn screen(s: &mut Session) -> (u32, u32, u32) {
    let block = BLOCK + 0x200;
    write_words(s, block, &[148, 6, 9, -1]);
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[block, block + 0x20]);
    s.kernel
        .os()
        .perform(
            X | tairix_riscos_kernel::swi::os::READ_VDU_VARIABLES,
            &mut regs,
        )
        .expect("the variables");
    let [start, line, log2_bpp] = read_words(s, block + 0x20, 3)[..] else {
        panic!("three variables");
    };
    (
        start.cast_unsigned(),
        line.cast_unsigned(),
        log2_bpp.cast_unsigned(),
    )
}

/// The pixels of the screen's `rows`, counted from its top, from column
/// `left` on for `width`.
pub(crate) fn pixels(
    s: &mut Session,
    left: u32,
    width: u32,
    rows: core::ops::Range<u32>,
) -> Vec<Vec<u32>> {
    let (start, line, log2_bpp) = screen(s);
    let bits = 1u32 << log2_bpp;
    let mut os = s.kernel.os();
    rows.map(|row| {
        (left..left + width)
            .map(|x| {
                let bit = x * bits;
                let mut word = [0u8; 4];
                let bytes = bits.div_ceil(8) as usize;
                os.read_bytes(start + row * line + bit / 8, &mut word[..bytes])
                    .expect("the screen");
                let value = u32::from_le_bytes(word) >> (bit % 8);
                if bits >= 32 {
                    value
                } else {
                    value & ((1 << bits) - 1)
                }
            })
            .collect()
    })
    .collect()
}

/// `bytes` written at `at`, a nought after them.
pub(crate) fn write(s: &mut Session, at: u32, bytes: &[u8]) -> u32 {
    crate::build::write(&mut s.kernel.os(), at, bytes)
}

/// `words` written at `at`.
pub(crate) fn write_words(s: &mut Session, at: u32, words: &[i32]) {
    let words: Vec<u32> = words.iter().map(|&word| word.cast_unsigned()).collect();
    s.kernel.os().write_words(at, &words).expect("written");
}

/// The `n` words at `at`.
pub(crate) fn read_words(s: &mut Session, at: u32, n: u32) -> Vec<i32> {
    let mut words = alloc::vec![0; n as usize];
    s.kernel.os().read_words(at, &mut words).expect("read");
    words.into_iter().map(u32::cast_signed).collect()
}

/// The handle `identifier` is found on at `points` sixteenths both ways at
/// the mode's resolution.
pub(crate) fn find(s: &mut Session, identifier: &[u8], points: i32) -> u32 {
    find_at(s, identifier, (points, points), (0, 0)).expect("found")
}

/// `Font_FindFont` of `identifier` at `points` and `resolution`.
pub(crate) fn find_at(
    s: &mut Session,
    identifier: &[u8],
    points: (i32, i32),
    resolution: (i32, i32),
) -> Result<u32, u32> {
    let name = write(s, TEXT + 0x800, identifier);
    call(
        s,
        crate::module::call::FIND_FONT,
        &[
            0,
            name,
            points.0.cast_unsigned(),
            points.1.cast_unsigned(),
            resolution.0.cast_unsigned(),
            resolution.1.cast_unsigned(),
        ],
    )
    .map(|regs| regs.r[0])
}
