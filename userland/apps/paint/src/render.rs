//! Painting a painter window, from state alone.
//!
//! A paint reads the picture rows in view and nothing more, so a picture far
//! larger than the window costs what the window shows. A shape being dragged
//! and a floating selection are composed into the layer painted on as its
//! rows are read, exactly as putting them down would compose them, and the
//! layers are then laid together, so what is shown is what will be drawn.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;
use core::ops::Range;

use tairix_colour::Rgba;
use tairix_controls::{blend_area, fill_area, paint_run, withheld, Checker, SwatchMark};
use tairix_font::BitmapFont;
use tairix_geometry::{to_i32, Point, Rect, Scale};
use tairix_icon::IconArtwork;
use tairix_image::{IndexDepth, Rgba8, SpriteName};
use tairix_raster::{Color, CoverageRows, Pixel, Surface};
use tairix_theme::Theme;
use tairix_util::fallible;

use crate::canvas::{Canvas, Kind, Sample};
use crate::colour::Ink;
use crate::compose::compose_run;
use crate::document::{Entry, Layer, Picture};
use crate::gradient::Laying;
use crate::layout::{Faces, Layout};
use crate::mask::Mask;
use crate::selection::Floating;
use crate::shape::{line_pixels, Bounds, Point as Fx, Shape, ShapeScratch, FX};
use crate::stroke::{lay_over, Blend, Coat};
use crate::text::TextEntry;
use crate::view::{Marking, View};
use crate::viewport::{screen_rect, Viewport};

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
    let controls = view.controls();
    if !withheld(surface, layout.top()) {
        fill_area(surface, layout.top(), chrome);
        controls
            .bar
            .render(surface, layout.bar(), faces, scale, theme);
        controls
            .commands
            .render(surface, layout.view_strip(), scale, theme, artwork);
    }
    if !withheld(surface, layout.tool_box()) {
        fill_area(surface, layout.tool_box(), chrome);
        controls
            .tool_box
            .render(surface, layout.tools(), scale, theme, artwork);
    }
    if !withheld(surface, layout.dock()) {
        fill_area(surface, layout.dock(), chrome);
        wells(surface, view, layout, theme, scale, faces.status);
        view.dock().0.render(surface, layout.picker(), scale, theme);
    }
    if !withheld(surface, layout.canvas()) {
        canvas(surface, view, layout, theme, scale, faces.status);
    }
    if !withheld(surface, layout.vertical_bar()) {
        controls
            .vertical
            .render(surface, layout.vertical_bar(), scale, theme);
    }
    if !withheld(surface, layout.horizontal_bar()) {
        controls
            .horizontal
            .render(surface, layout.horizontal_bar(), scale, theme);
    }
    fill_area(surface, layout.corner(), Color::from(palette.scroll_track));
    if !withheld(surface, layout.palette()) {
        fill_area(surface, layout.palette(), chrome);
        controls
            .swatches
            .render(surface, layout.swatches(), scale, theme);
    }
    if !withheld(surface, layout.status()) {
        status(surface, view, layout, theme, faces.status);
    }
    // An open list hangs over whatever lies beneath it.
    controls
        .bar
        .render_popup(surface, layout.bar(), scale, theme);
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

/// The two colour wells, the secondary behind the primary, the one the
/// picker edits rimmed in the accent, and beside them which one that is.
fn wells(
    surface: &mut Surface,
    view: &View,
    layout: &Layout,
    theme: &Theme,
    scale: Scale,
    font: BitmapFont,
) {
    let kind = view.document().picture().map_or(&Kind::Rgba, Picture::kind);
    let (primary, secondary) = view.inks();
    let editing = view.dock().1;
    let palette = theme.palette();
    for (rect, ink, mark) in [
        (layout.secondary_well(), secondary, SwatchMark::Secondary),
        (layout.primary_well(), primary, SwatchMark::Primary),
    ] {
        let Some((left, top)) = rect.surface_origin() else {
            continue;
        };
        let (border, rim) = if mark == editing {
            (palette.rim_active, scale.scale_length(2).max(2))
        } else {
            (palette.on_surface_muted, scale.scale_length(1).max(1))
        };
        surface.fill_rect(left, top, rect.width, rect.height, Color::from(border));
        let inner = rect.inset(rim);
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
    write_editing(
        &mut text,
        editing,
        if editing == SwatchMark::Primary {
            primary
        } else {
            secondary
        },
    );
    let y = font.centred_top(caption.top(), caption.height);
    let run = font.elide_to_width(&text, caption.width);
    paint_run(
        surface,
        font,
        run,
        (caption.left(), y),
        Color::from(palette.on_surface),
        None,
    );
}

/// What the dock's caption says: which ink the picker edits, and on a
/// palette picture which entry that is.
pub fn write_editing(out: &mut String, editing: SwatchMark, ink: Ink) {
    out.push_str(match editing {
        SwatchMark::Primary => "Primary",
        SwatchMark::Secondary => "Secondary",
    });
    match ink {
        Ink::Index(entry) => {
            let _ = write!(out, ", entry {entry}");
        }
        Ink::Clear => out.push_str(", clear"),
        Ink::Colour(_) => {}
    }
}

/// What the status band says of a picture: its size, its depth, and which
/// of several layers is painted on.
pub fn write_shape(out: &mut String, picture: &Picture) {
    let (width, height) = picture.size();
    let _ = write!(
        out,
        "{width} \u{00d7} {height}, {}",
        depth_label(picture.kind())
    );
    let layers = picture.layers();
    if let (true, Some(layer)) = (layers.len() > 1, layers.get(picture.active())) {
        let _ = write!(
            out,
            ", layer {} of {}: {}",
            picture.active() + 1,
            layers.len(),
            layer.name
        );
    }
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
    // A filter being previewed shows the layer as it would leave it.
    let canvas = view.preview_canvas().unwrap_or_else(|| picture.canvas());
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
    let mut shapes = [ShapeScratch::default(), ShapeScratch::default()];
    let layers = (!picture.single()).then(|| (picture.layers(), picture.active()));
    let mut rows_drawn = Rows::new(
        view,
        (canvas, layers),
        map,
        origin,
        (theme, scale),
        &mut shapes,
    );
    for screen_y in rows {
        rows_drawn.draw(surface, screen_y);
    }
    crop_box(surface, view, layout, theme, scale);
    marquee(surface, view, layout, theme);
    text_frame(surface, view, layout, theme);
    zoom_box(surface, view, layout, theme);
    clone_marker(surface, view, layout, theme);
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
        let tapped = |at: u32| {
            taps_in(
                picture_at(i64::from(at), origin, across, den),
                covered,
                width,
            )
        };
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
fn picture_at(at: i64, origin: i64, span: u64, den: u64) -> i64 {
    let offset = i128::from(at) - i128::from(origin);
    let picture = (offset * i128::from(den)).div_euclid(i128::from(span.max(1)));
    i64::try_from(picture).unwrap_or(i64::MAX)
}

/// What drawing the picture's rows needs, held across them so the buffers are
/// had once a paint and a picture row read once however many screen rows show
/// it.
struct Rows<'a> {
    view: &'a View,
    /// The layer painted on, as it shows.
    canvas: &'a Canvas,
    /// The picture's layers and which is painted on, where they must be laid
    /// together to show.
    layers: Option<(&'a [Layer], usize)>,
    kind: &'a Kind,
    origin: (i64, i64),
    span: (u64, u64, u64),
    columns: ColumnMap,
    checker: Checker,
    grid: Option<Color>,
    /// The shape being dragged, each layer's coverage traced once a paint.
    preview: [Option<(Coat, CoverageRows<'a>)>; 2],
    /// The selection the shape is held to, as it will be when it lands.
    clip: Option<&'a Mask>,
    /// The gradient being dragged.
    gradient: Option<Laying>,
    /// The text being typed, and the layer it will be set down as.
    text: Option<(&'a TextEntry, Coat)>,
    /// The picture row whose colours are held.
    held: Option<u32>,
    samples: Vec<Sample>,
    /// The floating selection's pixels over the row's run.
    above: Vec<Sample>,
    colours: Vec<[u8; 4]>,
    /// What the layers show together, and one layer's colours read beneath.
    composed: Vec<[u8; 4]>,
    beneath: Vec<[u8; 4]>,
    coverage: Vec<u8>,
    /// The selection's share of the row's run.
    chosen: Vec<u8>,
    /// The sum of the taps each reduced screen pixel takes.
    reduced: Vec<[u32; 4]>,
}

impl<'a> Rows<'a> {
    fn new(
        view: &'a View,
        (canvas, layers): (&'a Canvas, Option<(&'a [Layer], usize)>),
        columns: ColumnMap,
        origin: (i64, i64),
        (theme, scale): (&Theme, Scale),
        shapes: &'a mut [ShapeScratch; 2],
    ) -> Self {
        let grid = view.grid_shown().then(|| {
            let ink = theme.palette().on_surface_muted;
            Color::rgba(ink.r, ink.g, ink.b, GRID_ALPHA)
        });
        let mut preview = [None, None];
        if let Some(dragged) = view.preview() {
            // A preview whose outline cannot be held is not drawn; the shape
            // itself still lands when the drag ends.
            for ((slot, coat), scratch) in
                preview.iter_mut().zip(dragged.coats).zip(shapes.iter_mut())
            {
                if let Some((coat, shape)) = coat {
                    if let Ok(Some(rows)) = shape.rows(dragged.smooth, scratch) {
                        *slot = Some((coat, rows));
                    }
                }
            }
        }
        let kind = view.preview_kind().unwrap_or_else(|| canvas.kind());
        Self {
            view,
            canvas,
            layers,
            kind,
            origin,
            span: view.viewport().span(),
            columns,
            checker: Checker::new(theme, scale),
            grid,
            preview,
            clip: view.selection(),
            gradient: view.gradient().map(|gradient| gradient.on(kind)),
            text: view.text().map(|entry| {
                let kind = canvas.kind();
                let (ink, _) = view.inks();
                let blend = if view.smooth_shown(kind) {
                    Blend::Over
                } else {
                    Blend::Replace
                };
                (entry, Coat { ink, blend })
            }),
            held: None,
            samples: Vec::new(),
            above: Vec::new(),
            colours: Vec::new(),
            composed: Vec::new(),
            beneath: Vec::new(),
            coverage: Vec::new(),
            chosen: Vec::new(),
            reduced: Vec::new(),
        }
    }

    /// Read picture row `y` where the columns are tapped, as it shows: the
    /// floating selection and the shape being dragged composed into the layer
    /// painted on, and the layers laid together. Answers whether it could be
    /// held.
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
        self.compose_laid(i64::from(y));
        let map = &self.columns;
        if self.preview.iter().any(Option::is_some) {
            if !fallible::grow_to(&mut self.coverage, map.spanned, 0)
                || !fallible::grow_to(&mut self.chosen, map.spanned, 0)
            {
                return false;
            }
            self.coverage.truncate(map.spanned);
            self.chosen.truncate(map.spanned);
            if let Some(clip) = self.clip {
                clip.row(i64::from(y), i64::from(map.first), &mut self.chosen);
            }
            let masked = self.kind.masked();
            for (coat, rows) in self.preview.iter_mut().flatten() {
                rows.row(y, map.first, &mut self.coverage);
                if self.clip.is_some() {
                    for (cover, &chosen) in self.coverage.iter_mut().zip(&self.chosen) {
                        *cover = crate::mask::scale(*cover, chosen);
                    }
                }
                for (index, sample) in self.samples.iter_mut().enumerate() {
                    let at = map.column(index) - i64::from(map.first);
                    let cover = usize::try_from(at)
                        .ok()
                        .and_then(|at| self.coverage.get(at))
                        .copied()
                        .unwrap_or(0);
                    if cover > 0 {
                        *sample = lay_over(*sample, *coat, cover, masked);
                    }
                }
            }
        }
        for (colour, sample) in self.colours.iter_mut().zip(&self.samples) {
            *colour = self.kind.colour(*sample);
        }
        if !self.lay_layers(y) {
            return false;
        }
        self.held = Some(y);
        true
    }

    /// Lay the layers together over row `y` where they must be, the one
    /// painted on as its colours were read. Answers whether the room could
    /// be had.
    fn lay_layers(&mut self, y: u32) -> bool {
        let Some((layers, active)) = self.layers else {
            return true;
        };
        let count = self.colours.len();
        if !fallible::grow_to(&mut self.composed, count, [0; 4])
            || !fallible::grow_to(&mut self.beneath, count, [0; 4])
        {
            return false;
        }
        self.composed.truncate(count);
        self.beneath.truncate(count);
        let map = &self.columns;
        let shown = Some((active, self.colours.as_slice()));
        compose_run(
            layers,
            shown,
            &mut self.composed,
            &mut self.beneath,
            |canvas, into| {
                if map.gathered {
                    for (slot, &tap) in into.iter_mut().zip(&map.taps) {
                        *slot = canvas.colour_at(map.first + tap, y).unwrap_or([0; 4]);
                    }
                } else {
                    canvas.row_colours(y, map.first, into);
                }
            },
        );
        core::mem::swap(&mut self.colours, &mut self.composed);
        true
    }

    /// Lay the gradient being dragged, or the text being typed, over picture
    /// row `row` as read, each held to the selection as it will be.
    fn compose_laid(&mut self, row: i64) {
        let map = &self.columns;
        if let Some(gradient) = &self.gradient {
            for (index, sample) in self.samples.iter_mut().enumerate() {
                let column = map.column(index);
                let cover = self.clip.map_or(u8::MAX, |clip| clip.at(column, row));
                if cover > 0 {
                    *sample = gradient.laid((column, row), *sample, cover);
                }
            }
        }
        let Some((entry, coat)) = self.text else {
            return;
        };
        let bounds = entry.bounds();
        if !(bounds.y0..bounds.y1).contains(&row) {
            return;
        }
        let masked = self.kind.masked();
        for (index, sample) in self.samples.iter_mut().enumerate() {
            let column = map.column(index);
            let chosen = self.clip.map_or(u8::MAX, |clip| clip.at(column, row));
            let cover = crate::mask::scale(entry.at(column, row), chosen);
            if cover > 0 {
                *sample = lay_over(*sample, coat, cover, masked);
            }
        }
    }

    /// Draw screen row `screen_y` across the columns mapped.
    fn draw(&mut self, surface: &mut Surface, screen_y: u32) {
        let (across, down, den) = self.span;
        let height = self.canvas.height();
        let row = picture_at(i64::from(screen_y), self.origin.1, down, den);
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

/// The outline of the selection held — or of the floating selection — and of a
/// selection being marked out, in dashes of dark and light so it shows on
/// any picture. Each is drawn only where it crosses what this paint admits.
fn marquee(surface: &mut Surface, view: &View, layout: &Layout, theme: &Theme) {
    let Some(picture) = view.document().picture() else {
        return;
    };
    let size = picture.size();
    let area = layout.canvas();
    let viewport = view.viewport();
    let mut ants = Ants {
        surface,
        clip: area,
        dark: Color::from(theme.palette().on_surface),
        light: Color::from(theme.palette().surface),
    };
    let held = view
        .floating()
        .map_or_else(|| view.selection().cloned(), Floating::selection);
    if let Some(held) = held {
        if held.is_rect() {
            ants.rect(viewport.screen_span(held.bounds(), size, area));
        } else {
            ants.outline(&held, viewport, size);
        }
    }
    let screen = |(x, y): (i64, i64)| viewport.screen_of((x, y), size, area);
    match view.marking() {
        Some(Marking::Shape(Shape::Rect { span, .. })) => {
            ants.rect(viewport.screen_span(span.bounds(), size, area));
        }
        Some(Marking::Shape(shape)) => {
            // An outline that cannot be held is not drawn; the selection
            // still lands when the drag ends.
            let mut scratch = ShapeScratch::default();
            if let Ok(outline) = shape.outline(&mut scratch) {
                let corner = |&(x, y): &(i32, i32)| screen((i64::from(x), i64::from(y)));
                ants.path(outline.iter().map(corner), true);
            }
        }
        Some(Marking::Path { points, to }) => {
            let at = |point: &Fx| screen((point.x, point.y));
            ants.path(points.iter().chain(to.as_ref()).map(at), false);
        }
        None => {}
    }
}

/// The crop tool's box: what it cuts away veiled, its edge dashed, and its
/// eight handles.
fn crop_box(surface: &mut Surface, view: &View, layout: &Layout, theme: &Theme, scale: Scale) {
    let (Some(held), Some(picture)) = (view.crop_box(), view.document().picture()) else {
        return;
    };
    let size = picture.size();
    let area = layout.canvas();
    let viewport = view.viewport();
    let shown = viewport.to_screen(Bounds::picture(size.0, size.1), size, area);
    let kept = viewport.to_screen(held, size, area);
    let veil = theme.palette().drop_shadow.with_alpha(CROP_VEIL);
    let above = Rect::new(
        shown.left(),
        shown.top(),
        shown.width,
        to_u32(kept.top() - shown.top()),
    );
    let below = Rect::new(
        shown.left(),
        kept.bottom(),
        shown.width,
        to_u32(shown.bottom() - kept.bottom()),
    );
    let left = Rect::new(
        shown.left(),
        kept.top(),
        to_u32(kept.left() - shown.left()),
        kept.height,
    );
    let right = Rect::new(
        kept.right(),
        kept.top(),
        to_u32(shown.right() - kept.right()),
        kept.height,
    );
    for part in [above, below, left, right] {
        blend_area(surface, part.intersection(&area), veil);
    }
    let edge = viewport.screen_span(held, size, area);
    let (dark, light) = (
        Color::from(theme.palette().on_surface),
        Color::from(theme.palette().surface),
    );
    let breadth = i64::from(scale.scale_length(crate::view::CROP_REACH));
    for handle in crate::crop::handles(edge, breadth) {
        let rect = screen_rect(handle).intersection(&area);
        fill_area(surface, rect, dark);
        fill_area(surface, rect.inset(1), light);
    }
    let mut ants = Ants {
        surface,
        clip: area,
        dark,
        light,
    };
    ants.rect(edge);
}

/// The frame of the text being typed and its caret.
fn text_frame(surface: &mut Surface, view: &View, layout: &Layout, theme: &Theme) {
    let (Some(entry), Some(picture)) = (view.text(), view.document().picture()) else {
        return;
    };
    let size = picture.size();
    let area = layout.canvas();
    let viewport = view.viewport();
    let mut ants = Ants {
        surface,
        clip: area,
        dark: Color::from(theme.palette().on_surface),
        light: Color::from(theme.palette().surface),
    };
    let (x, top, bottom) = entry.caret();
    let caret = Bounds {
        x0: x,
        y0: top,
        x1: x + 1,
        y1: bottom,
    };
    let framed = entry.bounds().union(&caret);
    ants.rect(viewport.screen_span(framed, size, area));
    let screen = |at: (i64, i64)| viewport.screen_of((at.0 * FX, at.1 * FX), size, area);
    let (from, to) = (screen((x, top)), screen((x, bottom)));
    let colour = ants.dark;
    for y in from.1..to.1 {
        if let (Ok(px), Ok(py)) = (u32::try_from(from.0), u32::try_from(y)) {
            if area.contains(Point::new(to_i32_saturating(from.0), to_i32_saturating(y))) {
                ants.surface.fill_rect(px, py, 1, 1, colour);
            }
        }
    }
}

/// The zoom tool's box being dragged.
fn zoom_box(surface: &mut Surface, view: &View, layout: &Layout, theme: &Theme) {
    let Some((from, to)) = view.zoom_box() else {
        return;
    };
    let mut ants = Ants {
        surface,
        clip: layout.canvas(),
        dark: Color::from(theme.palette().on_surface),
        light: Color::from(theme.palette().surface),
    };
    let held = crate::view::screen_box(from, to);
    ants.rect(Bounds {
        x0: i64::from(held.left()),
        y0: i64::from(held.top()),
        x1: i64::from(held.right()),
        y1: i64::from(held.bottom()),
    });
}

/// A cross where the clone tool copies from.
fn clone_marker(surface: &mut Surface, view: &View, layout: &Layout, theme: &Theme) {
    let (Some(at), Some(picture)) = (view.clone_marker(layout), view.document().picture()) else {
        return;
    };
    let size = picture.size();
    let (x, y) = view
        .viewport()
        .screen_of((at.x, at.y), size, layout.canvas());
    let reach = i64::from(crate::view::MARKER);
    let mut ants = Ants {
        surface,
        clip: layout.canvas(),
        dark: Color::from(theme.palette().on_surface),
        light: Color::from(theme.palette().surface),
    };
    ants.line((x - reach, y), (x + reach, y));
    ants.line((x, y - reach), (x, y + reach));
}

/// How much the crop box's veil darkens what it cuts away, in 255ths.
const CROP_VEIL: u8 = 120;

fn to_i32_saturating(value: i64) -> i32 {
    i32::try_from(value.clamp(i64::from(i32::MIN), i64::from(i32::MAX))).unwrap_or(0)
}

fn to_u32(value: i32) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

/// Dashes of dark and light, drawn into a surface within `clip`.
struct Ants<'s> {
    surface: &'s mut Surface,
    clip: Rect,
    dark: Color,
    light: Color,
}

impl Ants<'_> {
    /// Screen pixel `(x, y)`, its dash counted `along` the edge it is on.
    fn dot(&mut self, x: i64, y: i64, along: i64) {
        let (Ok(px), Ok(py)) = (i32::try_from(x), i32::try_from(y)) else {
            return;
        };
        if !self.clip.contains(Point::new(px, py)) {
            return;
        }
        let colour = if along.div_euclid(DASH) % 2 == 0 {
            self.dark
        } else {
            self.light
        };
        if let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) {
            self.surface.fill_rect(x, y, 1, 1, colour);
        }
    }

    /// The edge of the screen pixels `edge`, `[x0, x1) × [y0, y1)`.
    fn rect(&mut self, edge: Bounds) {
        if edge.is_empty() {
            return;
        }
        let (left, top, right, bottom) = (edge.x0, edge.y0, edge.x1 - 1, edge.y1 - 1);
        let clip = self.clip;
        for x in left.max(i64::from(clip.left()))..=right.min(i64::from(clip.right())) {
            self.dot(x, top, x);
            self.dot(x, bottom, x);
        }
        for y in top.max(i64::from(clip.top()))..=bottom.min(i64::from(clip.bottom())) {
            self.dot(left, y, y);
            self.dot(right, y, y);
        }
    }

    /// The straight line from screen pixel `a` to `b`, its dashes counted
    /// along whichever axis it runs nearer.
    fn line(&mut self, a: (i64, i64), b: (i64, i64)) {
        let across = (b.0 - a.0).abs() >= (b.1 - a.1).abs();
        let Some((from, to)) = clipped(a, b, self.clip) else {
            return;
        };
        line_pixels(from, to, |x, y| self.dot(x, y, if across { x } else { y }));
    }

    /// The lines through the screen pixels `corners` in turn, back to the
    /// first when `closed`.
    fn path(&mut self, corners: impl Iterator<Item = (i64, i64)>, closed: bool) {
        let mut first = None;
        let mut last = None;
        for corner in corners {
            match last {
                Some(from) => self.line(from, corner),
                None => first = Some(corner),
            }
            last = Some(corner);
        }
        if let (true, Some(from), Some(to)) = (closed, last, first) {
            self.line(from, to);
        }
    }

    /// The outline of `chosen`: each screen pixel showing a picture pixel it
    /// chooses beside one showing a pixel it does not, its dashes counted
    /// along the edge — down a side, across a top or bottom.
    fn outline(&mut self, chosen: &Mask, viewport: &Viewport, size: (u32, u32)) {
        let area = self.clip;
        let reach = Bounds {
            x0: chosen.bounds().x0 - 1,
            y0: chosen.bounds().y0 - 1,
            x1: chosen.bounds().x1 + 1,
            y1: chosen.bounds().y1 + 1,
        };
        let placed = viewport.to_screen(reach, size, area);
        let Some((x, y)) = placed.surface_origin() else {
            return;
        };
        let Some((columns, rows)) = self.surface.admitted(x, y, placed.width, placed.height) else {
            return;
        };
        let origin = viewport.origin(size, area);
        let (across, down, den) = viewport.span();
        // One screen pixel either side of what is drawn, for the neighbours.
        let first = i64::from(columns.start) - 1;
        let Some(picture_columns) = fallible::collected(
            columns.len() + 2,
            (first..).map(|at| picture_at(at, origin.0, across, den)),
        ) else {
            return;
        };
        let width = picture_columns.len();
        let (Some(mut above), Some(mut here), Some(mut below)) = (
            fallible::filled(width, false),
            fallible::filled(width, false),
            fallible::filled(width, false),
        ) else {
            return;
        };
        let inside = |out: &mut [bool], screen_y: i64| {
            let row = picture_at(screen_y, origin.1, down, den);
            for (flag, &column) in out.iter_mut().zip(&picture_columns) {
                *flag = chosen.chooses(column, row);
            }
        };
        let top = i64::from(rows.start);
        inside(&mut above, top - 1);
        inside(&mut here, top);
        for screen_y in top..i64::from(rows.end) {
            inside(&mut below, screen_y + 1);
            for index in 1..width - 1 {
                if !here[index] {
                    continue;
                }
                let upright = !here[index - 1] || !here[index + 1];
                let flat = !above[index] || !below[index];
                if upright || flat {
                    let screen_x = first + i64::try_from(index).unwrap_or(0);
                    self.dot(
                        screen_x,
                        screen_y,
                        if upright { screen_y } else { screen_x },
                    );
                }
            }
            core::mem::swap(&mut above, &mut here);
            core::mem::swap(&mut here, &mut below);
        }
    }
}

/// The part of the line from `a` to `b` inside `clip`, each end on a screen
/// pixel, so a line reaching far off the canvas costs only what shows.
fn clipped(a: (i64, i64), b: (i64, i64), clip: Rect) -> Option<((i64, i64), (i64, i64))> {
    let (ax, ay) = (i128::from(a.0), i128::from(a.1));
    let (dx, dy) = (i128::from(b.0) - ax, i128::from(b.1) - ay);
    let (left, top) = (i128::from(clip.left()), i128::from(clip.top()));
    let (right, bottom) = (i128::from(clip.right()) - 1, i128::from(clip.bottom()) - 1);
    // Where along the line, as a fraction with a positive denominator, it
    // enters the clip and where it leaves.
    let mut enter = (0i128, 1i128);
    let mut leave = (1i128, 1i128);
    for (p, q) in [
        (-dx, ax - left),
        (dx, right - ax),
        (-dy, ay - top),
        (dy, bottom - ay),
    ] {
        if p == 0 {
            if q < 0 {
                return None;
            }
            continue;
        }
        let t = if p < 0 { (-q, -p) } else { (q, p) };
        let later = |a: (i128, i128), b: (i128, i128)| a.0 * b.1 > b.0 * a.1;
        if p < 0 {
            if later(t, leave) {
                return None;
            }
            if later(t, enter) {
                enter = t;
            }
        } else {
            if later(enter, t) {
                return None;
            }
            if later(leave, t) {
                leave = t;
            }
        }
    }
    let at = |(num, den): (i128, i128)| {
        let step = |delta: i128| (2 * delta * num + den).div_euclid(2 * den);
        let point = |base: i128, delta: i128| i64::try_from(base + step(delta)).unwrap_or(0);
        (point(ax, dx), point(ay, dy))
    };
    Some((at(enter), at(leave)))
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
        // The colour shown there, a floating selection and its lift's
        // leavings included, through every layer.
        let kind = picture.kind();
        let shown = picture.canvas().sample(x, px_y).map(|sample| {
            let sample = view.floating().map_or(sample, |floating| {
                floating.shows(i64::from(x), i64::from(px_y), sample, kind.masked())
            });
            picture.shown_at((x, px_y), kind.colour(sample))
        });
        write_position(&mut text, (x, px_y), shown);
    }
    draw(surface, layout.position(), &text, ink);
    text.clear();
    match (view.message(), document.picture()) {
        (Some(message), _) => text.push_str(message),
        (None, Some(picture)) => write_shape(&mut text, picture),
        (None, None) => text.push_str("A kept sprite"),
    }
    draw(
        surface,
        layout.message(),
        &text,
        if view.message().is_some() { ink } else { muted },
    );
    text.clear();
    if document.is_sprite_area() || document.is_pages() {
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
        let _ = write!(out, "{}", Rgba::from_array(colour).hex().hashed());
    }
}

/// Its sprite or page readout: which of how many shows, and a sprite's name.
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
