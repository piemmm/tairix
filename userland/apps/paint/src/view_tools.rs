//! What the tools beyond painting and selecting do: dragging and magnifying
//! the view, gradients, cloning, text, polygons and the crop box.

use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{Key, NamedKey};
use tairix_theme::Theme;
use tairix_util::fallible;

use super::{Aim, Compute, Draft, Gesture, Lands, Outcome, Settles, View};
use crate::colour::Ink;
use crate::crop::{set_out, Grab};
use crate::filter::Filter;
use crate::gradient::Gradient;
use crate::layout::Layout;
use crate::mask::Mask;
use crate::shape::{Bounds, Point as Fx, Shape, FX};
use crate::stroke::Stroke;
use crate::text::TextEntry;
use crate::tool::Style;
use crate::transform::Transform;
use crate::viewport::screen_rect;

/// How near a press must land to a crop box's edge to take it, in logical
/// pixels, and how wide its handles are drawn.
pub(crate) const CROP_REACH: u32 = 6;

/// The least a zoom box spans on either axis, in screen pixels, before it
/// is a box rather than a click.
const ZOOM_CLICK: i32 = 4;

/// What laying a gradient is said not to have managed.
const GRADIENT: &str = "lay the gradient";

impl View {
    /// The crop tool's box, in picture pixels.
    #[must_use]
    pub const fn crop_box(&self) -> Option<Bounds> {
        self.crop
    }

    /// The text being typed.
    #[must_use]
    pub const fn text(&self) -> Option<&TextEntry> {
        self.text.as_ref()
    }

    /// The zoom tool's box being dragged, its two corners on screen.
    #[must_use]
    pub const fn zoom_box(&self) -> Option<(Point, Point)> {
        match self.gesture {
            Some(Gesture::ZoomBox { from, to, .. }) => Some((from, to)),
            _ => None,
        }
    }

    /// The gradient being dragged, as it will be laid.
    #[must_use]
    pub fn gradient(&self) -> Option<Gradient> {
        let Some(Gesture::Gradient {
            from,
            to,
            secondary,
        }) = self.gesture
        else {
            return None;
        };
        let inks = if secondary {
            (self.secondary, self.primary)
        } else {
            (self.primary, self.secondary)
        };
        Some(Gradient {
            from,
            to,
            shape: self.options.gradient,
            inks,
        })
    }

    /// Where the clone tool copies from, for its marker: beside the pointer
    /// once a stroke has fixed how far away it lies.
    #[must_use]
    pub fn clone_marker(&self, layout: &Layout) -> Option<Fx> {
        if self.tool != crate::tool::Tool::Clone {
            return None;
        }
        match self.clone_offset {
            Some((dx, dy)) => {
                let at = self.at(layout);
                Some(Fx {
                    x: at.x + dx * FX,
                    y: at.y + dy * FX,
                })
            }
            None => self.clone_from,
        }
    }

    /// Whether a press drags the view: the hand tool, or Space held.
    pub(super) const fn panning(&self) -> bool {
        self.space || matches!(self.tool, crate::tool::Tool::Hand)
    }

    /// Begin dragging the view from where the pointer is.
    pub(super) fn begin_pan(&mut self) {
        self.gesture = Some(Gesture::Pan {
            from: self.pointer,
            scroll: self.viewport.scroll(),
        });
    }

    /// The view dragged on to where the pointer is now.
    pub(super) fn pan_to(
        &mut self,
        (from, scroll): (Point, (u64, u64)),
        layout: &Layout,
        damage: &mut Region,
    ) {
        let back = |at: u64, by: i32| at.saturating_add_signed(-i64::from(by));
        let x = back(scroll.0, self.pointer.x.saturating_sub(from.x));
        let y = back(scroll.1, self.pointer.y.saturating_sub(from.y));
        self.scroll_to(Some(x), Some(y), layout, damage);
    }

    /// Begin a zoom box, or a click, where the pointer is; out with Alt.
    pub(super) fn begin_zoom_box(&mut self) {
        self.gesture = Some(Gesture::ZoomBox {
            from: self.pointer,
            to: self.pointer,
            out: self.modifiers.alt,
        });
    }

    /// The zoom box dragged on to where the pointer is now.
    pub(super) fn zoom_box_to(&mut self, layout: &Layout, damage: &mut Region) {
        let Some(Gesture::ZoomBox { from, to, .. }) = &mut self.gesture else {
            return;
        };
        let before = screen_box(*from, *to);
        *to = self.pointer;
        damage.add(before.intersection(&layout.canvas()));
        damage.add(screen_box(*from, self.pointer).intersection(&layout.canvas()));
    }

    /// The zoom box let go: magnify to fill the canvas with what it holds,
    /// or for a click, step the zoom a rung in or out about it.
    pub(super) fn zoom_box_done(
        &mut self,
        (from, to, out): (Point, Point, bool),
        layout: &Layout,
        damage: &mut Region,
    ) {
        let area = layout.canvas();
        damage.add(screen_box(from, to).intersection(&area));
        let click = (to.x - from.x).abs() < ZOOM_CLICK && (to.y - from.y).abs() < ZOOM_CLICK;
        if click {
            let rung = self.viewport.rung_beside(if out { -1 } else { 1 });
            self.zoom_to(rung, from, layout, damage);
            return;
        }
        let size = self.picture_size();
        let corner = |point: Point| {
            let (x, y) = self.viewport.to_picture(point, size, area);
            (x.div_euclid(FX), y.div_euclid(FX))
        };
        let (a, b) = (corner(from), corner(to));
        let held = Bounds {
            x0: a.0.min(b.0),
            y0: a.1.min(b.1),
            x1: a.0.max(b.0) + 1,
            y1: a.1.max(b.1) + 1,
        };
        if self.viewport.frame(held, size, area) {
            damage.add(area);
            damage.add(layout.zoom());
        }
        self.settle(layout, damage);
    }

    /// Begin a gradient from `at`, its inks swapped when `secondary`.
    pub(super) fn begin_gradient(&mut self, at: Fx, secondary: bool) {
        self.gesture = Some(Gesture::Gradient {
            from: at,
            to: at,
            secondary,
        });
    }

    /// The gradient let go: laid by a worker, unless the drag spanned
    /// nothing.
    pub(super) fn gradient_done(
        &mut self,
        gradient: Gradient,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        damage.add(layout.canvas());
        if !gradient.spans() || !self.editable(layout, damage) {
            return Outcome::none();
        }
        let Some(Ok(canvas)) = self
            .document
            .picture()
            .map(|picture| picture.canvas().try_clone())
        else {
            self.state(
                "There is not enough memory to lay the gradient",
                layout,
                damage,
            );
            return Outcome::none();
        };
        let work = Compute::Gradient {
            canvas,
            gradient,
            clip: self.selection.clone(),
        };
        self.begin_work(
            work,
            Lands::Tiles(Settles::Nothing),
            GRADIENT,
            layout,
            damage,
        )
    }

    /// A press with the clone tool: with Alt, choose where it copies from;
    /// otherwise paint what lies there, at the distance the first stroke
    /// from it set.
    pub(super) fn clone_press(&mut self, at: Fx, layout: &Layout, damage: &mut Region) {
        if self.modifiers.alt {
            self.clone_from = Some(at);
            self.clone_offset = None;
            self.state("Copying from here: paint where it is to go", layout, damage);
            damage.add(layout.canvas());
            return;
        }
        let Some(from) = self.clone_from else {
            self.state("Alt-click where to copy from first", layout, damage);
            return;
        };
        if self.clone_offset.is_none() {
            let ((sx, sy), (dx, dy)) = (from.pixel(), at.pixel());
            self.clone_offset = Some((sx - dx, sy - dy));
        }
        self.begin_stroke(at, false, layout, damage);
    }

    /// A press with the text tool: set down the text being typed if the
    /// press is off it, and begin new text there.
    pub(super) fn text_press(&mut self, at: Fx, layout: &Layout, damage: &mut Region) {
        let pixel = at.pixel();
        let on_it = self.text.as_ref().is_some_and(|entry| {
            let b = entry.bounds();
            (b.x0..b.x1.max(b.x0 + 1)).contains(&pixel.0)
                && (b.y0..b.y1.max(b.y0 + 1)).contains(&pixel.1)
        });
        if on_it {
            return;
        }
        self.commit_text(layout, damage);
        self.text = Some(TextEntry::new(pixel));
        self.damage_text(layout, damage);
    }

    /// The face the text tool sets text in.
    pub(super) fn text_face(&self, theme: &Theme) -> BitmapFont {
        BitmapFont::new(theme.fonts().ui_family(), self.options.text_size)
    }

    /// Set the text being typed again, as its words, size or smoothing now
    /// stand, reporting what it covered and covers.
    pub(super) fn reset_text(&mut self, theme: &Theme, layout: &Layout, damage: &mut Region) {
        self.damage_text(layout, damage);
        let (face, smooth) = (self.text_face(theme), self.smooth(self.kind()));
        let Some(entry) = &mut self.text else {
            return;
        };
        if entry.set(face, smooth).is_err() {
            self.state(
                "There is not enough memory to set that text",
                layout,
                damage,
            );
        }
        self.damage_text(layout, damage);
    }

    /// A key while text is being typed and the picture has the keyboard:
    /// `None` where the key is not the text's.
    pub(super) fn text_key(
        &mut self,
        key: Key,
        theme: &Theme,
        layout: &Layout,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let chord = self.modifiers.ctrl || self.modifiers.alt || self.modifiers.meta;
        let entry = self.text.as_mut()?;
        let changed = match key {
            Key::Named(NamedKey::Escape) => {
                self.damage_text(layout, damage);
                self.text = None;
                return Some(Outcome::none());
            }
            Key::Char(ch) if !chord && !ch.is_control() => {
                if !entry.insert(ch) {
                    self.state("That text holds no more", layout, damage);
                    return Some(Outcome::none());
                }
                true
            }
            Key::Named(NamedKey::Enter) if !chord => entry.insert('\n'),
            Key::Named(NamedKey::Backspace) => entry.backspace(),
            Key::Named(NamedKey::Delete) => entry.delete(),
            Key::Named(NamedKey::Left) => entry.step(false),
            Key::Named(NamedKey::Right) => entry.step(true),
            Key::Named(NamedKey::Home) => {
                entry.to_line_edge(false);
                true
            }
            Key::Named(NamedKey::End) => {
                entry.to_line_edge(true);
                true
            }
            _ => return None,
        };
        if changed {
            self.reset_text(theme, layout, damage);
        }
        Some(Outcome::none())
    }

    /// Set the text being typed down on the picture, as one change.
    pub(super) fn commit_text(&mut self, layout: &Layout, damage: &mut Region) {
        let Some(entry) = self.text.take() else {
            return;
        };
        self.damage_text_of(&entry, layout, damage);
        if entry.is_empty() || !self.reserve(layout, damage) {
            return;
        }
        let kind = self.kind();
        let coat = Self::coat(self.primary, kind, self.smooth(kind));
        let mut stroke = Stroke::new(coat, None, self.selection.clone());
        let Some(canvas) = self.document.canvas_mut() else {
            return;
        };
        let laid = stroke.cover_rows(canvas, 0, entry.bounds(), |y, x, out| entry.row(y, x, out));
        let bounds = stroke.take_damage();
        if laid.is_ok() {
            self.document.record_tiles(stroke.finish());
        } else {
            stroke.revert(canvas);
            self.state(
                "There is not enough memory to set that text",
                layout,
                damage,
            );
        }
        self.damage_picture(bounds, layout, damage);
    }

    /// Report what the text being typed covers, its caret and its frame.
    fn damage_text(&self, layout: &Layout, damage: &mut Region) {
        if let Some(entry) = &self.text {
            self.damage_text_of(entry, layout, damage);
        }
    }

    fn damage_text_of(&self, entry: &TextEntry, layout: &Layout, damage: &mut Region) {
        let (x, top, bottom) = entry.caret();
        let caret = Bounds {
            x0: x - 1,
            y0: top,
            x1: x + 2,
            y1: bottom,
        };
        let framed = entry.bounds().union(&caret);
        self.damage_picture(Some(framed), layout, damage);
    }

    /// A press with the polygon tool: begin one, or place its next corner.
    pub(super) fn polygon_press(
        &mut self,
        at: Fx,
        secondary: bool,
        layout: &Layout,
        scale: Scale,
        damage: &mut Region,
    ) -> Outcome {
        if self.draft.is_some() {
            return self.place_corner(at, layout, scale, damage);
        }
        match fallible::collected(1, core::iter::once(at)) {
            Some(corners) => {
                self.draft = Some(Draft {
                    corners,
                    to: at,
                    aim: Aim::Shape { secondary },
                });
                self.damage_picture(Some(super::input::segment(at, at)), layout, damage);
            }
            None => self.state("There is not enough memory to draw that", layout, damage),
        }
        Outcome::none()
    }

    /// Draw the closed polygon through `corners` as the style says, its inks
    /// swapped when `secondary`.
    pub(super) fn put_polygon(
        &mut self,
        corners: &[Fx],
        secondary: bool,
        layout: &Layout,
        damage: &mut Region,
    ) {
        if corners.len() < 3 || !self.reserve(layout, damage) {
            return;
        }
        let kind = self.kind();
        let smooth = self.smooth(kind);
        let (front, back) = if secondary {
            (self.secondary, self.primary)
        } else {
            (self.primary, self.secondary)
        };
        let coat = |ink: Ink| Self::coat(ink, kind, smooth);
        let (fill, outline) = match self.options.style {
            Style::Outline => (None, Some(coat(front))),
            Style::Filled => (Some(coat(front)), None),
            Style::Both => (Some(coat(back)), Some(coat(front))),
        };
        let Some(first) = fill.or(outline) else {
            return;
        };
        let second = fill.and(outline);
        let radius = i64::from(self.options.size) * FX / 2;
        let mut stroke = Stroke::new(first, second, self.selection.clone());
        let Some(canvas) = self.document.canvas_mut() else {
            return;
        };
        let mut laid = Ok(());
        if fill.is_some() {
            laid = stroke.cover_enclosed(canvas, 0, corners, smooth);
        }
        if outline.is_some() {
            let index = usize::from(fill.is_some());
            let edges = corners.iter().zip(corners.iter().cycle().skip(1));
            for (&a, &b) in edges {
                if laid.is_err() {
                    break;
                }
                laid = stroke.cover(canvas, index, &Shape::Capsule { a, b, radius }, smooth);
            }
        }
        let bounds = stroke.take_damage();
        if laid.is_ok() {
            self.document.record_tiles(stroke.finish());
        } else {
            stroke.revert(canvas);
            self.state("There is not enough memory to draw that", layout, damage);
        }
        self.damage_picture(bounds, layout, damage);
    }

    /// A press with the crop tool: take the box's edge, corner or middle
    /// where it lands on one, and otherwise set a new box out.
    pub(super) fn crop_press(
        &mut self,
        at: Fx,
        layout: &Layout,
        scale: Scale,
        damage: &mut Region,
    ) {
        let size = self.picture_size();
        let area = layout.canvas();
        let pixel = at.pixel();
        if let Some(start) = self.crop {
            let edge = self.viewport.screen_span(start, size, area);
            let reach = i64::from(scale.scale_length(CROP_REACH));
            let pointer = (i64::from(self.pointer.x), i64::from(self.pointer.y));
            if let Some(grab) = Grab::of(edge, pointer, reach) {
                self.gesture = Some(Gesture::CropAdjust {
                    grab,
                    from: pixel,
                    start,
                });
                return;
            }
        }
        let before = self.crop;
        self.crop = Some(set_out(pixel, pixel, Bounds::picture(size.0, size.1)));
        self.gesture = Some(Gesture::CropNew {
            from: pixel,
            before,
        });
        self.crop_moved(before, layout, scale, damage);
    }

    /// The crop box dragged on to pixel `to`.
    pub(super) fn crop_to(
        &mut self,
        to: (i64, i64),
        layout: &Layout,
        scale: Scale,
        damage: &mut Region,
    ) {
        let size = self.picture_size();
        let within = Bounds::picture(size.0, size.1);
        let crop = match self.gesture {
            Some(Gesture::CropNew { from, .. }) => set_out(from, to, within),
            Some(Gesture::CropAdjust { grab, from, start }) => {
                grab.dragged(start, (to.0 - from.0, to.1 - from.1), within)
            }
            _ => return,
        };
        if self.crop != Some(crop) {
            let before = self.crop;
            self.crop = (!crop.is_empty()).then_some(crop);
            self.crop_moved(before, layout, scale, damage);
        }
    }

    /// Report what the crop box moving from `before` changed: both boxes,
    /// with the handles straddling their edges, where it was and is held;
    /// the whole canvas where the veil over what it cuts away comes or goes.
    fn crop_moved(
        &self,
        before: Option<Bounds>,
        layout: &Layout,
        scale: Scale,
        damage: &mut Region,
    ) {
        let area = layout.canvas();
        let (Some(before), Some(after)) = (before, self.crop) else {
            damage.add(area);
            return;
        };
        let reach = i64::from(scale.scale_length(CROP_REACH));
        for held in [before, after] {
            let edge = self.viewport.screen_span(held, self.picture_size(), area);
            damage.add(screen_rect(super::grown(edge, reach)).intersection(&area));
        }
    }

    /// Cut the picture down to the crop box, on a worker.
    pub(super) fn apply_crop(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        let Some(area) = self.crop.take() else {
            return Outcome::none();
        };
        damage.add(layout.canvas());
        let (Ok(x), Ok(y), Ok(width), Ok(height)) = (
            u32::try_from(area.x0),
            u32::try_from(area.y0),
            u32::try_from(area.x1 - area.x0),
            u32::try_from(area.y1 - area.y0),
        ) else {
            return Outcome::none();
        };
        self.transform(
            Transform::Crop {
                x,
                y,
                width,
                height,
            },
            "crop",
            layout,
            damage,
        )
    }

    /// Fill what is selected, or the whole picture, with the primary ink,
    /// as much of each pixel as the selection chooses.
    pub(super) fn fill_selection(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        if !self.editable(layout, damage) {
            return Outcome::none();
        }
        let (width, height) = self.picture_size();
        let Some(chosen) = self
            .selection
            .clone()
            .or_else(|| Mask::rect(Bounds::picture(width, height)))
        else {
            return Outcome::none();
        };
        let Some(Ok(canvas)) = self
            .document
            .picture()
            .map(|picture| picture.canvas().try_clone())
        else {
            self.state("There is not enough memory to fill that", layout, damage);
            return Outcome::none();
        };
        let work = Compute::Clear {
            canvas,
            chosen,
            ink: self.primary,
        };
        self.begin_work(
            work,
            Lands::Tiles(Settles::Nothing),
            "fill that",
            layout,
            damage,
        )
    }

    /// Damage the marker the clone tool draws where it copies from, at
    /// picture point `at`.
    pub(super) fn damage_marker(&self, at: Option<Fx>, layout: &Layout, damage: &mut Region) {
        if let Some(at) = at {
            damage.add(marker_rect(self, at, layout).intersection(&layout.canvas()));
        }
    }
}

/// The screen rectangle a marker at picture point `at` is drawn over.
fn marker_rect(view: &View, at: Fx, layout: &Layout) -> Rect {
    let size = view.picture_size();
    let (x, y) = view.viewport.screen_of((at.x, at.y), size, layout.canvas());
    let (x, y) = (
        i32::try_from(x).unwrap_or(i32::MIN),
        i32::try_from(y).unwrap_or(i32::MIN),
    );
    Rect::new(
        x.saturating_sub(MARKER),
        y.saturating_sub(MARKER),
        MARKER_SIDE,
        MARKER_SIDE,
    )
}

/// How far the clone marker's arms reach from its centre, in screen pixels.
pub(crate) const MARKER: i32 = 6;
const MARKER_SIDE: u32 = 13;

/// The screen rectangle two corners of a dragged box span, both inclusive.
pub(crate) fn screen_box(a: Point, b: Point) -> Rect {
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    let width = u32::try_from(a.x.max(b.x) - x0 + 1).unwrap_or(1);
    let height = u32::try_from(a.y.max(b.y) - y0 + 1).unwrap_or(1);
    Rect::new(x0, y0, width, height)
}

impl View {
    /// The picture as a filter being previewed shows it, if one is.
    #[must_use]
    pub(crate) fn preview_canvas(&self) -> Option<&crate::canvas::Canvas> {
        self.preview
            .as_ref()
            .and_then(|preview| preview.canvas.as_ref())
    }

    /// A palette picture's kind as an adjustment being previewed shows it.
    #[must_use]
    pub(crate) fn preview_kind(&self) -> Option<&crate::canvas::Kind> {
        self.preview
            .as_ref()
            .and_then(|preview| preview.kind.as_ref())
    }

    /// Adjust or filter the picture with `filter`: at once where nothing
    /// sets it, and otherwise through its form, previewed as it moves.
    pub(super) fn adjust(
        &mut self,
        filter: Filter,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        if !self.editable(layout, damage) {
            return Outcome::none();
        }
        let palette = self.kind().palette().is_some();
        if palette && filter.neighbourly() {
            let refusal = alloc::format!(
                "{} needs a colour picture: convert it to millions of colours first",
                filter.label()
            );
            self.state(&refusal, layout, damage);
            return Outcome::none();
        }
        if filter.parameters().is_empty() {
            return self.apply_filter(filter, layout, damage);
        }
        self.modal = Some(super::Modal::Form(alloc::boxed::Box::new(
            crate::dialog::Form::filter(filter),
        )));
        self.preview = Some(super::Previewing {
            filter,
            job: None,
            stale: false,
            asked: None,
            layer: self.active_layer(),
            generation: self.document.generation(),
            canvas: None,
            tiles: None,
            shown: None,
            kind: None,
        });
        damage.add(layout.window());
        self.refresh_preview(layout, damage)
    }

    /// The filter form's settings moved: preview them.
    pub(super) fn filter_moved(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        let Some(super::Modal::Form(form)) = &self.modal else {
            return Outcome::none();
        };
        let (Some(filter), Some(preview)) = (form.filter_answer(), &mut self.preview) else {
            return Outcome::none();
        };
        if preview.filter == filter {
            return Outcome::none();
        }
        preview.filter = filter;
        self.refresh_preview(layout, damage)
    }

    /// Show the preview of the settings held: a palette's at once, a colour
    /// picture's by a worker — one at a time, the last settings asked again
    /// once the one working lands.
    fn refresh_preview(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        let Some(filter) = self.preview.as_ref().map(|preview| preview.filter) else {
            return Outcome::none();
        };
        let mapped =
            match self.kind() {
                crate::canvas::Kind::Indexed {
                    depth,
                    palette,
                    masked,
                } => Some(filter.mapped_palette(palette).map(|palette| {
                    crate::canvas::Kind::Indexed {
                        depth: *depth,
                        palette,
                        masked: *masked,
                    }
                })),
                crate::canvas::Kind::Rgba => None,
            };
        let Some(preview) = &mut self.preview else {
            return Outcome::none();
        };
        if let Some(kind) = mapped {
            preview.kind = kind;
            damage.add(layout.canvas());
            return Outcome::none();
        }
        if preview.job.is_some() {
            preview.stale = true;
            return Outcome::none();
        }
        // Copied only once a worker is to be asked: a slider dragged while one
        // works would otherwise copy the picture's tile table each step.
        let Some(Ok(canvas)) = self
            .document
            .picture()
            .map(|picture| picture.canvas().try_clone())
        else {
            return Outcome::none();
        };
        let job = self.next_job;
        self.next_job += 1;
        preview.job = Some(job);
        preview.stale = false;
        preview.asked = Some(filter);
        preview.generation = self.document.generation();
        let work = Compute::Filter {
            canvas,
            filter,
            clip: self.selection.clone(),
        };
        Outcome::asking(super::Request::Own(super::Own::Compute { job, work }))
    }

    /// Whether job `job` is the preview being worked out.
    pub(super) fn previewing(&self, job: u64) -> bool {
        self.preview
            .as_ref()
            .is_some_and(|preview| preview.job == Some(job))
    }

    /// The preview's answer landed: show it, and ask again where the
    /// settings moved meanwhile.
    pub(super) fn previewed(
        &mut self,
        answer: super::Computed,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let shown = self
            .document
            .picture()
            .map(|picture| picture.canvas().try_clone());
        let Some(preview) = &mut self.preview else {
            return Outcome::none();
        };
        preview.job = None;
        match (answer, shown) {
            (super::Computed::Filtered(Ok(tiles)), Some(Ok(mut canvas))) => {
                for (index, tile) in &tiles {
                    canvas.replace_tile(*index, alloc::sync::Arc::clone(tile));
                }
                preview.canvas = Some(canvas);
                preview.tiles = Some(tiles);
                preview.shown = preview.asked;
                damage.add(layout.canvas());
            }
            _ => self.state("There is not enough memory to preview that", layout, damage),
        }
        let stale = self.preview.as_ref().is_some_and(|preview| preview.stale);
        if stale {
            return self.refresh_preview(layout, damage);
        }
        Outcome::none()
    }

    /// Carry out `filter`: a palette's adjustment as a palette change, a
    /// colour picture's by the preview's own tiles where they show exactly
    /// this, else by a worker.
    pub(super) fn apply_filter(
        &mut self,
        filter: Filter,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let preview = self.preview.take();
        damage.add(layout.canvas());
        if let Some(palette) = self.kind().palette() {
            if let Some(mapped) = filter.mapped_palette(palette) {
                self.change_palette(mapped, layout, damage);
            }
            return Outcome::none();
        }
        let current = (
            self.document.generation(),
            self.active_layer(),
            Some(filter),
        );
        if let Some(preview) =
            preview.filter(|preview| (preview.generation, preview.layer, preview.shown) == current)
        {
            // Tiles that no longer fit, or no room to keep them, leave it to
            // a worker.
            let layer = preview.layer;
            if let Some(Ok(true)) = preview
                .tiles
                .map(|tiles| self.document.adopt_tiles(layer, tiles))
            {
                return Outcome::none();
            }
        }
        let Some(Ok(canvas)) = self
            .document
            .picture()
            .map(|picture| picture.canvas().try_clone())
        else {
            self.state(
                "There is not enough memory to filter the picture",
                layout,
                damage,
            );
            return Outcome::none();
        };
        let work = Compute::Filter {
            canvas,
            filter,
            clip: self.selection.clone(),
        };
        self.begin_work(
            work,
            Lands::Tiles(Settles::Nothing),
            "filter the picture",
            layout,
            damage,
        )
    }
}
