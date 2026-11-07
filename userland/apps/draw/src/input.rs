//! What a Draw window does with the pointer and the keyboard.
//!
//! A press is routed to the part of the window under the pointer: the tool
//! box, the inspector, a ruler, or the canvas, where the tool in hand takes
//! it. A drag on the canvas is previewed as outlines over the tiles each
//! frame and lands in the document once, when it ends.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
use tairix_abi::touch::{PinchPhase, PINCH_SCALE_ONE};
use tairix_bezier::{Affine, Bounds, Point};
use tairix_controls::ToolbarOutcome;
use tairix_drawing::hit::{hit, within_area};
use tairix_drawing::shape::StarShape;
use tairix_drawing::{Guide, GuideAxis, Kind, Node, Paint, Shape, Style, Text};
use tairix_geometry::{Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_theme::Theme;
use tairix_util::mathf::{atan2, exp2, fabs, fmax, round, round_i32};
use tairix_window::docapp::{Outcome, Request};

use crate::edit::{self, Arrange};
use crate::glyphs::{Arrived, Lettering};
use crate::inspector::{Change, LayerCommand, Target};
use crate::layout::Layout;
use crate::tools::{self, Constraint, Tool};
use crate::view::{Gesture, Grip, GripPart, Handle, MenuKind, Own, Typing, View, TYPE_TEXT};

/// What an input comes to.
pub type ViewOutcome = Outcome<Request<MenuKind, Own>>;

/// How near, in logical pixels, the pointer must come to pick a handle, a
/// node or a guide.
const GRAB: u32 = 6;

/// How far past a corner handle, in logical pixels, a drag turns rather
/// than scales.
const TURN_REACH: u32 = 20;

/// How far, in logical pixels, one wheel detent scrolls.
const DETENT_SCROLL: u32 = 48;

/// How far a nudge moves, in drawing pixels; ten times as far with Shift.
const NUDGE: f64 = 1.0;

/// `style` stroked with the pattern at `index` of the inspector's dashes, in
/// lengths of its stroke's width.
fn dash(style: &mut Style, index: usize) {
    let width = if style.stroke.width > 0.0 {
        style.stroke.width
    } else {
        1.0
    };
    let pattern = crate::inspector::DASHES[index.min(crate::inspector::DASHES.len() - 1)];
    style.stroke.dashes = pattern.iter().map(|dash| dash * width).collect();
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
    pub(crate) fn drawing_at(&self, layout: &Layout, at: Point) -> Point {
        self.viewport.drawing_point(at - canvas_origin(layout))
    }

    /// The window device point over the drawing point `point`.
    pub(crate) fn window_at(&self, layout: &Layout, point: Point) -> Point {
        self.viewport.canvas_point(point) + canvas_origin(layout)
    }

    /// One device pixel, in drawing pixels.
    pub(crate) fn pixel(&self) -> f64 {
        1.0 / self.viewport.pixels()
    }

    /// How near the pointer must come to pick something, in drawing pixels.
    fn grab(&self, layout: &Layout) -> f64 {
        reach(layout, GRAB) * self.pixel()
    }

    /// The pointer as the drawing sees it, snapped as the window snaps.
    fn snapped(&self, layout: &Layout, at: Point) -> Point {
        let point = self.drawing_at(layout, at);
        if !self.show.snapping {
            return point;
        }
        crate::snap::snapped(self.shown(), point, self.grab(layout), self.show.grid)
    }

    /// Feed an input event.
    pub fn on_input(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> ViewOutcome {
        match *event {
            InputEvent::KeyPressed { key, modifiers } => {
                self.modifiers = modifiers;
                self.key(key, modifiers, layout, scale, theme, damage)
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
            _ => self.pointer_event(event, layout, scale, theme, damage),
        }
    }

    fn pointer_event(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
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
            layout.horizontal_bar,
            layout.vertical_bar,
            layout.bar_corner,
        ]
        .iter()
        .any(|bar| bar.contains(over));
        let held = self.bar_held();
        if on_bar || held || matches!(event, InputEvent::PointerMoved { .. }) {
            self.bars_input(event, over, (layout, scale, theme), damage);
            if on_bar || held {
                return Outcome::none();
            }
        }
        let inspecting = layout.panel.contains(over)
            || layout.tabs.contains(over)
            || self.inspector.holds_pointer();
        match *event {
            InputEvent::PointerScrolled { dx, dy } if !inspecting => {
                return self.wheel(dx, dy, layout, damage);
            }
            InputEvent::Pinch {
                phase,
                scale: spread,
                at,
            } => return self.pinch(phase, spread, device(at), layout, damage),
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
            return self.drag(event, layout, damage);
        }
        if layout.tools.contains(over) || matches!(event, InputEvent::PointerMoved { .. }) {
            if let ToolbarOutcome::Activated(action) =
                self.tool_box
                    .on_pointer(event, layout.tools, scale, theme, damage)
            {
                if let Some(&tool) = Tool::ALL.get(action.index) {
                    self.choose_tool(tool);
                    damage.add(layout.window);
                }
            }
            if layout.tools.contains(over) {
                return Outcome::none();
            }
        }
        if inspecting || self.inspector.was_over(layout) || self.inspector.has_keyboard() {
            let changes = self.inspect(|inspector, shown| {
                inspector.on_pointer(event, (layout, scale, theme), shown, damage)
            });
            for change in changes {
                self.apply_change(change, layout, damage);
            }
            // The keyboard went to the inspector, so typing ends where it is.
            if self.inspector.has_keyboard() && self.typing.is_some() {
                self.finish_typing();
                damage.add(layout.canvas);
            }
            if inspecting {
                return Outcome::none();
            }
        }
        match *event {
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => self.primary_press(over, layout, damage),
            InputEvent::PointerPressed {
                button: PointerButton::Middle,
            } if layout.canvas.contains(over) => {
                self.gesture = Some(Gesture::Pan { last: self.pointer });
                Outcome::none()
            }
            _ => Outcome::none(),
        }
    }

    /// A primary press at the window point `over` that no control took: a
    /// guide out of a ruler, the tool in hand on the canvas, or the bar.
    fn primary_press(
        &mut self,
        over: tairix_geometry::Point,
        layout: &Layout,
        damage: &mut Region,
    ) -> ViewOutcome {
        let ruled = layout.top_ruler.contains(over) || layout.left_ruler.contains(over);
        // Whatever the press begins acts on the drawing as shown.
        if ruled || layout.canvas.contains(over) {
            self.land_pending(damage);
        }
        if ruled {
            let horizontal = layout.top_ruler.contains(over);
            let at = self.drawing_at(layout, self.pointer);
            self.gesture = Some(Gesture::Guide {
                horizontal,
                at: if horizontal { at.y } else { at.x },
                index: None,
            });
            return Outcome::none();
        }
        if layout.canvas.contains(over) {
            return self.press(layout, damage);
        }
        if layout.bar.contains(over) {
            return self.bar_press(layout, damage);
        }
        Outcome::none()
    }

    /// A press on the tool's bar: its zoom readout opens the zoom menu.
    fn bar_press(&mut self, layout: &Layout, _damage: &mut Region) -> ViewOutcome {
        let zoom = crate::paint::zoom_box(layout);
        let over =
            tairix_geometry::Point::new(round_i32(self.pointer.x), round_i32(self.pointer.y));
        if zoom.contains(over) {
            return Outcome::asking(Request::Menu {
                kind: MenuKind::Zoom,
                anchor: zoom,
            });
        }
        Outcome::none()
    }

    fn wheel(&mut self, dx: i32, dy: i32, layout: &Layout, damage: &mut Region) -> ViewOutcome {
        let detents = |units: i32| f64::from(units) / f64::from(SCROLL_UNITS_PER_DETENT);
        let step = reach(layout, DETENT_SCROLL);
        if self.modifiers.ctrl {
            let factor = exp2(-detents(dy) / 4.0);
            let anchor = self.pointer - canvas_origin(layout);
            self.viewport.zoom_at(self.viewport.zoom() * factor, anchor);
        } else if self.modifiers.shift {
            self.viewport.scroll(Point::new(detents(dy) * step, 0.0));
        } else {
            self.viewport
                .scroll(Point::new(detents(dx) * step, detents(dy) * step));
        }
        damage.add(layout.window);
        Outcome::none()
    }

    fn pinch(
        &mut self,
        phase: PinchPhase,
        spread: u32,
        at: Point,
        layout: &Layout,
        damage: &mut Region,
    ) -> ViewOutcome {
        match phase {
            PinchPhase::Begin => self.pinched = Some(self.viewport.zoom()),
            PinchPhase::Update => {
                if let Some(began) = self.pinched {
                    let factor = f64::from(spread) / f64::from(PINCH_SCALE_ONE);
                    self.viewport
                        .zoom_at(began * factor, at - canvas_origin(layout));
                    damage.add(layout.window);
                }
            }
            PinchPhase::End | PinchPhase::Cancel => self.pinched = None,
        }
        Outcome::none()
    }

    /// A primary press on the canvas.
    fn press(&mut self, layout: &Layout, damage: &mut Region) -> ViewOutcome {
        let point = self.drawing_at(layout, self.pointer);
        let snapped = self.snapped(layout, self.pointer);
        if self.spaced || self.tool == Tool::Hand {
            self.gesture = Some(Gesture::Pan { last: self.pointer });
            return Outcome::none();
        }
        if let Some(index) = self.guide_near(layout) {
            let guide = self.shown().guides()[index];
            self.gesture = Some(Gesture::Guide {
                horizontal: guide.axis == GuideAxis::Horizontal,
                at: guide.position,
                index: Some(index),
            });
            return Outcome::none();
        }
        match self.tool {
            Tool::Select => self.select_press(layout, point, damage),
            Tool::Nodes => {
                if let Some(grip) = self.grip_near(layout) {
                    self.gesture = Some(Gesture::Node { grip, to: point });
                } else {
                    self.pick(point, false, damage, layout);
                }
            }
            Tool::Rectangle | Tool::Ellipse | Tool::Star | Tool::Line => {
                self.gesture = Some(Gesture::Create {
                    from: snapped,
                    to: snapped,
                });
            }
            Tool::Pen => self.pen_press(layout, snapped, damage),
            Tool::Pencil => {
                self.gesture = Some(Gesture::Freehand {
                    points: alloc::vec![point],
                });
            }
            Tool::Text => self.text_press(point, damage, layout),
            Tool::Eyedropper => self.eyedrop(point, damage, layout),
            Tool::Zoom => {
                let at = self.pointer - canvas_origin(layout);
                self.gesture = Some(Gesture::Zoom { from: at, to: at });
            }
            Tool::Hand => {}
        }
        Outcome::none()
    }

    fn select_press(&mut self, layout: &Layout, point: Point, damage: &mut Region) {
        if let Some(gesture) = self.handle_near(layout) {
            self.gesture = Some(gesture);
            return;
        }
        let picked = self.pick(point, self.modifiers.shift, damage, layout);
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
    pub(crate) fn pick(
        &mut self,
        point: Point,
        toggle: bool,
        damage: &mut Region,
        layout: &Layout,
    ) -> bool {
        let hit = hit(
            self.shown(),
            point,
            self.grab(layout),
            &Lettering::new(Arrived),
        )
        .ok()
        .flatten();
        let Some(mut path) = hit else {
            if !toggle {
                self.selection.clear();
                damage.add(layout.window);
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
        damage.add(layout.window);
        self.selection.contains(&path)
    }

    /// The scale or turn handle of the selection under the pointer.
    fn handle_near(&self, layout: &Layout) -> Option<Gesture> {
        let bounds = self.selection_bounds();
        if bounds.is_empty() {
            return None;
        }
        let point = self.drawing_at(layout, self.pointer);
        for handle in handles() {
            let at = self.window_at(layout, handle_point(bounds, handle));
            let distance = at.distance(self.pointer);
            if distance <= reach(layout, GRAB) {
                return Some(Gesture::Scale {
                    handle,
                    bounds,
                    to: point,
                });
            }
            let corner = handle.across != 0 && handle.down != 0;
            if corner && distance <= reach(layout, TURN_REACH) && !bounds.contains(point) {
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
    fn guide_near(&self, layout: &Layout) -> Option<usize> {
        let point = self.drawing_at(layout, self.pointer);
        let grab = self.grab(layout);
        self.shown()
            .guides()
            .iter()
            .position(|guide| match guide.axis {
                GuideAxis::Horizontal => fabs(point.y - guide.position) <= grab,
                GuideAxis::Vertical => fabs(point.x - guide.position) <= grab,
            })
    }

    /// The node or handle of the selected path under the pointer.
    fn grip_near(&self, layout: &Layout) -> Option<Grip> {
        let path = self.selection.first()?;
        let node = self.shown().node_at(path)?;
        let Kind::Shape {
            shape: Shape::Path(shape),
            ..
        } = node.kind()
        else {
            return None;
        };
        let to_drawing = node
            .transform()
            .then(edit::parent_space(self.shown(), path));
        let near = |point: Point| {
            self.window_at(layout, to_drawing.map(point))
                .distance(self.pointer)
                <= reach(layout, GRAB)
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

    fn pen_press(&mut self, layout: &Layout, point: Point, damage: &mut Region) {
        if let Some(first) = self.pen.first() {
            if self.window_at(layout, first.point).distance(self.pointer) <= reach(layout, GRAB)
                && self.pen.len() > 1
            {
                let nodes = core::mem::take(&mut self.pen);
                if let Some(shape) = tools::pen_path(&nodes, true) {
                    let style = self.style.clone();
                    self.add_shape(shape, style, "Draw path");
                }
                damage.add(layout.window);
                return;
            }
        }
        self.pen.push(tools::pen_node(point, None));
        self.gesture = Some(Gesture::PenHandle { to: point });
        damage.add(layout.canvas);
    }

    fn text_press(&mut self, point: Point, damage: &mut Region, layout: &Layout) {
        self.finish_typing();
        let hit = hit(
            self.shown(),
            point,
            self.grab(layout),
            &Lettering::new(Arrived),
        )
        .ok()
        .flatten();
        if let Some(path) = hit {
            let node = self.shown().node_at(&path);
            if let Some((Kind::Text { text, .. }, id)) = node.map(|node| (node.kind(), node.id())) {
                let caret = text.content.len();
                self.selection = alloc::vec![path.clone()];
                self.typing = Some(Typing { path, id, caret });
                damage.add(layout.window);
                return;
            }
        }
        if !self.takes_new_objects() {
            damage.add(layout.status);
            return;
        }
        let text = Text {
            content: Arc::from(""),
            family: Arc::from(self.text.family),
            size: self.text.size,
            weight: if self.text.bold { 700 } else { 400 },
            italic: self.text.italic,
            letter_spacing: 0.0,
            line_height: 1.25,
            align: self.text.align,
            origin: point,
        };
        let mut style = self.style.clone();
        if !style.fill.paints() {
            style.fill = style.stroke.paint.clone();
        }
        style.stroke.paint = Paint::None;
        let id = self.document.ids().issue();
        let Ok(node) = Node::new(
            id,
            Kind::Text {
                text: Arc::new(text),
                style: Arc::new(style),
            },
        ) else {
            return;
        };
        match edit::insert(self.document.drawing(), &[self.layer], Arc::new(node)) {
            Ok((drawing, path)) => {
                self.show_live(drawing, TYPE_TEXT);
                self.selection = alloc::vec![path.clone()];
                self.typing = Some(Typing { path, id, caret: 0 });
            }
            Err(error) => self.say_error("Text", error),
        }
        damage.add(layout.window);
    }

    fn eyedrop(&mut self, point: Point, damage: &mut Region, layout: &Layout) {
        let hit = hit(
            self.shown(),
            point,
            self.grab(layout),
            &Lettering::new(Arrived),
        )
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
            let result =
                edit::restyle(self.document.drawing(), &self.selection, &|_| taken.clone());
            self.apply(result, "Take style");
        }
        self.refresh_inspector();
        damage.add(layout.window);
    }

    /// A pointer event while a gesture is under way.
    fn drag(&mut self, event: &InputEvent, layout: &Layout, damage: &mut Region) -> ViewOutcome {
        let point = self.drawing_at(layout, self.pointer);
        let snapped = self.snapped(layout, self.pointer);
        let pixel = self.pixel();
        match event {
            InputEvent::PointerMoved { .. } => {
                match &mut self.gesture {
                    Some(Gesture::Pan { last }) => {
                        let by = *last - self.pointer;
                        *last = self.pointer;
                        self.viewport.scroll(by);
                        damage.add(layout.window);
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
                    Some(Gesture::Freehand { points }) => {
                        // A line memory will not grow ends where it reached.
                        let apart = points
                            .last()
                            .is_none_or(|last| last.distance(point) >= pixel);
                        if apart && points.try_reserve(1).is_ok() {
                            points.push(point);
                        }
                    }
                    Some(Gesture::Zoom { to, .. }) => *to = self.pointer - canvas_origin(layout),
                    Some(Gesture::Guide { horizontal, at, .. }) => {
                        *at = if *horizontal { point.y } else { point.x }
                    }
                    None => {}
                }
                damage.add(layout.canvas);
                damage.add(layout.status);
                Outcome::none()
            }
            InputEvent::PointerReleased { .. } | InputEvent::PointerCancelled => {
                let cancelled = matches!(event, InputEvent::PointerCancelled);
                if let Some(gesture) = self.gesture.take() {
                    if !cancelled {
                        self.land(gesture, layout);
                    }
                }
                self.refresh_inspector();
                damage.add(layout.window);
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
    fn land(&mut self, gesture: Gesture, layout: &Layout) {
        let constraint = Constraint {
            even: self.modifiers.shift,
            centred: self.modifiers.alt,
        };
        match gesture {
            Gesture::Pan { .. } => {}
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
                        self.add_shape(shape, style, "Draw freehand");
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
                let grab = reach(layout, GRAB);
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
                        f64::from(layout.canvas.width),
                        f64::from(layout.canvas.height),
                    );
                    self.viewport.fit(drawing_area, size, 0.0);
                }
            }
            Gesture::Guide {
                horizontal,
                at,
                index,
            } => self.place_guide(horizontal, at, index, layout),
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
            Ok(shape) => self.add_shape(shape, style, label),
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
        let Some(moved) = moved_node(node, &drawing, &path, grip, to) else {
            return;
        };
        let result = drawing
            .with_node_at(&path, Arc::new(moved))
            .map_err(edit::EditError::from);
        self.apply(result, "Move node");
    }

    fn place_guide(&mut self, horizontal: bool, at: f64, index: Option<usize>, layout: &Layout) {
        let over =
            tairix_geometry::Point::new(round_i32(self.pointer.x), round_i32(self.pointer.y));
        let mut guides: Vec<Guide> = self.document.drawing().guides().to_vec();
        if let Some(index) = index {
            if index < guides.len() {
                guides.remove(index);
            }
        }
        if layout.canvas.contains(over) {
            guides.push(Guide {
                axis: if horizontal {
                    GuideAxis::Horizontal
                } else {
                    GuideAxis::Vertical
                },
                position: at,
            });
        }
        let label = match (index, layout.canvas.contains(over)) {
            (None, _) => "Add guide",
            (Some(_), true) => "Move guide",
            (Some(_), false) => "Remove guide",
        };
        let result = self
            .document
            .drawing()
            .with_guides(Arc::from(guides))
            .map_err(edit::EditError::from);
        self.apply(result, label);
    }

    /// A key pressed: typing takes it first, then a field of the inspector
    /// holding the keyboard, then the command it is the chord of, then the
    /// canvas.
    fn key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> ViewOutcome {
        if self.typing.is_some() && !modifiers.ctrl {
            self.type_key(key, layout, damage);
            return Outcome::none();
        }
        if self.inspector.has_keyboard() {
            if !modifiers.ctrl {
                let changes = self.inspect(|inspector, shown| {
                    inspector.on_key(key, modifiers, (layout, scale, theme), shown, damage)
                });
                for change in changes {
                    self.apply_change(change, layout, damage);
                }
                return Outcome::none();
            }
            let released = self.inspect(|inspector, shown| {
                inspector.release_keyboard((layout, scale, theme), shown, damage)
            });
            if let Some(change) = released {
                self.apply_change(change, layout, damage);
            }
        }
        let backing_up = matches!(key, Key::Named(NamedKey::Delete | NamedKey::Backspace))
            && self.tool == Tool::Pen
            && !self.pen.is_empty();
        if backing_up {
            self.pen.pop();
            damage.add(layout.canvas);
            return Outcome::none();
        }
        if let Some(command) = self.chord(key, modifiers) {
            return self.carry_out(command, layout, damage);
        }
        damage.add(layout.window);
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
            NUDGE * 10.0
        } else {
            NUDGE
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

    /// A key typed into text: an edit shows the text afresh, to land as one
    /// step when typing ends; a move of the caret changes no drawing.
    fn type_key(&mut self, key: Key, layout: &Layout, damage: &mut Region) {
        let Some(Typing { path, id, caret }) = self.typing.clone() else {
            return;
        };
        let drawing = self.shown().clone();
        let typed = drawing.node_at(&path).filter(|node| node.id() == id);
        let Some((node, Kind::Text { text, style })) = typed.map(|node| (node, node.kind())) else {
            self.typing = None;
            return;
        };
        let mut content = String::from(&*text.content);
        let mut caret = char_start(&content, caret);
        let edited = match key {
            Key::Named(NamedKey::Escape) => {
                self.finish_typing();
                damage.add(layout.window);
                return;
            }
            Key::Named(NamedKey::Enter) => {
                content.insert(caret, '\n');
                caret += 1;
                true
            }
            Key::Named(NamedKey::Backspace) => match content[..caret].chars().next_back() {
                Some(previous) => {
                    caret -= previous.len_utf8();
                    content.remove(caret);
                    true
                }
                None => false,
            },
            Key::Named(NamedKey::Delete) if caret < content.len() => {
                content.remove(caret);
                true
            }
            Key::Named(NamedKey::Left) => {
                if let Some(previous) = content[..caret].chars().next_back() {
                    caret -= previous.len_utf8();
                }
                false
            }
            Key::Named(NamedKey::Right) => {
                if let Some(next) = content[caret..].chars().next() {
                    caret += next.len_utf8();
                }
                false
            }
            Key::Named(NamedKey::Home) => {
                caret = content[..caret].rfind('\n').map_or(0, |at| at + 1);
                false
            }
            Key::Named(NamedKey::End) => {
                caret = content[caret..]
                    .find('\n')
                    .map_or(content.len(), |at| caret + at);
                false
            }
            Key::Char(character) if !character.is_control() => {
                content.insert(caret, character);
                caret += character.len_utf8();
                true
            }
            _ => return,
        };
        damage.add(layout.canvas);
        if !edited {
            self.typing = Some(Typing { path, id, caret });
            return;
        }
        let set = Text {
            content: Arc::from(content.as_str()),
            ..(**text).clone()
        };
        let rebuilt = node.with_kind(Kind::Text {
            text: Arc::new(set),
            style: style.clone(),
        });
        match rebuilt.and_then(|rebuilt| drawing.with_node_at(&path, Arc::new(rebuilt))) {
            Ok(drawing) => {
                self.show_live(drawing, TYPE_TEXT);
                self.typing = Some(Typing { path, id, caret });
            }
            Err(error) => self.say_error("Type", error.into()),
        }
    }

    /// Carry out what the inspector changed.
    pub(crate) fn apply_change(&mut self, change: Change, layout: &Layout, damage: &mut Region) {
        damage.add(layout.window);
        match change {
            Change::Paint {
                target,
                paint,
                settled,
            } => {
                let label = match target {
                    Target::Fill => "Fill",
                    Target::Stroke => "Stroke",
                };
                self.restyle(label, settled, &|style| match target {
                    Target::Fill => style.fill = paint.clone(),
                    Target::Stroke => style.stroke.paint = paint.clone(),
                });
            }
            Change::Width { width, settled } => {
                self.restyle("Stroke width", settled, &|style| style.stroke.width = width);
            }
            Change::Join(join) => self.restyle("Join", true, &|style| style.stroke.join = join),
            Change::Cap(cap) => self.restyle("Cap", true, &|style| style.stroke.cap = cap),
            Change::Dash(index) => self.restyle("Dashes", true, &|style| dash(style, index)),
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
            Change::Family(family) => {
                self.text.family = family;
                self.reset_text("Font", &|text| text.family = Arc::from(family));
            }
            Change::Size(size) => {
                self.text.size = size;
                self.reset_text("Text size", &|text| text.size = size);
            }
            Change::Bold(bold) => {
                self.text.bold = bold;
                let weight = if bold { 700 } else { 400 };
                self.reset_text("Bold", &|text| text.weight = weight);
            }
            Change::Italic(italic) => {
                self.text.italic = italic;
                self.reset_text("Italic", &|text| text.italic = italic);
            }
            Change::TextAlign(align) => {
                self.text.align = align;
                self.reset_text("Align text", &|text| text.align = align);
            }
        }
        self.refresh_inspector();
    }

    /// What an inspector change edits: the drawing typing shows while it is
    /// under way, so the change lands with it, else the document's.
    fn edit_base(&self) -> &tairix_drawing::Drawing {
        if self.typing.is_some() {
            self.shown()
        } else {
            self.document.drawing()
        }
    }

    /// Take what an inspector change made: shown, to land with typing under
    /// way or as `label` once `settled`, or landed as `label` now.
    fn take_change(
        &mut self,
        result: Result<tairix_drawing::Drawing, edit::EditError>,
        label: &'static str,
        settled: bool,
    ) {
        match result {
            Ok(drawing) if drawing.is(self.shown()) => {}
            Ok(drawing) if self.typing.is_some() => self.show_live(drawing, TYPE_TEXT),
            // Back where the document stands: nothing is under way.
            Ok(drawing) if drawing.is(self.document.drawing()) => self.drop_live(),
            Ok(drawing) if settled => self.commit(drawing, label),
            Ok(drawing) => self.show_live(drawing, label),
            Err(error) => self.say_error(label, error),
        }
    }

    /// The style new objects are drawn in altered by `alter`, and the
    /// selection's with it: shown while `settled` is false, landed as one
    /// step called `label` once it is.
    fn restyle(&mut self, label: &'static str, settled: bool, alter: &dyn Fn(&mut Style)) {
        alter(&mut self.style);
        if self.selection.is_empty() {
            return;
        }
        let altered = |style: &Style| {
            let mut style = style.clone();
            alter(&mut style);
            style
        };
        let result = edit::restyle(self.edit_base(), &self.selection, &altered);
        self.take_change(result, label, settled);
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
                return None;
            };
            let radius = radius.clamp(0.0, tairix_util::mathf::fmin(rect.width, rect.height) / 2.0);
            let rounded = tairix_drawing::shape::RectShape {
                rx: radius,
                ry: radius,
                ..*rect
            };
            if rounded == *rect {
                return None;
            }
            Some(Kind::Shape {
                shape: Shape::Rect(rounded),
                style: style.clone(),
            })
        });
    }

    /// Every selected object `change` makes anew made so, as one step; one
    /// it leaves as it is, not at all.
    fn reshape_selected(&mut self, label: &'static str, change: &dyn Fn(&Kind) -> Option<Kind>) {
        if self.selection.is_empty() {
            return;
        }
        let result = edit::each(self.edit_base(), &self.selection, |_, _, node| {
            Ok(change(node.kind())
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
                return None;
            };
            let mut altered = *star;
            alter(&mut altered);
            if altered == *star {
                return None;
            }
            let star = altered;
            Some(Kind::Shape {
                shape: Shape::Star(star),
                style: style.clone(),
            })
        });
    }

    /// Every selected text set anew by `alter`.
    fn reset_text(&mut self, label: &'static str, alter: &dyn Fn(&mut Text)) {
        self.reshape_selected(label, &|kind| {
            let Kind::Text { text, style } = kind else {
                return None;
            };
            let mut altered = (**text).clone();
            alter(&mut altered);
            if altered == **text {
                return None;
            }
            Some(Kind::Text {
                text: Arc::new(altered),
                style: style.clone(),
            })
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
            Ok(drawing.with_node_at(&[index], Arc::new(change(layer)))?)
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
    let Kind::Shape {
        shape: Shape::Path(shape),
        ..
    } = node.kind()
    else {
        return None;
    };
    let to_drawing = node.transform().then(edit::parent_space(drawing, path));
    Some((shape, to_drawing.invert()?.map(to)))
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
) -> Option<Node> {
    let moved = gripped(node, drawing, path, grip, to)?;
    let (shape, _) = path_frame(node, drawing, path, to)?;
    let Kind::Shape { style, .. } = node.kind() else {
        return None;
    };
    let shape = shape.with_node(grip.contour, grip.node, moved).ok()?;
    node.with_kind(Kind::Shape {
        shape: Shape::Path(Arc::new(shape)),
        style: style.clone(),
    })
    .ok()
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
