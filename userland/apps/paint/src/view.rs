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

use tairix_abi::fs::Filetype;
use tairix_abi::window_ipc::{AppMenu, AppMenuItemId, CursorShape, DialogModality, SaveFormats};
use tairix_browse::vfs::write_document_title;
use tairix_colour::Rgba;
use tairix_controls::{
    ColourModel, ColourPicker, ControlRole, IconButton, Keystroke, PickerView, ScrollAction,
    ScrollBar, ScrollModel, ScrollOrientation, ScrollRange, SwatchGrid, SwatchMark, TitleBar,
    Toolbar, WellSpacing,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_icon::IconKind;
use tairix_image::{desktop_palette, IndexDepth, Rgba8};
use tairix_input::{InputEvent, Modifiers, PointerButton};
use tairix_parallel::JobRunner;
use tairix_raster::Color;
use tairix_reclaim::PressureBand;
use tairix_theme::Theme;
use tairix_window::docapp::{
    DialogWindow, DocumentView, SaveTarget, ToolGone, ToolMove, ToolOpening, ToolWindow,
};
use tairix_window::document::{Access, SavedDocument};
use tairix_window::menu::MenuCommand;

use crate::brush::Spray;
use crate::canvas::{Canvas, CanvasError, Kind, OutOfMemory, Tile};
use crate::colour::{Ink, BLACK, WHITE};
use crate::dialog::{Form, Purpose};
use crate::document::{Document, EntryId, Layer, NewPicture, Picture, Snapshot};
use crate::fill;
use crate::gradient::Gradient;
use crate::layers_pane::LayersPane;
use crate::layout::{Faces, Floor, Layout, Needs};
use crate::mask::{Combine, Mask, Recipe};
use crate::op::{Family, Halted, Op};
use crate::op_pane::OpPane;
use crate::pane::{Arrangement, PaneKind, Side};
use crate::preferences::{CanvasStyle, Grid, OpenAt, Preferences};
use crate::save::{format_for, natural, save_formats, survey, Loss, SaveFormat, SaveRefusal};
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

/// A new sprite's size to start.
pub(crate) const SPRITE_SIZE: (u32, u32) = (32, 32);

/// The most wells the palette strip holds: a 256-colour palette and the
/// clear ink of its mask.
pub const MOST_WELLS: usize = IndexDepth::Eight.colours() + 1;

/// How far a step of a bar scrolls the picture or a pane's body, in pixels.
pub(crate) const SCROLL_LINE: u64 = 16;

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
    /// The window's menu, opened by a secondary press anywhere in it or in
    /// one of its tool windows.
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
    /// Every tile `op` writes over `canvas` held to `clip`: those of `done`,
    /// which the preview worked out over the very same layer, kept, and the
    /// rest worked out — what an Apply lands when the preview it would adopt
    /// is incomplete.
    Op {
        /// The layer painted on, as it stood.
        canvas: Canvas,
        /// What is applied.
        op: Op,
        /// The selection it is held to, if one is.
        clip: Option<Mask>,
        /// What the preview had worked out of it.
        done: Vec<(usize, Arc<Tile>)>,
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
    /// Draw a sprite file window's thumbnails, each `side` pixels square.
    Thumbnails {
        /// The square's side.
        side: u32,
        /// Each sprite: its identity, its content revision, and its picture
        /// as it stood.
        pictures: Vec<(EntryId, u64, Picture)>,
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
    /// An operation's tiles, each the tile as it now stands.
    Op(Result<Vec<(usize, Arc<Tile>)>, Halted>),
    /// Thumbnails, each `side` pixels square of straight-alpha RGBA8 where
    /// one could be drawn.
    Thumbnails {
        /// The square's side.
        side: u32,
        /// Each sprite's identity, the revision it was drawn from, and its
        /// pixels.
        drawn: Vec<(EntryId, u64, Option<Vec<u8>>)>,
    },
}

/// Carry out `work`, an operation's blocks and a merge's or a flatten's tiles
/// spread across `runner`: what the worker runs.
#[must_use]
pub fn compute(work: Compute, runner: &dyn JobRunner) -> Computed {
    match work {
        Compute::Transform { layers } => Computed::Picture(transformed(&layers)),
        Compute::Compose { layers } => Computed::Composed(crate::compose::flatten(&layers, runner)),
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
        Compute::Op {
            canvas,
            op,
            clip,
            done,
        } => Computed::Op(crate::tiled::complete(
            &canvas,
            &op,
            clip.as_ref(),
            done,
            runner,
        )),
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
        Compute::Thumbnails { side, pictures } => Computed::Thumbnails {
            side,
            drawn: pictures
                .into_iter()
                .map(|(id, revision, picture)| {
                    let pixels = crate::thumb::thumbnail(&picture, side).map(|(pixels, _)| pixels);
                    (id, revision, pixels)
                })
                .collect(),
        },
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
    /// Have the preview worker work `work` out, answering job `job`: the
    /// window's newer work takes its place while it waits.
    Preview {
        /// Which job.
        job: u64,
        /// The window's preview ticket as the job was asked.
        stamp: crate::preview::Stamp,
        /// What.
        work: crate::preview::PreviewWork,
    },
    /// Open each of these sprites in a window of its own, or bring forward
    /// the one it has.
    OpenSprites(Vec<EntryId>),
    /// Bring forward the window of the sprite file this sprite window's
    /// sprite is in.
    ShowSpriteFile,
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
    /// Choose a tool.
    Tool(Tool),
    /// Magnify to a rung of the ladder.
    Zoom(usize),
    /// Open an operation in its pane: an entry of [`Op::every`].
    Adjust(usize),
    /// Show a pane of the chrome, or hide it.
    Pane(PaneKind),
    /// Put every pane back where a new window has it.
    ResetPanes,
    /// Float the palette in a tool window of its own, or dock it in its
    /// strip again.
    FloatPalette,
    /// Put the inks back to black and white.
    ResetColours,
    /// Take the next press on the picture as a colour for the ink the
    /// colour pane edits.
    PickColour,
    /// Open the sprites chosen in a sprite file window, each in a window of
    /// its own.
    EditEntry,
    /// Bring forward the sprite file window a sprite window's sprite is in.
    SpriteFile,
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
                | Self::FloatPalette
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
                | Self::FloatPalette
                | Self::ResetColours
                | Self::PickColour
        ) || self == Self::Tool(tool)
    }
}

/// The actions with no argument, by id: an action's position here is its
/// menu id, less one.
const PLAIN_ACTIONS: [Action; 61] = [
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
    Action::ResetPanes,
    Action::ResetColours,
    Action::PickColour,
    Action::EditEntry,
    Action::SpriteFile,
    Action::FloatPalette,
];

/// Where the argument-carrying families' ids start, and the one entry
/// field's own id.
const TOOL_IDS: u16 = 100;
const ZOOM_IDS: u16 = 200;
const RENAME_ID: u16 = 300;
const GO_TO_ENTRY: u16 = 301;
pub(crate) const RENAME_ENTRY: u16 = 302;
const GO_TO_LAYER: u16 = 303;
const OPERATION_IDS: u16 = 400;
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
            Self::Adjust(index) => at(OPERATION_IDS, Some(index)),
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
            OPERATION_IDS..PANE_IDS => {
                let index = usize::from(id - OPERATION_IDS);
                (index < Op::COUNT).then_some(Self::Adjust(index))
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

impl MenuCommand for Action {
    /// Everything done to the picture where it is, but not what opens a
    /// dialog, a pane, a field or another window, or what takes away the
    /// layer or entry being shown.
    fn acts_in_place(self) -> bool {
        !matches!(
            self,
            Self::NewPicture
                | Self::Open
                | Self::Save
                | Self::SaveAs
                | Self::Close
                | Self::Resize
                | Self::CanvasSize
                | Self::Convert
                | Self::EditPrimary
                | Self::EditSecondary
                | Self::NewEntry
                | Self::DeleteEntry
                | Self::GoTo
                | Self::Rename
                | Self::DeleteLayer
                | Self::GoToLayer
                | Self::Adjust(_)
                | Self::EditEntry
                | Self::SpriteFile
                | Self::FloatPalette
        )
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
    /// Paint laid down as the pointer moves along `path`, and for an
    /// airbrush as it is held (`spray`).
    Stroke {
        stroke: alloc::boxed::Box<Stroke>,
        path: crate::brush::Path,
        spray: Option<Spray>,
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
    /// An operation is applied, the pane `closes` names goes back to its
    /// operation's starting settings, and `then` follows.
    Adjusted {
        /// The pane whose open operation it was.
        closes: Option<PaneKind>,
        /// What follows.
        then: Then,
    },
}

/// What follows once a worker's answer has landed: a floating selection put
/// down, or an operation applied.
#[derive(Debug)]
enum Then {
    /// Nothing more.
    Rest,
    /// The action that waited for it.
    Act(Action),
    /// A pasted layer, floated in its place.
    Float(Floating),
    /// The save the user chose to make before the window closes.
    SaveThenClose,
    /// A menu's entry field committed: its id and the text it held.
    Enter(u16, String),
    /// A sprite window closing: its sprite is its file's, so nothing is asked.
    Close,
    /// Apply pressed in this pane while the other pane's operation was open.
    Apply(PaneKind),
    /// Operation `usize` of this pane's family chosen in place of its own.
    Choose(PaneKind, usize),
    /// This layer picked in the Layers pane to paint on.
    PaintOn(usize),
}

/// What a refusal leaves undone of the save that waited on it.
const SAVE_UNDONE: &str = "the picture was not saved";

/// What a refusal leaves undone of the save before the window closes that
/// waited on it.
const SAVE_CLOSE_UNDONE: &str = "the picture was not saved, and the window stays open";

/// What a refusal leaves undone of the close that waited on it.
const CLOSE_UNDONE: &str = "the window stays open";

impl Then {
    /// What follows and so is not done where the work it waits on is
    /// refused, named where the user would look for it: a save, a close.
    const fn undone(&self) -> Option<&'static str> {
        match self {
            Self::Act(Action::Save | Action::SaveAs) => Some(SAVE_UNDONE),
            Self::SaveThenClose => Some(SAVE_CLOSE_UNDONE),
            Self::Act(Action::Close) | Self::Close => Some(CLOSE_UNDONE),
            _ => None,
        }
    }
}

/// A form of settings the window asks in a dialog of its own.
#[derive(Debug)]
enum Modal {
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
    /// Whether the user chose to save before the window closes while it was
    /// out: once it, and what it was to be followed by, has landed, the
    /// window is saved and closed.
    closing: bool,
}

impl Pending {
    /// Whether a save that closes the window waits on it, as a quit then
    /// does.
    const fn closes(&self) -> bool {
        self.closing || self.lands.closes()
    }

    /// What waited on it and is not done where it is refused.
    const fn undone(&self) -> Option<&'static str> {
        if self.closing {
            Some(SAVE_CLOSE_UNDONE)
        } else {
            self.lands.undone()
        }
    }
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

impl Lands {
    /// The pane whose operation the answer applies, where it applies one.
    const fn operation(&self) -> Option<PaneKind> {
        match self {
            Self::Tiles(Settles::Adjusted { closes, .. }) => *closes,
            _ => None,
        }
    }

    /// Whether a save that closes the window follows the answer.
    const fn closes(&self) -> bool {
        match self {
            Self::Tiles(Settles::PutDown(then) | Settles::Adjusted { then, .. }) => {
                matches!(then, Then::SaveThenClose)
            }
            Self::Sheet { then_close } => *then_close,
            Self::Tiles(Settles::Nothing | Settles::Cleared)
            | Self::Selection
            | Self::Transform(_)
            | Self::Merged(_) => false,
        }
    }

    /// Whether the answer changes the document once it lands, which a close
    /// asks about: a selection made, or the Save As sheet's survey, does not.
    const fn changes(&self) -> bool {
        matches!(self, Self::Tiles(_) | Self::Transform(_) | Self::Merged(_))
    }

    /// What waited on the answer and is not done where it is refused.
    const fn undone(&self) -> Option<&'static str> {
        match self {
            Self::Tiles(Settles::PutDown(then) | Settles::Adjusted { then, .. }) => then.undone(),
            Self::Sheet { then_close: true } => Some(CLOSE_UNDONE),
            Self::Tiles(Settles::Nothing | Settles::Cleared)
            | Self::Selection
            | Self::Transform(_)
            | Self::Merged(_)
            | Self::Sheet { then_close: false } => None,
        }
    }
}

/// A palette entry being edited live from the colour dock: the palette it
/// had before, which settling records the change against as one step.
#[derive(Debug)]
pub(crate) struct PaletteEdit {
    pub(crate) entry: u8,
    pub(crate) before: Vec<Rgba8>,
}

/// What a sprite window (`plans/PAINT.md` PT33) holds of the sprite file
/// its one sprite is in.
#[derive(Debug)]
pub struct Member {
    /// The sprite's identity in its file.
    id: EntryId,
    /// What the file calls it.
    sprite: tairix_image::SpriteName,
    /// Whether the file holds changes it has not saved.
    file_modified: bool,
    /// The document's generation last handed to the file.
    handed: u64,
}

/// One painter window's state.
#[derive(Debug)]
pub struct View {
    document: Document,
    name: String,
    /// The filetype the volume records for the file the document came from,
    /// for as long as that is the file it is saved to.
    recorded: Option<Filetype>,
    /// The sprite file its one sprite is in, for a sprite window.
    member: Option<Member>,
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
    /// The client size, in logical pixels, each floating pane's tool window
    /// was resized to: window state, forgotten when the pane docks.
    chosen: [Option<(u32, u32)>; PaneKind::ALL.len()],
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
    /// The palette's own bar, which scrolls its wells along the strip or down
    /// a floating palette.
    palette_bar: ScrollBar,
    /// The control at the strip's right end that floats the palette.
    pop_out: IconButton,
    /// The well a middle press landed on, which its release over the same
    /// well makes the secondary ink.
    middle_well: Option<usize>,
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
    /// The Adjustments and the Effects panes, in [`PaneKind::OPERATIONS`]
    /// order: one at most has its operation open.
    operations: [OpPane; 2],
    /// What the picture shows of the open operation, and the histogram the
    /// panes read.
    looks: operations::Looks,
    /// The window's share of the preview worker: its ticket and its one job
    /// out.
    previews: previews::Previews,
    /// The Layers pane.
    layers: LayersPane,
    /// What the Layers pane shows beyond itself: the thumbnails, an opacity
    /// changed live and the composite beneath it.
    layer_looks: layers::LayerLooks,
    /// The bar each pane's body is scrolled by where its dock scrolls it, by
    /// [`PaneKind::index`].
    body_bars: [ScrollBar; PaneKind::ALL.len()],
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
    /// When the pointer was reported where it is, by its device.
    pointer_ns: u64,
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
    repeat_due: Option<u64>,
    /// The picture pixel under the pointer, which the status band states.
    hover: Option<(u32, u32)>,
    /// Where everything it draws goes, for its window's size.
    layout: Layout,
}

impl View {
    /// Whether the window holds a change no step records yet — an opacity or
    /// a palette entry changed live away from where it began, an operation
    /// open with its settings moved, or work out that changes the picture
    /// once it lands — which a close or a quit asks about as it asks about
    /// any other rather than closing over it.
    fn unrecorded(&self) -> bool {
        let palette = self
            .palette_edit
            .as_ref()
            .is_some_and(|edit| self.kind().palette() != Some(edit.before.as_slice()));
        let operation = self
            .open_operation()
            .is_some_and(|kind| self.operation_pane(kind).applying().is_some());
        let working = self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.lands.changes());
        palette || operation || working || self.opacity_changing()
    }
}

impl SavedDocument for View {
    type Snapshot = Snapshot;

    fn access(&self) -> Access {
        self.access
    }

    fn is_modified(&self) -> bool {
        self.document.is_modified() || self.held.is_some() || self.unrecorded()
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

    /// The file the Save As made is typed as the format its sheet chose,
    /// which the session recorded before the file was written.
    fn rename(&mut self, name: String) {
        self.name = name;
        self.recorded = self
            .save_as
            .take()
            .and_then(|format| tairix_browse::media::save_format(format.media()).filetype());
    }

    fn set_access(&mut self, access: Access) {
        self.access = access;
    }

    fn say(&mut self, message: String) {
        View::say(self, message);
    }
}

impl DocumentView for View {
    type Faces = Faces;
    type MenuKind = MenuKind;
    type Own = Own;

    fn name(&self) -> &str {
        View::name(self)
    }

    fn write_title(&self, title: &mut String) {
        View::write_title(self, title);
    }

    fn lay_out(
        &mut self,
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        faces: Faces,
        damage: &mut Region,
    ) {
        self.layout = self.layout_for(width, height, theme, scale, faces);
        self.settle(damage);
    }

    fn min_size(&self, theme: &Theme, scale: Scale, faces: Faces) -> (u32, u32) {
        View::min_size(self, theme, scale, faces)
    }

    fn message_area(&self) -> Rect {
        self.layout.message()
    }

    /// The Save As sheet of a save that closes the window, or the work
    /// that save waits on.
    fn asking_to_close(&self) -> bool {
        let sheet = matches!(
            &self.modal,
            Some(Modal::Form(form)) if form.purpose() == Purpose::SaveAs { then_close: true }
        );
        sheet || self.pending.as_ref().is_some_and(Pending::closes)
    }

    /// Busy while a worker has the picture; the arrow while a question or an
    /// open list holds the pointer; text entry over a number field; over the
    /// canvas, the hand where a press drags the view, text entry for the
    /// text tool, and the cross otherwise; the arrow elsewhere.
    fn cursor(&self, at: Point) -> CursorShape {
        let over_canvas = self.layout.canvas().contains(at);
        if over_canvas && self.panning() {
            CursorShape::Pointer
        } else if self.busy() {
            CursorShape::Busy
        } else if self.asking() || self.bar.listing() {
            CursorShape::Arrow
        } else if self.bar.text_at(self.layout.bar(), at) {
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

    fn tool_tip(&self, scale: Scale, theme: &Theme) -> Option<(Rect, &str)> {
        View::tool_tip(self, scale, theme)
    }

    fn menu(&self, kind: MenuKind) -> AppMenu {
        View::menu(self, kind)
    }

    fn close_requested(&mut self, damage: &mut Region) -> Outcome {
        View::close_requested(self, damage)
    }

    fn save_then_close(&mut self, damage: &mut Region) -> Outcome {
        View::save_then_close(self, damage)
    }

    fn chosen(&mut self, item: AppMenuItemId, damage: &mut Region) -> Outcome {
        View::chosen(self, item, damage)
    }

    fn entered(&mut self, item: AppMenuItemId, text: &str, damage: &mut Region) -> Outcome {
        View::entered(self, item, text, damage)
    }

    fn focus_changed(&mut self, focused: bool, damage: &mut Region) {
        View::focus_changed(self, focused, damage);
    }

    fn input(
        &mut self,
        input: &InputEvent,
        now_ns: u64,
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
                self.on_key(stroke, scale, theme, damage)
            }
            InputEvent::KeyReleased { key, .. } => {
                if key == tairix_input::Key::Char(' ') {
                    self.space = false;
                }
                Outcome::none()
            }
            _ => self.on_pointer(input, now_ns, scale, theme, damage),
        }
    }

    fn refuse_save(&self, name: &str, _: SaveTarget) -> Option<String> {
        self.save_format(name)
            .err()
            .map(|refusal| alloc::format!("{refusal}"))
    }

    fn ask_how(&mut self, then_close: bool, damage: &mut Region) -> Option<Outcome> {
        Some(self.ask_save_as(then_close, damage))
    }

    fn offered_extension(&self) -> &'static str {
        self.save_as
            .unwrap_or_else(|| natural(self.document.entries(), self.document.origin()))
            .extension()
    }

    fn save_formats(&self) -> Result<SaveFormats, String> {
        let entries = self.document.entries();
        let origin = self.document.origin();
        let format = self.save_as.unwrap_or_else(|| natural(entries, origin));
        save_formats(entries, origin, format).map_err(|refusal| alloc::format!("{refusal}"))
    }

    /// Each floating pane, in the rectangle of the drawing laid out for it,
    /// titled as its band is and sized as its layout allows.
    fn tool_window(&self, index: usize) -> Option<ToolWindow<'_>> {
        let slot = self.layout.floating().get(index)?;
        Some(ToolWindow {
            id: tool_id(slot.kind),
            title: self.headers[slot.kind.index()].title(),
            rect: slot.frame,
            sizing: slot.sizing,
        })
    }

    fn tool_opening(&mut self, id: u32) -> ToolOpening {
        View::tool_opening(self, id)
    }

    fn tool_moved(
        &mut self,
        moved: ToolMove,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        View::tool_moved(self, moved, scale, theme, damage)
    }

    fn tool_resized(
        &mut self,
        id: u32,
        size: (u32, u32),
        scale: Scale,
        damage: &mut Region,
    ) -> Outcome {
        View::tool_resized(self, id, size, scale, damage)
    }

    fn tool_gone(&mut self, id: u32, why: ToolGone, damage: &mut Region) -> Outcome {
        View::tool_gone(self, id, why, damage)
    }

    /// A form acts on the picture as it stood when the form opened, which
    /// its rows were filled from, so it holds the window until it is
    /// answered.
    fn dialog_window(&self, scale: Scale, theme: &Theme) -> Option<DialogWindow<'_>> {
        let form = self.modal()?;
        Some(DialogWindow {
            title: form.title(),
            rect: form.rect(self.layout.dialog_origin(), scale, theme),
            modality: DialogModality::OwnerModal,
        })
    }

    fn dialog_input(
        &mut self,
        input: &InputEvent,
        now_ns: u64,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        View::on_dialog(self, input, now_ns, (scale, theme), damage)
    }

    fn dialog_refused(&mut self, why: &str, damage: &mut Region) -> Outcome {
        View::dialog_refused(self, why, damage)
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

/// A size kept in logical pixels, in physical ones at `scale`.
fn physical(size: Option<(u32, u32)>, scale: Scale) -> Option<(u32, u32)> {
    let (width, height) = size?;
    Some((scale.scale_length(width), scale.scale_length(height)))
}

impl View {
    /// A window on `document`, called `name` and recorded by its volume as of
    /// `recorded`, which it may write as `access` says.
    #[must_use]
    pub fn new(
        document: Document,
        name: String,
        recorded: Option<Filetype>,
        access: Access,
    ) -> Self {
        let flat = || ScrollModel::new(ScrollRange::new(0, 0, 0), 1, 1);
        let kind = picture_kind(&document);
        let tool = Tool::Brush;
        let options = Options::default();
        let smooth = kind.sample_bytes() == 4;
        let aspect = document.picture().map_or((1, 1), Picture::pixel_aspect);
        let mut view = Self {
            document,
            name,
            recorded,
            member: None,
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
            chosen: [None; PaneKind::ALL.len()],
            commands: view_strip(true),
            bar: ToolControls::new(tool, options, smooth),
            wells: Vec::new(),
            swatches: SwatchGrid::new(1, Vec::new()).with_spacing(WellSpacing::Packed),
            picker: ColourPicker::new(Rgba::from_array(BLACK)),
            colour_controls: colour::colour_panel(PickerView::Square, ColourModel::Rgb),
            recents: Vec::new(),
            recent_grid: colour::recent_grid(),
            picking_colour: false,
            editing: SwatchMark::Primary,
            palette_edit: None,
            vertical: ScrollBar::new(ScrollOrientation::Vertical, flat()),
            horizontal: ScrollBar::new(ScrollOrientation::Horizontal, flat()),
            palette_bar: ScrollBar::new(ScrollOrientation::Horizontal, flat()),
            pop_out: IconButton::new(IconKind::PopOut, ControlRole::Neutral),
            middle_well: None,
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
            operations: [
                OpPane::new(Family::Adjustments),
                OpPane::new(Family::Effects),
            ],
            looks: operations::Looks::default(),
            previews: previews::Previews::default(),
            layers: LayersPane::new(),
            layer_looks: layers::LayerLooks::default(),
            body_bars: [ScrollBar::new(ScrollOrientation::Vertical, flat()); PaneKind::ALL.len()],
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
            pointer_ns: 0,
            modifiers: Modifiers::default(),
            zoom_carry: 0,
            pinch: None,
            message: None,
            modal: None,
            save_as: None,
            pending: None,
            next_job: 1,
            repeat_due: None,
            hover: None,
            layout: Layout::default(),
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

    /// Write what the window's title reads over `title`, reusing its room: a
    /// sprite window names its sprite and the file it is in, marked as that
    /// file is.
    pub fn write_title(&self, title: &mut String) {
        let read_only = self.access == Access::ReadOnly;
        if let Some(member) = &self.member {
            let shown = alloc::format!("{} in {}", member.sprite, self.name);
            write_document_title(title, &shown, member.file_modified, read_only, APP_TITLE);
        } else {
            let modified = SavedDocument::is_modified(self);
            write_document_title(title, &self.name, modified, read_only, APP_TITLE);
        }
    }

    /// A sprite window over `document`, the one sprite `id` of the sprite file
    /// `file`, which calls it `sprite` and is written as `access` allows.
    #[must_use]
    pub fn sprite_window(
        document: Document,
        (file, file_modified): (String, bool),
        (id, sprite): (EntryId, tairix_image::SpriteName),
        access: Access,
    ) -> Self {
        let mut view = Self::new(document, file, None, access);
        view.member = Some(Member {
            id,
            sprite,
            file_modified,
            handed: view.document.generation(),
        });
        view
    }

    /// The sprite this sprite window edits, by its identity in its file.
    #[must_use]
    pub fn member(&self) -> Option<EntryId> {
        self.member.as_ref().map(|member| member.id)
    }

    /// The sprite as it now stands, where it changed since it was last
    /// handed to its file; `None` for no change, for a window that is no
    /// sprite window, or where memory is refused — tried again on the next
    /// change.
    pub fn take_edit(&mut self) -> Option<Picture> {
        let generation = self.document.generation();
        let member = self
            .member
            .as_mut()
            .filter(|member| member.handed != generation)?;
        let copy = self.document.picture()?.try_clone().ok()?;
        member.handed = generation;
        Some(copy)
    }

    /// The sprite file `file` calls this window's sprite `sprite`, and holds
    /// unsaved changes when `modified`: answers whether the title changed.
    pub fn set_member(
        &mut self,
        sprite: tairix_image::SpriteName,
        file: &str,
        modified: bool,
    ) -> bool {
        let Some(member) = self.member.as_mut() else {
            return false;
        };
        let changed =
            member.sprite != sprite || member.file_modified != modified || self.name != file;
        member.sprite = sprite;
        member.file_modified = modified;
        if self.name != file {
            self.name = String::from(file);
        }
        changed
    }

    /// The tool in use.
    #[must_use]
    pub const fn tool(&self) -> Tool {
        self.tool
    }

    /// The format the document is written in under `name`: its own file's
    /// recorded filetype counts only for a save to that file.
    ///
    /// # Errors
    ///
    /// [`SaveRefusal`] where it cannot be written so; nothing is written.
    pub fn save_format(&self, name: &str) -> Result<SaveFormat, SaveRefusal> {
        let recorded = self.recorded.filter(|_| name == self.name);
        format_for(
            name,
            recorded,
            self.document.entries(),
            self.document.origin(),
        )
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
        self.pixel_grid_shown_from(self.style.pixel_grid_from)
    }

    /// Whether the grid between pixels is drawn when it shows from zoom
    /// `from`, in percent.
    fn pixel_grid_shown_from(&self, from: u32) -> bool {
        let (across, down) = self.viewport.pixel_span();
        let from = u64::from(from);
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
    pub fn adopt(&mut self, preferences: &Preferences, damage: &mut Region) {
        let was = core::mem::replace(&mut self.style, preferences.canvas_style());
        if was != self.style {
            self.restyled(&was, damage);
        }
        self.home_panes.clone_from(&preferences.panes);
        self.new_picture = (preferences.new, preferences.format);
    }

    /// Report what drawing with the style in force rather than `was`
    /// changes: the surround about the picture, and the picture itself only
    /// where its checkerboard, or a grid this window shows, is drawn
    /// differently. Whether a new window shows the grid, and snapping, draw
    /// nothing.
    fn restyled(&self, was: &CanvasStyle, damage: &mut Region) {
        let now = &self.style;
        let picture = self.document.picture().map_or(Rect::EMPTY, |picture| {
            let (width, height) = picture.size();
            self.screen(Bounds::picture(width, height))
        });
        if now.surround != was.surround {
            let mut around = Region::new();
            around.add(self.layout.canvas());
            around.subtract(picture);
            for &band in around.rects() {
                damage.add(band);
            }
        }
        let drawn = |grid: Grid| Grid {
            shown: false,
            snap: false,
            ..grid
        };
        let redrawn = (now.checker_side, now.shades) != (was.checker_side, was.shades)
            || self.pixel_grid_shown_from(was.pixel_grid_from) != self.pixel_grid_shown()
            || (self.grid_shown() && drawn(now.grid) != drawn(was.grid));
        if redrawn {
            damage.add(picture);
        }
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
            palette_bar: &self.palette_bar,
            pop_out: &self.pop_out,
        }
    }

    /// The modal form showing, if any.
    #[must_use]
    pub(crate) fn modal(&self) -> Option<&Form> {
        match &self.modal {
            Some(Modal::Form(form)) => Some(form),
            None => None,
        }
    }

    /// Whether a question is showing.
    #[must_use]
    pub const fn asking(&self) -> bool {
        self.modal.is_some()
    }

    /// Where everything it draws goes, for its window's size.
    #[must_use]
    pub const fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The layout of a `width`×`height` window.
    fn layout_for(
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
        let count = self
            .document
            .picture()
            .map_or(0, |picture| picture.layers().len());
        let operation =
            |kind: PaneKind| self.operation_pane(kind).heights(dock, faces, scale, theme);
        let needs = Needs {
            wells: self.wells.len(),
            clear: self.wells.last() == Some(&Ink::Clear),
            picker: self.picker.measured_height(dock, scale, theme),
            colour_controls: self
                .colour_controls
                .measured_height(dock, faces, scale, theme),
            recents: dock / u32::try_from(colour::RECENT_COLUMNS).unwrap_or(1) * 2,
            tool_box: self.tool_box.breadth(scale, theme),
            tool_box_length: self.tool_box.natural_length(scale, theme),
            tool_box_least: self.tool_box.min_length(scale, theme),
            layers: self.layers.measures(count, dock, faces, scale, theme),
            operations: PaneKind::OPERATIONS.map(operation),
            view_strip,
            // As many rows as any tool's bar takes, so the canvas stays put
            // whichever tool is chosen.
            bar_rows: ToolControls::most_rows(controls, faces, scale, theme),
            chosen: self.chosen.map(|size| physical(size, scale)),
        };
        let mut layout = Layout::for_window(width, height, theme, scale, (needs, &self.panes));
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
        Layout::min_size(theme, scale, floor, |width| {
            ToolControls::most_rows(width, faces, scale, theme)
        })
    }

    /// The tip for what the pointer is over — a tool, a view command, the
    /// palette's pop-out or a setting — with its rectangle.
    #[must_use]
    pub fn tool_tip(&self, scale: Scale, theme: &Theme) -> Option<(Rect, &'static str)> {
        let tools = self.layout.tools();
        if let Some(index) = self.tool_box.tool_at(tools, scale, theme, self.pointer) {
            let rect = self.tool_box.tool_rect(index, tools, scale, theme)?;
            return Some((rect, Tool::ALL.get(index)?.label()));
        }
        let commands = self.layout.view_strip();
        if let Some(index) = self.commands.tool_at(commands, scale, theme, self.pointer) {
            let rect = self.commands.tool_rect(index, commands, scale, theme)?;
            return Some((rect, VIEW_COMMANDS.get(index)?.2));
        }
        let pop_out = self.layout.pop_out();
        if pop_out.contains(self.pointer) {
            return Some((pop_out, "Float the palette in a window of its own"));
        }
        self.bar.tip(self.layout.bar(), self.pointer)
    }

    fn picture_size(&self) -> (u32, u32) {
        self.document.picture().map_or((1, 1), Picture::size)
    }

    /// The layer painted on.
    fn active_layer(&self) -> usize {
        self.document.picture().map_or(0, Picture::active)
    }

    /// Bring the scroll, the bars and the palette's columns and numbers into
    /// line with the layout, forgetting the size of a pane no longer floating;
    /// a preview held back for want of memory is asked again, as what it asks
    /// of the window laid out anew may fit.
    fn settle(&mut self, damage: &mut Region) {
        if core::mem::take(&mut self.open_fitted) {
            let rung = self
                .viewport
                .fitting(self.picture_size(), self.layout.canvas())
                .min(ACTUAL);
            let _ = self.viewport.zoom_to(
                rung,
                self.layout.canvas().center(),
                self.picture_size(),
                self.layout.canvas(),
            );
            damage.add(self.layout.canvas());
        }
        if self.swatches.columns() != self.layout.columns() {
            self.swatches.set_columns(self.layout.columns());
            damage.add(self.layout.palette());
        }
        let clear = usize::from(self.wells.last() == Some(&Ink::Clear));
        let numbered = self.wells.len().saturating_sub(clear);
        if self
            .swatches
            .number_wells(numbered, self.layout.number_face())
        {
            damage.add(self.layout.palette());
        }
        for kind in PaneKind::ALL {
            if !self.panes.floats(kind) {
                self.chosen[kind.index()] = None;
            }
        }
        self.settle_palette_bar(damage);
        let size = self.picture_size();
        let area = self.layout.canvas();
        if self.viewport.settle(size, area) {
            damage.add(area);
        }
        let (width, height) = self.viewport.extent(size);
        let (x, y) = self.viewport.scroll();
        let model = |content: u64, room: u32, offset: u64| {
            ScrollModel::in_pixels(
                ScrollRange::new(content.max(u64::from(room)), u64::from(room), offset),
                SCROLL_LINE,
            )
        };
        let vertical = model(height, area.height, y);
        let horizontal = model(width, area.width, x);
        if vertical != self.vertical.model() {
            self.vertical.set_model(vertical);
            damage.add(self.layout.vertical_bar());
        }
        if horizontal != self.horizontal.model() {
            self.horizontal.set_model(horizontal);
            damage.add(self.layout.horizontal_bar());
        }
        // The picture may have moved, or changed, under a pointer that did not.
        self.hovered(damage);
        self.settle_float_homes();
        self.settle_body_bars(damage);
        if self.layers.settle(self.layout.layers()) {
            damage.add(self.layout.layers().rows);
        }
        self.sync_layers(damage);
        self.lift_short_hold();
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
        // The columns the wells are set in follow the layout, which settles
        // them.
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
    fn screen(&self, bounds: Bounds) -> Rect {
        self.viewport
            .to_screen(bounds, self.picture_size(), self.layout.canvas())
    }

    /// Report `bounds` of the picture as damaged.
    fn damage_picture(&self, bounds: Option<Bounds>, damage: &mut Region) {
        if let Some(bounds) = bounds {
            damage.add(self.screen(grown(bounds, 1)));
            // The status band states the colour under the pointer.
            let under = self.hover.is_some_and(|(x, y)| {
                let (x, y) = (i64::from(x), i64::from(y));
                (bounds.x0..bounds.x1).contains(&x) && (bounds.y0..bounds.y1).contains(&y)
            });
            if under {
                damage.add(self.layout.position());
            }
        }
    }

    /// The window gained or lost the keyboard; losing it ends any drag, lets
    /// the Space held go, and lands an opacity changing or a name being typed.
    pub fn focus_changed(&mut self, focused: bool, damage: &mut Region) {
        self.headers_follow_focus(focused, damage);
        if !focused {
            self.space = false;
            self.end_gesture(damage);
            self.settle_opacity(damage);
            self.settle_rename(damage);
            self.pane_drag = None;
            self.band_grab = None;
        }
    }

    /// Size the history for memory pressure `band`, ask again for a preview
    /// held back for want of memory once the pressure falls, and from severe
    /// pressure on let the preview's overlay and the composite beneath an
    /// opacity changing go.
    pub fn adopt_pressure(&mut self, band: PressureBand, damage: &mut Region) {
        self.document.adopt_pressure(band);
        self.previews_pressured(band);
        if band >= PressureBand::Severe {
            self.drop_overlay(damage);
            self.drop_under();
        }
    }

    /// When the window next needs waking, if it does.
    #[must_use]
    pub fn deadline_ns(&self) -> Option<u64> {
        match (self.spray().map(Spray::due_ns), self.repeat_due) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (due, None) | (None, due) => due,
        }
    }

    /// The held airbrush's own clock, while it is held.
    fn spray(&self) -> Option<&Spray> {
        match &self.gesture {
            Some(Gesture::Stroke { spray, .. }) => spray.as_ref(),
            _ => None,
        }
    }

    /// Arm the deadlines the state calls for, now it is `now_ns`.
    pub fn arm_deadline(&mut self, now_ns: u64, scale: Scale, theme: &Theme) {
        let bodies = self
            .body_bars
            .iter_mut()
            .filter_map(|bar| bar.repeat_deadline(now_ns))
            .min();
        self.repeat_due = [
            self.tool_box
                .repeat_deadline(now_ns, self.layout.tools(), scale, theme),
            self.commands
                .repeat_deadline(now_ns, self.layout.view_strip(), scale, theme),
            self.vertical.repeat_deadline(now_ns),
            self.horizontal.repeat_deadline(now_ns),
            self.palette_bar.repeat_deadline(now_ns),
            self.layers.repeat_deadline(now_ns),
            bodies,
        ]
        .into_iter()
        .flatten()
        .min();
    }

    /// Do what has fallen due by `now_ns`: another dab of a held airbrush,
    /// another step of a held control.
    pub fn tick(&mut self, now_ns: u64, scale: Scale, theme: &Theme, damage: &mut Region) {
        if self.spray().is_some_and(|spray| spray.due_ns() <= now_ns) {
            self.airbrush(now_ns, damage);
        }
        self.tool_box
            .repeat(now_ns, self.layout.tools(), scale, theme, damage);
        self.commands
            .repeat(now_ns, self.layout.view_strip(), scale, theme, damage);
        if let Some(ScrollAction::ScrollTo { offset }) =
            self.vertical
                .repeat(now_ns, self.layout.vertical_bar(), scale, theme, damage)
        {
            self.scroll_to(None, Some(offset), damage);
        }
        if let Some(ScrollAction::ScrollTo { offset }) =
            self.horizontal
                .repeat(now_ns, self.layout.horizontal_bar(), scale, theme, damage)
        {
            self.scroll_to(Some(offset), None, damage);
        }
        if let Some(ScrollAction::ScrollTo { offset }) =
            self.palette_bar
                .repeat(now_ns, self.layout.palette_bar(), scale, theme, damage)
        {
            self.scroll_palette(offset, damage);
        }
        self.layers
            .repeat(now_ns, self.layout.layers(), scale, theme, damage);
        self.repeat_body_bars(now_ns, scale, theme, damage);
    }

    fn scroll_to(&mut self, x: Option<u64>, y: Option<u64>, damage: &mut Region) {
        let (sx, sy) = self.viewport.scroll();
        if self.viewport.scroll_to(
            x.unwrap_or(sx),
            y.unwrap_or(sy),
            self.picture_size(),
            self.layout.canvas(),
        ) {
            damage.add(self.layout.canvas());
        }
        self.settle(damage);
    }

    /// Magnify to rung `zoom`, keeping the picture point under `anchor`.
    fn zoom_to(&mut self, zoom: usize, anchor: Point, damage: &mut Region) {
        let area = self.layout.canvas();
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
            damage.add(self.layout.zoom());
        }
        self.settle(damage);
    }

    fn fit(&mut self, damage: &mut Region) {
        let rung = self
            .viewport
            .fitting(self.picture_size(), self.layout.canvas());
        self.zoom_to(rung, self.layout.canvas().center(), damage);
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

/// Where a dragged pane lands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Landing {
    /// Before the pane at `before` down `side`, or at the dock's foot where
    /// `before` is its length.
    Dock {
        /// The side whose dock it lands in.
        side: Side,
        /// Where down that dock.
        before: usize,
    },
    /// The palette's strip, beneath the canvas.
    Strip,
}

/// The window's controls, for the painter.
pub(crate) struct Controls<'a> {
    pub(crate) tool_box: &'a Toolbar,
    pub(crate) commands: &'a Toolbar,
    pub(crate) bar: &'a ToolControls,
    pub(crate) swatches: &'a SwatchGrid,
    pub(crate) vertical: &'a ScrollBar,
    pub(crate) horizontal: &'a ScrollBar,
    pub(crate) palette_bar: &'a ScrollBar,
    pub(crate) pop_out: &'a IconButton,
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

#[path = "view_operations.rs"]
mod operations;

#[path = "view_previews.rs"]
mod previews;

#[path = "view_colour.rs"]
mod colour;

pub(crate) use panes::landing_mark;
pub(crate) use tools::{screen_box, CROP_REACH, MARKER};

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
