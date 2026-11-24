//! The Font Manager's frame in a session, through its SWIs: handles found
//! and lost, read back, the catalogue Font$Path reaches, and the settings.

use std::sync::LazyLock;

use tairix_riscos_filing::session::{modules_with, session, Session, Setup, SLOT_BYTES};
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::{Fail, Regs};
use tairix_riscos_kernel::swi::{chunk, os, X};

/// The modules: the filing system's, the VDU drivers' and the Font
/// Manager's.
static MODULES: LazyLock<Vec<&'static Builtin>> =
    LazyLock::new(|| modules_with(&[&tairix_riscos_vdu::MODULES, &tairix_riscos_font::MODULES]));

/// Where a test writes text, and the buffer a call answers into.
const TEXT: u32 = 0x9000;
const BUFFER: u32 = 0xA000;

/// A session with no font service.
fn unserved() -> Session {
    session(&Setup {
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules: MODULES.as_slice(),
        bytes: SLOT_BYTES,
        keys: &[],
        disc: None,
    })
}

/// A session whose font service answers every character with a square.
fn fonts() -> Session {
    let s = unserved();
    s.serve_fonts(Box::new(tairix_riscos_font::build::square_faces));
    s
}

/// Font Manager call `offset` with `regs`: its registers, or its error's
/// number.
fn font(s: &mut Session, offset: u32, low: &[u32]) -> Result<Regs, u32> {
    let mut regs = Regs::default();
    regs.r[..low.len()].copy_from_slice(low);
    let mut os = s.kernel.os();
    match os.perform(X | (chunk::FONT + offset), &mut regs) {
        Ok(()) => Ok(regs),
        Err(Fail::Error(block)) => Err(os.read_word(block).expect("the error's number")),
        Err(other) => panic!("{other:?}"),
    }
}

fn write(s: &mut Session, at: u32, text: &[u8]) -> u32 {
    let mut os = s.kernel.os();
    os.write_bytes(at, text).expect("written");
    os.write_byte(at + u32::try_from(text.len()).expect("short"), 0)
        .expect("terminated");
    at
}

fn read(s: &mut Session, at: u32) -> Vec<u8> {
    s.kernel
        .os()
        .read_until(at, 256, |b| b < b' ')
        .expect("read")
        .0
}

fn mode_variable(s: &mut Session, variable: u32) -> u32 {
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[u32::MAX, variable]);
    s.kernel
        .os()
        .perform(X | os::READ_MODE_VARIABLE, &mut regs)
        .expect("read");
    regs.r[2]
}

const FIND_FONT: u32 = 0x01;
const LOSE_FONT: u32 = 0x02;
const READ_DEFN: u32 = 0x03;
const CURRENT_FONT: u32 = 0x0B;
const LIST_FONTS: u32 = 0x11;
const READ_FONT_PREFIX: u32 = 0x1D;
const LOOKUP_FONT: u32 = 0x28;
const FULL: u32 = u32::from_le_bytes(*b"FULL");

#[test]
fn a_rom_font_is_found_on_handle_1_its_master_on_2_at_the_mode_s_resolution() {
    let mut s = fonts();
    let start = font(&mut s, CURRENT_FONT, &[]).expect("current");
    assert_eq!(start.r[..4], [1, 0, 1, 14]);
    let name = write(&mut s, TEXT, b"Homerton.Medium");
    let found = font(&mut s, FIND_FONT, &[0, name, 192, 192, 0, 0]).expect("found");
    let (ex, ey) = (mode_variable(&mut s, 4), mode_variable(&mut s, 5));
    assert_eq!(found.r[0], 1);
    assert_eq!((found.r[4], found.r[5]), (180 >> ex, 180 >> ey));
    let defn = font(&mut s, READ_DEFN, &[1, BUFFER, 0]).expect("read");
    assert_eq!(read(&mut s, BUFFER), b"Homerton.Medium");
    assert_eq!(defn.r[2..8], [192, 192, 180 >> ex, 180 >> ey, 0, 1]);
    let master = font(&mut s, READ_DEFN, &[2, BUFFER, 0]).expect("its master");
    assert_eq!(master.r[2], u32::MAX);
    font(&mut s, READ_DEFN, &[1, BUFFER, 0, FULL]).expect("full");
    assert_eq!(read(&mut s, BUFFER), b"\\FHomerton.Medium\\Elatin1");
    font(&mut s, READ_FONT_PREFIX, &[1, BUFFER, 256]).expect("prefix");
    assert_eq!(read(&mut s, BUFFER), b"Resources:$.Fonts.Homerton.Medium.");
    let lookup = font(&mut s, LOOKUP_FONT, &[1, 0, 0]).expect("looked up");
    assert_eq!(
        lookup.r[2], 0x202,
        "in ROM, and a TAIRiX face, filled non-zero"
    );
    let again = write(&mut s, TEXT, b"homerton.medium");
    let found = font(&mut s, FIND_FONT, &[0, again, 192, 192, 0, 0]).expect("again");
    assert_eq!(found.r[0], 1);
    font(&mut s, LOSE_FONT, &[1]).expect("lost");
    let use_count = font(&mut s, READ_DEFN, &[1, BUFFER, 0]).expect("read").r[7];
    assert_eq!(use_count, 1);
    let missing = write(&mut s, TEXT, b"NoSuchFont");
    assert_eq!(
        font(&mut s, FIND_FONT, &[0, missing, 192, 192, 0, 0]),
        Err(0x20B)
    );
    assert_eq!(
        font(&mut s, FIND_FONT, &[0, name, 0, 192, 0, 0]),
        Err(0x21A)
    );
    assert_eq!(font(&mut s, READ_DEFN, &[77, BUFFER, 0]), Err(0x208));
}

#[test]
fn a_rom_font_the_font_service_does_not_draw_has_no_data() {
    let mut s = unserved();
    let name = write(&mut s, TEXT, b"Trinity.Medium");
    assert_eq!(
        font(&mut s, FIND_FONT, &[0, name, 192, 192, 0, 0]),
        Err(0x20B)
    );
}

#[test]
fn the_catalogue_lists_the_rom_fonts_in_order() {
    let mut s = fonts();
    let mut listed = Vec::new();
    let mut counter = 0x3_0000;
    for _ in 0..64 {
        let regs = font(
            &mut s,
            LIST_FONTS,
            &[0, BUFFER, counter, 256, BUFFER + 256, 256],
        )
        .expect("listed");
        if regs.r[2] == u32::MAX {
            break;
        }
        listed.push(String::from_utf8(read(&mut s, BUFFER)).expect("text"));
        counter = regs.r[2];
    }
    assert_eq!(listed.first().map(String::as_str), Some("Corpus.Bold"));
    assert_eq!(
        listed.iter().filter(|f| f.starts_with("Homerton")).count(),
        4
    );
    assert!(listed
        .windows(2)
        .all(|w| w[0].to_lowercase() < w[1].to_lowercase()));
}
