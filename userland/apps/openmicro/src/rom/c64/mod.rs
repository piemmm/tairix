//! The clean-room ROMs of the Commodore 64 and its 1541 (`plans/C64.md` §5).

pub mod basic;
pub mod charset;
pub mod kernal;
pub mod petscii;
#[cfg(test)]
pub(crate) mod testing;

use crate::machine::c64::memory::{BASIC_BASE, BASIC_BYTES, KERNAL_BASE, KERNAL_BYTES};
use crate::rom::asm::{Asm, AsmError, Image, ERASED};

/// BASIC's ROM and the KERNAL's, one assembly as the originals are one
/// program: BASIC runs on past its own ROM into the room the KERNAL's
/// leaves.
pub struct System {
    /// From BASIC's base to the KERNAL's end, the RAM and I/O between the
    /// two ROMs erased.
    pub image: Image,
}

impl System {
    /// BASIC's ROM.
    #[must_use]
    pub fn basic(&self) -> &[u8] {
        self.image.bytes.get(..BASIC_BYTES).unwrap_or_default()
    }

    /// The KERNAL's ROM.
    #[must_use]
    pub fn kernal(&self) -> &[u8] {
        self.image.bytes.get(kernal_offset()..).unwrap_or_default()
    }

    /// One ROM's `bytes` from `base`, with the labels that fall in it.
    fn rom(&self, bytes: &[u8], base: u16) -> Image {
        let end = u32::from(base) + u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        let symbols = self.image.symbols.iter().copied();
        Image {
            bytes: bytes.to_vec(),
            symbols: symbols
                .filter(|&(_, at)| (u32::from(base)..end).contains(&u32::from(at)))
                .collect(),
        }
    }
}

/// BASIC's ROM as the bundle ships it.
///
/// # Errors
///
/// The builder's first [`AsmError`].
pub fn basic_image() -> Result<Image, AsmError> {
    let system = system()?;
    Ok(system.rom(system.basic(), BASIC_BASE))
}

/// The KERNAL's ROM as the bundle ships it.
///
/// # Errors
///
/// The builder's first [`AsmError`].
pub fn kernal_image() -> Result<Image, AsmError> {
    let system = system()?;
    Ok(system.rom(system.kernal(), KERNAL_BASE))
}

/// Where the KERNAL's ROM starts in [`System::image`].
fn kernal_offset() -> usize {
    usize::from(KERNAL_BASE - BASIC_BASE)
}

/// The BASIC and KERNAL ROMs: every fixed address pinned before any code
/// flows, so none is run over; the KERNAL and its tables from its base,
/// claimed whole; then BASIC from its own base, running on into the KERNAL
/// ROM's free tail.
///
/// # Errors
///
/// The builder's first [`AsmError`].
pub fn system() -> Result<System, AsmError> {
    let between = BASIC_BASE.wrapping_add(u16::try_from(BASIC_BYTES).unwrap_or(u16::MAX));
    let mut a = Asm::new(BASIC_BASE, kernal_offset() + KERNAL_BYTES);
    a.pin(between, |a| {
        a.fill(usize::from(KERNAL_BASE - between), ERASED);
    });
    kernal::pin(&mut a);
    basic::pin(&mut a);
    a.org(KERNAL_BASE);
    kernal::emit(&mut a);
    a.place_later();
    a.claim(KERNAL_BASE, a.pc());
    basic::emit(&mut a)?;
    a.place_later();
    Ok(System { image: a.finish()? })
}
