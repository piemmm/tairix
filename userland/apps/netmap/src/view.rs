//! The interactive map: what the pointer, the wheel, a trackpad's two-finger
//! scroll and a pinch do to the view, and how the map is drawn.
//!
//! Nodes are drawn at a constant on-screen size whatever the zoom — a map pin,
//! not a scaled picture — so zooming spreads the network out rather than
//! inflating it, and labels appear once the nodes are far enough apart to
//! carry them.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use tairix_abi::net_ipc::address_parts;
use tairix_abi::touch::PINCH_SCALE_ONE;
use tairix_controls::{wheel_steps, Fact, FactList, WHEEL_STEP};
use tairix_font::{BitmapFont, ELLIPSIS};
use tairix_geometry::{Point, Rect, Scale};
use tairix_icon::{badge, builtin_icon, BadgeHue, IconKind, PlateShape};
use tairix_input::{InputEvent, Key, NamedKey, PinchPhase, PointerButton};
use tairix_procinfo::render_ip;
use tairix_raster::{Canvas, Color, ScanScratch, Surface, SUBPIXEL};
use tairix_theme::Theme;
use tairix_util::mathf;

use crate::infer::{Evidence, Seen};
use crate::layout::round;
use crate::model::{
    DeviceClass, EdgeKind, Liveness, Map, Node, NodeId, NodeKey, NodeKind, OsFamily,
};
use crate::viewport::Viewport;

/// A node's plate radius, in logical pixels: every node is drawn this size
/// whatever the zoom.
pub const NODE_RADIUS: u32 = 18;
/// How far a press may travel and still be a click rather than a drag, in
/// logical pixels.
const DRAG_SLOP: u32 = 4;
/// The side of the square cells the placed plates and names are indexed
/// by, in device pixels: about a name's width, so a claim looks in a few.
const OCCUPANCY_CELL: u32 = 64;
/// The widest a node label is drawn, in logical pixels.
const LABEL_WIDTH: u32 = 150;
/// The detail panel's width, in logical pixels.
const PANEL_WIDTH: u32 = 300;
/// The room a fitted map keeps clear of the canvas edge for its outermost
/// plates and labels, in logical pixels.
const FIT_MARGIN: u32 = 64;
/// One wheel detent of Ctrl-zoom, as a ratio.
const ZOOM_STEP: (u32, u32) = (5, 4);
/// The most zoom steps one wheel event applies, so a hostile or wild delta
/// costs a bounded loop.
const MAX_ZOOM_STEPS: i64 = 16;
/// Vertices on the circle a halo or light is drawn as.
const CIRCLE_VERTICES: usize = 40;
/// The sticker a host's system emblem is drawn on, the same on either theme:
/// the emblem reads as part of the node's badge, not as the desktop's text.
const EMBLEM_PLATE: Color = Color::rgb(255, 255, 255);
/// The emblem's own colour on its sticker.
const EMBLEM_INK: Color = Color::rgb(0x2a, 0x2a, 0x30);
/// The rasters each cache keeps between frames before it is emptied. Plates
/// and glyphs are keyed by kind, hue or ink, and side, which a theme or density
/// change replaces wholesale, so a bound this size is never reached in steady
/// use.
const MAX_CACHED_RASTERS: usize = 96;

/// A primary press in progress.
#[derive(Clone, Copy, Debug)]
struct Press {
    origin: Point,
    last: Point,
    dragged: bool,
}

/// The map as the user sees and steers it.
pub struct MapView {
    map: Map,
    viewport: Viewport,
    canvas: Rect,
    selected: Option<NodeKey>,
    pointer: Point,
    press: Option<Press>,
    pinch_from: Option<u32>,
    ctrl: bool,
    zoom_carry: i64,
    pan_carry: (i64, i64),
    /// Whether the view keeps the whole map in sight as it changes: until the
    /// user zooms or pans it, and again once they ask with Home.
    follow: bool,
    /// The density last drawn or steered at, which a fit's margin is sized by.
    scale: Scale,
    notices: Vec<String>,
    /// The hue of the host each service is offered by, which the service
    /// wears.
    service_hues: BTreeMap<NodeId, BadgeHue>,
    unit_circle: Vec<(f64, f64)>,
    painter: Painter,
}

/// The renderer's reusable working storage: rasterised plates and glyphs, and
/// the scratch the scan converter and the ring polygons need.
#[derive(Default)]
struct Painter {
    plates: BTreeMap<(IconKind, BadgeHue, u32), Surface>,
    glyphs: BTreeMap<(IconKind, u32, [u8; 4]), Surface>,
    occupancy: Occupancy,
    scratch: ScanScratch,
    polygon: Vec<(i32, i32)>,
}

/// The plates and names already on the canvas, so a name is drawn only where
/// it overlaps neither. Each is tested exactly; a grid of cells, each holding
/// a list threaded through one flat table, finds the few near a claim, and
/// every table is kept from frame to frame.
#[derive(Default)]
struct Occupancy {
    origin: Point,
    cols: usize,
    rows: usize,
    /// Each cell's latest entry, one past its index; zero for none.
    heads: Vec<usize>,
    /// The placed box an entry stands for, and the cell's entry before it.
    entries: Vec<(usize, usize)>,
    placed: Vec<Rect>,
}

impl Occupancy {
    /// Forget everything placed, over `canvas`.
    fn reset(&mut self, canvas: Rect) {
        self.origin = canvas.origin;
        self.cols = usize::try_from(canvas.width.div_ceil(OCCUPANCY_CELL)).unwrap_or(0);
        self.rows = usize::try_from(canvas.height.div_ceil(OCCUPANCY_CELL)).unwrap_or(0);
        self.heads.clear();
        self.heads.resize(self.cols * self.rows, 0);
        self.entries.clear();
        self.placed.clear();
    }

    /// The columns and the rows of cells `rect` covers, clipped to the canvas.
    fn span(&self, rect: Rect) -> (Range<usize>, Range<usize>) {
        let axis = |start: i32, len: u32, origin: i32, count: usize| {
            let cell = i64::from(OCCUPANCY_CELL);
            let count = i64::try_from(count).unwrap_or(i64::MAX);
            let from = i64::from(start) - i64::from(origin);
            let lo = from.div_euclid(cell).clamp(0, count);
            let hi = (from + i64::from(len) + cell - 1)
                .div_euclid(cell)
                .clamp(lo, count);
            usize::try_from(lo).unwrap_or(0)..usize::try_from(hi).unwrap_or(0)
        };
        (
            axis(rect.left(), rect.width, self.origin.x, self.cols),
            axis(rect.top(), rect.height, self.origin.y, self.rows),
        )
    }

    /// Whether anything placed overlaps `rect`.
    fn overlaps(&self, rect: Rect) -> bool {
        let (cols, rows) = self.span(rect);
        rows.flat_map(|row| cols.clone().map(move |col| row * self.cols + col))
            .any(|cell| {
                let mut next = self.heads.get(cell).copied().unwrap_or(0);
                while let Some(&(placed, before)) =
                    next.checked_sub(1).and_then(|at| self.entries.get(at))
                {
                    if self
                        .placed
                        .get(placed)
                        .is_some_and(|other| !other.intersection(&rect).is_empty())
                    {
                        return true;
                    }
                    next = before;
                }
                false
            })
    }

    /// Place `rect`, whatever it overlaps.
    fn mark(&mut self, rect: Rect) {
        let index = self.placed.len();
        self.placed.push(rect);
        let (cols, rows) = self.span(rect);
        for row in rows {
            for col in cols.clone() {
                let cell = row * self.cols + col;
                if let Some(head) = self.heads.get_mut(cell) {
                    self.entries.push((index, *head));
                    *head = self.entries.len();
                }
            }
        }
    }

    /// Place `rect` if nothing placed overlaps it, and say whether it did.
    fn claim(&mut self, rect: Rect) -> bool {
        if self.overlaps(rect) {
            return false;
        }
        self.mark(rect);
        true
    }
}

/// How a node is drawn: its glyph, and the hue of the plate it stands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    /// The glyph.
    pub glyph: IconKind,
    /// The plate's hue, which the edges leading to the node wear too.
    pub hue: BadgeHue,
}

impl MapView {
    /// An empty view over `canvas`.
    #[must_use]
    pub fn new(canvas: Rect) -> Self {
        let unit_circle = (0..CIRCLE_VERTICES)
            .map(|i| {
                let angle = core::f64::consts::TAU * f64::from(u32::try_from(i).unwrap_or(0))
                    / f64::from(u32::try_from(CIRCLE_VERTICES).unwrap_or(1));
                (mathf::cos(angle), mathf::sin(angle))
            })
            .collect();
        Self {
            map: Map::new(),
            viewport: Viewport::new(canvas.width, canvas.height),
            canvas,
            selected: None,
            pointer: Point::ORIGIN,
            press: None,
            pinch_from: None,
            ctrl: false,
            zoom_carry: 0,
            pan_carry: (0, 0),
            follow: true,
            scale: Scale::ONE,
            notices: Vec::new(),
            service_hues: BTreeMap::new(),
            unit_circle,
            painter: Painter::default(),
        }
    }

    /// The map being shown.
    #[must_use]
    pub fn map(&self) -> &Map {
        &self.map
    }

    /// The current pan and zoom.
    #[must_use]
    pub fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    /// The selected node, if it is still on the map.
    #[must_use]
    pub fn selected(&self) -> Option<&Node> {
        let key = self.selected.as_ref()?;
        self.map.node(self.map.id_for(key)?)
    }

    /// Show `map`, already laid out. The selection follows its node across
    /// the rebuild by identity, and is dropped only if the node is gone.
    pub fn set_map(&mut self, map: Map) {
        self.service_hues = map
            .edges()
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Serves)
            .filter_map(|edge| Some((edge.to, node_look(map.node(edge.from)?).hue)))
            .collect();
        self.map = map;
        if self
            .selected
            .as_ref()
            .is_some_and(|key| self.map.id_for(key).is_none())
        {
            self.selected = None;
        }
        self.reframe();
    }

    /// Keep the whole map in sight while the view follows it, else keep the
    /// pan where the map can still be reached.
    fn reframe(&mut self) {
        if self.follow {
            let margin = self.scale.scale_length(FIT_MARGIN);
            self.viewport.fit(&self.map, margin);
        } else {
            self.viewport.clamp_pan(&self.map);
        }
    }

    /// The lines stated under the map: what a source could not tell, and why.
    pub fn set_notices(&mut self, notices: Vec<String>) {
        self.notices = notices;
    }

    /// Move the view onto a resized canvas.
    pub fn resize(&mut self, canvas: Rect) {
        self.canvas = canvas;
        self.viewport.set_view_size(canvas.width, canvas.height);
        self.reframe();
    }

    /// The hue `node` is drawn in: what it is, or for a service, the hue of
    /// the host offering it.
    fn hue(&self, node: &Node) -> BadgeHue {
        match node.kind {
            NodeKind::Service => self
                .service_hues
                .get(&node.id)
                .copied()
                .unwrap_or(BadgeHue::Grey),
            _ => node_look(node).hue,
        }
    }

    fn local(&self, at: Point) -> Point {
        Point::new(at.x - self.canvas.left(), at.y - self.canvas.top())
    }

    /// Route one input event, answering whether the picture changed.
    pub fn on_input(&mut self, event: &InputEvent, scale: Scale) -> bool {
        self.scale = scale;
        match *event {
            InputEvent::KeyPressed {
                key: Key::Named(NamedKey::Home),
                modifiers,
            } => {
                self.ctrl = modifiers.ctrl;
                let before = (self.viewport.zoom(), self.viewport.center());
                self.follow = true;
                self.reframe();
                before != (self.viewport.zoom(), self.viewport.center())
            }
            InputEvent::ModifiersChanged { modifiers }
            | InputEvent::KeyPressed { modifiers, .. }
            | InputEvent::KeyReleased { modifiers, .. } => {
                self.ctrl = modifiers.ctrl;
                false
            }
            InputEvent::PointerMoved { to } => self.pointer_moved(to, scale),
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => {
                self.press = Some(Press {
                    origin: self.pointer,
                    last: self.pointer,
                    dragged: false,
                });
                false
            }
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => self.released(scale),
            InputEvent::PointerPressed { .. } | InputEvent::PointerReleased { .. } => false,
            InputEvent::PointerScrolled { dx, dy } => {
                if self.ctrl {
                    self.wheel_zoom(dy)
                } else {
                    self.wheel_pan(dx, dy)
                }
            }
            InputEvent::Pinch {
                phase,
                scale: ratio,
                at,
            } => self.pinch(phase, ratio, at),
        }
    }

    fn pointer_moved(&mut self, to: Point, scale: Scale) -> bool {
        self.pointer = to;
        let Some(press) = self.press.as_mut() else {
            return false;
        };
        let slop = i64::from(scale.scale_length(DRAG_SLOP));
        let travel_x = i64::from(to.x) - i64::from(press.origin.x);
        let travel_y = i64::from(to.y) - i64::from(press.origin.y);
        if !press.dragged && travel_x * travel_x + travel_y * travel_y <= slop * slop {
            return false;
        }
        press.dragged = true;
        let (dx, dy) = (to.x - press.last.x, to.y - press.last.y);
        press.last = to;
        self.follow = false;
        self.viewport.pan_by(dx, dy);
        self.viewport.clamp_pan(&self.map);
        dx != 0 || dy != 0
    }

    fn released(&mut self, scale: Scale) -> bool {
        let Some(press) = self.press.take() else {
            return false;
        };
        if press.dragged {
            return false;
        }
        let hit = self
            .viewport
            .nearest_node(
                &self.map,
                self.local(self.pointer),
                scale.scale_length(NODE_RADIUS),
            )
            .and_then(|id| self.map.node(id))
            .map(|node| node.key.clone());
        let changed = hit != self.selected;
        self.selected = hit;
        changed
    }

    fn wheel_pan(&mut self, dx: i32, dy: i32) -> bool {
        let px_x = wheel_steps(dx, u64::from(WHEEL_STEP), &mut self.pan_carry.0);
        let px_y = wheel_steps(dy, u64::from(WHEEL_STEP), &mut self.pan_carry.1);
        if px_x == 0 && px_y == 0 {
            return false;
        }
        // Scrolling toward the end moves the view on, as dragging the content
        // the other way does.
        self.follow = false;
        self.viewport.pan_by(clamp_i32(-px_x), clamp_i32(-px_y));
        self.viewport.clamp_pan(&self.map);
        true
    }

    fn wheel_zoom(&mut self, dy: i32) -> bool {
        let steps = wheel_steps(dy.saturating_neg(), 1, &mut self.zoom_carry)
            .clamp(-MAX_ZOOM_STEPS, MAX_ZOOM_STEPS);
        if steps == 0 {
            return false;
        }
        let (num, den) = if steps > 0 {
            ZOOM_STEP
        } else {
            (ZOOM_STEP.1, ZOOM_STEP.0)
        };
        let mut zoom = u64::from(self.viewport.zoom());
        for _ in 0..steps.unsigned_abs() {
            zoom = zoom * u64::from(num) / u64::from(den);
        }
        self.follow = false;
        let before = self.viewport.zoom();
        self.viewport.set_zoom_about(
            self.local(self.pointer),
            u32::try_from(zoom).unwrap_or(u32::MAX),
        );
        self.viewport.clamp_pan(&self.map);
        self.viewport.zoom() != before
    }

    fn pinch(&mut self, phase: PinchPhase, ratio: u32, at: Point) -> bool {
        if phase == PinchPhase::Begin {
            self.pinch_from = Some(self.viewport.zoom());
            self.follow = false;
        }
        let Some(from) = self.pinch_from else {
            return false;
        };
        if phase.ends() {
            self.pinch_from = None;
        }
        let target = if phase == PinchPhase::Cancel {
            from
        } else {
            let scaled = u64::from(from) * u64::from(ratio) / u64::from(PINCH_SCALE_ONE);
            u32::try_from(scaled).unwrap_or(u32::MAX)
        };
        let before = self.viewport.zoom();
        self.viewport.set_zoom_about(self.local(at), target);
        self.viewport.clamp_pan(&self.map);
        self.viewport.zoom() != before
    }

    /// Draw the whole canvas into `surface`.
    pub fn render(&mut self, surface: &mut Surface, scale: Scale, theme: &Theme, font: BitmapFont) {
        if scale != self.scale {
            self.scale = scale;
            self.reframe();
        }
        let canvas = self.canvas;
        if let (Ok(x), Ok(y)) = (u32::try_from(canvas.left()), u32::try_from(canvas.top())) {
            surface.fill_rect(
                x,
                y,
                canvas.width,
                canvas.height,
                Color::from(theme.palette().surface),
            );
        }
        let radius = scale.scale_length(NODE_RADIUS);
        let frame = Frame {
            radius,
            bounds: canvas_bounds(canvas, radius),
            selected: self.selected.as_ref().and_then(|key| self.map.id_for(key)),
        };
        self.draw_edges(surface, scale);
        self.painter.occupancy.reset(canvas);
        self.draw_nodes(surface, frame, theme);
        self.draw_labels(surface, frame, scale, theme, font);
        if let Some(node) = self.selected() {
            let facts = node_facts(node);
            draw_panel(surface, canvas, &facts, &node.label, scale, theme, font);
        }
        draw_notices(
            surface,
            canvas,
            &self.notices,
            self.map.truncated(),
            scale,
            theme,
            font,
        );
    }

    /// Where `node` is drawn on the canvas.
    fn window_point(&self, node: &Node) -> Point {
        let at = self.viewport.to_window(node.pos);
        Point::new(at.x + self.canvas.origin.x, at.y + self.canvas.origin.y)
    }

    /// Every edge, as a line in the hue of the node it leads to; the default
    /// route and named routes heavier.
    fn draw_edges(&self, surface: &mut Surface, scale: Scale) {
        let thin = i32::try_from(scale.scale_length(2)).unwrap_or(2) * SUBPIXEL;
        let thick = i32::try_from(scale.scale_length(3)).unwrap_or(3) * SUBPIXEL;
        for edge in self.map.edges() {
            let (Some(from), Some(to)) = (self.map.node(edge.from), self.map.node(edge.to)) else {
                continue;
            };
            let (a, b) = (self.window_point(from), self.window_point(to));
            let weight = match edge.kind {
                EdgeKind::Default | EdgeKind::Routes => thick,
                _ => thin,
            };
            surface.stroke_polyline(
                &[
                    (a.x * SUBPIXEL, a.y * SUBPIXEL),
                    (b.x * SUBPIXEL, b.y * SUBPIXEL),
                ],
                weight,
                self.hue(to).middle(),
            );
        }
    }

    /// Every node in sight, each plate marked in the occupancy index so no
    /// name is drawn over it.
    fn draw_nodes(&mut self, surface: &mut Surface, frame: Frame, theme: &Theme) {
        for node in self.map.nodes() {
            let centre = self.window_point(node);
            if !frame.bounds.contains(centre) {
                continue;
            }
            let look = Look {
                hue: self.hue(node),
                ..node_look(node)
            };
            self.painter.node(
                surface,
                &self.unit_circle,
                centre,
                frame.radius,
                node,
                look,
                frame.selected == Some(node.id),
                theme,
            );
            self.painter
                .occupancy
                .mark(plate_rect(centre, frame.radius));
        }
    }

    /// Every name that has room, the selection's first and then in the map's
    /// own order, none drawn over a plate or another name.
    fn draw_labels(
        &mut self,
        surface: &mut Surface,
        frame: Frame,
        scale: Scale,
        theme: &Theme,
        font: BitmapFont,
    ) {
        let selected = frame.selected;
        let labels = selected.and_then(|id| self.map.node(id)).into_iter().chain(
            self.map
                .nodes()
                .iter()
                .filter(|node| Some(node.id) != selected),
        );
        for node in labels {
            let centre = self.window_point(node);
            if !frame.bounds.contains(centre) {
                continue;
            }
            let Some(label) = Label::plan(font, centre, frame.radius, &node.label, scale) else {
                continue;
            };
            if Some(node.id) == selected {
                self.painter.occupancy.mark(label.rect);
            } else if !self.painter.occupancy.claim(label.rect) {
                continue;
            }
            label.draw(surface, font, theme);
        }
    }
}

/// What one frame's node and label passes share.
#[derive(Clone, Copy)]
struct Frame {
    /// A node's plate radius at this frame's density.
    radius: u32,
    /// The canvas widened by a plate, so a node half off the edge still draws.
    bounds: Rect,
    /// The selected node, if it is still on the map.
    selected: Option<NodeId>,
}

impl Painter {
    /// A node's halo (accented when selected), its badge, the liveness light
    /// on one shoulder and its system's emblem on the other.
    #[allow(
        clippy::too_many_arguments,
        reason = "one node's whole drawing context, each part used once"
    )]
    fn node(
        &mut self,
        surface: &mut Surface,
        unit_circle: &[(f64, f64)],
        centre: Point,
        radius: u32,
        node: &Node,
        look: Look,
        selected: bool,
        theme: &Theme,
    ) {
        let palette = theme.palette();
        // A halo in the canvas colour parts the plate from the edges passing
        // under it; a selected node's is the accent, and wider.
        let (halo, halo_width) = if selected {
            (Color::from(palette.accent), radius / 5)
        } else {
            (Color::from(palette.surface), radius / 10)
        };
        self.disc(
            surface,
            unit_circle,
            centre,
            radius + halo_width.max(1),
            halo,
        );
        let r = i32::try_from(radius).unwrap_or(0);
        if let Some(plate) = self.plate(look.glyph, look.hue, radius * 2) {
            surface.blit(centre.x - r, centre.y - r, plate);
        }

        let shoulder = i32::try_from(radius * 7 / 10).unwrap_or(0);
        if let Some(emblem) = os_emblem(node.classification.os) {
            let at = Point::new(centre.x - shoulder, centre.y + shoulder);
            let badge = (radius / 2).max(2);
            self.disc(
                surface,
                unit_circle,
                at,
                badge + 1,
                Color::from(palette.surface),
            );
            self.disc(surface, unit_circle, at, badge, EMBLEM_PLATE);
            let side = badge * 8 / 5;
            let inset = i32::try_from(side / 2).unwrap_or(0);
            if let Some(glyph) = self.glyph(emblem, side, EMBLEM_INK) {
                surface.blit(at.x - inset, at.y - inset, glyph);
            }
        }

        let light = match node.liveness {
            Liveness::Reachable => Some(Color::from(palette.success)),
            Liveness::Unreachable => Some(Color::from(palette.danger)),
            Liveness::Advertised => Some(Color::from(palette.accent)),
            Liveness::Unknown => None,
        };
        if let Some(light) = light {
            let dot = Point::new(centre.x + shoulder, centre.y + shoulder);
            let dot_radius = (radius / 4).max(2);
            self.disc(
                surface,
                unit_circle,
                dot,
                dot_radius + 1,
                Color::from(palette.surface),
            );
            self.disc(surface, unit_circle, dot, dot_radius, light);
        }
    }

    fn disc(
        &mut self,
        surface: &mut Surface,
        unit_circle: &[(f64, f64)],
        centre: Point,
        radius: u32,
        ink: Color,
    ) {
        let r = f64::from(radius) * f64::from(SUBPIXEL);
        let (cx, cy) = (centre.x * SUBPIXEL, centre.y * SUBPIXEL);
        self.polygon.clear();
        self.polygon.extend(
            unit_circle
                .iter()
                .map(|&(c, s)| (cx + round(r * c), cy + round(r * s))),
        );
        Canvas::fill_polygon_subpixel(surface, &self.polygon, ink, &mut self.scratch);
    }

    /// `kind` in white on a disc of `hue` `side` pixels across, rasterised
    /// once and kept.
    fn plate(&mut self, kind: IconKind, hue: BadgeHue, side: u32) -> Option<&Surface> {
        let key = (kind, hue, side);
        if !self.plates.contains_key(&key) {
            if self.plates.len() >= MAX_CACHED_RASTERS {
                self.plates.clear();
            }
            let raster = badge(kind, hue, PlateShape::Round, side)?;
            self.plates.insert(key, raster);
        }
        self.plates.get(&key)
    }

    /// `kind` at `side` pixels in `ink`, rasterised once and kept.
    fn glyph(&mut self, kind: IconKind, side: u32, ink: Color) -> Option<&Surface> {
        let key = (kind, side, [ink.r, ink.g, ink.b, ink.a]);
        if !self.glyphs.contains_key(&key) {
            if self.glyphs.len() >= MAX_CACHED_RASTERS {
                self.glyphs.clear();
            }
            let raster = builtin_icon(kind, ink).rasterise(side)?;
            self.glyphs.insert(key, raster);
        }
        self.glyphs.get(&key)
    }
}

/// How a node is drawn: what it is when that is fixed, else what passive
/// inference made of it. Computers are blue, servers graphite, the routers and
/// switches traffic passes through orange, interfaces and the air sky,
/// networks indigo, printers purple, storage teal, phones green, connected
/// devices pink, and what nothing has identified grey. A service is drawn as
/// what it is shared from; [`MapView`] gives it its host's hue.
#[must_use]
pub fn node_look(node: &Node) -> Look {
    let (glyph, hue) = match node.kind {
        NodeKind::LocalHost => (IconKind::Computer, BadgeHue::Blue),
        NodeKind::Interface => (IconKind::Ethernet, BadgeHue::Sky),
        NodeKind::Subnet => (IconKind::Subnet, BadgeHue::Indigo),
        NodeKind::Service => (IconKind::Sharing, BadgeHue::Grey),
        NodeKind::Switch => (IconKind::NetworkSwitch, BadgeHue::Orange),
        NodeKind::AccessPoint => (IconKind::WiFi, BadgeHue::Sky),
        NodeKind::Beyond => (IconKind::Networking, BadgeHue::Indigo),
        NodeKind::Neighbour | NodeKind::RemoteHost | NodeKind::Router | NodeKind::Gateway => {
            match node.classification.device {
                DeviceClass::Unknown => match node.kind {
                    NodeKind::Router | NodeKind::Gateway => (IconKind::Router, BadgeHue::Orange),
                    _ => (IconKind::DeviceUnknown, BadgeHue::Grey),
                },
                DeviceClass::LocalHost | DeviceClass::Desktop => {
                    (IconKind::Computer, BadgeHue::Blue)
                }
                DeviceClass::Laptop => (IconKind::Laptop, BadgeHue::Blue),
                DeviceClass::Server => (IconKind::Server, BadgeHue::Graphite),
                DeviceClass::Router => (IconKind::Router, BadgeHue::Orange),
                DeviceClass::Switch => (IconKind::NetworkSwitch, BadgeHue::Orange),
                DeviceClass::AccessPoint => (IconKind::WiFi, BadgeHue::Sky),
                DeviceClass::Printer => (IconKind::Printer, BadgeHue::Purple),
                DeviceClass::Phone => (IconKind::Phone, BadgeHue::Green),
                DeviceClass::Nas => (IconKind::Nas, BadgeHue::Teal),
                DeviceClass::Iot => (IconKind::IotDevice, BadgeHue::Pink),
            }
        }
    };
    Look { glyph, hue }
}

/// The emblem a host's inferred operating system is marked with, for a
/// family that has one: a router's or switch's own glyph already says what
/// it runs, and this machine is labelled as itself.
#[must_use]
pub fn os_emblem(os: OsFamily) -> Option<IconKind> {
    match os {
        OsFamily::Linux => Some(IconKind::OsLinux),
        OsFamily::Android => Some(IconKind::OsAndroid),
        OsFamily::Windows => Some(IconKind::OsWindows),
        OsFamily::MacOs | OsFamily::Ios => Some(IconKind::OsApple),
        OsFamily::Unknown | OsFamily::Tairix | OsFamily::NetworkOs => None,
    }
}

/// What the detail panel says of a node.
fn node_facts(node: &Node) -> Vec<Fact> {
    let mut facts =
        Vec::with_capacity(3 + node.addrs.len() + node.facts.len() + node.evidence.len());
    facts.push(Fact::new("Kind", kind_name(node.kind)));
    let class = node.classification;
    if class.device != DeviceClass::Unknown {
        facts.push(Fact::new(
            "Looks like",
            guess(class_name(class.device), class.device_confidence),
        ));
    }
    if class.os != OsFamily::Unknown {
        facts.push(Fact::new(
            "Runs",
            guess(os_name(class.os), class.os_confidence),
        ));
    }
    for addr in &node.addrs {
        let (family, bytes) = address_parts(*addr);
        facts.push(Fact::new("Address", render_ip(family, &bytes)));
    }
    for (key, value) in &node.facts {
        facts.push(Fact::new(fact_label(key), value.as_str()));
    }
    for evidence in &node.evidence {
        facts.push(Fact::new("Evidence", evidence_line(evidence)));
    }
    facts
}

/// A guess as the panel states it: the name, and how sure unless certain.
fn guess(name: &str, confidence: u8) -> String {
    if confidence >= 100 {
        String::from(name)
    } else {
        format!("{name} ({confidence}% sure)")
    }
}

/// What one piece of evidence was, and what it suggested.
fn evidence_line(evidence: &Evidence) -> String {
    let seen = match &evidence.seen {
        Seen::Gateway => String::from("Is the default gateway"),
        Seen::Router => String::from("Routes to another network"),
        Seen::Vendor(name) => format!("Made by {name}"),
        Seen::Service(service) => format!("Advertises {service}"),
        Seen::Model(model) => format!("Names its model {model}"),
        Seen::MacOsVersion => String::from("States a macOS version"),
    };
    let device = evidence.device.map(|(class, _)| class_name(class));
    let os = evidence.os.map(|(family, _)| os_name(family));
    match (device, os) {
        (Some(device), Some(os)) => format!("{seen} ({device}, {os})"),
        (Some(one), None) | (None, Some(one)) => format!("{seen} ({one})"),
        (None, None) => seen,
    }
}

fn kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::LocalHost => "This machine",
        NodeKind::Interface => "Network interface",
        NodeKind::Subnet => "Network",
        NodeKind::Neighbour => "Host on the network",
        NodeKind::Service => "Service",
        NodeKind::Router => "Router",
        NodeKind::Gateway => "Default gateway",
        NodeKind::Switch => "Switch",
        NodeKind::AccessPoint => "Access point",
        NodeKind::RemoteHost => "Remote host",
        NodeKind::Beyond => "Beyond this network",
    }
}

fn os_name(os: OsFamily) -> &'static str {
    match os {
        OsFamily::Unknown => "Unknown",
        OsFamily::Tairix => "TAIRiX",
        OsFamily::Linux => "Linux",
        OsFamily::Android => "Android",
        OsFamily::Windows => "Windows",
        OsFamily::MacOs => "macOS",
        OsFamily::Ios => "iOS",
        OsFamily::NetworkOs => "Network equipment OS",
    }
}

fn class_name(class: DeviceClass) -> &'static str {
    match class {
        DeviceClass::Unknown => "Unknown",
        DeviceClass::LocalHost => "This machine",
        DeviceClass::Desktop => "Desktop computer",
        DeviceClass::Laptop => "Laptop",
        DeviceClass::Server => "Server",
        DeviceClass::Router => "Router",
        DeviceClass::Switch => "Switch",
        DeviceClass::AccessPoint => "Access point",
        DeviceClass::Printer => "Printer",
        DeviceClass::Phone => "Phone",
        DeviceClass::Nas => "Network storage",
        DeviceClass::Iot => "Connected device",
    }
}

fn fact_label(key: &str) -> &str {
    match key {
        "mac" => "Hardware address",
        "vendor" => "Made by",
        "mtu" => "MTU",
        "kind" => "Link",
        "neighbor" => "Reachability",
        "dns name" => "DNS name",
        "local name" => "Local name",
        "service" => "Service type",
        "port" => "Port",
        "host" => "Host",
        "txt" => "TXT data",
        other => other,
    }
}

/// The canvas widened by a node's radius, so a node half off the edge still
/// draws its visible half.
fn canvas_bounds(canvas: Rect, radius: u32) -> Rect {
    let pad = i32::try_from(radius * 2).unwrap_or(0);
    Rect::new(
        canvas.left() - pad,
        canvas.top() - pad,
        canvas.width.saturating_add(radius * 4),
        canvas.height.saturating_add(radius * 4),
    )
}

/// The square a node's plate and halo cover.
fn plate_rect(centre: Point, radius: u32) -> Rect {
    let reach = radius + radius / 5 + 1;
    let half = i32::try_from(reach).unwrap_or(0);
    Rect::new(centre.x - half, centre.y - half, reach * 2, reach * 2)
}

/// A node's name as it will be drawn under its plate: the text, elided to the
/// widest a label is drawn, and the box it fills.
struct Label<'a> {
    shown: &'a str,
    elided: bool,
    rect: Rect,
}

impl<'a> Label<'a> {
    /// Where `label` goes under the plate of a node at `centre`, or `None` for
    /// a node with no name.
    fn plan(
        font: BitmapFont,
        centre: Point,
        radius: u32,
        label: &'a str,
        scale: Scale,
    ) -> Option<Self> {
        if label.is_empty() {
            return None;
        }
        let (shown, elided) = font.elide_to_width(label, scale.scale_length(LABEL_WIDTH));
        let ellipsis = if elided { font.text_width(ELLIPSIS) } else { 0 };
        let width = font.text_width(shown) + ellipsis;
        let x = centre.x - i32::try_from(width / 2).unwrap_or(0);
        let y = centre.y + i32::try_from(radius + radius / 3).unwrap_or(0);
        Some(Self {
            shown,
            elided,
            rect: Rect::new(x, y, width, font.line_height()),
        })
    }

    fn draw(&self, surface: &mut Surface, font: BitmapFont, theme: &Theme) {
        let ink = Color::from(theme.palette().on_surface);
        let pen = font.draw_text(surface, self.rect.left(), self.rect.top(), self.shown, ink);
        if self.elided {
            font.draw_text(surface, pen, self.rect.top(), ELLIPSIS, ink);
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the panel's whole drawing context, each part used once"
)]
fn draw_panel(
    surface: &mut Surface,
    canvas: Rect,
    facts: &[Fact],
    title: &str,
    scale: Scale,
    theme: &Theme,
    font: BitmapFont,
) {
    let palette = theme.palette();
    let margin = scale.scale_length(12);
    let width = scale
        .scale_length(PANEL_WIDTH)
        .min(canvas.width.saturating_sub(margin * 2));
    if width == 0 {
        return;
    }
    let list = FactList::new(facts.to_vec());
    let title_height = font.line_height() + margin;
    let height = (title_height + list.measured_height(scale, theme) + margin)
        .min(canvas.height.saturating_sub(margin * 2));
    let x = canvas.right() - i32::try_from(width + margin).unwrap_or(0);
    let y = canvas.top() + i32::try_from(margin).unwrap_or(0);
    let (Ok(px), Ok(py)) = (u32::try_from(x), u32::try_from(y)) else {
        return;
    };
    surface.fill_round_rect(
        px,
        py,
        width,
        height,
        scale.scale_length(8),
        Color::from(palette.surface_raised),
    );
    let inset = i32::try_from(margin).unwrap_or(0);
    let (shown, _) = font.elide_to_width(title, width.saturating_sub(margin * 2));
    font.draw_text(
        surface,
        x + inset,
        y + inset / 2,
        shown,
        Color::from(palette.on_surface),
    );
    let list_top = y + i32::try_from(title_height).unwrap_or(0);
    list.render(
        surface,
        Rect::new(
            x + inset,
            list_top,
            width.saturating_sub(margin * 2),
            height.saturating_sub(title_height + margin),
        ),
        scale,
        theme,
    );
}

fn draw_notices(
    surface: &mut Surface,
    canvas: Rect,
    notices: &[String],
    truncated: bool,
    scale: Scale,
    theme: &Theme,
    font: BitmapFont,
) {
    let ink = Color::from(theme.palette().on_surface_muted);
    let margin = i32::try_from(scale.scale_length(12)).unwrap_or(0);
    let line = i32::try_from(font.line_height()).unwrap_or(0);
    let mut y = canvas.bottom() - margin - line;
    let truncation = truncated.then_some("Some of the network is not shown: the map is full.");
    for text in truncation
        .into_iter()
        .chain(notices.iter().map(String::as_str))
    {
        font.draw_text(surface, canvas.left() + margin, y, text, ink);
        y -= line;
    }
}

fn clamp_i32(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
