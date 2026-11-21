//! Mode strings, as RISC OS 5's `OS_ScreenMode` 13 to 15 read and write
//! them: attributes set apart by spaces or commas, perhaps after a base
//! mode's number — `X` and `Y` the size, `C` the colours or `G` the greys,
//! `F` the frame rate, `EX` and `EY` the eigen factors, `T` with `TX` and
//! `TY` a Teletext mode's colours and characters, `L` a direct-colour
//! pixel's layout. The variables they set follow the selector's own words
//! in the order RISC OS 5 lays them: the colours, the flags, the eigen
//! factors, the Teletext size.

use alloc::string::String;

use crate::mode::{flag, var, Format};
use crate::select::Selector;

/// What each colours attribute gives: its depth, its highest colour and its
/// flags.
const COLOURS: [(&[u8], u32, u32, u32); 12] = [
    (b"2", 0, 1, 0),
    (b"4", 1, 3, 0),
    (b"16", 2, 15, 0),
    (b"64", 3, 63, 0),
    (b"256", 3, 255, flag::FULL_PALETTE),
    (b"4K", 4, 0xFFF, 0),
    (b"4T", 4, 0xFFF, 0),
    (b"32K", 4, 0xFFFF, 0),
    (b"32T", 4, 0xFFFF, 0),
    (b"64K", 4, 0xFFFF, flag::FULL_PALETTE),
    (b"64T", 4, 0xFFFF, flag::FULL_PALETTE),
    (b"16M", 5, 0xFFFF_FFFF, 0),
];
/// And each greys attribute.
const GREYS: [(&[u8], u32, u32, u32); 4] = [
    (b"2", 0, 1, flag::GREYSCALE),
    (b"4", 1, 3, flag::GREYSCALE),
    (b"16", 2, 15, flag::GREYSCALE),
    (b"256", 3, 255, flag::GREYSCALE | flag::FULL_PALETTE),
];
/// Each pixel layout's flags.
const LAYOUTS: [(&[u8], u32); 4] = [
    (b"TBGR", 0),
    (b"TRGB", flag::RGB),
    (b"ABGR", flag::ALPHA),
    (b"ARGB", flag::ALPHA | flag::RGB),
];

/// What the attributes have said.
#[derive(Default)]
struct Said {
    base: Option<Format>,
    x: Option<u32>,
    y: Option<u32>,
    colours: Option<(u32, u32, u32)>,
    frame: Option<u32>,
    ex: Option<u32>,
    ey: Option<u32>,
    teletext: Option<(u32, u32)>,
    tx: Option<u32>,
    ty: Option<u32>,
    layout: Option<u32>,
    /// Colours were given as greys.
    greys: bool,
}

/// The selector `text` describes, if it describes one.
pub(crate) fn parse(text: &[u8]) -> Option<Selector> {
    let mut said = Said::default();
    let words = text
        .split(|&b| b == b' ' || b == b',')
        .filter(|w| !w.is_empty());
    for (i, word) in words.enumerate() {
        if i == 0 && word.iter().all(u8::is_ascii_digit) {
            let number = decimal(word)?;
            said.base = Some(Format::numbered(tairix_vdu::mode::numbered(number)?));
            continue;
        }
        let (key, value) = split_key(word)?;
        let once = |slot: &mut Option<u32>, v: u32| -> Option<()> {
            if slot.replace(v).is_some() {
                return None;
            }
            Some(())
        };
        match key {
            b"X" => once(&mut said.x, decimal(value)?)?,
            b"Y" => once(&mut said.y, decimal(value)?)?,
            b"F" => once(&mut said.frame, decimal(value)?)?,
            b"EX" => once(&mut said.ex, eig(value)?)?,
            b"EY" => once(&mut said.ey, eig(value)?)?,
            b"TX" => once(&mut said.tx, decimal(value)?)?,
            b"TY" => once(&mut said.ty, decimal(value)?)?,
            b"C" | b"G" => {
                let table: &[(&[u8], u32, u32, u32)] = if key == b"C" { &COLOURS } else { &GREYS };
                let &(_, depth, ncolour, flags) = table
                    .iter()
                    .find(|(name, ..)| name.eq_ignore_ascii_case(value))?;
                if said.colours.replace((depth, ncolour, flags)).is_some()
                    || said.teletext.is_some()
                {
                    return None;
                }
                said.greys = key == b"G";
            }
            b"T" => {
                let &(_, depth, ncolour, _) = COLOURS
                    .iter()
                    .find(|(name, ..)| name.eq_ignore_ascii_case(value))?;
                if said.teletext.replace((depth, ncolour)).is_some() || said.colours.is_some() {
                    return None;
                }
            }
            b"L" => {
                let &(_, flags) = LAYOUTS
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(value))?;
                once(&mut said.layout, flags)?;
            }
            _ => return None,
        }
    }
    build(&said)
}

/// The selector what was said makes.
fn build(said: &Said) -> Option<Selector> {
    let base = said.base;
    let width = said
        .x
        .or_else(|| base.map(|b| b.width() << (b.log2_bpc() - b.log2_bpp())))?;
    let height = said.y.or_else(|| base.map(|b| b.height()))?;
    let (log2_bpp, ncolour, mut flags) = match (said.colours, said.teletext) {
        (Some(colours), None) => colours,
        (None, Some((depth, ncolour))) => (depth, ncolour, flag::TELETEXT | flag::GAP),
        _ => {
            let b = base?;
            (
                b.log2_bpp(),
                b.ncolour(),
                b.flags() & (flag::FULL_PALETTE | flag::GREYSCALE),
            )
        }
    };
    if let Some(layout) = said.layout {
        let spare = match (log2_bpp, ncolour) {
            (4, 0xFFF | 0xFFFF) => flags & flag::FULL_PALETTE == 0 || layout & flag::ALPHA == 0,
            (5, _) => true,
            _ => layout == 0,
        };
        if !spare {
            return None;
        }
        flags |= layout;
    }
    let (ex, ey) = match (said.ex, said.ey, base) {
        (None, None, Some(b)) => (Some(b.x_eig()), Some(b.y_eig())),
        (ex, ey, _) => (ex, ey),
    };
    let teletext = said.teletext.is_some();
    let pairs = [
        Some((var::N_COLOUR, ncolour)),
        Some((var::MODE_FLAGS, flags)),
        ex.map(|ex| (var::X_EIG, ex)),
        ey.map(|ey| (var::Y_EIG, ey)),
        said.tx
            .filter(|_| teletext)
            .map(|tx| (var::SCR_R_COL, tx.saturating_sub(1))),
        said.ty
            .filter(|_| teletext)
            .map(|ty| (var::SCR_B_ROW, ty.saturating_sub(1))),
    ];
    Selector::new(width, height, log2_bpp, said.frame.unwrap_or(u32::MAX))
        .with(pairs.into_iter().flatten())
}

/// A mode string for `selector`: its size, its colours, its frame rate and
/// the eigen factors it sets; `None` where there is no room for it.
pub(crate) fn write(selector: &Selector) -> Option<String> {
    let find = |n: u32| {
        selector
            .pairs()
            .iter()
            .rev()
            .find(|(i, _)| *i == n)
            .map(|&(_, v)| v)
    };
    let flags = find(var::MODE_FLAGS).unwrap_or(0);
    let ncolour = find(var::N_COLOUR);
    let depth = selector.log2_bpp;
    let colours = if flags & flag::TELETEXT != 0 {
        COLOURS
            .iter()
            .find(|(_, d, n, _)| *d == depth && Some(*n) == ncolour)
            .map(|(name, ..)| ('T', *name))
    } else if flags & flag::GREYSCALE != 0 {
        GREYS
            .iter()
            .find(|(_, d, ..)| *d == depth)
            .map(|(name, ..)| ('G', *name))
    } else {
        let full = flags & flag::FULL_PALETTE;
        COLOURS
            .iter()
            .find(|(_, d, n, f)| *d == depth && ncolour.is_none_or(|nc| nc == *n) && *f == full)
            .or_else(|| COLOURS.iter().find(|(_, d, ..)| *d == depth))
            .map(|(name, ..)| ('C', *name))
    };
    let layout = LAYOUTS
        .iter()
        .find(|(_, f)| *f != 0 && flags & (flag::RGB | flag::ALPHA) == *f)
        .map(|(name, _)| *name);
    let text = |name: &'static [u8]| core::str::from_utf8(name).unwrap_or_default();
    tairix_util::fallible::written(|out| {
        write!(out, "X{} Y{}", selector.width, selector.height)?;
        if let Some((letter, name)) = colours {
            write!(out, " {letter}{}", text(name))?;
        }
        if let Some(name) = layout {
            write!(out, " L{}", text(name))?;
        }
        if let Some(ex) = find(var::X_EIG) {
            write!(out, " EX{ex}")?;
        }
        if let Some(ey) = find(var::Y_EIG) {
            write!(out, " EY{ey}")?;
        }
        if selector.frame_rate != u32::MAX {
            write!(out, " F{}", selector.frame_rate)?;
        }
        Ok(())
    })
}

/// An attribute's letters and the value after them.
fn split_key(word: &[u8]) -> Option<(&'static [u8], &[u8])> {
    for key in [
        b"EX".as_slice(),
        b"EY",
        b"TX",
        b"TY",
        b"X",
        b"Y",
        b"C",
        b"G",
        b"F",
        b"T",
        b"L",
    ] {
        let matches = word
            .get(..key.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(key));
        if matches && word.len() > key.len() {
            return Some((key, &word[key.len()..]));
        }
    }
    None
}

fn decimal(digits: &[u8]) -> Option<u32> {
    if digits.is_empty() || digits.len() > 9 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(digits.iter().fold(0, |n, &d| n * 10 + u32::from(d - b'0')))
}

fn eig(digits: &[u8]) -> Option<u32> {
    decimal(digits).filter(|&e| e <= 3)
}

#[cfg(test)]
mod tests;
