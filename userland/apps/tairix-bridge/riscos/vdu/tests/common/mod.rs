//! What the VDU drivers' integration tests share: a session with the drivers
//! and their module, the VDU stream written, and whether the drivers still
//! answer.

#![allow(
    dead_code,
    reason = "each test crate including this uses the helpers its own tests need"
)]

use tairix_riscos_filing::session::{session, Session, Setup, SLOT_BYTES};
use tairix_riscos_kernel::os::{Fail, Regs};
use tairix_riscos_kernel::swi::os::{
    screen_mode, CLI, MODULE, READ_MODE_VARIABLE, SCREEN_MODE, SPRITE_OP, WRITE_C,
};
use tairix_riscos_kernel::swi::X;

/// Where a test writes a mode selector, in the application slot.
pub const SELECTOR_AT: u32 = 0x9000;
/// Where a test writes a command line, in the application slot.
pub const COMMAND_AT: u32 = 0xA000;

/// A session with the drivers and their module, `keys` to type.
pub fn drivers(keys: &[u8]) -> Session {
    session(&Setup {
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules: &tairix_riscos_vdu::MODULES,
        bytes: SLOT_BYTES,
        keys,
        disc: None,
    })
}

/// `bytes` written to the VDU a character at a time.
pub fn vdu(session: &mut Session, bytes: &[u8]) -> Result<(), Fail> {
    for &byte in bytes {
        let mut regs = Regs::new([u32::from(byte), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        session.kernel.os().perform(X | WRITE_C, &mut regs)?;
    }
    Ok(())
}

/// Whether the drivers still read a mode variable.
pub fn answers(session: &mut Session) -> bool {
    let mut regs = Regs::new([u32::MAX, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    session
        .kernel
        .os()
        .perform(X | READ_MODE_VARIABLE, &mut regs)
        .is_ok()
}

/// SWI `swi`'s X form with R0 onwards `r`, its registers on exit.
pub fn call(session: &mut Session, swi: u32, r: &[u32]) -> Result<Regs, Fail> {
    let mut regs = Regs::default();
    regs.r[..r.len()].copy_from_slice(r);
    session.kernel.os().perform(X | swi, &mut regs)?;
    Ok(regs)
}

/// The number of the error `fail` gives.
pub fn error_number(session: &mut Session, fail: Fail) -> u32 {
    match fail {
        Fail::Error(block) => session.kernel.os().read_word(block).expect("its number"),
        Fail::Unwind(unwind) => panic!("unwound: {unwind:?}"),
    }
}

/// `text` run as a command.
pub fn command(session: &mut Session, text: &str) -> Result<(), Fail> {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    session
        .kernel
        .os()
        .write_bytes(COMMAND_AT, &bytes)
        .expect("written");
    call(session, CLI, &[COMMAND_AT]).map(drop)
}

/// A mode of `log2_bpp` bits a pixel at 640 by 480, chosen by selector.
pub fn select_depth(session: &mut Session, log2_bpp: u32) -> Result<(), Fail> {
    select(session, &[1, 640, 480, log2_bpp, u32::MAX, u32::MAX])
}

/// A 640 by 480 mode of 256 colours each its own palette entry.
pub fn select_full_palette(session: &mut Session) -> Result<(), Fail> {
    select(
        session,
        &[1, 640, 480, 3, u32::MAX, 0, 0x80, 3, 255, u32::MAX],
    )
}

/// The mode the selector `words` describe.
fn select(session: &mut Session, words: &[u32]) -> Result<(), Fail> {
    let os = &mut session.kernel.os();
    for (i, &word) in (0u32..).zip(words) {
        os.write_word(SELECTOR_AT + 4 * i, word)?;
    }
    call(session, SCREEN_MODE, &[screen_mode::SELECT, SELECTOR_AT]).map(drop)
}

/// The RMA's free bytes, as `OS_Module` 5 describes them.
pub fn rma_free(session: &mut Session) -> u32 {
    call(session, MODULE, &[5]).expect("described").r[3]
}

/// A sprite area at `area` holding one 8 by 8 sprite of `mode` named as
/// the string at `name`, given its mode's palette where `palette`: the
/// sprite's address.
pub fn lone_sprite(
    session: &mut Session,
    area: u32,
    name: u32,
    mode: u32,
    palette: bool,
) -> Result<u32, Fail> {
    {
        let os = &mut session.kernel.os();
        os.write_word(area, 0x4000)?;
        os.write_word(area + 8, 16)?;
    }
    call(session, SPRITE_OP, &[0x109, area])?;
    call(
        session,
        SPRITE_OP,
        &[0x10F, area, name, u32::from(palette), 8, 8, mode],
    )?;
    Ok(call(session, SPRITE_OP, &[0x118, area, name])?.r[2])
}

/// The palette of `sprite`, the last in `area`, grown to `entries` with its
/// image moved up past them, each new entry steady in colour word `word`.
pub fn grow_palette(
    session: &mut Session,
    area: u32,
    sprite: u32,
    entries: u32,
    word: u32,
) -> Result<(), Fail> {
    const IMAGE: u32 = 32;
    const MASK: u32 = 36;
    let os = &mut session.kernel.os();
    let next = os.read_word(sprite)?;
    let image = os.read_word(sprite + IMAGE)?;
    let held = (image.min(os.read_word(sprite + MASK)?) - 44) / 8;
    let grown = 8 * (entries - held);
    let mut pixels = vec![0; usize::try_from(next - image).expect("a sprite's bytes")];
    os.read_bytes(sprite + image, &mut pixels)?;
    os.write_bytes(sprite + image + grown, &pixels)?;
    for entry in held..entries {
        os.write_word(sprite + 44 + 8 * entry, word)?;
        os.write_word(sprite + 48 + 8 * entry, word)?;
    }
    for offset in [0, IMAGE, MASK] {
        let held = os.read_word(sprite + offset)?;
        os.write_word(sprite + offset, held + grown)?;
    }
    let free = os.read_word(area + 12)?;
    os.write_word(area + 12, free + grown)
}
