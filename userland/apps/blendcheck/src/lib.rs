//! TAIRiX `blendcheck` — the display engine's blend of translucency beside
//! the desktop's own (`plans/GPU.md` GD3).
//!
//! One screen: a grey ramp across the width and, one above another, three
//! panes of dark glass over it, each blended a different way:
//!
//! 1. by the desktop, dithered, as the compositor composes translucency;
//! 2. by the desktop, rounded to nearest — what banding looks like;
//! 3. by the display engine, handed the glass as a layer of its own.
//!
//! An engine whose blend does not band shows the third pane like the first;
//! one whose blend does shows it like the second. That comparison, judged on
//! the real panel, is what decides whether an engine may state
//! `AccelCaps::band_free_blend`.
//!
//! This crate builds the pattern; `src/run.rs` takes the display and shows
//! it. `no_std` with `alloc`, no `unsafe`, and no `unwrap`/`expect`/`panic!`
//! in production paths: a refused allocation is a `None`.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::driver::display::{AccelCaps, AccelLayer, BufferId, DamageRect, DisplayMode};
use tairix_display::{scanout_len, ChannelOrder};
use tairix_raster::{blend_span, DitherRow, Pixel};
use tairix_util::fallible;
use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};
use tairix_util::quote::quote;

/// The usage banner the short-help switches print when the Help tree is
/// unavailable.
pub const USAGE: &str = "usage: blendcheck [-h | -?]";

/// One thing the `blendcheck` tool can do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    /// Show the pattern until a key or a button is pressed.
    Run,
    /// Render the tool's own short help.
    Help,
}

/// Why [`parse`] refused a command line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlendcheckError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// An operand; the tool takes none.
    ExtraOperand(String),
}

impl fmt::Display for BlendcheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::ExtraOperand(operand) => {
                write!(f, "extra operand {operand}", operand = quote(operand))
            }
        }
    }
}

/// The reserved help switches, which are all the tool takes.
const OPTIONS: &[Opt<()>] = &[
    Opt::flag('h', &["help"], ()),
    Opt::letter('?', Takes::Nothing, ()),
];

/// Parse `args` (excluding the program name) into a [`Command`].
///
/// # Errors
///
/// * [`BlendcheckError::Option`] — an option `getopt` refuses.
/// * [`BlendcheckError::ExtraOperand`] — an operand.
pub fn parse(args: &[&str]) -> Result<Command, BlendcheckError> {
    let mut operand = None;
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(BlendcheckError::Option)? {
            Arg::Opt((), _) => return Ok(Command::Help),
            Arg::Operand(found) => {
                operand.get_or_insert(found);
            }
        }
    }
    match operand {
        Some(operand) => Err(BlendcheckError::ExtraOperand(String::from(operand))),
        None => Ok(Command::Run),
    }
}

/// The glass every pane lays over the ramp: black, covering 208 of 255, so
/// the full ramp beneath it keeps 48 levels — a step every forty-odd pixels
/// across a 1920-pixel screen, wide enough to see wherever nothing dithers it.
pub const GLASS: Pixel = Pixel {
    r: 0,
    g: 0,
    b: 0,
    a: 208,
};

/// How a pane's glass is blended, top to bottom.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pane {
    /// By the desktop, with the compositor's ordered dither.
    Dithered,
    /// By the desktop, rounded to nearest.
    Rounded,
    /// By the display engine.
    Engine,
}

impl Pane {
    /// The panes top to bottom.
    pub const ALL: [Self; 3] = [Self::Dithered, Self::Rounded, Self::Engine];
}

/// The pattern for one display mode.
pub struct Pattern {
    /// The output frame, laid out as the mode: the ramp, with the first two
    /// panes' glass blended in and the engine's pane left bare beneath its
    /// layer.
    pub frame: Vec<u8>,
    /// The engine's glass, one pane of [`GLASS`] in the mode's byte order,
    /// rows `4 × width` bytes apart.
    pub glass: Vec<u8>,
    /// Where each pane of [`Pane::ALL`] lies on the output.
    pub panes: [DamageRect; 3],
}

/// The ramp's grey at column `x` of a `width`-pixel screen, `width >= 2`.
fn ramp(x: u32, width: u32) -> Pixel {
    let span = u64::from(width.saturating_sub(1)).max(1);
    let level = (u64::from(x) * 255 + span / 2) / span;
    let level = u8::try_from(level).unwrap_or(u8::MAX);
    Pixel {
        r: level,
        g: level,
        b: level,
        a: u8::MAX,
    }
}

/// Where the panes lie on a `width` × `height` screen: each the middle half of
/// a third of the height, across the whole width, so the bare ramp shows above
/// and below every pane. `None` on a screen too small to hold them.
fn panes(width: u32, height: u32) -> Option<[DamageRect; 3]> {
    let band = height / 3;
    let pane = band / 2;
    if width < 2 || pane == 0 {
        return None;
    }
    let at = |index: u32| DamageRect {
        x: 0,
        y: index * band + band / 4,
        width_px: width,
        height_px: pane,
    };
    Some([at(0), at(1), at(2)])
}

/// The pane screen row `y` lies in, if any.
fn pane_at(panes: &[DamageRect; 3], y: u32) -> Option<Pane> {
    Pane::ALL.into_iter().zip(panes).find_map(|(pane, rect)| {
        (rect.y..rect.y.saturating_add(rect.height_px))
            .contains(&y)
            .then_some(pane)
    })
}

/// The pattern for `mode`, or `None` for a mode with no software byte order,
/// a screen too small for the panes, or a heap that refuses the frame.
#[must_use]
pub fn pattern(mode: &DisplayMode) -> Option<Pattern> {
    let order = ChannelOrder::for_format(mode.format)?;
    let (width, height) = (mode.width_px, mode.height_px);
    let panes = panes(width, height)?;
    let cols = usize::try_from(width).ok()?;
    let row_bytes = cols.checked_mul(4)?;
    let stride = usize::try_from(mode.stride_bytes)
        .ok()
        .filter(|stride| *stride >= row_bytes)?;
    let mut frame = fallible::filled(scanout_len(mode)?, 0u8)?;
    let bare = fallible::collected(cols, (0..width).map(|x| ramp(x, width)))?;
    let glass_row = fallible::filled(cols, GLASS)?;
    let mut row = fallible::filled(cols, Pixel::TRANSPARENT)?;
    for (y, line) in (0..height).zip(frame.chunks_mut(stride)) {
        row.copy_from_slice(&bare);
        match pane_at(&panes, y) {
            Some(Pane::Dithered) => blend_span(&mut row, &glass_row, 255, DitherRow::at(y), 0),
            Some(Pane::Rounded) => blend_span(&mut row, &glass_row, 255, DitherRow::NEAREST, 0),
            Some(Pane::Engine) | None => {}
        }
        if order.encode_run(&row, line.get_mut(..row_bytes)?) != cols {
            return None;
        }
    }
    let pane_rows = usize::try_from(panes[2].height_px).ok()?;
    let mut glass = fallible::filled(row_bytes.checked_mul(pane_rows)?, 0u8)?;
    for line in glass.chunks_mut(row_bytes) {
        if order.encode_run(&glass_row, line) != cols {
            return None;
        }
    }
    Some(Pattern {
        frame,
        glass,
        panes,
    })
}

impl Pattern {
    /// The engine's pane, which its layer is shown at.
    #[must_use]
    pub const fn engine_pane(&self) -> DamageRect {
        self.panes[2]
    }

    /// Whether an engine of `caps` can show the glass as one layer.
    #[must_use]
    pub const fn fits(&self, caps: &AccelCaps) -> bool {
        let pane = self.engine_pane();
        pane.width_px <= caps.max_width_px && pane.height_px <= caps.max_height_px
    }

    /// Copy the glass into a layer buffer whose rows are `stride` bytes
    /// apart, answering whether `bytes` held it all.
    pub fn lay_glass(&self, bytes: &mut [u8], stride: u32) -> bool {
        let pane = self.engine_pane();
        let (Ok(stride), Ok(rows), Ok(cols)) = (
            usize::try_from(stride),
            usize::try_from(pane.height_px),
            usize::try_from(pane.width_px),
        ) else {
            return false;
        };
        let row_bytes = cols.saturating_mul(4);
        if row_bytes == 0
            || self.glass.len() != row_bytes.saturating_mul(rows)
            || stride < row_bytes
            || bytes.len() < stride.saturating_mul(rows)
        {
            return false;
        }
        for (dst, src) in bytes.chunks_mut(stride).zip(self.glass.chunks(row_bytes)) {
            let Some(dst) = dst.get_mut(..row_bytes) else {
                return false;
            };
            dst.copy_from_slice(src);
        }
        true
    }

    /// The layer showing buffer `buffer`, the laid glass, over the engine's
    /// pane.
    #[must_use]
    pub fn engine_layer(&self, buffer: BufferId) -> AccelLayer {
        let pane = self.engine_pane();
        AccelLayer {
            buffer,
            src: DamageRect {
                x: 0,
                y: 0,
                width_px: pane.width_px,
                height_px: pane.height_px,
            },
            dst_x: i32::try_from(pane.x).unwrap_or(i32::MAX),
            dst_y: i32::try_from(pane.y).unwrap_or(i32::MAX),
            dst_width_px: pane.width_px,
            dst_height_px: pane.height_px,
            opacity: u8::MAX,
        }
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
