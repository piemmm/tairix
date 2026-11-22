//! Setting colours through `OS_SetColour`, as RISC OS 5 sets them: a
//! graphics colour nearest a wanted one, or an ECF nearer still where asked
//! and the mode has a palette; the furthest; a colour number as it is; and
//! the text colours.

use tairix_colour::Rgb;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::colour_trans::BACKGROUND;
use tairix_riscos_kernel::swi::os::{set_colour, SET_COLOUR};
use tairix_riscos_kernel::swi::X;
use tairix_vdu::mode::var;

use super::matching::Look;
use super::{current, ecf, loadings, CURRENT};

/// `SetGCOL`'s R3 bit asking for an ECF where one comes closer.
const USE_ECF: u32 = 1 << 8;
/// `SetColour`'s R3 bit asking for the text colour.
const TEXT: u32 = 1 << 9;

/// `ColourTrans_SetGCOL`, `look` nearest, and `SetOppGCOL`: the colour set
/// with R4's action, R0 its GCOL and R2 the mode's log2 of bits a pixel.
/// `SetGCOL` reports a full palette's GCOL in the grouped form, and leaves
/// only R3's background bit.
pub(super) fn set_gcol(os: &mut Os<'_>, regs: &mut Regs, look: Look) -> Result<(), Fail> {
    let Some(loadings) = loadings(os) else {
        return Ok(());
    };
    let wanted = Rgb::from_riscos_word(regs.r[0]);
    let [log2_bpp] = crate::vars::mode_variables(os, CURRENT, [var::LOG2_BPP])?;
    let Some(against) = current(os)? else {
        return Ok(());
    };
    let number = against.colour_number(wanted, loadings, look);
    let gcol = against.gcol_of(number);
    let reported = match look {
        Look::Nearest => against.reported_gcol(number),
        Look::Furthest => gcol,
    };
    let ecf = if look == Look::Nearest && regs.r[3] & USE_ECF != 0 {
        ecf::pattern(against, wanted, loadings)
    } else {
        None
    };
    let flags = background(regs.r[3]) | (regs.r[4] & set_colour::ACTION);
    match ecf {
        Some(rows) => set_pattern(os, flags, &rows)?,
        None => set(os, flags, gcol)?,
    }
    regs.r[0] = reported;
    regs.r[2] = log2_bpp.unwrap_or(0);
    if look == Look::Nearest {
        regs.r[3] &= BACKGROUND;
    }
    Ok(())
}

/// `ColourTrans_SetColour`: colour number R0 set as the graphics colour
/// with R4's action, or as the text colour where R3 asks.
pub(super) fn set_colour_number(os: &mut Os<'_>, regs: &Regs) -> Result<(), Fail> {
    let to = if regs.r[3] & TEXT == 0 {
        regs.r[4] & set_colour::ACTION
    } else {
        set_colour::TEXT
    };
    set(os, to | background(regs.r[3]), regs.r[0])
}

/// `ColourTrans_SetTextColour`, `look` nearest, and `SetOppTextColour`.
/// RISC OS 5 leaves R0 as it was, not the GCOL its manual gives.
pub(super) fn set_text_colour(os: &mut Os<'_>, regs: &Regs, look: Look) -> Result<(), Fail> {
    let Some(loadings) = loadings(os) else {
        return Ok(());
    };
    let wanted = Rgb::from_riscos_word(regs.r[0]);
    let Some(against) = current(os)? else {
        return Ok(());
    };
    let gcol = against.gcol(wanted, loadings, look);
    set(os, set_colour::TEXT | background(regs.r[3]), gcol)
}

/// `OS_SetColour`'s background flag where R3 asks for the background.
const fn background(r3: u32) -> u32 {
    if r3 & BACKGROUND == 0 {
        0
    } else {
        set_colour::BACKGROUND
    }
}

/// `colour` set through `OS_SetColour` with `flags`, a GCOL in a mode whose
/// GCOLs and colour numbers differ.
fn set(os: &mut Os<'_>, flags: u32, colour: u32) -> Result<(), Fail> {
    let mut regs = Regs::new([flags, colour, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    os.perform(X | SET_COLOUR, &mut regs)
}

/// `rows` set as the pattern `flags` names.
fn set_pattern(os: &mut Os<'_>, flags: u32, rows: &[u32; 8]) -> Result<(), Fail> {
    let mut bytes = [0u8; 32];
    for (chunk, row) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(rows) {
        *chunk = row.to_le_bytes();
    }
    os.below_supervisor_stack(32, |os, at| {
        os.write_bytes(at, &bytes)?;
        set(os, flags | set_colour::PATTERN, at)
    })
}
