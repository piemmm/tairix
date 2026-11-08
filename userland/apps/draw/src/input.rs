//! What a Draw window does with the pointer and the keyboard.
//!
//! A press is routed to the part of the window under the pointer: the tool
//! box, the inspector, a ruler, or the canvas, where the tool in hand takes
//! it. A drag on the canvas is previewed as outlines over the tiles each
//! frame and lands in the document once, when it ends.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
use tairix_abi::touch::{PinchPhase, PINCH_SCALE_ONE};
use tairix_bezier::{Affine, Bounds, Point};
use tairix_collections::try_arc;
use tairix_controls::ToolbarOutcome;
use tairix_drawing::hit::{hit, within_area};
use tairix_drawing::shape::StarShape;
use tairix_drawing::{
    Baseline, DrawingError, Guide, GuideAxis, Kind, Node, ObjectId, Paint, RunPaint, RunStyle,
    Shape, SharedSlice, SharedStr, Style, Text, TextPath,
};
use tairix_geometry::{Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_theme::Theme;
use tairix_util::mathf::{atan2, exp2, fabs, fmax, round, round_i32};
use tairix_window::docapp::{Outcome, Request};

use crate::edit::{self, Arrange};
use crate::glyphs::{Arrived, Lettering};
use crate::gradient;
use crate::inspector::{Change, LayerCommand, PageChange, Target, TextChange};

/// What makes a paint anew: the paint as it is, and what answers the bounds,
/// in its own space, of the object it paints.
type Repaint<'a> = dyn Fn(&Paint, &dyn Fn() -> tairix_bezier::Bounds) -> Result<Paint, tairix_drawing::DrawingError>
    + 'a;
use crate::layout::Layout;
use crate::tools::{self, Constraint, Tool};
use crate::view::{Gesture, Grip, GripPart, Handle, MenuKind, Own, Typing, View, TYPE_TEXT};

/// What an input comes to.
pub type ViewOutcome = Outcome<Request<MenuKind, Own>>;

/// How far past a corner handle, in logical pixels, a drag turns rather
/// than scales.
const TURN_REACH: u32 = 20;

/// How far, in logical pixels, one wheel detent scrolls.
const DETENT_SCROLL: u32 = 48;

/// `style` stroked with the pattern at `index` of the inspector's dashes, in
/// lengths of its stroke's width.
fn dash(style: &mut Style, index: usize) -> Result<(), tairix_drawing::DrawingError> {
    let width = if style.stroke.width > 0.0 {
        style.stroke.width
    } else {
        1.0
    };
    let pattern = crate::inspector::DASHES[index.min(crate::inspector::DASHES.len() - 1)];
    let mut scaled = [0.0; crate::inspector::MOST_DASHES];
    for (out, dash) in scaled.iter_mut().zip(pattern) {
        *out = dash * width;
    }
    style.stroke.dashes = SharedSlice::try_from_slice(&scaled[..pattern.len()])?;
    Ok(())
}

/// `logical` pixels, in the window's device pixels.
fn reach(layout: &Layout, logical: u32) -> f64 {
    f64::from(layout.length(logical))
}

fn device(point: tairix_geometry::Point) -> Point {
    Point::new(f64::from(point.x), f64::from(point.y))
}

fn canvas_origin(layout: &Layout) -> Point {
    Point::new(
        f64::from(layout.canvas.left()),
        f64::from(layout.canvas.top()),
    )
}

impl View {
    /// The drawing point under the window's device point `at`.
    pub(crate) fn drawing_at(&self, at: Point) -> Point {
        self.viewport
            .drawing_point(at - canvas_origin(&self.layout))
    }

    /// The window device point over the drawing point `point`.
    pub(crate) fn window_at(&self, point: Point) -> Point {
        self.viewport.canvas_point(point) + canvas_origin(&self.layout)
    }

    /// One device pixel, in drawing pixels.
    pub(crate) fn pixel(&self) -> f64 {
        1.0 / self.viewport.pixels()
    }

    /// How near the pointer must come to pick something, in drawing pixels.
    fn grab(&self) -> f64 {
        reach(&self.layout, self.handling.pick) * self.pixel()
    }

    /// The pointer as the drawing sees it, snapped as the window snaps.
    fn snapped(&self, at: Point) -> Point {
        let point = self.drawing_at(at);
        if !self.show.snapping {
            return point;
        }
        crate::snap::snapped(self.shown(), point, self.grab(), self.show.grid)
    }

    /// Feed an input event.
    pub fn on_input(
        &mut self,
        event: &InputEvent,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> ViewOutcome {
        match *event {
            InputEvent::KeyPressed { key, modifiers } => {
                self.modifiers = modifiers;
                self.key(key, modifiers, scale, theme, damage)
            }
            InputEvent::KeyReleased { key, modifiers } => {
                self.modifiers = modifiers;
                if key == Key::Char(' ') {
                    self.spaced = false;
                }
                Outcome::none()
            }
            InputEvent::ModifiersChanged { modifiers } => {
                self.modifiers = modifiers;
                Outcome::none()
            }
            _ => self.pointer_event(event, scale, theme, damage),
        }
    }

    fn pointer_event(
        &mut self,
        event: &InputEvent,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> ViewOutcome {
        if let InputEvent::PointerMoved { to } = *event {
            self.pointer = device(to);
        }
        let over =
            tairix_geometry::Point::new(round_i32(self.pointer.x), round_i32(self.pointer.y));
        let on_bar = [
            self.layout.horizontal_bar,
            self.layout.vertical_bar,
            self.layout.bar_corner,
        ]
        .iter()
        .any(|bar| bar.contains(over));
        let held = self.bar_held();
        if on_bar || held || matches!(event, InputEvent::PointerMoved { .. }) {
            self.bars_input(event, over, (scale, theme), damage);
            if on_bar || held {
                return Outcome::none();
            }
        }
        let inspecting = self.layout.panel.contains(over)
            || self.layout.tabs.contains(over)
            || self.inspector.holds_pointer();
        match *event {
            InputEvent::PointerScrolled { dx, dy } if !inspecting => {
                return self.wheel(dx, dy, damage);
            }
            InputEvent::Pinch {
                phase,
                scale: spread,
                at,
            } => return self.pinch(phase, spread, device(at), damage),
            InputEvent::PointerPressed {
                button: PointerButton::Secondary,
            } if self.gesture.is_none() => {
                return Outcome::asking(Request::Menu {
                    kind: MenuKind::Window,
                    anchor: Rect::new(over.x, over.y, 0, 0),
                });
            }
            _ => {}
        }
        if self.gesture.is_some() {
            return self.drag(event, damage);
        }
        if self.layout.tools.contains(over) || matches!(event, InputEvent::PointerMoved { .. }) {
            if let ToolbarOutcome::Activated(action) =
                self.tool_box
                    .on_pointer(event, self.layout.tools, scale, theme, damage)
            {
                if let Some(&tool) = Tool::ALL.get(action.index) {
                    self.choose_tool(tool);
                    damage.add(self.layout.window);
                }
            }
            if self.layout.tools.contains(over) {
                return Outcome::none();
            }
        }
        if inspecting || self.inspector.was_over(&self.layout) || self.inspector.has_keyboard() {
            let changes = self.inspect(|inspector, shown, layout| {
                inspector.on_pointer(event, (layout, scale, theme), shown, damage)
            });
            for change in changes {
                self.apply_change(change, damage);
            }
            // The keyboard went to the inspector, so typing ends where it is.
            if self.inspector.has_keyboard() && self.typing.is_some() {
                self.finish_typing();
                damage.add(self.layout.canvas);
            }
            if inspecting {
                return Outcome::none();
            }
        }
        match *event {
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => self.primary_press(over, damage),
            InputEvent::PointerPressed {
                button: PointerButton::Middle,
            } if self.layout.canvas.contains(over) => {
                self.gesture = Some(Gesture::Pan { last: self.pointer });
                Outcome::none()
            }
            _ => Outcome::none(),
        }
    }

    /// A primary press at the window point `over` that no control took: a
    /// guide out of a ruler, the tool in hand on the canvas, or the bar.
    fn primary_press(&mut self, over: tairix_geometry::Point, damage: &mut Region) -> ViewOutcome {
        let ruled = self.layout.top_ruler.contains(over) || self.layout.left_ruler.contains(over);
        // Whatever the press begins acts on the drawing as shown.
        if ruled || self.layout.canvas.contains(over) {
            self.land_pending(damage);
        }
        if ruled {
            let horizontal = self.layout.top_ruler.contains(over);
            let at = self.drawing_at(self.pointer);
            self.gesture = Some(Gesture::Guide {
                horizontal,
                at: if horizontal { at.y } else { at.x },
                index: None,
            });
            return Outcome::none();
        }
        if self.layout.canvas.contains(over) {
            return self.press(damage);
        }
        if self.layout.zoom.contains(over) {
            return Outcome::asking(Request::Menu {
                kind: MenuKind::Zoom,
                anchor: self.layout.zoom,
            });
        }
        Outcome::none()
    }

    fn wheel(&mut self, dx: i32, dy: i32, damage: &mut Region) -> ViewOutcome {
        let detents = |units: i32| f64::from(units) / f64::from(SCROLL_UNITS_PER_DETENT);
        let step = reach(&self.layout, DETENT_SCROLL);
        if self.modifiers.ctrl {
            let factor = exp2(-detents(dy) / 4.0);
            let anchor = self.pointer - canvas_origin(&self.layout);
            self.viewport.zoom_at(self.viewport.zoom() * factor, anchor);
        } else if self.modifiers.shift {
            self.viewport.scroll(Point::new(detents(dy) * step, 0.0));
        } else {
            self.viewport
                .scroll(Point::new(detents(dx) * step, detents(dy) * step));
        }
        damage.add(self.layout.window);
        Outcome::none()
    }

    fn pinch(
        &mut self,
        phase: PinchPhase,
        spread: u32,
        at: Point,
        damage: &mut Region,
    ) -> ViewOutcome {
        match phase {
            PinchPhase::Begin => self.pinched = Some(self.viewport.zoom()),
            PinchPhase::Update => {
                if let Some(began) = self.pinched {
                    let factor = f64::from(spread) / f64::from(PINCH_SCALE_ONE);
                    self.viewport
                        .zoom_at(began * factor, at - canvas_origin(&self.layout));
                    damage.add(self.layout.window);
                }
            }
            PinchPhase::End | PinchPhase::Cancel => self.pinched = None,
        }
        Outcome::none()
    }

    /// A primary press on the canvas.
    fn press(&mut self, damage: &mut Region) -> ViewOutcome {
        let point = self.drawing_at(self.pointer);
        let snapped = self.snapped(self.pointer);
        if self.spaced || self.tool == Tool::Hand {
            self.gesture = Some(Gesture::Pan { last: self.pointer });
            return Outcome::none();
        }
        if let Some(index) = self.guide_near() {
            let guide = self.shown().guides()[index];
            self.gesture = Some(Gesture::Guide {
                horizontal: guide.axis == GuideAxis::Horizontal,
                at: guide.position,
                index: Some(index),
            });
            return Outcome::none();
        }
        match self.tool {
            Tool::Select => self.select_press(point, damage),
            Tool::Nodes => {
                if let Some(grip) = self.grip_near() {
                    self.gesture = Some(Gesture::Node { grip, to: point });
                } else {
                    self.pick(point, false, damage);
                }
            }
            Tool::Rectangle | Tool::Ellipse | Tool::Star | Tool::Line => {
                self.gesture = Some(Gesture::Create {
                    from: snapped,
                    to: snapped,
                });
            }
            Tool::Pen => self.pen_press(snapped, damage),
            Tool::Pencil => {
                self.gesture = Some(Gesture::Freehand {
                    points: alloc::vec![point],
                });
            }
            Tool::Text => self.text_press(point, damage),
            Tool::Gradient => {
                if self.selection.is_empty() {
                    self.pick(point, false, damage);
                }
                if !self.selection.is_empty() {
                    self.gesture = Some(Gesture::Gradient {
                        from: snapped,
                        to: snapped,
                    });
                }
            }
            Tool::Eyedropper => self.eyedrop(point, damage),
            Tool::Zoom => {
                let at = self.pointer - canvas_origin(&self.layout);
                self.gesture = Some(Gesture::Zoom { from: at, to: at });
            }
            Tool::Hand => {}
        }
        Outcome::none()
    }

    fn select_press(&mut self, point: Point, damage: &mut Region) {
        if let Some(gesture) = self.handle_near() {
            self.gesture = Some(gesture);
            return;
        }
        let picked = self.pick(point, self.modifiers.shift, damage);
        if picked {
            self.gesture = Some(Gesture::Move {
                from: point,
                to: point,
            });
        } else {
            self.gesture = Some(Gesture::Marquee {
                from: point,
                to: point,
                adding: self.modifiers.shift,
            });
        }
    }

    /// Select the object under `point`, adding it to the selection, or taking
    /// it out, when `toggle`: the object within its layer, a group whole.
    /// Answers whether the pointer is now over something selected.
    pub(crate) fn pick(&mut self, point: Point, toggle: bool, damage: &mut Region) -> bool {
        let hit = hit(self.shown(), point, self.grab(), &Lettering::new(Arrived))
            .ok()
            .flatten();
        let Some(mut path) = hit else {
            if !toggle {
                self.selection.clear();
                damage.add(self.layout.window);
            }
            return false;
        };
        path.truncate(2);
        let locked = self
            .shown()
            .node_at(&path[..1])
            .is_some_and(|layer| layer.locked())
            || self
                .shown()
                .node_at(&path)
                .is_some_and(|node| node.locked());
        if locked {
            return false;
        }
        let held = self.selection.contains(&path);
        if toggle {
            if held {
                self.selection.retain(|selected| *selected != path);
            } else {
                self.selection.push(path.clone());
            }
        } else if !held {
            self.selection = alloc::vec![path.clone()];
        }
        if let Some(&layer) = path.first() {
            self.layer = layer;
        }
        self.refresh_inspector();
        damage.add(self.layout.window);
        self.selection.contains(&path)
    }

    /// The scale or turn handle of the selection under the pointer.
    fn handle_near(&self) -> Option<Gesture> {
        let bounds = self.selection_bounds();
        if bounds.is_empty() {
            return None;
        }
        let point = self.drawing_at(self.pointer);
        for handle in handles() {
            let at = self.window_at(handle_point(bounds, handle));
            let distance = at.distance(self.pointer);
            if distance <= reach(&self.layout, self.handling.pick) {
                return Some(Gesture::Scale {
                    handle,
                    bounds,
                    to: point,
                });
            }
            let corner = handle.across != 0 && handle.down != 0;
            if corner && distance <= reach(&self.layout, TURN_REACH) && !bounds.contains(point) {
                return Some(Gesture::Rotate {
                    centre: bounds.centre(),
                    from: point,
                    to: point,
                });
            }
        }
        None
    }

    /// The guide under the pointer, if one is.
    fn guide_near(&self) -> Option<usize> {
        let point = self.drawing_at(self.pointer);
        let grab = self.grab();
        self.shown()
            .guides()
            .iter()
            .position(|guide| match guide.axis {
                GuideAxis::Horizontal => fabs(point.y - guide.position) <= grab,
                GuideAxis::Vertical => fabs(point.x - guide.position) <= grab,
            })
    }

    /// The node or handle of the selected path, or of the path the
    /// selected text is set along, under the pointer.
    fn grip_near(&self) -> Option<Grip> {
        let path = self.selection.first()?;
        let node = self.shown().node_at(path)?;
        let shape = node_path(node)?;
        let to_drawing = node
            .transform()
            .then(edit::parent_space(self.shown(), path));
        let near = |point: Point| {
            self.window_at(to_drawing.map(point)).distance(self.pointer)
                <= reach(&self.layout, self.handling.pick)
        };
        for (contour_at, contour) in shape.contours.iter().enumerate() {
            for (node_at, path_node) in contour.nodes.iter().enumerate() {
                let parts = [
                    (Some(path_node.point), GripPart::Point),
                    (path_node.before, GripPart::Before),
                    (path_node.after, GripPart::After),
                ];
                for (point, part) in parts {
                    if point.is_some_and(near) {
                        return Some(Grip {
                            contour: contour_at,
                            node: node_at,
                            part,
                        });
                    }
                }
            }
        }
        None
    }

    fn pen_press(&mut self, point: Point, damage: &mut Region) {
        if let Some(first) = self.pen.first() {
            if self.window_at(first.point).distance(self.pointer)
                <= reach(&self.layout, self.handling.pick)
                && self.pen.len() > 1
            {
                let nodes = core::mem::take(&mut self.pen);
                match tools::pen_path(&nodes, true) {
                    Ok(Some(shape)) => {
                        let style = self.style.clone();
                        self.add_shape(shape, &style, "Draw path");
                    }
                    Ok(None) => {}
                    Err(error) => self.say_error("Draw path", error.into()),
                }
                damage.add(self.layout.window);
                return;
            }
        }
        self.pen.push(tools::pen_node(point, None));
        self.gesture = Some(Gesture::PenHandle { to: point });
        damage.add(self.layout.canvas);
    }

    /// A press with the text tool: on the text being typed it puts the caret
    /// there — shift holds the selection's other end — and on another text
    /// begins typing into it there; a drag then selects. Elsewhere it begins
    /// a new text.
    fn text_press(&mut self, point: Point, damage: &mut Region) {
        let at_point = |view: &Self| {
            hit(view.shown(), point, view.grab(), &Lettering::new(Arrived))
                .ok()
                .flatten()
        };
        let within_typing = self
            .typing
            .as_ref()
            .is_some_and(|typing| at_point(self).as_ref() == Some(&typing.path));
        if !within_typing {
            self.finish_typing();
        }
        if let Some(path) = at_point(self) {
            let node = self.shown().node_at(&path);
            if let Some((Kind::Text { .. }, id)) = node.map(|node| (node.kind(), node.id())) {
                if !within_typing {
                    self.selection = alloc::vec![path.clone()];
                    self.typing = Some(Typing::at(path, id, 0));
                }
                self.place_caret(point, within_typing && self.modifiers.shift);
                self.gesture = Some(Gesture::TextSelect);
                self.refresh_inspector();
                damage.add(self.layout.window);
                return;
            }
        }
        if !self.takes_new_objects() {
            damage.add(self.layout.status);
            return;
        }
        let id = self.document.ids().issue();
        let made = self
            .new_text(id, point)
            .map_err(edit::EditError::from)
            .and_then(|node| edit::insert(self.document.drawing(), &[self.layer], node));
        match made {
            Ok((drawing, path)) => {
                self.show_live(drawing, TYPE_TEXT);
                self.selection = alloc::vec![path.clone()];
                self.typing = Some(Typing::at(path, id, 0));
            }
            Err(error) => self.say_error("Text", error),
        }
        damage.add(self.layout.window);
    }

    /// The caret of the text being typed put at the character boundary
    /// nearest `point`, in the drawing; the selection's other end held where
    /// `extend` says so.
    fn place_caret(&mut self, point: Point, extend: bool) {
        let Some(typing) = &self.typing else {
            return;
        };
        let drawing = self.shown();
        let Some(node) = drawing
            .node_at(&typing.path)
            .filter(|node| node.id() == typing.id)
        else {
            return;
        };
        let Kind::Text { text, .. } = node.kind() else {
            return;
        };
        let Some(to_text) = node
            .transform()
            .then(edit::parent_space(drawing, &typing.path))
            .invert()
        else {
            return;
        };
        match text.offset_at(&Lettering::new(Arrived), to_text.map(point)) {
            Ok(at) => {
                if let Some(typing) = &mut self.typing {
                    typing.caret = at;
                    if !extend {
                        typing.anchor = at;
                    }
                }
            }
            Err(error) => self.say_error("Type", error.into()),
        }
    }

    /// A new, empty text `id` at `point`, set as the next text is and in the
    /// style new objects are drawn in, filled where that is only stroked.
    fn new_text(&self, id: ObjectId, point: Point) -> Result<Arc<Node>, DrawingError> {
        let text = Text {
            content: SharedStr::new(),
            family: SharedStr::try_from_str(self.text.family)?,
            size: self.text.size,
            weight: if self.text.bold { 700 } else { 400 },
            italic: self.text.italic,
            letter_spacing: 0.0,
            align: self.text.align,
            baseline: tairix_drawing::Baseline::Lines {
                origin: point,
                line_height: tairix_drawing::DEFAULT_LINE_HEIGHT,
            },
            runs: tairix_drawing::SharedSlice::new(),
        };
        let mut style = self.style.clone();
        if !style.fill.paints() {
            style.fill = style.stroke.paint.clone();
        }
        style.stroke.paint = Paint::None;
        let node = Node::new(
            id,
            Kind::Text {
                text: try_arc(text)?,
                style: try_arc(style)?,
            },
        )?;
        Ok(try_arc(node)?)
    }

    fn eyedrop(&mut self, point: Point, damage: &mut Region) {
        let hit = hit(self.shown(), point, self.grab(), &Lettering::new(Arrived))
            .ok()
            .flatten();
        let Some(style) = hit.and_then(|path| {
            self.shown()
                .node_at(&path)
                .and_then(|node| node.style().cloned())
        }) else {
            return;
        };
        self.style = (*style).clone();
        if !self.selection.is_empty() {
            let taken = (*style).clone();
            let glyphs = Lettering::new(Arrived);
            let result = edit::restyle(
                self.document.drawing(),
                &self.selection,
                &|_, placed| gradient::fitted_style(&taken, placed.bounds()),
                &glyphs,
            );
            self.apply(result, "Take style");
        }
        self.refresh_inspector();
        damage.add(self.layout.window);
    }

    /// A pointer event while a gesture is under way.
    fn drag(&mut self, event: &InputEvent, damage: &mut Region) -> ViewOutcome {
        let point = self.drawing_at(self.pointer);
        let snapped = self.snapped(self.pointer);
        let pixel = self.pixel();
        match event {
            InputEvent::PointerMoved { .. }
                if matches!(self.gesture, Some(Gesture::TextSelect)) =>
            {
                self.place_caret(point, true);
                self.refresh_inspector();
                damage.add(self.layout.window);
                Outcome::none()
            }
            InputEvent::PointerMoved { .. } => {
                match &mut self.gesture {
                    Some(Gesture::Pan { last }) => {
                        let by = *last - self.pointer;
                        *last = self.pointer;
                        self.viewport.scroll(by);
                        damage.add(self.layout.window);
                        return Outcome::none();
                    }
                    Some(
                        Gesture::Marquee { to, .. }
                        | Gesture::Rotate { to, .. }
                        | Gesture::Node { to, .. },
                    ) => *to = point,
                    Some(
                        Gesture::Move { to, .. }
                        | Gesture::Scale { to, .. }
                        | Gesture::Create { to, .. }
                        | Gesture::PenHandle { to },
                    ) => {
                        *to = snapped;
                    }
                    Some(Gesture::Gradient { from, to }) => {
                        *to = snapped;
                        let line = (*from, *to);
                        self.lay_gradient(line, false);
                    }
                    Some(Gesture::Freehand { points }) => {
                        // A line memory will not grow ends where it reached.
                        let apart = points
                            .last()
                            .is_none_or(|last| last.distance(point) >= pixel);
                        if apart && points.try_reserve(1).is_ok() {
                            points.push(point);
                        }
                    }
                    Some(Gesture::Zoom { to, .. }) => {
                        *to = self.pointer - canvas_origin(&self.layout);
                    }
                    Some(Gesture::Guide { horizontal, at, .. }) => {
                        *at = if *horizontal { point.y } else { point.x }
                    }
                    Some(Gesture::TextSelect) | None => {}
                }
                damage.add(self.layout.canvas);
                damage.add(self.layout.status);
                Outcome::none()
            }
            InputEvent::PointerReleased { .. } | InputEvent::PointerCancelled => {
                let cancelled = matches!(event, InputEvent::PointerCancelled);
                if let Some(gesture) = self.gesture.take() {
                    if !cancelled {
                        self.land(gesture);
                    }
                }
                self.refresh_inspector();
                damage.add(self.layout.window);
                Outcome::none()
            }
            _ => Outcome::none(),
        }
    }

    /// The map a move, scale or turn gesture applies, in the drawing's space.
    pub(crate) fn gesture_map(&self, gesture: &Gesture) -> Option<Affine> {
        let even = self.modifiers.shift;
        match gesture {
            Gesture::Move { from, to } => {
                let mut by = *to - *from;
                if even {
                    if fabs(by.x) > fabs(by.y) {
                        by.y = 0.0;
                    } else {
                        by.x = 0.0;
                    }
                }
                Some(Affine::translate(by.x, by.y))
            }
            Gesture::Scale { handle, bounds, to } => {
                let anchor = handle_point(
                    *bounds,
                    Handle {
                        across: -handle.across,
                        down: -handle.down,
                    },
                );
                let grip = handle_point(*bounds, *handle);
                let span = grip - anchor;
                let reach = *to - anchor;
                let ratio = |reach: f64, span: f64| {
                    if fabs(span) > f64::EPSILON {
                        reach / span
                    } else {
                        1.0
                    }
                };
                let mut sx = if handle.across == 0 {
                    1.0
                } else {
                    ratio(reach.x, span.x)
                };
                let mut sy = if handle.down == 0 {
                    1.0
                } else {
                    ratio(reach.y, span.y)
                };
                if even && handle.across != 0 && handle.down != 0 {
                    let uniform = fmax(fabs(sx), fabs(sy));
                    sx = uniform.copysign(sx);
                    sy = uniform.copysign(sy);
                }
                let tiny = 1e-6;
                if fabs(sx) < tiny || fabs(sy) < tiny {
                    return None;
                }
                Some(
                    Affine::translate(-anchor.x, -anchor.y)
                        .then(Affine::scale(sx, sy))
                        .then(Affine::translate(anchor.x, anchor.y)),
                )
            }
            Gesture::Rotate { centre, from, to } => {
                let start = atan2(from.y - centre.y, from.x - centre.x);
                let now = atan2(to.y - centre.y, to.x - centre.x);
                let mut degrees = (now - start).to_degrees();
                if even {
                    degrees = round(degrees / 15.0) * 15.0;
                }
                Some(Affine::rotate_degrees_about(degrees, centre.x, centre.y))
            }
            _ => None,
        }
    }

    /// A gesture ended: land what it made.
    fn land(&mut self, gesture: Gesture) {
        let constraint = Constraint {
            even: self.modifiers.shift,
            centred: self.modifiers.alt,
        };
        match gesture {
            Gesture::Pan { .. } | Gesture::TextSelect => {}
            Gesture::Gradient { from, to } => self.lay_gradient((from, to), true),
            Gesture::Marquee { from, to, adding } => {
                if let Err(error) = self.select_within(Bounds::new(from, to), adding) {
                    self.say_error("Select", error);
                }
            }
            Gesture::Move { .. } | Gesture::Scale { .. } | Gesture::Rotate { .. } => {
                let label = match gesture {
                    Gesture::Move { .. } => "Move",
                    Gesture::Scale { .. } => "Scale",
                    _ => "Rotate",
                };
                if let Some(map) = self
                    .gesture_map(&gesture)
                    .filter(|map| *map != Affine::IDENTITY)
                {
                    let copying = matches!(gesture, Gesture::Move { .. }) && self.modifiers.alt;
                    if copying {
                        self.duplicate_by(map);
                    } else {
                        let result = edit::transform(self.document.drawing(), &self.selection, map);
                        self.apply(result, label);
                    }
                }
            }
            Gesture::Create { from, to } => self.create(from, to, constraint),
            Gesture::Freehand { points } => {
                let tolerance = 1.5 * self.pixel();
                match tools::freehand(&points, tolerance) {
                    Ok(Some(shape)) => {
                        let mut style = self.style.clone();
                        style.fill = Paint::None;
                        self.add_shape(shape, &style, "Draw freehand");
                    }
                    Ok(None) => {}
                    Err(error) => self.say_error("Draw freehand", error.into()),
                }
            }
            Gesture::PenHandle { to } => {
                let pixel = self.pixel();
                if let Some(last) = self.pen.last_mut() {
                    let handle = (to.distance(last.point) > 2.0 * pixel).then_some(to);
                    *last = tools::pen_node(last.point, handle);
                }
            }
            Gesture::Node { grip, to } => self.move_node(grip, to),
            Gesture::Zoom { from, to } => {
                let area = Bounds::new(from, to);
                let grab = reach(&self.layout, self.handling.pick);
                if area.width() < grab && area.height() < grab {
                    let zoom = self
                        .viewport
                        .stepped(self.modifiers.alt || self.modifiers.shift);
                    self.viewport.zoom_at(zoom, to);
                } else {
                    let drawing_area = Bounds::new(
                        self.viewport.drawing_point(area.min),
                        self.viewport.drawing_point(area.max),
                    );
                    let size = Point::new(
                        f64::from(self.layout.canvas.width),
                        f64::from(self.layout.canvas.height),
                    );
                    self.viewport.fit(drawing_area, size, 0.0);
                }
            }
            Gesture::Guide {
                horizontal,
                at,
                index,
            } => self.place_guide(horizontal, at, index),
        }
    }

    /// The objects of the shown, unlocked layers that `area` takes — wholly
    /// within it, or with Alt whatever it touches — selected, or added to
    /// the selection when `adding`.
    fn select_within(&mut self, area: Bounds, adding: bool) -> Result<(), edit::EditError> {
        if !adding {
            self.selection.clear();
        }
        let mut held = Vec::new();
        if !tairix_util::fallible::reserve(&mut held, self.selection.len()) {
            return Err(tairix_drawing::DrawingError::OutOfMemory.into());
        }
        held.extend(self.selection.iter().map(Vec::as_slice));
        held.sort_unstable();
        let drawing = self.shown().clone();
        let mut taken = Vec::new();
        for (index, layer) in drawing.layers().iter().enumerate() {
            if !layer.visible() || layer.locked() {
                continue;
            }
            let Some(children) = layer.children() else {
                continue;
            };
            let mut within = within_area(children, layer.transform(), area, self.modifiers.alt)?;
            within.retain(|&child| {
                children.get(child).is_some_and(|node| !node.locked())
                    && held.binary_search(&[index, child].as_slice()).is_err()
            });
            let paths = edit::paths_in(&[index], within.into_iter())?;
            if !tairix_util::fallible::reserve(&mut taken, paths.len()) {
                return Err(tairix_drawing::DrawingError::OutOfMemory.into());
            }
            taken.extend(paths);
        }
        if !tairix_util::fallible::reserve(&mut self.selection, taken.len()) {
            return Err(tairix_drawing::DrawingError::OutOfMemory.into());
        }
        self.selection.extend(taken);
        Ok(())
    }

    fn duplicate_by(&mut self, map: Affine) {
        let offset = Point::new(map.e, map.f);
        let mut ids = self.document.ids().clone();
        match edit::duplicate(self.document.drawing(), &self.selection, offset, &mut ids) {
            Ok((drawing, placed)) => {
                *self.document.ids() = ids;
                self.commit(drawing, "Duplicate");
                self.select(placed);
            }
            Err(error) => self.say_error("Duplicate", error),
        }
    }

    fn create(&mut self, from: Point, to: Point, constraint: Constraint) {
        if from.distance(to) < 2.0 * self.pixel() {
            return;
        }
        let (shape, label) = match self.tool {
            Tool::Rectangle => (
                Ok(tools::rectangle(from, to, constraint, self.corner_radius)),
                "Draw rectangle",
            ),
            Tool::Ellipse => (Ok(tools::ellipse(from, to, constraint)), "Draw ellipse"),
            Tool::Star => (
                Ok(tools::star(
                    from,
                    to,
                    self.star.points,
                    self.star.inner,
                    self.star.polygon,
                    constraint,
                )),
                "Draw star",
            ),
            Tool::Line => (
                tools::line(from, tools::line_end(from, to, constraint)),
                "Draw line",
            ),
            _ => return,
        };
        let mut style = self.style.clone();
        if self.tool == Tool::Line {
            style.fill = Paint::None;
        }
        match shape {
            Ok(shape) => self.add_shape(shape, &style, label),
            Err(error) => self.say_error(label, error.into()),
        }
    }

    fn move_node(&mut self, grip: Grip, to: Point) {
        let Some(path) = self.selection.first().cloned() else {
            return;
        };
        let drawing = self.document.drawing().clone();
        let Some(node) = drawing.node_at(&path) else {
            return;
        };
        let moved = match moved_node(node, &drawing, &path, grip, to) {
            Ok(Some(moved)) => moved,
            Ok(None) => return,
            Err(error) => return self.say_error("Move node", error.into()),
        };
        let result = try_arc(moved)
            .map_err(DrawingError::from)
            .and_then(|moved| drawing.with_node_at(&path, moved))
            .map_err(edit::EditError::from);
        self.apply(result, "Move node");
    }

    fn place_guide(&mut self, horizontal: bool, at: f64, index: Option<usize>) {
        let over =
            tairix_geometry::Point::new(round_i32(self.pointer.x), round_i32(self.pointer.y));
        let placed = self.layout.canvas.contains(over).then_some(Guide {
            axis: if horizontal {
                GuideAxis::Horizontal
            } else {
                GuideAxis::Vertical
            },
            position: at,
        });
        let label = match (index, placed) {
            (None, _) => "Add guide",
            (Some(_), Some(_)) => "Move guide",
            (Some(_), None) => "Remove guide",
        };
        let result =
            with_guide(self.document.drawing(), index, placed).map_err(edit::EditError::from);
        self.apply(result, label);
    }

    /// A key pressed: typing takes it first, then a field of the inspector
    /// holding the keyboard, then the command it is the chord of, then the
    /// canvas.
    fn key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> ViewOutcome {
        if self.typing.is_some() && !modifiers.ctrl {
            self.type_key(key, modifiers.shift, damage);
            return Outcome::none();
        }
        if let (Some(typing), true, Key::Char('a' | 'A')) = (&mut self.typing, modifiers.ctrl, &key)
        {
            let length = self
                .live
                .as_ref()
                .map_or_else(|| self.document.drawing(), |live| &live.drawing)
                .node_at(&typing.path)
                .and_then(|node| match node.kind() {
                    Kind::Text { text, .. } => Some(text.content.len()),
                    _ => None,
                });
            if let Some(length) = length {
                typing.anchor = 0;
                typing.caret = length;
                self.refresh_inspector();
                damage.add(self.layout.window);
                return Outcome::none();
            }
        }
        if self.inspector.has_keyboard() {
            if !modifiers.ctrl {
                let changes = self.inspect(|inspector, shown, layout| {
                    inspector.on_key(key, modifiers, (layout, scale, theme), shown, damage)
                });
                for change in changes {
                    self.apply_change(change, damage);
                }
                return Outcome::none();
            }
            let released = self.inspect(|inspector, shown, layout| {
                inspector.release_keyboard((layout, scale, theme), shown, damage)
            });
            if let Some(change) = released {
                self.apply_change(change, damage);
            }
        }
        let backing_up = matches!(key, Key::Named(NamedKey::Delete | NamedKey::Backspace))
            && self.tool == Tool::Pen
            && !self.pen.is_empty();
        if backing_up {
            self.pen.pop();
            damage.add(self.layout.canvas);
            return Outcome::none();
        }
        if let Some(command) = self.chord(key, modifiers) {
            return self.carry_out(command, damage);
        }
        damage.add(self.layout.window);
        match key {
            Key::Char(' ') => self.spaced = true,
            Key::Named(NamedKey::Escape) => {
                self.pen.clear();
                self.gesture = None;
                self.selection.clear();
            }
            Key::Named(NamedKey::Enter) => self.finish_pen(),
            Key::Named(NamedKey::Left) => self.nudge(-1.0, 0.0),
            Key::Named(NamedKey::Right) => self.nudge(1.0, 0.0),
            Key::Named(NamedKey::Up) => self.nudge(0.0, -1.0),
            Key::Named(NamedKey::Down) => self.nudge(0.0, 1.0),
            Key::Char(character) if !modifiers.ctrl && !modifiers.alt && !modifiers.meta => {
                if let Some(tool) = Tool::for_key(character) {
                    self.choose_tool(tool);
                }
            }
            _ => {}
        }
        Outcome::none()
    }

    fn nudge(&mut self, across: f64, down: f64) {
        if self.selection.is_empty() {
            return;
        }
        let step = if self.modifiers.shift {
            self.handling.nudge * 10.0
        } else {
            self.handling.nudge
        };
        let result = edit::transform(
            self.document.drawing(),
            &self.selection,
            Affine::translate(across * step, down * step),
        );
        self.apply(result, "Nudge");
    }

    pub(crate) fn delete_selection(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        let result = edit::remove(self.document.drawing(), &self.selection);
        if self.apply(result, "Delete") {
            self.selection.clear();
        }
    }

    pub(crate) fn arrange(&mut self, arrange: Arrange) {
        match edit::arrange(self.document.drawing(), &self.selection, arrange) {
            Ok((drawing, placed)) => {
                self.commit(drawing, "Arrange");
                self.select(placed);
            }
            Err(error) => self.say_error("Arrange", error),
        }
    }

    /// A key typed into text: an edit replaces the selection — or at a bare
    /// caret adds or takes away one character — the runs following the
    /// characters, and shows the text afresh to land as one step when typing
    /// ends. A move of the caret changes no drawing; with `extend` it carries
    /// the selection with it.
    fn type_key(&mut self, key: Key, extend: bool, damage: &mut Region) {
        let Some(typing) = self.typing.clone() else {
            return;
        };
        let drawing = self.shown().clone();
        let typed = drawing
            .node_at(&typing.path)
            .filter(|node| node.id() == typing.id);
        let Some((node, Kind::Text { text, .. })) = typed.map(|node| (node, node.kind())) else {
            self.typing = None;
            return;
        };
        let content = &*text.content;
        let caret = char_start(content, typing.caret);
        let anchor = char_start(content, typing.anchor);
        let selected = caret.min(anchor)..caret.max(anchor);
        let before = |at: usize| {
            content[..at]
                .chars()
                .next_back()
                .map_or(at, |c| at - c.len_utf8())
        };
        let after = |at: usize| {
            content[at..]
                .chars()
                .next()
                .map_or(at, |c| at + c.len_utf8())
        };
        let along = matches!(text.baseline, tairix_drawing::Baseline::Path(_));
        let mut spelled = [0; 4];
        let (edit, moved) = match key {
            Key::Named(NamedKey::Escape) => {
                self.finish_typing();
                damage.add(self.layout.window);
                return;
            }
            // A text along a path is one line.
            Key::Named(NamedKey::Enter) if along => return,
            Key::Named(NamedKey::Enter) => (Some((selected, "\n")), caret),
            Key::Named(NamedKey::Backspace) if selected.is_empty() => {
                (Some((before(caret)..caret, "")), caret)
            }
            Key::Named(NamedKey::Delete) if selected.is_empty() => {
                (Some((caret..after(caret), "")), caret)
            }
            Key::Named(NamedKey::Backspace | NamedKey::Delete) => (Some((selected, "")), caret),
            Key::Named(NamedKey::Left) if !extend && !selected.is_empty() => (None, selected.start),
            Key::Named(NamedKey::Right) if !extend && !selected.is_empty() => (None, selected.end),
            Key::Named(NamedKey::Left) => (None, before(caret)),
            Key::Named(NamedKey::Right) => (None, after(caret)),
            Key::Named(NamedKey::Home) => {
                (None, content[..caret].rfind('\n').map_or(0, |at| at + 1))
            }
            Key::Named(NamedKey::End) => (
                None,
                content[caret..]
                    .find('\n')
                    .map_or(content.len(), |at| caret + at),
            ),
            Key::Char(character) if !character.is_control() => {
                let spelled: &str = character.encode_utf8(&mut spelled);
                (Some((selected, spelled)), caret)
            }
            _ => return,
        };
        damage.add(self.layout.window);
        let Some((range, with)) =
            edit.filter(|(range, with)| !range.is_empty() || !with.is_empty())
        else {
            let anchor = if extend { anchor } else { moved };
            self.typing = Some(Typing {
                caret: moved,
                anchor,
                ..typing
            });
            self.refresh_inspector();
            return;
        };
        let caret = range.start + with.len();
        let result = text
            .replaced(range, with)
            .map_err(edit::EditError::from)
            .and_then(|text| retyped(&drawing, &typing.path, node, text));
        match result {
            Ok(drawing) => {
                self.show_live(drawing, TYPE_TEXT);
                self.typing = Some(Typing::at(typing.path, typing.id, caret));
            }
            Err(error) => self.say_error("Type", error),
        }
        self.refresh_inspector();
    }

    /// Carry out what the inspector changed.
    pub(crate) fn apply_change(&mut self, change: Change, damage: &mut Region) {
        damage.add(self.layout.window);
        match change {
            Change::Paint {
                target,
                paint,
                settled,
            } => self.paint_with(target, &paint, settled),
            Change::Width { width, settled } => {
                self.restyle("Stroke width", settled, &|style| style.stroke.width = width);
            }
            Change::Join(join) => self.restyle("Join", true, &|style| style.stroke.join = join),
            Change::Cap(cap) => self.restyle("Cap", true, &|style| style.stroke.cap = cap),
            Change::Dash(index) => {
                self.restyle_or_refuse("Dashes", true, &|style| dash(style, index));
            }
            Change::Head { end, shape } => self.restyle("Arrowhead", true, &|style| {
                let head = if end {
                    &mut style.stroke.end
                } else {
                    &mut style.stroke.start
                };
                head.shape = shape;
            }),
            Change::EvenOdd(even_odd) => self.restyle("Fill rule", true, &|style| {
                style.fill_rule = if even_odd {
                    tairix_bezier::FillRule::EvenOdd
                } else {
                    tairix_bezier::FillRule::NonZero
                };
            }),
            Change::Opacity { opacity, settled } => self.set_opacity(opacity, settled),
            Change::Layer(command) => {
                if command != LayerCommand::Choose(self.layer) {
                    self.land_pending(damage);
                }
                self.layer_command(command);
            }
            Change::Place { index, value } => self.place_selection(index, value),
            Change::Align(align) => self.align_selection(align),
            Change::Distribute(down) => {
                let glyphs = Lettering::new(Arrived);
                let result = edit::distribute(self.edit_base(), &self.selection, down, &glyphs);
                self.take_change(result, "Distribute", true);
            }
            Change::Corners(radius) => self.round_corners(radius),
            Change::Points(points) => {
                self.star.points = points;
                self.reshape_star("Points", &|star| star.points = points);
            }
            Change::Inner(inner) => {
                let inner = inner.clamp(0.01, 1.0);
                self.star.inner = inner;
                self.reshape_star("Inner radius", &|star| star.inner_ratio = inner);
            }
            Change::Polygon(polygon) => {
                self.star.polygon = polygon;
                self.reshape_star("Polygon", &|star| star.polygon = polygon);
            }
            Change::Text(change) => self.change_text(change),
            Change::Offset(distance) => self.offset_distance = distance,
            Change::Smoothness(tolerance) => self.simplify_tolerance = tolerance,
            Change::Page(change) => self.change_page(change),
            Change::Form { target, form } => self
                .repaint((target, "Gradient", true), &|paint, bounds| {
                    gradient::formed(paint, form, bounds)
                }),
            Change::Stops {
                target,
                stops,
                spread,
                form,
                settled,
            } => self.repaint((target, "Gradient", settled), &|paint, bounds| {
                gradient::restopped(paint, (&stops, spread, form), bounds)
            }),
            Change::Rotation(degrees) => self.rotate_to(degrees),
            Change::KeepSwatch(colour) => self.keep_swatch(colour),
            Change::DropSwatch(index) => self.drop_swatch(index),
            Change::Refused { what, error } => self.say_error(what, error.into()),
        }
        self.refresh_inspector();
    }

    /// The `target` paint made `paint`: of the characters typing has
    /// selected, as runs, where it is a colour or none; else of the selection
    /// and of what is drawn next — a gradient always paints a whole text.
    fn paint_with(&mut self, target: Target, paint: &Paint, settled: bool) {
        let label = match target {
            Target::Fill => "Fill",
            Target::Stroke => "Stroke",
        };
        let typed = self.typed().filter(|range| !range.is_empty());
        if let (Some(range), Some(painted)) = (typed, RunPaint::of(paint)) {
            return self.restyle_characters(label, range, &|run| match target {
                Target::Fill => RunStyle {
                    fill: Some(painted),
                    ..run.clone()
                },
                Target::Stroke => RunStyle {
                    stroke: Some(painted),
                    ..run.clone()
                },
            });
        }
        self.restyle(label, settled, &|style| match target {
            Target::Fill => style.fill = paint.clone(),
            Target::Stroke => style.stroke.paint = paint.clone(),
        });
    }

    /// Text set as `change` asks: what is typed next, and the characters
    /// typing has selected — as runs — or else the whole of each text
    /// selected, what its runs said of the same given up.
    fn change_text(&mut self, change: TextChange) {
        match change {
            TextChange::Family(family) => {
                self.text.family = family;
                match SharedStr::try_from_str(family) {
                    Ok(family) => self.set_text(
                        "Font",
                        &|run| RunStyle {
                            family: Some(family.clone()),
                            ..run.clone()
                        },
                        &|text| text.family = family.clone(),
                        &|run| run.family = None,
                    ),
                    Err(error) => self.say_error("Font", DrawingError::from(error).into()),
                }
            }
            TextChange::Size(size) => {
                self.text.size = size;
                self.set_text(
                    "Text size",
                    &|run| RunStyle {
                        size: Some(size),
                        ..run.clone()
                    },
                    &|text| text.size = size,
                    &|run| run.size = None,
                );
            }
            TextChange::Bold(bold) => {
                self.text.bold = bold;
                let weight = if bold { 700 } else { 400 };
                self.set_text(
                    "Bold",
                    &|run| RunStyle {
                        weight: Some(weight),
                        ..run.clone()
                    },
                    &|text| text.weight = weight,
                    &|run| run.weight = None,
                );
            }
            TextChange::Italic(italic) => {
                self.text.italic = italic;
                self.set_text(
                    "Italic",
                    &|run| RunStyle {
                        italic: Some(italic),
                        ..run.clone()
                    },
                    &|text| text.italic = italic,
                    &|run| run.italic = None,
                );
            }
            TextChange::Spacing(spacing) => self.set_text(
                "Letter spacing",
                &|run| RunStyle {
                    letter_spacing: Some(spacing),
                    ..run.clone()
                },
                &|text| text.letter_spacing = spacing,
                &|run| run.letter_spacing = None,
            ),
            TextChange::Raise(raise) => {
                // A text has no raise of its own: raising the whole of it
                // raises every character as one run.
                let raised = &|run: &RunStyle| RunStyle {
                    baseline_shift: (raise != 0.0).then_some(raise),
                    ..run.clone()
                };
                match self.typed().filter(|range| !range.is_empty()) {
                    Some(range) => self.restyle_characters("Raise", range, raised),
                    None => self.reset_text("Raise", &|text| {
                        *text = text.restyled(0..text.content.len(), raised)?;
                        Ok(())
                    }),
                }
            }
            TextChange::Align(align) => {
                self.text.align = align;
                self.reset_text("Align text", &|text| {
                    text.align = align;
                    Ok(())
                });
            }
            TextChange::LineHeight(height) => self.reset_text("Line height", &|text| {
                if let Baseline::Lines { line_height, .. } = &mut text.baseline {
                    *line_height = height;
                }
                Ok(())
            }),
            TextChange::Start(start) => self.reset_text("Start along path", &|text| {
                if let Baseline::Path(along) = &text.baseline {
                    let moved = TextPath {
                        start,
                        ..(**along).clone()
                    };
                    text.baseline = Baseline::Path(try_arc(moved)?);
                }
                Ok(())
            }),
        }
    }

    /// A change to how text is set: to the characters typing has selected,
    /// `run` makes each run's style of what it was; else `own` sets each
    /// selected text and `clear` takes the same from every run of it.
    fn set_text(
        &mut self,
        label: &'static str,
        run: &dyn Fn(&RunStyle) -> RunStyle,
        own: &dyn Fn(&mut Text),
        clear: &dyn Fn(&mut RunStyle),
    ) {
        if let Some(range) = self.typed().filter(|range| !range.is_empty()) {
            return self.restyle_characters(label, range, run);
        }
        self.reset_text(label, &|text| {
            own(text);
            if !text.runs.is_empty() {
                *text = text.restyled(0..text.content.len(), |style| {
                    let mut cleared = style.clone();
                    clear(&mut cleared);
                    cleared
                })?;
            }
            Ok(())
        });
    }

    /// The characters `range` of the text being typed restyled by `run`, to
    /// land with the typing.
    fn restyle_characters(
        &mut self,
        label: &'static str,
        range: core::ops::Range<usize>,
        run: &dyn Fn(&RunStyle) -> RunStyle,
    ) {
        let Some(typing) = self.typing.clone() else {
            return;
        };
        let drawing = self.shown().clone();
        let typed = drawing
            .node_at(&typing.path)
            .filter(|node| node.id() == typing.id);
        let Some((node, Kind::Text { text, .. })) = typed.map(|node| (node, node.kind())) else {
            return;
        };
        let result = text
            .restyled(range, run)
            .map_err(edit::EditError::from)
            .and_then(|text| retyped(&drawing, &typing.path, node, text));
        self.take_change(result, label, true);
    }

    fn change_page(&mut self, change: PageChange) {
        match change {
            PageChange::Width(width) => {
                self.repage("Page size", true, |page| page.width = width);
            }
            PageChange::Height(height) => {
                self.repage("Page size", true, |page| page.height = height);
            }
            PageChange::Landscape(landscape) => self.lay_page(landscape),
            PageChange::Unit(unit) => self.set_unit(unit),
            PageChange::GridSpacing(spacing) => {
                let unit = self.edit_base().page().unit.pixels();
                self.regrid("Grid spacing", |grid| grid.spacing = spacing * unit);
            }
            PageChange::Subdivisions(cells) => {
                self.regrid("Grid subdivisions", |grid| grid.subdivisions = cells);
            }
            PageChange::Resolution(resolution) => {
                let result = self
                    .edit_base()
                    .with_resolution(resolution)
                    .map_err(edit::EditError::from);
                self.take_change(result, "Copy resolution", true);
            }
            PageChange::Ground { colour, settled } => {
                self.repage("Page ground", settled, |page| page.background = colour);
            }
        }
    }

    /// What an inspector change edits: the drawing typing shows while it is
    /// under way, so the change lands with it, else the document's.
    pub(crate) fn edit_base(&self) -> &tairix_drawing::Drawing {
        if self.typing.is_some() {
            self.shown()
        } else {
            self.document.drawing()
        }
    }

    /// Take what an inspector change made: shown, to land with typing under
    /// way or as `label` once `settled`, or landed as `label` now.
    pub(crate) fn take_change(
        &mut self,
        result: Result<tairix_drawing::Drawing, edit::EditError>,
        label: &'static str,
        settled: bool,
    ) {
        match result {
            Ok(drawing) if self.typing.is_some() => {
                if !drawing.is(self.shown()) {
                    self.show_live(drawing, TYPE_TEXT);
                }
            }
            // Back where the document stands: nothing is under way.
            Ok(drawing) if drawing.is(self.document.drawing()) => self.drop_live(),
            // Settled as it was shown live, it still lands.
            Ok(drawing) if settled => self.commit(drawing, label),
            Ok(drawing) if drawing.is(self.shown()) => {}
            Ok(drawing) => self.show_live(drawing, label),
            Err(error) => self.say_error(label, error),
        }
    }

    /// The style new objects are drawn in altered by `alter`, and the
    /// selection's with it: shown while `settled` is false, landed as one
    /// step called `label` once it is.
    fn restyle(&mut self, label: &'static str, settled: bool, alter: &dyn Fn(&mut Style)) {
        self.restyle_or_refuse(label, settled, &|style| {
            alter(style);
            Ok(())
        });
    }

    /// As [`restyle`](Self::restyle), for an alteration that can be refused,
    /// which is said and leaves the drawing as it was.
    fn restyle_or_refuse(
        &mut self,
        label: &'static str,
        settled: bool,
        alter: &dyn Fn(&mut Style) -> Result<(), tairix_drawing::DrawingError>,
    ) {
        if let Err(error) = alter(&mut self.style) {
            return self.say_error(label, error.into());
        }
        if self.selection.is_empty() {
            return;
        }
        let altered = |style: &Style, _: &edit::Placed<'_>| {
            let mut style = style.clone();
            alter(&mut style)?;
            Ok(style)
        };
        let glyphs = Lettering::new(Arrived);
        let result = edit::restyle(self.edit_base(), &self.selection, &altered, &glyphs);
        self.take_change(result, label, settled);
    }

    /// The `target` paint new objects are drawn in, and each selected
    /// object's, made what `repaint` makes of it and of the object's own
    /// bounds: shown while `settled` is false, landed as one step called
    /// `label` once it is. New objects' paint is made over a unit box, which
    /// an object drawn in it is fitted to again.
    fn repaint(
        &mut self,
        (target, label, settled): (Target, &'static str, bool),
        repaint: &Repaint<'_>,
    ) {
        let unit = || tairix_bezier::Bounds::new(Point::ZERO, Point::new(1.0, 1.0));
        match repaint(target.of(&self.style), &unit) {
            Ok(paint) => *target.of_mut(&mut self.style) = paint,
            Err(error) => return self.say_error(label, error.into()),
        }
        if self.selection.is_empty() {
            return;
        }
        let repainted = |style: &Style, placed: &edit::Placed<'_>| {
            let mut style = style.clone();
            *target.of_mut(&mut style) = repaint(target.of(&style), &|| placed.bounds())?;
            Ok(style)
        };
        let glyphs = Lettering::new(Arrived);
        let result = edit::restyle(self.edit_base(), &self.selection, &repainted, &glyphs);
        self.take_change(result, label, settled);
    }

    /// The selection's paint in the Style tab's hand laid as a gradient from
    /// the line's start to its end on the page — its end in 15° steps with
    /// Shift, radial about its start with Alt — in each object's own space:
    /// shown live until `settled`, then one step. A line no longer than a
    /// pixel lays nothing.
    fn lay_gradient(&mut self, (from, to): (Point, Point), settled: bool) {
        let even = Constraint {
            even: self.modifiers.shift,
            centred: false,
        };
        let end = tools::line_end(from, to, even);
        if from.distance(end) < self.pixel() {
            if settled {
                self.drop_live();
            }
            return;
        }
        let (target, radial) = (self.inspector.target(), self.modifiers.alt);
        let lay = |style: &Style, placed: &edit::Placed<'_>| {
            // A transform with no inverse leaves an object no area to paint.
            let Some(back) = placed.to_drawing().invert() else {
                return Ok(style.clone());
            };
            let mut style = style.clone();
            let line = (back.map(from), back.map(end));
            *target.of_mut(&mut style) = gradient::laid(target.of(&style), line, radial)?;
            Ok(style)
        };
        let glyphs = Lettering::new(Arrived);
        let result = edit::restyle(self.edit_base(), &self.selection, &lay, &glyphs);
        self.take_change(result, "Gradient", settled);
    }

    /// The selection turned about its centre so it stands `degrees`
    /// anticlockwise: one object as its own angle says, several together.
    fn rotate_to(&mut self, degrees: f64) {
        if self.selection.is_empty() {
            return;
        }
        let turn = degrees - edit::angle_of(self.edit_base(), &self.selection);
        // The field shows hundredths of a degree: an angle asked for that
        // reads as the one shown is no turn.
        if tairix_util::mathf::round(turn * 100.0) == 0.0 {
            return;
        }
        let centre = self.selection_bounds().centre();
        // The page runs downward, so a turn anticlockwise on it is negative.
        let map = tairix_bezier::Affine::rotate_degrees_about(-turn, centre.x, centre.y);
        let result = edit::transform(self.edit_base(), &self.selection, map);
        self.take_change(result, "Rotate", true);
    }

    /// The drawing keeping `colour` among its swatches, once.
    fn keep_swatch(&mut self, colour: tairix_colour::Rgba) {
        self.reswatch("Add swatch", |swatches| {
            if !swatches.contains(&colour) && swatches.try_reserve(1).is_ok() {
                swatches.push(colour);
            }
        });
    }

    /// The drawing keeping its swatches but the one at `index`.
    fn drop_swatch(&mut self, index: usize) {
        self.reswatch("Remove swatch", |swatches| {
            if index < swatches.len() {
                swatches.remove(index);
            }
        });
    }

    /// The drawing keeping `swatches` as `alter` leaves them, as one step
    /// called `label`; a list left as it was is no step.
    fn reswatch(&mut self, label: &'static str, alter: impl FnOnce(&mut Vec<tairix_colour::Rgba>)) {
        let base = self.edit_base();
        let Some(mut swatches) = tairix_util::fallible::copied(base.swatches()) else {
            return self.say_error(label, tairix_drawing::DrawingError::OutOfMemory.into());
        };
        alter(&mut swatches);
        if swatches.as_slice() == base.swatches() {
            return;
        }
        let result = SharedSlice::try_from_vec(swatches)
            .map_err(DrawingError::from)
            .and_then(|swatches| base.with_swatches(swatches))
            .map_err(edit::EditError::from);
        self.take_change(result, label, true);
    }

    fn set_opacity(&mut self, opacity: f64, settled: bool) {
        if self.selection.is_empty() {
            return;
        }
        let opacity = opacity.clamp(0.0, 1.0);
        #[allow(
            clippy::float_cmp,
            reason = "an opacity left exactly as it was is no change"
        )]
        let result = edit::each(self.edit_base(), &self.selection, |_, _, node| {
            if node.opacity() == opacity {
                return Ok(None);
            }
            Ok(Some(node.with_opacity(opacity)?))
        });
        self.take_change(result, "Opacity", settled);
    }

    /// One measure of the selection's bounds — left, top, width or height,
    /// by `index` — made `value`, the others kept, scaling what it holds to
    /// fit.
    fn place_selection(&mut self, index: usize, value: f64) {
        let bounds = self.selection_bounds();
        let mut place = [bounds.min.x, bounds.min.y, bounds.width(), bounds.height()];
        let (Some(measure), false) = (place.get_mut(index), bounds.is_empty()) else {
            return;
        };
        if index >= 2 && value <= 0.0 {
            return;
        }
        *measure = value;
        let [left, top, width, height] = place;
        let factor = |to: f64, from: f64| if from > 0.0 { to / from } else { 1.0 };
        let map = Affine::translate(-bounds.min.x, -bounds.min.y)
            .then(Affine::scale(
                factor(width, bounds.width()),
                factor(height, bounds.height()),
            ))
            .then(Affine::translate(left, top));
        let result = edit::transform(self.edit_base(), &self.selection, map);
        self.take_change(result, "Place", true);
    }

    /// Line the selection up: one object to the page, several to their
    /// bounds or, asked, to the page.
    fn align_selection(&mut self, align: edit::Align) {
        let to = if self.inspector.to_page() || self.selection.len() < 2 {
            self.document.drawing().page().bounds()
        } else {
            self.selection_bounds()
        };
        let glyphs = Lettering::new(Arrived);
        let result = edit::align(self.edit_base(), &self.selection, align, to, &glyphs);
        self.take_change(result, "Align", true);
    }

    /// New rectangles rounded by `radius`, and the selected ones with them,
    /// each to no more than half its shorter side.
    fn round_corners(&mut self, radius: f64) {
        self.corner_radius = radius;
        self.reshape_selected("Corners", &|kind| {
            let Kind::Shape {
                shape: Shape::Rect(rect),
                style,
            } = kind
            else {
                return Ok(None);
            };
            let radius = radius.clamp(0.0, tairix_util::mathf::fmin(rect.width, rect.height) / 2.0);
            let rounded = tairix_drawing::shape::RectShape {
                rx: radius,
                ry: radius,
                ..*rect
            };
            if rounded == *rect {
                return Ok(None);
            }
            Ok(Some(Kind::Shape {
                shape: Shape::Rect(rounded),
                style: style.clone(),
            }))
        });
    }

    /// Every selected object `change` makes anew made so, as one step; one
    /// it leaves as it is, not at all.
    fn reshape_selected(
        &mut self,
        label: &'static str,
        change: &dyn Fn(&Kind) -> Result<Option<Kind>, DrawingError>,
    ) {
        if self.selection.is_empty() {
            return;
        }
        let result = edit::each(self.edit_base(), &self.selection, |_, _, node| {
            Ok(change(node.kind())?
                .map(|kind| node.with_kind(kind))
                .transpose()?)
        });
        self.take_change(result, label, true);
    }

    /// Every selected star altered by `alter`.
    fn reshape_star(&mut self, label: &'static str, alter: &dyn Fn(&mut StarShape)) {
        self.reshape_selected(label, &|kind| {
            let Kind::Shape {
                shape: Shape::Star(star),
                style,
            } = kind
            else {
                return Ok(None);
            };
            let mut altered = *star;
            alter(&mut altered);
            if altered == *star {
                return Ok(None);
            }
            Ok(Some(Kind::Shape {
                shape: Shape::Star(altered),
                style: style.clone(),
            }))
        });
    }

    /// Every selected text set anew by `alter`.
    fn reset_text(
        &mut self,
        label: &'static str,
        alter: &dyn Fn(&mut Text) -> Result<(), DrawingError>,
    ) {
        self.reshape_selected(label, &|kind| {
            let Kind::Text { text, style } = kind else {
                return Ok(None);
            };
            let mut altered = (**text).clone();
            alter(&mut altered)?;
            if altered == **text {
                return Ok(None);
            }
            Ok(Some(Kind::Text {
                text: try_arc(altered)?,
                style: style.clone(),
            }))
        });
    }

    /// Carry out a layer command.
    pub(crate) fn layer_command(&mut self, command: LayerCommand) {
        let drawing = self.document.drawing().clone();
        let layers = drawing.layers().len();
        let rebuilt = |index: usize,
                       change: &dyn Fn(&Node) -> Node|
         -> Result<tairix_drawing::Drawing, edit::EditError> {
            let layer = drawing.node_at(&[index]).ok_or(edit::EditError::Nothing)?;
            Ok(drawing.with_node_at(&[index], try_arc(change(layer))?)?)
        };
        match command {
            LayerCommand::Choose(index) => self.layer = index.min(layers.saturating_sub(1)),
            LayerCommand::Visible(index) => {
                let result = rebuilt(index, &|layer| layer.with_visible(!layer.visible()));
                self.apply(result, "Show layer");
            }
            LayerCommand::Lock(index) => {
                let result = rebuilt(index, &|layer| layer.with_locked(!layer.locked()));
                self.apply(result, "Lock layer");
            }
            LayerCommand::New => {
                let name = alloc::format!("Layer {}", layers + 1);
                match edit::add_layer(&drawing, &name, self.document.ids()) {
                    Ok((drawing, path)) => {
                        self.commit(drawing, "New layer");
                        self.layer = path[0];
                    }
                    Err(error) => self.say_error("New layer", error),
                }
            }
            LayerCommand::Remove => {
                let result = edit::remove_layer(&drawing, self.layer);
                if self.apply(result, "Remove layer") {
                    self.selection.clear();
                    self.layer = self.layer.saturating_sub(1);
                }
            }
            LayerCommand::Raise | LayerCommand::Lower => {
                let to = if command == LayerCommand::Raise {
                    (self.layer + 1).min(layers.saturating_sub(1))
                } else {
                    self.layer.saturating_sub(1)
                };
                if to != self.layer {
                    let result = edit::move_layer(&drawing, self.layer, to);
                    if self.apply(result, "Move layer") {
                        self.layer = to;
                        self.selection.clear();
                    }
                }
            }
        }
    }
}

/// `at` within `text`, moved back to the start of the character it falls
/// in, so a caret carried over a change of text never splits one.
fn char_start(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The eight handles, corners first.
pub(crate) fn handles() -> [Handle; 8] {
    let at = |across, down| Handle { across, down };
    [
        at(-1, -1),
        at(1, -1),
        at(1, 1),
        at(-1, 1),
        at(0, -1),
        at(1, 0),
        at(0, 1),
        at(-1, 0),
    ]
}

/// Where `handle` stands on `bounds`.
pub(crate) fn handle_point(bounds: Bounds, handle: Handle) -> Point {
    let pick = |side: i8, low: f64, high: f64| match side {
        -1 => low,
        1 => high,
        _ => f64::midpoint(low, high),
    };
    Point::new(
        pick(handle.across, bounds.min.x, bounds.max.x),
        pick(handle.down, bounds.min.y, bounds.max.y),
    )
}

/// The local point the drawing point `to` is at, for the path `node` at
/// `path`, and that path's shape.
fn path_frame<'n>(
    node: &'n Node,
    drawing: &tairix_drawing::Drawing,
    path: &[usize],
    to: Point,
) -> Option<(&'n Arc<tairix_drawing::PathShape>, Point)> {
    let shape = node_path(node)?;
    let to_drawing = node.transform().then(edit::parent_space(drawing, path));
    Some((shape, to_drawing.invert()?.map(to)))
}

/// The path `node` is edited by node by node: a path shape's own, or the
/// path a text is set along.
pub(crate) fn node_path(node: &Node) -> Option<&Arc<tairix_drawing::PathShape>> {
    match node.kind() {
        Kind::Shape {
            shape: Shape::Path(shape),
            ..
        } => Some(shape),
        Kind::Text { text, .. } => match &text.baseline {
            Baseline::Path(along) => Some(&along.path),
            Baseline::Lines { .. } => None,
        },
        Kind::Shape { .. } | Kind::Image { .. } | Kind::Group { .. } => None,
    }
}

/// `node` with `shape` in place of the path it is edited by.
fn with_node_path(
    node: &Node,
    shape: tairix_drawing::PathShape,
) -> Result<Option<Node>, DrawingError> {
    let kind = match node.kind() {
        Kind::Shape { style, .. } => Kind::Shape {
            shape: Shape::Path(try_arc(shape)?),
            style: style.clone(),
        },
        Kind::Text { text, style } => {
            let Baseline::Path(along) = &text.baseline else {
                return Ok(None);
            };
            let moved = TextPath {
                path: try_arc(shape)?,
                start: along.start,
            };
            let text = Text {
                baseline: Baseline::Path(try_arc(moved)?),
                ..(**text).clone()
            };
            Kind::Text {
                text: try_arc(text)?,
                style: style.clone(),
            }
        }
        Kind::Image { .. } | Kind::Group { .. } => return Ok(None),
    };
    Ok(Some(node.with_kind(kind)?))
}

/// `target`, a path node, with the part `part` of it moved to the local
/// point `local`: the node carrying its handles with it, a handle keeping
/// the node's join.
pub(crate) fn moved_point(
    target: &tairix_drawing::PathNode,
    part: GripPart,
    local: Point,
) -> tairix_drawing::PathNode {
    let mut target = *target;
    match part {
        GripPart::Point => {
            let by = local - target.point;
            target.point = local;
            target.before = target.before.map(|handle| handle + by);
            target.after = target.after.map(|handle| handle + by);
        }
        GripPart::Before | GripPart::After => {
            let (moved, other) = if part == GripPart::Before {
                (&mut target.before, &mut target.after)
            } else {
                (&mut target.after, &mut target.before)
            };
            *moved = Some(local);
            let away = local - target.point;
            match target.join {
                tairix_drawing::NodeJoin::Symmetric => *other = Some(target.point - away),
                tairix_drawing::NodeJoin::Smooth => {
                    if let (Some(handle), Some(direction)) = (*other, (-away).normalised()) {
                        let length = handle.distance(target.point);
                        *other = Some(target.point + direction * length);
                    }
                }
                tairix_drawing::NodeJoin::Corner => {}
            }
        }
    }
    target
}

/// The node `grip` names of the path `node` at `path`, as dragging it to the
/// drawing point `to` makes it, in the path's own space.
pub(crate) fn gripped(
    node: &Node,
    drawing: &tairix_drawing::Drawing,
    path: &[usize],
    grip: Grip,
    to: Point,
) -> Option<tairix_drawing::PathNode> {
    let (shape, local) = path_frame(node, drawing, path, to)?;
    let target = shape.contours.get(grip.contour)?.nodes.get(grip.node)?;
    Some(moved_point(target, grip.part, local))
}

/// The path `node` at `path` with the node `grip` names dragged to the
/// drawing point `to`.
pub(crate) fn moved_node(
    node: &Node,
    drawing: &tairix_drawing::Drawing,
    path: &[usize],
    grip: Grip,
    to: Point,
) -> Result<Option<Node>, DrawingError> {
    let Some(moved) = gripped(node, drawing, path, grip, to) else {
        return Ok(None);
    };
    let Some((shape, _)) = path_frame(node, drawing, path, to) else {
        return Ok(None);
    };
    with_node_path(node, shape.with_node(grip.contour, grip.node, moved)?)
}

/// `drawing` with the text `node` at `path` set as `text`.
fn retyped(
    drawing: &tairix_drawing::Drawing,
    path: &[usize],
    node: &Node,
    text: Text,
) -> Result<tairix_drawing::Drawing, edit::EditError> {
    let Kind::Text { style, .. } = node.kind() else {
        return Err(edit::EditError::Nothing);
    };
    let rebuilt = node.with_kind(Kind::Text {
        text: try_arc(text)?,
        style: style.clone(),
    })?;
    Ok(drawing.with_node_at(path, try_arc(rebuilt)?)?)
}

/// `drawing` with its guide `index`, if any, taken away, and `placed`, if
/// any, added after the rest.
fn with_guide(
    drawing: &tairix_drawing::Drawing,
    index: Option<usize>,
    placed: Option<Guide>,
) -> Result<tairix_drawing::Drawing, DrawingError> {
    let mut guides =
        tairix_util::fallible::copied(drawing.guides()).ok_or(DrawingError::OutOfMemory)?;
    if let Some(index) = index.filter(|&index| index < guides.len()) {
        guides.remove(index);
    }
    if let Some(guide) = placed {
        if !tairix_util::fallible::push(&mut guides, guide) {
            return Err(DrawingError::OutOfMemory);
        }
    }
    drawing.with_guides(SharedSlice::try_from_vec(guides)?)
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
