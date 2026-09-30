//! The one geometry the authentication surface paints, hit-tests, and reports
//! damage against.
//!
//! The body — the account tiles, or the chosen account's disc, name, and
//! secret field — is centred on the screen, and the chrome is one row along
//! the top of the screen: what the machine is in the left corner, the clock
//! in the right. The row depends on the screen alone, so choosing an account
//! leaves it where it was.
//!
//! Every length is authored in *logical* pixels at the reference density and
//! converted through the one shared [`Scale`], so the composition is the same
//! at any DPI. Each band is a fixed logical height with its text centred
//! inside it, which is what lets [`crate::panel_rect`] answer where the
//! prompt is without measuring a font.

use tairix_controls::{paint_run, run_width};
use tairix_font::{BitmapFont, TextShadow};
use tairix_geometry::{Rect, Scale};
use tairix_raster::{Color, Surface};
use tairix_theme::Rgba;

/// Gap from the top and sides of the screen to the chrome's two lines, and
/// between them.
pub(crate) const CHROME_INSET: u32 = 16;

/// The chrome row's band.
const CHROME_BAND: u32 = 18;

/// The least gap between the chrome row and a body a short screen pushes up
/// against it.
const BODY_GAP: u32 = 28;

/// The smallest body the chrome will give up room for.
///
/// The prompt body, because asking for a secret is what the screen is *for*:
/// a screen that cannot hold the clock and still show the prompt keeps the
/// prompt.
const MIN_BODY: u32 = PROMPT_BODY;

/// The chosen account's disc, on the prompt.
pub(crate) const AVATAR_SIDE: u32 = 88;

/// Gap between that disc and the account name under it.
pub(crate) const AVATAR_GAP: u32 = 14;

/// The account name's band.
pub(crate) const NAME_BAND: u32 = 26;

/// Gap between the account name and the prompt block under it.
pub(crate) const NAME_GAP: u32 = 18;

/// The prompt block's width.
///
/// Wider than the field, because the notice under it is prose and a block
/// only as wide as the pill would cut it short.
const PROMPT_WIDTH: u32 = 420;

/// The secret field's width, centred in the block.
pub(crate) const FIELD_WIDTH: u32 = 320;

/// Gap between the secret field and the notice under it.
const NOTICE_GAP: u32 = 10;

/// The notice line's band.
pub(crate) const NOTICE_BAND: u32 = 20;

/// Gap between the notice and the step-back line.
const BACK_GAP: u32 = 6;

/// The step-back line's band.
const BACK_BAND: u32 = 18;

/// The prompt block's height: the secret field, the notice, and the
/// step-back line.
///
/// The field's own row height comes from the theme's control metric, so the
/// block reserves comfortably more than the shipped one needs and every line
/// under the field is placed against the field's *actual* rectangle and
/// dropped if the block runs out — the block therefore always contains
/// everything drawn in it, whatever the theme.
pub(crate) const PROMPT_HEIGHT: u32 = 96;

/// The prompt body's height: the disc, the account name, and the block.
pub(crate) const PROMPT_BODY: u32 = AVATAR_SIDE + AVATAR_GAP + NAME_BAND + NAME_GAP + PROMPT_HEIGHT;

/// Gap between the tile grid and the chooser's one hint line.
pub(crate) const CHOOSER_HINT_GAP: u32 = 20;

/// Margin kept clear at each side of the screen, so a wide row of tiles
/// never runs to the very edge.
pub(crate) const SIDE_MARGIN: u32 = 32;

/// Where the prompt's three parts sit on the screen.
///
/// One definition, so the disc, the name, and the block cannot drift apart —
/// and so [`crate::panel_rect`], which an embedder asks where the prompt is,
/// is the very block the field is drawn in rather than a second guess at it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Prompt {
    /// The chosen account's disc, centred at the top of the body.
    pub(crate) disc: Rect,
    /// The full-width band the account's name is centred in, so a long name
    /// is not cut to the block's width.
    pub(crate) name: Rect,
    /// The block holding the field and the lines under it.
    pub(crate) block: Rect,
}

impl Prompt {
    /// The prompt's geometry on `screen`.
    pub(crate) fn new(screen: Rect, scale: Scale) -> Self {
        let body_top = body_top(screen, scale, scale.scale_length(PROMPT_BODY));
        let side = scale.scale_length(AVATAR_SIDE).min(screen.width);
        let disc = Rect::new(
            centre_on(screen.origin.x, screen.width, side),
            body_top,
            side,
            side,
        );
        let name = Rect::new(
            screen.origin.x,
            down(down(disc.origin.y, side), scale.scale_length(AVATAR_GAP)),
            screen.width,
            scale.scale_length(NAME_BAND),
        );
        let width = scale.scale_length(PROMPT_WIDTH).min(screen.width);
        let block = Rect::new(
            centre_on(screen.origin.x, screen.width, width),
            down(
                down(name.origin.y, name.height),
                scale.scale_length(NAME_GAP),
            ),
            width,
            scale.scale_length(PROMPT_HEIGHT).min(screen.height),
        );
        Self { disc, name, block }
    }
}

/// The top of a `body_height`-tall body on `screen`: centred on the screen,
/// but never nearer the chrome row than its gap, so a short screen pushes the
/// body down rather than under the clock.
///
/// One definition for both bodies, so each is centred alike.
pub(crate) fn body_top(screen: Rect, scale: Scale, body_height: u32) -> i32 {
    let chrome = chrome_band(screen, scale);
    let floor = if chrome.is_empty() {
        screen.origin.y
    } else {
        down(chrome.bottom(), scale.scale_length(BODY_GAP))
    };
    centre_on(screen.origin.y, screen.height, body_height).max(floor)
}

/// The full-width row along the top of `screen` the chrome is drawn in, or
/// [`Rect::EMPTY`] when the screen has no room for it and a prompt as well.
///
/// A function of the screen and the density alone — never of which body is
/// up — so a chooser taller than a prompt is never the reason the clock
/// disappears.
pub(crate) fn chrome_band(screen: Rect, scale: Scale) -> Rect {
    let inset = scale.scale_length(CHROME_INSET);
    let height = scale.scale_length(CHROME_BAND);
    let needed = inset
        .saturating_add(height)
        .saturating_add(scale.scale_length(BODY_GAP))
        .saturating_add(scale.scale_length(MIN_BODY));
    if screen.height < needed {
        return Rect::EMPTY;
    }
    Rect::new(
        screen.origin.x,
        down(screen.origin.y, inset),
        screen.width,
        height,
    )
}

/// Where the chrome's two lines go in `row`: the identity from the left
/// inset, and the clock, `clock_width` wide, against the right one.
///
/// The identity has only what the clock leaves, so a narrow screen cuts the
/// machine's name before the time.
pub(crate) fn chrome_lines(row: Rect, scale: Scale, clock_width: u32) -> (Rect, Rect) {
    let inset = scale.scale_length(CHROME_INSET);
    let inner = row.width.saturating_sub(inset.saturating_mul(2));
    let clock = clock_width.min(inner);
    let left = down(row.origin.x, inset);
    let identity = match clock {
        0 => inner,
        _ => inner.saturating_sub(clock).saturating_sub(inset),
    };
    (
        Rect::new(left, row.origin.y, identity, row.height),
        Rect::new(down(left, inner - clock), row.origin.y, clock, row.height),
    )
}

/// `extent` centred within `[origin, origin + available)`.
pub(crate) fn centre_on(origin: i32, available: u32, extent: u32) -> i32 {
    down(origin, available.saturating_sub(extent) / 2)
}

/// `origin` moved on by `offset` pixels, saturating rather than wrapping.
pub(crate) fn down(origin: i32, offset: u32) -> i32 {
    origin.saturating_add(i32::try_from(offset).unwrap_or(i32::MAX))
}

/// The band the notice sits in: under `field`, across the prompt `block`.
///
/// `None` when the block has no room left for it, which is what keeps the
/// block containing everything drawn in it however tall the theme's own
/// control row turns out to be.
pub(crate) fn notice_band(block: Rect, field: Rect, scale: Scale) -> Option<Rect> {
    confine(
        Rect::new(
            block.origin.x,
            down(
                down(field.origin.y, field.height),
                scale.scale_length(NOTICE_GAP),
            ),
            block.width,
            scale.scale_length(NOTICE_BAND),
        ),
        block,
    )
}

/// The band the step-back line sits in, under the `notice` band.
pub(crate) fn back_band(block: Rect, notice: Rect, scale: Scale) -> Option<Rect> {
    confine(
        Rect::new(
            block.origin.x,
            down(
                down(notice.origin.y, notice.height),
                scale.scale_length(BACK_GAP),
            ),
            block.width,
            scale.scale_length(BACK_BAND),
        ),
        block,
    )
}

/// `band` itself when it fits inside `bounds`, or `None` when it does not.
fn confine(band: Rect, bounds: Rect) -> Option<Rect> {
    let top = band.origin.y;
    let bottom = down(bounds.origin.y, bounds.height);
    if band.height == 0 || top < bounds.origin.y || down(top, band.height) > bottom {
        return None;
    }
    Some(band)
}

/// Where a line sits across the band it is drawn in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Align {
    /// Against the band's left edge.
    Start,
    /// Centred in the band.
    Centre,
    /// Against the band's right edge.
    End,
}

/// Draw `text` in `band`, placed across it by `align`, in `ink`, elided with
/// the shared mark where it is wider than the band, over `shadow` where the
/// ground is a picture rather than a known colour.
///
/// The one line-drawing definition the surface shares, so every line fits
/// and takes its shadow alike. A band shorter than the font's line box draws
/// nothing rather than letting text spill past the rectangle the surface
/// reports as damaged.
pub(crate) fn draw_line(
    surface: &mut Surface,
    band: Rect,
    text: &str,
    font: BitmapFont,
    ink: Rgba,
    shadow: Option<TextShadow>,
    align: Align,
) {
    let line = font.line_height();
    if text.is_empty() || band.width == 0 || line > band.height {
        return;
    }
    let run = font.elide_to_width(text, band.width);
    let width = run_width(font, run);
    let x = match align {
        Align::Start => band.origin.x,
        Align::Centre => centre_on(band.origin.x, band.width, width),
        Align::End => down(band.origin.x, band.width.saturating_sub(width)),
    };
    let y = down(band.origin.y, (band.height - line) / 2);
    paint_run(surface, font, run, (x, y), Color::from(ink), shadow);
}
