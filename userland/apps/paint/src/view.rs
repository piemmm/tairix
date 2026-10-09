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

use tairix_abi::window_ipc::{AppMenu, AppMenuItemId, CursorShape, SaveEndings};
use tairix_browse::vfs::write_document_title;
use tairix_colour::Rgba;
use tairix_controls::{
    ColourModel, ColourPicker, Keystroke, PickerView, ScrollAction, ScrollBar, ScrollModel,
    ScrollOrientation, ScrollRange, SwatchGrid, SwatchMark, TitleBar, Toolbar, REPEAT_DELAY_NS,
    REPEAT_INTERVAL_NS,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_image::{desktop_palette, IndexDepth, Rgba8};
use tairix_input::{InputEvent, Modifiers, PointerButton};
use tairix_raster::Color;
use tairix_reclaim::PressureBand;
use tairix_theme::Theme;
use tairix_window::docapp::{DocumentView, ToolGone, ToolMove, ToolOpening, ToolWindow};
use tairix_window::document::{Access, SavedDocument};

use crate::adjust::AdjustPane;
use crate::canvas::{Canvas, CanvasError, Kind, OutOfMemory, Tile};
use crate::colour::{Ink, BLACK, WHITE};
use crate::dialog::{Form, Purpose};
use crate::document::{Document, Layer, NewPicture, Picture, Snapshot};
use crate::fill;
use crate::filter::{Filter, FilterError};
use crate::gradient::Gradient;
use crate::layout::{Faces, Floor, Layout, Needs};
use crate::mask::{Combine, Mask, Recipe};
use crate::pane::{Arrangement, PaneKind, Side};
use crate::preferences::{CanvasStyle, OpenAt, Preferences};
use crate::save::{format_for, natural, save_endings, survey, Loss, SaveFormat, SaveRefusal};
use crate::selection::{cut_out, Floating};
use crate::shape::{Bounds, Point as Fx, Shape, Span, FX};
use crate::stroke::{Blend, Coat, Stroke};
use crate::text::TextEntry;
use crate::tool::{
    tool_box, tool_index, view_strip, Marquee, Options, Style, Tool, ViewCommand, VIEW_COMMANDS,
};
use crate::tool_controls::ToolControls;
use crate::transform::{Transform, TransformError};
use crate::viewport::{Viewport, ACTUAL, ZOOMS};

/// The application's name, as window titles end.
pub const APP_TITLE: &str = "Paint";

/// How often a held airbrush lays another dab.
pub const AIRBRUSH_INTERVAL_NS: u64 = 25_000_000;

/// A new sprite's size to start.
const SPRITE_SIZE: (u32, u32) = (32, 32);

/// The most wells the palette strip holds: a 256-colour palette and the
/// clear ink of its mask.
pub const MOST_WELLS: usize = IndexDepth::Eight.colours() + 1;

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
    /// Make each of the picture's layers anew, each by its own transform.
    Transform {
        /// The layers' pixels, as they stood, and what to make of each.
        layers: Vec<(Canvas, Transform)>,
    },
    /// Lay `layers` together onto nothing: a merge, or the picture flattened.
    Compose {
        /// The layers, the bottom first, as they stood.
        layers: Vec<Layer>,
    },
    /// Find what each format the document can be written as would not keep
    /// of it, for the Save As sheet.
    Survey {
        /// The document, as it stood.
        snapshot: Snapshot,
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
        coat: Coat,
        /// Whether only pixels joined to `at` are filled, rather than every
        /// pixel like it.
        contiguous: bool,
        /// The selection the fill is held to, if one is.
        clip: Option<Mask>,
    },
    /// Run `filter` over `canvas`.
    Filter {
        /// The picture, as it stood.
        canvas: Canvas,
        /// What is run.
        filter: Filter,
        /// The selection it is held to, if one is.
        clip: Option<Mask>,
    },
    /// Count `canvas`'s levels for a histogram.
    Histogram {
        /// The picture, as it stood.
        canvas: Canvas,
        /// The selection it is held to, if one is.
        clip: Option<Mask>,
    },
    /// Lay `gradient` over `canvas`.
    Gradient {
        /// The picture, as it stood.
        canvas: Canvas,
        /// What is laid.
        gradient: Gradient,
        /// The selection it is held to, if one is.
        clip: Option<Mask>,
    },
    /// Put `floating` down on `canvas`.
    PutDown {
        /// The picture, as it stood.
        canvas: Canvas,
        /// The selection floating over it.
        floating: Floating,
    },
    /// Clear what `chosen` selects of `canvas` to `ink`, as an eraser would.
    Clear {
        /// The picture, as it stood.
        canvas: Canvas,
        /// What is cleared, and how much of each pixel.
        chosen: Mask,
        /// What is left there.
        ink: Ink,
    },
    /// Make a selection and meet it with the one held.
    Select {
        /// What it is made from.
        recipe: Recipe,
        /// The selection held, which it meets.
        before: Option<Mask>,
        /// How the two meet.
        combine: Combine,
        /// How far the new part's edge is softened, in pixels, and whether
        /// its edges are smoothed.
        edge: (u32, bool),
        /// The picture's pixels, which hold it.
        within: Bounds,
    },
}

/// What a worker answers.
#[derive(Debug)]
pub enum Computed {
    /// A transform's layers, one a layer asked.
    Picture(Result<Vec<Canvas>, TransformError>),
    /// Layers laid together.
    Composed(Result<Canvas, OutOfMemory>),
    /// What each format the document can be written as would not keep.
    Survey(Result<Vec<(SaveFormat, Vec<Loss>)>, OutOfMemory>),
    /// A fill's tiles, each the tile as it now stands.
    Tiles(Result<Vec<(usize, Arc<Tile>)>, OutOfMemory>),
    /// The selection made, `None` where it chooses nothing.
    Selection(Result<Option<Mask>, OutOfMemory>),
    /// A filter's tiles, each the tile as it now stands.
    Filtered(Result<Vec<(usize, Arc<Tile>)>, FilterError>),
    /// A histogram.
    Histogram(Result<crate::histogram::Histogram, OutOfMemory>),
}

/// Carry out `work`: what the worker runs.
#[must_use]
pub fn compute(work: Compute) -> Computed {
    match work {
        Compute::Transform { layers } => Computed::Picture(transformed(&layers)),
        Compute::Compose { layers } => Computed::Composed(crate::compose::flatten(&layers)),
        Compute::Survey { snapshot } => {
            Computed::Survey(survey(&snapshot.entries, snapshot.current, snapshot.origin))
        }
        Compute::Fill {
            mut canvas,
            at,
            tolerance,
            coat,
            contiguous,
            clip,
        } => Computed::Tiles(filled(
            &mut canvas,
            (at, tolerance, contiguous),
            (coat, clip),
        )),
        Compute::Gradient {
            mut canvas,
            gradient,
            clip,
        } => Computed::Tiles(crate::gradient::lay(&mut canvas, &gradient, clip.as_ref())),
        Compute::Filter {
            mut canvas,
            filter,
            clip,
        } => Computed::Filtered(crate::filter::apply(&mut canvas, &filter, clip.as_ref())),
        Compute::Histogram { canvas, clip } => {
            Computed::Histogram(crate::histogram::Histogram::of(&canvas, clip.as_ref()))
        }
        Compute::PutDown {
            mut canvas,
            floating,
        } => Computed::Tiles(floating.put_down(&mut canvas)),
        Compute::Clear {
            mut canvas,
            chosen,
            ink,
        } => Computed::Tiles(crate::selection::cleared(&mut canvas, &chosen, ink)),
        Compute::Select {
            recipe,
            before,
            combine,
            edge,
            within,
        } => Computed::Selection(crate::mask::select(
            &recipe,
            before.as_ref(),
            combine,
            edge,
            within,
        )),
    }
}

/// Each of `layers` made anew by its transform.
fn transformed(layers: &[(Canvas, Transform)]) -> Result<Vec<Canvas>, TransformError> {
    let mut made = Vec::new();
    made.try_reserve_exact(layers.len())
        .map_err(|_| TransformError::OutOfMemory)?;
    for (canvas, transform) in layers {
        made.push(crate::transform::apply(canvas, *transform)?);
    }
    Ok(made)
}

/// What a copy takes, as it stood: the pixels are cut out on the queue's
/// worker, so a copy of a whole picture costs the loop nothing.
#[derive(Debug)]
pub enum Clip {
    /// A floating selection.
    Floating(Floating),
    /// What a selection chooses of a picture.
    Area {
        /// The picture.
        canvas: Canvas,
        /// The selection, and how much of each pixel it chooses.
        chosen: Mask,
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
            Self::Area { canvas, chosen } => cut_out(canvas, chosen),
        }
    }
}

fn filled(
    canvas: &mut Canvas,
    (at, tolerance, contiguous): ((u32, u32), u8, bool),
    (coat, clip): (Coat, Option<Mask>),
) -> Result<Vec<(usize, Arc<Tile>)>, OutOfMemory> {
    let region = if contiguous {
        fill::region(canvas, at.0, at.1, tolerance)?
    } else {
        fill::similar(canvas, at.0, at.1, tolerance)?
    };
    let Some(region) = region else {
        return Ok(Vec::new());
    };
    let stroke = fill::fill(canvas, &region, coat, clip)?;
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
    /// Open a window on a new picture, to be saved as `format`.
    NewWindow {
        /// The picture.
        picture: NewPicture,
        /// What it is to be saved as.
        format: SaveFormat,
    },
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
    /// Paste what the clipboard holds, floating over a picture of this
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
    /// Save somewhere new, in a format chosen first.
    SaveAs,
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
    /// Fill what is selected, or the whole picture, with the primary ink.
    FillSelection,
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
    /// Show the sprite or page before.
    PreviousEntry,
    /// Show the one after.
    NextEntry,
    /// Add a sprite, or a page to a TIFF's pages.
    NewEntry,
    /// Add a copy of the sprite or page showing.
    DuplicateEntry,
    /// Remove the one showing.
    DeleteEntry,
    /// Move the one showing up the list.
    EntryUp,
    /// Move it down.
    EntryDown,
    /// Magnify more.
    ZoomIn,
    /// Magnify less.
    ZoomOut,
    /// Fit the picture in the window.
    Fit,
    /// A picture pixel to a screen pixel.
    Actual,
    /// Show or hide the grid between pixels.
    PixelGrid,
    /// Show or hide the grid laid over the picture.
    Grid,
    /// Put a floating selection down.
    PutDown,
    /// Show the sprite of the name typed, or the sprite or page of the
    /// number.
    GoTo,
    /// Rename the sprite showing.
    Rename,
    /// Add a clear layer over the one painted on.
    NewLayer,
    /// Add a copy of the layer painted on over it.
    DuplicateLayer,
    /// Take the layer painted on away.
    DeleteLayer,
    /// Paint on the layer above.
    LayerAbove,
    /// Paint on the layer below.
    LayerBelow,
    /// Paint on the layer of the name typed, or of the number.
    GoToLayer,
    /// Move the layer painted on up the stack.
    RaiseLayer,
    /// Move it down.
    LowerLayer,
    /// Lay the layer painted on over the one beneath, the two as one.
    MergeDown,
    /// Lay every layer together as one.
    Flatten,
    /// Show the layer painted on, or hide it.
    ShowLayer,
    /// Name the layer painted on, and say how much of it shows.
    LayerProperties,
    /// Choose a tool.
    Tool(Tool),
    /// Magnify to a rung of the ladder.
    Zoom(usize),
    /// Adjust or filter the picture: an entry of [`Filter::ALL`].
    Adjust(usize),
    /// Show a pane of the chrome, or hide it.
    Pane(PaneKind),
    /// Put every pane back where a new window has it.
    ResetPanes,
    /// Put the inks back to black and white.
    ResetColours,
    /// Take the next press on the picture as a colour for the ink the
    /// colour pane edits.
    PickColour,
}

impl Action {
    /// Whether it acts with a floating selection still floating: copying,
    /// clearing or pasting over it, the view and the inks; every other
    /// action puts the selection down first.
    #[must_use]
    pub const fn leaves_floating(self) -> bool {
        matches!(
            self,
            Self::NewPicture
                | Self::Open
                | Self::Close
                | Self::Cut
                | Self::Copy
                | Self::Paste
                | Self::Delete
                | Self::DeleteEntry
                | Self::EditPrimary
                | Self::EditSecondary
                | Self::SwapColours
                | Self::ZoomIn
                | Self::ZoomOut
                | Self::Fit
                | Self::Actual
                | Self::PixelGrid
                | Self::Grid
                | Self::Rename
                | Self::Zoom(_)
                | Self::Tool(Tool::Select)
                | Self::Pane(_)
                | Self::ResetPanes
                | Self::ResetColours
                | Self::PickColour
        )
    }
}

impl Action {
    /// Whether it leaves what `tool` is in the middle of — a polygon's
    /// corners, a crop box, text being typed — as it is: the view's own
    /// actions, the inks, and choosing the tool again; every other action
    /// finishes or turns it down first.
    #[must_use]
    pub fn spares(self, tool: Tool) -> bool {
        matches!(
            self,
            Self::ZoomIn
                | Self::ZoomOut
                | Self::Fit
                | Self::Actual
                | Self::PixelGrid
                | Self::Grid
                | Self::Zoom(_)
                | Self::EditPrimary
                | Self::EditSecondary
                | Self::SwapColours
                | Self::Pane(_)
                | Self::ResetPanes
                | Self::ResetColours
                | Self::PickColour
        ) || self == Self::Tool(tool)
    }
}

/// The actions with no argument, by id: an action's position here is its
/// menu id, less one.
const PLAIN_ACTIONS: [Action; 59] = [
    Action::NewPicture,
    Action::Open,
    Action::Save,
    Action::SaveAs,
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
    Action::PreviousEntry,
    Action::NextEntry,
    Action::NewEntry,
    Action::DuplicateEntry,
    Action::DeleteEntry,
    Action::EntryUp,
    Action::EntryDown,
    Action::ZoomIn,
    Action::ZoomOut,
    Action::Fit,
    Action::Actual,
    Action::PixelGrid,
    Action::Grid,
    Action::PutDown,
    Action::GoTo,
    Action::FillSelection,
    Action::NewLayer,
    Action::DuplicateLayer,
    Action::DeleteLayer,
    Action::LayerAbove,
    Action::LayerBelow,
    Action::GoToLayer,
    Action::RaiseLayer,
    Action::LowerLayer,
    Action::MergeDown,
    Action::Flatten,
    Action::ShowLayer,
    Action::LayerProperties,
    Action::ResetPanes,
    Action::ResetColours,
    Action::PickColour,
];

/// Where the argument-carrying families' ids start, and the one entry
/// field's own id.
const TOOL_IDS: u16 = 100;
const ZOOM_IDS: u16 = 200;
const RENAME_ID: u16 = 300;
const GO_TO_ENTRY: u16 = 301;
const RENAME_ENTRY: u16 = 302;
const GO_TO_LAYER: u16 = 303;
const FILTER_IDS: u16 = 400;
const PANE_IDS: u16 = 500;

impl Action {
    /// The menu id this action is chosen by.
    #[must_use]
    pub fn id(self) -> u16 {
        let at =
            |base: u16, index: Option<usize>| base + u16::try_from(index.unwrap_or(0)).unwrap_or(0);
        match self {
            Self::Tool(tool) => at(TOOL_IDS, Some(tool_index(tool))),
            Self::Zoom(rung) => at(ZOOM_IDS, Some(rung)),
            Self::Adjust(index) => at(FILTER_IDS, Some(index)),
            Self::Pane(kind) => at(PANE_IDS, Some(kind.index())),
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
            FILTER_IDS..PANE_IDS => {
                let index = usize::from(id - FILTER_IDS);
                (index < Filter::ALL.len()).then_some(Self::Adjust(index))
            }
            PANE_IDS.. => PaneKind::ALL
                .get(usize::from(id - PANE_IDS))
                .map(|&kind| Self::Pane(kind)),
            _ => None,
        }
    }
}

impl From<Action> for u16 {
    fn from(action: Action) -> Self {
        action.id()
    }
}

impl From<ViewCommand> for Action {
    fn from(command: ViewCommand) -> Self {
        match command {
            ViewCommand::ZoomOut => Self::ZoomOut,
            ViewCommand::ZoomIn => Self::ZoomIn,
            ViewCommand::Fit => Self::Fit,
            ViewCommand::Actual => Self::Actual,
            ViewCommand::PixelGrid => Self::PixelGrid,
        }
    }
}

/// A selection being marked out, as its outline is drawn.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Marking<'a> {
    /// A rectangle or an ellipse, dragged.
    Shape(Shape),
    /// A path through these points, in picture units: a lasso's, or a
    /// polygon's corners and on to `to`, where the pointer is.
    Path {
        /// The points so far.
        points: &'a [Fx],
        /// Where the next corner would go.
        to: Option<Fx>,
    },
}

/// A shape being dragged, as it will be laid down: each layer and the shape
/// it covers, fill first.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Preview {
    /// The coats.
    pub coats: [Option<(Coat, Shape)>; 2],
    /// Whether edges are smoothed.
    pub smooth: bool,
}

/// A drag in progress on the canvas.
#[derive(Debug)]
enum Gesture {
    /// Paint laid down as the pointer moves along `path`.
    Stroke {
        stroke: alloc::boxed::Box<Stroke>,
        path: crate::brush::Path,
    },
    /// A shape following the pointer, put down when it lets go.
    Shape { from: Fx, to: Fx, secondary: bool },
    /// A rectangle or an ellipse being marked out, met with the selection
    /// held as `combine` says once it lets go.
    Marquee { from: Fx, to: Fx, combine: Combine },
    /// A lasso drawn through `points`, met so once it lets go.
    Lasso { points: Vec<Fx>, combine: Combine },
    /// A gradient dragged from one picture point to another, its inks
    /// swapped when `secondary`.
    Gradient { from: Fx, to: Fx, secondary: bool },
    /// The view dragged from screen point `from`, scrolled as it was.
    Pan { from: Point, scroll: (u64, u64) },
    /// A box dragged on screen to magnify to, or a click to step the zoom,
    /// out when `out`.
    ZoomBox { from: Point, to: Point, out: bool },
    /// A crop box being set out from pixel `from`, the one held before it
    /// kept to turn the drag down to.
    CropNew {
        from: (i64, i64),
        before: Option<Bounds>,
    },
    /// The crop box's edges `grab` takes, dragged from pixel `from`, the box
    /// `start` as it was.
    CropAdjust {
        grab: crate::crop::Grab,
        from: (i64, i64),
        start: Bounds,
    },
    /// A floating selection being dragged from pixel `from`, its top left
    /// then at `corner`.
    Move {
        from: (i64, i64),
        corner: (i64, i64),
    },
}

/// A polygon being marked out a corner at a time: `to` is where the pointer
/// last was, which the next corner follows.
#[derive(Debug)]
struct Draft {
    corners: Vec<Fx>,
    to: Fx,
    aim: Aim,
}

/// What a polygon being marked out becomes once it is closed.
#[derive(Copy, Clone, Debug)]
enum Aim {
    /// A selection, met with the one held as it says.
    Select(Combine),
    /// A polygon drawn, its inks swapped when `secondary`.
    Shape { secondary: bool },
}

/// What becomes of the selection once a worker's answer lands.
#[derive(Debug)]
enum Settles {
    /// Nothing of it: a fill or a transform.
    Nothing,
    /// The floating selection is down, what it covers left selected, and `Then`
    /// follows.
    PutDown(Then),
    /// What floated, or was selected, is cleared away.
    Cleared,
    /// The open adjustment is applied and closed, and `Then` follows.
    Adjusted(Then),
}

/// What follows a floating selection's putting down once it has landed.
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
    /// A menu's entry field committed: its id and the text it held.
    Enter(u16, String),
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
    /// The entry it was asked of, the layer painted on, and the document's
    /// generation then: the state its answer is written over.
    entry: usize,
    layer: usize,
    generation: u64,
    /// What its answer lands as.
    lands: Lands,
    /// What to say if it cannot be done.
    what: &'static str,
}

/// What a worker's answer becomes once it lands.
#[derive(Debug)]
enum Lands {
    /// Tiles of the layer painted on, after which the selection settles so.
    Tiles(Settles),
    /// The selection.
    Selection,
    /// Every layer made anew by `Transform`, the sprite details refitted.
    Transform(Transform),
    /// The layers in the range laid together as one.
    Merged(core::ops::Range<usize>),
    /// The Save As sheet's choices, the window closing once it is saved when
    /// set.
    Sheet {
        /// Whether the save closes the window.
        then_close: bool,
    },
}

/// A palette entry being edited live from the colour dock: the palette it
/// had before, which settling records the change against as one step.
#[derive(Debug)]
pub(crate) struct PaletteEdit {
    pub(crate) entry: u8,
    pub(crate) before: Vec<Rgba8>,
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
    tool_box: Toolbar,
    /// Where each pane of the chrome is, and the band each is headed by.
    panes: Arrangement,
    headers: [TitleBar; PaneKind::ALL.len()],
    /// A pane being dragged by its band, and where it would land.
    pane_drag: Option<PaneDrag>,
    /// Where a press held on a pane's band lies from the band's top-left.
    band_grab: Option<Point>,
    /// Where a floating pane being moved by its tool window would dock.
    tool_landing: Option<Landing>,
    /// Where each pane torn out opens its tool window, until it opens.
    openings: [Option<ToolOpening>; PaneKind::ALL.len()],
    /// Where each floating pane opens when nothing tore it out: under the top
    /// band, at the edge of its home side.
    float_homes: [(i32, i32); PaneKind::ALL.len()],
    /// The view strip: the view's own commands.
    commands: Toolbar,
    bar: ToolControls,
    wells: Vec<Ink>,
    swatches: SwatchGrid,
    /// The colour pane's picker, editing the ink `editing` names.
    picker: ColourPicker,
    /// The colour pane's buttons and choices.
    colour_controls: crate::panel::Panel,
    /// The colours last settled, the most recent first, and their wells.
    recents: Vec<Rgba>,
    recent_grid: SwatchGrid,
    /// Whether the next press on the picture is a one-shot colour pick.
    picking_colour: bool,
    editing: SwatchMark,
    palette_edit: Option<PaletteEdit>,
    vertical: ScrollBar,
    horizontal: ScrollBar,
    gesture: Option<Gesture>,
    /// The button that began the drag under way: only its release ends it.
    dragging: Option<PointerButton>,
    /// The selection held, which painting is held to.
    selection: Option<Mask>,
    /// Counts every change to the selection, so an answer worked from one
    /// selection is not landed on another.
    selection_epoch: u64,
    /// A polygon being marked out.
    draft: Option<Draft>,
    /// The crop tool's box, in picture pixels.
    crop: Option<Bounds>,
    /// Text being typed.
    text: Option<TextEntry>,
    /// Where the clone tool copies from, and once a stroke has begun from
    /// it, how far that lies from where it paints, in pixels.
    clone_from: Option<Fx>,
    clone_offset: Option<(i64, i64)>,
    /// Whether Space is held, which drags the view whatever the tool.
    space: bool,
    /// The Adjustment pane: the open adjustment's settings, or the list to
    /// open one from.
    adjustment: AdjustPane,
    /// What the picture shows of the open adjustment, and the histogram it
    /// reads.
    looks: adjust::Looks,
    held: Option<Floating>,
    /// The grids shown.
    grids: Grids,
    style: CanvasStyle,
    /// The panes *Reset panes* puts back.
    home_panes: Arrangement,
    /// The picture is fitted to the window once it is first laid out.
    open_fitted: bool,
    /// What *New picture* offers to start.
    new_picture: (NewPicture, SaveFormat),
    pointer: Point,
    modifiers: Modifiers,
    /// What a Ctrl-wheel turn has left short of a whole zoom rung.
    zoom_carry: i64,
    /// A pinch under way: the view it began from and where it began.
    pinch: Option<(Viewport, Point)>,
    message: Option<String>,
    modal: Option<Modal>,
    /// The format the Save As sheet chose, which the picker that follows
    /// holds the name to.
    save_as: Option<SaveFormat>,
    pending: Option<Pending>,
    next_job: u64,
    airbrush_due: Option<u64>,
    repeat_due: Option<u64>,
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
        // Every save puts a floating selection down before it asks for one.
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
        self.save_as = None;
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
        match &self.modal {
            Some(Modal::Close(_)) => true,
            Some(Modal::Form(form)) => form.purpose() == Purpose::SaveAs { then_close: true },
            None => false,
        }
    }

    /// Busy while a worker has the picture; the arrow while a question or an
    /// open list holds the pointer; text entry over a number field; over the
    /// canvas, the hand where a press drags the view, text entry for the
    /// text tool, and the cross otherwise; the arrow elsewhere.
    fn cursor(&self, layout: &Layout, at: Point) -> CursorShape {
        let over_canvas = layout.canvas().contains(at);
        if over_canvas && self.panning() {
            CursorShape::Pointer
        } else if self.busy() {
            CursorShape::Busy
        } else if self.asking() || self.bar.listing() {
            CursorShape::Arrow
        } else if self.bar.text_at(layout.bar(), at) {
            CursorShape::Text
        } else if self.document.picture().is_some() && over_canvas {
            if self.tool == Tool::Text {
                CursorShape::Text
            } else {
                CursorShape::Crosshair
            }
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
            InputEvent::KeyReleased { key, .. } => {
                if key == tairix_input::Key::Char(' ') {
                    self.space = false;
                }
                Outcome::none()
            }
            _ => self.on_pointer(input, layout, scale, theme, damage),
        }
    }

    fn refuse_save(&self, name: &str) -> Option<String> {
        self.save_format(name)
            .err()
            .map(|refusal| alloc::format!("{refusal}"))
    }

    fn ask_how(
        &mut self,
        then_close: bool,
        layout: &Layout,
        damage: &mut Region,
    ) -> Option<Outcome> {
        Some(self.ask_save_as(then_close, layout, damage))
    }

    fn offered_extension(&self) -> &'static str {
        self.save_as
            .unwrap_or_else(|| natural(self.document.entries(), self.document.origin()))
            .extension()
    }

    fn save_endings(&self) -> Result<SaveEndings, String> {
        let entries = self.document.entries();
        let origin = self.document.origin();
        let format = self.save_as.unwrap_or_else(|| natural(entries, origin));
        save_endings(entries, origin, format).map_err(|refusal| alloc::format!("{refusal}"))
    }

    /// Each floating pane, in the rectangle of the drawing laid out for it.
    fn tool_window(&self, layout: &Layout, index: usize) -> Option<ToolWindow<'_>> {
        let slot = layout.floating().get(index)?;
        Some(ToolWindow {
            id: tool_id(slot.kind),
            title: slot.kind.title(),
            rect: slot.frame,
        })
    }

    fn tool_opening(&mut self, id: u32) -> ToolOpening {
        View::tool_opening(self, id)
    }

    fn tool_moved(
        &mut self,
        moved: ToolMove,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        View::tool_moved(self, moved, layout, scale, theme, damage)
    }

    fn tool_gone(
        &mut self,
        id: u32,
        why: ToolGone,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        View::tool_gone(self, id, why, layout, damage)
    }
}

/// The name a floating pane's tool window goes by: its place in the list of
/// panes.
fn tool_id(kind: PaneKind) -> u32 {
    u32::try_from(kind.index()).unwrap_or(u32::MAX)
}

/// The pane whose tool window goes by `id`.
fn tool_pane(id: u32) -> Option<PaneKind> {
    PaneKind::ALL.into_iter().find(|&kind| tool_id(kind) == id)
}

impl View {
    /// A window on `document`, called `name`, which it may write as `access`
    /// says.
    #[must_use]
    pub fn new(document: Document, name: String, access: Access) -> Self {
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
            primary: Ink::Colour(BLACK),
            secondary: Ink::Colour(WHITE),
            inks_for: Kind::Rgba,
            tool_box: tool_box(tool),
            panes: Arrangement::default(),
            headers: PaneKind::ALL.map(pane_header),
            pane_drag: None,
            band_grab: None,
            tool_landing: None,
            openings: [None; PaneKind::ALL.len()],
            float_homes: [(0, 0); PaneKind::ALL.len()],
            commands: view_strip(true),
            bar: ToolControls::new(tool, options, smooth),
            wells: Vec::new(),
            swatches: SwatchGrid::new(1, Vec::new()),
            picker: ColourPicker::new(Rgba::from_array(BLACK)),
            colour_controls: colour::colour_panel(PickerView::Square, ColourModel::Rgb),
            recents: Vec::new(),
            recent_grid: colour::recent_grid(),
            picking_colour: false,
            editing: SwatchMark::Primary,
            palette_edit: None,
            vertical: ScrollBar::new(ScrollOrientation::Vertical, flat()),
            horizontal: ScrollBar::new(ScrollOrientation::Horizontal, flat()),
            gesture: None,
            dragging: None,
            selection: None,
            selection_epoch: 0,
            draft: None,
            crop: None,
            text: None,
            clone_from: None,
            clone_offset: None,
            space: false,
            adjustment: AdjustPane::choosing(),
            looks: adjust::Looks::default(),
            held: None,
            grids: Grids {
                spaced: false,
                pixels: true,
            },
            style: CanvasStyle::default(),
            home_panes: Arrangement::default(),
            open_fitted: false,
            new_picture: (NewPicture::DEFAULT, SaveFormat::Png),
            pointer: Point::new(-1, -1),
            modifiers: Modifiers::default(),
            zoom_carry: 0,
            pinch: None,
            message: None,
            modal: None,
            save_as: None,
            pending: None,
            next_job: 1,
            airbrush_due: None,
            repeat_due: None,
            hover: None,
        };
        view.picker.set_earlier(Some(Rgba::from_array(BLACK)));
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

    /// Whether the grid between pixels is to be drawn: asked for, and at a
    /// zoom past the one the settings name.
    #[must_use]
    pub fn pixel_grid_shown(&self) -> bool {
        let (across, down) = self.viewport.pixel_span();
        let from = u64::from(self.style.pixel_grid_from);
        self.grids.pixels && from != 0 && across.min(down) * 100 >= from
    }

    /// Whether the grid laid over the picture is to be drawn.
    #[must_use]
    pub const fn grid_shown(&self) -> bool {
        self.grids.spaced
    }

    /// Whether what is drawn and marked lands on the grid: the grid shown,
    /// and its snapping on.
    #[must_use]
    pub const fn snapping(&self) -> bool {
        self.grids.spaced && self.style.grid.snap
    }

    /// `at`, on the grid's nearest crossing where snapping is on.
    #[must_use]
    pub(crate) fn on_grid(&self, at: Fx) -> Fx {
        if self.snapping() {
            crate::grid::snap_point(&self.style.grid, at)
        } else {
            at
        }
    }

    /// The box of pixels from `from` to `to`, covering whole cells of the
    /// grid where snapping is on.
    #[must_use]
    pub(crate) fn box_of(&self, from: Fx, to: Fx) -> Span {
        let (from, to) = (from.pixel(), to.pixel());
        let (from, to) = if self.snapping() {
            crate::grid::snap_span(&self.style.grid, from, to)
        } else {
            (from, to)
        };
        Span { from, to }
    }

    /// Where the window's panes stand.
    #[must_use]
    pub const fn panes(&self) -> &Arrangement {
        &self.panes
    }

    /// What the window draws the picture with.
    #[must_use]
    pub const fn canvas_style(&self) -> &CanvasStyle {
        &self.style
    }

    /// Start as a new window does: with the tool, the panes and the grid the
    /// settings name, and fitted or at actual size once first laid out.
    pub fn begin(&mut self, preferences: &Preferences) {
        self.tool = preferences.tool;
        self.tool_box.set_active(tool_index(self.tool));
        self.bar = ToolControls::new(self.tool, self.options, self.kind().sample_bytes() == 4);
        self.panes.clone_from(&preferences.panes);
        self.home_panes.clone_from(&preferences.panes);
        self.grids.spaced = preferences.grid.shown;
        self.style = preferences.canvas_style();
        self.open_fitted = preferences.open_at == OpenAt::Fitted;
        self.new_picture = (preferences.new, preferences.format);
    }

    /// Draw as the settings now say, and put the panes back to the ones they
    /// name when asked to; what a window started with stays its own.
    pub fn adopt(&mut self, preferences: &Preferences, layout: &Layout, damage: &mut Region) {
        let style = preferences.canvas_style();
        if style != self.style {
            self.style = style;
            damage.add(layout.canvas());
        }
        self.home_panes.clone_from(&preferences.panes);
        self.new_picture = (preferences.new, preferences.format);
    }

    /// The selection held, which painting is held to.
    #[must_use]
    pub const fn selection(&self) -> Option<&Mask> {
        self.selection.as_ref()
    }

    /// Replace the selection held, answering the one it replaces; a change
    /// is counted, so an answer worked from one selection is not landed on
    /// another.
    pub(super) fn set_selection(&mut self, selection: Option<Mask>) -> Option<Mask> {
        if self.selection.is_some() || selection.is_some() {
            self.selection_epoch = self.selection_epoch.wrapping_add(1);
        }
        core::mem::replace(&mut self.selection, selection)
    }

    /// The selection being marked out, if one is.
    #[must_use]
    pub fn marking(&self) -> Option<Marking<'_>> {
        if let Some(draft) = &self.draft {
            return Some(Marking::Path {
                points: &draft.corners,
                to: Some(draft.to),
            });
        }
        match &self.gesture {
            Some(Gesture::Marquee { from, to, .. }) => {
                Some(Marking::Shape(self.marquee_shape(*from, *to)))
            }
            Some(Gesture::Lasso { points, .. }) => Some(Marking::Path { points, to: None }),
            _ => None,
        }
    }

    /// The shape the select tool marks out dragged from `from` to `to`.
    fn marquee_shape(&self, from: Fx, to: Fx) -> Shape {
        let span = self.box_of(from, to);
        match self.options.marquee {
            Marquee::Ellipse => Shape::Ellipse {
                span,
                outline: None,
            },
            _ => Shape::Rect {
                span,
                outline: None,
            },
        }
    }

    /// The selection floating over the picture.
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
            coats: self.shape_coats(*from, *to, *secondary, kind),
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

    /// The colour dock's picker, and which ink it edits.
    #[must_use]
    pub(crate) const fn dock(&self) -> (&ColourPicker, SwatchMark) {
        (&self.picker, self.editing)
    }

    /// The controls, for the painter.
    #[must_use]
    pub(crate) const fn controls(&self) -> Controls<'_> {
        Controls {
            tool_box: &self.tool_box,
            commands: &self.commands,
            bar: &self.bar,
            swatches: &self.swatches,
            vertical: &self.vertical,
            horizontal: &self.horizontal,
        }
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
        let dock = Layout::dock_inner_width(theme, scale);
        let view_strip = self.commands.natural_length(scale, theme);
        let controls = Layout::controls_width(width, view_strip, theme, scale);
        let needs = Needs {
            wells: self.wells.len(),
            picker: self.picker.measured_height(dock, scale, theme),
            colour_controls: self
                .colour_controls
                .measured_height(dock, faces, scale, theme),
            recents: dock / u32::try_from(colour::RECENT_COLUMNS).unwrap_or(1) * 2,
            tool_box: self.tool_box.breadth(scale, theme),
            tool_box_length: self.tool_box.natural_length(scale, theme),
            adjustment: self.adjustment.measured_height(dock, faces, scale, theme),
            view_strip,
            // As many rows as any tool's bar takes, so the canvas stays put
            // whichever tool is chosen.
            bar_rows: ToolControls::most_rows(controls, faces, scale, theme),
        };
        let mut layout =
            Layout::for_window(width, height, theme, scale, faces, (needs, &self.panes));
        let placement = self
            .bar
            .place(layout.controls(), layout.window(), faces, scale, theme);
        layout.seat_bar(placement);
        layout
    }

    /// The smallest window worth laying out: one that seats every setting
    /// of every tool's bar and the largest palette, whatever document it is
    /// given.
    #[must_use]
    pub fn min_size(&self, theme: &Theme, scale: Scale, faces: Faces) -> (u32, u32) {
        let floor = Floor {
            controls: ToolControls::least_width(faces, scale, theme),
            view_strip: self.commands.natural_length(scale, theme),
            tool_box: (
                self.tool_box.breadth(scale, theme),
                self.tool_box.min_length(scale, theme),
            ),
            wells: MOST_WELLS,
        };
        Layout::min_size(theme, scale, faces, floor, |width| {
            ToolControls::most_rows(width, faces, scale, theme)
        })
    }

    /// The tip for what the pointer is over — a tool, a view command or a
    /// setting — with its rectangle.
    #[must_use]
    pub fn tool_tip(
        &self,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
    ) -> Option<(Rect, &'static str)> {
        let tools = layout.tools();
        if let Some(index) = self.tool_box.tool_at(tools, scale, theme, self.pointer) {
            let rect = self.tool_box.tool_rect(index, tools, scale, theme)?;
            return Some((rect, Tool::ALL.get(index)?.label()));
        }
        let commands = layout.view_strip();
        if let Some(index) = self.commands.tool_at(commands, scale, theme, self.pointer) {
            let rect = self.commands.tool_rect(index, commands, scale, theme)?;
            return Some((rect, VIEW_COMMANDS.get(index)?.2));
        }
        self.bar.tip(layout.bar(), self.pointer)
    }

    fn picture_size(&self) -> (u32, u32) {
        self.document.picture().map_or((1, 1), Picture::size)
    }

    /// The layer painted on.
    fn active_layer(&self) -> usize {
        self.document.picture().map_or(0, Picture::active)
    }

    /// Bring the scroll, the bars and the palette's rows into line with the
    /// layout.
    pub fn settle(&mut self, layout: &Layout, damage: &mut Region) {
        if core::mem::take(&mut self.open_fitted) {
            let rung = self
                .viewport
                .fitting(self.picture_size(), layout.canvas())
                .min(ACTUAL);
            let _ = self.viewport.zoom_to(
                rung,
                layout.canvas().center(),
                self.picture_size(),
                layout.canvas(),
            );
            damage.add(layout.canvas());
        }
        if self.swatches.columns() != layout.columns() {
            self.swatches.set_columns(layout.columns());
            damage.add(layout.palette());
        }
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
        self.settle_float_homes(layout);
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
            let edited = self.ink(self.editing == SwatchMark::Secondary);
            self.primary = self.primary.adapted(&self.inks_for, &kind);
            self.secondary = self.secondary.adapted(&self.inks_for, &kind);
            self.inks_for = kind.clone();
            if self.ink(self.editing == SwatchMark::Secondary) != edited {
                // Carried on, a drag in the dock would edit the ink this one
                // became: a palette entry, where it was choosing a colour.
                self.picker.finish_drag();
            }
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
        // The rows the wells fill follow the layout, which settles them.
        self.swatches
            .adopt_colours(self.swatches.columns(), colours);
        self.mark_wells();
        self.sync_picker();
        self.sync_recents();
        self.bar
            .allow_partial(kind.sample_bytes() == 4, self.options);
        let aspect = self
            .document
            .picture()
            .map_or((1, 1), Picture::pixel_aspect);
        self.viewport.set_aspect(aspect);
    }

    /// Put the grid's marks on the wells the inks are, and on none for an
    /// ink the palette does not hold.
    fn mark_wells(&mut self) {
        let at = |ink: Ink| self.wells.iter().position(|&well| well == ink);
        self.swatches.adopt_selected(at(self.primary));
        self.swatches.adopt_secondary(at(self.secondary));
    }

    /// Bring the dock's picker into line with the ink it edits: its colour,
    /// with the colour it had as the earlier one when the ink changed from
    /// outside the picker; its opacity; and whether it may edit at all.
    ///
    /// A palette picture's ink is an entry, so editing it edits the palette,
    /// which the picture holds no edits for while a worker has it; and clear
    /// is the mask rather than a colour.
    pub(crate) fn sync_picker(&mut self) {
        let ink = self.ink(self.editing == SwatchMark::Secondary);
        let kind = picture_kind(&self.document);
        let truecolour = matches!(kind, Kind::Rgba);
        let colour = Rgba::from_array(ink.shown(kind));
        let picture = self.document.picture();
        let opacity = truecolour || picture.is_some_and(|picture| picture.sprite.is_none());
        let editable = truecolour
            || matches!(ink, Ink::Index(_)) && self.pending.is_none() && picture.is_some();
        if opacity != self.picker.has_opacity() {
            self.picker.set_opacity(opacity);
        }
        // A colour with no alpha is the clear ink, which shows as nothing.
        let showing = if truecolour {
            Ink::of_colour(self.picker.colour().to_array()) == ink
        } else {
            self.picker.colour() == colour
        };
        if !showing && self.palette_edit.is_none() {
            self.picker.set_colour(colour);
            self.picker.set_earlier(Some(colour));
        }
        let mut state = self.picker.state();
        state.enabled = editable;
        if state != self.picker.state() {
            self.picker.set_state(state);
        }
    }

    fn smooth(&self, kind: &Kind) -> bool {
        self.options.smooth && kind.sample_bytes() == 4
    }

    /// Whether edges are smoothed on a picture of `kind`, for the painter.
    #[must_use]
    pub(crate) fn smooth_shown(&self, kind: &Kind) -> bool {
        self.smooth(kind)
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

    fn coat(ink: Ink, kind: &Kind, smooth: bool) -> Coat {
        let blend = if smooth && kind.sample_bytes() == 4 {
            Blend::Over
        } else {
            Blend::Replace
        };
        Coat { ink, blend }
    }

    /// The coats and shapes a shape tool lays from `from` to `to`.
    fn shape_coats(
        &self,
        from: Fx,
        to: Fx,
        secondary: bool,
        kind: &Kind,
    ) -> [Option<(Coat, Shape)>; 2] {
        let smooth = self.smooth(kind);
        let (front, back) = if secondary {
            (self.secondary, self.primary)
        } else {
            (self.primary, self.secondary)
        };
        let width = self.options.size;
        let mut span = self.box_of(from, to);
        if self.modifiers.shift && self.tool != Tool::Line {
            span = span.squared();
        }
        let radius = self.options.corners;
        let shape = |outline: Option<u32>| match self.tool {
            Tool::Rectangle if radius > 0 => Shape::Rounded {
                span,
                outline,
                radius,
            },
            Tool::Rectangle => Shape::Rect { span, outline },
            _ => Shape::Ellipse { span, outline },
        };
        match self.tool {
            Tool::Line => {
                let (from, to) = (self.on_grid(from), self.on_grid(to));
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
                [Some((Self::coat(front, kind, smooth), line)), None]
            }
            _ => match self.options.style {
                Style::Outline => [
                    Some((Self::coat(front, kind, smooth), shape(Some(width)))),
                    None,
                ],
                Style::Filled => [Some((Self::coat(front, kind, smooth), shape(None))), None],
                Style::Both => [
                    Some((Self::coat(back, kind, smooth), shape(None))),
                    Some((Self::coat(front, kind, smooth), shape(Some(width)))),
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

    /// The window gained or lost the keyboard; losing it ends any drag, and
    /// the Space held with it.
    pub fn focus_changed(&mut self, focused: bool, layout: &Layout, damage: &mut Region) {
        self.headers_follow_focus(focused, layout, damage);
        if !focused {
            self.space = false;
            self.end_gesture(layout, damage);
            self.pane_drag = None;
            self.band_grab = None;
        }
    }

    /// Size the history for memory pressure `band`.
    pub fn adopt_pressure(&mut self, band: PressureBand) {
        self.document.adopt_pressure(band);
    }

    /// When the window next needs waking, if it does.
    #[must_use]
    pub fn deadline_ns(&self) -> Option<u64> {
        match (self.airbrush_due, self.repeat_due) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (due, None) | (None, due) => due,
        }
    }

    /// Arm the deadlines the state calls for, now it is `now_ns`.
    pub fn arm_deadline(&mut self, now_ns: u64) {
        let holding = self.tool_box.is_repeating()
            || self.commands.is_repeating()
            || self.vertical.is_repeating()
            || self.horizontal.is_repeating();
        self.repeat_due = match (holding, self.repeat_due) {
            (true, None) => Some(now_ns.saturating_add(REPEAT_DELAY_NS)),
            (true, armed) => armed,
            (false, _) => None,
        };
        let airbrushing =
            matches!(self.gesture, Some(Gesture::Stroke { .. })) && self.tool == Tool::Airbrush;
        self.airbrush_due = match (airbrushing, self.airbrush_due) {
            (true, None) => Some(now_ns.saturating_add(AIRBRUSH_INTERVAL_NS)),
            (true, armed) => armed,
            (false, _) => None,
        };
    }

    /// Do what has fallen due by `now_ns`: another dab of a held airbrush,
    /// another step of a held control.
    pub fn tick(
        &mut self,
        now_ns: u64,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        if self.airbrush_due.is_some_and(|due| due <= now_ns) {
            self.airbrush_due = Some(now_ns.saturating_add(AIRBRUSH_INTERVAL_NS));
            self.airbrush(layout, damage);
        }
        if self.repeat_due.is_some_and(|due| due <= now_ns) {
            self.repeat_due = Some(now_ns.saturating_add(REPEAT_INTERVAL_NS));
            self.tool_box.repeat(layout.tools(), scale, theme, damage);
            self.commands
                .repeat(layout.view_strip(), scale, theme, damage);
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

/// Which grids a window shows.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Grids {
    /// The grid laid over the picture.
    spaced: bool,
    /// The grid between pixels, from the zoom the canvas style names.
    pixels: bool,
}

/// The mini title band heading pane `kind`.
fn pane_header(kind: PaneKind) -> TitleBar {
    let mut header = TitleBar::pane();
    header.set_title(kind.title());
    header
}

/// A pane dragged by its band, the gap down a dock it would land in, and
/// where the press holds the band from its top-left.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct PaneDrag {
    pub(crate) kind: PaneKind,
    pub(crate) landing: Option<Landing>,
    pub(crate) grab: Point,
}

/// Where a dragged pane lands: before the pane at `before` down `side`, or
/// at the dock's foot where `before` is its length.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Landing {
    pub(crate) side: Side,
    pub(crate) before: usize,
}

/// The window's controls, for the painter.
pub(crate) struct Controls<'a> {
    pub(crate) tool_box: &'a Toolbar,
    pub(crate) commands: &'a Toolbar,
    pub(crate) bar: &'a ToolControls,
    pub(crate) swatches: &'a SwatchGrid,
    pub(crate) vertical: &'a ScrollBar,
    pub(crate) horizontal: &'a ScrollBar,
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
    document.picture().map_or(&NO_PICTURE, Picture::kind)
}

#[path = "view_input.rs"]
mod input;

#[path = "view_tools.rs"]
mod tools;

#[path = "view_layers.rs"]
mod layers;

#[path = "view_panes.rs"]
mod panes;

#[path = "view_adjust.rs"]
mod adjust;

#[path = "view_colour.rs"]
mod colour;

pub(crate) use input::close_rect;
pub(crate) use panes::landing_mark;
pub(crate) use tools::{screen_box, CROP_REACH, MARKER};

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
