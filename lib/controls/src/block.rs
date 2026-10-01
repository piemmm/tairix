//! The titled-block anatomy: a bordered, slightly-lighter plate under a
//! small-caps accent title with a hairline rule (`plans/switchboard/02-cpu.png`,
//! `01-tasks.png`, `08-recovery.png`).
//!
//! The Switchboard's panes and the System Monitor screensaver's panels are
//! both built from blocks, and the boards draw all of them the same way.
//! Defining that once is what stops two surfaces reading as two products.
//!
//! Not a control: [`Panel`](crate::collection::Panel) is a different anatomy
//! (a header band at control height, a dominant rail, a signal bead, an
//! actions row). This is composition over the shared plate primitives.

use tairix_font::BitmapFont;
use tairix_geometry::{to_i32, Rect, Scale};
use tairix_raster::{Color, Surface};
use tairix_theme::{TextRole, Theme};

use crate::paint::{inset, paint_run, paint_surface_plate, plate_border, ChromeLayer};

/// Paint a block's plate over `bounds`, answering the rectangle its content
/// draws inside — or [`None`] when `bounds` is too small to seat one.
///
/// The plate is a hairline rim at the shared plate border over a ground one
/// step lighter than the surface it stands on. It counts as a raised plate,
/// so on a floating theme it reads as an object on the glass instead of
/// dissolving into it.
///
/// The plate is inset from `bounds` by [`plate_margin`], so `bounds` is the
/// block's *slot*: two blocks in adjacent slots leave a gap between their rims
/// instead of sharing an edge.
pub fn plate(surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) -> Option<Rect> {
    let slot = plate_rect(bounds, scale, theme)?;
    let (x, y) = slot.surface_origin()?;
    let (w, h) = (slot.width, slot.height);
    let radius = scale
        .scale_length(theme.metrics().control_corner_radius)
        .min(w / 2)
        .min(h / 2);
    paint_surface_plate(
        surface,
        (x, y, w, h),
        (radius, plate_border(theme, scale)),
        theme,
        (theme.palette().surface_raised, ChromeLayer::Plate),
    )?;
    content_rect(bounds, scale, theme)
}

/// The margin a plate is inset from its slot.
///
/// Half the control gap on every side, so two plates in adjacent slots leave
/// one whole control gap between their rims, the same in both directions.
#[must_use]
pub fn plate_margin(scale: Scale, theme: &Theme) -> u32 {
    scale.scale_length(theme.metrics().control_gap).max(2) / 2
}

/// The rectangle a plate over `bounds` occupies: the slot less its margin.
fn plate_rect(bounds: Rect, scale: Scale, theme: &Theme) -> Option<Rect> {
    let (x, y) = bounds.surface_origin()?;
    let (ix, iy, iw, ih) = inset(
        x,
        y,
        bounds.width,
        bounds.height,
        plate_margin(scale, theme),
    )?;
    Some(Rect::new(to_i32(ix), to_i32(iy), iw, ih))
}

/// The rectangle a block's content occupies inside a plate over `bounds`,
/// without drawing anything, so a hit test or a layout made before the paint
/// cannot disagree with it.
#[must_use]
pub fn content_rect(bounds: Rect, scale: Scale, theme: &Theme) -> Option<Rect> {
    let (x, y) = bounds.surface_origin()?;
    let (ix, iy, iw, ih) = inset(
        x,
        y,
        bounds.width,
        bounds.height,
        content_inset(scale, theme),
    )?;
    Some(Rect::new(to_i32(ix), to_i32(iy), iw, ih))
}

/// The rectangle a titled block's content occupies: its plate's interior less
/// the band [`title`] claims.
#[must_use]
pub fn titled_content(bounds: Rect, scale: Scale, theme: &Theme) -> Option<Rect> {
    let inner = content_rect(bounds, scale, theme)?;
    let band = title_height(scale, theme);
    let height = inner.height.checked_sub(band)?;
    (height > 0).then(|| {
        Rect::new(
            inner.left(),
            inner.top().saturating_add(to_i32(band)),
            inner.width,
            height,
        )
    })
}

/// How tall a title and its hairline rule stand: the header line, then the
/// rule with half a control gap either side of it. [`title`] advances by
/// exactly this.
#[must_use]
pub fn title_height(scale: Scale, theme: &Theme) -> u32 {
    let gap = scale.scale_length(theme.metrics().control_gap).max(1) / 2;
    title_font(theme, scale)
        .line_height()
        .saturating_add(gap.saturating_mul(2))
        .saturating_add(plate_border(theme, scale))
}

/// The face a block's title is set in: the role a header over a list takes,
/// bold and below body size.
fn title_font(theme: &Theme, scale: Scale) -> BitmapFont {
    BitmapFont::for_role(theme.fonts(), TextRole::SectionHeader, scale)
}

/// How far a plate's content sits inside its slot: the plate's own margin,
/// then its rim, then the theme's control padding.
#[must_use]
pub fn content_inset(scale: Scale, theme: &Theme) -> u32 {
    plate_margin(scale, theme)
        .saturating_add(plate_border(theme, scale))
        .saturating_add(scale.scale_length(theme.metrics().control_inset).max(1))
}

/// Draw a block's title at the top of `rect` and the hairline rule under it,
/// answering the y its content starts at.
///
/// The accent is what makes a block's name read as a label rather than as one
/// more reading. A block whose content brings its own plates takes
/// [`bare_title`] instead, since its cells' rims already separate the two.
pub fn title(surface: &mut Surface, rect: Rect, scale: Scale, theme: &Theme, text: &str) -> i32 {
    let y = bare_title(surface, rect, scale, theme, text);
    let thickness = plate_border(theme, scale);
    let gap = scale.scale_length(theme.metrics().control_gap).max(1) / 2;
    let top = y.saturating_add(to_i32(gap));
    if let (Ok(x), Ok(ry)) = (u32::try_from(rect.left()), u32::try_from(top)) {
        if top.saturating_add(to_i32(thickness)) <= rect.bottom() {
            surface.fill_rect(
                x,
                ry,
                rect.width,
                thickness,
                Color::from(theme.palette().border),
            );
        }
    }
    top.saturating_add(to_i32(thickness.saturating_add(gap)))
}

/// Draw a block's title with no rule under it, answering the y its content
/// starts at. A title longer than the block is cut with the shared mark.
pub fn bare_title(
    surface: &mut Surface,
    rect: Rect,
    scale: Scale,
    theme: &Theme,
    text: &str,
) -> i32 {
    let font = title_font(theme, scale);
    paint_run(
        surface,
        font,
        font.elide_to_width(text, rect.width),
        (rect.left(), rect.top()),
        Color::from(theme.palette().accent),
        None,
    );
    rect.top()
        .saturating_add(to_i32(font.line_height().min(rect.height)))
}
