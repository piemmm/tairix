//! The pictures of what a command does.
//!
//! [`menu_symbol_icon`] is the one map from a menu row's function onto the
//! [`IconKind`] that pictures it, and the command kinds are drawn here as
//! [`symbol`](crate::symbol) marks: stroked two units wide with round caps
//! and joins on the 24-unit grid, solid only where a feature is too small to
//! outline, so they read alike from a menu row to a toolbar.

use tairix_abi::window_ipc::AppMenuSymbol;

use crate::glyph::IconKind;
use crate::symbol::{circle, cut, fill, stroke, Mark, Outline};

/// The stroke every command picture is drawn with, in symbol units.
const W: f64 = 2.0;

/// The picture a menu row doing `symbol` draws.
///
/// Total over the vocabulary. Several functions share a picture where they
/// are one gesture on different things — every way to play draws the play
/// mark — but each function has exactly one.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one arm a function; splitting it would part the vocabulary from its one map"
)]
pub const fn menu_symbol_icon(symbol: AppMenuSymbol) -> IconKind {
    use AppMenuSymbol as S;
    match symbol {
        S::Open => IconKind::Open,
        S::OpenWith => IconKind::OpenWith,
        S::OpenFolder | S::Folder => IconKind::Folder,
        S::Application => IconKind::AppBundle,
        S::NewWindow => IconKind::NewWindow,
        S::NewDocument => IconKind::NewDocument,
        S::NewFolder => IconKind::NewFolder,
        S::Save => IconKind::Save,
        S::SaveAs => IconKind::SaveAs,
        S::Close => IconKind::Close,
        S::Quit => IconKind::Quit,
        S::Info => IconKind::Info,
        S::Properties => IconKind::Properties,
        S::Settings => IconKind::Settings,
        S::File => IconKind::Page,
        S::Edit => IconKind::Edit,
        S::View | S::Visible => IconKind::Visible,
        S::Hidden => IconKind::Hidden,
        S::Cut => IconKind::Cut,
        S::Copy => IconKind::Copy,
        S::Paste => IconKind::Paste,
        S::SelectAll => IconKind::SelectAll,
        S::SelectNone => IconKind::SelectNone,
        S::Rename => IconKind::Rename,
        S::Duplicate => IconKind::Duplicate,
        S::Delete => IconKind::Delete,
        S::Remove => IconKind::Remove,
        S::Clear => IconKind::Clear,
        S::Undo => IconKind::Undo,
        S::Redo => IconKind::Redo,
        S::Indent => IconKind::Indent,
        S::Outdent => IconKind::Outdent,
        S::Comment => IconKind::Comment,
        S::Overwrite => IconKind::Overwrite,
        S::Find => IconKind::Search,
        S::Replace => IconKind::Replace,
        S::FindNext => IconKind::FindNext,
        S::FindPrevious => IconKind::FindPrevious,
        S::GoTo => IconKind::GoTo,
        S::NextProblem => IconKind::Problem,
        S::ViewText => IconKind::ViewText,
        S::ViewHex => IconKind::ViewHex,
        S::ViewData => IconKind::ViewData,
        S::ViewCode => IconKind::ViewCode,
        S::InstructionSet => IconKind::Caching,
        S::Syntax => IconKind::Syntax,
        S::TabWidth => IconKind::TabWidth,
        S::LineEndings => IconKind::LineEndings,
        S::ZoomIn => IconKind::ZoomIn,
        S::ZoomOut => IconKind::ZoomOut,
        S::ZoomFit => IconKind::ZoomFit,
        S::FitWidth => IconKind::FitWidth,
        S::ZoomActual => IconKind::ZoomActual,
        S::ZoomLevel | S::ToolZoom => IconKind::ToolZoom,
        S::TextLarger => IconKind::TextLarger,
        S::TextSmaller => IconKind::TextSmaller,
        S::TextActual => IconKind::TextActual,
        S::RotateLeft => IconKind::RotateLeft,
        S::RotateRight => IconKind::RotateRight,
        S::RotateHalf => IconKind::RotateHalf,
        S::FlipHorizontal => IconKind::Mirror,
        S::FlipVertical => IconKind::FlipVertical,
        S::Grid => IconKind::Grid,
        S::PixelGrid => IconKind::PixelGrid,
        S::Panes => IconKind::Panes,
        S::ResetPanes => IconKind::ResetPanes,
        S::Display => IconKind::Layout,
        S::LargeIcons => IconKind::LargeIcons,
        S::SmallIcons => IconKind::SmallIcons,
        S::List => IconKind::ListView,
        S::Refresh => IconKind::Refresh,
        S::SortByName => IconKind::SortByName,
        S::SortByKind => IconKind::SortByKind,
        S::SortBySize => IconKind::SortBySize,
        S::SortByDate | S::SetDateTime => IconKind::Calendar,
        S::ArrangeLeft => IconKind::ArrangeLeft,
        S::ArrangeRight => IconKind::ArrangeRight,
        S::Wallpaper => IconKind::Wallpaper,
        S::Shortcut => IconKind::Shortcut,
        S::Home | S::BringHome => IconKind::Home,
        S::Drive => IconKind::Disk,
        S::Previous => IconKind::NavBack,
        S::Next => IconKind::NavForward,
        S::MoveUp => IconKind::MoveUp,
        S::MoveDown => IconKind::MoveDown,
        S::Layers => IconKind::Layers,
        S::LayerNew => IconKind::LayerNew,
        S::LayerAbove => IconKind::LayerAbove,
        S::LayerBelow => IconKind::LayerBelow,
        S::LayerRaise => IconKind::LayerRaise,
        S::LayerLower => IconKind::LayerLower,
        S::MergeDown => IconKind::MergeDown,
        S::Flatten => IconKind::Flatten,
        S::Image => IconKind::Image,
        S::Resize => IconKind::Resize,
        S::CanvasSize => IconKind::CanvasSize,
        S::InvertColours => IconKind::Invert,
        S::ColourModel => IconKind::ColourModel,
        S::MaskAdd => IconKind::MaskAdd,
        S::MaskRemove => IconKind::MaskRemove,
        S::FillSelection | S::ToolFill => IconKind::ToolFill,
        S::CropToSelection | S::ToolCrop => IconKind::ToolCrop,
        S::PutDown => IconKind::PutDown,
        S::Erase | S::ToolEraser => IconKind::ToolEraser,
        S::Colours => IconKind::Theme,
        S::ColourPrimary => IconKind::ColourPrimary,
        S::ColourSecondary => IconKind::ColourSecondary,
        S::ColourSwap => IconKind::ColourSwap,
        S::ColourReset => IconKind::ColourReset,
        S::PickColour | S::ToolEyedropper => IconKind::ToolEyedropper,
        S::Adjust => IconKind::Sliders,
        S::Brightness => IconKind::Brightness,
        S::HueSaturation => IconKind::Droplet,
        S::ColourBalance => IconKind::ColourBalance,
        S::Levels => IconKind::Levels,
        S::Curves => IconKind::Curves,
        S::WhiteBalance => IconKind::WhiteBalance,
        S::Posterise => IconKind::Posterise,
        S::Threshold => IconKind::Threshold,
        S::Desaturate => IconKind::Desaturate,
        S::Blur => IconKind::Blur,
        S::Sharpen => IconKind::Sharpen,
        S::Pixelate => IconKind::Pixelate,
        S::Noise => IconKind::Noise,
        S::FindEdges => IconKind::FindEdges,
        S::Pages => IconKind::Pages,
        S::Tools => IconKind::Toolbox,
        S::ToolSelect => IconKind::ToolSelect,
        S::ToolPencil => IconKind::ToolPencil,
        S::ToolBrush => IconKind::ToolBrush,
        S::ToolAirbrush => IconKind::ToolSpray,
        S::ToolClone => IconKind::ToolClone,
        S::ToolGradient => IconKind::ToolGradient,
        S::ToolText => IconKind::ToolText,
        S::ToolLine => IconKind::ToolLine,
        S::ToolRectangle => IconKind::ToolRectangle,
        S::ToolEllipse => IconKind::ToolEllipse,
        S::ToolPolygon => IconKind::ToolPolygon,
        S::ToolHand => IconKind::ToolHand,
        S::SystemMonitor => IconKind::Activity,
        S::TaskShell => IconKind::Terminal,
        S::LockScreen => IconKind::LockScreen,
        S::SwitchUser => IconKind::Users,
        S::LogOut => IconKind::LogOut,
        S::Restart => IconKind::Restart,
        S::ShutDown => IconKind::Power,
        S::Clock => IconKind::DateTime,
        S::Network => IconKind::Networking,
        S::NetworkActivity => IconKind::TcpIp,
        S::SwitchTo => IconKind::SwitchTo,
        S::Pause => IconKind::Pause,
        S::Resume | S::Play => IconKind::Resume,
        S::LowerPriority => IconKind::Gauge,
        S::Logs => IconKind::Logs,
        S::ForceQuit => IconKind::ForceQuit,
        S::Shuffle => IconKind::Shuffle,
        S::Repeat | S::RepeatAll => IconKind::Repeat,
        S::RepeatOff => IconKind::RepeatOff,
        S::RepeatOne => IconKind::RepeatOne,
        S::Loudness => IconKind::Loudness,
        S::AudioOutput => IconKind::Sound,
        S::NewGame => IconKind::Flag,
        S::LevelEasy => IconKind::LevelEasy,
        S::LevelMedium => IconKind::LevelMedium,
        S::LevelHard => IconKind::LevelHard,
        S::QuestionMarks => IconKind::AlertQuestion,
        S::LetOut => IconKind::DoorOpen,
    }
}

/// The marks a command kind is drawn with, or `None` for a kind drawn
/// elsewhere.
#[allow(
    clippy::too_many_lines,
    reason = "one arm a kind; splitting it would part the kinds from the one table they are drawn by"
)]
pub(crate) const fn marks(kind: IconKind) -> Option<&'static [Mark]> {
    Some(match kind {
        IconKind::Refresh => REFRESH,
        IconKind::ZoomActual => ONE_TO_ONE,
        IconKind::Info => INFO,
        IconKind::Open => OPEN,
        IconKind::OpenWith => OPEN_WITH,
        IconKind::NewWindow => NEW_WINDOW,
        IconKind::NewDocument => NEW_DOCUMENT,
        IconKind::Save => SAVE,
        IconKind::SaveAs => SAVE_AS,
        IconKind::Close => CLOSE,
        IconKind::Quit => QUIT,
        IconKind::Properties => PROPERTIES,
        IconKind::Page => PAGE,
        IconKind::Edit => EDIT,
        IconKind::Visible => EYE,
        IconKind::Hidden => EYE_STRUCK,
        IconKind::Cut => SCISSORS,
        IconKind::Copy => COPY,
        IconKind::Paste => CLIPBOARD,
        IconKind::SelectAll => SELECT_ALL,
        IconKind::SelectNone => SELECT_NONE,
        IconKind::Rename => RENAME,
        IconKind::Duplicate => DUPLICATE,
        IconKind::Delete => BIN,
        IconKind::Remove => MINUS_RING,
        IconKind::Clear => ERASER,
        IconKind::Undo => UNDO,
        IconKind::Redo => REDO,
        IconKind::Indent => INDENT,
        IconKind::Outdent => OUTDENT,
        IconKind::Comment => SLASHES,
        IconKind::Overwrite => BLOCK_CARET,
        IconKind::Search => MAGNIFIER,
        IconKind::Replace => REPLACE,
        IconKind::FindNext => FIND_NEXT,
        IconKind::FindPrevious => FIND_PREVIOUS,
        IconKind::GoTo => NUMBER_SIGN,
        IconKind::Problem => TRIANGLE,
        IconKind::ViewText => TEXT_LINES,
        IconKind::ViewHex => BINARY,
        IconKind::ViewData => BRACES,
        IconKind::ViewCode => ANGLES,
        IconKind::Syntax => PAGE_OF_CODE,
        IconKind::TabWidth => TAB_STOP,
        IconKind::LineEndings => PILCROW,
        IconKind::FitWidth => FIT_WIDTH,
        IconKind::TextLarger => TEXT_LARGER,
        IconKind::TextSmaller => TEXT_SMALLER,
        IconKind::TextActual => TEXT_ACTUAL,
        IconKind::RotateHalf => HALF_TURN,
        IconKind::FlipVertical => FLIP_VERTICAL,
        IconKind::Grid => GRID,
        IconKind::Panes => PANES,
        IconKind::ResetPanes => TURN_BACK,
        IconKind::Layout => LAYOUT,
        IconKind::LargeIcons => LARGE_TILES,
        IconKind::SmallIcons => SMALL_TILES,
        IconKind::ListView => LIST,
        IconKind::SortByName => SORT_BY_NAME,
        IconKind::SortByKind => SHAPES,
        IconKind::SortBySize => SORT_BY_SIZE,
        IconKind::Calendar => CALENDAR,
        IconKind::ArrangeLeft => ARRANGE_LEFT,
        IconKind::ArrangeRight => ARRANGE_RIGHT,
        IconKind::Shortcut => SHORTCUT,
        IconKind::Home => HOUSE,
        IconKind::MoveUp => ARROW_UP,
        IconKind::MoveDown => ARROW_DOWN,
        IconKind::Layers => LAYERS,
        IconKind::LayerNew => LAYER_NEW,
        IconKind::LayerAbove => LAYER_ABOVE,
        IconKind::LayerBelow => LAYER_BELOW,
        IconKind::LayerRaise => LAYER_RAISE,
        IconKind::LayerLower => LAYER_LOWER,
        IconKind::MergeDown => MERGE_DOWN,
        IconKind::Flatten => FLATTEN,
        IconKind::Resize => RESIZE,
        IconKind::CanvasSize => CANVAS_SIZE,
        IconKind::Invert => INVERT,
        IconKind::ColourModel => THREE_CIRCLES,
        IconKind::MaskAdd => MASK_ADD,
        IconKind::MaskRemove => MASK_REMOVE,
        IconKind::PutDown => ANCHOR,
        IconKind::ColourPrimary => COLOUR_PRIMARY,
        IconKind::ColourSecondary => COLOUR_SECONDARY,
        IconKind::ColourSwap => SWAP,
        IconKind::ColourReset => COLOUR_RESET,
        IconKind::Sliders => SLIDERS,
        IconKind::Brightness => SUN,
        IconKind::Droplet => DROPLET,
        IconKind::ColourBalance => BALANCE,
        IconKind::Levels => HISTOGRAM,
        IconKind::Curves => CURVE,
        IconKind::WhiteBalance => THERMOMETER,
        IconKind::Posterise => STAIRS,
        IconKind::Threshold => HALF_FRAME,
        IconKind::Desaturate => DROPLET_STRUCK,
        IconKind::Blur => DOT_RING,
        IconKind::Sharpen => SPARK,
        IconKind::Pixelate => CHEQUER,
        IconKind::Noise => SCATTER,
        IconKind::FindEdges => EDGES,
        IconKind::Pages => PAGES,
        IconKind::Toolbox => TOOLBOX,
        IconKind::Activity => PULSE,
        IconKind::Terminal => PROMPT,
        IconKind::LogOut => LOG_OUT,
        IconKind::Restart => RESTART,
        IconKind::SwitchTo => SWITCH_TO,
        IconKind::Gauge => GAUGE,
        IconKind::Logs => LOG_PAGE,
        IconKind::ForceQuit => OCTAGON_CROSS,
        IconKind::RepeatOff => REPEAT_OFF,
        IconKind::Loudness => WAVEFORM,
        IconKind::Flag => FLAG,
        IconKind::LevelEasy => LEVEL_EASY,
        IconKind::LevelMedium => LEVEL_MEDIUM,
        IconKind::LevelHard => LEVEL_HARD,
        IconKind::DoorOpen => DOOR_OPEN,
        _ => return None,
    })
}

/// Strokes `paths` at the command weight.
const fn line(paths: &'static [Outline]) -> Mark {
    stroke(paths, W)
}

/// A ring broken at its upper right, the arrowhead closing the gap.
const REFRESH: &[Mark] = &[line(&[
    Outline::Path("M20 12A8 8 0 1 1 17.66 6.34L20 8.6"),
    Outline::Path("M20 3.6V8.6H15"),
])];

/// A disc with an `i` cut out of it: what the thing on display is.
const INFO: &[Mark] = &[cut(&[
    circle(12.0, 12.0, 9.5),
    circle(12.0, 7.4, 1.5),
    Outline::Path("M12 10.3A1.25 1.25 0 0 1 13.25 11.55V16.85A1.25 1.25 0 0 1 10.75 16.85V11.55A1.25 1.25 0 0 1 12 10.3Z"),
])];

/// One to one: the true size.
const ONE_TO_ONE: &[Mark] = &[
    line(&[
        Outline::Path("M4.5 7.5L7 5V19"),
        Outline::Path("M14.5 7.5L17 5V19"),
    ]),
    fill(&[circle(12.0, 9.5, 1.3), circle(12.0, 15.0, 1.3)]),
];

/// An open folder: its back and tab, and the front tipped open.
const OPEN: &[Mark] = &[line(&[Outline::Path(
    "M4 19L7.1 11.4A1.6 1.6 0 0 1 8.6 10.4H20.2A0.9 0.9 0 0 1 21 11.6L18.7 18.2\
     A1.6 1.6 0 0 1 17.2 19.2H4.6A1.6 1.6 0 0 1 3 17.6V6.4A1.4 1.4 0 0 1 4.4 5H8.6\
     L10.6 7.2H16.6A1.4 1.4 0 0 1 18 8.6V10.4",
)])];

/// A box with an arrow leaving its corner.
const OPEN_WITH: &[Mark] = &[line(&[
    Outline::Path("M11 4H6A2 2 0 0 0 4 6V18A2 2 0 0 0 6 20H18A2 2 0 0 0 20 18V13"),
    Outline::Path("M14 4H20V10"),
    Outline::Path("M20 4L11.5 12.5"),
])];

/// A rounded window outline with its title band, the shape several window
/// pictures start from.
const WINDOW: Outline =
    Outline::Path("M5 4H19A2 2 0 0 1 21 6V18A2 2 0 0 1 19 20H5A2 2 0 0 1 3 18V6A2 2 0 0 1 5 4Z");

/// A window bearing a plus.
const NEW_WINDOW: &[Mark] = &[line(&[
    WINDOW,
    Outline::Path("M3 8.6H21"),
    Outline::Path("M12 11.4V17.2M9.1 14.3H14.9"),
])];

/// A page with a folded corner.
const SHEET: Outline =
    Outline::Path("M14 3H7A2 2 0 0 0 5 5V19A2 2 0 0 0 7 21H17A2 2 0 0 0 19 19V8Z");

/// The fold of [`SHEET`]'s corner.
const FOLD: Outline = Outline::Path("M14 3V8H19");

/// A page bearing a plus.
const NEW_DOCUMENT: &[Mark] = &[line(&[
    SHEET,
    FOLD,
    Outline::Path("M12 11.5V17.5M9 14.5H15"),
])];

/// A floppy disk: its body with a clipped corner, the shutter and the label.
const SAVE: &[Mark] = &[line(&[
    Outline::Path("M5 3H16.4L21 7.6V19A2 2 0 0 1 19 21H5A2 2 0 0 1 3 19V5A2 2 0 0 1 5 3Z"),
    Outline::Path("M8 3V7.5H15V3"),
    Outline::Path("M7 21V15A1 1 0 0 1 8 14H16A1 1 0 0 1 17 15V21"),
])];

/// A floppy disk with a pencil laid across its corner.
const SAVE_AS: &[Mark] = &[line(&[
    Outline::Path("M11.5 21H5A2 2 0 0 1 3 19V5A2 2 0 0 1 5 3H15.4L20 7.6V11"),
    Outline::Path("M7.5 3V7H14V3"),
    Outline::Path("M7 21V15A1 1 0 0 1 8 14H13"),
    Outline::Path("M19.2 13.2L21.5 15.5L15.5 21.5H13.2V19.2Z"),
])];

/// A window bearing a cross.
const CLOSE: &[Mark] = &[line(&[WINDOW, Outline::Path("M9 9L15 15M15 9L9 15")])];

/// A cross in a ring.
const QUIT: &[Mark] = &[line(&[
    circle(12.0, 12.0, 9.0),
    Outline::Path("M9 9L15 15M15 9L9 15"),
])];

/// A card of labelled lines.
const PROPERTIES: &[Mark] = &[
    line(&[
        Outline::Path(
            "M5 3H19A2 2 0 0 1 21 5V19A2 2 0 0 1 19 21H5A2 2 0 0 1 3 19V5A2 2 0 0 1 5 3Z",
        ),
        Outline::Path("M10.5 8H17M10.5 12H17M10.5 16H17"),
    ]),
    fill(&[
        circle(7.3, 8.0, 1.2),
        circle(7.3, 12.0, 1.2),
        circle(7.3, 16.0, 1.2),
    ]),
];

/// A page with a folded corner: the document menu.
const PAGE: &[Mark] = &[line(&[SHEET, FOLD])];

/// A pencil over the line it writes.
const EDIT: &[Mark] = &[line(&[
    Outline::Path("M15.6 4.4A2 2 0 0 1 18.4 4.4L19.6 5.6A2 2 0 0 1 19.6 8.4L9 19H5V15Z"),
    Outline::Path("M13.5 6.5L17.5 10.5"),
    Outline::Path("M13 21H21"),
])];

/// The almond of an eye.
const ALMOND: Outline = Outline::Path(
    "M2.5 12C4.7 7.8 8 5.6 12 5.6C16 5.6 19.3 7.8 21.5 12C19.3 16.2 16 18.4 12 18.4\
     C8 18.4 4.7 16.2 2.5 12Z",
);

/// An open eye.
const EYE: &[Mark] = &[line(&[ALMOND, circle(12.0, 12.0, 3.0)])];

/// An eye struck through.
const EYE_STRUCK: &[Mark] = &[line(&[
    ALMOND,
    circle(12.0, 12.0, 3.0),
    Outline::Path("M4 4L20 20"),
])];

/// Scissors, open, their blades crossing.
const SCISSORS: &[Mark] = &[line(&[
    circle(6.0, 6.5, 2.8),
    circle(6.0, 17.5, 2.8),
    Outline::Path("M8.4 8L20 18.5"),
    Outline::Path("M8.4 16L20 5.5"),
])];

/// Two pages, the front one before the corner of the one behind it.
const COPY: &[Mark] = &[line(&[
    Outline::Path(
        "M10 8H19A2 2 0 0 1 21 10V19A2 2 0 0 1 19 21H10A2 2 0 0 1 8 19V10A2 2 0 0 1 10 8Z",
    ),
    Outline::Path("M16 4H6A2 2 0 0 0 4 6V16"),
])];

/// A clipboard: its board and the clip at its top.
const CLIPBOARD: &[Mark] = &[line(&[
    Outline::Path("M8 5H6A2 2 0 0 0 4 7V20A2 2 0 0 0 6 22H18A2 2 0 0 0 20 20V7A2 2 0 0 0 18 5H16"),
    Outline::Path(
        "M9.5 3H14.5A1 1 0 0 1 15.5 4V6A1 1 0 0 1 14.5 7H9.5A1 1 0 0 1 8.5 6V4A1 1 0 0 1 9.5 3Z",
    ),
])];

/// The dashes of a selection frame: its four corners and a dash down the
/// middle of each side.
const DASHED_FRAME: Outline = Outline::Path(
    "M4 7.5V5A1 1 0 0 1 5 4H7.5M16.5 4H19A1 1 0 0 1 20 5V7.5M20 16.5V19A1 1 0 0 1 19 20\
     H16.5M7.5 20H5A1 1 0 0 1 4 19V16.5M10.5 4H13.5M20 10.5V13.5M13.5 20H10.5M4 13.5V10.5",
);

/// A dashed frame around a filled square.
const SELECT_ALL: &[Mark] = &[
    line(&[DASHED_FRAME]),
    fill(&[Outline::Path(
        "M9.2 8.2H14.8A1 1 0 0 1 15.8 9.2V14.8A1 1 0 0 1 14.8 15.8H9.2A1 1 0 0 1 8.2 14.8V9.2\
         A1 1 0 0 1 9.2 8.2Z",
    )]),
];

/// A dashed frame around a cross.
const SELECT_NONE: &[Mark] = &[line(&[
    DASHED_FRAME,
    Outline::Path("M9.4 9.4L14.6 14.6M14.6 9.4L9.4 14.6"),
])];

/// A text field holding a word, the caret standing across it.
const RENAME: &[Mark] = &[line(&[
    Outline::Path("M5 8H19A2 2 0 0 1 21 10V14A2 2 0 0 1 19 16H5A2 2 0 0 1 3 14V10A2 2 0 0 1 5 8Z"),
    Outline::Path("M6.6 12H10.6"),
    Outline::Path("M15.5 4.5V19.5M13.5 4.5H17.5M13.5 19.5H17.5"),
])];

/// Two pages, the front one bearing a plus.
const DUPLICATE: &[Mark] = &[line(&[
    Outline::Path(
        "M10 8H19A2 2 0 0 1 21 10V19A2 2 0 0 1 19 21H10A2 2 0 0 1 8 19V10A2 2 0 0 1 10 8Z",
    ),
    Outline::Path("M16 4H6A2 2 0 0 0 4 6V16"),
    Outline::Path("M14.5 11.5V17.5M11.5 14.5H17.5"),
])];

/// A waste bin: its lid and handle, the tapering can and its ribs.
const BIN: &[Mark] = &[line(&[
    Outline::Path("M3.5 6.5H20.5"),
    Outline::Path("M9 6.5V4.5A1 1 0 0 1 10 3.5H14A1 1 0 0 1 15 4.5V6.5"),
    Outline::Path("M5.8 6.5L6.9 19.2A1.6 1.6 0 0 0 8.5 20.7H15.5A1.6 1.6 0 0 0 17.1 19.2L18.2 6.5"),
    Outline::Path("M10 10.5V16.7M14 10.5V16.7"),
])];

/// A minus in a ring.
const MINUS_RING: &[Mark] = &[line(&[circle(12.0, 12.0, 9.0), Outline::Path("M8 12H16")])];

/// An eraser tipped on its edge, and the line it has rubbed along.
const ERASER: &[Mark] = &[line(&[
    Outline::Path(
        "M9.6 20.5L4.3 15.2A1.5 1.5 0 0 1 4.3 13.1L12.9 4.5A1.5 1.5 0 0 1 15 4.5L19.7 9.2\
         A1.5 1.5 0 0 1 19.7 11.3L10.5 20.5",
    ),
    Outline::Path("M7.4 20.5H20.5"),
    Outline::Path("M8.6 8.8L15.4 15.6"),
])];

/// An arrow curving back the way it came.
const UNDO: &[Mark] = &[line(&[
    Outline::Path("M8.5 13.5L3.5 8.5L8.5 3.5"),
    Outline::Path("M3.5 8.5H14.5A6 6 0 0 1 14.5 20.5H11"),
])];

/// An arrow curving on again.
const REDO: &[Mark] = &[line(&[
    Outline::Path("M15.5 13.5L20.5 8.5L15.5 3.5"),
    Outline::Path("M20.5 8.5H9.5A6 6 0 0 0 9.5 20.5H13"),
])];

/// Lines moved in past a chevron.
const INDENT: &[Mark] = &[line(&[
    Outline::Path("M3 5H21M11 10H21M11 14H21M3 19H21"),
    Outline::Path("M3.5 9L6.5 12L3.5 15"),
])];

/// Lines, a chevron pointing out of them.
const OUTDENT: &[Mark] = &[line(&[
    Outline::Path("M3 5H21M11 10H21M11 14H21M3 19H21"),
    Outline::Path("M6.5 9L3.5 12L6.5 15"),
])];

/// A pair of slashes: a line comment.
const SLASHES: &[Mark] = &[line(&[Outline::Path("M7 20L11 4M13 20L17 4")])];

/// A letter beside a block caret.
const BLOCK_CARET: &[Mark] = &[
    line(&[Outline::Path("M2.8 19L7.4 5.5L12 19"), Outline::Path("M4.6 14.2H10.2")]),
    fill(&[Outline::Path(
        "M15.2 5.5H19.8A0.8 0.8 0 0 1 20.6 6.3V18.2A0.8 0.8 0 0 1 19.8 19H15.2A0.8 0.8 0 0 1 14.4 18.2\
         V6.3A0.8 0.8 0 0 1 15.2 5.5Z",
    )]),
];

/// A magnifier.
const MAGNIFIER: &[Mark] = &[line(&[
    circle(10.5, 10.5, 6.5),
    Outline::Path("M15.3 15.3L20.5 20.5"),
])];

/// One box turning into another along an arrow.
const REPLACE: &[Mark] = &[line(&[
    Outline::Path(
        "M4.5 3H9.5A1.5 1.5 0 0 1 11 4.5V9.5A1.5 1.5 0 0 1 9.5 11H4.5A1.5 1.5 0 0 1 3 9.5V4.5A1.5 1.5 0 0 1 4.5 3Z",
    ),
    Outline::Path(
        "M14.5 13H19.5A1.5 1.5 0 0 1 21 14.5V19.5A1.5 1.5 0 0 1 19.5 21H14.5A1.5 1.5 0 0 1 13 19.5V14.5A1.5 1.5 0 0 1 14.5 13Z",
    ),
    Outline::Path("M14 7H15A2.5 2.5 0 0 1 17.5 9.5V10"),
    Outline::Path("M15.2 8.2L17.5 10.5L19.8 8.2"),
])];

/// A magnifier beside a down arrow.
const FIND_NEXT: &[Mark] = &[line(&[
    circle(8.6, 9.6, 5.4),
    Outline::Path("M12.6 13.6L15.6 16.6"),
    Outline::Path("M19.4 4V13.6"),
    Outline::Path("M17 11.2L19.4 13.6L21.8 11.2"),
])];

/// A magnifier beside an up arrow.
const FIND_PREVIOUS: &[Mark] = &[line(&[
    circle(8.6, 9.6, 5.4),
    Outline::Path("M12.6 13.6L15.6 16.6"),
    Outline::Path("M19.4 20V10.4"),
    Outline::Path("M17 12.8L19.4 10.4L21.8 12.8"),
])];

/// A number sign.
const NUMBER_SIGN: &[Mark] = &[line(&[Outline::Path(
    "M4 9H20M4 15H20M10 3.5L8 20.5M16 3.5L14 20.5",
)])];

/// A warning triangle.
const TRIANGLE: &[Mark] = &[
    line(&[
        Outline::Path(
            "M10.3 4.4A2 2 0 0 1 13.7 4.4L21.2 17.5A2 2 0 0 1 19.5 20.5H4.5A2 2 0 0 1 2.8 17.5Z",
        ),
        Outline::Path("M12 9.2V13.8"),
    ]),
    fill(&[circle(12.0, 17.0, 1.25)]),
];

/// Lines of text.
const TEXT_LINES: &[Mark] = &[line(&[Outline::Path("M4 6H20M4 10H14M4 14H20M4 18H11")])];

/// Binary digits in two rows.
const BINARY: &[Mark] = &[line(&[
    Outline::Path("M6.5 4A2.5 2.5 0 0 1 9 6.5V7.5A2.5 2.5 0 0 1 4 7.5V6.5A2.5 2.5 0 0 1 6.5 4Z"),
    Outline::Path("M15.5 5.5L17.5 4V10"),
    Outline::Path("M4.5 15.5L6.5 14V20"),
    Outline::Path(
        "M17.5 14A2.5 2.5 0 0 1 20 16.5V17.5A2.5 2.5 0 0 1 15 17.5V16.5A2.5 2.5 0 0 1 17.5 14Z",
    ),
])];

/// Braces around a run of values.
const BRACES: &[Mark] = &[
    line(&[
        Outline::Path("M8 3.5H7.2A2 2 0 0 0 5.2 5.5V9.6C5.2 11 4.4 12 3.2 12C4.4 12 5.2 13 5.2 14.4V18.5A2 2 0 0 0 7.2 20.5H8"),
        Outline::Path("M16 3.5H16.8A2 2 0 0 1 18.8 5.5V9.6C18.8 11 19.6 12 20.8 12C19.6 12 18.8 13 18.8 14.4V18.5A2 2 0 0 1 16.8 20.5H16"),
    ]),
    fill(&[circle(9.0, 12.0, 1.15), circle(12.0, 12.0, 1.15), circle(15.0, 12.0, 1.15)]),
];

/// Angle brackets around a slash.
const ANGLES: &[Mark] = &[line(&[
    Outline::Path("M7.5 7L2.5 12L7.5 17"),
    Outline::Path("M16.5 7L21.5 12L16.5 17"),
    Outline::Path("M14 4L10 20"),
])];

/// A page bearing angle brackets.
const PAGE_OF_CODE: &[Mark] = &[line(&[
    SHEET,
    FOLD,
    Outline::Path("M10 12.5L8 14.75L10 17"),
    Outline::Path("M14 12.5L16 14.75L14 17"),
])];

/// An arrow meeting a stop.
const TAB_STOP: &[Mark] = &[line(&[
    Outline::Path("M3 12H16"),
    Outline::Path("M11.5 7.5L16 12L11.5 16.5"),
    Outline::Path("M20.5 5.5V18.5"),
])];

/// A pilcrow.
const PILCROW: &[Mark] = &[line(&[
    Outline::Path("M13 4V20M17.5 4V20"),
    Outline::Path("M19.5 4H10.3A4.3 4.3 0 0 0 10.3 12.6H13"),
])];

/// A double arrow between two bars.
const FIT_WIDTH: &[Mark] = &[line(&[
    Outline::Path("M3 4V20M21 4V20"),
    Outline::Path("M7 12H17"),
    Outline::Path("M9.5 9.5L7 12L9.5 14.5M14.5 9.5L17 12L14.5 14.5"),
])];

/// A capital letter, the shape the text-size pictures share.
const LETTER: Outline = Outline::Path("M2.8 19.5L7.6 5L12.4 19.5M4.5 14.8H10.7");

/// A letter beside a plus.
const TEXT_LARGER: &[Mark] = &[line(&[LETTER, Outline::Path("M18 8.5V15.5M14.5 12H21.5")])];

/// A letter beside a minus.
const TEXT_SMALLER: &[Mark] = &[line(&[LETTER, Outline::Path("M14.5 12H21.5")])];

/// A letter beside an equals sign.
const TEXT_ACTUAL: &[Mark] = &[line(&[
    LETTER,
    Outline::Path("M14.5 10H21.5M14.5 14.5H21.5"),
])];

/// A half-turn arrow.
const HALF_TURN: &[Mark] = &[line(&[
    Outline::Path("M3.5 15A8 8 0 0 1 19.5 15"),
    Outline::Path("M17 12.5L19.5 15L22 12.5"),
    Outline::Path("M3.5 15V19"),
])];

/// Two arrowheads facing away from a level axis: the mirror turned on its
/// side.
const FLIP_VERTICAL: &[Mark] = &[fill(&[
    Outline::Path("M2 11H22V13H2Z"),
    Outline::Path("M5 9H19L12 2Z"),
    Outline::Path("M5 15H19L12 22Z"),
])];

/// A ruled frame.
const GRID: &[Mark] = &[line(&[
    Outline::Path("M5 3H19A2 2 0 0 1 21 5V19A2 2 0 0 1 19 21H5A2 2 0 0 1 3 19V5A2 2 0 0 1 5 3Z"),
    Outline::Path("M3 9H21M3 15H21M9 3V21M15 3V21"),
])];

/// A window with a side pane.
const PANES: &[Mark] = &[line(&[
    WINDOW,
    Outline::Path("M9.5 4V20"),
    Outline::Path("M5.5 8H7M5.5 11H7"),
])];

/// An arrow turning back round to where it began.
const TURN_BACK: &[Mark] = &[line(&[
    Outline::Path("M4 12A8 8 0 1 0 6.34 6.34L4 8.6"),
    Outline::Path("M4 3.6V8.6H9"),
])];

/// A window divided into its panels.
const LAYOUT: &[Mark] = &[line(&[WINDOW, Outline::Path("M3 9H21M11 9V20")])];

/// Four large tiles.
const LARGE_TILES: &[Mark] = &[line(&[
    Outline::Path("M4.5 3H8.5A1.5 1.5 0 0 1 10 4.5V8.5A1.5 1.5 0 0 1 8.5 10H4.5A1.5 1.5 0 0 1 3 8.5V4.5A1.5 1.5 0 0 1 4.5 3Z"),
    Outline::Path("M15.5 3H19.5A1.5 1.5 0 0 1 21 4.5V8.5A1.5 1.5 0 0 1 19.5 10H15.5A1.5 1.5 0 0 1 14 8.5V4.5A1.5 1.5 0 0 1 15.5 3Z"),
    Outline::Path("M4.5 14H8.5A1.5 1.5 0 0 1 10 15.5V19.5A1.5 1.5 0 0 1 8.5 21H4.5A1.5 1.5 0 0 1 3 19.5V15.5A1.5 1.5 0 0 1 4.5 14Z"),
    Outline::Path("M15.5 14H19.5A1.5 1.5 0 0 1 21 15.5V19.5A1.5 1.5 0 0 1 19.5 21H15.5A1.5 1.5 0 0 1 14 19.5V15.5A1.5 1.5 0 0 1 15.5 14Z"),
])];

/// Small tiles beside their names, in two columns.
const SMALL_TILES: &[Mark] = &[
    fill(&[
        Outline::Path("M3 4H6.5V7.5H3Z"),
        Outline::Path("M3 10.25H6.5V13.75H3Z"),
        Outline::Path("M3 16.5H6.5V20H3Z"),
        Outline::Path("M13 4H16.5V7.5H13Z"),
        Outline::Path("M13 10.25H16.5V13.75H13Z"),
        Outline::Path("M13 16.5H16.5V20H13Z"),
    ]),
    line(&[Outline::Path(
        "M8.4 5.75H11M8.4 12H11M8.4 18.25H11M18.4 5.75H21M18.4 12H21M18.4 18.25H21",
    )]),
];

/// Rows of points and lines.
const LIST: &[Mark] = &[
    fill(&[
        circle(4.5, 6.0, 1.4),
        circle(4.5, 12.0, 1.4),
        circle(4.5, 18.0, 1.4),
    ]),
    line(&[Outline::Path("M9 6H21M9 12H21M9 18H21")]),
];

/// The down arrow the ordering pictures share.
const ORDER_ARROW: Outline = Outline::Path("M6 4V20M3 17L6 20L9 17");

/// Letters beside a down arrow.
const SORT_BY_NAME: &[Mark] = &[line(&[
    ORDER_ARROW,
    Outline::Path("M13 11L16.5 3.5L20 11M14.1 8.8H18.9"),
    Outline::Path("M13 13.5H20L13 20.5H20"),
])];

/// A triangle, a circle and a square.
const SHAPES: &[Mark] = &[line(&[
    Outline::Path("M7.5 3.2L11.8 10.6H3.2Z"),
    circle(7.5, 17.0, 3.6),
    Outline::Path("M15.2 8.8H19.8A1.2 1.2 0 0 1 21 10V14.6A1.2 1.2 0 0 1 19.8 15.8H15.2A1.2 1.2 0 0 1 14 14.6V10A1.2 1.2 0 0 1 15.2 8.8Z"),
])];

/// Shortening bars beside a down arrow.
const SORT_BY_SIZE: &[Mark] = &[line(&[
    ORDER_ARROW,
    Outline::Path("M12 6H21M12 11H18.5M12 16H16"),
])];

/// A calendar page with its two rings.
const CALENDAR: &[Mark] = &[
    line(&[
        Outline::Path(
            "M5 5H19A2 2 0 0 1 21 7V19A2 2 0 0 1 19 21H5A2 2 0 0 1 3 19V7A2 2 0 0 1 5 5Z",
        ),
        Outline::Path("M3 10H21"),
        Outline::Path("M8 3V7M16 3V7"),
    ]),
    fill(&[circle(8.0, 14.5, 1.3), circle(12.0, 14.5, 1.3)]),
];

/// Blocks against a left edge.
const ARRANGE_LEFT: &[Mark] = &[line(&[
    Outline::Path("M3.5 3V21"),
    Outline::Path("M7.5 5.5H15A1.5 1.5 0 0 1 16.5 7V8.5A1.5 1.5 0 0 1 15 10H7.5Z"),
    Outline::Path("M7.5 14H19A1.5 1.5 0 0 1 20.5 15.5V17A1.5 1.5 0 0 1 19 18.5H7.5Z"),
])];

/// Blocks against a right edge.
const ARRANGE_RIGHT: &[Mark] = &[line(&[
    Outline::Path("M20.5 3V21"),
    Outline::Path("M16.5 5.5H9A1.5 1.5 0 0 0 7.5 7V8.5A1.5 1.5 0 0 0 9 10H16.5Z"),
    Outline::Path("M16.5 14H5A1.5 1.5 0 0 0 3.5 15.5V17A1.5 1.5 0 0 0 5 18.5H16.5Z"),
])];

/// A curved arrow in a box.
const SHORTCUT: &[Mark] = &[line(&[
    Outline::Path("M5 3H19A2 2 0 0 1 21 5V19A2 2 0 0 1 19 21H5A2 2 0 0 1 3 19V5A2 2 0 0 1 5 3Z"),
    Outline::Path("M8 16.5V14A4.5 4.5 0 0 1 12.5 9.5H16.5"),
    Outline::Path("M13.5 6.5L16.5 9.5L13.5 12.5"),
])];

/// A house: its roof, walls and door.
const HOUSE: &[Mark] = &[line(&[
    Outline::Path("M3 10.6L12 3.4L21 10.6"),
    Outline::Path("M5.5 8.8V19A1.6 1.6 0 0 0 7.1 20.6H16.9A1.6 1.6 0 0 0 18.5 19V8.8"),
    Outline::Path("M10 20.6V15H14V20.6"),
])];

/// An arrow up.
const ARROW_UP: &[Mark] = &[line(&[
    Outline::Path("M12 20V4"),
    Outline::Path("M6 10L12 4L18 10"),
])];

/// An arrow down.
const ARROW_DOWN: &[Mark] = &[line(&[
    Outline::Path("M12 4V20"),
    Outline::Path("M6 14L12 20L18 14"),
])];

/// A sheet seen edge on, the shape the layer pictures stack.
const SHEET_ON_EDGE: Outline = Outline::Path("M12 3L21 7.5L12 12L3 7.5Z");

/// Stacked sheets seen edge on.
const LAYERS: &[Mark] = &[line(&[
    SHEET_ON_EDGE,
    Outline::Path("M3 12L12 16.5L21 12"),
    Outline::Path("M3 16.5L12 21L21 16.5"),
])];

/// Stacked sheets bearing a plus.
const LAYER_NEW: &[Mark] = &[line(&[
    Outline::Path("M11 3L19 7L11 11L3 7Z"),
    Outline::Path("M3 11.5L11 15.5L13 14.5"),
    Outline::Path("M18 13V21M14 17H22"),
])];

/// An arrow above a sheet.
const LAYER_ABOVE: &[Mark] = &[line(&[
    Outline::Path("M12 12L21 16.5L12 21L3 16.5Z"),
    Outline::Path("M12 9.5V3M8.8 6.2L12 3L15.2 6.2"),
])];

/// An arrow below a sheet.
const LAYER_BELOW: &[Mark] = &[line(&[
    SHEET_ON_EDGE,
    Outline::Path("M12 14.5V21M8.8 17.8L12 21L15.2 17.8"),
])];

/// A solid square in front of an open one.
const LAYER_RAISE: &[Mark] = &[
    fill(&[Outline::Path(
        "M4 3H13A1 1 0 0 1 14 4V13A1 1 0 0 1 13 14H4A1 1 0 0 1 3 13V4A1 1 0 0 1 4 3Z",
    )]),
    line(&[Outline::Path(
        "M17 10H20A1 1 0 0 1 21 11V20A1 1 0 0 1 20 21H11A1 1 0 0 1 10 20V17",
    )]),
];

/// An open square in front of a solid one.
const LAYER_LOWER: &[Mark] = &[
    line(&[Outline::Path(
        "M4 3H13A1 1 0 0 1 14 4V13A1 1 0 0 1 13 14H4A1 1 0 0 1 3 13V4A1 1 0 0 1 4 3Z",
    )]),
    fill(&[Outline::Path(
        "M16 10H20A1 1 0 0 1 21 11V20A1 1 0 0 1 20 21H11A1 1 0 0 1 10 20V16H14A2 2 0 0 0 16 14Z",
    )]),
];

/// Two arrows joining into one.
const MERGE_DOWN: &[Mark] = &[line(&[
    Outline::Path("M7 3V7.5A4 4 0 0 0 8.2 10.4L12 14.2V21"),
    Outline::Path("M17 3V7.5A4 4 0 0 1 15.8 10.4L12 14.2"),
    Outline::Path("M8.5 17.5L12 21L15.5 17.5"),
])];

/// Two arrows pressing onto a line.
const FLATTEN: &[Mark] = &[line(&[
    Outline::Path("M3 12H21"),
    Outline::Path("M12 3V8.5M9 5.5L12 8.5L15 5.5"),
    Outline::Path("M12 21V15.5M9 18.5L12 15.5L15 18.5"),
])];

/// Arrows pointing out of opposite corners.
const RESIZE: &[Mark] = &[line(&[
    Outline::Path("M14 3H21V10M21 3L14 10"),
    Outline::Path("M10 21H3V14M3 21L10 14"),
])];

/// A picture inside a larger frame.
const CANVAS_SIZE: &[Mark] = &[
    line(&[Outline::Path(
        "M5 3H19A2 2 0 0 1 21 5V19A2 2 0 0 1 19 21H5A2 2 0 0 1 3 19V5A2 2 0 0 1 5 3Z",
    )]),
    fill(&[Outline::Path("M7.5 6.5H13.5A1 1 0 0 1 14.5 7.5V13.5A1 1 0 0 1 13.5 14.5H7.5A1 1 0 0 1 6.5 13.5V7.5A1 1 0 0 1 7.5 6.5Z")]),
];

/// A square half filled across its diagonal.
const INVERT: &[Mark] = &[
    line(&[Outline::Path(
        "M5 3H19A2 2 0 0 1 21 5V19A2 2 0 0 1 19 21H5A2 2 0 0 1 3 19V5A2 2 0 0 1 5 3Z",
    )]),
    fill(&[Outline::Path("M4 20L20 4V19A1 1 0 0 1 19 20Z")]),
];

/// Three overlapping circles.
const THREE_CIRCLES: &[Mark] = &[line(&[
    circle(12.0, 8.5, 5.0),
    circle(8.2, 15.0, 5.0),
    circle(15.8, 15.0, 5.0),
])];

/// A frame around a disc, the mask both mask pictures carry.
const MASK: &[Mark] = &[
    line(&[Outline::Path(
        "M5 3H15A2 2 0 0 1 17 5V11.5M11.5 17H5A2 2 0 0 1 3 15V5A2 2 0 0 1 5 3Z",
    )]),
    fill(&[circle(10.0, 10.0, 3.6)]),
];

/// The mask, bearing a plus.
const MASK_ADD: &[Mark] = &[
    MASK[0],
    MASK[1],
    line(&[Outline::Path("M18 14V22M14 18H22")]),
];

/// The mask, bearing a minus.
const MASK_REMOVE: &[Mark] = &[MASK[0], MASK[1], line(&[Outline::Path("M14 18H22")])];

/// An anchor.
const ANCHOR: &[Mark] = &[line(&[
    circle(12.0, 5.2, 2.2),
    Outline::Path("M12 7.4V21"),
    Outline::Path("M8.5 11H15.5"),
    Outline::Path("M4.5 13.5A7.5 7.5 0 0 0 19.5 13.5"),
])];

/// A filled circle in front of an open one.
const COLOUR_PRIMARY: &[Mark] = &[
    line(&[Outline::Path("M16.6 9.2A5.5 5.5 0 1 1 9.2 16.6")]),
    fill(&[circle(9.0, 9.0, 6.0)]),
];

/// An open circle in front of a filled one.
const COLOUR_SECONDARY: &[Mark] = &[
    fill(&[Outline::Path(
        "M16.9 8.7A6.5 6.5 0 1 1 8.7 16.9A7 7 0 0 0 16.9 8.7Z",
    )]),
    line(&[circle(9.0, 9.0, 5.5)]),
];

/// Two arrows passing each other.
const SWAP: &[Mark] = &[line(&[
    Outline::Path("M4 8H18M14.5 4.5L18 8L14.5 11.5"),
    Outline::Path("M20 16H6M9.5 12.5L6 16L9.5 19.5"),
])];

/// A filled and an open square, apart.
const COLOUR_RESET: &[Mark] = &[
    fill(&[Outline::Path(
        "M4 3H10A1 1 0 0 1 11 4V10A1 1 0 0 1 10 11H4A1 1 0 0 1 3 10V4A1 1 0 0 1 4 3Z",
    )]),
    line(&[Outline::Path(
        "M15 13H19A1 1 0 0 1 20 14V18A1 1 0 0 1 19 19H15A1 1 0 0 1 14 18V14A1 1 0 0 1 15 13Z",
    )]),
];

/// Three sliders, each at its own setting.
const SLIDERS: &[Mark] = &[line(&[
    Outline::Path("M3 5.5H11M15 5.5H21M3 12H6M10 12H21M3 18.5H14M18 18.5H21"),
    Outline::Path("M13 3V8M8 9.5V14.5M16 16V21"),
])];

/// A sun.
const SUN: &[Mark] = &[line(&[
    circle(12.0, 12.0, 4.0),
    Outline::Path(
        "M12 2.6V4.6M12 19.4V21.4M2.6 12H4.6M19.4 12H21.4M5.4 5.4L6.8 6.8M17.2 17.2L18.6 18.6\
         M5.4 18.6L6.8 17.2M17.2 6.8L18.6 5.4",
    ),
])];

/// A drop's outline, the shape the hue and saturation pictures share.
const DROP: Outline =
    Outline::Path("M12 3C12 3 18.5 9.6 18.5 14.4A6.5 6.5 0 0 1 5.5 14.4C5.5 9.6 12 3 12 3Z");

/// A drop.
const DROPLET: &[Mark] = &[line(&[DROP, Outline::Path("M9 14.6A3 3 0 0 0 12 17.6")])];

/// A drop struck through.
const DROPLET_STRUCK: &[Mark] = &[line(&[DROP, Outline::Path("M3.5 3.5L20.5 20.5")])];

/// A balance on its post, a pan hung from either end of its beam.
const BALANCE: &[Mark] = &[line(&[
    Outline::Path("M12 4V20.5M8 20.5H16"),
    Outline::Path("M4.5 7H19.5"),
    Outline::Path("M4.5 7L2.5 13.2A2.4 2.4 0 0 0 6.5 13.2Z"),
    Outline::Path("M19.5 7L17.5 13.2A2.4 2.4 0 0 0 21.5 13.2Z"),
])];

/// The bars of a histogram over its baseline.
const HISTOGRAM: &[Mark] = &[line(&[Outline::Path(
    "M3 20.5H21M6 20.5V16M9.5 20.5V9.5M13 20.5V5M16.5 20.5V11.5M20 20.5V16.5",
)])];

/// A curve rising across a frame.
const CURVE: &[Mark] = &[
    line(&[Outline::Path(
        "M5 3H19A2 2 0 0 1 21 5V19A2 2 0 0 1 19 21H5A2 2 0 0 1 3 19V5A2 2 0 0 1 5 3Z",
    )]),
    line(&[Outline::Path("M6 18C11.5 18 12.5 6 18 6")]),
];

/// A thermometer.
const THERMOMETER: &[Mark] = &[line(&[
    Outline::Path("M10 14.2V5A2 2 0 0 1 14 5V14.2A4 4 0 1 1 10 14.2Z"),
    Outline::Path("M12 11V17.5"),
])];

/// A staircase.
const STAIRS: &[Mark] = &[line(&[Outline::Path("M3 20.5H8V15.5H13V10.5H18V5.5H21")])];

/// A frame filled on one side.
const HALF_FRAME: &[Mark] = &[
    line(&[Outline::Path(
        "M5 3H19A2 2 0 0 1 21 5V19A2 2 0 0 1 19 21H5A2 2 0 0 1 3 19V5A2 2 0 0 1 5 3Z",
    )]),
    fill(&[Outline::Path(
        "M12 4H19A1 1 0 0 1 20 5V19A1 1 0 0 1 19 20H12Z",
    )]),
];

/// A ring of dots around a centre one.
const DOT_RING: &[Mark] = &[fill(&[
    circle(12.0, 12.0, 1.6),
    circle(12.0, 4.0, 1.3),
    circle(17.66, 6.34, 1.3),
    circle(20.0, 12.0, 1.3),
    circle(17.66, 17.66, 1.3),
    circle(12.0, 20.0, 1.3),
    circle(6.34, 17.66, 1.3),
    circle(4.0, 12.0, 1.3),
    circle(6.34, 6.34, 1.3),
    circle(12.0, 8.0, 1.0),
    circle(16.0, 12.0, 1.0),
    circle(12.0, 16.0, 1.0),
    circle(8.0, 12.0, 1.0),
])];

/// A four-pointed spark: an edge brought to a point.
const SPARK: &[Mark] = &[fill(&[Outline::Path(
    "M12 2.5L13.9 10.1L21.5 12L13.9 13.9L12 21.5L10.1 13.9L2.5 12L10.1 10.1Z",
)])];

/// A chequered frame.
const CHEQUER: &[Mark] = &[
    line(&[Outline::Path(
        "M5 3H19A2 2 0 0 1 21 5V19A2 2 0 0 1 19 21H5A2 2 0 0 1 3 19V5A2 2 0 0 1 5 3Z",
    )]),
    fill(&[
        Outline::Path("M4 4H9V9H4Z"),
        Outline::Path("M15 4H20V9H15Z"),
        Outline::Path("M9.5 9.5H14.5V14.5H9.5Z"),
        Outline::Path("M4 15H9V20H4Z"),
        Outline::Path("M15 15H20V20H15Z"),
    ]),
];

/// Scattered dots.
const SCATTER: &[Mark] = &[fill(&[
    circle(5.0, 5.0, 1.4),
    circle(11.5, 4.0, 1.4),
    circle(18.5, 5.5, 1.4),
    circle(7.5, 10.0, 1.4),
    circle(14.5, 9.0, 1.4),
    circle(20.0, 12.0, 1.4),
    circle(4.0, 15.0, 1.4),
    circle(10.5, 14.5, 1.4),
    circle(16.5, 16.0, 1.4),
    circle(6.0, 20.0, 1.4),
    circle(13.0, 19.5, 1.4),
    circle(19.5, 20.0, 1.4),
])];

/// A diamond between corner marks.
const EDGES: &[Mark] = &[line(&[
    Outline::Path(
        "M3 8V5A2 2 0 0 1 5 3H8M16 3H19A2 2 0 0 1 21 5V8M21 16V19A2 2 0 0 1 19 21H16M8 21H5\
         A2 2 0 0 1 3 19V16",
    ),
    Outline::Path("M12 7L17 12L12 17L7 12Z"),
])];

/// Two pages, one behind the other.
const PAGES: &[Mark] = &[line(&[
    Outline::Path("M12 7H6A2 2 0 0 0 4 9V19A2 2 0 0 0 6 21H14A2 2 0 0 0 16 19V11Z"),
    Outline::Path("M12 7V11H16"),
    Outline::Path("M8 5V5A2 2 0 0 1 10 3H16L20 7V15A2 2 0 0 1 18 17"),
])];

/// A toolbox: its body, its handle and its latch.
const TOOLBOX: &[Mark] = &[line(&[
    Outline::Path("M4 8H20A1 1 0 0 1 21 9V19A1 1 0 0 1 20 20H4A1 1 0 0 1 3 19V9A1 1 0 0 1 4 8Z"),
    Outline::Path("M9 8V5.5A1.5 1.5 0 0 1 10.5 4H13.5A1.5 1.5 0 0 1 15 5.5V8"),
    Outline::Path("M3 13H21"),
    Outline::Path("M10 12V14.5H14V12"),
])];

/// The trace of a pulse.
const PULSE: &[Mark] = &[line(&[Outline::Path(
    "M2.5 12H6.5L9.5 4.5L14.5 19.5L17.5 12H21.5",
)])];

/// A prompt in a window.
const PROMPT: &[Mark] = &[line(&[
    WINDOW,
    Outline::Path("M7 9L10 12L7 15"),
    Outline::Path("M12.5 15H17"),
])];

/// An arrow leaving a door frame.
const LOG_OUT: &[Mark] = &[line(&[
    Outline::Path("M10 4H6A2 2 0 0 0 4 6V18A2 2 0 0 0 6 20H10"),
    Outline::Path("M9.5 12H20"),
    Outline::Path("M16 8L20 12L16 16"),
])];

/// A turning arrow around a bar.
const RESTART: &[Mark] = &[line(&[
    Outline::Path("M20 12A8 8 0 1 1 17.66 6.34L20 8.6"),
    Outline::Path("M20 3.6V8.6H15"),
    Outline::Path("M12 8.2V13"),
])];

/// A window in front of another.
const SWITCH_TO: &[Mark] = &[line(&[
    Outline::Path("M4 14V5A1 1 0 0 1 5 4H14"),
    Outline::Path("M9.5 8H19.5A1.5 1.5 0 0 1 21 9.5V19.5A1.5 1.5 0 0 1 19.5 21H9.5A1.5 1.5 0 0 1 8 19.5V9.5A1.5 1.5 0 0 1 9.5 8Z"),
    Outline::Path("M8 12H21"),
])];

/// A dial with its needle low.
const GAUGE: &[Mark] = &[
    line(&[
        Outline::Path("M4.6 17.5A8.5 8.5 0 1 1 19.4 17.5"),
        Outline::Path("M12 14.5L8 10.5"),
    ]),
    fill(&[circle(12.0, 14.5, 1.7)]),
];

/// A page of lines.
const LOG_PAGE: &[Mark] = &[line(&[
    SHEET,
    FOLD,
    Outline::Path("M9 12.5H15M9 16H15M9 9H11"),
])];

/// A cross in an octagon.
const OCTAGON_CROSS: &[Mark] = &[line(&[
    Outline::Path("M8.3 3H15.7L21 8.3V15.7L15.7 21H8.3L3 15.7V8.3Z"),
    Outline::Path("M9 9L15 15M15 9L9 15"),
])];

/// A straight arrow: played once, through to its end.
const REPEAT_OFF: &[Mark] = &[line(&[
    Outline::Path("M3.5 12H20"),
    Outline::Path("M14 6L20 12L14 18"),
])];

/// A waveform.
const WAVEFORM: &[Mark] = &[line(&[Outline::Path(
    "M3.5 10V14M7.5 7V17M11.5 3.5V20.5M15.5 8V16M19.5 10.5V13.5",
)])];

/// A flag on its pole.
const FLAG: &[Mark] = &[line(&[
    Outline::Path("M5 21V3.5"),
    Outline::Path("M5 4H17.5L15 8.5L17.5 13H5"),
])];

/// A bar the level pictures stand, lowest first.
const BARS: [Outline; 3] = [
    Outline::Path("M5 14H7A1 1 0 0 1 8 15V19A1 1 0 0 1 7 20H5A1 1 0 0 1 4 19V15A1 1 0 0 1 5 14Z"),
    Outline::Path(
        "M11 9H13A1 1 0 0 1 14 10V19A1 1 0 0 1 13 20H11A1 1 0 0 1 10 19V10A1 1 0 0 1 11 9Z",
    ),
    Outline::Path(
        "M17 4H19A1 1 0 0 1 20 5V19A1 1 0 0 1 19 20H17A1 1 0 0 1 16 19V5A1 1 0 0 1 17 4Z",
    ),
];

/// Three rising bars, the first filled.
const LEVEL_EASY: &[Mark] = &[fill(&[BARS[0]]), stroke(&[BARS[1], BARS[2]], 1.6)];

/// Three rising bars, two filled.
const LEVEL_MEDIUM: &[Mark] = &[fill(&[BARS[0], BARS[1]]), stroke(&[BARS[2]], 1.6)];

/// Three rising bars, all filled.
const LEVEL_HARD: &[Mark] = &[fill(&[BARS[0], BARS[1], BARS[2]])];

/// An open door in its frame.
const DOOR_OPEN: &[Mark] = &[
    line(&[
        Outline::Path("M13.5 4H17.5A1.5 1.5 0 0 1 19 5.5V20.5"),
        Outline::Path("M2.5 20.5H21.5"),
        Outline::Path(
            "M5 19.3V5.2A1 1 0 0 1 5.8 4.2L11.8 3A1 1 0 0 1 13 4V20A1 1 0 0 1 11.8 21L5.8 19.9\
             A1 1 0 0 1 5 19.3Z",
        ),
    ]),
    fill(&[circle(10.4, 12.2, 1.1)]),
];

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
