//! The composed painter window: the document, how it is shown, the chrome
//! around it, and the one input entry point.
//!
//! Nothing here reads a file or asks a service. Input draws on the picture and
//! records the damage it did; what needs a worker or the desktop is answered
//! as a [`Request`] for `Run` to carry out. Work that grows with the picture
//! rather than with the brush — a fill, a transform — goes to a worker, and
//! the picture takes no edits until it is back.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::time::Duration64;
use tairix_abi::window_ipc::{AppMenu, AppMenuItemId, CursorShape, SaveEndings};
use tairix_browse::vfs::write_document_title;
use tairix_controls::{
    FieldGroup, Keystroke, ScrollAction, ScrollBar, ScrollModel, ScrollOrientation, ScrollRange,
    SwatchGrid, Toolbar, REPEAT_DELAY_NS, REPEAT_INTERVAL_NS,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_image::{desktop_palette, IndexDepth};
use tairix_input::{ClickRun, InputEvent, Modifiers, PointerButton};
use tairix_raster::Color;
use tairix_reclaim::PressureBand;
use tairix_rng::NonCryptoRng;
use tairix_theme::Theme;
use tairix_window::docapp::DocumentView;
use tairix_window::document::{Access, SavedDocument};

use crate::canvas::{Canvas, CanvasError, Kind, OutOfMemory, Tile};
use crate::colour::{Ink, WHITE};
use crate::dialog::Form;
use crate::document::{Document, NewPicture, Picture, Snapshot};
use crate::fill;
use crate::layout::{Faces, Layout, PanelNeeds};
use crate::save::{format_for, natural, save_endings, SaveFormat, SaveRefusal};
use crate::selection::{cut_out, Floating};
use crate::shape::{Bounds, Point as Fx, Shape, Span, FX};
use crate::stroke::{Blend, Layer, Stroke};
use crate::tool::{strip, strip_tip, tool_index, Options, Style, Tool};
use crate::transform::{Transform, TransformError};
use crate::viewport::{Viewport, GRID_FROM, ZOOMS};

/// The application's name, as window titles end.
pub const APP_TITLE: &str = "Paint";

/// How often a held spray lays more paint.
pub const SPRAY_INTERVAL_NS: u64 = 25_000_000;

/// A new sprite's size to start.
const SPRITE_SIZE: (u32, u32) = (32, 32);

/// The Wimp's sixteen colours, as a colour picture's wells offer them, and
/// nothing after.
fn colour_wells() -> Vec<Ink> {
    desktop_palette(IndexDepth::Four)
        .iter()
        .map(|&[r, g, b]| Ink::Colour([r, g, b, 255]))
        .chain(core::iter::once(Ink::Clear))
        .collect()
}

/// A menu the window asks the desktop to open.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MenuKind {
    /// The window's menu, opened by a secondary press anywhere in it.
    Window,
    /// The magnifications, from the status band.
    Zoom,
}

/// Work a worker does for the window.
#[derive(Debug)]
pub enum Compute {
    /// Make a new picture of `canvas`.
    Transform {
        /// The picture, as it stood.
        canvas: Canvas,
        /// What to make of it.
        transform: Transform,
    },
    /// Fill the region of `canvas` joined to `at`.
    Fill {
        /// The picture, as it stood.
        canvas: Canvas,
        /// The pixel filled from.
        at: (u32, u32),
        /// How far a colour may differ and still be filled.
        tolerance: u8,
        /// What is put down.
        layer: Layer,
    },
    /// Put `floating` down on `canvas`.
    PutDown {
        /// The picture, as it stood.
        canvas: Canvas,
        /// The layer floating over it.
        floating: Floating,
    },
    /// Clear `area` of `canvas` to `ink`, as an eraser would.
    Clear {
        /// The picture, as it stood.
        canvas: Canvas,
        /// What is cleared.
        area: Bounds,
        /// What is left there.
        ink: Ink,
    },
}

/// What a worker answers.
#[derive(Debug)]
pub enum Computed {
    /// A transform's picture.
    Picture(Result<Canvas, TransformError>),
    /// A fill's tiles, each the tile as it now stands.
    Tiles(Result<Vec<(usize, Arc<Tile>)>, OutOfMemory>),
}

/// Carry out `work`: what the worker runs.
#[must_use]
pub fn compute(work: Compute) -> Computed {
    match work {
        Compute::Transform { canvas, transform } => {
            Computed::Picture(crate::transform::apply(&canvas, transform))
        }
        Compute::Fill {
            mut canvas,
            at,
            tolerance,
            layer,
        } => Computed::Tiles(filled(&mut canvas, at, tolerance, layer)),
        Compute::PutDown {
            mut canvas,
            floating,
        } => Computed::Tiles(floating.put_down(&mut canvas)),
        Compute::Clear {
            mut canvas,
            area,
            ink,
        } => Computed::Tiles(crate::selection::cleared(&mut canvas, area, ink)),
    }
}

/// What a copy takes, as it stood: the pixels are cut out on the queue's
/// worker, so a copy of a whole picture costs the loop nothing.
#[derive(Debug)]
pub enum Clip {
    /// A floating layer.
    Floating(Floating),
    /// The part `area` of a picture.
    Area {
        /// The picture.
        canvas: Canvas,
        /// The part copied.
        area: Bounds,
    },
}

impl Clip {
    /// The pixels copied, alone.
    ///
    /// # Errors
    ///
    /// [`CanvasError`] where they cannot be held.
    pub fn pixels(&self) -> Result<Canvas, CanvasError> {
        match self {
            Self::Floating(floating) => floating.pixels(),
            Self::Area { canvas, area } => cut_out(canvas, *area),
        }
    }
}

fn filled(
    canvas: &mut Canvas,
    at: (u32, u32),
    tolerance: u8,
    layer: Layer,
) -> Result<Vec<(usize, Arc<Tile>)>, OutOfMemory> {
    let Some(region) = fill::region(canvas, at.0, at.1, tolerance)? else {
        return Ok(Vec::new());
    };
    let stroke = fill::fill(canvas, &region, layer)?;
    let written = stroke.finish();
    let mut tiles = Vec::new();
    tiles
        .try_reserve_exact(written.len())
        .map_err(|_| OutOfMemory)?;
    tiles.extend(
        written
            .into_iter()
            .map(|(index, _)| (index, Arc::clone(canvas.tile(index)))),
    );
    Ok(tiles)
}

/// What the window asks of `Run` that only the painter carries out.
#[derive(Debug)]
pub enum Own {
    /// Open a window on a new picture.
    NewWindow(NewPicture),
    /// Put these pixels on the clipboard.
    Copy(Clip),
    /// Put these pixels on the clipboard, and have a worker clear them,
    /// answering job `job`.
    Cut {
        /// What is copied.
        clip: Clip,
        /// Which job clears it.
        job: u64,
        /// The clearing.
        work: Compute,
    },
    /// Paste what the clipboard holds, as a layer over a picture of this
    /// kind.
    Paste(Kind),
    /// Have a worker do `work`, answering job `job`.
    Compute {
        /// Which job.
        job: u64,
        /// What.
        work: Compute,
    },
}

/// What the window asks of `Run`.
pub type Request = tairix_window::docapp::Request<MenuKind, Own>;

/// What an input event led to beyond the damage it recorded.
pub type Outcome = tairix_window::docapp::Outcome<Request>;

/// What a menu row or a key asks for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Action {
    /// Begin a new picture.
    NewPicture,
    /// Open a document.
    Open,
    /// Save.
    Save,
    /// Save somewhere new.
    SaveAs,
    /// Set the JPEG quality.
    Quality,
    /// Close the window.
    Close,
    /// Undo.
    Undo,
    /// Redo.
    Redo,
    /// Cut the selection.
    Cut,
    /// Copy the selection.
    Copy,
    /// Paste.
    Paste,
    /// Select the whole picture.
    SelectAll,
    /// Put the selection down and forget it.
    Deselect,
    /// Clear what is selected.
    Delete,
    /// Keep only what is selected.
    Crop,
    /// Stretch or shrink the picture.
    Resize,
    /// Change the canvas's size.
    CanvasSize,
    /// A quarter turn anticlockwise.
    RotateLeft,
    /// A quarter turn clockwise.
    RotateRight,
    /// A half turn.
    RotateHalf,
    /// Mirror left to right.
    FlipAcross,
    /// Mirror top to bottom.
    FlipDown,
    /// Every colour its opposite.
    Invert,
    /// Store the picture at another depth.
    Convert,
    /// Give a palette picture a mask.
    AddMask,
    /// Take a palette picture's mask away.
    RemoveMask,
    /// Edit the primary colour.
    EditPrimary,
    /// Edit the secondary colour.
    EditSecondary,
    /// Swap the primary and secondary colours.
    SwapColours,
    /// Show the sprite before.
    PreviousSprite,
    /// Show the sprite after.
    NextSprite,
    /// Add a sprite.
    NewSprite,
    /// Add a copy of the sprite showing.
    DuplicateSprite,
    /// Remove the sprite showing.
    DeleteSprite,
    /// Move the sprite showing up the list.
    SpriteUp,
    /// Move it down.
    SpriteDown,
    /// Magnify more.
    ZoomIn,
    /// Magnify less.
    ZoomOut,
    /// Fit the picture in the window.
    Fit,
    /// A picture pixel to a screen pixel.
    Actual,
    /// Show or hide the grid between pixels.
    Grid,
    /// Put a floating selection down.
    PutDown,
    /// Show the sprite of the name typed.
    GoTo,
    /// Rename the sprite showing.
    Rename,
    /// Choose a tool.
    Tool(Tool),
    /// Magnify to a rung of the ladder.
    Zoom(usize),
}

impl Action {
    /// Whether it acts with a floating layer still floating: copying,
    /// clearing or pasting over it, the view and the inks; every other
    /// action puts the layer down first.
    #[must_use]
    pub const fn leaves_floating(self) -> bool {
        matches!(
            self,
            Self::NewPicture
                | Self::Open
                | Self::Quality
                | Self::Close
                | Self::Cut
                | Self::Copy
                | Self::Paste
                | Self::Delete
                | Self::DeleteSprite
                | Self::EditPrimary
                | Self::EditSecondary
                | Self::SwapColours
                | Self::ZoomIn
                | Self::ZoomOut
                | Self::Fit
                | Self::Actual
                | Self::Grid
                | Self::Rename
                | Self::Zoom(_)
                | Self::Tool(Tool::Select)
        )
    }
}

/// The actions with no argument, by id: an action's position here is its
/// menu id, less one.
const PLAIN_ACTIONS: [Action; 43] = [
    Action::NewPicture,
    Action::Open,
    Action::Save,
    Action::SaveAs,
    Action::Quality,
    Action::Close,
    Action::Undo,
    Action::Redo,
    Action::Cut,
    Action::Copy,
    Action::Paste,
    Action::SelectAll,
    Action::Deselect,
    Action::Delete,
    Action::Crop,
    Action::Resize,
    Action::CanvasSize,
    Action::RotateLeft,
    Action::RotateRight,
    Action::RotateHalf,
    Action::FlipAcross,
    Action::FlipDown,
    Action::Invert,
    Action::Convert,
    Action::AddMask,
    Action::RemoveMask,
    Action::EditPrimary,
    Action::EditSecondary,
    Action::SwapColours,
    Action::PreviousSprite,
    Action::NextSprite,
    Action::NewSprite,
    Action::DuplicateSprite,
    Action::DeleteSprite,
    Action::SpriteUp,
    Action::SpriteDown,
    Action::ZoomIn,
    Action::ZoomOut,
    Action::Fit,
    Action::Actual,
    Action::Grid,
    Action::PutDown,
    Action::GoTo,
];

/// Where the argument-carrying families' ids start, and the one entry
/// field's own id.
const TOOL_IDS: u16 = 100;
const ZOOM_IDS: u16 = 200;
const RENAME_ID: u16 = 300;
const GO_TO_ENTRY: u16 = 301;
const RENAME_ENTRY: u16 = 302;

impl Action {
    /// The menu id this action is chosen by.
    #[must_use]
    pub fn id(self) -> u16 {
        let at =
            |base: u16, index: Option<usize>| base + u16::try_from(index.unwrap_or(0)).unwrap_or(0);
        match self {
            Self::Tool(tool) => at(TOOL_IDS, Some(tool_index(tool))),
            Self::Zoom(rung) => at(ZOOM_IDS, Some(rung)),
            Self::Rename => RENAME_ID,
            plain => at(1, PLAIN_ACTIONS.iter().position(|&a| a == plain)),
        }
    }

    /// The action a menu id names, if any.
    #[must_use]
    pub fn from_id(id: u16) -> Option<Self> {
        match id {
            0 => None,
            1..TOOL_IDS => PLAIN_ACTIONS.get(usize::from(id - 1)).copied(),
            TOOL_IDS..ZOOM_IDS => Tool::ALL
                .get(usize::from(id - TOOL_IDS))
                .map(|&t| Self::Tool(t)),
            ZOOM_IDS..RENAME_ID => {
                let rung = usize::from(id - ZOOM_IDS);
                (rung < ZOOMS.len()).then_some(Self::Zoom(rung))
            }
            RENAME_ID => Some(Self::Rename),
            _ => None,
        }
    }
}

impl From<Action> for u16 {
    fn from(action: Action) -> Self {
        action.id()
    }
}

/// A shape being dragged, as it will be laid down: each layer and the shape
/// it covers, fill first.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Preview {
    /// The layers.
    pub layers: [Option<(Layer, Shape)>; 2],
    /// Whether edges are smoothed.
    pub smooth: bool,
}

/// A drag in progress on the canvas.
#[derive(Debug)]
enum Gesture {
    /// Paint laid down as the pointer moves.
    Stroke {
        stroke: Stroke,
        /// Where the pointer was last, in picture units.
        last: Fx,
    },
    /// A shape following the pointer, put down when it lets go.
    Shape { from: Fx, to: Fx, secondary: bool },
    /// A selection being marked out, from one pixel to another.
    Marquee { from: (i64, i64) },
    /// A floating layer being dragged from pixel `from`, the pointer last
    /// over pixel `last`.
    Move { from: (i64, i64), last: (i64, i64) },
}

/// What becomes of the selection once a worker's answer lands.
#[derive(Debug)]
enum Settles {
    /// Nothing of it: a fill or a transform.
    Nothing,
    /// The floating layer is down, what it covers left selected, and `Then`
    /// follows.
    PutDown(Then),
    /// What floated, or was selected, is cleared away.
    Cleared,
}

/// What follows a floating layer's putting down once it has landed.
#[derive(Debug)]
enum Then {
    /// Nothing more.
    Rest,
    /// The action that needed it down.
    Act(Action),
    /// A pasted layer, floated in its place.
    Float(Floating),
    /// The save a close asked for.
    SaveThenClose,
}

/// A modal question over the window.
#[derive(Debug)]
enum Modal {
    /// Save the changes before closing?
    Close(tairix_controls::Dialog),
    /// A form of settings.
    Form(alloc::boxed::Box<Form>),
}

/// Work a worker is doing for the window, and what becomes of its answer.
#[derive(Debug)]
struct Pending {
    job: u64,
    /// The entry it was asked of, and the document's generation then: the
    /// state its answer is written over.
    entry: usize,
    generation: u64,
    /// The transform asked for, whose sprite details its answer refits.
    transform: Option<Transform>,
    /// What becomes of the selection once it lands.
    settles: Settles,
    /// What to say if it cannot be done.
    what: &'static str,
}

/// One painter window's state.
#[derive(Debug)]
pub struct View {
    document: Document,
    name: String,
    access: Access,
    viewport: Viewport,
    tool: Tool,
    options: Options,
    primary: Ink,
    secondary: Ink,
    /// The kind the two inks were chosen on, so they are carried across when
    /// the picture's kind changes.
    inks_for: Kind,
    toolbar: Toolbar,
    wells: Vec<Ink>,
    swatches: SwatchGrid,
    settings: FieldGroup,
    vertical: ScrollBar,
    horizontal: ScrollBar,
    gesture: Option<Gesture>,
    /// The button that began the drag under way: only its release ends it.
    dragging: Option<PointerButton>,
    /// The rectangle marked out, in picture pixels.
    selection: Option<Bounds>,
    held: Option<Floating>,
    grid: bool,
    pointer: Point,
    modifiers: Modifiers,
    message: Option<String>,
    modal: Option<Modal>,
    pending: Option<Pending>,
    next_job: u64,
    spray: NonCryptoRng,
    spray_due: Option<u64>,
    repeat_due: Option<u64>,
    clicks: ClickRun,
    double_click: Duration64,
    /// The picture pixel under the pointer, which the status band states.
    hover: Option<(u32, u32)>,
}

impl SavedDocument for View {
    type Snapshot = Snapshot;

    fn access(&self) -> Access {
        self.access
    }

    fn is_modified(&self) -> bool {
        self.document.is_modified() || self.held.is_some()
    }

    fn is_empty(&self) -> bool {
        !self.document.is_modified() && self.document.history_depth() == 0
    }

    fn snapshot(&mut self) -> Option<(u64, Arc<Snapshot>)> {
        // Every save puts a floating layer down before it asks for one.
        if self.held.is_some() {
            return None;
        }
        let snapshot = self.document.snapshot().ok()?;
        Some((self.document.generation(), Arc::new(snapshot)))
    }

    fn saved(&mut self, generation: u64) {
        self.document.saved(generation);
    }

    fn rename(&mut self, name: String) {
        self.name = name;
    }

    fn set_access(&mut self, access: Access) {
        self.access = access;
    }

    fn say(&mut self, message: String) {
        View::say(self, message);
    }
}

impl DocumentView for View {
    type Layout = Layout;
    type Faces = Faces;
    type MenuKind = MenuKind;
    type Own = Own;

    fn name(&self) -> &str {
        View::name(self)
    }

    fn write_title(&self, title: &mut String) {
        View::write_title(self, title);
    }

    fn layout(&self, width: u32, height: u32, theme: &Theme, scale: Scale, faces: Faces) -> Layout {
        View::layout(self, width, height, theme, scale, faces)
    }

    fn min_size(&self, theme: &Theme, scale: Scale, faces: Faces) -> (u32, u32) {
        View::min_size(self, theme, scale, faces)
    }

    fn settle(&mut self, layout: &Layout, damage: &mut Region) {
        View::settle(self, layout, damage);
    }

    fn message_area(layout: &Layout) -> Rect {
        layout.message()
    }

    fn asking_to_close(&self) -> bool {
        matches!(self.modal, Some(Modal::Close(_)))
    }

    /// Busy while a worker has the picture, the cross over the canvas, the
    /// arrow elsewhere.
    fn cursor(&self, layout: &Layout, at: Point) -> CursorShape {
        if self.busy() {
            CursorShape::Busy
        } else if !self.asking()
            && self.document.picture().is_some()
            && layout.canvas().contains(at)
        {
            CursorShape::Crosshair
        } else {
            CursorShape::Arrow
        }
    }

    fn tool_tip(&self, layout: &Layout, scale: Scale, theme: &Theme) -> Option<(Rect, &str)> {
        View::tool_tip(self, layout, scale, theme)
    }

    fn menu(&self, kind: MenuKind) -> AppMenu {
        View::menu(self, kind)
    }

    fn close_requested(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        View::close_requested(self, layout, damage)
    }

    fn chosen(&mut self, item: AppMenuItemId, layout: &Layout, damage: &mut Region) -> Outcome {
        View::chosen(self, item, layout, damage)
    }

    fn entered(
        &mut self,
        item: AppMenuItemId,
        text: &str,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        View::entered(self, item, text, layout, damage)
    }

    fn focus_changed(&mut self, focused: bool, layout: &Layout, damage: &mut Region) {
        View::focus_changed(self, focused, layout, damage);
    }

    fn input(
        &mut self,
        input: &InputEvent,
        now_ns: u64,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        match *input {
            InputEvent::KeyPressed { key, modifiers } => {
                let stroke = Keystroke {
                    key,
                    modifiers,
                    at_ns: now_ns,
                };
                self.on_key(stroke, layout, scale, theme, damage)
            }
            InputEvent::KeyReleased { .. } => Outcome::none(),
            _ => self.on_pointer(input, now_ns, layout, scale, theme, damage),
        }
    }

    fn refuse_save(&self, name: &str) -> Option<String> {
        self.save_format(name)
            .err()
            .map(|refusal| alloc::format!("{refusal}"))
    }

    fn offered_extension(&self) -> &'static str {
        natural(self.document.entries(), self.document.origin()).extension()
    }

    fn save_endings(&self) -> Result<SaveEndings, String> {
        save_endings(self.document.entries(), self.document.origin())
            .map_err(|refusal| alloc::format!("{refusal}"))
    }
}

impl View {
    /// A window on `document`, called `name`, which it may write as `access`
    /// says; presses pair under `double_click`.
    #[must_use]
    pub fn new(document: Document, name: String, access: Access, double_click: Duration64) -> Self {
        let flat = || ScrollModel::new(ScrollRange::new(0, 0, 0), 1, 1);
        let kind = picture_kind(&document);
        let tool = Tool::Brush;
        let options = Options::default();
        let smooth = kind.sample_bytes() == 4;
        let aspect = document.picture().map_or((1, 1), Picture::pixel_aspect);
        let mut view = Self {
            document,
            name,
            access,
            viewport: Viewport::new(aspect),
            tool,
            options,
            primary: Ink::Colour([0, 0, 0, 255]),
            secondary: Ink::Colour(WHITE),
            inks_for: Kind::Rgba,
            toolbar: strip(tool),
            wells: Vec::new(),
            swatches: SwatchGrid::new(8, Vec::new()),
            settings: options.panel(tool, smooth),
            vertical: ScrollBar::new(ScrollOrientation::Vertical, flat()),
            horizontal: ScrollBar::new(ScrollOrientation::Horizontal, flat()),
            gesture: None,
            dragging: None,
            selection: None,
            held: None,
            grid: false,
            pointer: Point::new(-1, -1),
            modifiers: Modifiers::default(),
            message: None,
            modal: None,
            pending: None,
            next_job: 1,
            spray: NonCryptoRng::seed_from_u64(0x5eed_5eed),
            spray_due: None,
            repeat_due: None,
            clicks: ClickRun::new(),
            double_click,
            hover: None,
        };
        view.adopt_kind();
        view
    }

    /// The document.
    #[must_use]
    pub const fn document(&self) -> &Document {
        &self.document
    }

    /// What the document is called.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Say `message` in the status band.
    pub fn say(&mut self, message: impl Into<String>) {
        self.message = Some(message.into());
    }

    /// The status band's message, if one was said since.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Write what the window's title reads over `title`, reusing its room.
    pub fn write_title(&self, title: &mut String) {
        let (modified, read_only) = (
            SavedDocument::is_modified(self),
            self.access == Access::ReadOnly,
        );
        write_document_title(title, &self.name, modified, read_only, APP_TITLE);
    }

    /// The tool in use.
    #[must_use]
    pub const fn tool(&self) -> Tool {
        self.tool
    }

    /// The format the document is written in under `name`.
    ///
    /// # Errors
    ///
    /// [`SaveRefusal`] where it cannot be written so; nothing is written.
    pub fn save_format(&self, name: &str) -> Result<SaveFormat, SaveRefusal> {
        format_for(name, self.document.entries(), self.document.origin())
    }

    /// The primary and secondary inks.
    #[must_use]
    pub const fn inks(&self) -> (Ink, Ink) {
        (self.primary, self.secondary)
    }

    /// How the picture is shown.
    #[must_use]
    pub const fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    /// Whether the grid between pixels is to be drawn.
    #[must_use]
    pub fn grid_shown(&self) -> bool {
        self.grid
            && self
                .viewport
                .pixel_span()
                .0
                .min(self.viewport.pixel_span().1)
                >= GRID_FROM
    }

    /// The selection marked out, in picture pixels.
    #[must_use]
    pub const fn selection(&self) -> Option<Bounds> {
        self.selection
    }

    /// The layer floating over the picture.
    #[must_use]
    pub fn floating(&self) -> Option<&Floating> {
        self.held.as_ref()
    }

    /// The shape a drag is drawing, not yet put down.
    #[must_use]
    pub fn preview(&self) -> Option<Preview> {
        let Some(Gesture::Shape {
            from,
            to,
            secondary,
        }) = &self.gesture
        else {
            return None;
        };
        let kind = picture_kind(&self.document);
        Some(Preview {
            layers: self.shape_layers(*from, *to, *secondary, kind),
            smooth: self.smooth(kind),
        })
    }

    /// The picture pixel under the pointer.
    #[must_use]
    pub const fn hover(&self) -> Option<(u32, u32)> {
        self.hover
    }

    /// Whether a worker is doing something the picture waits for.
    #[must_use]
    pub const fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// The controls, for the painter.
    #[must_use]
    pub(crate) const fn controls(
        &self,
    ) -> (&Toolbar, &SwatchGrid, &FieldGroup, &ScrollBar, &ScrollBar) {
        (
            &self.toolbar,
            &self.swatches,
            &self.settings,
            &self.vertical,
            &self.horizontal,
        )
    }

    /// The modal question showing, if any: a close question or a form.
    #[must_use]
    pub(crate) fn modal(&self) -> Option<Result<&tairix_controls::Dialog, &Form>> {
        match &self.modal {
            Some(Modal::Close(dialog)) => Some(Ok(dialog)),
            Some(Modal::Form(form)) => Some(Err(&**form)),
            None => None,
        }
    }

    /// Whether a question is showing.
    #[must_use]
    pub const fn asking(&self) -> bool {
        self.modal.is_some()
    }

    /// The layout of a `width`×`height` window.
    #[must_use]
    pub fn layout(
        &self,
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        faces: Faces,
    ) -> Layout {
        let inner = Layout::panel_inner_width(theme, scale);
        let settings = if self.settings.is_empty() {
            0
        } else {
            let column = self.settings.slot_column(inner, scale, theme);
            self.settings.measured_height(inner, column, scale, theme)
        };
        let needs = PanelNeeds {
            swatches: self.swatches.height_for_width(inner),
            settings,
        };
        Layout::for_window(width, height, theme, scale, faces, needs)
    }

    /// The smallest window worth laying out.
    #[must_use]
    pub fn min_size(&self, theme: &Theme, scale: Scale, faces: Faces) -> (u32, u32) {
        Layout::min_size(theme, scale, faces, &self.toolbar)
    }

    /// The tip for the tool the pointer is over, with its rectangle.
    #[must_use]
    pub fn tool_tip(
        &self,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
    ) -> Option<(Rect, &'static str)> {
        let tools = layout.tools();
        let index = self.toolbar.tool_at(tools, scale, theme, self.pointer)?;
        let rect = self.toolbar.tool_rect(index, tools, scale, theme)?;
        Some((rect, strip_tip(index)?))
    }

    fn picture_size(&self) -> (u32, u32) {
        self.document.picture().map_or((1, 1), |picture| {
            (picture.canvas.width(), picture.canvas.height())
        })
    }

    /// Bring the scroll and the bars into line with the layout.
    pub fn settle(&mut self, layout: &Layout, damage: &mut Region) {
        let size = self.picture_size();
        let area = layout.canvas();
        if self.viewport.settle(size, area) {
            damage.add(area);
        }
        let (width, height) = self.viewport.extent(size);
        let (x, y) = self.viewport.scroll();
        let model = |content: u64, room: u32, offset: u64| {
            ScrollModel::in_pixels(
                ScrollRange::new(content.max(u64::from(room)), u64::from(room), offset),
                16,
            )
        };
        let vertical = model(height, area.height, y);
        let horizontal = model(width, area.width, x);
        if vertical != self.vertical.model() {
            self.vertical.set_model(vertical);
            damage.add(layout.vertical_bar());
        }
        if horizontal != self.horizontal.model() {
            self.horizontal.set_model(horizontal);
            damage.add(layout.horizontal_bar());
        }
        // The picture may have moved, or changed, under a pointer that did not.
        self.hovered(layout, damage);
    }

    /// The kind the picture showing has, or the inks' own for a kept sprite.
    fn kind(&self) -> &Kind {
        picture_kind(&self.document)
    }

    /// Carry the inks and the panel over to the kind the picture showing now
    /// has.
    fn adopt_kind(&mut self) {
        let kind = self.kind().clone();
        if kind != self.inks_for {
            self.primary = self.primary.adapted(&self.inks_for, &kind);
            self.secondary = self.secondary.adapted(&self.inks_for, &kind);
            self.inks_for = kind.clone();
        }
        self.wells = match &kind {
            Kind::Indexed {
                palette, masked, ..
            } => palette
                .iter()
                .enumerate()
                .map(|(index, _)| Ink::Index(u8::try_from(index).unwrap_or(u8::MAX)))
                .chain(masked.then_some(Ink::Clear))
                .collect(),
            Kind::Rgba => colour_wells(),
        };
        let colours: Vec<Color> = self
            .wells
            .iter()
            .map(|ink| {
                let [r, g, b, a] = ink.shown(&kind);
                Color::rgba(r, g, b, a)
            })
            .collect();
        let columns = match self.wells.len() {
            0..=4 => 4,
            5..=32 => 8,
            _ => 16,
        };
        self.swatches.adopt_colours(columns, colours);
        self.mark_wells();
        self.settings = self.options.panel(self.tool, kind.sample_bytes() == 4);
        let aspect = self
            .document
            .picture()
            .map_or((1, 1), Picture::pixel_aspect);
        self.viewport.set_aspect(aspect);
    }

    /// Put the grid's marks on the wells the inks are.
    fn mark_wells(&mut self) {
        let at = |ink: Ink| self.wells.iter().position(|&well| well == ink);
        if let Some(index) = at(self.primary) {
            self.swatches.adopt_selected(index);
        }
        self.swatches.adopt_secondary(at(self.secondary));
    }

    fn smooth(&self, kind: &Kind) -> bool {
        self.options.smooth && kind.sample_bytes() == 4
    }

    /// The ink `secondary` or the primary would lay on `kind`.
    fn ink(&self, secondary: bool) -> Ink {
        if secondary {
            self.secondary
        } else {
            self.primary
        }
    }

    /// What an eraser lays on `kind`: nothing where a picture can be clear,
    /// else the secondary colour.
    fn eraser_ink(&self, kind: &Kind) -> Ink {
        if kind.holds_transparency() {
            Ink::Clear
        } else {
            self.secondary
        }
    }

    fn layer(ink: Ink, kind: &Kind, smooth: bool) -> Layer {
        let blend = if smooth && kind.sample_bytes() == 4 {
            Blend::Over
        } else {
            Blend::Replace
        };
        Layer { ink, blend }
    }

    /// The layers and shapes a shape tool lays from `from` to `to`.
    fn shape_layers(
        &self,
        from: Fx,
        to: Fx,
        secondary: bool,
        kind: &Kind,
    ) -> [Option<(Layer, Shape)>; 2] {
        let smooth = self.smooth(kind);
        let (front, back) = if secondary {
            (self.secondary, self.primary)
        } else {
            (self.primary, self.secondary)
        };
        let width = self.options.size;
        let mut span = Span {
            from: from.pixel(),
            to: to.pixel(),
        };
        if self.modifiers.shift && self.tool != Tool::Line {
            span = span.squared();
        }
        let shape = |outline: Option<u32>| match self.tool {
            Tool::Rectangle => Shape::Rect { span, outline },
            _ => Shape::Ellipse { span, outline },
        };
        match self.tool {
            Tool::Line => {
                let to = if self.modifiers.shift {
                    snapped(from, to)
                } else {
                    to
                };
                let line = Shape::Capsule {
                    a: from,
                    b: to,
                    radius: i64::from(width) * FX / 2,
                };
                [Some((Self::layer(front, kind, smooth), line)), None]
            }
            _ => match self.options.style {
                Style::Outline => [
                    Some((Self::layer(front, kind, smooth), shape(Some(width)))),
                    None,
                ],
                Style::Filled => [Some((Self::layer(front, kind, smooth), shape(None))), None],
                Style::Both => [
                    Some((Self::layer(back, kind, smooth), shape(None))),
                    Some((Self::layer(front, kind, smooth), shape(Some(width)))),
                ],
            },
        }
    }

    /// The screen rectangle `bounds` of the picture falls on.
    fn screen(&self, bounds: Bounds, layout: &Layout) -> Rect {
        self.viewport
            .to_screen(bounds, self.picture_size(), layout.canvas())
    }

    /// Report `bounds` of the picture as damaged.
    fn damage_picture(&self, bounds: Option<Bounds>, layout: &Layout, damage: &mut Region) {
        if let Some(bounds) = bounds {
            damage.add(self.screen(grown(bounds, 1), layout));
            // The status band states the colour under the pointer.
            let under = self.hover.is_some_and(|(x, y)| {
                let (x, y) = (i64::from(x), i64::from(y));
                (bounds.x0..bounds.x1).contains(&x) && (bounds.y0..bounds.y1).contains(&y)
            });
            if under {
                damage.add(layout.position());
            }
        }
    }

    /// The window gained or lost the keyboard; losing it ends any drag.
    pub fn focus_changed(&mut self, focused: bool, layout: &Layout, damage: &mut Region) {
        if !focused {
            self.end_gesture(layout, damage);
        }
    }

    /// Size the history for memory pressure `band`.
    pub fn adopt_pressure(&mut self, band: PressureBand) {
        self.document.adopt_pressure(band);
    }

    /// When the window next needs waking, if it does.
    #[must_use]
    pub fn deadline_ns(&self) -> Option<u64> {
        match (self.spray_due, self.repeat_due) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (due, None) | (None, due) => due,
        }
    }

    /// Arm the deadlines the state calls for, now it is `now_ns`.
    pub fn arm_deadline(&mut self, now_ns: u64) {
        let holding = self.toolbar.is_repeating()
            || self.vertical.is_repeating()
            || self.horizontal.is_repeating();
        self.repeat_due = match (holding, self.repeat_due) {
            (true, None) => Some(now_ns.saturating_add(REPEAT_DELAY_NS)),
            (true, armed) => armed,
            (false, _) => None,
        };
        let spraying =
            matches!(self.gesture, Some(Gesture::Stroke { .. })) && self.tool == Tool::Spray;
        self.spray_due = match (spraying, self.spray_due) {
            (true, None) => Some(now_ns.saturating_add(SPRAY_INTERVAL_NS)),
            (true, armed) => armed,
            (false, _) => None,
        };
    }

    /// Do what has fallen due by `now_ns`: another burst of spray, another
    /// step of a held control.
    pub fn tick(
        &mut self,
        now_ns: u64,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        if self.spray_due.is_some_and(|due| due <= now_ns) {
            self.spray_due = Some(now_ns.saturating_add(SPRAY_INTERVAL_NS));
            if let Some(Gesture::Stroke { last, .. }) = &self.gesture {
                let at = *last;
                self.spray_at(at, layout, damage);
            }
        }
        if self.repeat_due.is_some_and(|due| due <= now_ns) {
            self.repeat_due = Some(now_ns.saturating_add(REPEAT_INTERVAL_NS));
            self.toolbar.repeat(layout.tools(), scale, theme, damage);
            if let Some(ScrollAction::ScrollTo { offset }) =
                self.vertical.repeat(layout.vertical_bar(), damage)
            {
                self.scroll_to(None, Some(offset), layout, damage);
            }
            if let Some(ScrollAction::ScrollTo { offset }) =
                self.horizontal.repeat(layout.horizontal_bar(), damage)
            {
                self.scroll_to(Some(offset), None, layout, damage);
            }
        }
    }

    fn scroll_to(&mut self, x: Option<u64>, y: Option<u64>, layout: &Layout, damage: &mut Region) {
        let (sx, sy) = self.viewport.scroll();
        if self.viewport.scroll_to(
            x.unwrap_or(sx),
            y.unwrap_or(sy),
            self.picture_size(),
            layout.canvas(),
        ) {
            damage.add(layout.canvas());
        }
        self.settle(layout, damage);
    }

    /// Magnify to rung `zoom`, keeping the picture point under `anchor`.
    fn zoom_to(&mut self, zoom: usize, anchor: Point, layout: &Layout, damage: &mut Region) {
        let area = layout.canvas();
        let anchor = if area.contains(anchor) {
            anchor
        } else {
            area.center()
        };
        if self
            .viewport
            .zoom_to(zoom, anchor, self.picture_size(), area)
        {
            damage.add(area);
            damage.add(layout.zoom());
        }
        self.settle(layout, damage);
    }

    fn fit(&mut self, layout: &Layout, damage: &mut Region) {
        let rung = self.viewport.fitting(self.picture_size(), layout.canvas());
        self.zoom_to(rung, layout.canvas().center(), layout, damage);
    }
}

/// The pixel `bounds` grown by `by` on every side.
fn grown(bounds: Bounds, by: i64) -> Bounds {
    Bounds {
        x0: bounds.x0 - by,
        y0: bounds.y0 - by,
        x1: bounds.x1 + by,
        y1: bounds.y1 + by,
    }
}

/// `to` moved onto the nearest of the eight directions from `from`: what a
/// line drawn with Shift held follows.
fn snapped(from: Fx, to: Fx) -> Fx {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let (ax, ay) = (dx.abs(), dy.abs());
    // tan(22.5°) is about 0.414: past it the line leaves its axis.
    if ay * 1000 < ax * 414 {
        Fx { x: to.x, y: from.y }
    } else if ax * 1000 < ay * 414 {
        Fx { x: from.x, y: to.y }
    } else {
        let d = ax.max(ay);
        Fx {
            x: from.x + d * dx.signum(),
            y: from.y + d * dy.signum(),
        }
    }
}

/// The kind of the picture showing, or colour for a kept sprite.
fn picture_kind(document: &Document) -> &Kind {
    /// What a kept sprite, which has no pixels, is drawn on as.
    static NO_PICTURE: Kind = Kind::Rgba;
    document
        .picture()
        .map_or(&NO_PICTURE, |picture| picture.canvas.kind())
}

#[path = "view_input.rs"]
mod input;

pub(crate) use input::close_rect;

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
