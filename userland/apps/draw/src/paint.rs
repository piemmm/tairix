//! Painting a Draw window.
//!
//! The canvas is the page on its pasteboard with the tiles rendered for it
//! laid over, and over those what the hand is doing: the selection and its
//! handles, the drag's preview, the path being drawn, the nodes being moved.
//! Everything painted here is read from the view as it stands: a paint reads
//! nothing and renders no drawing.

use alloc::string::String;

use tairix_bezier::flatten::flatten_cubic;
use tairix_bezier::{Affine, Bounds, Path, Point, Segment};
use tairix_controls::{fill_area, withheld};
use tairix_drawing::{GuideAxis, Kind, Node, Shape};
use tairix_geometry::{Rect, Scale};
use tairix_icon::IconArtwork;
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;
use tairix_util::mathf::{ceil, fabs, floor, fmin, ln, power, real, round, round_i32};

use crate::edit;
use crate::glyphs::{Arrived, Lettering};
use crate::input::{handle_point, handles};
use crate::inspector::{text_in, Face, Shown};
use crate::layout::{px, Faces, Layout};
use crate::tiles::TILE;
use crate::tools::{self, Constraint, Tool};
use crate::view::{Gesture, Grip, View};

/// The most points a frame's previews flatten, past which what is left is
/// previewed by its bounds alone.
const PREVIEW_POINTS: usize = 20_000;

/// The least distance between two ruler figures, in device pixels.
const RULER_SPACING: f64 = 60.0;

/// The zoom readout on the tool's bar.
#[must_use]
pub fn zoom_box(layout: &Layout) -> Rect {
    let width = layout.length(72);
    let bar = layout.bar;
    Rect::new(
        bar.right() - i32::try_from(width + layout.length(8)).unwrap_or(0),
        bar.top() + i32::try_from(layout.length(5)).unwrap_or(0),
        width,
        bar.height.saturating_sub(layout.length(10)),
    )
}

/// A zoom as a whole percentage, or to one place below 10%.
#[must_use]
pub fn percent(zoom: f64) -> String {
    let percent = zoom * 100.0;
    if percent < 10.0 {
        alloc::format!("{percent:.1}")
    } else {
        alloc::format!("{}", round_i32(percent))
    }
}

impl View {
    /// Paint the window.
    pub fn render(
        &self,
        surface: &mut Surface,
        layout: &Layout,
        (theme, scale, faces): (&Theme, Scale, Faces),
        artwork: &mut dyn IconArtwork,
    ) {
        let palette = theme.palette();
        let chrome = Color::from(palette.surface_raised);
        if !withheld(surface, layout.tools) {
            fill_area(surface, layout.tools, chrome);
            self.tool_box
                .render(surface, layout.tools, scale, theme, artwork);
        }
        if !withheld(surface, layout.bar) {
            fill_area(surface, layout.bar, chrome);
            self.bar(surface, layout, theme, faces);
        }
        self.rulers(surface, layout, theme, faces);
        if !withheld(surface, layout.canvas) {
            if let Some((x, y)) = layout.canvas.surface_origin() {
                surface.with_clip(x, y, layout.canvas.width, layout.canvas.height, |surface| {
                    self.canvas(surface, layout, theme);
                });
            }
        }
        if !withheld(surface, layout.panel) || !withheld(surface, layout.tabs) {
            let shown = shown_of(
                self.shown(),
                &self.selection,
                (&self.style, self.star.polygon, &self.text),
                self.layer,
            );
            self.inspector
                .render(surface, (layout, scale, theme, faces), &shown);
        }
        if !withheld(surface, layout.horizontal_bar) || !withheld(surface, layout.vertical_bar) {
            fill_area(surface, layout.bar_corner, chrome);
            self.render_bars(surface, layout, scale, theme);
        }
        if !withheld(surface, layout.status) {
            fill_area(surface, layout.status, chrome);
            self.status(surface, layout, theme, faces);
        }
    }

    fn bar(&self, surface: &mut Surface, layout: &Layout, theme: &Theme, faces: Faces) {
        let palette = theme.palette();
        let hint = match self.tool {
            Tool::Select => "Drag to move \u{b7} handles scale, past a corner turns \u{b7} Shift adds, Alt copies",
            Tool::Nodes => "Drag a node or a handle \u{b7} smooth nodes keep their handles in line",
            Tool::Rectangle | Tool::Ellipse => "Drag to draw \u{b7} Shift for even sides \u{b7} Alt from the centre",
            Tool::Star => "Drag out from the centre \u{b7} Shift turns in 15\u{b0} steps",
            Tool::Line => "Drag to draw \u{b7} Shift in 15\u{b0} steps",
            Tool::Pen => "Click for corners, drag for curves \u{b7} click the first node to close \u{b7} Enter ends",
            Tool::Pencil => "Drag to draw freehand",
            Tool::Text => "Click to type \u{b7} click text to edit it \u{b7} Escape ends",
            Tool::Eyedropper => "Click an object to take its style",
            Tool::Zoom => "Click to zoom in, Alt-click out \u{b7} drag a rectangle to fill the window",
            Tool::Hand => "Drag to scroll",
        };
        let inset = i32::try_from(layout.length(10)).unwrap_or(0);
        let height = i32::try_from(faces.label.line_height()).unwrap_or(0);
        let y = layout.bar.top() + (i32::try_from(layout.bar.height).unwrap_or(0) - height) / 2;
        faces.label.draw_text(
            surface,
            layout.bar.left() + inset,
            y,
            hint,
            Color::from(palette.on_surface_muted),
        );
        let zoom = zoom_box(layout);
        fill_area(surface, zoom, Color::from(palette.surface));
        let readout = alloc::format!("{}%", percent(self.viewport.zoom()));
        text_in(
            surface,
            zoom,
            &readout,
            faces,
            Color::from(palette.on_surface),
        );
    }

    fn rulers(&self, surface: &mut Surface, layout: &Layout, theme: &Theme, faces: Faces) {
        let palette = theme.palette();
        for ruler in [layout.top_ruler, layout.left_ruler, layout.corner] {
            fill_area(surface, ruler, Color::from(palette.surface));
        }
        let unit = self.document.drawing().page().unit;
        let per_unit = unit.pixels() * self.viewport.pixels();
        // A figure every 1, 2 or 5 times a power of ten units, far enough
        // apart to read.
        let least = RULER_SPACING / per_unit;
        let decade = power(10.0, floor(ln(least) / core::f64::consts::LN_10));
        let step = [1.0, 2.0, 5.0, 10.0]
            .iter()
            .map(|m| m * decade)
            .find(|step| *step >= least)
            .unwrap_or(decade * 10.0);
        let ink = Color::from(palette.on_surface_muted);
        let origin = self.viewport.origin();
        for (ruler, across) in [(layout.top_ruler, true), (layout.left_ruler, false)] {
            if withheld(surface, ruler) {
                continue;
            }
            let (start, length, offset) = if across {
                (
                    f64::from(ruler.left() - layout.canvas.left()),
                    f64::from(ruler.width),
                    origin.x,
                )
            } else {
                (
                    f64::from(ruler.top() - layout.canvas.top()),
                    f64::from(ruler.height),
                    origin.y,
                )
            };
            let first_mark = floor(((start - offset) / per_unit) / step);
            let last_mark = ceil(((start + length - offset) / per_unit) / step);
            // Figures stand at least the spacing apart, so a ruler holds a
            // bounded count of them however far the canvas has scrolled.
            for mark in 0..whole_count(length / RULER_SPACING) + 2 {
                let index = first_mark + real(mark);
                if index > last_mark {
                    break;
                }
                let value = index * step;
                let at = round_i32(offset + value * per_unit);
                let label = figure(value, step);
                if across {
                    let x = layout.canvas.left() + at;
                    fill_area(surface, Rect::new(x, ruler.top(), 1, ruler.height), ink);
                    faces
                        .status
                        .draw_text(surface, x + 2, ruler.top(), &label, ink);
                } else {
                    let y = layout.canvas.top() + at;
                    fill_area(surface, Rect::new(ruler.left(), y, ruler.width, 1), ink);
                    faces
                        .status
                        .draw_text(surface, ruler.left() + 1, y + 1, &label, ink);
                }
            }
        }
    }

    fn canvas(&self, surface: &mut Surface, layout: &Layout, theme: &Theme) {
        let palette = theme.palette();
        fill_area(surface, layout.canvas, Color::from(palette.desktop));
        let drawing = self.shown();
        let page = self.window_rect(layout, drawing.page().bounds());
        if let Some(page) = page {
            let shadow = Rect::new(page.left() + 3, page.top() + 3, page.width, page.height);
            fill_area(
                surface,
                shadow.intersection(&layout.canvas),
                Color::from(palette.drop_shadow),
            );
            let ground = drawing
                .page()
                .background
                .unwrap_or(tairix_colour::Rgba::new(255, 255, 255, 255));
            fill_area(
                surface,
                page.intersection(&layout.canvas),
                Color::from(ground),
            );
        }
        let origin = self.viewport.origin();
        for key in self.visible_tiles(layout) {
            let Some(tile) = self.tiles.tile(&key) else {
                continue;
            };
            let at = key.origin();
            let x = layout.canvas.left() + round_i32(origin.x + at.x);
            let y = layout.canvas.top() + round_i32(origin.y + at.y);
            if Rect::new(x, y, TILE, TILE)
                .intersection(&layout.canvas)
                .is_empty()
            {
                continue;
            }
            surface.blit(x, y, tile);
        }
        let accent = Color::from(palette.accent);
        if let Some(page) = page {
            outline_rect(surface, page, Color::from(palette.border));
        }
        if self.show.grid {
            self.grid_lines(surface, layout, Color::from(palette.selection_tint));
        }
        for guide in drawing.guides() {
            let colour = Color::from(palette.selection_fill);
            match guide.axis {
                GuideAxis::Horizontal => {
                    let y = round_i32(self.window_at(layout, Point::new(0.0, guide.position)).y);
                    fill_area(
                        surface,
                        Rect::new(layout.canvas.left(), y, layout.canvas.width, 1),
                        colour,
                    );
                }
                GuideAxis::Vertical => {
                    let x = round_i32(self.window_at(layout, Point::new(guide.position, 0.0)).x);
                    fill_area(
                        surface,
                        Rect::new(x, layout.canvas.top(), 1, layout.canvas.height),
                        colour,
                    );
                }
            }
        }
        self.overlay(surface, layout, accent);
    }

    /// The window's device rectangle over `bounds` of the drawing.
    fn window_rect(&self, layout: &Layout, bounds: Bounds) -> Option<Rect> {
        if bounds.is_empty() {
            return None;
        }
        let a = self.window_at(layout, bounds.min);
        let b = self.window_at(layout, bounds.max);
        let left = round_i32(fmin(a.x, b.x));
        let top = round_i32(fmin(a.y, b.y));
        let width = u32::try_from(round_i32(fabs(b.x - a.x))).ok()?;
        let height = u32::try_from(round_i32(fabs(b.y - a.y))).ok()?;
        Some(Rect::new(left, top, width.max(1), height.max(1)))
    }

    fn grid_lines(&self, surface: &mut Surface, layout: &Layout, colour: Color) {
        let grid = self.shown().grid();
        let cell = grid.spacing / f64::from(grid.subdivisions.max(1));
        let step = cell * self.viewport.pixels();
        if step < 6.0 {
            return;
        }
        let origin = self.viewport.origin();
        let first = |at: f64| {
            let into = at - floor(at / step) * step;
            if (0.0..step).contains(&into) {
                into
            } else {
                0.0
            }
        };
        let canvas = layout.canvas;
        let lines = |length: u32| whole_count(f64::from(length) / step) + 1;
        for line in 0..lines(canvas.width) {
            let x = round_i32(first(origin.x) + real(line) * step);
            if x >= px(canvas.width) {
                break;
            }
            fill_area(
                surface,
                Rect::new(canvas.left() + x, canvas.top(), 1, canvas.height),
                colour,
            );
        }
        for line in 0..lines(canvas.height) {
            let y = round_i32(first(origin.y) + real(line) * step);
            if y >= px(canvas.height) {
                break;
            }
            fill_area(
                surface,
                Rect::new(canvas.left(), canvas.top() + y, canvas.width, 1),
                colour,
            );
        }
    }

    /// What the hand is doing, over the tiles.
    fn overlay(&self, surface: &mut Surface, layout: &Layout, accent: Color) {
        let to_window = self.viewport.to_canvas().then(Affine::translate(
            f64::from(layout.canvas.left()),
            f64::from(layout.canvas.top()),
        ));
        self.selection_marks(surface, layout, to_window, accent);
        if self.tool == Tool::Nodes {
            self.node_marks(surface, layout, to_window, accent);
        }
        self.gesture_preview(surface, layout, to_window, accent);
        self.pen_marks(surface, layout, to_window, accent);
        if let Some(typing) = &self.typing {
            self.caret(surface, layout, typing, to_window, accent);
        }
    }

    /// The selection: where a drag would put it while one is under way, its
    /// bounds and, for the select tool, its handles otherwise.
    fn selection_marks(
        &self,
        surface: &mut Surface,
        layout: &Layout,
        to_window: Affine,
        accent: Color,
    ) {
        let drawing = self.shown();
        let mut budget = PREVIEW_POINTS;
        let preview = self
            .gesture
            .as_ref()
            .and_then(|gesture| self.gesture_map(gesture));
        if let Some(map) = preview {
            for path in &self.selection {
                // Past the budget, what is left is the selection's own frame.
                if budget < BOUNDS_POINTS {
                    stroke_bounds(
                        surface,
                        self.selection_bounds(),
                        map.then(to_window),
                        accent,
                    );
                    break;
                }
                let Some(node) = drawing.node_at(path) else {
                    continue;
                };
                let placed = node
                    .transform()
                    .then(edit::parent_space(drawing, path))
                    .then(map)
                    .then(to_window);
                // A path larger than what is left is not even built.
                let outline = match node.kind() {
                    Kind::Shape { shape, .. } if node.path_nodes() < budget => shape.to_path().ok(),
                    _ => None,
                };
                let drawn = outline.is_some_and(|outline| {
                    stroke_path(surface, &outline, placed, accent, &mut budget)
                });
                if !drawn {
                    stroke_bounds(surface, node.local_bounds(), placed, accent);
                    budget = budget.saturating_sub(BOUNDS_POINTS);
                }
            }
        } else if !self.selection.is_empty() {
            let bounds = self.selection_bounds();
            stroke_bounds(surface, bounds, to_window, accent);
            if self.tool == Tool::Select {
                for handle in handles() {
                    let at = device_point(to_window.map(handle_point(bounds, handle)));
                    fill_area(surface, square(layout, at, 7), accent);
                    fill_area(surface, square(layout, at, 5), WHITE);
                }
            }
        }
    }

    /// What the gesture under way would make: a marquee, a zoom area, a new
    /// shape, a freehand line or a guide.
    fn gesture_preview(
        &self,
        surface: &mut Surface,
        layout: &Layout,
        to_window: Affine,
        accent: Color,
    ) {
        let constraint = Constraint {
            even: self.modifiers.shift,
            centred: self.modifiers.alt,
        };
        let mut budget = PREVIEW_POINTS;
        match &self.gesture {
            Some(Gesture::Marquee { from, to, .. }) => {
                stroke_bounds(surface, Bounds::new(*from, *to), to_window, accent);
            }
            Some(Gesture::Zoom { from, to }) => {
                let canvas = Affine::translate(
                    f64::from(layout.canvas.left()),
                    f64::from(layout.canvas.top()),
                );
                stroke_bounds(surface, Bounds::new(*from, *to), canvas, accent);
            }
            Some(Gesture::Create { from, to }) => {
                let shape = match self.tool {
                    Tool::Rectangle => {
                        Some(tools::rectangle(*from, *to, constraint, self.corner_radius))
                    }
                    Tool::Ellipse => Some(tools::ellipse(*from, *to, constraint)),
                    Tool::Star => Some(tools::star(
                        *from,
                        *to,
                        self.star.points,
                        self.star.inner,
                        self.star.polygon,
                        constraint,
                    )),
                    Tool::Line => tools::line(*from, tools::line_end(*from, *to, constraint)).ok(),
                    _ => None,
                };
                if let Some(path) = shape.and_then(|shape: Shape| shape.to_path().ok()) {
                    stroke_path(surface, &path, to_window, accent, &mut budget);
                }
            }
            Some(Gesture::Node { grip, to }) => {
                let moving = self.node_moving(*grip, *to, to_window);
                let outline = moving
                    .as_ref()
                    .and_then(|(moved, placed)| match moved.kind() {
                        Kind::Shape { shape, .. } => {
                            shape.to_path().ok().map(|path| (path, *placed))
                        }
                        _ => None,
                    });
                if let Some((outline, placed)) = outline {
                    stroke_path(surface, &outline, placed, accent, &mut budget);
                }
            }
            Some(Gesture::Freehand { points }) => {
                let mut line = Polyline::new(surface, hairline(layout), accent);
                for point in points {
                    line.to(device_point(to_window.map(*point)));
                }
                line.finish();
            }
            Some(Gesture::Guide { horizontal, at, .. }) => {
                if *horizontal {
                    let y = round_i32(to_window.map(Point::new(0.0, *at)).y);
                    fill_area(
                        surface,
                        Rect::new(layout.canvas.left(), y, layout.canvas.width, 1),
                        accent,
                    );
                } else {
                    let x = round_i32(to_window.map(Point::new(*at, 0.0)).x);
                    fill_area(
                        surface,
                        Rect::new(x, layout.canvas.top(), 1, layout.canvas.height),
                        accent,
                    );
                }
            }
            _ => {}
        }
    }

    /// The path the pen is setting, its nodes marked.
    fn pen_marks(&self, surface: &mut Surface, layout: &Layout, to_window: Affine, accent: Color) {
        if !self.pen.is_empty() {
            let mut nodes = self.pen.clone();
            if let Some(Gesture::PenHandle { to }) = &self.gesture {
                if let Some(last) = nodes.last_mut() {
                    *last = tools::pen_node(last.point, Some(*to));
                }
            }
            if let Some(path) =
                tools::pen_path(&nodes, false).and_then(|shape| shape.to_path().ok())
            {
                let mut budget = PREVIEW_POINTS;
                stroke_path(surface, &path, to_window, accent, &mut budget);
            }
            for node in &nodes {
                let at = device_point(to_window.map(node.point));
                fill_area(surface, square(layout, at, 6), accent);
            }
        }
    }

    /// The path a node drag under way would make of the selected one, and
    /// the map placing it in the window: none for a path past what a preview
    /// is allowed, which its marks alone follow.
    fn node_moving(&self, grip: Grip, to: Point, to_window: Affine) -> Option<(Node, Affine)> {
        let drawing = self.shown();
        let path = self.selection.first()?;
        let node = drawing
            .node_at(path)
            .filter(|node| node.path_nodes() < PREVIEW_POINTS)?;
        let moved = crate::input::moved_node(node, drawing, path, grip, to)?;
        let placed = moved
            .transform()
            .then(edit::parent_space(drawing, path))
            .then(to_window);
        Some((moved, placed))
    }

    /// The selected path's nodes and handles, the one a drag under way holds
    /// where the pointer has it.
    fn node_marks(&self, surface: &mut Surface, layout: &Layout, to_window: Affine, accent: Color) {
        let drawing = self.shown();
        let Some(path) = self.selection.first() else {
            return;
        };
        let Some(node) = drawing.node_at(path) else {
            return;
        };
        let Kind::Shape {
            shape: Shape::Path(shape),
            ..
        } = node.kind()
        else {
            return;
        };
        let held = match &self.gesture {
            Some(Gesture::Node { grip, to }) => {
                crate::input::gripped(node, drawing, path, *grip, *to)
                    .map(|moved| (grip.contour, grip.node, moved))
            }
            _ => None,
        };
        let placed = node
            .transform()
            .then(edit::parent_space(drawing, path))
            .then(to_window);
        let canvas = layout.canvas;
        for (contour_at, contour) in shape.contours.iter().enumerate() {
            for (node_at, node) in contour.nodes.iter().enumerate() {
                let node = match held {
                    Some((contour, index, moved)) if (contour, index) == (contour_at, node_at) => {
                        moved
                    }
                    _ => *node,
                };
                let at = device_point(placed.map(node.point));
                if !canvas.contains(tairix_geometry::Point::new(at.0, at.1)) {
                    continue;
                }
                for handle in [node.before, node.after].into_iter().flatten() {
                    let end = device_point(placed.map(handle));
                    surface.stroke_polyline(&[at, end], hairline(layout), accent);
                    fill_area(surface, square(layout, end, 5), accent);
                }
                fill_area(surface, square(layout, at, 7), accent);
                fill_area(surface, square(layout, at, 5), WHITE);
            }
        }
    }

    /// A caret where typing goes: where the text sets its next character, by
    /// the glyphs that have arrived.
    fn caret(
        &self,
        surface: &mut Surface,
        layout: &Layout,
        typing: &crate::view::Typing,
        to_window: Affine,
        accent: Color,
    ) {
        let drawing = self.shown();
        let Some(node) = drawing.node_at(&typing.path) else {
            return;
        };
        let Kind::Text { text, .. } = node.kind() else {
            return;
        };
        let Ok(pen) = text.pen_at(&Lettering::new(Arrived), typing.caret) else {
            return;
        };
        let placed = node
            .transform()
            .then(edit::parent_space(drawing, &typing.path))
            .then(to_window);
        let top = device_point(placed.map(Point::new(pen.x, pen.y - text.size)));
        let bottom = device_point(placed.map(Point::new(pen.x, pen.y + text.size * 0.2)));
        surface.stroke_polyline(&[top, bottom], 2 * hairline(layout), accent);
    }

    fn status(&self, surface: &mut Surface, layout: &Layout, theme: &Theme, faces: Faces) {
        let palette = theme.palette();
        let inset = i32::try_from(layout.length(8)).unwrap_or(0);
        let height = i32::try_from(faces.status.line_height()).unwrap_or(0);
        let y =
            layout.status.top() + (i32::try_from(layout.status.height).unwrap_or(0) - height) / 2;
        let unit = self.document.drawing().page().unit;
        let pointer = self.drawing_at(layout, self.pointer) / unit.pixels();
        let mut readout = alloc::format!(
            "{}, {} {}",
            round2(pointer.x),
            round2(pointer.y),
            unit.symbol()
        );
        if !self.selection.is_empty() {
            let bounds = self.selection_bounds();
            readout = alloc::format!(
                "{readout} \u{b7} {} selected, {} \u{d7} {} {}",
                self.selection.len(),
                round2(bounds.width() / unit.pixels()),
                round2(bounds.height() / unit.pixels()),
                unit.symbol()
            );
        }
        let readout_width = i32::try_from(faces.status.text_width(&readout)).unwrap_or(0);
        let right = layout.status.right() - inset - readout_width;
        faces.status.draw_text(
            surface,
            right,
            y,
            &readout,
            Color::from(palette.on_surface_muted),
        );
        if let Some(message) = &self.message {
            faces.status.draw_text(
                surface,
                layout.status.left() + inset,
                y,
                message,
                Color::from(palette.on_surface),
            );
        }
    }
}

/// What the inspector shows of `drawing`: the style, shape and text of the
/// first object of `selection`, or the defaults for new objects where it has
/// none, and the layers, the one at `layer` in hand.
pub(crate) fn shown_of<'a>(
    drawing: &'a tairix_drawing::Drawing,
    selection: &[tairix_drawing::NodePath],
    (style, polygon, text): (&'a tairix_drawing::Style, bool, &crate::view::TextDefaults),
    layer: usize,
) -> Shown<'a> {
    let first = selection.first().and_then(|path| drawing.node_at(path));
    let style = first
        .and_then(|node| node.style())
        .map_or(style, |style| &**style);
    let polygon = match first.map(|node| node.kind()) {
        Some(Kind::Shape {
            shape: Shape::Star(star),
            ..
        }) => star.polygon,
        _ => polygon,
    };
    let (family, bold, italic, align) = match first.map(|node| node.kind()) {
        Some(Kind::Text { text: set, .. }) => {
            (&*set.family, set.weight >= 600, set.italic, set.align)
        }
        _ => (text.family, text.bold, text.italic, text.align),
    };
    Shown {
        style,
        layers: drawing.layers(),
        layer,
        locked: drawing.node_at(&[layer]).is_some_and(|row| row.locked()),
        polygon,
        face: Face {
            family,
            bold,
            italic,
            align,
        },
    }
}

/// `value` to two places, trailing zeros dropped.
fn round2(value: f64) -> String {
    let rounded = round(value * 100.0) / 100.0;
    let mut text = alloc::format!("{rounded:.2}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    if text == "-0" {
        text = String::from("0");
    }
    text
}

/// A ruler figure: whole when the step is.
fn figure(value: f64, step: f64) -> String {
    if step >= 1.0 {
        alloc::format!("{}", round_i32(value))
    } else {
        round2(value)
    }
}

/// The mark the selection's handles and a path's nodes are drawn white within.
const WHITE: Color = Color::rgba(255, 255, 255, 255);

/// How many whole times `share` holds one, none for less or for no number.
fn whole_count(share: f64) -> usize {
    usize::try_from(round_i32(floor(share))).unwrap_or(0)
}

/// A line one logical pixel wide, in device pixels.
fn hairline(layout: &Layout) -> i32 {
    i32::try_from(layout.length(1).max(1)).unwrap_or(1)
}

/// A square mark `side` logical pixels across, centred on `at`.
fn square(layout: &Layout, at: (i32, i32), side: u32) -> Rect {
    let side = layout.length(side).max(1);
    let half = i32::try_from(side / 2).unwrap_or(0);
    Rect::new(at.0 - half, at.1 - half, side, side)
}

fn device_point(point: Point) -> (i32, i32) {
    (round_i32(point.x), round_i32(point.y))
}

/// Stroke `path`, placed by `map`, as a hairline, spending no more than
/// `budget` of points: answers whether all of it was drawn, flattening
/// nothing past what is left.
fn stroke_path(
    surface: &mut Surface,
    path: &Path,
    map: Affine,
    colour: Color,
    budget: &mut usize,
) -> bool {
    for subpath in &path.subpaths {
        let mut line = Polyline::new(surface, 1, colour);
        line.left = *budget;
        line.take(map.map(subpath.start));
        for segment in subpath.segments() {
            if line.left == 0 {
                break;
            }
            match segment.transformed(map) {
                Segment::Line(segment) => line.take(segment.p1),
                Segment::Cubic(cubic) => flatten_cubic(cubic, 0.5, |point| line.take(point)),
            }
        }
        *budget = line.left;
        line.finish();
        if *budget == 0 {
            return false;
        }
    }
    true
}

/// How many points of a preview's budget a frame stroked in place of an
/// object counts for.
const BOUNDS_POINTS: usize = 5;

/// The longest run of points stroked at once.
const RUN: usize = 128;

/// One hairline polyline stroked a run of points at a time, each run picking
/// up where the last ended, so a line of any length is drawn without holding
/// its points.
struct Polyline<'s> {
    surface: &'s mut Surface,
    width: i32,
    colour: Color,
    run: [(i32, i32); RUN],
    len: usize,
    /// How many more points [`take`](Self::take) spends.
    left: usize,
}

impl<'s> Polyline<'s> {
    const fn new(surface: &'s mut Surface, width: i32, colour: Color) -> Self {
        Self {
            surface,
            width,
            colour,
            run: [(0, 0); RUN],
            len: 0,
            left: usize::MAX,
        }
    }

    /// The line on to the device point over `point`, while points are left
    /// to spend.
    fn take(&mut self, point: Point) {
        if self.left > 0 {
            self.left -= 1;
            self.to(device_point(point));
        }
    }

    /// The line on to `point`.
    fn to(&mut self, point: (i32, i32)) {
        if self.len == RUN {
            self.stroke();
            self.run[0] = self.run[RUN - 1];
            self.len = 1;
        }
        self.run[self.len] = point;
        self.len += 1;
    }

    fn stroke(&mut self) {
        if self.len > 1 {
            self.surface
                .stroke_polyline(&self.run[..self.len], self.width, self.colour);
        }
    }

    /// Stroke what is left of the line.
    fn finish(mut self) {
        self.stroke();
    }
}

/// Stroke `bounds`, placed by `map`.
fn stroke_bounds(surface: &mut Surface, bounds: Bounds, map: Affine, colour: Color) {
    if bounds.is_empty() {
        return;
    }
    let corners = bounds.corners().map(|corner| device_point(map.map(corner)));
    surface.stroke_polyline(
        &[corners[0], corners[1], corners[2], corners[3], corners[0]],
        1,
        colour,
    );
}

fn outline_rect(surface: &mut Surface, rect: Rect, colour: Color) {
    fill_area(
        surface,
        Rect::new(rect.left(), rect.top(), rect.width, 1),
        colour,
    );
    fill_area(
        surface,
        Rect::new(rect.left(), rect.bottom() - 1, rect.width, 1),
        colour,
    );
    fill_area(
        surface,
        Rect::new(rect.left(), rect.top(), 1, rect.height),
        colour,
    );
    fill_area(
        surface,
        Rect::new(rect.right() - 1, rect.top(), 1, rect.height),
        colour,
    );
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use alloc::sync::Arc;

    use tairix_bezier::{Affine, Path, PathEl, Point, Subpath};
    use tairix_drawing::unkept::Unkept;
    use tairix_drawing::{Ids, Kind, Node, Shape, Style};
    use tairix_raster::{Color, Surface};
    use tairix_window::document::Access;

    use super::{stroke_path, PREVIEW_POINTS};
    use crate::document::{blank, Document};
    use crate::edit;
    use crate::tools;
    use crate::view::{Grip, GripPart, View};

    #[test]
    fn a_stroke_past_its_budget_spends_it_draws_no_more_and_says_so() {
        let mut wavy = Subpath::new(Point::ZERO);
        for step in 0..1000 {
            let at = f64::from(step);
            wavy.elements.push(PathEl::Cubic(
                Point::new(at, 900.0),
                Point::new(at + 1.0, -900.0),
                Point::new(at + 1.0, 0.0),
            ));
        }
        let path = Path {
            subpaths: alloc::vec![wavy],
        };
        let mut surface = Surface::new(64, 64).expect("a surface");
        let mut budget = 100;
        let ink = Color::rgba(0, 0, 0, 255);
        assert!(!stroke_path(
            &mut surface,
            &path,
            Affine::IDENTITY,
            ink,
            &mut budget
        ));
        assert_eq!(budget, 0, "spent, and no more taken");
        let mut room = PREVIEW_POINTS;
        let mut line = Subpath::new(Point::ZERO);
        line.elements.push(PathEl::Line(Point::new(10.0, 10.0)));
        let small = Path {
            subpaths: alloc::vec![line],
        };
        assert!(stroke_path(
            &mut surface,
            &small,
            Affine::IDENTITY,
            ink,
            &mut room
        ));
        assert_eq!(room, PREVIEW_POINTS - 2);
    }

    #[test]
    fn a_node_dragged_is_previewed_where_the_pointer_has_it() {
        let mut ids = Ids::new();
        let drawing = blank(&mut ids).expect("a drawing");
        let nodes = [
            tools::pen_node(Point::new(10.0, 10.0), None),
            tools::pen_node(Point::new(100.0, 10.0), None),
        ];
        let shape = tools::pen_path(&nodes, false).expect("a path");
        let node = Node::new(
            ids.issue(),
            Kind::Shape {
                shape,
                style: Arc::new(Style::default()),
            },
        )
        .expect("a path");
        let (drawing, at) = edit::insert(&drawing, &[0], Arc::new(node)).expect("put");
        let mut view = View::new(
            Document::new(drawing, ids, true),
            String::from("Untitled"),
            Access::Untitled,
            Unkept::NONE,
            1.0,
        );
        view.selection = alloc::vec![at];
        let grip = Grip {
            contour: 0,
            node: 1,
            part: GripPart::Point,
        };
        let (moved, _) = view
            .node_moving(grip, Point::new(150.0, 40.0), Affine::IDENTITY)
            .expect("a preview");
        let Kind::Shape {
            shape: Shape::Path(path),
            ..
        } = moved.kind()
        else {
            panic!("a path");
        };
        assert_eq!(path.contours[0].nodes[1].point, Point::new(150.0, 40.0));
    }
}
