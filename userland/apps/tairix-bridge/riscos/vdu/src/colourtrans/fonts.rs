//! The colours a font is painted in, as RISC OS 5 picks them. In a mode of
//! up to sixteen colours: the background's and foreground's matches, and the
//! longest run of colour numbers stepping away from the foreground's, up or
//! down, each lying within a step of the shade it stands for. In a deeper
//! mode the colours are left for the Font Manager to set from the words.

use tairix_colour::Rgb;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::font::{SET_FONT_COLOURS, SET_PALETTE};
use tairix_riscos_kernel::swi::X;

use super::matching::{Loadings, Look};
use super::palettes::Against;
use super::{current, own, BAD_FONT_OFFSET};

/// The longest offset a run reaches, and the furthest either way a deeper
/// mode takes.
const LONGEST: i32 = 14;

/// What a font is painted in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Chosen {
    /// The background's colour number, the foreground's first, and the
    /// offset to the last.
    Run {
        background: u32,
        foreground: u32,
        offset: i32,
    },
    /// A mode of 256 colours or more: the words stand.
    Deep,
}

/// `ColourTrans_ReturnFontColours`: R1 and R2 the background's and
/// foreground's words, R3 the longest offset wanted.
pub(super) fn return_font_colours(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match chosen(os, regs)? {
        Some(chosen) => give(os, regs, chosen).map(drop),
        None => Ok(()),
    }
}

/// `ColourTrans_SetFontColours`: as `ReturnFontColours`, then the colours
/// set through the Font Manager, by `Font_SetPalette` in a deeper mode.
pub(super) fn set_font_colours(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let words = [regs.r[1], regs.r[2]];
    let Some(chosen) = chosen(os, regs)? else {
        return Ok(());
    };
    let deep = give(os, regs, chosen)?;
    let mut call = Regs::default();
    call.r[..4].copy_from_slice(&regs.r[..4]);
    let swi = if deep {
        call.r[1..3].copy_from_slice(&[0, 0]);
        call.r[4..6].copy_from_slice(&words);
        SET_PALETTE
    } else {
        SET_FONT_COLOURS
    };
    os.perform(X | swi, &mut call)
}

/// The colours chosen for R1 to R3; none while ColourTrans holds nothing.
fn chosen(os: &mut Os<'_>, regs: &Regs) -> Result<Option<Chosen>, Fail> {
    let [background, foreground] = [regs.r[1], regs.r[2]].map(Rgb::from_riscos_word);
    let longest = regs.r[3].cast_signed();
    Ok(current(os)?
        .map(|(against, loadings)| choose(against, background, foreground, longest, loadings)))
}

/// `chosen` given in R1 to R3; whether the mode is a deeper one. A deeper
/// mode's offset beyond the Font Manager's range is refused.
fn give(os: &mut Os<'_>, regs: &mut Regs, chosen: Chosen) -> Result<bool, Fail> {
    match chosen {
        Chosen::Run {
            background,
            foreground,
            offset,
        } => {
            regs.r[1..4].copy_from_slice(&[background, foreground, offset.cast_unsigned()]);
            Ok(false)
        }
        Chosen::Deep if (-LONGEST..=LONGEST).contains(&regs.r[3].cast_signed()) => {
            regs.r[2] = 0;
            Ok(true)
        }
        Chosen::Deep => Err(os.own_error(own, BAD_FONT_OFFSET, &[])),
    }
}

/// What `background` and `foreground` are painted in among `against`'s
/// colours, the run at most `longest` long.
fn choose(
    against: &Against,
    background: Rgb,
    foreground: Rgb,
    longest: i32,
    loadings: Loadings,
) -> Chosen {
    let colours = match against.palette() {
        Some((log2_bpp, colours)) if log2_bpp <= 2 => colours,
        _ => return Chosen::Deep,
    };
    let first = against.colour_number(foreground, loadings, Look::Nearest);
    let shades = Shades {
        colours,
        background,
        foreground,
        first,
        loadings,
    };
    let most = longest.clamp(0, LONGEST);
    let [down, up] = [1, -1].map(|step| shades.longest_run(step, most));
    let (step, run) = if down > up { (1, down) } else { (-1, up) };
    Chosen::Run {
        background: against.colour_number(background, loadings, Look::Nearest),
        foreground: first.wrapping_add_signed(-step * run),
        offset: step * run,
    }
}

/// The colours a run is sought among, and what it runs between.
struct Shades<'a> {
    colours: &'a [Rgb],
    background: Rgb,
    foreground: Rgb,
    /// The foreground's colour number, where every run ends.
    first: u32,
    loadings: Loadings,
}

impl Shades<'_> {
    /// The longest run, at most `most`, stepping `step` down the colour
    /// numbers from the foreground's.
    fn longest_run(&self, step: i32, most: i32) -> i32 {
        (1..=most)
            .rev()
            .find(|&run| self.fits(step, run))
            .unwrap_or(0)
    }

    /// Whether the `run` colours stepping `step` from the foreground's each
    /// lie nearer than a whole step to the shade they stand for.
    fn fits(&self, step: i32, run: i32) -> bool {
        let span = run + 1;
        let [fg, bg] = [self.foreground, self.background].map(|c| c.to_array().map(i32::from));
        let reach: [i32; 3] = core::array::from_fn(|c| ((bg[c] - fg[c]) / span).abs());
        let [r, g, b] = reach.map(|c| u8::try_from(c).unwrap_or(u8::MAX));
        let threshold = self.loadings.error(Rgb::new(r, g, b), Rgb::BLACK);
        (1..=run).all(|k| {
            let number = i64::from(self.first) - i64::from(step * k);
            let Some(&colour) = usize::try_from(number)
                .ok()
                .and_then(|n| self.colours.get(n))
            else {
                return false;
            };
            let shade: [u8; 3] = core::array::from_fn(|c| {
                let ideal = fg[c] + ((bg[c] - fg[c]) * k).div_euclid(span);
                u8::try_from(ideal).unwrap_or(u8::MAX)
            });
            let [r, g, b] = shade;
            self.loadings.error(colour, Rgb::new(r, g, b)) < threshold
        })
    }
}

#[cfg(test)]
mod tests;
