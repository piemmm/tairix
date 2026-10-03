//! A grid of colour wells with a primary mark and an optional secondary one:
//! a scheme editor's wells, an image editor's palette.
//!
//! Every length is authored logically and converted through [`Scale`], every
//! colour and radius comes from the active [`Theme`], and a mark reads by
//! shape as well as by colour, because a well's own colour cannot be relied
//! on for contrast. A translucent well is shown over a checker, so a colour
//! that is partly or wholly transparent is not mistaken for an opaque one.
//!
//! The wells share the bounds they are drawn in evenly, row by row, so one
//! layout serves drawing, hit-testing and the damage a mark's move reports.

use alloc::vec::Vec;

use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::{Color, Ring, RingInk, Surface, SUBPIXEL};
use tairix_theme::Theme;

use crate::checker::Checker;
use crate::damage;
use crate::paint::{inset, paint_bead, plate_border, resolve_bead, surface_rect, withheld};
use crate::state::{ControlDisposition, ControlState, RenderInvariant};

/// Which of a grid's two marks an interaction moves.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SwatchMark {
    /// The selection a grid carries.
    Primary,
    /// A second choice an owner may track beside it, such as a background
    /// colour beside a foreground one.
    Secondary,
}

/// What routing an input event into a [`SwatchGrid`] concluded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SwatchAction {
    /// `mark` now sits on the well at `index`.
    Selected {
        /// The mark that moved.
        mark: SwatchMark,
        /// The zero-based index of the well it moved to.
        index: usize,
    },
}

/// A grid of colour wells.
///
/// Equal grids draw the same pixels: the pointer and the press latch are
/// hit-testing bookkeeping no render path reads.
#[derive(Clone, Debug, PartialEq)]
pub struct SwatchGrid {
    colours: Vec<Color>,
    columns: usize,
    primary: Option<usize>,
    secondary: Option<usize>,
    state: ControlState,
    pointer: RenderInvariant<Point>,
    /// The well a primary press armed, and the mark it will move if the
    /// release lands over the same well.
    armed: RenderInvariant<Option<(usize, SwatchMark)>>,
}

impl SwatchGrid {
    /// Wells of `colours`, `columns` to a row, with the first selected and
    /// no secondary mark.
    #[must_use]
    pub fn new(columns: usize, colours: Vec<Color>) -> Self {
        let primary = (!colours.is_empty()).then_some(0);
        Self {
            colours,
            columns: columns.max(1),
            primary,
            secondary: None,
            state: ControlState::idle(),
            pointer: RenderInvariant::new(Point::ORIGIN),
            armed: RenderInvariant::new(None),
        }
    }

    /// How many wells the grid holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.colours.len()
    }

    /// Whether the grid holds no wells.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.colours.is_empty()
    }

    /// Wells to a row.
    #[must_use]
    pub const fn columns(&self) -> usize {
        self.columns
    }

    /// Rows the wells fill.
    #[must_use]
    pub fn rows(&self) -> usize {
        self.colours.len().div_ceil(self.columns)
    }

    /// The colour of well `index`.
    #[must_use]
    pub fn colour(&self, index: usize) -> Option<Color> {
        self.colours.get(index).copied()
    }

    /// Set well `index`'s colour; an out-of-range index is ignored. The owner
    /// reports the repaint, as for every owner-committed value.
    pub fn set_colour(&mut self, index: usize, colour: Color) {
        if let Some(well) = self.colours.get_mut(index) {
            *well = colour;
        }
    }

    /// Show `colours`, `columns` to a row, keeping each mark and any press
    /// in progress where they still name a well: the colours can change
    /// under a user still choosing one.
    pub fn adopt_colours(&mut self, columns: usize, colours: Vec<Color>) {
        self.colours = colours;
        self.columns = columns.max(1);
        let len = self.colours.len();
        self.primary = self.primary.filter(|&index| index < len);
        self.secondary = self.secondary.filter(|&index| index < len);
        *self.armed = self.armed.filter(|&(index, _)| index < len);
    }

    /// The well the primary mark is on, if any is marked.
    #[must_use]
    pub const fn selected(&self) -> Option<usize> {
        self.primary
    }

    /// The well the secondary mark is on, if it is shown.
    #[must_use]
    pub const fn secondary(&self) -> Option<usize> {
        self.secondary
    }

    /// Put the primary mark on well `index`, or on none, without reporting,
    /// for an owner whose choice is no longer one of the wells; an
    /// out-of-range index marks none.
    pub fn adopt_selected(&mut self, index: Option<usize>) {
        self.primary = index.filter(|&index| index < self.colours.len());
    }

    /// Show the secondary mark on well `index`, or not at all, without
    /// reporting; an out-of-range index hides it.
    pub fn adopt_secondary(&mut self, index: Option<usize>) {
        self.secondary = index.filter(|&index| index < self.colours.len());
    }

    /// The grid's composed state.
    #[must_use]
    pub const fn state(&self) -> ControlState {
        self.state
    }

    /// Replace the grid's composed state; the owner reports the repaint.
    pub fn set_state(&mut self, state: ControlState) {
        self.state = state;
    }

    /// Set the grid's keyboard focus; the owner reports the repaint.
    pub fn set_focused(&mut self, focused: bool) {
        self.state.focus.focused = focused;
    }

    /// The height the grid needs with each well the theme's control height:
    /// the size for a handful of wells a user edits one at a time.
    #[must_use]
    pub fn preferred_height(&self, scale: Scale, theme: &Theme) -> u32 {
        let side = scale.scale_length(theme.metrics().control_height).max(1);
        let gap = well_gap(scale, theme);
        let rows = u32::try_from(self.rows()).unwrap_or(u32::MAX);
        side.saturating_mul(rows)
            .saturating_add(gap.saturating_mul(rows.saturating_sub(1)))
    }

    /// The height `width` of square wells needs: the size for a palette,
    /// whose wells are as many as fit rather than a comfortable few.
    #[must_use]
    pub fn height_for_width(&self, width: u32) -> u32 {
        let columns = u32::try_from(self.columns).unwrap_or(u32::MAX).max(1);
        let rows = u32::try_from(self.rows()).unwrap_or(u32::MAX);
        (width / columns).saturating_mul(rows)
    }

    /// Where well `index` is drawn within `bounds`.
    #[must_use]
    pub fn cell_rect(&self, bounds: Rect, index: usize) -> Option<Rect> {
        if index >= self.colours.len() || bounds.width == 0 || bounds.height == 0 {
            return None;
        }
        let (cx, cw) = axis_span(index % self.columns, self.columns, bounds.width)?;
        let (cy, ch) = axis_span(index / self.columns, self.rows(), bounds.height)?;
        Some(Rect::new(
            bounds.left().saturating_add(to_i32(cx)),
            bounds.top().saturating_add(to_i32(cy)),
            cw,
            ch,
        ))
    }

    /// The well `point` is over within `bounds`, found directly from the
    /// layout rather than by testing every well.
    #[must_use]
    pub fn well_at(&self, bounds: Rect, point: Point) -> Option<usize> {
        if !bounds.contains(point) {
            return None;
        }
        let x = u32::try_from(i64::from(point.x) - i64::from(bounds.left())).ok()?;
        let y = u32::try_from(i64::from(point.y) - i64::from(bounds.top())).ok()?;
        let column = axis_index(x, self.columns, bounds.width)?;
        let row = axis_index(y, self.rows(), bounds.height)?;
        let index = row.checked_mul(self.columns)?.checked_add(column)?;
        (index < self.colours.len()).then_some(index)
    }

    /// The well the keyboard acts on: the primary mark's, or the first.
    fn cursor(&self) -> Option<usize> {
        self.primary.or((!self.colours.is_empty()).then_some(0))
    }

    /// Paint the grid into `surface` at `bounds`.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        if withheld(surface, bounds) {
            return;
        }
        let focus = self.state.focus.focused.then(|| self.cursor()).flatten();
        for index in 0..self.colours.len() {
            let Some((x, y, w, h)) = self.cell_rect(bounds, index).and_then(surface_rect) else {
                continue;
            };
            if surface.admits(x, y, w, h) {
                self.paint_well(
                    surface,
                    index,
                    (x, y, w, h),
                    focus == Some(index),
                    (scale, theme),
                );
            }
        }
        if let Some((colour, shape)) = resolve_bead(theme, self.state) {
            if let Some((x, y, w, _)) = surface_rect(bounds) {
                let size = scale.scale_length(theme.metrics().bead_size).max(3).min(w);
                paint_bead(surface, x + w - size, y, size, colour, shape);
            }
        }
    }

    fn paint_well(
        &self,
        surface: &mut Surface,
        index: usize,
        (cx, cy, cw, ch): (u32, u32, u32, u32),
        focused: bool,
        (scale, theme): (Scale, &Theme),
    ) {
        let palette = theme.palette();
        let margin = (well_gap(scale, theme) / 2).min(cw / 8).min(ch / 8);
        let Some((x, y, w, h)) = inset(cx, cy, cw, ch, margin) else {
            return;
        };
        let Some(colour) = self.colours.get(index).copied() else {
            return;
        };
        if colour.a < u8::MAX {
            paint_checker(surface, (x, y, w, h), theme);
        }
        let radius = scale
            .scale_length(theme.metrics().control_corner_radius)
            .min(w / 4)
            .min(h / 4);
        surface.fill_round_rect(x, y, w, h, radius, colour);
        let border = plate_border(theme, scale).min(w / 4).min(h / 4).max(1);
        outline(surface, (x, y, w, h), border, Color::from(palette.rim));
        let contrast = contrast_for(colour, theme);
        if self.primary == Some(index) {
            let ring = border.saturating_mul(2).min(w / 3).min(h / 3).max(1);
            outline(surface, (x, y, w, h), ring, Color::from(palette.rim_active));
            paint_diamond(surface, (x, y, w, h), contrast);
        }
        if self.secondary == Some(index) {
            let inner = (w.min(h) / 4).max(1);
            if let Some(square) = inset(x, y, w, h, inner) {
                outline(surface, square, border, contrast);
            }
        }
        if self.state.disposition() == ControlDisposition::DisabledByState {
            let veil = Color::from(palette.surface.with_alpha(128));
            surface.fill_round_rect(x, y, w, h, radius, veil);
        }
        // In the margin round the well, so it never covers the mark inside it.
        if focused && margin > 0 {
            surface.wash_ring(
                cx,
                cy,
                cw,
                ch,
                Ring::uniform(radius.saturating_add(margin), border.min(margin)),
                RingInk::Solid(Color::from(palette.rim_active)),
            );
        }
    }

    /// Feed a pointer event: a primary press and release over the same well
    /// moves `mark` onto it. A grid that is not actionable takes nothing.
    ///
    /// The press latch and the pointer position are hit-testing bookkeeping
    /// and draw nothing, so only a mark that moves reports.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        mark: SwatchMark,
        damage: &mut Region,
    ) -> Option<SwatchAction> {
        if let InputEvent::PointerMoved { to } = event {
            *self.pointer = *to;
        }
        if !self.state.is_actionable() {
            *self.armed = None;
            return None;
        }
        let over = self.well_at(bounds, *self.pointer);
        match event {
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => {
                *self.armed = over.map(|index| (index, mark));
                None
            }
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => match (self.armed.take(), over) {
                (Some((armed, mark)), Some(released)) if armed == released => {
                    Some(self.select(mark, released, bounds, damage))
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Feed a key to a focused, actionable grid: Left and Right move the
    /// primary mark one well, wrapping from the last well to the first; Up
    /// and Down move it one row within its column, wrapping to the column's
    /// other end, which a short last row may not reach. With no well marked,
    /// Right and Down mark the first and Left and Up the last.
    pub fn on_key(&mut self, key: Key, bounds: Rect, damage: &mut Region) -> Option<SwatchAction> {
        let len = self.colours.len();
        if len == 0 || !self.state.focus.focused || !self.state.is_actionable() {
            return None;
        }
        let columns = self.columns.max(1);
        let next = match (key, self.primary) {
            (Key::Named(NamedKey::Right | NamedKey::Down), None) => 0,
            (Key::Named(NamedKey::Left | NamedKey::Up), None) => len - 1,
            (Key::Named(NamedKey::Right), Some(at)) => (at + 1) % len,
            (Key::Named(NamedKey::Left), Some(at)) => (at + len - 1) % len,
            (Key::Named(NamedKey::Down), Some(at)) => match at + columns {
                below if below < len => below,
                _ => at % columns,
            },
            (Key::Named(NamedKey::Up), Some(at)) => at.checked_sub(columns).unwrap_or_else(|| {
                let bottom = (len - 1) / columns * columns + at % columns;
                if bottom < len {
                    bottom
                } else {
                    bottom - columns
                }
            }),
            _ => return None,
        };
        Some(self.select(SwatchMark::Primary, next, bounds, damage))
    }

    /// Move `mark` onto well `index`, reporting the well it left and the one
    /// it arrives on. A re-selection is still a completed interaction, so the
    /// action is always answered; only the report is conditional. A focused
    /// grid's focus ring follows the primary mark, so the first well, which
    /// carries the ring while none is marked, reports too.
    fn select(
        &mut self,
        mark: SwatchMark,
        index: usize,
        bounds: Rect,
        damage: &mut Region,
    ) -> SwatchAction {
        let from = match mark {
            SwatchMark::Primary => self.primary.or(self.cursor()),
            SwatchMark::Secondary => self.secondary,
        };
        damage::move_mark(
            from,
            Some(index),
            |well| self.cell_rect(bounds, well),
            damage,
        );
        if mark == SwatchMark::Primary && self.primary.is_none() {
            if let Some(rect) = self.cell_rect(bounds, index) {
                damage.add(rect);
            }
        }
        match mark {
            SwatchMark::Primary => self.primary = Some(index),
            SwatchMark::Secondary => self.secondary = Some(index),
        }
        SwatchAction::Selected { mark, index }
    }
}

/// The scaled gap between wells, from the theme's control gap.
fn well_gap(scale: Scale, theme: &Theme) -> u32 {
    scale.scale_length(theme.metrics().control_gap).max(1)
}

/// Black or white, whichever reads over `colour` as it shows: a translucent
/// well shows mostly the surface beneath it.
fn contrast_for(colour: Color, theme: &Theme) -> Color {
    let surface = Color::from(theme.palette().surface);
    let alpha = u32::from(colour.a);
    let seen = (u32::from(colour.luma()) * alpha + u32::from(surface.luma()) * (255 - alpha)) / 255;
    if seen > 128 {
        Color::rgb(0, 0, 0)
    } else {
        Color::rgb(255, 255, 255)
    }
}

/// A hollow rectangle of `thickness` inside `(x, y, w, h)`.
fn outline(
    surface: &mut Surface,
    (x, y, w, h): (u32, u32, u32, u32),
    thickness: u32,
    colour: Color,
) {
    if w == 0 || h == 0 || thickness == 0 {
        return;
    }
    let edge = thickness.min(w).min(h);
    surface.fill_rect(x, y, w, edge, colour);
    surface.fill_rect(x, y + h - edge, w, edge, colour);
    surface.fill_rect(x, y, edge, h, colour);
    surface.fill_rect(x + w - edge, y, edge, h, colour);
}

/// Two by two squares of the transparency checkerboard under a translucent
/// well.
fn paint_checker(surface: &mut Surface, (x, y, w, h): (u32, u32, u32, u32), theme: &Theme) {
    Checker::new(theme, Scale::ONE)
        .with_side(w.min(h).div_ceil(2))
        .paint(surface, x, y, w, h);
}

/// The primary mark: a small diamond centred in the well, drawn where it
/// sits rather than through a surface of its own.
fn paint_diamond(surface: &mut Surface, (x, y, w, h): (u32, u32, u32, u32), colour: Color) {
    let side = (w.min(h) / 2).max(3).min(w).min(h);
    // Placed in sub-pixels, so an odd side or well stays centred and square.
    let sub = |pixels: u32| to_i32(pixels).saturating_mul(SUBPIXEL);
    let cx = sub(x).saturating_add(sub(w) / 2);
    let cy = sub(y).saturating_add(sub(h) / 2);
    let half = sub(side) / 2;
    let points = [
        (cx, cy.saturating_sub(half)),
        (cx.saturating_add(half), cy),
        (cx, cy.saturating_add(half)),
        (cx.saturating_sub(half), cy),
    ];
    surface.fill_polygon_subpixel(&points, colour);
}

/// The span `(offset, length)` of share `index` of `count` equal shares of
/// `total` pixels, the remainder going a pixel at a time to the leading
/// shares so every pixel is covered exactly once.
fn axis_span(index: usize, count: usize, total: u32) -> Option<(u32, u32)> {
    if count == 0 || index >= count {
        return None;
    }
    let count = u32::try_from(count).ok()?;
    let index = u32::try_from(index).ok()?;
    let (base, remainder) = (total / count, total % count);
    let offset = base
        .saturating_mul(index)
        .saturating_add(remainder.min(index));
    Some((offset, base + u32::from(index < remainder)))
}

/// The share of `count` shares of `total` pixels that pixel `offset` falls
/// in: the inverse of [`axis_span`].
fn axis_index(offset: u32, count: usize, total: u32) -> Option<usize> {
    let shares = u32::try_from(count).ok().filter(|&shares| shares > 0)?;
    if offset >= total {
        return None;
    }
    let (base, remainder) = (total / shares, total % shares);
    // Only short of `u32::MAX` where a share is a whole `total`, with none
    // wide.
    let wider = base.saturating_add(1);
    let wide = wider.saturating_mul(remainder);
    let index = if offset < wide {
        offset / wider
    } else {
        remainder + (offset - wide) / base.max(1)
    };
    usize::try_from(index).ok().filter(|&index| index < count)
}
