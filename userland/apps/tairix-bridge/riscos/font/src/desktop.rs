//! The desktop font (`plans/RISCOS-EMULATOR.md` §8): the `Desktop` ROM
//! family, which is the TAIRiX desktop's interface face in whatever family
//! the session's host gives, and the Wimp's variables naming it.

use alloc::vec::Vec;

use tairix_riscos_kernel::desktop::DesktopFont;
use tairix_riscos_kernel::os::{Fail, Os};

/// The family the desktop font's ROM fonts are named by, and the style of
/// the one the Wimp's variables name.
const FAMILY: &str = "Desktop";
const MEDIUM: &[u8] = b".Medium";
/// The Wimp's variables: the desktop font's name, and its height and width
/// in sixteenths of a point.
const WIMP_FONT: &[u8] = b"Wimp$Font";
const WIMP_FONT_SIZES: [&[u8]; 2] = [b"Wimp$FontSize", b"Wimp$FontWidth"];

/// Whether RISC OS font `name` is one of the desktop font's.
pub(crate) fn names(name: &str) -> bool {
    name.split('.')
        .next()
        .is_some_and(|family| family.trim().eq_ignore_ascii_case(FAMILY))
}

/// The Wimp's desktop font variables, each where none is set: the desktop
/// font's medium weight, at `desktop`'s size where its host gives one, the
/// Wimp's own twelve points standing otherwise.
pub(crate) fn set_variables(os: &mut Os<'_>, desktop: DesktopFont) -> Result<(), Fail> {
    if os.variable_text(WIMP_FONT)?.is_none() {
        let mut name = Vec::new();
        if name.try_reserve_exact(FAMILY.len() + MEDIUM.len()).is_err() {
            return Err(os.no_memory());
        }
        name.extend_from_slice(FAMILY.as_bytes());
        name.extend_from_slice(MEDIUM);
        os.set_string_variable(WIMP_FONT, &name)?;
    }
    let Some(size) = desktop.size else {
        return Ok(());
    };
    let mut buffer = [0; 20];
    let digits = tairix_util::fmt::format_u64(u64::from(size), &mut buffer).as_bytes();
    for variable in WIMP_FONT_SIZES {
        if os.variable_text(variable)?.is_none() {
            os.set_string_variable(variable, digits)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
