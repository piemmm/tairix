use alloc::vec::Vec;

use tairix_riscos_filing::session::Session;

use crate::fixture::{call, find, find_at, fonts, mode, write, TEXT};
use crate::module::call::{MAKE_BITMAP, READ_DEFN};
use crate::outlines::{self, Bitmaps};

const PIX: &str = "HostFS:$.Lib.Pix.";

/// Mode 28, ninety dots an inch, and `Pix` found at 16 points in it.
fn pix_at_16() -> (Session, u32) {
    let mut s = fonts();
    mode(&mut s, 28);
    let handle = find(&mut s, b"Pix", 256);
    (s, handle)
}

/// The file `name` in `Pix`'s directory, or none.
fn pix_file(s: &mut Session, name: &str) -> Option<Vec<u8>> {
    let mut os = s.kernel.os();
    let mut full = PIX.as_bytes().to_vec();
    full.extend_from_slice(name.as_bytes());
    crate::scan::load(&mut os, &full, 1 << 20).expect("read")
}

fn make(s: &mut Session, font: u32, size: (i32, i32), flags: u32) -> Result<(), u32> {
    let [x, y] = [size.0, size.1].map(i32::cast_unsigned);
    call(s, MAKE_BITMAP, &[0, font, x, y, 90, 90, flags]).map(drop)
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("a word"))
}

#[test]
fn a_one_bit_file_is_laid_out_as_risc_os_5_30_lays_one_out() {
    let (mut s, pix) = pix_at_16();
    make(&mut s, pix, (256, 256), 0).expect("made");
    let file = pix_file(&mut s, "b320x320").expect("the file");
    assert_eq!(
        &file[..8],
        b"FONT\x01\x08\x40\x00",
        "one bit, version 8, flag words before indices"
    );
    let table = usize::try_from(word(&file, 16)).expect("small");
    assert_eq!(
        (word(&file, 20), word(&file, 24)),
        (3, 1),
        "chunks to G's, one scaffold entry"
    );
    assert_eq!(
        &file[52..62],
        &[10, 0, 0, 1, 90, 0, 0, 1, 90, 0],
        "the sizes and resolutions"
    );
    assert_eq!(
        &file[62..table],
        b"Pix\x0016x16 points at 90x90 dpi\x00",
        "name, description, aligned"
    );
    assert_eq!(table, 0x5C);
    let chunk_2 = usize::try_from(word(&file, table + 8)).expect("small");
    assert_eq!(word(&file, chunk_2), 0x8000_0000);
    let a = chunk_2 + 4 + usize::try_from(word(&file, chunk_2 + 4 + 4)).expect("small");
    assert_eq!(
        &file[a..a + 12],
        &[2, 0, 0, 4, 8, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0],
        "A as RISC OS 5.30 wrote it"
    );
    assert_eq!(
        word(&file, chunk_2 + 4),
        0,
        "glyph 64, which the font has not, is not defined"
    );
    assert_eq!(
        usize::try_from(word(&file, table + 12)).expect("small"),
        file.len(),
        "the end"
    );
}

#[test]
fn a_file_of_sub_placements_holds_each_glyph_four_times_across() {
    let (mut s, pix) = pix_at_16();
    make(&mut s, pix, (256, 256), 0b11).expect("made");
    let bytes = pix_file(&mut s, "f320x320").expect("the file");
    assert_eq!(
        u16::from_le_bytes([bytes[6], bytes[7]]),
        0x41,
        "flagged, placed across"
    );
    let outlines::File::Bitmaps(bitmaps) = outlines::File::read(bytes).expect("a font file") else {
        panic!("bitmaps");
    };
    let Bitmaps {
        placements,
        points,
        resolution,
        ..
    } = &bitmaps;
    assert_eq!(
        (*placements, *points, *resolution),
        ((true, false), (256, 256), (90, 90))
    );
    let lefts: Vec<i32> = (0..4)
        .map(|across| {
            bitmaps
                .glyph(u32::from(b'A'), (across, 0))
                .expect("room")
                .expect("A")
                .left
        })
        .collect();
    assert_eq!(lefts[0], 0);
    assert!(
        lefts.windows(2).all(|pair| pair[0] <= pair[1]),
        "each a quarter further: {lefts:?}"
    );
}

#[test]
fn a_glyph_past_eight_bits_of_coordinates_takes_twelve() {
    let (mut s, pix) = pix_at_16();
    make(&mut s, pix, (6400, 6400), 0).expect("made");
    let file = pix_file(&mut s, "b8000x8000").expect("the file");
    let table = usize::try_from(word(&file, 16)).expect("small");
    let chunk_2 = usize::try_from(word(&file, table + 8)).expect("small");
    let a = chunk_2 + 4 + usize::try_from(word(&file, chunk_2 + 4 + 4)).expect("small");
    assert_eq!(
        &file[a..a + 7],
        &[3, 0, 0, 0, 0x64, 0x80, 0x0C],
        "100 by 200 at (0, 0), as RISC OS 5.30 packs it"
    );
    let g = chunk_2 + 4 + usize::try_from(word(&file, chunk_2 + 4 + 4 * 7)).expect("small");
    assert_eq!(
        &file[g..g + 5],
        &[2, 0, 0xCE, 0x64, 0x32],
        "G fits eight bits: 100 by 50 from (0, -50)"
    );
}

#[test]
fn a_file_deleted_is_gone_and_deleting_one_not_there_is_no_error() {
    let (mut s, pix) = pix_at_16();
    make(&mut s, pix, (256, 256), 1).expect("made");
    assert!(pix_file(&mut s, "f320x320").is_some());
    make(&mut s, pix, (256, 256), 0b1001).expect("deleted");
    assert!(pix_file(&mut s, "f320x320").is_none());
    make(&mut s, pix, (256, 256), 0b1001).expect("nothing to delete");
}

#[test]
fn what_risc_os_5_30_refuses_is_refused_with_its_errors() {
    let (mut s, pix) = pix_at_16();
    assert_eq!(
        make(&mut s, pix, (256, 256), 0b10),
        Err(0x213),
        "sub-placement in a one-bit file"
    );
    assert_eq!(make(&mut s, pix, (256, 256), 0b100), Err(0x213));
    assert_eq!(make(&mut s, pix, (256, 256), 1 << 4), Err(0x213));
    assert_eq!(make(&mut s, pix, (0, 0), 1), Err(0x21A));
    assert_eq!(
        make(&mut s, pix, (8000, 8000), 0),
        Err(0xCC),
        "10000 pixels has no file name"
    );
    assert_eq!(make(&mut s, pix, (-256, -256), 1), Err(0x20B));
    assert_eq!(make(&mut s, 99, (256, 256), 1), Err(0x208));
    let name = write(&mut s, TEXT, b"Nonesuch");
    assert_eq!(make(&mut s, name, (256, 256), 1), Err(0x20B));
}

#[test]
fn a_font_named_is_found_for_the_call_and_lost_after_it() {
    let (mut s, _) = pix_at_16();
    let name = write(&mut s, TEXT, b"Pix");
    make(&mut s, name, (192, 192), 1).expect("made by name");
    assert!(pix_file(&mut s, "f240x240").is_some());
    let twelve = find_at(&mut s, b"Pix", (192, 192), (90, 90)).expect("found");
    let uses = call(&mut s, READ_DEFN, &[twelve, crate::fixture::BLOCK, 0])
        .expect("defined")
        .r[7];
    assert_eq!(uses, 1, "the call's own use of the size lost");
}

#[test]
fn a_file_made_is_what_painting_then_draws_from() {
    let (mut s, pix) = pix_at_16();
    let size = crate::makebitmap::Size {
        depth: crate::outlines::Depth::Four,
        points: (256, 256),
        resolution: (90, 90),
        placements: (false, false),
    };
    let font = crate::resolve::font(&mut s.kernel.os(), pix).expect("resolved");
    let made = crate::makebitmap::file(&font, &size).expect("laid out");
    make(&mut s, pix, (256, 256), 1).expect("made");
    assert_eq!(
        pix_file(&mut s, "f320x320"),
        Some(made),
        "the call saves what the font's glyphs make"
    );
}
