//! A Draw window: the document it edits, how the canvas sees it, the tool in
//! hand and what it is doing, the selection, and the inspector.
//!
//! What the canvas shows is the *shown* drawing: the document's, or, while an
//! interaction is under way — a drag, typing, a colour being chosen — the
//! drawing that interaction would make, which lands in the document as one
//! step when the interaction settles. Each shown drawing is a scene; the
//! tiles are rendered for the present scene off the event loop.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_bezier::{Affine, Bounds, Point};
use tairix_collections::try_arc;
use tairix_colour::Rgba;
use tairix_controls::{
    ControlRole, IconButton, ScrollBar, ScrollModel, ScrollOrientation, ScrollRange, Toolbar,
};
use tairix_drawing::render::Mode;
use tairix_drawing::unkept::{Lost, Unkept};
use tairix_drawing::{
    Drawing, DrawingError, NodePath, ObjectId, Paint, PathNode, SharedSlice, Style,
};
use tairix_geometry::{Region, Scale};
use tairix_icon::menu_symbol_icon;
use tairix_input::Modifiers;
use tairix_theme::Theme;
use tairix_util::mathf::fmin;
use tairix_window::document::Access;

use crate::document::Document;
use crate::edit::{self, EditError};
use crate::glyphs::{Arrived, Lettering};
use crate::inspector::{Inspector, Shown};
use crate::layout::Layout;
use crate::preferences::{Handling, Preferences};
use crate::tiles::{covering, RenderJob, TileKey, Tiles};
use crate::tools::Tool;
use crate::viewport::Viewport;

/// The menus a window opens.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MenuKind {
    /// The window menu.
    Window,
    /// The zoom readout's menu of zooms.
    Zoom,
}

/// What a window asks only the application for.
#[derive(Debug)]
pub enum Own {
    /// Put this drawing on the clipboard, written as SVG on the queue.
    Copy(Drawing),
    /// Read the clipboard's drawing in the sandbox and paste it.
    Paste,
    /// Reshape the selection on the queue.
    Compute(alloc::boxed::Box<crate::compute::Compute>),
    /// Open a new window.
    NewWindow,
}

/// The document as a save writes it.
#[derive(Debug)]
pub struct Snapshot {
    /// The drawing.
    pub drawing: Drawing,
}

/// A drag or other interaction the canvas is part way through.
#[derive(Clone, Debug)]
pub enum Gesture {
    /// Scrolling the canvas, the pointer last at `last` in device pixels.
    Pan {
        /// Where the pointer last was.
        last: Point,
    },
    /// Dragging out a rectangle of selection.
    Marquee {
        /// Where it began, in the drawing.
        from: Point,
        /// Where the pointer is.
        to: Point,
        /// Whether what it takes is added to the selection.
        adding: bool,
    },
    /// Moving the selection.
    Move {
        /// Where the drag began, in the drawing.
        from: Point,
        /// Where the pointer is.
        to: Point,
    },
    /// Scaling the selection by one of its handles.
    Scale {
        /// The handle held.
        handle: Handle,
        /// The selection's bounds when the drag began.
        bounds: Bounds,
        /// Where the pointer is.
        to: Point,
    },
    /// Turning the selection about its centre.
    Rotate {
        /// The centre it turns about.
        centre: Point,
        /// Where the drag began.
        from: Point,
        /// Where the pointer is.
        to: Point,
    },
    /// Laying a gradient across the selection.
    Gradient {
        /// Where the drag began, in the drawing: where it starts.
        from: Point,
        /// Where the pointer is: where it ends.
        to: Point,
    },
    /// Drawing out a new object with a creating tool.
    Create {
        /// Where the drag began, in the drawing.
        from: Point,
        /// Where the pointer is.
        to: Point,
    },
    /// Drawing freehand.
    Freehand {
        /// The points the pointer has passed through.
        points: Vec<Point>,
    },
    /// Dragging out the handle of the pen's newest node.
    PenHandle {
        /// Where the pointer is.
        to: Point,
    },
    /// Dragging a path's node or one of its handles.
    Node {
        /// Which node, and which part of it.
        grip: Grip,
        /// Where the pointer is.
        to: Point,
    },
    /// Dragging out a rectangle to zoom into.
    Zoom {
        /// Where it began, in canvas device pixels.
        from: Point,
        /// Where the pointer is.
        to: Point,
    },
    /// Dragging the caret of the text being typed across it, the selection
    /// held from where the press put it.
    TextSelect,
    /// Dragging a guide out of a ruler, or an existing one.
    Guide {
        /// Whether it runs across the page.
        horizontal: bool,
        /// Where it stands, in the drawing.
        at: f64,
        /// The guide it moves, if it was there already.
        index: Option<usize>,
    },
}

/// One of the eight handles a selection is scaled by.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Handle {
    /// Which side across it is on: -1 left, 0 middle, 1 right.
    pub across: i8,
    /// Which side down: -1 top, 0 middle, 1 bottom.
    pub down: i8,
}

/// A path node, or one of its handles, being dragged.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Grip {
    /// Which contour of the path.
    pub contour: usize,
    /// Which node of the contour.
    pub node: usize,
    /// Which part of it.
    pub part: GripPart,
}

/// Which part of a node a grip holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GripPart {
    /// The node itself.
    Point,
    /// Its incoming handle.
    Before,
    /// Its outgoing handle.
    After,
}

/// Text being typed into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Typing {
    /// The text object.
    pub path: NodePath,
    /// Its identity, so a drawing replaced beneath the typing is never typed
    /// into at whatever now stands at the path.
    pub id: ObjectId,
    /// Where the caret is, a byte offset into its content.
    pub caret: usize,
    /// Where the selection began, the caret its other end; the caret itself
    /// where nothing is selected.
    pub anchor: usize,
}

impl Typing {
    /// Typing into the text `id` at `path`, the caret at `caret` and nothing
    /// selected.
    #[must_use]
    pub const fn at(path: NodePath, id: ObjectId, caret: usize) -> Self {
        Self {
            path,
            id,
            caret,
            anchor: caret,
        }
    }

    /// The bytes selected, empty at the caret where nothing is.
    #[must_use]
    pub fn selection(&self) -> core::ops::Range<usize> {
        self.caret.min(self.anchor)..self.caret.max(self.anchor)
    }
}

/// What an interaction under way shows in place of the document's drawing,
/// and what the step it lands as is called.
#[derive(Clone, Debug)]
pub(crate) struct Live {
    pub(crate) drawing: Drawing,
    pub(crate) label: &'static str,
}

/// What typing lands as.
pub(crate) const TYPE_TEXT: &str = "Type text";

/// The most of the canvas a placed picture fills on either axis, so where it
/// lies can be seen.
const PLACED_SHARE: f64 = 0.8;

/// How the canvas shows the drawing and helps place things on it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Showing {
    /// Outlines rather than paint.
    pub outline: bool,
    /// The grid.
    pub grid: bool,
    /// Whether points snap to guides, the page and the grid.
    pub snapping: bool,
}

/// How new stars are drawn.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct StarDefaults {
    /// How many points.
    pub points: u32,
    /// The inner radius, of the outer.
    pub inner: f64,
    /// Whether a polygon rather than a star.
    pub polygon: bool,
}

/// How new text is set.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TextDefaults {
    /// The family, by its CSS name.
    pub family: &'static str,
    /// The size, in drawing pixels.
    pub size: f64,
    /// Whether bold.
    pub bold: bool,
    /// Whether italic.
    pub italic: bool,
    /// How lines align on the origin.
    pub align: tairix_drawing::TextAlign,
}

impl Default for TextDefaults {
    fn default() -> Self {
        Self {
            family: "sans-serif",
            size: 24.0,
            bold: false,
            italic: false,
            align: tairix_drawing::TextAlign::Start,
        }
    }
}

/// A Draw window's engine.
pub struct View {
    pub(crate) document: Document,
    pub(crate) name: String,
    pub(crate) access: Access,
    /// What the file held that the drawing could not; a document that lost
    /// anything is never written back over its own file.
    pub(crate) unkept: Unkept,
    pub(crate) message: Option<String>,
    pub(crate) viewport: Viewport,
    /// Whether the viewport has been fitted to the page yet.
    pub(crate) placed: bool,
    pub(crate) tiles: Tiles,
    /// The drawing an interaction under way would make, shown in place of
    /// the document's until it settles.
    pub(crate) live: Option<Live>,
    /// Counts every drawing shown.
    pub(crate) scene: u64,
    pub(crate) show: Showing,
    pub(crate) tool: Tool,
    /// How far the keys move things and how near the pointer must come.
    pub(crate) handling: Handling,
    pub(crate) tool_box: Toolbar,
    /// What each tool's button's tip says, in the tool box's order.
    pub(crate) tips: [String; Tool::ALL.len()],
    /// The canvas's bar across; its model is derived afresh each time.
    pub(crate) across: ScrollBar,
    /// The canvas's bar down.
    pub(crate) down: ScrollBar,
    /// When a held control next steps.
    pub(crate) repeat_due: Option<u64>,
    /// The objects selected, each its path from its layer.
    pub(crate) selection: Vec<NodePath>,
    /// The layer new objects go into.
    pub(crate) layer: usize,
    pub(crate) gesture: Option<Gesture>,
    /// Where the pointer last was, in window device pixels.
    pub(crate) pointer: Point,
    pub(crate) modifiers: Modifiers,
    /// Whether the space bar is held, which scrolls whatever the tool.
    pub(crate) spaced: bool,
    /// The nodes the pen has set.
    pub(crate) pen: Vec<PathNode>,
    pub(crate) typing: Option<Typing>,
    /// The style new objects are drawn in.
    pub(crate) style: Style,
    pub(crate) inspector: Inspector,
    /// The next job's number.
    pub(crate) jobs: u64,
    /// The reshaping asked of the queue and not yet answered.
    pub(crate) pending: Option<u64>,
    /// The zoom a pinch began at.
    pub(crate) pinched: Option<f64>,
    /// How round a new rectangle's corners are, in drawing pixels.
    pub(crate) corner_radius: f64,
    /// How far Outset and Inset move an outline, in drawing pixels.
    pub(crate) offset_distance: f64,
    /// How near Simplify keeps a path to itself, in drawing pixels.
    pub(crate) simplify_tolerance: f64,
    /// How new stars are drawn.
    pub(crate) star: StarDefaults,
    /// How new text is set.
    pub(crate) text: TextDefaults,
    /// The layout the view was last settled into.
    pub(crate) laid: Option<Layout>,
    /// The selection's bounds, the scene and selection they are of.
    framed: core::cell::RefCell<Option<(u64, Vec<NodePath>, edit::Framed)>>,
}

impl View {
    /// A window onto `document`, called `name`, written as `access` allows;
    /// `unkept` what its file held that it could not.
    #[must_use]
    pub fn new(
        document: Document,
        name: String,
        access: Access,
        unkept: Unkept,
        scale: f64,
    ) -> Self {
        let mut tool_box = Toolbar::new()
            .with_orientation(ScrollOrientation::Vertical)
            .with_lanes(1);
        for tool in Tool::ALL {
            tool_box = tool_box.with_icon(
                IconButton::new(menu_symbol_icon(tool.symbol()), ControlRole::Neutral),
                0,
            );
        }
        let start = Preferences::default();
        tool_box.set_active(start.tool.index());
        let idle = ScrollModel::in_pixels(ScrollRange::EMPTY, 1);
        let style = start.style();
        let access = if unkept.is_empty() || access != Access::Writable {
            access
        } else {
            Access::ReadOnly
        };
        let mut view = Self {
            document,
            name,
            access,
            unkept,
            message: None,
            viewport: Viewport::new(1.0, scale),
            placed: false,
            tiles: Tiles::default(),
            live: None,
            scene: 0,
            show: start.show,
            tool: start.tool,
            handling: start.handling(),
            tool_box,
            tips: Tool::ALL.map(Tool::tip),
            across: ScrollBar::new(ScrollOrientation::Horizontal, idle),
            down: ScrollBar::new(ScrollOrientation::Vertical, idle),
            repeat_due: None,
            selection: Vec::new(),
            layer: 0,
            gesture: None,
            pointer: Point::ZERO,
            modifiers: Modifiers::default(),
            spaced: false,
            pen: Vec::new(),
            typing: None,
            inspector: Inspector::new(&style),
            style,
            jobs: 0,
            pending: None,
            pinched: None,
            corner_radius: 0.0,
            offset_distance: crate::inspector::DEFAULT_OFFSET,
            simplify_tolerance: crate::inspector::DEFAULT_SMOOTHNESS,
            star: StarDefaults {
                points: 5,
                inner: 0.5,
                polygon: false,
            },
            text: TextDefaults::default(),
            laid: None,
            framed: core::cell::RefCell::new(None),
        };
        view.layer = view.document.drawing().layers().len().saturating_sub(1);
        if !view.unkept.is_empty() {
            view.message = Some(alloc::format!(
                "Opened read-only: the file holds {}, which a drawing cannot keep. Save a copy to keep the rest.",
                crate::listed(view.unkept.iter().map(Lost::describe))
            ));
        }
        view
    }

    /// Start as `preferences` say a new window starts: its tool, how it
    /// shows the drawing, and the style new objects are drawn in.
    pub fn begin(&mut self, preferences: &Preferences) {
        self.choose_tool(preferences.tool);
        self.show = preferences.show;
        self.style = preferences.style();
        self.handling = preferences.handling();
        self.refresh_inspector();
    }

    /// Take up how `preferences` say the keys and the pointer move things,
    /// as every open window does when they settle.
    pub fn adopt(&mut self, preferences: &Preferences) {
        self.handling = preferences.handling();
    }

    /// The document.
    #[must_use]
    pub const fn document(&self) -> &Document {
        &self.document
    }

    /// The drawing the canvas shows.
    #[must_use]
    pub fn shown(&self) -> &Drawing {
        self.live
            .as_ref()
            .map_or_else(|| self.document.drawing(), |live| &live.drawing)
    }

    /// Show `drawing` while an interaction is under way, to land as a step
    /// called `label` when it settles.
    pub(crate) fn show_live(&mut self, drawing: Drawing, label: &'static str) {
        let old = self.shown().clone();
        self.scene += 1;
        self.tiles.changed(&old, &drawing, self.scene);
        self.live = Some(Live { drawing, label });
    }

    /// The interaction ended without changing anything: show the document.
    pub(crate) fn drop_live(&mut self) {
        if let Some(live) = self.live.take() {
            self.scene += 1;
            let document = self.document.drawing().clone();
            self.tiles.changed(&live.drawing, &document, self.scene);
        }
    }

    /// The drawing a copy of the document writes, whatever was under way
    /// landed in it first.
    pub fn copied(&mut self, damage: &mut Region) -> Drawing {
        self.land_pending(damage);
        self.finish_pen();
        self.document.drawing().clone()
    }

    /// Land whatever is under way — what a field of the inspector holds,
    /// typing, a live change — so what follows acts on the drawing shown.
    pub(crate) fn land_pending(&mut self, damage: &mut Region) {
        let under_way =
            self.live.is_some() || self.typing.is_some() || self.inspector.has_keyboard();
        if !under_way {
            return;
        }
        if let Some(layout) = self.laid {
            if let Some(change) = self.inspector.settle_edits(&layout, damage) {
                self.apply_change(change, &layout, damage);
            }
            damage.add(layout.window);
        }
        self.finish_typing();
        self.land_live();
    }

    /// Make `drawing` the document's as one step called `label`.
    pub(crate) fn commit(&mut self, drawing: Drawing, label: &'static str) {
        let old = self.shown().clone();
        self.live = None;
        self.document.commit(drawing, label);
        self.scene += 1;
        let now = self.document.drawing().clone();
        self.tiles.changed(&old, &now, self.scene);
        self.keep_selection_valid();
    }

    /// Take the result of an edit, or say why it could not be made.
    pub(crate) fn apply(
        &mut self,
        result: Result<Drawing, EditError>,
        label: &'static str,
    ) -> bool {
        match result {
            Ok(drawing) => {
                self.commit(drawing, label);
                true
            }
            Err(error) => {
                self.say_error(label, error);
                false
            }
        }
    }

    pub(crate) fn say_error(&mut self, what: &str, error: EditError) {
        self.message = Some(alloc::format!("{what}: {error}"));
    }

    /// Take a step back, or forward again when `again`.
    pub(crate) fn undo(&mut self, again: bool, damage: &mut Region) {
        let under_way = self.live.is_some();
        let generation = self.document.generation();
        self.land_pending(damage);
        // What was under way and came to no step — text left empty — was
        // what Undo offered to take back, and it is gone.
        if under_way && !again && self.document.generation() == generation {
            return;
        }
        let label = if again {
            self.document.redo_label()
        } else {
            self.document.undo_label()
        };
        let Some(label) = label else {
            return;
        };
        let old = self.document.drawing().clone();
        let ids = self.selected_ids();
        if again {
            self.document.redo();
        } else {
            self.document.undo();
        }
        self.scene += 1;
        let now = self.document.drawing().clone();
        self.tiles.changed(&old, &now, self.scene);
        self.reselect(&ids);
        self.message = Some(alloc::format!(
            "{} {label}",
            if again { "Redid" } else { "Undid" }
        ));
    }

    /// The identity of each object selected, in the selection's order.
    fn selected_ids(&self) -> Vec<Option<ObjectId>> {
        let drawing = self.document.drawing();
        let mut ids = Vec::new();
        if ids.try_reserve(self.selection.len()).is_ok() {
            ids.extend(
                self.selection
                    .iter()
                    .map(|path| drawing.node_at(path).map(|node| node.id())),
            );
        }
        ids
    }

    /// The selection, once the drawing was replaced beneath it, made of the
    /// same objects as `ids`: each still at its path kept, each that moved
    /// found where it went, each gone dropped.
    fn reselect(&mut self, ids: &[Option<ObjectId>]) {
        if ids.len() != self.selection.len() {
            self.selection.clear();
        }
        let drawing = self.document.drawing();
        let mut lost = Vec::new();
        let mut wanted = Vec::new();
        let room = lost.try_reserve(ids.len()).is_ok() && wanted.try_reserve(ids.len()).is_ok();
        if !room {
            self.selection.clear();
        }
        for (slot, (path, id)) in self.selection.iter_mut().zip(ids).enumerate() {
            let here = drawing.node_at(path).map(|node| node.id());
            if here.is_some() && here == *id {
                continue;
            }
            path.clear();
            if let Some(id) = *id {
                lost.push(slot);
                wanted.push(id);
            }
        }
        if !wanted.is_empty() {
            if let Ok(found) = drawing.find_each(&wanted) {
                for (&slot, path) in lost.iter().zip(found) {
                    if let Some(path) = path {
                        self.selection[slot] = path;
                    }
                }
            }
        }
        self.keep_selection_valid();
    }

    /// Select `paths`, keeping only those that may be edited: what an edit
    /// placed may hold objects a document hid or locked.
    pub(crate) fn select(&mut self, paths: Vec<NodePath>) {
        self.selection = paths;
        self.keep_selection_valid();
    }

    /// Drop from the selection anything no longer in the drawing or no
    /// longer to be edited, hidden or locked, and keep the layer in hand one
    /// that exists.
    pub(crate) fn keep_selection_valid(&mut self) {
        let drawing = self
            .live
            .as_ref()
            .map_or_else(|| self.document.drawing(), |live| &live.drawing);
        let layers = drawing.layers().len();
        self.selection
            .retain(|path| path.len() > 1 && editable(drawing, path));
        let typed = self.typing.as_ref().is_some_and(|typing| {
            editable(drawing, &typing.path)
                && drawing
                    .node_at(&typing.path)
                    .is_some_and(|node| node.id() == typing.id)
        });
        if !typed {
            self.typing = None;
        }
        if self.layer >= layers {
            self.layer = layers.saturating_sub(1);
        }
    }

    /// The selection's bounds in the drawing's space, worked out once a scene.
    #[must_use]
    pub fn selection_bounds(&self) -> Bounds {
        let mut memo = self.framed.borrow_mut();
        if let Some((scene, selection, framed)) = &*memo {
            if *scene == self.scene && *selection == self.selection {
                return framed.bounds;
            }
        }
        let framed = edit::framed(self.shown(), &self.selection, &Lettering::new(Arrived));
        *memo = copied_paths(&self.selection).map(|selection| (self.scene, selection, framed));
        framed.bounds
    }

    /// How the tiles in view are drawn.
    #[must_use]
    pub fn mode(&self) -> Mode {
        if self.show.outline {
            Mode::Outline(Rgba::new(20, 20, 24, 255))
        } else {
            Mode::Painted
        }
    }

    /// The tiles the canvas of `layout` shows now.
    #[must_use]
    pub fn visible_tiles(&self, layout: &Layout) -> Vec<TileKey> {
        let canvas = layout.canvas;
        let area = Bounds::new(
            Point::ZERO,
            Point::new(f64::from(canvas.width), f64::from(canvas.height)),
        );
        covering(
            area,
            self.viewport.origin(),
            self.viewport.pixels(),
            self.show.outline,
        )
    }

    /// The render job the canvas wants next, if any tile in view wants one.
    pub fn wanted_render(&mut self, layout: &Layout) -> Option<RenderJob> {
        let visible = self.visible_tiles(layout);
        let keys = self.tiles.wanted(&visible);
        if keys.is_empty() {
            return None;
        }
        Some(RenderJob {
            scene: self.tiles.scene(),
            drawing: self.shown().clone(),
            keys,
            mode: self.mode(),
        })
    }

    /// A render job asked for `keys` landed with `tiles`, those it drew; the
    /// rest could not be drawn. The glyphs they were drawn with have arrived
    /// with them, so text is framed afresh.
    pub fn rendered(
        &mut self,
        scene: u64,
        (keys, tiles): (
            &[TileKey],
            impl IntoIterator<Item = (TileKey, tairix_raster::Surface)>,
        ),
        layout: &Layout,
        damage: &mut Region,
    ) {
        let visible = self.visible_tiles(layout);
        for (key, surface) in tiles {
            self.tiles.landed(scene, key, surface, &visible);
        }
        self.tiles.refused(keys);
        damage.add(layout.canvas);
        // Glyphs that arrived with the tiles may frame text afresh; bounds
        // that waited on none stand.
        let waiting = self
            .framed
            .get_mut()
            .as_ref()
            .is_none_or(|(_, _, framed)| !framed.complete);
        if waiting {
            *self.framed.get_mut() = None;
            if !self.selection.is_empty() {
                self.refresh_inspector();
                damage.add(layout.panel);
            }
        }
    }

    /// The renderer could not take the job asked for `keys`: they are asked
    /// for again once it can.
    pub fn render_refused(&mut self, keys: &[TileKey]) {
        self.tiles.not_taken(keys);
    }

    /// The drawing a copy of the selection puts on the clipboard: its objects
    /// alone, on the drawing's page, in one layer.
    ///
    /// # Errors
    ///
    /// Why the copy could not be made, a refused allocation among them.
    pub(crate) fn selection_drawing(&mut self) -> Result<Option<Drawing>, DrawingError> {
        let drawing = self.document.drawing();
        let mut nodes = Vec::new();
        if !tairix_util::fallible::reserve(&mut nodes, self.selection.len()) {
            return Err(DrawingError::OutOfMemory);
        }
        for path in &self.selection {
            let Some(node) = drawing.node_at(path) else {
                return Ok(None);
            };
            let placed =
                node.with_transform(node.transform().then(edit::parent_space(drawing, path)))?;
            nodes.push(try_arc(placed)?);
        }
        let layer = tairix_drawing::Node::new(
            self.document.ids().issue(),
            tairix_drawing::Kind::Group {
                children: tairix_drawing::Children::from_nodes(nodes)?,
                clip: None,
            },
        )?;
        let drawing = self.document.drawing();
        Ok(Some(Drawing::new(
            *drawing.page(),
            tairix_drawing::Children::single(try_arc(layer)?)?,
            SharedSlice::new(),
            drawing.grid(),
        )?))
    }

    /// The clipboard's drawing, read in the sandbox, arrived: its objects
    /// join the layer in hand, selected.
    pub fn pasted(
        &mut self,
        pasted: Result<Drawing, String>,
        damage: &mut Region,
        layout: &Layout,
    ) {
        damage.add(layout.status);
        let pasted = match pasted {
            Ok(pasted) => pasted,
            Err(why) => {
                self.message = Some(why);
                return;
            }
        };
        self.land_pending(damage);
        if !self.takes_new_objects() {
            return;
        }
        let parent = [self.layer];
        match self.placed_paste(&pasted, &parent) {
            Ok(nodes) if nodes.is_empty() => {
                self.message = Some(String::from(
                    "The clipboard's drawing holds nothing to paste",
                ));
            }
            Ok(nodes) => match edit::append(self.document.drawing(), &parent, nodes) {
                Ok((drawing, placed)) => {
                    self.commit(drawing, "Paste");
                    self.select(placed);
                    self.message = None;
                    self.refresh_inspector();
                }
                Err(error) => self.say_error("Paste", error),
            },
            Err(error) => self.say_error("Paste", error),
        }
        damage.add(layout.window);
    }

    /// A picture the user chose to place, called `name`, landed in the layer
    /// in hand: in the middle of what the canvas shows, at its own size or,
    /// where that would not fit, as large as most of it holds.
    pub fn placed_picture(
        &mut self,
        name: &str,
        placed: Result<tairix_drawing::Image, String>,
        damage: &mut Region,
        layout: &Layout,
    ) {
        damage.add(layout.status);
        let image = match placed {
            Ok(image) => image,
            Err(why) => {
                self.message = Some(alloc::format!("{name} could not be placed: {why}"));
                return;
            }
        };
        self.land_pending(damage);
        if !self.takes_new_objects() {
            return;
        }
        let canvas = Point::new(
            f64::from(layout.canvas.width),
            f64::from(layout.canvas.height),
        );
        let shown = Bounds::from_points([
            self.viewport.drawing_point(Point::ZERO),
            self.viewport.drawing_point(canvas),
        ]);
        let place = image.place();
        let size = Point::new(place.width(), place.height());
        let fit = fmin(
            1.0,
            fmin(
                PLACED_SHARE * shown.width() / size.x,
                PLACED_SHARE * shown.height() / size.y,
            ),
        );
        let corner = shown.centre() - size * (fit / 2.0);
        let on_page = Affine::scale(fit, fit).then(Affine::translate(corner.x, corner.y));
        let mut nodes = Vec::new();
        let made = self.picture_node(image, on_page, name).and_then(|node| {
            tairix_util::fallible::try_push(&mut nodes, node)
                .map_err(|_| EditError::from(tairix_drawing::DrawingError::OutOfMemory))
        });
        match made {
            Ok(()) => match edit::append(self.document.drawing(), &[self.layer], nodes) {
                Ok((drawing, placed)) => {
                    self.commit(drawing, "Place picture");
                    self.select(placed);
                    self.message = None;
                    self.refresh_inspector();
                }
                Err(error) => self.say_error("Place picture", error),
            },
            Err(error) => self.say_error("Place picture", error),
        }
        damage.add(layout.window);
    }

    /// `image` as an object of the layer in hand, laid by `on_page` in the
    /// drawing's space and named after the file it came from.
    fn picture_node(
        &mut self,
        image: tairix_drawing::Image,
        on_page: Affine,
        name: &str,
    ) -> Result<Arc<tairix_drawing::Node>, EditError> {
        let into = self
            .document
            .drawing()
            .node_at(&[self.layer])
            .and_then(|layer| layer.transform().invert())
            .ok_or(EditError::Nothing)?;
        let node = tairix_drawing::Node::new(
            self.document.ids().issue(),
            tairix_drawing::Kind::Image {
                image: try_arc(image)?,
            },
        )?
        .with_transform(on_page.then(into))?;
        let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
        let named = match node.with_name(Some(tairix_collections::SharedStr::try_from_str(stem)?)) {
            Ok(named) => named,
            Err(tairix_drawing::DrawingError::OutOfMemory) => {
                return Err(tairix_drawing::DrawingError::OutOfMemory.into());
            }
            // A name no object may have leaves the picture unnamed.
            Err(_) => node,
        };
        Ok(try_arc(named)?)
    }

    /// The objects of `pasted`, each given identities of its own and placed
    /// in the group at `parent` where it stood on its own page.
    fn placed_paste(
        &mut self,
        pasted: &Drawing,
        parent: &[usize],
    ) -> Result<Vec<Arc<tairix_drawing::Node>>, EditError> {
        // What was copied keeps its place on the page, whatever either
        // layer's own transform.
        let into = self
            .document
            .drawing()
            .node_at(parent)
            .and_then(|layer| layer.transform().invert())
            .ok_or(EditError::Nothing)?;
        let mut nodes = Vec::new();
        for layer in pasted.layers().iter() {
            let Some(children) = layer.children() else {
                continue;
            };
            if nodes.try_reserve(children.len()).is_err() {
                return Err(tairix_drawing::DrawingError::OutOfMemory.into());
            }
            for child in children.iter() {
                let placing = child.transform().then(layer.transform()).then(into);
                let moved = child.with_transform(placing)?;
                nodes.push(try_arc(edit::renumbered(&moved, self.document.ids())?)?);
            }
        }
        Ok(nodes)
    }

    /// Whether the layer in hand takes new objects, saying why when it does
    /// not.
    pub(crate) fn takes_new_objects(&mut self) -> bool {
        let layer = self.document.drawing().node_at(&[self.layer]);
        let refusal = match layer {
            None => Some("There is no layer to draw into"),
            Some(layer) if layer.locked() => Some("The layer in hand is locked"),
            Some(layer) if !layer.visible() => Some("The layer in hand is hidden"),
            Some(_) => None,
        };
        if let Some(refusal) = refusal {
            self.message = Some(String::from(refusal));
        }
        refusal.is_none()
    }

    /// The tool in hand became `tool`.
    pub(crate) fn choose_tool(&mut self, tool: Tool) {
        self.finish_pen();
        self.finish_typing();
        self.tool = tool;
        self.tool_box.set_active(tool.index());
        self.gesture = None;
    }

    /// End the pen's path, keeping it when it has two nodes or more.
    pub(crate) fn finish_pen(&mut self) {
        let nodes = core::mem::take(&mut self.pen);
        match crate::tools::pen_path(&nodes, false) {
            Ok(Some(shape)) => {
                let mut style = self.style.clone();
                style.fill = Paint::None;
                self.add_shape(shape, &style, "Draw path");
            }
            Ok(None) => {}
            Err(error) => self.say_error("Draw path", EditError::Drawing(error)),
        }
    }

    /// End typing, landing what was typed as one step; text left with
    /// nothing typed is no object at all.
    pub(crate) fn finish_typing(&mut self) {
        let Some(typing) = self.typing.take() else {
            return;
        };
        let empty = self.shown().node_at(&typing.path).is_some_and(|node| {
            matches!(node.kind(), tairix_drawing::Kind::Text { text, .. } if text.content.is_empty())
        });
        let existed = self
            .document
            .drawing()
            .node_at(&typing.path)
            .is_some_and(|node| node.id() == typing.id);
        if !empty {
            if !existed {
                self.fit_new_text(&typing.path);
            }
            self.land_live();
            return;
        }
        self.selection.retain(|path| *path != typing.path);
        if existed {
            let result = edit::remove(self.document.drawing(), core::slice::from_ref(&typing.path));
            self.apply(result, "Delete text");
        } else {
            self.drop_live();
        }
    }

    /// The text just typed at `path`, its gradients fitted to what it now
    /// holds: until it was typed it had no box to fit them to.
    fn fit_new_text(&mut self, path: &[usize]) {
        let drawing = self.shown();
        let Some(node) = drawing.node_at(path) else {
            return;
        };
        let tairix_drawing::Kind::Text { text, style } = node.kind() else {
            return;
        };
        let graded = |paint: &Paint| matches!(paint, Paint::Gradient(_));
        if !graded(&style.fill) && !graded(&style.stroke.paint) {
            return;
        }
        let bounds = edit::own_bounds(node, &Lettering::new(Arrived));
        let refitted = crate::gradient::fitted_style(style, bounds).and_then(|fitted| {
            let fitted = node.with_kind(tairix_drawing::Kind::Text {
                text: text.clone(),
                style: try_arc(fitted)?,
            })?;
            drawing.with_node_at(path, try_arc(fitted)?)
        });
        match refitted {
            Ok(fitted) if !fitted.is(drawing) => self.show_live(fitted, TYPE_TEXT),
            Ok(_) => {}
            Err(error) => self.say_error(TYPE_TEXT, EditError::Drawing(error)),
        }
    }

    /// Land what the interaction under way shows as one step.
    pub(crate) fn land_live(&mut self) {
        // Committed while still shown, so the tiles see nothing change.
        if let Some(live) = self.live.clone() {
            self.commit(live.drawing, live.label);
        }
    }

    /// `shape`, styled `style` with its gradients fitted to it, added to the
    /// layer in hand and selected.
    pub(crate) fn add_shape(
        &mut self,
        shape: tairix_drawing::Shape,
        style: &Style,
        label: &'static str,
    ) {
        let fitted = shape
            .bounds()
            .and_then(|bounds| crate::gradient::fitted_style(style, bounds));
        let style = match fitted {
            Ok(style) => style,
            Err(error) => return self.say_error(label, EditError::Drawing(error)),
        };
        let id = self.document.ids().issue();
        let node = try_arc(style)
            .map_err(DrawingError::from)
            .and_then(|style| {
                tairix_drawing::Node::new(id, tairix_drawing::Kind::Shape { shape, style })
            });
        match node {
            Ok(node) => self.add_node(node, label),
            Err(error) => self.say_error(label, EditError::Drawing(error)),
        }
    }

    /// `node` added to the layer in hand and selected.
    pub(crate) fn add_node(&mut self, node: tairix_drawing::Node, label: &'static str) {
        if !self.takes_new_objects() {
            return;
        }
        let added = try_arc(node)
            .map_err(EditError::from)
            .and_then(|node| edit::insert(self.document.drawing(), &[self.layer], node));
        match added {
            Ok((drawing, path)) => {
                self.commit(drawing, label);
                self.select(alloc::vec![path]);
                self.refresh_inspector();
            }
            Err(error) => self.say_error(label, error),
        }
    }

    /// Hold the history, and the tiles beyond those in view, to what memory
    /// pressure `band` allows.
    pub fn adopt_pressure(&mut self, band: tairix_reclaim::PressureBand) {
        self.document.adopt_pressure(band);
        let visible = self
            .laid
            .map(|layout| self.visible_tiles(&layout))
            .unwrap_or_default();
        self.tiles.adopt_pressure(band, &visible);
    }

    /// Arm the deadlines the held controls call for, now it is `now_ns`.
    pub fn arm_deadline(&mut self, now_ns: u64, layout: &Layout, scale: Scale, theme: &Theme) {
        let tools = self
            .tool_box
            .repeat_deadline(now_ns, layout.tools, scale, theme);
        let panel = self.inspector.repeat_deadline(now_ns);
        let bars = self.bars_deadline(now_ns);
        self.repeat_due = [tools, panel, bars].into_iter().flatten().min();
    }

    /// When the window next needs waking, if it does.
    #[must_use]
    pub const fn deadline_ns(&self) -> Option<u64> {
        self.repeat_due
    }

    /// Step whichever held control is due at `now_ns`.
    pub fn tick(
        &mut self,
        now_ns: u64,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        self.tool_box
            .repeat(now_ns, layout.tools, scale, theme, damage);
        self.inspect(|inspector, shown| {
            inspector.repeat(now_ns, (layout, scale, theme), shown, damage);
        });
        self.bars_repeat(now_ns, (layout, scale, theme), damage);
    }

    /// Hand the inspector, with what it shows of the window, to `using`.
    pub(crate) fn inspect<R>(&mut self, using: impl FnOnce(&mut Inspector, &Shown<'_>) -> R) -> R {
        let drawing = self
            .live
            .as_ref()
            .map_or_else(|| self.document.drawing(), |live| &live.drawing);
        let defaults = (&self.style, self.star.polygon, &self.text);
        let typed = self.typed();
        let shown = crate::paint::shown_of(drawing, (&self.selection, typed), defaults, self.layer);
        using(&mut self.inspector, &shown)
    }

    /// Bring the inspector into line with the selection.
    pub(crate) fn refresh_inspector(&mut self) {
        let bounds = self.selection_bounds();
        let drawing = self
            .live
            .as_ref()
            .map_or_else(|| self.document.drawing(), |live| &live.drawing);
        let typed = self.typed();
        self.inspector
            .follow(drawing, &self.selection, &self.style, (bounds, typed));
    }

    /// The bytes selected in the text being typed, if typing is under way.
    pub(crate) fn typed(&self) -> Option<core::ops::Range<usize>> {
        self.typing.as_ref().map(Typing::selection)
    }

    /// The next job's number.
    pub(crate) fn next_job(&mut self) -> u64 {
        self.jobs += 1;
        self.jobs
    }
}

/// A copy of `paths`, or `None` when memory refuses it.
pub(crate) fn copied_paths(paths: &[NodePath]) -> Option<Vec<NodePath>> {
    let mut copy = Vec::new();
    copy.try_reserve(paths.len()).ok()?;
    for path in paths {
        copy.push(tairix_util::fallible::copied(path)?);
    }
    Some(copy)
}

/// Whether the object at `path` may be edited: it, and every group holding
/// it, is in the drawing, shown and unlocked.
#[must_use]
pub(crate) fn editable(drawing: &Drawing, path: &[usize]) -> bool {
    (1..=path.len()).all(|depth| {
        drawing
            .node_at(&path[..depth])
            .is_some_and(|node| node.visible() && !node.locked())
    })
}
