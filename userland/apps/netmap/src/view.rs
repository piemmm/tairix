//! The interactive map: what the pointer, the wheel, a trackpad's two-finger
//! scroll and a pinch do to the view, and how the map is drawn.
//!
//! Nodes are drawn at a constant on-screen size whatever the zoom — a map pin,
//! not a scaled picture — so zooming spreads the network out rather than
//! inflating it, and labels appear once the nodes are far enough apart to
//! carry them.
//!
//! A service is folded into its host until the user spreads it: the host wears
//! a count of what it offers and names it in its tooltip, and a click fans the
//! services out around it. One placement says where every plate stands, and
//! drawing, clicking and the tooltip all read it, so a plate is hit where it
//! is drawn.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::f64::consts::FRAC_PI_2;
use core::ops::Range;

use tairix_abi::net_ipc::address_parts;
use tairix_abi::touch::PINCH_SCALE_ONE;
use tairix_abi::window_ipc::TOOLTIP_TEXT_MAX;
use tairix_abi::Errno;
use tairix_controls::{wheel_steps, Fact, FactList, WHEEL_STEP};
use tairix_font::{BitmapFont, ELLIPSIS};
use tairix_geometry::{Point, Rect, Scale};
use tairix_icon::{badge, builtin_icon, BadgeHue, IconKind, PlateShape};
use tairix_input::{InputEvent, Key, NamedKey, PinchPhase, PointerButton};
use tairix_procinfo::{render_ip, render_mac};
use tairix_raster::{Canvas, Color, ScanScratch, Surface, SUBPIXEL};
use tairix_theme::motion::smoothstep;
use tairix_theme::{Fade, MotionInteraction, MotionTheme, TextRole, Theme};
use tairix_util::join::join_within;
use tairix_util::mathf;

use crate::infer::{Evidence, Seen};
use crate::ingest::assigned_oui;
use crate::menu::{elide, menu, NodeMenu, Steer, Subject};
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
/// How far from a spread host's centre its nearest services stand, in logical
/// pixels: clear of its plate, its count and their own plates.
const SPREAD_FIRST_ARC: u32 = 68;
/// How much further out each further arc of services stands, in logical
/// pixels: a plate and the name under it.
const SPREAD_ARC_STEP: u32 = 64;
/// How far apart two services on one arc stand, in logical pixels: two
/// plates with their halos, and a gap.
const SPREAD_PITCH: u32 = 52;
/// The widest an arc of services reaches, in thousandths of a radian (three
/// quarters of a turn), centred on the host's outward direction: the quarter
/// it leaves open is where the line into the host arrives.
const SPREAD_ARC_MILLIRADIANS: u32 = 4712;
/// Hosts whose services are drawn at once: the one spreading, and the one
/// still folding.
const MAX_SPREADS: usize = 2;
/// What a host's count reads for each number of services it states exactly;
/// a host offering more reads [`COUNT_MORE`].
const COUNTS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
/// What a host offering more services than [`COUNTS`] states reads.
const COUNT_MORE: &str = "9+";
/// How far a spread service's backing reaches past its name, in logical
/// pixels.
const BACKING_PAD: u32 = 2;

/// A primary press in progress.
#[derive(Clone, Copy, Debug)]
struct Press {
    origin: Point,
    last: Point,
    dragged: bool,
}

/// A host whose services are out, or on their way out or in.
#[derive(Clone, Debug)]
struct Spread {
    host: NodeKey,
    /// How far out its services are: none at `0`, all the way at `u8::MAX`.
    fade: Fade,
}

/// Which plates a plate is drawn over: the map's own, then the services a
/// spread host has out, then the spread hosts, which cover their services as
/// those leave and return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layer {
    Map,
    Service,
    SpreadHost,
}

/// One plate the view draws: the node, where its centre is, and its layer.
#[derive(Clone, Copy, Debug)]
struct Plate {
    id: NodeId,
    at: Point,
    layer: Layer,
}

/// What an input event asks of the shell the view runs in.
#[derive(Debug)]
pub enum Reaction {
    /// Nothing visible changed.
    Idle,
    /// The picture changed and wants painting.
    Repaint,
    /// Open this context menu, or the reason it could not be built.
    Menu(Result<Box<NodeMenu>, Errno>),
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
    /// The host each service is offered by.
    owners: BTreeMap<NodeId, NodeId>,
    /// Each host's services, ordered by identity.
    offered: BTreeMap<NodeId, Vec<NodeId>>,
    /// The hosts whose services are drawn, oldest first.
    spreads: Vec<Spread>,
    motion: MotionTheme,
    /// Every plate the view draws, in drawing order.
    plates: Vec<Plate>,
    /// Where the detail panel was last drawn: it covers the map under it, so
    /// nothing there is hit or tipped.
    panel: Option<Rect>,
    /// A spread host's services' offsets from its centre, kept between uses.
    offsets: Vec<(i32, i32)>,
    unit_circle: Vec<(f64, f64)>,
    painter: Painter,
}

/// The renderer's reusable working storage: rasterised plates and glyphs, and
/// the scratch the scan converter and the ring polygons need.
#[derive(Default)]
struct Painter {
    plates: BTreeMap<(IconKind, BadgeHue, u32), Surface>,
    glyphs: BTreeMap<(IconKind, u32, [u8; 4]), Surface>,
    /// What the map's own names keep clear of.
    occupancy: Occupancy,
    /// What a spread's names keep clear of.
    overlay: Occupancy,
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
    /// An empty view over `canvas`, animating at the pace `motion` sets.
    #[must_use]
    pub fn new(canvas: Rect, motion: MotionTheme) -> Self {
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
            owners: BTreeMap::new(),
            offered: BTreeMap::new(),
            spreads: Vec::new(),
            motion,
            plates: Vec::new(),
            panel: None,
            offsets: Vec::new(),
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

    /// Whether `host`'s services are out, or on their way out.
    #[must_use]
    pub fn is_spread(&self, host: &NodeKey) -> bool {
        self.spreads
            .iter()
            .any(|spread| spread.host == *host && spread.fade.target() == u8::MAX)
    }

    /// Show `map`, already laid out. The selection and the spread services
    /// follow their nodes across the rebuild by identity, and are dropped only
    /// if the nodes are gone.
    pub fn set_map(&mut self, map: Map) {
        self.owners.clear();
        self.offered.clear();
        for edge in map
            .edges()
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Serves)
        {
            self.owners.insert(edge.to, edge.from);
            self.offered.entry(edge.from).or_default().push(edge.to);
        }
        let key = |id: &NodeId| map.node(*id).map(|node| &node.key);
        for services in self.offered.values_mut() {
            services.sort_unstable_by(|a, b| key(a).cmp(&key(b)));
        }
        self.map = map;
        let (map, offered) = (&self.map, &self.offered);
        self.spreads.retain(|spread| {
            map.id_for(&spread.host)
                .is_some_and(|id| offered.contains_key(&id))
        });
        if self
            .selected
            .as_ref()
            .is_some_and(|key| self.map.id_for(key).is_none())
        {
            self.deselect();
        }
        self.reframe();
    }

    /// Clear the selection, answering whether there was one. Its panel goes
    /// with it.
    fn deselect(&mut self) -> bool {
        self.panel = None;
        self.selected.take().is_some()
    }

    /// Whether `at` lies on the detail panel rather than the map.
    fn on_panel(&self, at: Point) -> bool {
        self.panel.is_some_and(|panel| panel.contains(at))
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

    /// Fit the whole map and keep following it, answering whether the view
    /// moved.
    fn fit(&mut self) -> bool {
        let before = (self.viewport.zoom(), self.viewport.center());
        self.follow = true;
        self.reframe();
        before != (self.viewport.zoom(), self.viewport.center())
    }

    /// The lines stated under the map: what a source could not tell, and why.
    pub fn set_notices(&mut self, notices: Vec<String>) {
        self.notices = notices;
    }

    /// Animate at the pace `motion` sets from the next change on.
    pub fn set_motion(&mut self, motion: MotionTheme) {
        self.motion = motion;
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
                .owners
                .get(&node.id)
                .and_then(|host| self.map.node(*host))
                .map_or(BadgeHue::Grey, |host| node_look(host).hue),
            _ => node_look(node).hue,
        }
    }

    fn local(&self, at: Point) -> Point {
        Point::new(at.x - self.canvas.left(), at.y - self.canvas.top())
    }

    /// Route one input event arriving at `now_ns`, answering what it asks of
    /// the shell.
    pub fn on_input(&mut self, event: &InputEvent, scale: Scale, now_ns: u64) -> Reaction {
        self.scale = scale;
        let changed = match *event {
            InputEvent::KeyPressed {
                key: Key::Named(NamedKey::Home),
                modifiers,
            } => {
                self.ctrl = modifiers.ctrl;
                self.fit()
            }
            InputEvent::KeyPressed {
                key: Key::Named(NamedKey::Escape),
                modifiers,
            } => {
                self.ctrl = modifiers.ctrl;
                let deselected = self.deselect();
                self.fold_all(now_ns) | deselected
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
            } => self.released(scale, now_ns),
            InputEvent::PointerPressed {
                button: PointerButton::Secondary,
            } => return Reaction::Menu(self.context_menu(scale, now_ns)),
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
        };
        if changed {
            Reaction::Repaint
        } else {
            Reaction::Idle
        }
    }

    /// Carry out what a menu row asked of the view, answering whether the
    /// picture changed.
    pub fn steer(&mut self, steer: &Steer, now_ns: u64) -> bool {
        match steer {
            Steer::ShowServices(host) => self.spread(host, now_ns),
            Steer::HideServices(host) => self.fold(host, now_ns),
            Steer::Fit => self.fit(),
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

    fn released(&mut self, scale: Scale, now_ns: u64) -> bool {
        let Some(press) = self.press.take() else {
            return false;
        };
        if press.dragged {
            return false;
        }
        self.clicked(scale, now_ns)
    }

    /// A click selects what it lands on. On a host with services it spreads
    /// them, or folds them when that host was already selected and spread; on
    /// a spread service it keeps its host spread; anywhere else it folds every
    /// spread, and on empty canvas it clears the selection too.
    fn clicked(&mut self, scale: Scale, now_ns: u64) -> bool {
        if self.on_panel(self.pointer) {
            return false;
        }
        self.lay_plates(scale, now_ns);
        let Some(node) = self
            .hit(self.pointer, scale.scale_length(NODE_RADIUS))
            .and_then(|plate| self.map.node(plate.id))
        else {
            let deselected = self.deselect();
            return self.fold_all(now_ns) | deselected;
        };
        let (id, key) = (node.id, node.key.clone());
        let again = self.selected.as_ref() == Some(&key);
        let host = self
            .owners
            .get(&id)
            .and_then(|host| self.map.node(*host))
            .map(|host| host.key.clone());
        let spread = if self.offered.contains_key(&id) {
            if again && self.is_spread(&key) {
                self.fold(&key, now_ns)
            } else {
                self.spread(&key, now_ns)
            }
        } else if let Some(host) = host {
            self.spread(&host, now_ns)
        } else {
            self.fold_all(now_ns)
        };
        self.selected = Some(key);
        spread || !again
    }

    /// The menu for whatever the secondary press landed on, anchored at it:
    /// the node under it, the selected node when it is the panel describing
    /// that node, else the map's own.
    fn context_menu(&mut self, scale: Scale, now_ns: u64) -> Result<Box<NodeMenu>, Errno> {
        self.lay_plates(scale, now_ns);
        let pressed = if self.on_panel(self.pointer) {
            self.selected.as_ref().and_then(|key| self.map.id_for(key))
        } else {
            self.hit(self.pointer, scale.scale_length(NODE_RADIUS))
                .map(|plate| plate.id)
        };
        let subject = pressed
            .and_then(|id| self.map.node(id))
            .map(|node| Subject {
                node,
                services: self.offered.get(&node.id).map_or(0, Vec::len),
                spread: self.is_spread(&node.key),
            });
        menu(subject, self.pointer).map(Box::new)
    }

    /// Spread `host`'s services and fold every other host's, answering whether
    /// anything is now on its way somewhere new. A host with no services has
    /// nothing to spread.
    fn spread(&mut self, host: &NodeKey, now_ns: u64) -> bool {
        if !self
            .map
            .id_for(host)
            .is_some_and(|id| self.offered.contains_key(&id))
        {
            return false;
        }
        let duration = self.motion.duration(MotionInteraction::PanelOpen);
        let mut changed = false;
        for other in self.spreads.iter_mut().filter(|other| other.host != *host) {
            changed |= retarget(&mut other.fade, now_ns, duration, 0);
        }
        if let Some(open) = self.spreads.iter_mut().find(|open| open.host == *host) {
            return retarget(&mut open.fade, now_ns, duration, u8::MAX) | changed;
        }
        if self.spreads.len() >= MAX_SPREADS {
            // Every spread left is folding by now; the oldest snaps shut.
            self.spreads.remove(0);
        }
        self.spreads.push(Spread {
            host: host.clone(),
            fade: Fade::start(now_ns, duration, 0, u8::MAX),
        });
        true
    }

    /// Fold `host`'s services back in.
    fn fold(&mut self, host: &NodeKey, now_ns: u64) -> bool {
        let duration = self.motion.duration(MotionInteraction::PanelOpen);
        self.spreads
            .iter_mut()
            .find(|spread| spread.host == *host)
            .is_some_and(|spread| retarget(&mut spread.fade, now_ns, duration, 0))
    }

    /// Fold every spread host's services back in.
    fn fold_all(&mut self, now_ns: u64) -> bool {
        let duration = self.motion.duration(MotionInteraction::PanelOpen);
        self.spreads.iter_mut().fold(false, |changed, spread| {
            retarget(&mut spread.fade, now_ns, duration, 0) | changed
        })
    }

    /// Nanoseconds until the next animation frame is due, or `None` when
    /// nothing is moving.
    #[must_use]
    pub fn next_frame_in(&self, now_ns: u64) -> Option<u64> {
        self.spreads
            .iter()
            .filter_map(|spread| spread.fade.next_frame_in(now_ns))
            .min()
    }

    /// The tooltip for the plate under the pointer: the region it covers and
    /// its one line. None while a gesture or an animation is under way, since
    /// a tip would chase a moving plate.
    pub fn tip(&mut self, scale: Scale, now_ns: u64) -> Option<(Rect, String)> {
        let moving = self.spreads.iter().any(|spread| spread.fade.running());
        if self.press.is_some() || self.pinch_from.is_some() || moving {
            return None;
        }
        if self.on_panel(self.pointer) {
            return None;
        }
        self.lay_plates(scale, now_ns);
        let radius = scale.scale_length(NODE_RADIUS);
        let plate = self.hit(self.pointer, radius)?;
        let node = self.map.node(plate.id)?;
        Some((plate_rect(plate.at, radius), self.tip_line(node)))
    }

    /// What a node's tooltip says: a service's name, type and port; a host's
    /// name and the services it offers; anything else's full name.
    fn tip_line(&self, node: &Node) -> String {
        let line = match (&node.endpoint, self.offered.get(&node.id)) {
            (Some(endpoint), _) => format!(
                "{} — {}, port {}",
                node.label,
                endpoint.short_type(),
                endpoint.port
            ),
            (None, Some(services)) => self.offering(&node.label, services),
            (None, None) => node.label.clone(),
        };
        elide(&line, TOOLTIP_TEXT_MAX)
    }

    /// `label`, how many services it offers, and their types, each once with
    /// how many instances of it there are when more than one.
    fn offering(&self, label: &str, services: &[NodeId]) -> String {
        let mut types: BTreeMap<&str, usize> = BTreeMap::new();
        for endpoint in services
            .iter()
            .filter_map(|id| self.map.node(*id)?.endpoint.as_ref())
        {
            *types.entry(endpoint.short_type()).or_default() += 1;
        }
        let noun = if services.len() == 1 {
            "service"
        } else {
            "services"
        };
        let head = format!(
            "{} — {} {noun}: ",
            elide(label, TOOLTIP_TEXT_MAX / 2),
            services.len()
        );
        let entries = types.iter().map(|(name, &count)| {
            if count > 1 {
                format!("{name} ({count})")
            } else {
                String::from(*name)
            }
        });
        let list = join_within(entries, TOOLTIP_TEXT_MAX.saturating_sub(head.len()));
        format!("{head}{list}")
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

    /// Draw the whole canvas into `surface` as it stands at `now_ns`, then let
    /// go of every animation that frame finished.
    pub fn render(&mut self, surface: &mut Surface, scale: Scale, theme: &Theme, now_ns: u64) {
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
        let body = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
        let radius = scale.scale_length(NODE_RADIUS);
        let frame = Frame {
            radius,
            bounds: canvas_bounds(canvas, radius),
            selected: self.selected.as_ref().and_then(|key| self.map.id_for(key)),
        };
        self.lay_plates(scale, now_ns);
        self.draw_map_edges(surface, scale);
        self.painter.occupancy.reset(canvas);
        self.draw_plates(surface, frame, theme, Layer::Map);
        self.mark_plates(frame, Layer::Map, Pass::Map);
        self.mark_plates(frame, Layer::SpreadHost, Pass::Map);
        self.draw_names(surface, frame, scale, theme, body, Pass::Map);
        if !self.spreads.is_empty() {
            self.draw_spread_lines(surface, scale);
            self.painter.overlay.reset(canvas);
            self.draw_plates(surface, frame, theme, Layer::Service);
            self.mark_plates(frame, Layer::Service, Pass::Spread);
            self.mark_plates(frame, Layer::SpreadHost, Pass::Spread);
            self.draw_names(surface, frame, scale, theme, body, Pass::Spread);
            self.draw_plates(surface, frame, theme, Layer::SpreadHost);
        }
        self.panel = self.selected().and_then(|node| {
            let facts = node_facts(node);
            draw_panel(surface, canvas, &facts, &node.label, scale, theme, body)
        });
        draw_notices(
            surface,
            canvas,
            &self.notices,
            self.map.truncated(),
            scale,
            theme,
            body,
        );
        self.settle(now_ns);
    }

    /// Let go of every animation finished by `now_ns`: a host whose services
    /// came all the way out keeps them there, and one whose services are all
    /// the way in is no longer spread. [`render`](Self::render) does this once
    /// it has drawn the frame; a shell whose due frame had nothing in sight to
    /// draw calls it itself.
    pub fn settle(&mut self, now_ns: u64) {
        for spread in &mut self.spreads {
            if spread.fade.running() && spread.fade.strength(now_ns) == spread.fade.target() {
                spread.fade.settle();
            }
        }
        self.spreads
            .retain(|spread| spread.fade.running() || spread.fade.target() != 0);
    }

    /// Where every plate stands at `now_ns`, in drawing order: the map's own
    /// nodes that are in sight, then each spread host's services as far out as
    /// its spread has come, then the spread hosts themselves.
    fn lay_plates(&mut self, scale: Scale, now_ns: u64) {
        let bounds = canvas_bounds(self.canvas, scale.scale_length(NODE_RADIUS));
        let mut hosts = [None; MAX_SPREADS];
        for (slot, spread) in hosts.iter_mut().zip(&self.spreads) {
            *slot = self.map.id_for(&spread.host);
        }
        let mut plates = core::mem::take(&mut self.plates);
        let mut offsets = core::mem::take(&mut self.offsets);
        plates.clear();
        for node in self.map.nodes() {
            if node.kind == NodeKind::Service || hosts.contains(&Some(node.id)) {
                continue;
            }
            let at = self.window_point(node);
            if bounds.contains(at) {
                plates.push(Plate {
                    id: node.id,
                    at,
                    layer: Layer::Map,
                });
            }
        }
        for (spread, host) in self.spreads.iter().zip(hosts) {
            let Some(host) = host.and_then(|id| self.map.node(id)) else {
                continue;
            };
            let Some(services) = self.offered.get(&host.id) else {
                continue;
            };
            let out = i64::from(smoothstep(spread.fade.strength(now_ns)));
            if out == 0 {
                continue;
            }
            let centre = self.window_point(host);
            spread_offsets(services.len(), outward(host.pos), scale, &mut offsets);
            let travel = |offset: i32| {
                i32::try_from(i64::from(offset) * out / i64::from(u8::MAX)).unwrap_or(offset)
            };
            for (&id, &(dx, dy)) in services.iter().zip(&offsets) {
                plates.push(Plate {
                    id,
                    at: Point::new(
                        centre.x.saturating_add(travel(dx)),
                        centre.y.saturating_add(travel(dy)),
                    ),
                    layer: Layer::Service,
                });
            }
        }
        for host in hosts.into_iter().flatten() {
            if let Some(node) = self.map.node(host) {
                plates.push(Plate {
                    id: host,
                    at: self.window_point(node),
                    layer: Layer::SpreadHost,
                });
            }
        }
        self.plates = plates;
        self.offsets = offsets;
    }

    /// The topmost laid plate whose disc of `radius` holds `at`.
    fn hit(&self, at: Point, radius: u32) -> Option<Plate> {
        let limit = i64::from(radius) * i64::from(radius);
        self.plates.iter().rev().copied().find(|plate| {
            let dx = i64::from(plate.at.x) - i64::from(at.x);
            let dy = i64::from(plate.at.y) - i64::from(at.y);
            dx * dx + dy * dy <= limit
        })
    }

    /// Where `node` is drawn on the canvas when it is not out of a spread host.
    fn window_point(&self, node: &Node) -> Point {
        let at = self.viewport.to_window(node.pos);
        Point::new(at.x + self.canvas.origin.x, at.y + self.canvas.origin.y)
    }

    /// The rectangle a frame that only moves services must repaint: all that
    /// every moving spread can reach — its host, its services' plates at their
    /// farthest, the lines to them and the names under them — clipped to the
    /// canvas, or `None` when nothing in sight moves. The map beneath a spread
    /// is laid out as if nothing were spread, so nothing outside it changes.
    pub fn frame_damage(&mut self, scale: Scale, theme: &Theme) -> Option<Rect> {
        let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
        let radius = scale.scale_length(NODE_RADIUS);
        let reach = to_i32(plate_reach(radius));
        let pad = scale.scale_length(BACKING_PAD);
        let side = reach.max(to_i32(
            scale.scale_length(LABEL_WIDTH) / 2 + font.text_width(ELLIPSIS) + pad,
        ));
        let below = reach.max(to_i32(radius + radius / 3 + font.line_height() + pad));
        let mut offsets = core::mem::take(&mut self.offsets);
        let mut damage: Option<Rect> = None;
        for spread in self.spreads.iter().filter(|spread| spread.fade.running()) {
            let Some(host) = self
                .map
                .id_for(&spread.host)
                .and_then(|id| self.map.node(id))
            else {
                continue;
            };
            let Some(services) = self.offered.get(&host.id) else {
                continue;
            };
            spread_offsets(services.len(), outward(host.pos), scale, &mut offsets);
            let (left, right, top, bottom) =
                offsets
                    .iter()
                    .fold((0, 0, 0, 0), |(left, right, top, bottom), &(dx, dy)| {
                        (left.min(dx), right.max(dx), top.min(dy), bottom.max(dy))
                    });
            let centre = self.window_point(host);
            let x = centre.x.saturating_add(left).saturating_sub(side);
            let y = centre.y.saturating_add(top).saturating_sub(reach);
            let far_x = centre.x.saturating_add(right).saturating_add(side);
            let far_y = centre.y.saturating_add(bottom).saturating_add(below);
            let reached = Rect::new(
                x,
                y,
                u32::try_from(far_x.saturating_sub(x)).unwrap_or(0),
                u32::try_from(far_y.saturating_sub(y)).unwrap_or(0),
            );
            damage = Some(damage.map_or(reached, |so_far| so_far.union(&reached)));
        }
        self.offsets = offsets;
        damage
            .map(|damage| damage.intersection(&self.canvas))
            .filter(|damage| !damage.is_empty())
    }

    /// Every edge between the map's own nodes, each in the hue of the node it
    /// leads to; the default route and named routes heavier.
    fn draw_map_edges(&self, surface: &mut Surface, scale: Scale) {
        let thin = line_weight(scale, 2);
        let thick = line_weight(scale, 3);
        for edge in self.map.edges() {
            let (Some(from), Some(to)) = (self.map.node(edge.from), self.map.node(edge.to)) else {
                continue;
            };
            if from.kind == NodeKind::Service || to.kind == NodeKind::Service {
                continue;
            }
            let weight = match edge.kind {
                EdgeKind::Default | EdgeKind::Routes => thick,
                _ => thin,
            };
            stroke(
                surface,
                self.window_point(from),
                self.window_point(to),
                weight,
                self.hue(to),
            );
        }
    }

    /// The line from each spread host to each of its services that is out, in
    /// the host's hue.
    fn draw_spread_lines(&self, surface: &mut Surface, scale: Scale) {
        let thin = line_weight(scale, 2);
        for plate in self
            .plates
            .iter()
            .filter(|plate| plate.layer == Layer::Service)
        {
            let host = self
                .owners
                .get(&plate.id)
                .and_then(|host| self.map.node(*host));
            if let Some(host) = host {
                stroke(
                    surface,
                    self.window_point(host),
                    plate.at,
                    thin,
                    node_look(host).hue,
                );
            }
        }
    }

    /// Every laid plate of `layer` that is in sight.
    fn draw_plates(&mut self, surface: &mut Surface, frame: Frame, theme: &Theme, layer: Layer) {
        let small = BitmapFont::for_role(theme.fonts(), TextRole::Caption, self.scale);
        for plate in self.plates.iter().filter(|plate| plate.layer == layer) {
            if !frame.bounds.contains(plate.at) {
                continue;
            }
            let Some(node) = self.map.node(plate.id) else {
                continue;
            };
            let badge = Badge {
                node,
                centre: plate.at,
                radius: frame.radius,
                look: Look {
                    hue: self.hue(node),
                    ..node_look(node)
                },
                selected: frame.selected == Some(node.id),
                services: self.offered.get(&node.id).map_or(0, Vec::len),
            };
            self.painter
                .node(surface, &self.unit_circle, &badge, theme, small);
        }
    }

    /// Mark every laid plate of `layer` in `pass`'s occupancy, so none of its
    /// names is drawn over one.
    fn mark_plates(&mut self, frame: Frame, layer: Layer, pass: Pass) {
        let occupancy = self.painter.occupancy_for(pass);
        for plate in self.plates.iter().filter(|plate| plate.layer == layer) {
            if frame.bounds.contains(plate.at) {
                occupancy.mark(plate_rect(plate.at, frame.radius));
            }
        }
    }

    /// Every name in `pass` that has room, the selection's first, none drawn
    /// over a plate or another name of that pass. The map's names belong to
    /// its own nodes and the spread hosts; a spread's names belong to the
    /// services out, and stand on a backing so they read over the map beneath.
    fn draw_names(
        &mut self,
        surface: &mut Surface,
        frame: Frame,
        scale: Scale,
        theme: &Theme,
        font: BitmapFont,
        pass: Pass,
    ) {
        let selected = frame.selected;
        let ours = |plate: &&Plate| (plate.layer == Layer::Service) == (pass == Pass::Spread);
        let chosen = self
            .plates
            .iter()
            .filter(ours)
            .filter(|plate| Some(plate.id) == selected);
        let rest = self
            .plates
            .iter()
            .filter(ours)
            .filter(|plate| Some(plate.id) != selected);
        let pad = scale.scale_length(BACKING_PAD);
        let occupancy = self.painter.occupancy_for(pass);
        for plate in chosen.chain(rest) {
            if !frame.bounds.contains(plate.at) {
                continue;
            }
            let Some(node) = self.map.node(plate.id) else {
                continue;
            };
            let Some(label) = Label::plan(font, plate.at, frame.radius, &node.label, scale) else {
                continue;
            };
            let footprint = match pass {
                Pass::Map => label.rect,
                Pass::Spread => grown(label.rect, pad),
            };
            if Some(node.id) == selected {
                occupancy.mark(footprint);
            } else if !occupancy.claim(footprint) {
                continue;
            }
            if pass == Pass::Spread {
                fill_backing(surface, footprint, theme);
            }
            label.draw(surface, font, theme);
        }
    }
}

/// Which of a frame's two name passes a plate's name belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pass {
    /// The map's own nodes, laid out as if nothing were spread.
    Map,
    /// The services spread hosts have out, over the map.
    Spread,
}

/// A line from `a` to `b`, `weight` subpixels wide, in `hue`.
fn stroke(surface: &mut Surface, a: Point, b: Point, weight: i32, hue: BadgeHue) {
    surface.stroke_polyline(
        &[
            (a.x * SUBPIXEL, a.y * SUBPIXEL),
            (b.x * SUBPIXEL, b.y * SUBPIXEL),
        ],
        weight,
        hue.middle(),
    );
}

/// A line `logical` pixels wide at `scale`, in subpixels.
fn line_weight(scale: Scale, logical: u32) -> i32 {
    to_i32(scale.scale_length(logical)).saturating_mul(SUBPIXEL)
}

fn to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// `rect` grown by `by` on every side.
fn grown(rect: Rect, by: u32) -> Rect {
    let inset = to_i32(by);
    Rect::new(
        rect.left().saturating_sub(inset),
        rect.top().saturating_sub(inset),
        rect.width.saturating_add(by * 2),
        rect.height.saturating_add(by * 2),
    )
}

/// The canvas-coloured pill a spread service's name stands on.
fn fill_backing(surface: &mut Surface, backing: Rect, theme: &Theme) {
    if let Some((x, y)) = backing.surface_origin() {
        surface.fill_round_rect(
            x,
            y,
            backing.width,
            backing.height,
            backing.height / 2,
            Color::from(theme.palette().surface),
        );
    }
}

/// Send `fade` toward `to` from the strength it stands at, answering whether
/// that is somewhere new for it to go.
fn retarget(fade: &mut Fade, now_ns: u64, duration_ms: u16, to: u8) -> bool {
    if fade.target() == to {
        return false;
    }
    *fade = Fade::start(now_ns, duration_ms, fade.strength(now_ns), to);
    true
}

/// The direction away from this machine at graph point `pos`, in radians;
/// straight up for this machine itself.
fn outward(pos: Point) -> f64 {
    if pos == Point::ORIGIN {
        -FRAC_PI_2
    } else {
        mathf::atan2(f64::from(pos.y), f64::from(pos.x))
    }
}

/// Where each of `count` services stands around its host when fully spread,
/// as offsets from the host's centre in device pixels at `scale`: on arcs
/// centred on `outward`, the nearest [`SPREAD_FIRST_ARC`] out and each further
/// one [`SPREAD_ARC_STEP`] beyond, each holding as many as fit along it
/// [`SPREAD_PITCH`] apart.
fn spread_offsets(count: usize, outward: f64, scale: Scale, out: &mut Vec<(i32, i32)>) {
    out.clear();
    let pitch = scale.scale_length(SPREAD_PITCH).max(1);
    let step = scale.scale_length(SPREAD_ARC_STEP).max(1);
    let mut radius = scale.scale_length(SPREAD_FIRST_ARC).max(1);
    while out.len() < count {
        let carries =
            u64::from(SPREAD_ARC_MILLIRADIANS) * u64::from(radius) / (1000 * u64::from(pitch)) + 1;
        let here = usize::try_from(carries)
            .unwrap_or(usize::MAX)
            .min(count - out.len());
        let gap = f64::from(pitch) / f64::from(radius);
        let first = outward - gap * f64::from(u32::try_from(here - 1).unwrap_or(0)) / 2.0;
        for place in 0..here {
            let angle = first + gap * f64::from(u32::try_from(place).unwrap_or(0));
            out.push((
                mathf::round_i32(f64::from(radius) * mathf::cos(angle)),
                mathf::round_i32(f64::from(radius) * mathf::sin(angle)),
            ));
        }
        radius = radius.saturating_add(step);
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

/// One node's whole badge as a frame draws it.
struct Badge<'a> {
    node: &'a Node,
    centre: Point,
    radius: u32,
    look: Look,
    selected: bool,
    /// How many services the node offers, which its count states.
    services: usize,
}

impl Painter {
    fn occupancy_for(&mut self, pass: Pass) -> &mut Occupancy {
        match pass {
            Pass::Map => &mut self.occupancy,
            Pass::Spread => &mut self.overlay,
        }
    }

    /// A node's halo (accented when selected), its badge, the liveness light
    /// on one lower shoulder, its system's emblem on the other, and the count
    /// of the services it offers on its upper shoulder.
    fn node(
        &mut self,
        surface: &mut Surface,
        unit_circle: &[(f64, f64)],
        badge: &Badge<'_>,
        theme: &Theme,
        small: BitmapFont,
    ) {
        let palette = theme.palette();
        let (centre, radius) = (badge.centre, badge.radius);
        // A halo in the canvas colour parts the plate from the edges passing
        // under it; a selected node's is the accent, and wider.
        let (halo, halo_width) = if badge.selected {
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
        if let Some(plate) = self.plate(badge.look.glyph, badge.look.hue, radius * 2) {
            surface.blit(centre.x - r, centre.y - r, plate);
        }

        let shoulder = i32::try_from(radius * 7 / 10).unwrap_or(0);
        if let Some(emblem) = os_emblem(badge.node.classification.os) {
            let at = Point::new(centre.x - shoulder, centre.y + shoulder);
            let sticker = (radius / 2).max(2);
            self.disc(
                surface,
                unit_circle,
                at,
                sticker + 1,
                Color::from(palette.surface),
            );
            self.disc(surface, unit_circle, at, sticker, EMBLEM_PLATE);
            let side = sticker * 8 / 5;
            let inset = i32::try_from(side / 2).unwrap_or(0);
            if let Some(glyph) = self.glyph(emblem, side, EMBLEM_INK) {
                surface.blit(at.x - inset, at.y - inset, glyph);
            }
        }

        let light = match badge.node.liveness {
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

        if badge.services > 0 {
            let count = COUNTS.get(badge.services).copied().unwrap_or(COUNT_MORE);
            let at = Point::new(centre.x + shoulder, centre.y - shoulder);
            draw_count(surface, at, count, theme, small);
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
                .map(|&(c, s)| (cx + mathf::round_i32(r * c), cy + mathf::round_i32(r * s))),
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

/// A host's count of services: `text` on an accent pill centred at `at`,
/// rimmed in the canvas colour so it reads apart from the plate under it.
fn draw_count(surface: &mut Surface, at: Point, text: &str, theme: &Theme, font: BitmapFont) {
    let palette = theme.palette();
    let height = font.line_height();
    let width = (font.text_width(text) + height / 2).max(height);
    let pill = Rect::new(
        at.x - i32::try_from(width / 2).unwrap_or(0),
        at.y - i32::try_from(height / 2).unwrap_or(0),
        width,
        height,
    );
    let (Ok(left), Ok(top)) = (
        u32::try_from(pill.left().saturating_sub(1)),
        u32::try_from(pill.top().saturating_sub(1)),
    ) else {
        return;
    };
    surface.fill_round_rect(
        left,
        top,
        width + 2,
        height + 2,
        height / 2 + 1,
        Color::from(palette.surface),
    );
    surface.fill_round_rect(
        left + 1,
        top + 1,
        width,
        height,
        height / 2,
        Color::from(palette.accent),
    );
    let pen = pill.left() + i32::try_from((width - font.text_width(text)) / 2).unwrap_or(0);
    font.draw_text(
        surface,
        pen,
        pill.top(),
        text,
        Color::from(palette.on_accent),
    );
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
        Vec::with_capacity(8 + node.addrs.len() + node.facts.len() + node.evidence.len());
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
    if let Some(mac) = node.mac {
        let rendered = render_mac(mac);
        let shown = if assigned_oui(mac).is_some() {
            rendered
        } else {
            format!("{rendered} (locally administered)")
        };
        facts.push(Fact::new("Hardware address", shown));
    }
    if let Some(endpoint) = &node.endpoint {
        facts.push(Fact::new("Service type", endpoint.service_type.as_str()));
        facts.push(Fact::new("Port", format!("{}", endpoint.port)));
        facts.push(Fact::new("Host", endpoint.host.as_str()));
        if !endpoint.text.is_empty() {
            facts.push(Fact::new("TXT data", endpoint.text.join(" ")));
        }
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
        "vendor" => "Made by",
        "mtu" => "MTU",
        "kind" => "Link",
        "neighbor" => "Reachability",
        "dns name" => "DNS name",
        "local name" => "Local name",
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

/// How far a node's plate and its widest halo reach from its centre.
fn plate_reach(radius: u32) -> u32 {
    radius + radius / 5 + 1
}

/// The square a node's plate and halo cover.
fn plate_rect(centre: Point, radius: u32) -> Rect {
    let reach = plate_reach(radius);
    let half = to_i32(reach);
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

/// The detail panel in the canvas's top-right corner, answering the rectangle
/// it covers, or `None` when the canvas has no room for one.
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
) -> Option<Rect> {
    let palette = theme.palette();
    let margin = scale.scale_length(12);
    let width = scale
        .scale_length(PANEL_WIDTH)
        .min(canvas.width.saturating_sub(margin * 2));
    if width == 0 {
        return None;
    }
    let list = FactList::new(facts.to_vec());
    let title_height = font.line_height() + margin;
    let height = (title_height + list.measured_height(scale, theme) + margin)
        .min(canvas.height.saturating_sub(margin * 2));
    let x = canvas.right() - i32::try_from(width + margin).unwrap_or(0);
    let y = canvas.top() + i32::try_from(margin).unwrap_or(0);
    let (Ok(px), Ok(py)) = (u32::try_from(x), u32::try_from(y)) else {
        return None;
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
    Some(Rect::new(x, y, width, height))
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
