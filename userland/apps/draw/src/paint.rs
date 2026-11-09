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
use tairix_colour::Rgba;
use tairix_controls::{blend_area, fill_area, withheld, Ruler, RulerAxis, StatusBand, StatusTone};
use tairix_drawing::{GradientKind, GuideAxis, Kind, Node, Paint, PathNode, Shape, PAPER_WHITE};
use tairix_geometry::{Rect, Scale};
use tairix_icon::IconArtwork;
use tairix_raster::{Color, Surface};
use tairix_theme::{Palette, Theme};
use tairix_util::mathf::{ceil, clamp, fabs, floor, fmin, real, round, round_i32};

use crate::edit;
use crate::glyphs::{Arrived, Lettering};
use crate::input::{handle_point, handles};
use crate::inspector::{Face, Shown};
use crate::layout::{px, Faces, Layout};
use crate::tiles::{TileKey, TILE};
use crate::tools::{self, Tool};
use crate::view::{Gesture, Grip, Typing, View};
use crate::viewport::readout;

/// The most points a frame's previews flatten, past which what is left is
/// previewed by its bounds alone.
const PREVIEW_POINTS: usize = 20_000;

impl View {
    /// Paint the window.
    pub fn render(
        &self,
        surface: &mut Surface,
        (theme, scale, faces): (&Theme, Scale, Faces),
        artwork: &mut dyn IconArtwork,
    ) {
        let palette = theme.palette();
        let chrome = Color::from(palette.surface_raised);
        if !withheld(surface, self.layout.tools) {
            fill_area(surface, self.layout.tools, chrome);
            self.tool_box
                .render(surface, self.layout.tools, scale, theme, artwork);
        }
        self.rulers(surface, scale, theme);
        if !withheld(surface, self.layout.canvas) {
            if let Some((x, y)) = self.layout.canvas.surface_origin() {
                surface.with_clip(
                    x,
                    y,
                    self.layout.canvas.width,
                    self.layout.canvas.height,
                    |surface| {
                        self.canvas(surface, theme);
                    },
                );
            }
        }
        if !withheld(surface, self.layout.panel) || !withheld(surface, self.layout.tabs) {
            let shown = shown_of(
                self.shown(),
                (&self.selection, self.typed()),
                (&self.style, self.star.polygon, &self.text),
                self.layer,
            );
            self.inspector
                .render(surface, (&self.layout, scale, theme, faces), &shown);
        }
        if !withheld(surface, self.layout.horizontal_bar)
            || !withheld(surface, self.layout.vertical_bar)
        {
            fill_area(surface, self.layout.bar_corner, chrome);
            self.render_bars(surface, scale, theme);
        }
        if !withheld(surface, self.layout.status) {
            self.status(surface, theme, StatusBand::new(theme, scale));
        }
    }

    fn rulers(&self, surface: &mut Surface, scale: Scale, theme: &Theme) {
        fill_area(
            surface,
            self.layout.corner,
            Color::from(theme.palette().surface),
        );
        let unit = self.document.drawing().page().unit;
        let per_unit = unit.pixels() * self.viewport.pixels();
        let origin = self.viewport.origin();
        let along = |ruler: Rect, axis: RulerAxis| match axis {
            RulerAxis::Across => origin.x + f64::from(self.layout.canvas.left() - ruler.left()),
            RulerAxis::Down => origin.y + f64::from(self.layout.canvas.top() - ruler.top()),
        };
        for (ruler, axis) in [
            (self.layout.top_ruler, RulerAxis::Across),
            (self.layout.left_ruler, RulerAxis::Down),
        ] {
            if withheld(surface, ruler) {
                continue;
            }
            if let Some(measure) = Ruler::new(per_unit, along(ruler, axis)) {
                measure.paint(surface, ruler, axis, (scale, theme));
            }
        }
    }

    fn canvas(&self, surface: &mut Surface, theme: &Theme) {
        let palette = theme.palette();
        fill_area(surface, self.layout.canvas, Color::from(palette.desktop));
        let drawing = self.shown();
        let page = self.window_rect(drawing.page().bounds());
        let rule = self.layout.length(1).max(1);
        if let Some(page) = page {
            let fall = px(self.layout.length(SHADOW_FALL));
            let shadow = Rect::new(
                page.left().saturating_add(fall),
                page.top().saturating_add(fall),
                page.width,
                page.height,
            );
            blend_area(
                surface,
                shadow.intersection(&self.layout.canvas),
                palette.drop_shadow,
            );
            fill_area(
                surface,
                page.intersection(&self.layout.canvas),
                Color::from(paper(drawing.page())),
            );
        }
        let stand_in = self.tiles.stand_in(self.show.outline);
        let onto = stand_in
            .as_ref()
            .map(|stand_in| stand_in.onto(self.viewport.pixels()));
        for key in self.visible_tiles() {
            let at = key.origin();
            let (x, y) = self.tile_origin(&key);
            let place = Rect::new(x, y, TILE, TILE).intersection(&self.layout.canvas);
            if place.is_empty() {
                continue;
            }
            if let Some(tile) = self.tiles.tile(&key) {
                surface.blit(x, y, tile);
                continue;
            }
            // Until its own tile lands, the last zoom's tiles stand in,
            // placed as the tile will be so nothing moves when it does.
            let (Some(stand_in), Some(onto), Some((left, top))) =
                (&stand_in, onto, place.surface_origin())
            else {
                continue;
            };
            let to_canvas = Affine::translate(f64::from(x) - at.x, f64::from(y) - at.y);
            surface.with_clip(left, top, place.width, place.height, |surface| {
                surface.blit_transformed(&stand_in.picture, onto.then(to_canvas), 255);
            });
        }
        if let Some(page) = page {
            outline_rect(surface, page, Color::from(palette.border), rule);
        }
        if self.show.grid {
            self.grid_lines(surface, palette.selection_tint, rule);
        }
        for guide in drawing.guides() {
            let colour = palette.selection_fill;
            match guide.axis {
                GuideAxis::Horizontal => {
                    let y = round_i32(self.window_at(Point::new(0.0, guide.position)).y);
                    blend_area(
                        surface,
                        Rect::new(self.layout.canvas.left(), y, self.layout.canvas.width, rule),
                        colour,
                    );
                }
                GuideAxis::Vertical => {
                    let x = round_i32(self.window_at(Point::new(guide.position, 0.0)).x);
                    blend_area(
                        surface,
                        Rect::new(x, self.layout.canvas.top(), rule, self.layout.canvas.height),
                        colour,
                    );
                }
            }
        }
        self.overlay(surface, palette);
    }

    /// Where the tile `key`'s top-left corner stands in the window.
    pub(crate) fn tile_origin(&self, key: &TileKey) -> (i32, i32) {
        let origin = self.viewport.origin();
        let at = key.origin();
        let canvas = self.layout.canvas;
        (
            canvas.left().saturating_add(round_i32(origin.x + at.x)),
            canvas.top().saturating_add(round_i32(origin.y + at.y)),
        )
    }

    /// The window's device rectangle over `bounds` of the drawing.
    pub(crate) fn window_rect(&self, bounds: Bounds) -> Option<Rect> {
        if bounds.is_empty() {
            return None;
        }
        let a = self.window_at(bounds.min);
        let b = self.window_at(bounds.max);
        let left = round_i32(fmin(a.x, b.x));
        let top = round_i32(fmin(a.y, b.y));
        let width = u32::try_from(round_i32(fabs(b.x - a.x))).ok()?;
        let height = u32::try_from(round_i32(fabs(b.y - a.y))).ok()?;
        Some(Rect::new(left, top, width.max(1), height.max(1)))
    }

    /// The grid over the canvas, its lines `rule` device pixels wide.
    fn grid_lines(&self, surface: &mut Surface, colour: Rgba, rule: u32) {
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
        let canvas = self.layout.canvas;
        let lines = |length: u32| whole_count(f64::from(length) / step) + 1;
        for line in 0..lines(canvas.width) {
            let x = round_i32(first(origin.x) + real(line) * step);
            if x >= px(canvas.width) {
                break;
            }
            blend_area(
                surface,
                Rect::new(canvas.left() + x, canvas.top(), rule, canvas.height),
                colour,
            );
        }
        for line in 0..lines(canvas.height) {
            let y = round_i32(first(origin.y) + real(line) * step);
            if y >= px(canvas.height) {
                break;
            }
            blend_area(
                surface,
                Rect::new(canvas.left(), canvas.top() + y, canvas.width, rule),
                colour,
            );
        }
    }

    /// What the hand is doing, over the tiles.
    /// The map from the drawing onto the window the overlay is drawn through.
    pub(crate) fn overlay_map(&self) -> Affine {
        self.viewport.to_canvas().then(canvas_map(&self.layout))
    }

    fn overlay(&self, surface: &mut Surface, palette: &Palette) {
        let to_window = self.overlay_map();
        let pen = Pen::hairline(&self.layout, Color::from(palette.accent));
        self.selection_marks(surface, to_window, pen);
        if self.tool == Tool::Nodes {
            self.node_marks(surface, to_window, pen);
        }
        self.gesture_preview(surface, to_window, pen);
        if self.tool == Tool::Gradient && self.gesture.is_none() {
            self.gradient_marks(surface, to_window, pen);
        }
        self.pen_marks(surface, to_window, pen);
        if let Some(typing) = &self.typing {
            let marked = Color::from(palette.selection_fill);
            self.caret(surface, typing, to_window, (pen, marked));
        }
    }

    /// The selection: where a drag would put it while one is under way, its
    /// bounds and, for the select tool, its handles otherwise.
    fn selection_marks(&self, surface: &mut Surface, to_window: Affine, pen: Pen) {
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
                    stroke_bounds(surface, self.selection_bounds(), map.then(to_window), pen);
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
                    stroke_path(surface, &outline, placed, pen, &mut budget)
                });
                if !drawn {
                    stroke_bounds(surface, node.local_bounds(), placed, pen);
                    budget = budget.saturating_sub(BOUNDS_POINTS);
                }
            }
        } else if !self.selection.is_empty() {
            let bounds = self.selection_bounds();
            stroke_bounds(surface, bounds, to_window, pen);
            if self.tool == Tool::Select {
                for handle in handles() {
                    let at = device_point(to_window.map(handle_point(bounds, handle)));
                    fill_area(surface, square(&self.layout, at, 7), pen.colour);
                    fill_area(surface, square(&self.layout, at, 5), WHITE);
                }
            }
        }
    }

    /// Where the one object selected lays the gradient the Style tab edits:
    /// a linear one from its start to its end, a radial one from its centre
    /// out to its circle.
    fn gradient_marks(&self, surface: &mut Surface, to_window: Affine, pen: Pen) {
        if let Some((course, placed)) = self.gradient_course(to_window) {
            gradient_line(surface, &self.layout, course, placed, pen);
        }
    }

    /// The course of the gradient the Style tab edits, from its start to its
    /// end, and the map placing it in the window.
    fn gradient_course(&self, to_window: Affine) -> Option<((Point, Point), Affine)> {
        let [path] = self.selection.as_slice() else {
            return None;
        };
        let drawing = self.shown();
        let node = drawing.node_at(path)?;
        let Some(Paint::Gradient(gradient)) =
            node.style().map(|style| self.inspector.target().of(style))
        else {
            return None;
        };
        let course = match gradient.kind {
            GradientKind::Linear { start, end } => (start, end),
            GradientKind::Radial { centre, radius, .. } => {
                (centre, centre + Point::new(radius, 0.0))
            }
        };
        let placed = gradient
            .transform
            .then(node.transform())
            .then(edit::parent_space(drawing, path))
            .then(to_window);
        Some((course, placed))
    }

    /// What the gesture under way would make: a marquee, a zoom area, a new
    /// shape, a freehand line or a guide.
    fn gesture_preview(&self, surface: &mut Surface, to_window: Affine, pen: Pen) {
        let mut budget = PREVIEW_POINTS;
        match &self.gesture {
            Some(Gesture::Marquee { from, to, .. }) => {
                stroke_bounds(surface, Bounds::new(*from, *to), to_window, pen);
            }
            Some(Gesture::Zoom { from, to }) => {
                let canvas = canvas_map(&self.layout);
                stroke_bounds(surface, Bounds::new(*from, *to), canvas, pen);
            }
            Some(Gesture::Create { from, to }) => {
                let shape = self.created(*from, *to).and_then(Result::ok);
                if let Some(path) = shape.and_then(|shape| shape.to_path().ok()) {
                    stroke_path(surface, &path, to_window, pen, &mut budget);
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
                    stroke_path(surface, &outline, placed, pen, &mut budget);
                }
            }
            Some(Gesture::Freehand { points }) => {
                let mut line = Polyline::new(surface, pen);
                for point in points {
                    line.to(to_window.map(*point));
                }
                line.finish();
            }
            Some(Gesture::Gradient { from, to }) => {
                let end = tools::line_end(*from, *to, self.constraint());
                gradient_line(surface, &self.layout, (*from, end), to_window, pen);
            }
            Some(Gesture::Guide { horizontal, at, .. }) => {
                if *horizontal {
                    let y = round_i32(to_window.map(Point::new(0.0, *at)).y);
                    fill_area(
                        surface,
                        Rect::new(self.layout.canvas.left(), y, self.layout.canvas.width, 1),
                        pen.colour,
                    );
                } else {
                    let x = round_i32(to_window.map(Point::new(*at, 0.0)).x);
                    fill_area(
                        surface,
                        Rect::new(x, self.layout.canvas.top(), 1, self.layout.canvas.height),
                        pen.colour,
                    );
                }
            }
            _ => {}
        }
    }

    /// The path the pen is setting, its nodes marked.
    fn pen_marks(&self, surface: &mut Surface, to_window: Affine, pen: Pen) {
        if !self.pen.is_empty() {
            let mut nodes = self.pen.clone();
            if let (Some(last), Some(newest)) = (nodes.last_mut(), self.pen_newest()) {
                *last = newest;
            }
            if let Some(path) = tools::pen_path(&nodes, false)
                .ok()
                .flatten()
                .and_then(|shape| shape.to_path().ok())
            {
                let mut budget = PREVIEW_POINTS;
                stroke_path(surface, &path, to_window, pen, &mut budget);
            }
            for node in &nodes {
                let at = device_point(to_window.map(node.point));
                fill_area(surface, square(&self.layout, at, 6), pen.colour);
            }
        }
    }

    /// The newest node of the path the pen is setting, as a drag of its
    /// handle under way shapes it.
    fn pen_newest(&self) -> Option<PathNode> {
        let newest = *self.pen.last()?;
        Some(match &self.gesture {
            Some(Gesture::PenHandle { to }) => tools::pen_node(newest.point, Some(*to)),
            _ => newest,
        })
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
        let moved = crate::input::moved_node(node, drawing, path, grip, to).ok()??;
        let placed = moved
            .transform()
            .then(edit::parent_space(drawing, path))
            .then(to_window);
        Some((moved, placed))
    }

    /// The selected path's nodes and handles — or those of the path the
    /// selected text is set along — the one a drag under way holds where the
    /// pointer has it.
    fn node_marks(&self, surface: &mut Surface, to_window: Affine, pen: Pen) {
        let drawing = self.shown();
        let Some(path) = self.selection.first() else {
            return;
        };
        let Some(node) = drawing.node_at(path) else {
            return;
        };
        let Some(shape) = crate::input::node_path(node) else {
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
        let canvas = self.layout.canvas;
        for (contour_at, contour) in shape.contours.iter().enumerate() {
            for (node_at, node) in contour.nodes.iter().enumerate() {
                let node = match held {
                    Some((contour, index, moved)) if (contour, index) == (contour_at, node_at) => {
                        moved
                    }
                    _ => *node,
                };
                let point = placed.map(node.point);
                let at = device_point(point);
                if !canvas.contains(tairix_geometry::Point::new(at.0, at.1)) {
                    continue;
                }
                for handle in [node.before, node.after].into_iter().flatten() {
                    let end = placed.map(handle);
                    pen.line(surface, point, end);
                    fill_area(
                        surface,
                        square(&self.layout, device_point(end), 5),
                        pen.colour,
                    );
                }
                fill_area(surface, square(&self.layout, at, 7), pen.colour);
                fill_area(surface, square(&self.layout, at, 5), WHITE);
            }
        }
    }

    /// Where typing goes, its marks painted.
    fn caret(
        &self,
        surface: &mut Surface,
        typing: &Typing,
        to_window: Affine,
        (pen, marked): (Pen, Color),
    ) {
        self.typing_marks(typing, to_window, |mark| match mark {
            TypingMark::Band(band) => surface.fill_polygon_subpixel(&band.map(sub_pixel), marked),
            TypingMark::Caret([top, bottom]) => pen.heavier(2).line(surface, top, bottom),
        });
    }

    /// The marks typing shows, in the order they are drawn: a band across
    /// each character selected, then a caret standing across the line where
    /// the text sets its next character — turned with a path it is set along
    /// — by the glyphs that have arrived.
    fn typing_marks(&self, typing: &Typing, to_window: Affine, mut mark: impl FnMut(TypingMark)) {
        let drawing = self.shown();
        let Some(node) = drawing.node_at(&typing.path) else {
            return;
        };
        let Kind::Text { text, .. } = node.kind() else {
            return;
        };
        let placed = node
            .transform()
            .then(edit::parent_space(drawing, &typing.path))
            .then(to_window);
        let glyphs = Lettering::new(Arrived);
        let selected = typing.selection();
        if !selected.is_empty() {
            for (index, range) in text.line_ranges().enumerate() {
                let span = range.start.max(selected.start)..range.end.min(selected.end);
                if span.start >= span.end {
                    continue;
                }
                let Ok(pens) = text.pens(&glyphs, index, range) else {
                    return;
                };
                for pair in pens.windows(2) {
                    if pair[0].offset < span.start || pair[0].offset >= span.end {
                        continue;
                    }
                    let band = [
                        (pair[0], -CARET_ABOVE),
                        (pair[1], -CARET_ABOVE),
                        (pair[1], CARET_BELOW),
                        (pair[0], CARET_BELOW),
                    ]
                    .map(|(pen, across)| {
                        let up = Point::new(-pen.direction.y, pen.direction.x)
                            * (across * pen.face.size);
                        placed.map(pen.point + up)
                    });
                    mark(TypingMark::Band(band));
                }
            }
        }
        let Ok(caret) = text.caret(&glyphs, typing.caret) else {
            return;
        };
        let across = Point::new(-caret.direction.y, caret.direction.x) * caret.size;
        mark(TypingMark::Caret([
            placed.map(caret.pen - across * CARET_ABOVE),
            placed.map(caret.pen + across * CARET_BELOW),
        ]));
    }

    /// What the marks of the gesture under way cover in the window, their
    /// lines' and marks' reach included: where a move during it repaints,
    /// both where they were and where they now are. `None` with no gesture,
    /// or one that marks nothing on the canvas.
    pub(crate) fn gesture_extent(&self) -> Option<Rect> {
        let to_window = self.overlay_map();
        let marked = match self.gesture.as_ref()? {
            Gesture::Pan { .. } => return None,
            gesture @ (Gesture::Move { .. } | Gesture::Scale { .. } | Gesture::Rotate { .. }) => {
                self.selection_marked(self.gesture_map(gesture), to_window)
            }
            Gesture::Node { grip, to } => self
                .node_marked(Some((*grip, *to)), to_window)
                .union(self.preview_marked(to_window)),
            Gesture::PenHandle { .. } => self.pen_end_marked(to_window),
            Gesture::TextSelect => self.typing_marked(to_window),
            Gesture::Marquee { .. }
            | Gesture::Zoom { .. }
            | Gesture::Create { .. }
            | Gesture::Freehand { .. }
            | Gesture::Gradient { .. }
            | Gesture::Guide { .. } => self.preview_marked(to_window),
        };
        self.marks_rect(marked)
    }

    /// What the whole overlay covers in the window, as
    /// [`gesture_extent`](Self::gesture_extent) measures a gesture's marks:
    /// where a change to the selection, the tool in hand or what it is doing
    /// repaints, both where the marks were and where they now are.
    pub(crate) fn overlay_extent(&self) -> Option<Rect> {
        let to_window = self.overlay_map();
        let map = self
            .gesture
            .as_ref()
            .and_then(|gesture| self.gesture_map(gesture));
        let mut marked = self
            .selection_marked(map, to_window)
            .union(self.preview_marked(to_window))
            .union(self.pen_marked(to_window))
            .union(self.typing_marked(to_window));
        if self.tool == Tool::Nodes {
            let held = match &self.gesture {
                Some(Gesture::Node { grip, to }) => Some((*grip, *to)),
                _ => None,
            };
            marked = marked.union(self.node_marked(held, to_window));
        }
        if self.tool == Tool::Gradient && self.gesture.is_none() {
            if let Some(((start, end), placed)) = self.gradient_course(to_window) {
                marked = marked.union(Bounds::new(placed.map(start), placed.map(end)));
            }
        }
        self.marks_rect(marked)
    }

    /// The canvas rectangle `marked`, in window positions, covers once the
    /// lines and marks drawn through it reach past it, or `None` where it
    /// covers none of the canvas.
    pub(crate) fn marks_rect(&self, marked: Bounds) -> Option<Rect> {
        if !marked.is_finite() {
            return None;
        }
        let reach = f64::from(self.layout.length(OVERLAY_REACH));
        let canvas = self.layout.canvas;
        let across = |at: f64| {
            round_i32(clamp(
                at,
                f64::from(canvas.left()),
                f64::from(canvas.right()),
            ))
        };
        let down = |at: f64| {
            round_i32(clamp(
                at,
                f64::from(canvas.top()),
                f64::from(canvas.bottom()),
            ))
        };
        let (left, right) = (
            across(floor(marked.min.x - reach)),
            across(ceil(marked.max.x + reach)),
        );
        let (top, bottom) = (
            down(floor(marked.min.y - reach)),
            down(ceil(marked.max.y + reach)),
        );
        let width = u32::try_from(right.saturating_sub(left)).ok()?;
        let height = u32::try_from(bottom.saturating_sub(top)).ok()?;
        let rect = Rect::new(left, top, width, height);
        (!rect.is_empty()).then_some(rect)
    }

    /// What the selection's marks cover in the window: each object where
    /// `map` — the move, scale or turn a drag under way makes — puts it,
    /// outlined or framed by what it paints, and the frame and handles where
    /// no map does.
    fn selection_marked(&self, map: Option<Affine>, to_window: Affine) -> Bounds {
        if self.selection.is_empty() {
            return Bounds::EMPTY;
        }
        let placed = map.unwrap_or(Affine::IDENTITY).then(to_window);
        let framed = self.selection_bounds().transformed(placed);
        match map {
            None => framed,
            Some(_) => framed.union(self.selection_reach().transformed(placed)),
        }
    }

    /// What the selected path's node marks cover in the window: every node
    /// and handle, the one `held` — a grip dragged to a point — where the
    /// drag has it.
    fn node_marked(&self, held: Option<(Grip, Point)>, to_window: Affine) -> Bounds {
        let drawing = self.shown();
        let Some((path, node)) = self
            .selection
            .first()
            .and_then(|path| Some((path, drawing.node_at(path)?)))
        else {
            return Bounds::EMPTY;
        };
        let Some(shape) = crate::input::node_path(node) else {
            return Bounds::EMPTY;
        };
        let placed = node
            .transform()
            .then(edit::parent_space(drawing, path))
            .then(to_window);
        let moved =
            held.and_then(|(grip, to)| crate::input::gripped(node, drawing, path, grip, to));
        let nodes = shape
            .contours
            .iter()
            .flat_map(|contour| contour.nodes.iter().copied())
            .chain(moved);
        Bounds::from_points(nodes.flat_map(|node| node_points(node).map(|point| placed.map(point))))
    }

    /// What the preview of the gesture under way covers in the window, as
    /// [`gesture_preview`](Self::gesture_preview) draws it.
    fn preview_marked(&self, to_window: Affine) -> Bounds {
        match &self.gesture {
            Some(Gesture::Marquee { from, to, .. }) => {
                Bounds::new(*from, *to).transformed(to_window)
            }
            Some(Gesture::Zoom { from, to }) => {
                Bounds::new(*from, *to).transformed(canvas_map(&self.layout))
            }
            Some(Gesture::Create { from, to }) => self
                .created(*from, *to)
                .and_then(Result::ok)
                .and_then(|shape| shape.to_path().ok())
                .map_or(Bounds::EMPTY, |path| path.bounds().transformed(to_window)),
            Some(Gesture::Node { grip, to }) => self
                .node_moving(*grip, *to, to_window)
                .map_or(Bounds::EMPTY, |(moved, placed)| {
                    moved.local_bounds().transformed(placed)
                }),
            Some(Gesture::Freehand { points }) => {
                Bounds::from_points(points.iter().map(|point| to_window.map(*point)))
            }
            Some(Gesture::Gradient { from, to }) => {
                let end = tools::line_end(*from, *to, self.constraint());
                Bounds::new(to_window.map(*from), to_window.map(end))
            }
            Some(Gesture::Guide { horizontal, at, .. }) => {
                let canvas = self.layout.canvas;
                let (left, right) = (f64::from(canvas.left()), f64::from(canvas.right()));
                let (top, bottom) = (f64::from(canvas.top()), f64::from(canvas.bottom()));
                if *horizontal {
                    let y = to_window.map(Point::new(0.0, *at)).y;
                    Bounds::new(Point::new(left, y), Point::new(right, y))
                } else {
                    let x = to_window.map(Point::new(*at, 0.0)).x;
                    Bounds::new(Point::new(x, top), Point::new(x, bottom))
                }
            }
            _ => Bounds::EMPTY,
        }
    }

    /// What the path the pen is setting covers in the window: its nodes and
    /// their handles, the newest's as a drag of its handle shapes them.
    fn pen_marked(&self, to_window: Affine) -> Bounds {
        let older = self.pen.len().saturating_sub(1);
        let nodes = self
            .pen
            .iter()
            .take(older)
            .copied()
            .chain(self.pen_newest());
        Bounds::from_points(
            nodes.flat_map(|node| node_points(node).map(|point| to_window.map(point))),
        )
    }

    /// What a drag of the pen's newest handle reshapes in the window: the one
    /// stretch of the path ending at that node.
    fn pen_end_marked(&self, to_window: Affine) -> Bounds {
        let (Some(newest), [.., before, _]) = (self.pen_newest(), self.pen.as_slice()) else {
            return Bounds::EMPTY;
        };
        let reshaped = [
            Some(before.point),
            before.after,
            newest.before,
            Some(newest.point),
        ];
        Bounds::from_points(
            reshaped
                .into_iter()
                .flatten()
                .map(|point| to_window.map(point)),
        )
    }

    /// What typing's marks cover in the window.
    fn typing_marked(&self, to_window: Affine) -> Bounds {
        let mut marked = Bounds::EMPTY;
        if let Some(typing) = &self.typing {
            self.typing_marks(typing, to_window, |mark| {
                let corners: &[Point] = match &mark {
                    TypingMark::Band(band) => band,
                    TypingMark::Caret(ends) => ends,
                };
                marked = corners.iter().copied().fold(marked, Bounds::union_point);
            });
        }
        marked
    }

    /// Where the status reads the pointer as being, in hundredths of the
    /// page's unit across and down: a move that leaves it reading the same
    /// repaints nothing.
    pub(crate) fn pointer_reading(&self) -> (f64, f64) {
        let unit = self.document.drawing().page().unit;
        let pointer = self.drawing_at(self.pointer) / unit.pixels();
        (hundredths(pointer.x), hundredths(pointer.y))
    }

    fn status(&self, surface: &mut Surface, theme: &Theme, band: StatusBand) {
        let unit = self.document.drawing().page().unit;
        let (across, down) = self.pointer_reading();
        let at = alloc::format!(
            "{}, {} {}",
            two_places(across),
            two_places(down),
            unit.symbol()
        );
        let mut selected = String::new();
        if !self.selection.is_empty() {
            let bounds = self.selection_bounds();
            selected = alloc::format!(
                "{} selected, {} \u{d7} {} {}",
                self.selection.len(),
                round2(bounds.width() / unit.pixels()),
                round2(bounds.height() / unit.pixels()),
                unit.symbol()
            );
        }
        let (message, tone) = match &self.message {
            Some(message) => (message.as_str(), StatusTone::Ink),
            None => (self.tool.hint(), StatusTone::Muted),
        };
        let zoom = readout(self.viewport.zoom());
        band.paint(
            surface,
            self.layout.status,
            theme,
            &[
                (self.layout.message, message, tone),
                (self.layout.pointer, &at, StatusTone::Muted),
                (self.layout.selected, &selected, StatusTone::Muted),
                (self.layout.zoom, &zoom, StatusTone::Ink),
            ],
        );
    }
}

/// The colour the canvas shows `page` in: its ground laid over white paper,
/// so a translucent ground never leaves the window translucent.
fn paper(page: &tairix_drawing::Page) -> tairix_colour::Rgba {
    page.background
        .map_or(PAPER_WHITE, |ground| ground.over(PAPER_WHITE))
}

/// What the inspector shows of `drawing`: the style, shape and text of the
/// first object of `selection` — of a text being typed, as the character
/// `typed` begins with is set — or the defaults for new objects where it has
/// none, and the layers, the one at `layer` in hand.
pub(crate) fn shown_of<'a>(
    drawing: &'a tairix_drawing::Drawing,
    (selection, typed): (&[tairix_drawing::NodePath], Option<core::ops::Range<usize>>),
    (style, polygon, text): (&'a tairix_drawing::Style, bool, &crate::view::TextDefaults),
    layer: usize,
) -> Shown<'a> {
    let first = selection.first().and_then(|path| drawing.node_at(path));
    let own = first
        .and_then(|node| node.style())
        .map_or(style, |style| &**style);
    let style =
        crate::inspector::typed_style(own, first.map(alloc::sync::Arc::as_ref), typed.clone());
    let polygon = match first.map(|node| node.kind()) {
        Some(Kind::Shape {
            shape: Shape::Star(star),
            ..
        }) => star.polygon,
        _ => polygon,
    };
    let (family, bold, italic, align, along) = match first.map(|node| node.kind()) {
        Some(Kind::Text { text: set, .. }) => {
            let face = set.face(typed.and_then(|range| set.run_for(range)));
            let along = matches!(set.baseline, tairix_drawing::Baseline::Path(_));
            (
                face.family,
                face.weight >= 600,
                face.italic,
                set.align,
                along,
            )
        }
        _ => (text.family, text.bold, text.italic, text.align, false),
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
            along,
        },
        page: *drawing.page(),
    }
}

/// `value` in whole hundredths, the precision the status reads to.
fn hundredths(value: f64) -> f64 {
    round(value * 100.0)
}

/// `value` to two places, trailing zeros dropped.
fn round2(value: f64) -> String {
    two_places(hundredths(value))
}

/// A whole number of `hundredths` to two places, trailing zeros dropped.
fn two_places(hundredths: f64) -> String {
    let mut text = alloc::format!("{:.2}", hundredths / 100.0);
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

/// How far above its baseline the caret and a selection's mark reach, and
/// how far below, in ems of the characters beside them.
const CARET_ABOVE: f64 = 1.0;
const CARET_BELOW: f64 = 0.2;

/// `point`, a window position, in the sub-pixel units an anti-aliased
/// polygon is placed by.
/// How far past the points it is drawn through a line or mark of the overlay
/// can reach, in logical pixels: half the largest mark, a heavy line's
/// weight, and their smoothing and rounding.
const OVERLAY_REACH: u32 = 8;

/// One mark typing shows, in window positions.
enum TypingMark {
    /// A selected character's band, corner by corner.
    Band([Point; 4]),
    /// The caret, from its top to its foot.
    Caret([Point; 2]),
}

/// The map from the canvas's own pixels onto the window.
fn canvas_map(layout: &Layout) -> Affine {
    Affine::translate(
        f64::from(layout.canvas.left()),
        f64::from(layout.canvas.top()),
    )
}

/// The node `node` of a path and the ends of its handles: what its marks are
/// drawn through, and what holds the curves either side of it.
fn node_points(node: PathNode) -> impl Iterator<Item = Point> {
    [Some(node.point), node.before, node.after]
        .into_iter()
        .flatten()
}

fn sub_pixel(point: Point) -> (i32, i32) {
    let unit = f64::from(tairix_raster::SUBPIXEL);
    (round_i32(point.x * unit), round_i32(point.y * unit))
}

/// How far the page's shadow falls below and right of it, in logical pixels.
const SHADOW_FALL: u32 = 3;

/// The mark the selection's handles and a path's nodes are drawn white within.
const WHITE: Color = Color::rgba(255, 255, 255, 255);

/// How many whole times `share` holds one, none for less or for no number.
fn whole_count(share: f64) -> usize {
    usize::try_from(round_i32(floor(share))).unwrap_or(0)
}

/// A square mark `side` logical pixels across, centred on `at`.
fn square(layout: &Layout, at: (i32, i32), side: u32) -> Rect {
    let side = layout.length(side).max(1);
    let half = px(side / 2);
    Rect::new(
        at.0.saturating_sub(half),
        at.1.saturating_sub(half),
        side,
        side,
    )
}

fn device_point(point: Point) -> (i32, i32) {
    (round_i32(point.x), round_i32(point.y))
}

/// How the overlay's lines are drawn: their weight in device pixels and their
/// colour, which its marks are filled with too.
#[derive(Clone, Copy)]
struct Pen {
    weight: i32,
    colour: Color,
}

impl Pen {
    /// A line one logical pixel wide.
    fn hairline(layout: &Layout, colour: Color) -> Self {
        Self {
            weight: i32::try_from(layout.length(1).max(1)).unwrap_or(1),
            colour,
        }
    }

    /// This pen `times` as heavy.
    const fn heavier(self, times: i32) -> Self {
        Self {
            weight: self.weight.saturating_mul(times),
            ..self
        }
    }

    /// The straight line from `a` to `b`, window positions.
    fn line(self, surface: &mut Surface, a: Point, b: Point) {
        let mut line = Polyline::new(surface, self);
        line.to(a);
        line.to(b);
        line.finish();
    }
}

/// The sub-pixel vertex a line `weight` device pixels wide is laid through
/// for the window position `point`: the middle of the pixel the overlay's
/// marks centre on for an odd weight and that pixel's corner for an even one,
/// so a level or upright line covers whole pixels and stays crisp.
fn stroke_vertex(point: Point, weight: i32) -> (i32, i32) {
    let unit = tairix_raster::SUBPIXEL;
    let offset = if weight % 2 == 0 { 0 } else { unit / 2 };
    let (x, y) = device_point(point);
    (
        x.saturating_mul(unit).saturating_add(offset),
        y.saturating_mul(unit).saturating_add(offset),
    )
}

/// Stroke `path`, placed by `map`, with `pen`, spending no more than
/// `budget` of points: answers whether all of it was drawn, flattening
/// nothing past what is left.
fn stroke_path(
    surface: &mut Surface,
    path: &Path,
    map: Affine,
    pen: Pen,
    budget: &mut usize,
) -> bool {
    for subpath in &path.subpaths {
        let mut line = Polyline::new(surface, pen);
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

/// One polyline stroked a run of points at a time, each run picking up where
/// the last ended, so a line of any length is drawn without holding its
/// points.
struct Polyline<'s> {
    surface: &'s mut Surface,
    pen: Pen,
    run: [(i32, i32); RUN],
    len: usize,
    /// How many more points [`take`](Self::take) spends.
    left: usize,
}

impl<'s> Polyline<'s> {
    const fn new(surface: &'s mut Surface, pen: Pen) -> Self {
        Self {
            surface,
            pen,
            run: [(0, 0); RUN],
            len: 0,
            left: usize::MAX,
        }
    }

    /// The line on to `point`, while points are left to spend.
    fn take(&mut self, point: Point) {
        if self.left > 0 {
            self.left -= 1;
            self.to(point);
        }
    }

    /// The line on to the window position `point`.
    fn to(&mut self, point: Point) {
        if self.len == RUN {
            self.stroke();
            self.run[0] = self.run[RUN - 1];
            self.len = 1;
        }
        self.run[self.len] = stroke_vertex(point, self.pen.weight);
        self.len += 1;
    }

    fn stroke(&mut self) {
        if self.len > 1 {
            let weight = self.pen.weight.saturating_mul(tairix_raster::SUBPIXEL);
            self.surface
                .stroke_polyline(&self.run[..self.len], weight, self.pen.colour);
        }
    }

    /// Stroke what is left of the line.
    fn finish(mut self) {
        self.stroke();
    }
}

/// Stroke `bounds`, placed by `map`, with `pen`.
fn stroke_bounds(surface: &mut Surface, bounds: Bounds, map: Affine, pen: Pen) {
    if bounds.is_empty() {
        return;
    }
    let mut line = Polyline::new(surface, pen);
    let corners = bounds.corners();
    for corner in corners.iter().chain(&corners[..1]) {
        line.to(map.map(*corner));
    }
    line.finish();
}

/// A gradient's course from `start` to `end`, mapped onto the window by
/// `to_window`: a line between a handle at either end.
fn gradient_line(
    surface: &mut Surface,
    layout: &Layout,
    (start, end): (Point, Point),
    to_window: Affine,
    pen: Pen,
) {
    let (a, b) = (to_window.map(start), to_window.map(end));
    pen.line(surface, a, b);
    for at in [a, b] {
        let at = device_point(at);
        fill_area(surface, square(layout, at, 7), pen.colour);
        fill_area(surface, square(layout, at, 5), WHITE);
    }
}

/// A rim `weight` pixels thick just inside `rect`.
pub(crate) fn outline_rect(surface: &mut Surface, rect: Rect, colour: Color, weight: u32) {
    let weight = weight.min(rect.width / 2).min(rect.height / 2).max(1);
    let right = rect.right().saturating_sub(px(weight));
    let bottom = rect.bottom().saturating_sub(px(weight));
    let rims = [
        Rect::new(rect.left(), rect.top(), rect.width, weight),
        Rect::new(rect.left(), bottom, rect.width, weight),
        Rect::new(rect.left(), rect.top(), weight, rect.height),
        Rect::new(right, rect.top(), weight, rect.height),
    ];
    for rim in rims {
        fill_area(surface, rim, colour);
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use alloc::sync::Arc;

    use tairix_bezier::{Affine, Bounds, Path, PathEl, Point, Subpath};
    use tairix_drawing::unkept::Unkept;
    use tairix_drawing::{Ids, Kind, Node, Shape, Style};
    use tairix_raster::{Color, Pixel, Surface};
    use tairix_window::document::Access;

    use super::{paper, stroke_bounds, stroke_path, Pen, PREVIEW_POINTS};
    use crate::document::{blank, Document};
    use crate::edit;
    use crate::tools;
    use crate::view::{Grip, GripPart, View};
    use tairix_drawing::PAPER_WHITE;

    #[test]
    fn a_page_is_shown_opaque_whatever_its_ground() {
        let page = |background| tairix_drawing::Page {
            background,
            ..tairix_drawing::Page::of(&tairix_drawing::PAPER[0])
        };
        assert_eq!(paper(&page(None)), PAPER_WHITE, "no ground is white paper");
        let blue = tairix_colour::Rgba::new(0, 0, 255, 255);
        assert_eq!(paper(&page(Some(blue))), blue);
        let shown = paper(&page(Some(tairix_colour::Rgba::new(0, 0, 0, 128))));
        assert_eq!(shown.a, 255, "a translucent ground leaves no hole");
        assert!(
            (126..=128).contains(&shown.r),
            "half black over white is grey: {shown:?}"
        );
    }

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
        let ink = Pen {
            weight: 1,
            colour: Color::rgba(0, 0, 0, 255),
        };
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

    /// The pixel at `(x, y)` of `surface`.
    fn pixel(surface: &Surface, (x, y): (u32, u32)) -> Pixel {
        surface.get(x, y).expect("on the surface")
    }

    #[test]
    fn an_overlay_line_covers_whole_pixels_where_its_window_points_are() {
        let ink = Pixel {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        };
        let clear = Pixel {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        };
        let mut surface = Surface::new(64, 64).expect("a surface");
        let frame = Bounds::new(Point::new(10.0, 10.0), Point::new(40.0, 30.0));
        let hairline = Pen {
            weight: 1,
            colour: Color::rgba(0, 0, 255, 255),
        };
        stroke_bounds(&mut surface, frame, Affine::IDENTITY, hairline);
        for edge in [(25, 10), (40, 20), (25, 30), (10, 20)] {
            assert_eq!(pixel(&surface, edge), ink, "a whole pixel at {edge:?}");
        }
        for beside in [(25, 9), (25, 11), (39, 20), (41, 20), (25, 20)] {
            assert_eq!(pixel(&surface, beside), clear, "nothing at {beside:?}");
        }
        let mut surface = Surface::new(64, 64).expect("a surface");
        hairline
            .heavier(2)
            .line(&mut surface, Point::new(20.0, 5.0), Point::new(20.0, 50.0));
        for (x, covered) in [(18, false), (19, true), (20, true), (21, false)] {
            let shown = pixel(&surface, (x, 30));
            assert_eq!(shown == ink, covered, "column {x}: {shown:?}");
            assert!(shown == ink || shown == clear, "no fringe at column {x}");
        }
    }

    #[test]
    fn a_node_dragged_is_previewed_where_the_pointer_has_it() {
        let mut ids = Ids::new();
        let drawing =
            blank(&mut ids, &crate::preferences::Preferences::default()).expect("a drawing");
        let nodes = [
            tools::pen_node(Point::new(10.0, 10.0), None),
            tools::pen_node(Point::new(100.0, 10.0), None),
        ];
        let shape = tools::pen_path(&nodes, false)
            .expect("held")
            .expect("a path");
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

    #[test]
    fn a_long_message_is_cut_short_of_the_readouts_beside_it() {
        use tairix_controls::StatusBand;
        use tairix_geometry::{Region, Scale};
        use tairix_theme::Theme;
        use tairix_window::docapp::DocumentView;

        let mut ids = Ids::new();
        let drawing =
            blank(&mut ids, &crate::preferences::Preferences::default()).expect("a drawing");
        let mut view = View::new(
            Document::new(drawing, ids, true),
            String::from("Untitled"),
            Access::Untitled,
            Unkept::NONE,
            1.0,
        );
        let theme = Theme::light();
        let faces = crate::layout::Faces::of(&theme, Scale::ONE);
        view.lay_out(1080, 720, &theme, Scale::ONE, faces, &mut Region::new());
        let layout = view.layout;
        let band = StatusBand::new(&theme, Scale::ONE);
        let painted = |view: &View| {
            let mut surface = Surface::new(1080, 720).expect("a window");
            view.status(&mut surface, &theme, band);
            surface
        };
        let quiet = painted(&view);
        view.message = Some(String::from("A message far longer than its room ").repeat(20));
        let loud = painted(&view);
        let readouts = layout.pointer.union(&layout.selected);
        let reached = (layout.status.top()..layout.status.bottom()).any(|y| {
            (readouts.left()..readouts.right()).any(|x| {
                let at = (u32::try_from(x).expect("on"), u32::try_from(y).expect("on"));
                loud.get(at.0, at.1) != quiet.get(at.0, at.1)
            })
        });
        assert!(!reached, "the message stays in its own slot");
        assert!(layout.message.right() <= readouts.left());
    }

    #[test]
    fn a_new_zoom_shows_the_last_zooms_tiles_until_its_own_land() {
        use tairix_geometry::{Region, Scale};
        use tairix_theme::Theme;
        use tairix_window::docapp::DocumentView;

        let mut ids = Ids::new();
        let drawing =
            blank(&mut ids, &crate::preferences::Preferences::default()).expect("a drawing");
        let mut view = View::new(
            Document::new(drawing, ids, true),
            String::from("Untitled"),
            Access::Untitled,
            Unkept::NONE,
            1.0,
        );
        let theme = Theme::light();
        let faces = crate::layout::Faces::of(&theme, Scale::ONE);
        view.lay_out(1080, 720, &theme, Scale::ONE, faces, &mut Region::new());
        let layout = view.layout;
        let red = Color::rgb(220, 20, 30);
        let land = |view: &mut View, colour: Option<Color>| {
            while let Some(job) = view.wanted_render() {
                let tiles = job.keys.iter().map(|key| {
                    let mut tile =
                        Surface::new(crate::tiles::TILE, crate::tiles::TILE).expect("a tile");
                    if let Some(colour) = colour {
                        tile.fill(colour);
                    }
                    (*key, tile)
                });
                view.rendered(job.scene, (&job.keys, tiles), &mut Region::new());
            }
        };
        let centre = |view: &View| {
            let mut surface = Surface::new(1080, 720).expect("a window");
            view.canvas(&mut surface, &theme);
            let (x, y) = (
                layout.canvas.left() + i32::try_from(layout.canvas.width / 2).expect("wide"),
                layout.canvas.top() + i32::try_from(layout.canvas.height / 2).expect("tall"),
            );
            surface
                .get(u32::try_from(x).expect("on"), u32::try_from(y).expect("on"))
                .expect("on the window")
        };
        land(&mut view, Some(red));
        assert_eq!(centre(&view), red.premultiply(), "the first zoom's tiles");
        view.viewport.zoom_at(
            2.0,
            Point::new(
                f64::from(layout.canvas.width) / 2.0,
                f64::from(layout.canvas.height) / 2.0,
            ),
        );
        let asked = view.wanted_render().expect("the new zoom's tiles");
        view.render_refused(&asked.keys);
        assert_eq!(centre(&view), red.premultiply(), "stood in for, scaled");
        land(&mut view, None);
        assert_ne!(
            centre(&view),
            red.premultiply(),
            "its own tiles, drawing nothing, show what lies beneath"
        );
    }
}
