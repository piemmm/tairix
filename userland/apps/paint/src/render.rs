//! Painting a painter window, from state alone.
//!
//! A paint reads the picture rows in view and nothing more, so a picture far
//! larger than the window costs what the window shows. A shape being dragged
//! and a floating selection are composed into those rows as they are read,
//! exactly as putting them down would compose them, so what is shown is what
//! will be drawn.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;
use core::ops::Range;

use tairix_controls::{blend_area, fill_area, withheld, Checker};
use tairix_font::BitmapFont;
use tairix_geometry::{to_i32, Rect, Scale};
use tairix_icon::IconArtwork;
use tairix_image::{IndexDepth, Rgba8, SpriteName};
use tairix_raster::{Color, Pixel, Surface};
use tairix_theme::Theme;
use tairix_util::fallible;

use crate::canvas::{Canvas, Kind, Sample};
use crate::colour::write_hex;
use crate::document::Entry;
use crate::layout::{Faces, Layout};
use crate::shape::Bounds;
use crate::stroke::lay_over;
use crate::view::{Preview, View};

/// How much a modal question darkens the window behind it, in 255ths.
const VEIL_ALPHA: u8 = 110;

/// How strongly the grid between pixels is drawn, in 255ths.
const GRID_ALPHA: u8 = 70;

/// The marquee's dashes, in physical pixels.
const DASH: i64 = 4;

/// Paint the whole window into `surface`, whose clip narrows it to what this
/// round repaints; `artwork` resolves the toolbar's glyphs.
pub fn render_into(
    surface: &mut Surface,
    view: &View,
    layout: &Layout,
    theme: &Theme,
    scale: Scale,
    faces: Faces,
    artwork: &mut dyn IconArtwork,
) {
    let palette = theme.palette();
    surface.fill(Color::from(palette.surface));
    let chrome = Color::from(palette.surface_raised);
    let (toolbar, swatches, settings, vertical, horizontal) = view.controls();
    if !withheld(surface, layout.toolbar()) {
        fill_area(surface, layout.toolbar(), chrome);
        toolbar.render(surface, layout.tools(), scale, theme, artwork);
    }
    if !withheld(surface, layout.panel()) {
        fill_area(surface, layout.panel(), chrome);
        wells(surface, view, layout, theme, scale, faces.status);
        swatches.render(surface, layout.swatches(), scale, theme);
        if !settings.is_empty() {
            let placed = settings.layout(layout.settings(), layout.window(), scale, theme);
            settings.render(surface, placed, scale, theme);
            if !placed.popup.is_empty() {
                settings.render_popup(surface, placed.popup, scale, theme);
            }
        }
    }
    if !withheld(surface, layout.canvas()) {
        canvas(surface, view, layout, theme, scale, faces.status);
    }
    if !withheld(surface, layout.vertical_bar()) {
        vertical.render(surface, layout.vertical_bar(), scale, theme);
    }
    if !withheld(surface, layout.horizontal_bar()) {
        horizontal.render(surface, layout.horizontal_bar(), scale, theme);
    }
    fill_area(surface, layout.corner(), Color::from(palette.scroll_track));
    if !withheld(surface, layout.status()) {
        status(surface, view, layout, theme, faces.status);
    }
    if let Some(modal) = view.modal() {
        let window = layout.window();
        blend_area(surface, window, palette.drop_shadow.with_alpha(VEIL_ALPHA));
        match modal {
            Ok(dialog) => {
                let bounds = crate::view::close_rect(dialog, window, scale, theme);
                dialog.render(surface, bounds, scale, theme);
            }
            Err(form) => form.render(surface, window, scale, theme),
        }
    }
}

/// The two colour wells, the secondary behind the primary, and the primary
/// colour spelled out beside them.
fn wells(
    surface: &mut Surface,
    view: &View,
    layout: &Layout,
    theme: &Theme,
    scale: Scale,
    font: BitmapFont,
) {
    let kind = view
        .document()
        .picture()
        .map_or(&Kind::Rgba, |picture| picture.canvas.kind());
    let (primary, secondary) = view.inks();
    let border = Color::from(theme.palette().on_surface_muted);
    for (rect, ink) in [
        (layout.secondary_well(), secondary),
        (layout.primary_well(), primary),
    ] {
        let Some((left, top)) = rect.surface_origin() else {
            continue;
        };
        surface.fill_rect(left, top, rect.width, rect.height, border);
        let inner = rect.inset(scale.scale_length(1).max(1));
        if let Some((inner_left, inner_top)) = inner.surface_origin() {
            Checker::new(theme, scale).paint(
                surface,
                inner_left,
                inner_top,
                inner.width,
                inner.height,
            );
            let colour = ink.shown(kind);
            surface.fill_round_rect(
                inner_left,
                inner_top,
                inner.width,
                inner.height,
                0,
                Color::rgba(colour[0], colour[1], colour[2], colour[3]),
            );
        }
    }
    let caption = layout.well_caption();
    let mut text = String::new();
    write_hex(primary.shown(kind), &mut text);
    if let crate::colour::Ink::Index(index) = primary {
        let _ = write!(text, "  [{index}]");
    }
    let y = font.centred_top(caption.top(), caption.height);
    let fitted = font.truncate_to_width(&text, caption.width);
    font.draw_text(
        surface,
        caption.left(),
        y,
        fitted,
        Color::from(theme.palette().on_surface),
    );
}

/// The canvas: the picture over the checkerboard where it is clear, the grid,
/// the selection; or why a kept sprite shows none.
fn canvas(
    surface: &mut Surface,
    view: &View,
    layout: &Layout,
    theme: &Theme,
    scale: Scale,
    font: BitmapFont,
) {
    let area = layout.canvas();
    let Some(picture) = view.document().picture() else {
        kept(surface, view, area, theme, font);
        return;
    };
    let canvas = &picture.canvas;
    let size = (canvas.width(), canvas.height());
    let placed = view
        .viewport()
        .to_screen(Bounds::picture(size.0, size.1), size, area);
    let Some((x, y)) = placed.surface_origin() else {
        return;
    };
    let Some((columns, rows)) = surface.admitted(x, y, placed.width, placed.height) else {
        return;
    };
    let viewport = view.viewport();
    let origin = viewport.origin(size, layout.canvas());
    let Some(map) = ColumnMap::new(columns, origin.0, viewport.span(), size.0) else {
        return;
    };
    let mut rows_drawn = Rows::new(view, canvas, map, origin, theme, scale);
    for screen_y in rows {
        rows_drawn.draw(surface, screen_y);
    }
    marquee(surface, view, layout, theme);
}

/// Why the canvas shows no picture: the entry is a sprite kept as its bytes.
fn kept(surface: &mut Surface, view: &View, area: Rect, theme: &Theme, font: BitmapFont) {
    let Entry::Kept(kept) = view.document().entry() else {
        return;
    };
    let text = alloc::format!(
        "\u{201c}{}\u{201d} cannot be edited: {}. It is kept, and saved back unchanged.",
        kept.name,
        kept.reason
    );
    let fitted = font.truncate_to_width(&text, area.width);
    let width = to_i32(font.text_width(fitted));
    let x = area.left() + (to_i32(area.width) - width) / 2;
    let y = font.centred_top(area.top(), area.height);
    font.draw_text(
        surface,
        x,
        y,
        fitted,
        Color::from(theme.palette().on_surface_muted),
    );
}

/// Where each screen column drawn reads the picture, worked out once a paint
/// rather than once a row.
struct ColumnMap {
    /// The screen columns drawn.
    screen: Range<u32>,
    /// The first picture column read; every tap is counted from it.
    first: u32,
    /// For each screen column in turn, each of its taps: the picture column
    /// it reads, less `first`.
    taps: Vec<u32>,
    /// Taps a screen column takes: one, or two spread across the picture
    /// columns it covers when reduced.
    per_column: usize,
    /// The picture is read only where the taps fall, rather than across
    /// every column between them: what a reduced paint, which covers far
    /// more columns than it samples, reads.
    gathered: bool,
    /// Picture columns from `first` the taps span.
    spanned: usize,
}

impl ColumnMap {
    /// The map of `screen`, the picture's columns starting at screen column
    /// `origin` at `span` screen pixels a picture pixel, across a picture
    /// `width` wide; `None` when its room is refused.
    fn new(screen: Range<u32>, origin: i64, span: (u64, u64, u64), width: u32) -> Option<Self> {
        let (across, _, den) = span;
        let covered = den / across.max(1);
        let per_column = tap_count(covered);
        let tapped = |at: u32| taps_in(picture_at(at, origin, across, den), covered, width);
        let first = tapped(screen.start)[0];
        let mut taps = Vec::new();
        taps.try_reserve_exact(screen.len() * per_column).ok()?;
        let mut end = first;
        for at in screen.clone() {
            for &column in &tapped(at)[..per_column] {
                end = end.max(column);
                taps.push(u32::try_from(column - first).ok()?);
            }
        }
        let spanned = usize::try_from(end - first + 1).ok()?;
        Some(Self {
            screen,
            first: u32::try_from(first).ok()?,
            gathered: spanned > taps.len(),
            taps,
            per_column,
            spanned,
        })
    }

    /// Where the colour of tap `entry` sits among those read.
    fn index(&self, entry: usize) -> usize {
        if self.gathered {
            entry
        } else {
            self.taps[entry] as usize
        }
    }

    /// The picture column the `index`th colour read stands for.
    fn column(&self, index: usize) -> i64 {
        let from = if self.gathered {
            i64::from(self.taps[index])
        } else {
            i64::try_from(index).unwrap_or(i64::MAX)
        };
        i64::from(self.first) + from
    }
}

/// The picture column or row under screen column or row `at`, from `origin`
/// at `span / den` screen pixels a picture pixel.
fn picture_at(at: u32, origin: i64, span: u64, den: u64) -> i64 {
    let offset = i128::from(at) - i128::from(origin);
    let picture = (offset * i128::from(den)).div_euclid(i128::from(span.max(1)));
    i64::try_from(picture).unwrap_or(i64::MAX)
}

/// What drawing the picture's rows needs, held across them so the buffers are
/// had once a paint and a picture row read once however many screen rows show
/// it.
struct Rows<'a> {
    view: &'a View,
    canvas: &'a Canvas,
    kind: &'a Kind,
    origin: (i64, i64),
    span: (u64, u64, u64),
    columns: ColumnMap,
    checker: Checker,
    grid: Option<Color>,
    preview: Option<Preview>,
    /// The picture row whose colours are held.
    held: Option<u32>,
    samples: Vec<Sample>,
    /// The floating layer's pixels over the row's run.
    above: Vec<Sample>,
    colours: Vec<[u8; 4]>,
    coverage: Vec<u8>,
    /// The sum of the taps each reduced screen pixel takes.
    reduced: Vec<[u32; 4]>,
}

impl<'a> Rows<'a> {
    fn new(
        view: &'a View,
        canvas: &'a Canvas,
        columns: ColumnMap,
        origin: (i64, i64),
        theme: &Theme,
        scale: Scale,
    ) -> Self {
        let grid = view.grid_shown().then(|| {
            let ink = theme.palette().on_surface_muted;
            Color::rgba(ink.r, ink.g, ink.b, GRID_ALPHA)
        });
        Self {
            view,
            canvas,
            kind: canvas.kind(),
            origin,
            span: view.viewport().span(),
            columns,
            checker: Checker::new(theme, scale),
            grid,
            preview: view.preview(),
            held: None,
            samples: Vec::new(),
            above: Vec::new(),
            colours: Vec::new(),
            coverage: Vec::new(),
            reduced: Vec::new(),
        }
    }

    /// Read picture row `y` where the columns are tapped, as it shows: the
    /// floating layer and the shape being dragged composed in. Answers
    /// whether it could be held.
    fn read(&mut self, y: u32) -> bool {
        if self.held == Some(y) {
            return true;
        }
        self.held = None;
        let map = &self.columns;
        let count = if map.gathered {
            map.taps.len()
        } else {
            map.spanned
        };
        if !fallible::grow_to(&mut self.samples, count, Sample::Rgba([0; 4]))
            || !fallible::grow_to(&mut self.colours, count, [0; 4])
        {
            return false;
        }
        self.samples.truncate(count);
        self.colours.truncate(count);
        if map.gathered {
            for (sample, &tap) in self.samples.iter_mut().zip(&map.taps) {
                *sample = self
                    .canvas
                    .sample(map.first + tap, y)
                    .unwrap_or(Sample::Rgba([0; 4]));
            }
        } else {
            self.canvas.row_samples(y, map.first, &mut self.samples);
        }
        if let Some(floating) = self.view.floating() {
            let (row, masked) = (i64::from(y), self.kind.masked());
            if map.gathered {
                for (index, sample) in self.samples.iter_mut().enumerate() {
                    *sample = floating.shows(map.column(index), row, *sample, masked);
                }
            } else {
                if !fallible::grow_to(&mut self.above, count, Sample::Rgba([0; 4])) {
                    return false;
                }
                let first = i64::from(map.first);
                floating.compose_row(row, first, &mut self.samples, &mut self.above, masked);
            }
        }
        if let Some(preview) = &self.preview {
            if !fallible::grow_to(&mut self.coverage, map.spanned, 0) {
                return false;
            }
            self.coverage.truncate(map.spanned);
            let masked = self.kind.masked();
            for (layer, shape) in preview.layers.iter().flatten() {
                shape.row(
                    i64::from(y),
                    i64::from(map.first),
                    preview.smooth,
                    &mut self.coverage,
                );
                for (index, sample) in self.samples.iter_mut().enumerate() {
                    let at = map.column(index) - i64::from(map.first);
                    let cover = usize::try_from(at)
                        .ok()
                        .and_then(|at| self.coverage.get(at))
                        .copied()
                        .unwrap_or(0);
                    if cover > 0 {
                        *sample = lay_over(*sample, *layer, cover, masked);
                    }
                }
            }
        }
        for (colour, sample) in self.colours.iter_mut().zip(&self.samples) {
            *colour = self.kind.colour(*sample);
        }
        self.held = Some(y);
        true
    }

    /// Draw screen row `screen_y` across the columns mapped.
    fn draw(&mut self, surface: &mut Surface, screen_y: u32) {
        let (across, down, den) = self.span;
        let height = self.canvas.height();
        let row = picture_at(screen_y, self.origin.1, down, den);
        let covered = den / down.max(1);
        let mut rows = [0u32; 2];
        for (slot, row) in rows.iter_mut().zip(taps_in(row, covered, height)) {
            *slot = u32::try_from(row).unwrap_or(0);
        }
        let rows = &rows[..tap_count(covered)];
        if den <= across.min(down) {
            if self.read(rows[0]) {
                self.write_row(surface, screen_y);
            }
            return;
        }
        let width = self.columns.screen.len();
        if !fallible::grow_to(&mut self.reduced, width, [0; 4]) {
            return;
        }
        self.reduced.truncate(width);
        self.reduced.fill([0; 4]);
        let per_column = self.columns.per_column;
        for &row in rows {
            if !self.read(row) {
                return;
            }
            for (column, slot) in self.reduced.iter_mut().enumerate() {
                for entry in column * per_column..(column + 1) * per_column {
                    let colour = self.colours[self.columns.index(entry)];
                    let pixel =
                        Color::rgba(colour[0], colour[1], colour[2], colour[3]).premultiply();
                    slot[0] += u32::from(pixel.r);
                    slot[1] += u32::from(pixel.g);
                    slot[2] += u32::from(pixel.b);
                    slot[3] += u32::from(pixel.a);
                }
            }
        }
        let samples = u32::try_from(rows.len() * per_column).unwrap_or(1).max(1);
        self.write_reduced(surface, screen_y, samples);
    }

    /// Write screen row `screen_y` from the colours read, each picture pixel
    /// spread over the screen pixels it spans.
    fn write_row(&self, surface: &mut Surface, screen_y: u32) {
        let (across, down, den) = self.span;
        let origin = self.origin;
        let checker = self.checker;
        let grid = self.grid;
        let on_row_line = grid.is_some() && {
            let offset = i64::from(screen_y) - origin.1;
            let step = i64::try_from(down / den.max(1)).unwrap_or(1).max(1);
            offset.rem_euclid(step) == 0
        };
        let column_step = i64::try_from(across / den.max(1)).unwrap_or(1).max(1);
        let screen = &self.columns.screen;
        let width = screen.end - screen.start;
        let Some((start, span)) = surface.row_span_mut(screen_y, screen.start, width) else {
            return;
        };
        let skip = (start - screen.start) as usize;
        let checker_origin = (origin.0.max(0), origin.1.max(0));
        for ((at, slot), entry) in (start..).zip(span.iter_mut()).zip(skip..) {
            let colour = self.colours[self.columns.index(entry)];
            let colour = Color::rgba(colour[0], colour[1], colour[2], colour[3]);
            let mut pixel = shown(colour, &checker, at, screen_y, checker_origin);
            if let Some(line) = grid {
                let on_column_line = (i64::from(at) - origin.0).rem_euclid(column_step) == 0;
                if on_row_line || on_column_line {
                    pixel = line.premultiply().over(pixel);
                }
            }
            *slot = pixel;
        }
    }

    /// Write screen row `screen_y` from the reduced sums of `samples` taps
    /// each.
    fn write_reduced(&self, surface: &mut Surface, screen_y: u32, samples: u32) {
        let screen = &self.columns.screen;
        let width = screen.end - screen.start;
        let checker = self.checker;
        let checker_origin = (self.origin.0.max(0), self.origin.1.max(0));
        let Some((start, span)) = surface.row_span_mut(screen_y, screen.start, width) else {
            return;
        };
        let skip = (start - screen.start) as usize;
        for ((at, slot), sum) in (start..)
            .zip(span.iter_mut())
            .zip(self.reduced.iter().skip(skip))
        {
            let mean =
                |channel: u32| u8::try_from((channel + samples / 2) / samples).unwrap_or(u8::MAX);
            let pixel = Pixel {
                r: mean(sum[0]),
                g: mean(sum[1]),
                b: mean(sum[2]),
                a: mean(sum[3]),
            };
            let below = checker_at(&checker, at, screen_y, checker_origin).premultiply();
            *slot = pixel.over(below);
        }
    }
}

/// How many picture pixels a screen pixel covering `covered` of them
/// samples: one, or two when it is reduced.
const fn tap_count(covered: u64) -> usize {
    if covered < 2 {
        1
    } else {
        2
    }
}

/// The picture pixels a screen pixel samples whose footprint starts at
/// `start` and covers `covered` of a side `length` long: a quarter and three
/// quarters of the way across what of the footprint the picture holds, so a
/// last screen pixel the edge cuts short still shows its own pixels.
fn taps_in(start: i64, covered: u64, length: u32) -> [i64; 2] {
    let last = i64::from(length).saturating_sub(1).max(0);
    let start = start.clamp(0, last);
    let covered = i64::try_from(covered.max(1)).unwrap_or(i64::MAX);
    let there = (last - start + 1).min(covered);
    [start + there / 4, start + there * 3 / 4]
}

/// The checkerboard's colour at screen pixel `(x, y)`, its squares counted
/// from where the picture starts.
fn checker_at(checker: &Checker, x: u32, y: u32, from: (i64, i64)) -> Color {
    let dx = u32::try_from((i64::from(x) - from.0).max(0)).unwrap_or(0);
    let dy = u32::try_from((i64::from(y) - from.1).max(0)).unwrap_or(0);
    checker.at(dx, dy)
}

/// `colour` as it shows at screen pixel `(x, y)`: over the checkerboard
/// where it is not opaque.
fn shown(colour: Color, checker: &Checker, x: u32, y: u32, from: (i64, i64)) -> Pixel {
    if colour.a == u8::MAX {
        return colour.premultiply();
    }
    colour
        .premultiply()
        .over(checker_at(checker, x, y, from).premultiply())
}

/// The selection's edge, or the floating layer's, in dashes of dark and
/// light so it shows on any picture.
fn marquee(surface: &mut Surface, view: &View, layout: &Layout, theme: &Theme) {
    let bounds = view
        .floating()
        .map(crate::selection::Floating::bounds)
        .or(view.selection());
    let Some(bounds) = bounds else {
        return;
    };
    let Some(picture) = view.document().picture() else {
        return;
    };
    let size = (picture.canvas.width(), picture.canvas.height());
    let clip = layout.canvas();
    let edge = view.viewport().screen_span(bounds, size, clip);
    let dark = Color::from(theme.palette().on_surface);
    let light = Color::from(theme.palette().surface);
    let (left, top, right, bottom) = (edge.x0, edge.y0, edge.x1 - 1, edge.y1 - 1);
    let mut dot = |x: i64, y: i64, along: i64| {
        let (Ok(px), Ok(py)) = (i32::try_from(x), i32::try_from(y)) else {
            return;
        };
        if !clip.contains(tairix_geometry::Point::new(px, py)) {
            return;
        }
        let colour = if along.div_euclid(DASH) % 2 == 0 {
            dark
        } else {
            light
        };
        if let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) {
            surface.fill_rect(x, y, 1, 1, colour);
        }
    };
    for x in left.max(i64::from(clip.left()))..=right.min(i64::from(clip.right())) {
        dot(x, top, x - left);
        dot(x, bottom, x - left);
    }
    for y in top.max(i64::from(clip.top()))..=bottom.min(i64::from(clip.bottom())) {
        dot(left, y, y - top);
        dot(right, y, y - top);
    }
}

/// The status band: the picture and the pixel under the pointer, a message,
/// the sprite showing, and the magnification.
fn status(surface: &mut Surface, view: &View, layout: &Layout, theme: &Theme, font: BitmapFont) {
    let palette = theme.palette();
    fill_area(
        surface,
        layout.status(),
        Color::from(palette.surface_raised),
    );
    let band = layout.status();
    let y = font.centred_top(band.top(), band.height);
    let ink = Color::from(palette.on_surface);
    let muted = Color::from(palette.on_surface_muted);
    let draw = |surface: &mut Surface, rect: Rect, words: &str, colour: Color| {
        let fitted = font.truncate_to_width(words, rect.width);
        font.draw_text(surface, rect.left(), y, fitted, colour);
    };
    let document = view.document();
    let mut text = String::new();
    if let (Some((x, px_y)), Some(picture)) = (view.hover(), document.picture()) {
        // The colour shown there, a floating layer and its lift's leavings
        // included.
        let kind = picture.canvas.kind();
        let shown = picture.canvas.sample(x, px_y).map(|sample| {
            view.floating().map_or(sample, |floating| {
                floating.shows(i64::from(x), i64::from(px_y), sample, kind.masked())
            })
        });
        write_position(
            &mut text,
            (x, px_y),
            shown.map(|sample| kind.colour(sample)),
        );
    }
    draw(surface, layout.position(), &text, ink);
    text.clear();
    match (view.message(), document.picture()) {
        (Some(message), _) => text.push_str(message),
        (None, Some(picture)) => {
            let canvas = &picture.canvas;
            let _ = write!(
                text,
                "{} \u{00d7} {}, {}",
                canvas.width(),
                canvas.height(),
                depth_label(canvas.kind())
            );
        }
        (None, None) => text.push_str("A kept sprite"),
    }
    draw(
        surface,
        layout.message(),
        &text,
        if view.message().is_some() { ink } else { muted },
    );
    text.clear();
    if document.is_sprite_area() {
        write_sprite(
            &mut text,
            document.current() + 1,
            document.entries().len(),
            document.entry().name(),
        );
    }
    draw(surface, layout.sprite(), &text, ink);
    text.clear();
    write_zoom(&mut text, view.viewport().percent());
    draw(surface, layout.zoom(), &text, ink);
}

/// The status band's position readout, after what `out` holds: picture
/// pixel `at`, and the colour shown there.
pub fn write_position(out: &mut String, (x, y): (u32, u32), colour: Option<Rgba8>) {
    let _ = write!(out, "({x}, {y}) ");
    if let Some(colour) = colour {
        write_hex(colour, out);
    }
}

/// Its sprite readout: which sprite of how many shows, and its name.
pub fn write_sprite(out: &mut String, ordinal: usize, count: usize, name: Option<&SpriteName>) {
    let _ = write!(out, "{ordinal} of {count}");
    if let Some(name) = name {
        let _ = write!(out, ": {name}");
    }
}

/// Its zoom readout, which the zoom menu names each rung by too.
pub fn write_zoom(out: &mut String, percent: u32) {
    let _ = write!(out, "{percent}%");
}

/// How a picture's pixels are stored, as a person reads it.
fn depth_label(kind: &Kind) -> &'static str {
    match kind.depth() {
        None => "millions of colours",
        Some(IndexDepth::Eight) => "256 colours",
        Some(IndexDepth::Four) => "16 colours",
        Some(IndexDepth::Two) => "4 colours",
        Some(IndexDepth::One) => "2 colours",
    }
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
