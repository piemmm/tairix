//! The closed set of built-in icon glyphs.
//!
//! [`IconKind`] is the vocabulary of glyphs the desktop draws: the taskbar's
//! status/notification area (network, volume, battery, bell), the file
//! manager's file-type icons (folder, document, application bundle, and the
//! broad content classes text/image/archive/executable), and the file
//! manager's toolbar command icons (back/forward/up navigation, refresh, the
//! view toggle, sort, and new folder). A theme asset id
//! resolves to a kind through [`IconKind::for_asset`]; an unrecognised id
//! falls back to [`IconKind::Generic`] rather than failing, so an unknown
//! asset still shows a placeholder instead of nothing. [`builtin_icon`] turns
//! a kind plus a single theme colour into a [`VectorIcon`]; the glyphs are
//! monochrome silhouettes tinted by the caller, so re-theming is data, not
//! new code. A settings category's or pane's glyph is the symbol its colour
//! badge carries ([`crate::badge()`]).
//!
//! [`disk_icon`] maps the storage medium a mounted volume reports onto the
//! drive kind that represents it, so the file manager and the desktop draw
//! the same icon for the same medium rather than each guessing.

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::blkio::{BlkMedium, BlkRotation, BlkTransport};
use tairix_raster::{Affine, Color};
use tairix_svg::geom::{place, SubPath};

use crate::fit::{GridFit, Phase};
use crate::symbol;
use crate::vector::{IconLayer, VectorIcon};

/// The design-grid side every built-in glyph is authored on.
const DESIGN: u32 = 24;

/// A desktop icon glyph — a taskbar status/notification icon or a file
/// manager file-type icon.
///
/// A closed set: adding a glyph is a new variant plus its coordinate table
/// (and its [`index`](Self::index) slot), never an open-ended string lookup
/// at the draw site.
///
/// A fine-grained file-class kind (an HTML or Rust text file, a PNG or SVG
/// image, a specific disk medium) deliberately shares its broad family's
/// built-in glyph: [`builtin_icon`] draws a `TextHtml` as the plain text
/// glyph, an `ImagePng` as the plain image glyph, and every `Disk*` as the
/// one disk glyph. The distinction still names a distinct on-disk asset id,
/// so a system that ships the raster artwork resolves the precise icon while
/// one that does not still shows a meaningful family glyph rather than the
/// bare [`Generic`](Self::Generic) placeholder — fallback stays total.
///
/// `Ord` orders the cache-invalidation candidates a reclaim cache indexes,
/// not a meaningful glyph ordering — the taskbar's icon cache
/// (`plans/SMARTRAM.md` section 6.4) needs `IconKind` as a `BTreeMap` key.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum IconKind {
    /// Network / signal-strength bars.
    Network,
    /// A speaker, for volume / audio status.
    Volume,
    /// A battery body, for power status.
    Battery,
    /// A bell, for pending notifications.
    Bell,
    /// A closed folder, for a directory the browser can descend into.
    Folder,
    /// An open folder, for a directory being entered or a drop target.
    FolderOpen,
    /// A folder with pages stacked behind it, for a directory that holds at
    /// least one entry — the occupancy cue beside the empty
    /// [`Folder`](Self::Folder).
    FolderFilled,
    /// A folder's back and tab, which a folder's picture of what it holds
    /// draws its cards in front of. Never the icon of an entry.
    FolderBack,
    /// A folder's front pocket, drawn over the cards standing in it. Never
    /// the icon of an entry.
    FolderFront,
    /// A generic document, for a regular file of no recognised class.
    File,
    /// An application tile, for a `<Name>.app` bundle.
    AppBundle,
    /// Lines of text, for a text/document file.
    Text,
    /// A picture, for an image file.
    Image,
    /// A package, for an archive file.
    Archive,
    /// A pair of notes, for a sound file.
    Audio,
    /// A film camera, for a video file.
    Video,
    /// A bar behind a left-pointing triangle, for the start of the track or
    /// the one before it.
    SkipPrevious,
    /// A right-pointing triangle against a bar, for the next track.
    SkipNext,
    /// Two crossing arrows, for playing in a shuffled order.
    Shuffle,
    /// A loop of two arrows, for playing the whole list again.
    Repeat,
    /// A loop of two arrows around a stroke, for playing one track again.
    RepeatOne,
    /// A microphone, for sound being recorded.
    Microphone,
    /// A speaker struck through, for a muted output.
    VolumeMuted,
    /// The [`Network`](Self::Network) bars beside a cross: no interface is
    /// connected.
    NetworkOffline,
    /// A run/bolt mark, for an executable.
    Executable,
    /// A chevron pointing left, for the file manager's Back navigation
    /// command.
    NavBack,
    /// A chevron pointing right, for the file manager's Forward navigation
    /// command.
    NavForward,
    /// A chevron pointing up, for the file manager's Up (climb-to-parent)
    /// command.
    NavUp,
    /// A circular arrow, for the file manager's Refresh command.
    Refresh,
    /// A grid of tiles, for the file manager's list/grid view toggle.
    ViewToggle,
    /// Descending horizontal bars, for the file manager's sort command.
    Sort,
    /// A folder with a plus badge, for the file manager's New Folder command.
    NewFolder,
    /// A waste bin, for the file manager's Trash location (the "go to Trash"
    /// command that navigates to the user's Trash directory).
    Trash,
    /// An open, tipped-out waste bin, for the file manager's Empty Trash
    /// command (the permanent removal of the Trash's contents).
    EmptyTrash,
    /// A three-by-three grid of application tiles, for the taskbar's
    /// program-library launcher.
    Library,
    /// A head-and-shoulders bust, for a user account.
    ///
    /// The last-resort mark for the desktop's own account capsule at the
    /// trailing end of the icon bar, and for anywhere else an account is
    /// shown with no picture behind it. An account that has a name resolves
    /// to its circular identity disc ([`monogram_disc`](crate::monogram_disc))
    /// instead, so this is reached only when no picture can be produced at
    /// all.
    User,
    /// A long-running system service bundle; shares the app-bundle glyph.
    ServiceBundle,
    /// An HTML document; shares the text glyph.
    TextHtml,
    /// A Rust source file; shares the text glyph.
    TextRust,
    /// A Java source file; shares the text glyph.
    TextJava,
    /// A shell script; shares the text glyph.
    ShellScript,
    /// A PDF document; shares the text glyph.
    Pdf,
    /// A PNG image; shares the image glyph.
    ImagePng,
    /// A JPEG image; shares the image glyph.
    ImageJpeg,
    /// A GIF image; shares the image glyph.
    ImageGif,
    /// An SVG image; shares the image glyph.
    ImageSvg,
    /// A RISC OS sprite image; shares the image glyph.
    ImageSprite,
    /// A drive whose medium is unknown or paravirtual: the generic disk
    /// glyph, drawn when nothing better can be said honestly.
    Disk,
    /// A rotational hard disk; shares the disk glyph.
    DiskHard,
    /// A solid-state disk; shares the disk glyph.
    DiskSolidState,
    /// A USB flash drive; shares the disk glyph.
    DiskUsb,
    /// An `NVMe` drive; shares the disk glyph.
    DiskNvme,
    /// An SD card; shares the disk glyph.
    DiskSdCard,
    /// An external USB drive that reported no rotation: the enclosure
    /// picture claims no medium; shares the disk glyph.
    DiskExternal,
    /// The fallback glyph for an unrecognised asset id: a filled diamond.
    Generic,
    /// Two upright bars, for pausing what is playing.
    Pause,
    /// A right-pointing triangle, for playing what is paused.
    Resume,
    /// A magnifier bearing a plus, for magnifying what is displayed.
    ZoomIn,
    /// A magnifier bearing a minus, for reducing what is displayed.
    ZoomOut,
    /// Four corner brackets, for scaling what is displayed to its window.
    ZoomFit,
    /// Callipers around a fixed box, for displaying at the true pixel size.
    ZoomActual,
    /// A clockwise half-turn arrow, for turning what is displayed right.
    RotateRight,
    /// An anticlockwise half-turn arrow, for turning what is displayed left.
    RotateLeft,
    /// Two arrowheads facing away from an axis, for mirroring what is
    /// displayed.
    Mirror,
    /// An `i` in a ring, for showing what is known about the thing on
    /// display.
    Info,
    /// A cog, for the desktop's settings surface and its General category.
    Settings,
    /// A half-filled ring, for the appearance (light/dark, contrast) category.
    Appearance,
    /// A framed landscape, for the desktop-backdrop category.
    Wallpaper,
    /// A monitor on a stand, for the attached-displays category.
    Display,
    /// A padlock, for the screen-lock category.
    LockScreen,
    /// A crescent moon beside two stars, for the screensaver category.
    Screensaver,
    /// The power symbol, for the machine's power category.
    Power,
    /// A globe, for the networking category, beside the tray's
    /// [`Network`](Self::Network) bars that read one link's signal.
    Networking,
    /// The Bluetooth rune, for the short-range-radio category.
    Bluetooth,
    /// A speaker sounding, for the audio category, beside the tray's
    /// [`Volume`](Self::Volume) reading.
    Sound,
    /// A bell, for the notification-policy category, beside the tray's
    /// pending-notification [`Bell`](Self::Bell).
    Notifications,
    /// A key bank, for the keyboard category.
    Keyboard,
    /// A mouse body and wheel, for the pointing-device category.
    Mouse,
    /// A pad with its click band, for the trackpad category.
    Trackpad,
    /// A touch on an upright screen, for the touch-input category.
    Touchscreen,
    /// A printer with a sheet through it, for the print and scan category.
    Printer,
    /// A figure with its arms open, for the accessibility category.
    Accessibility,
    /// A speech bubble bearing a letter, for the language and region
    /// category.
    Language,
    /// Joined nodes, for the file- and screen-sharing category.
    Sharing,
    /// Two busts, for the accounts and groups category, beside the single
    /// [`User`](Self::User) bust that stands for one account.
    Users,
    /// Stacked media, for the storage category, beside the
    /// [`Disk`](Self::Disk) family that stands for one drive.
    Storage,
    /// An `i` in a ring, for the pane saying what this machine is, beside the
    /// [`Info`](Self::Info) command glyph.
    About,
    /// An arrow entering a door, for the login and startup pane.
    Startup,
    /// A memory chip, for the pane bounding what may be kept as caches.
    Caching,
    /// A clock face, for the date and time pane.
    DateTime,
    /// A network plug, for the wired-interface pane.
    Ethernet,
    /// A radio fan over its source, for the wireless-network pane.
    WiFi,
    /// A signpost, for the name-resolution pane.
    Dns,
    /// Two opposed arrows, for the protocol-options pane.
    TcpIp,
    /// Three faders at different levels, for the sound devices' pane.
    SoundDevices,
    /// A beamed pair of notes, for the desktop's own sounds.
    SoundEffects,
    /// A painter's palette, for the desktop-theme category.
    Theme,
    /// A dashed rectangle, for choosing an area of a picture.
    ToolSelect,
    /// A pencil, for setting single pixels.
    ToolPencil,
    /// A paintbrush, for painting soft strokes.
    ToolBrush,
    /// A spray can and its spray, for airbrushed paint that builds up.
    ToolSpray,
    /// An eraser over the line it rubs out, for clearing paint.
    ToolEraser,
    /// A tipped paint bucket, for filling an area.
    ToolFill,
    /// An eyedropper, for taking a colour from a picture.
    ToolEyedropper,
    /// A line between two handles, for drawing straight lines.
    ToolLine,
    /// A rectangle's outline, for drawing rectangles.
    ToolRectangle,
    /// An ellipse's outline, for drawing ellipses.
    ToolEllipse,
    /// A ruled grid, for showing the boundaries between pixels.
    PixelGrid,
    /// A frame filled with bars narrowing across it, for laying a gradient.
    ToolGradient,
    /// A rubber stamp, for copying one part of a picture onto another.
    ToolClone,
    /// A capital letter, for setting text into a picture.
    ToolText,
    /// An irregular polygon's outline, for drawing polygons corner by corner.
    ToolPolygon,
    /// Two crossed crop marks, for cutting a picture down to a part of it.
    ToolCrop,
    /// An open hand, for dragging the view about.
    ToolHand,
    /// A magnifier centred on a point, for magnifying where it is pressed.
    ToolZoom,
    /// A desktop computer: a monitor on its stand beside a tower.
    Computer,
    /// A portable computer: an open lid over its keyboard deck.
    Laptop,
    /// A server: three stacked rack units, each with a drive slot and a light.
    Server,
    /// A router: a low box of status lights under two antennas.
    Router,
    /// A network switch: a row of ports between traffic passing both ways.
    NetworkSwitch,
    /// A phone or other handheld.
    Phone,
    /// Network-attached storage: a box of drive bays.
    Nas,
    /// An embedded, connected device, drawn as a smart bulb.
    IotDevice,
    /// A device on the network nothing has yet identified.
    DeviceUnknown,
    /// One network segment: hosts tapped onto a shared line.
    Subnet,
    /// Linux, drawn as a penguin.
    OsLinux,
    /// Windows, drawn as a four-paned window.
    OsWindows,
    /// Apple's systems, drawn as the looped square of a Mac's Command key.
    OsApple,
    /// Android, drawn as the head of its robot.
    OsAndroid,
    /// A cross on a red disc: something the user asked for did not happen.
    AlertError,
    /// An exclamation mark on an amber triangle: something will go wrong.
    AlertWarning,
    /// An `i` on a blue disc: something the user should know.
    AlertInformation,
    /// A question mark on a blue disc: something only the user can decide.
    AlertQuestion,
    /// An open folder, for opening an item or choosing a document to open.
    Open,
    /// A box with an arrow leaving its corner, for opening an item in another application.
    OpenWith,
    /// A window bearing a plus, for opening another window.
    NewWindow,
    /// A page bearing a plus, for starting a new document.
    NewDocument,
    /// A floppy disk, for saving a document.
    Save,
    /// A floppy disk under a pencil, for saving a document somewhere new.
    SaveAs,
    /// A window bearing a cross, for closing a window or a document.
    Close,
    /// A cross in a ring, for ending an application.
    Quit,
    /// A card of labelled lines, for an item's facts and settings.
    Properties,
    /// A page with a folded corner, for the document menu.
    Page,
    /// A pencil over a line, for the editing menu.
    Edit,
    /// An open eye, for showing something.
    Visible,
    /// An eye struck through, for hiding something.
    Hidden,
    /// Scissors, for cutting the selection to the clipboard.
    Cut,
    /// Two overlapping pages, for copying the selection.
    Copy,
    /// A clipboard, for pasting.
    Paste,
    /// A dashed frame around a filled square, for selecting everything.
    SelectAll,
    /// A dashed frame around a cross, for selecting nothing.
    SelectNone,
    /// A text field with its caret, for renaming.
    Rename,
    /// Two overlapping pages, the front bearing a plus, for duplicating.
    Duplicate,
    /// A waste bin, for destroying an item.
    Delete,
    /// A minus in a ring, for taking an item out of a list.
    Remove,
    /// An eraser, for emptying a screen or a list.
    Clear,
    /// An arrow curving back, for undoing.
    Undo,
    /// An arrow curving on, for redoing.
    Redo,
    /// Lines moved in past a chevron, for indenting.
    Indent,
    /// Lines with a chevron pointing out, for outdenting.
    Outdent,
    /// A pair of slashes, for commenting lines out.
    Comment,
    /// A letter beside a block caret, for typing over text.
    Overwrite,
    /// A magnifier, for finding text.
    Search,
    /// Two boxes changing places, for finding and replacing.
    Replace,
    /// A magnifier beside a down arrow, for the next match.
    FindNext,
    /// A magnifier beside an up arrow, for the previous match.
    FindPrevious,
    /// A number sign, for going to a numbered place.
    GoTo,
    /// A warning triangle, for the next problem.
    Problem,
    /// Lines of text, for showing a document as text.
    ViewText,
    /// Binary digits, for showing a document's bytes.
    ViewHex,
    /// Braces around a run of values, for showing a document's structure.
    ViewData,
    /// Angle brackets around a slash, for showing machine code.
    ViewCode,
    /// A page bearing angle brackets, for the syntax a document is coloured as.
    Syntax,
    /// An arrow meeting a stop, for tab stops.
    TabWidth,
    /// A pilcrow, for how lines end.
    LineEndings,
    /// A double arrow between two bars, for fitting the width.
    FitWidth,
    /// A letter beside a plus, for larger text.
    TextLarger,
    /// A letter beside a minus, for smaller text.
    TextSmaller,
    /// A letter beside an equals sign, for text at its own size.
    TextActual,
    /// A half-turn arrow, for turning half a turn.
    RotateHalf,
    /// Two arrowheads facing away from a level axis, for mirroring top to bottom.
    FlipVertical,
    /// A ruled frame, for a grid.
    Grid,
    /// A window with a side pane, for the panes.
    Panes,
    /// An arrow turning back round, for putting the panes back.
    ResetPanes,
    /// A window divided into panels, for how a window shows its items.
    Layout,
    /// Four large tiles, for large icons.
    LargeIcons,
    /// Small tiles beside their names in two columns, for small icons.
    SmallIcons,
    /// Rows of points and lines, for a list.
    ListView,
    /// Letters beside a down arrow, for ordering by name.
    SortByName,
    /// A triangle, a circle and a square, for ordering by kind.
    SortByKind,
    /// Shortening bars beside a down arrow, for ordering by size.
    SortBySize,
    /// A calendar page, for a date.
    Calendar,
    /// Blocks against a left edge, for arranging from the left.
    ArrangeLeft,
    /// Blocks against a right edge, for arranging from the right.
    ArrangeRight,
    /// A curved arrow in a box, for a desktop shortcut.
    Shortcut,
    /// A house, for the user's home folder.
    Home,
    /// An arrow up, for moving up.
    MoveUp,
    /// An arrow down, for moving down.
    MoveDown,
    /// Stacked sheets seen edge on, for layers.
    Layers,
    /// Stacked sheets bearing a plus, for a new layer.
    LayerNew,
    /// An arrow above a sheet, for the layer above.
    LayerAbove,
    /// An arrow below a sheet, for the layer below.
    LayerBelow,
    /// A solid square in front of an open one, for raising a layer.
    LayerRaise,
    /// An open square in front of a solid one, for lowering a layer.
    LayerLower,
    /// Two arrows joining into one, for merging down.
    MergeDown,
    /// Two arrows pressing onto a line, for flattening.
    Flatten,
    /// Arrows pointing out of opposite corners, for resizing.
    Resize,
    /// A picture inside a larger frame, for the canvas size.
    CanvasSize,
    /// A square half filled across its diagonal, for inverting colours.
    Invert,
    /// Three overlapping circles, for how colours are held.
    ColourModel,
    /// A frame around a disc, bearing a plus, for adding a mask.
    MaskAdd,
    /// A frame around a disc, bearing a minus, for removing a mask.
    MaskRemove,
    /// An anchor, for putting down what is carried.
    PutDown,
    /// A filled circle in front of an open one, for the primary colour.
    ColourPrimary,
    /// An open circle in front of a filled one, for the secondary colour.
    ColourSecondary,
    /// Two arrows passing each other, for swapping the colours.
    ColourSwap,
    /// A filled and an open square, for the default colours.
    ColourReset,
    /// Three sliders, for adjustments.
    Sliders,
    /// A sun, for brightness and contrast.
    Brightness,
    /// A drop, for hue and saturation.
    Droplet,
    /// A disc cut into thirds, for the balance of colours.
    ColourBalance,
    /// The bars of a histogram, for levels.
    Levels,
    /// A curve rising across a frame, for curves.
    Curves,
    /// A thermometer, for white balance.
    WhiteBalance,
    /// A staircase, for posterising.
    Posterise,
    /// A frame filled on one side, for a threshold.
    Threshold,
    /// A drop struck through, for taking the colour out.
    Desaturate,
    /// A ring of dots, for blurring.
    Blur,
    /// A peak, for sharpening.
    Sharpen,
    /// A chequered frame, for pixelating.
    Pixelate,
    /// Scattered dots, for noise.
    Noise,
    /// A diamond between corner marks, for finding edges.
    FindEdges,
    /// Two pages, one behind the other, for sprites and pages.
    Pages,
    /// A toolbox, for the tools.
    Toolbox,
    /// The trace of a pulse, for watching what the machine is doing.
    Activity,
    /// A prompt in a window, for a shell.
    Terminal,
    /// An arrow leaving a door frame, for ending a session.
    LogOut,
    /// A turning arrow around a bar, for restarting.
    Restart,
    /// A window in front of another, for bringing a window forward.
    SwitchTo,
    /// A dial with its needle low, for lowering priority.
    Gauge,
    /// A page of lines, for logs.
    Logs,
    /// A cross in an octagon, for ending a task at once.
    ForceQuit,
    /// A loop of arrows struck through, for playing once.
    RepeatOff,
    /// A waveform, for evening out loudness.
    Loudness,
    /// A flag on its pole, for a new game.
    Flag,
    /// Three rising bars, the first filled, for the easiest level.
    LevelEasy,
    /// Three rising bars, two filled, for the middle level.
    LevelMedium,
    /// Three rising bars, all filled, for the hardest level.
    LevelHard,
    /// An open door, for letting the companion out.
    DoorOpen,
}

impl IconKind {
    /// Resolve a theme asset identifier to a glyph, falling back to
    /// [`Generic`](Self::Generic) for an unknown id so an unexpected
    /// notification still draws a placeholder.
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "one arm a kind; splitting it would part the kinds from the one table they are spelled by"
    )]
    pub fn for_asset(asset: &str) -> Self {
        match asset {
            "network" => Self::Network,
            "volume" => Self::Volume,
            "battery" => Self::Battery,
            "bell" => Self::Bell,
            "folder" => Self::Folder,
            "folder-open" => Self::FolderOpen,
            "folder-filled" => Self::FolderFilled,
            "folder-back" => Self::FolderBack,
            "folder-front" => Self::FolderFront,
            "audio" => Self::Audio,
            "video" => Self::Video,
            "skip-previous" => Self::SkipPrevious,
            "skip-next" => Self::SkipNext,
            "shuffle" => Self::Shuffle,
            "repeat" => Self::Repeat,
            "repeat-one" => Self::RepeatOne,
            "microphone" => Self::Microphone,
            "volume-muted" => Self::VolumeMuted,
            "network-offline" => Self::NetworkOffline,
            "file" => Self::File,
            "app-bundle" => Self::AppBundle,
            "text" => Self::Text,
            "image" => Self::Image,
            "archive" => Self::Archive,
            "executable" => Self::Executable,
            "nav-back" => Self::NavBack,
            "nav-forward" => Self::NavForward,
            "nav-up" => Self::NavUp,
            "refresh" => Self::Refresh,
            "view-toggle" => Self::ViewToggle,
            "sort" => Self::Sort,
            "new-folder" => Self::NewFolder,
            "trash" => Self::Trash,
            "empty-trash" => Self::EmptyTrash,
            "library" => Self::Library,
            "user" => Self::User,
            "service-bundle" => Self::ServiceBundle,
            "text-html" => Self::TextHtml,
            "text-x-rust" => Self::TextRust,
            "text-x-java" => Self::TextJava,
            "application-x-shellscript" => Self::ShellScript,
            "application-pdf" => Self::Pdf,
            "image-png" => Self::ImagePng,
            "image-jpeg" => Self::ImageJpeg,
            "image-gif" => Self::ImageGif,
            "image-svg-xml" => Self::ImageSvg,
            "image-x-riscos-sprite" => Self::ImageSprite,
            "disk" => Self::Disk,
            "disk-hard" => Self::DiskHard,
            "disk-solid-state" => Self::DiskSolidState,
            "disk-usb" => Self::DiskUsb,
            "disk-nvme" => Self::DiskNvme,
            "disk-sd" => Self::DiskSdCard,
            "disk-external" => Self::DiskExternal,
            "pause" => Self::Pause,
            "resume" => Self::Resume,
            "zoom-in" => Self::ZoomIn,
            "zoom-out" => Self::ZoomOut,
            "zoom-fit" => Self::ZoomFit,
            "zoom-actual" => Self::ZoomActual,
            "rotate-right" => Self::RotateRight,
            "rotate-left" => Self::RotateLeft,
            "mirror" => Self::Mirror,
            "info" => Self::Info,
            "settings" => Self::Settings,
            "appearance" => Self::Appearance,
            "wallpaper" => Self::Wallpaper,
            "display" => Self::Display,
            "lock-screen" => Self::LockScreen,
            "screensaver" => Self::Screensaver,
            "power" => Self::Power,
            "networking" => Self::Networking,
            "bluetooth" => Self::Bluetooth,
            "sound" => Self::Sound,
            "notifications" => Self::Notifications,
            "keyboard" => Self::Keyboard,
            "mouse" => Self::Mouse,
            "trackpad" => Self::Trackpad,
            "touchscreen" => Self::Touchscreen,
            "printer" => Self::Printer,
            "accessibility" => Self::Accessibility,
            "language" => Self::Language,
            "sharing" => Self::Sharing,
            "users" => Self::Users,
            "storage" => Self::Storage,
            "about" => Self::About,
            "startup" => Self::Startup,
            "caching" => Self::Caching,
            "date-time" => Self::DateTime,
            "ethernet" => Self::Ethernet,
            "wifi" => Self::WiFi,
            "dns" => Self::Dns,
            "tcp-ip" => Self::TcpIp,
            "sound-devices" => Self::SoundDevices,
            "sound-effects" => Self::SoundEffects,
            "theme" => Self::Theme,
            "tool-select" => Self::ToolSelect,
            "tool-pencil" => Self::ToolPencil,
            "tool-brush" => Self::ToolBrush,
            "tool-spray" => Self::ToolSpray,
            "tool-eraser" => Self::ToolEraser,
            "tool-fill" => Self::ToolFill,
            "tool-eyedropper" => Self::ToolEyedropper,
            "tool-line" => Self::ToolLine,
            "tool-rectangle" => Self::ToolRectangle,
            "tool-ellipse" => Self::ToolEllipse,
            "pixel-grid" => Self::PixelGrid,
            "tool-gradient" => Self::ToolGradient,
            "tool-clone" => Self::ToolClone,
            "tool-text" => Self::ToolText,
            "tool-polygon" => Self::ToolPolygon,
            "tool-crop" => Self::ToolCrop,
            "tool-hand" => Self::ToolHand,
            "tool-zoom" => Self::ToolZoom,
            "computer" => Self::Computer,
            "laptop" => Self::Laptop,
            "server" => Self::Server,
            "router" => Self::Router,
            "network-switch" => Self::NetworkSwitch,
            "phone" => Self::Phone,
            "nas" => Self::Nas,
            "iot-device" => Self::IotDevice,
            "device-unknown" => Self::DeviceUnknown,
            "subnet" => Self::Subnet,
            "os-linux" => Self::OsLinux,
            "os-windows" => Self::OsWindows,
            "os-apple" => Self::OsApple,
            "os-android" => Self::OsAndroid,
            "alert-error" => Self::AlertError,
            "alert-warning" => Self::AlertWarning,
            "alert-information" => Self::AlertInformation,
            "alert-question" => Self::AlertQuestion,
            "open" => Self::Open,
            "open-with" => Self::OpenWith,
            "new-window" => Self::NewWindow,
            "new-document" => Self::NewDocument,
            "save" => Self::Save,
            "save-as" => Self::SaveAs,
            "close" => Self::Close,
            "quit" => Self::Quit,
            "properties" => Self::Properties,
            "page" => Self::Page,
            "edit" => Self::Edit,
            "visible" => Self::Visible,
            "hidden" => Self::Hidden,
            "cut" => Self::Cut,
            "copy" => Self::Copy,
            "paste" => Self::Paste,
            "select-all" => Self::SelectAll,
            "select-none" => Self::SelectNone,
            "rename" => Self::Rename,
            "duplicate" => Self::Duplicate,
            "delete" => Self::Delete,
            "remove" => Self::Remove,
            "clear" => Self::Clear,
            "undo" => Self::Undo,
            "redo" => Self::Redo,
            "indent" => Self::Indent,
            "outdent" => Self::Outdent,
            "comment" => Self::Comment,
            "overwrite" => Self::Overwrite,
            "search" => Self::Search,
            "replace" => Self::Replace,
            "find-next" => Self::FindNext,
            "find-previous" => Self::FindPrevious,
            "go-to" => Self::GoTo,
            "problem" => Self::Problem,
            "view-text" => Self::ViewText,
            "view-hex" => Self::ViewHex,
            "view-data" => Self::ViewData,
            "view-code" => Self::ViewCode,
            "syntax" => Self::Syntax,
            "tab-width" => Self::TabWidth,
            "line-endings" => Self::LineEndings,
            "fit-width" => Self::FitWidth,
            "text-larger" => Self::TextLarger,
            "text-smaller" => Self::TextSmaller,
            "text-actual" => Self::TextActual,
            "rotate-half" => Self::RotateHalf,
            "flip-vertical" => Self::FlipVertical,
            "grid" => Self::Grid,
            "panes" => Self::Panes,
            "reset-panes" => Self::ResetPanes,
            "layout" => Self::Layout,
            "large-icons" => Self::LargeIcons,
            "small-icons" => Self::SmallIcons,
            "list-view" => Self::ListView,
            "sort-by-name" => Self::SortByName,
            "sort-by-kind" => Self::SortByKind,
            "sort-by-size" => Self::SortBySize,
            "calendar" => Self::Calendar,
            "arrange-left" => Self::ArrangeLeft,
            "arrange-right" => Self::ArrangeRight,
            "shortcut" => Self::Shortcut,
            "home" => Self::Home,
            "move-up" => Self::MoveUp,
            "move-down" => Self::MoveDown,
            "layers" => Self::Layers,
            "layer-new" => Self::LayerNew,
            "layer-above" => Self::LayerAbove,
            "layer-below" => Self::LayerBelow,
            "layer-raise" => Self::LayerRaise,
            "layer-lower" => Self::LayerLower,
            "merge-down" => Self::MergeDown,
            "flatten" => Self::Flatten,
            "resize" => Self::Resize,
            "canvas-size" => Self::CanvasSize,
            "invert" => Self::Invert,
            "colour-model" => Self::ColourModel,
            "mask-add" => Self::MaskAdd,
            "mask-remove" => Self::MaskRemove,
            "put-down" => Self::PutDown,
            "colour-primary" => Self::ColourPrimary,
            "colour-secondary" => Self::ColourSecondary,
            "colour-swap" => Self::ColourSwap,
            "colour-reset" => Self::ColourReset,
            "sliders" => Self::Sliders,
            "brightness" => Self::Brightness,
            "droplet" => Self::Droplet,
            "colour-balance" => Self::ColourBalance,
            "levels" => Self::Levels,
            "curves" => Self::Curves,
            "white-balance" => Self::WhiteBalance,
            "posterise" => Self::Posterise,
            "threshold" => Self::Threshold,
            "desaturate" => Self::Desaturate,
            "blur" => Self::Blur,
            "sharpen" => Self::Sharpen,
            "pixelate" => Self::Pixelate,
            "noise" => Self::Noise,
            "find-edges" => Self::FindEdges,
            "pages" => Self::Pages,
            "toolbox" => Self::Toolbox,
            "activity" => Self::Activity,
            "terminal" => Self::Terminal,
            "log-out" => Self::LogOut,
            "restart" => Self::Restart,
            "switch-to" => Self::SwitchTo,
            "gauge" => Self::Gauge,
            "logs" => Self::Logs,
            "force-quit" => Self::ForceQuit,
            "repeat-off" => Self::RepeatOff,
            "loudness" => Self::Loudness,
            "flag" => Self::Flag,
            "level-easy" => Self::LevelEasy,
            "level-medium" => Self::LevelMedium,
            "level-hard" => Self::LevelHard,
            "door-open" => Self::DoorOpen,
            _ => Self::Generic,
        }
    }

    /// This kind's stable index into the closed [`ICON_KINDS`] table, so an
    /// [`IconSet`] can store one slot per kind by position rather than a field
    /// per kind. The identity `ICON_KINDS[kind.index()] == kind` holds for
    /// every kind.
    ///
    /// [`ICON_KINDS`]: crate::load::ICON_KINDS
    /// [`IconSet`]: crate::load::IconSet
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "one arm a kind; splitting it would part the kinds from the one table they are spelled by"
    )]
    pub const fn index(self) -> usize {
        match self {
            Self::Network => 0,
            Self::Volume => 1,
            Self::Battery => 2,
            Self::Bell => 3,
            Self::Folder => 4,
            Self::FolderOpen => 5,
            Self::File => 6,
            Self::AppBundle => 7,
            Self::Text => 8,
            Self::Image => 9,
            Self::Archive => 10,
            Self::Executable => 11,
            Self::NavBack => 12,
            Self::NavForward => 13,
            Self::NavUp => 14,
            Self::Refresh => 15,
            Self::ViewToggle => 16,
            Self::Sort => 17,
            Self::NewFolder => 18,
            Self::Generic => 19,
            Self::Trash => 20,
            Self::EmptyTrash => 21,
            Self::Library => 22,
            Self::User => 23,
            Self::ServiceBundle => 24,
            Self::TextHtml => 25,
            Self::TextRust => 26,
            Self::TextJava => 27,
            Self::ShellScript => 28,
            Self::Pdf => 29,
            Self::ImagePng => 30,
            Self::ImageJpeg => 31,
            Self::ImageGif => 32,
            Self::ImageSvg => 33,
            Self::ImageSprite => 34,
            Self::Disk => 35,
            Self::DiskHard => 36,
            Self::DiskSolidState => 37,
            Self::DiskUsb => 38,
            Self::Pause => 39,
            Self::Resume => 40,
            Self::FolderFilled => 41,
            Self::ZoomIn => 42,
            Self::ZoomOut => 43,
            Self::ZoomFit => 44,
            Self::ZoomActual => 45,
            Self::RotateRight => 46,
            Self::RotateLeft => 47,
            Self::Mirror => 48,
            Self::Info => 49,
            Self::Settings => 50,
            Self::Appearance => 51,
            Self::Wallpaper => 52,
            Self::Display => 53,
            Self::LockScreen => 54,
            Self::Screensaver => 55,
            Self::Power => 56,
            Self::Bluetooth => 57,
            Self::Sound => 58,
            Self::Notifications => 59,
            Self::Keyboard => 60,
            Self::Mouse => 61,
            Self::Trackpad => 62,
            Self::Touchscreen => 63,
            Self::Printer => 64,
            Self::Accessibility => 65,
            Self::Language => 66,
            Self::Sharing => 67,
            Self::Users => 68,
            Self::Storage => 69,
            Self::Networking => 70,
            Self::About => 71,
            Self::Startup => 72,
            Self::Caching => 73,
            Self::DateTime => 74,
            Self::Ethernet => 75,
            Self::WiFi => 76,
            Self::Dns => 77,
            Self::TcpIp => 78,
            Self::Theme => 79,
            Self::ToolSelect => 80,
            Self::ToolPencil => 81,
            Self::ToolBrush => 82,
            Self::ToolSpray => 83,
            Self::ToolEraser => 84,
            Self::ToolFill => 85,
            Self::ToolEyedropper => 86,
            Self::ToolLine => 87,
            Self::ToolRectangle => 88,
            Self::ToolEllipse => 89,
            Self::PixelGrid => 90,
            Self::ToolGradient => 91,
            Self::ToolClone => 92,
            Self::ToolText => 93,
            Self::ToolPolygon => 94,
            Self::ToolCrop => 95,
            Self::ToolHand => 96,
            Self::ToolZoom => 97,
            Self::FolderBack => 98,
            Self::FolderFront => 99,
            Self::Audio => 100,
            Self::Video => 101,
            Self::SkipPrevious => 102,
            Self::SkipNext => 103,
            Self::Shuffle => 104,
            Self::Repeat => 105,
            Self::RepeatOne => 106,
            Self::Microphone => 107,
            Self::VolumeMuted => 108,
            Self::NetworkOffline => 109,
            Self::SoundDevices => 110,
            Self::SoundEffects => 111,
            Self::Computer => 112,
            Self::Laptop => 113,
            Self::Server => 114,
            Self::Router => 115,
            Self::NetworkSwitch => 116,
            Self::Phone => 117,
            Self::Nas => 118,
            Self::IotDevice => 119,
            Self::DeviceUnknown => 120,
            Self::Subnet => 121,
            Self::OsLinux => 122,
            Self::OsWindows => 123,
            Self::OsApple => 124,
            Self::OsAndroid => 125,
            Self::DiskNvme => 126,
            Self::DiskSdCard => 127,
            Self::DiskExternal => 128,
            Self::AlertError => 129,
            Self::AlertWarning => 130,
            Self::AlertInformation => 131,
            Self::AlertQuestion => 132,
            Self::Open => 133,
            Self::OpenWith => 134,
            Self::NewWindow => 135,
            Self::NewDocument => 136,
            Self::Save => 137,
            Self::SaveAs => 138,
            Self::Close => 139,
            Self::Quit => 140,
            Self::Properties => 141,
            Self::Page => 142,
            Self::Edit => 143,
            Self::Visible => 144,
            Self::Hidden => 145,
            Self::Cut => 146,
            Self::Copy => 147,
            Self::Paste => 148,
            Self::SelectAll => 149,
            Self::SelectNone => 150,
            Self::Rename => 151,
            Self::Duplicate => 152,
            Self::Delete => 153,
            Self::Remove => 154,
            Self::Clear => 155,
            Self::Undo => 156,
            Self::Redo => 157,
            Self::Indent => 158,
            Self::Outdent => 159,
            Self::Comment => 160,
            Self::Overwrite => 161,
            Self::Search => 162,
            Self::Replace => 163,
            Self::FindNext => 164,
            Self::FindPrevious => 165,
            Self::GoTo => 166,
            Self::Problem => 167,
            Self::ViewText => 168,
            Self::ViewHex => 169,
            Self::ViewData => 170,
            Self::ViewCode => 171,
            Self::Syntax => 172,
            Self::TabWidth => 173,
            Self::LineEndings => 174,
            Self::FitWidth => 175,
            Self::TextLarger => 176,
            Self::TextSmaller => 177,
            Self::TextActual => 178,
            Self::RotateHalf => 179,
            Self::FlipVertical => 180,
            Self::Grid => 181,
            Self::Panes => 182,
            Self::ResetPanes => 183,
            Self::Layout => 184,
            Self::LargeIcons => 185,
            Self::SmallIcons => 186,
            Self::ListView => 187,
            Self::SortByName => 188,
            Self::SortByKind => 189,
            Self::SortBySize => 190,
            Self::Calendar => 191,
            Self::ArrangeLeft => 192,
            Self::ArrangeRight => 193,
            Self::Shortcut => 194,
            Self::Home => 195,
            Self::MoveUp => 196,
            Self::MoveDown => 197,
            Self::Layers => 198,
            Self::LayerNew => 199,
            Self::LayerAbove => 200,
            Self::LayerBelow => 201,
            Self::LayerRaise => 202,
            Self::LayerLower => 203,
            Self::MergeDown => 204,
            Self::Flatten => 205,
            Self::Resize => 206,
            Self::CanvasSize => 207,
            Self::Invert => 208,
            Self::ColourModel => 209,
            Self::MaskAdd => 210,
            Self::MaskRemove => 211,
            Self::PutDown => 212,
            Self::ColourPrimary => 213,
            Self::ColourSecondary => 214,
            Self::ColourSwap => 215,
            Self::ColourReset => 216,
            Self::Sliders => 217,
            Self::Brightness => 218,
            Self::Droplet => 219,
            Self::ColourBalance => 220,
            Self::Levels => 221,
            Self::Curves => 222,
            Self::WhiteBalance => 223,
            Self::Posterise => 224,
            Self::Threshold => 225,
            Self::Desaturate => 226,
            Self::Blur => 227,
            Self::Sharpen => 228,
            Self::Pixelate => 229,
            Self::Noise => 230,
            Self::FindEdges => 231,
            Self::Pages => 232,
            Self::Toolbox => 233,
            Self::Activity => 234,
            Self::Terminal => 235,
            Self::LogOut => 236,
            Self::Restart => 237,
            Self::SwitchTo => 238,
            Self::Gauge => 239,
            Self::Logs => 240,
            Self::ForceQuit => 241,
            Self::RepeatOff => 242,
            Self::Loudness => 243,
            Self::Flag => 244,
            Self::LevelEasy => 245,
            Self::LevelMedium => 246,
            Self::LevelHard => 247,
            Self::DoorOpen => 248,
        }
    }

    /// The canonical asset identifier for this kind — the inverse of
    /// [`for_asset`](Self::for_asset).
    ///
    /// A desktop loader names a kind's on-disk SVG asset by this id, so the
    /// id↔kind mapping lives in one place rather than being restated at the
    /// load site. The round trip holds for every kind:
    /// `IconKind::for_asset(kind.asset_id()) == kind`.
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "one arm a kind; splitting it would part the kinds from the one table they are spelled by"
    )]
    pub fn asset_id(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Volume => "volume",
            Self::Battery => "battery",
            Self::Bell => "bell",
            Self::Folder => "folder",
            Self::FolderOpen => "folder-open",
            Self::File => "file",
            Self::AppBundle => "app-bundle",
            Self::Text => "text",
            Self::Image => "image",
            Self::Archive => "archive",
            Self::Executable => "executable",
            Self::NavBack => "nav-back",
            Self::NavForward => "nav-forward",
            Self::NavUp => "nav-up",
            Self::Refresh => "refresh",
            Self::ViewToggle => "view-toggle",
            Self::Sort => "sort",
            Self::NewFolder => "new-folder",
            Self::Generic => "generic",
            Self::Trash => "trash",
            Self::EmptyTrash => "empty-trash",
            Self::Library => "library",
            Self::User => "user",
            Self::ServiceBundle => "service-bundle",
            Self::TextHtml => "text-html",
            Self::TextRust => "text-x-rust",
            Self::TextJava => "text-x-java",
            Self::ShellScript => "application-x-shellscript",
            Self::Pdf => "application-pdf",
            Self::ImagePng => "image-png",
            Self::ImageJpeg => "image-jpeg",
            Self::ImageGif => "image-gif",
            Self::ImageSvg => "image-svg-xml",
            Self::ImageSprite => "image-x-riscos-sprite",
            Self::Disk => "disk",
            Self::DiskHard => "disk-hard",
            Self::DiskSolidState => "disk-solid-state",
            Self::DiskUsb => "disk-usb",
            Self::DiskNvme => "disk-nvme",
            Self::DiskSdCard => "disk-sd",
            Self::DiskExternal => "disk-external",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::FolderFilled => "folder-filled",
            Self::ZoomIn => "zoom-in",
            Self::ZoomOut => "zoom-out",
            Self::ZoomFit => "zoom-fit",
            Self::ZoomActual => "zoom-actual",
            Self::RotateRight => "rotate-right",
            Self::RotateLeft => "rotate-left",
            Self::Mirror => "mirror",
            Self::Info => "info",
            Self::Settings => "settings",
            Self::Appearance => "appearance",
            Self::Wallpaper => "wallpaper",
            Self::Display => "display",
            Self::LockScreen => "lock-screen",
            Self::Screensaver => "screensaver",
            Self::Power => "power",
            Self::Networking => "networking",
            Self::Bluetooth => "bluetooth",
            Self::Sound => "sound",
            Self::Notifications => "notifications",
            Self::Keyboard => "keyboard",
            Self::Mouse => "mouse",
            Self::Trackpad => "trackpad",
            Self::Touchscreen => "touchscreen",
            Self::Printer => "printer",
            Self::Accessibility => "accessibility",
            Self::Language => "language",
            Self::Sharing => "sharing",
            Self::Users => "users",
            Self::Storage => "storage",
            Self::About => "about",
            Self::Startup => "startup",
            Self::Caching => "caching",
            Self::DateTime => "date-time",
            Self::Ethernet => "ethernet",
            Self::WiFi => "wifi",
            Self::Dns => "dns",
            Self::TcpIp => "tcp-ip",
            Self::SoundDevices => "sound-devices",
            Self::SoundEffects => "sound-effects",
            Self::Theme => "theme",
            Self::ToolSelect => "tool-select",
            Self::ToolPencil => "tool-pencil",
            Self::ToolBrush => "tool-brush",
            Self::ToolSpray => "tool-spray",
            Self::ToolEraser => "tool-eraser",
            Self::ToolFill => "tool-fill",
            Self::ToolEyedropper => "tool-eyedropper",
            Self::ToolLine => "tool-line",
            Self::ToolRectangle => "tool-rectangle",
            Self::ToolEllipse => "tool-ellipse",
            Self::PixelGrid => "pixel-grid",
            Self::ToolGradient => "tool-gradient",
            Self::ToolClone => "tool-clone",
            Self::ToolText => "tool-text",
            Self::ToolPolygon => "tool-polygon",
            Self::ToolCrop => "tool-crop",
            Self::ToolHand => "tool-hand",
            Self::ToolZoom => "tool-zoom",
            Self::Computer => "computer",
            Self::Laptop => "laptop",
            Self::Server => "server",
            Self::Router => "router",
            Self::NetworkSwitch => "network-switch",
            Self::Phone => "phone",
            Self::Nas => "nas",
            Self::IotDevice => "iot-device",
            Self::DeviceUnknown => "device-unknown",
            Self::Subnet => "subnet",
            Self::OsLinux => "os-linux",
            Self::OsWindows => "os-windows",
            Self::OsApple => "os-apple",
            Self::OsAndroid => "os-android",
            Self::AlertError => "alert-error",
            Self::AlertWarning => "alert-warning",
            Self::AlertInformation => "alert-information",
            Self::AlertQuestion => "alert-question",
            Self::FolderBack => "folder-back",
            Self::FolderFront => "folder-front",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::SkipPrevious => "skip-previous",
            Self::SkipNext => "skip-next",
            Self::Shuffle => "shuffle",
            Self::Repeat => "repeat",
            Self::RepeatOne => "repeat-one",
            Self::Microphone => "microphone",
            Self::VolumeMuted => "volume-muted",
            Self::NetworkOffline => "network-offline",
            Self::Open => "open",
            Self::OpenWith => "open-with",
            Self::NewWindow => "new-window",
            Self::NewDocument => "new-document",
            Self::Save => "save",
            Self::SaveAs => "save-as",
            Self::Close => "close",
            Self::Quit => "quit",
            Self::Properties => "properties",
            Self::Page => "page",
            Self::Edit => "edit",
            Self::Visible => "visible",
            Self::Hidden => "hidden",
            Self::Cut => "cut",
            Self::Copy => "copy",
            Self::Paste => "paste",
            Self::SelectAll => "select-all",
            Self::SelectNone => "select-none",
            Self::Rename => "rename",
            Self::Duplicate => "duplicate",
            Self::Delete => "delete",
            Self::Remove => "remove",
            Self::Clear => "clear",
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::Indent => "indent",
            Self::Outdent => "outdent",
            Self::Comment => "comment",
            Self::Overwrite => "overwrite",
            Self::Search => "search",
            Self::Replace => "replace",
            Self::FindNext => "find-next",
            Self::FindPrevious => "find-previous",
            Self::GoTo => "go-to",
            Self::Problem => "problem",
            Self::ViewText => "view-text",
            Self::ViewHex => "view-hex",
            Self::ViewData => "view-data",
            Self::ViewCode => "view-code",
            Self::Syntax => "syntax",
            Self::TabWidth => "tab-width",
            Self::LineEndings => "line-endings",
            Self::FitWidth => "fit-width",
            Self::TextLarger => "text-larger",
            Self::TextSmaller => "text-smaller",
            Self::TextActual => "text-actual",
            Self::RotateHalf => "rotate-half",
            Self::FlipVertical => "flip-vertical",
            Self::Grid => "grid",
            Self::Panes => "panes",
            Self::ResetPanes => "reset-panes",
            Self::Layout => "layout",
            Self::LargeIcons => "large-icons",
            Self::SmallIcons => "small-icons",
            Self::ListView => "list-view",
            Self::SortByName => "sort-by-name",
            Self::SortByKind => "sort-by-kind",
            Self::SortBySize => "sort-by-size",
            Self::Calendar => "calendar",
            Self::ArrangeLeft => "arrange-left",
            Self::ArrangeRight => "arrange-right",
            Self::Shortcut => "shortcut",
            Self::Home => "home",
            Self::MoveUp => "move-up",
            Self::MoveDown => "move-down",
            Self::Layers => "layers",
            Self::LayerNew => "layer-new",
            Self::LayerAbove => "layer-above",
            Self::LayerBelow => "layer-below",
            Self::LayerRaise => "layer-raise",
            Self::LayerLower => "layer-lower",
            Self::MergeDown => "merge-down",
            Self::Flatten => "flatten",
            Self::Resize => "resize",
            Self::CanvasSize => "canvas-size",
            Self::Invert => "invert",
            Self::ColourModel => "colour-model",
            Self::MaskAdd => "mask-add",
            Self::MaskRemove => "mask-remove",
            Self::PutDown => "put-down",
            Self::ColourPrimary => "colour-primary",
            Self::ColourSecondary => "colour-secondary",
            Self::ColourSwap => "colour-swap",
            Self::ColourReset => "colour-reset",
            Self::Sliders => "sliders",
            Self::Brightness => "brightness",
            Self::Droplet => "droplet",
            Self::ColourBalance => "colour-balance",
            Self::Levels => "levels",
            Self::Curves => "curves",
            Self::WhiteBalance => "white-balance",
            Self::Posterise => "posterise",
            Self::Threshold => "threshold",
            Self::Desaturate => "desaturate",
            Self::Blur => "blur",
            Self::Sharpen => "sharpen",
            Self::Pixelate => "pixelate",
            Self::Noise => "noise",
            Self::FindEdges => "find-edges",
            Self::Pages => "pages",
            Self::Toolbox => "toolbox",
            Self::Activity => "activity",
            Self::Terminal => "terminal",
            Self::LogOut => "log-out",
            Self::Restart => "restart",
            Self::SwitchTo => "switch-to",
            Self::Gauge => "gauge",
            Self::Logs => "logs",
            Self::ForceQuit => "force-quit",
            Self::RepeatOff => "repeat-off",
            Self::Loudness => "loudness",
            Self::Flag => "flag",
            Self::LevelEasy => "level-easy",
            Self::LevelMedium => "level-medium",
            Self::LevelHard => "level-hard",
            Self::DoorOpen => "door-open",
        }
    }
}

/// The icon that represents a mounted volume's storage medium.
///
/// A picture claims only what the device reported: a medium reported
/// rotating is the hard disk on an ATA bus or behind a USB bridge alike, a
/// USB unit is a stick once it reported a non-rotating medium, and one that
/// reported neither is the external drive, which shows no medium. An
/// undeclared or paravirtual device is the generic drive.
#[must_use]
pub const fn disk_icon(medium: Option<BlkMedium>) -> IconKind {
    let Some(medium) = medium else {
        return IconKind::Disk;
    };
    match (medium.transport, medium.rotation) {
        (BlkTransport::Virtual, _) | (BlkTransport::Ata, BlkRotation::Unknown) => IconKind::Disk,
        (BlkTransport::Nvme, _) => IconKind::DiskNvme,
        (BlkTransport::Sd, _) => IconKind::DiskSdCard,
        (BlkTransport::Ata | BlkTransport::Usb, BlkRotation::Rotating { .. }) => IconKind::DiskHard,
        (BlkTransport::Ata, BlkRotation::NonRotating(_)) => IconKind::DiskSolidState,
        (BlkTransport::Usb, BlkRotation::NonRotating(_)) => IconKind::DiskUsb,
        (BlkTransport::Usb, BlkRotation::Unknown) => IconKind::DiskExternal,
    }
}

/// Build the built-in glyph for `kind`, tinted with `color`.
///
/// The returned [`VectorIcon`] is authored on a fixed square design grid, for
/// a caller that scales it; one drawn at a known pixel side is fitted to that
/// side's grid instead ([`glyph_mask`](crate::artwork::glyph_mask)).
#[must_use]
pub fn builtin_icon(kind: IconKind, color: Color) -> VectorIcon {
    match polygons(kind, color) {
        Some(layers) => VectorIcon::new(DESIGN, layers),
        None => {
            symbol::glyph(kind, color).unwrap_or_else(|| VectorIcon::new(DESIGN, generic(color)))
        }
    }
}

/// [`builtin_icon`] for a picture `side` pixels square, fitted to that side's
/// pixel grid: a symbol's strokes a whole number of pixels wide and every edge
/// on the grid, a polygon's edges on pixel boundaries.
pub(crate) fn builtin_icon_at(kind: IconKind, color: Color, side: u32) -> VectorIcon {
    match polygons(kind, color) {
        Some(layers) => fitted_polygons(&layers, side),
        None => symbol::glyph_at(kind, color, side)
            .unwrap_or_else(|| fitted_polygons(&generic(color), side)),
    }
}

/// `layers`, authored on the [`DESIGN`] grid, fitted to a `side`-pixel
/// square's pixel grid together, so each part still meets the next.
fn fitted_polygons(layers: &[IconLayer], side: u32) -> VectorIcon {
    if side == 0 {
        return VectorIcon::new(DESIGN, layers.to_vec());
    }
    let px = f64::from(side) / f64::from(DESIGN);
    let mut parts: Vec<Vec<SubPath>> = layers
        .iter()
        .map(|layer| {
            layer
                .contours
                .iter()
                .map(|ring| {
                    SubPath::closed(
                        ring.iter()
                            .map(|&(x, y)| (f64::from(x) * px, f64::from(y) * px))
                            .collect(),
                    )
                })
                .collect()
        })
        .collect();
    let fit = GridFit::new(
        side,
        parts.iter().map(|part| (part.as_slice(), Phase::Boundary)),
    );
    let to_design = Affine::scale(
        f64::from(symbol::DESIGN) / f64::from(side),
        f64::from(symbol::DESIGN) / f64::from(side),
    );
    let fitted = layers
        .iter()
        .zip(&mut parts)
        .map(|(layer, part)| {
            fit.apply(part);
            IconLayer::filled(layer.paint.clone(), layer.rule, place(part, to_design))
        })
        .collect();
    VectorIcon::new(symbol::DESIGN, fitted)
}

/// `kind`'s glyph as filled polygons on the [`DESIGN`] grid, tinted with
/// `color`, or `None` for a kind drawn as a [`symbol`] mark.
#[allow(
    clippy::too_many_lines,
    reason = "one arm a kind; splitting it would part the kinds from the one table they are drawn by"
)]
fn polygons(kind: IconKind, color: Color) -> Option<Vec<IconLayer>> {
    Some(match kind {
        IconKind::Network => network(color),
        IconKind::Volume => volume(color),
        IconKind::Battery => battery(color),
        IconKind::Bell => bell(color),
        IconKind::Folder => folder(color),
        IconKind::FolderOpen => folder_open(color),
        IconKind::FolderFilled => folder_filled(color),
        IconKind::FolderBack => folder_back(color),
        IconKind::FolderFront => folder_front(color),
        IconKind::File => file(color),
        // The app-bundle, text, and image families each share one built-in
        // glyph: the fine-grained kinds differ only in their shipped raster
        // artwork, so a system without it still shows the broad family glyph.
        IconKind::AppBundle | IconKind::ServiceBundle => app_bundle(color),
        IconKind::Text
        | IconKind::TextHtml
        | IconKind::TextRust
        | IconKind::TextJava
        | IconKind::ShellScript
        | IconKind::Pdf => text(color),
        IconKind::Image
        | IconKind::ImagePng
        | IconKind::ImageJpeg
        | IconKind::ImageGif
        | IconKind::ImageSvg
        | IconKind::ImageSprite => image(color),
        IconKind::Archive => archive(color),
        IconKind::Audio => audio(color),
        IconKind::Video => video(color),
        IconKind::SkipPrevious => skip(color, false),
        IconKind::SkipNext => skip(color, true),
        IconKind::Shuffle => shuffle(color),
        IconKind::Repeat => repeat(color, false),
        IconKind::RepeatOne => repeat(color, true),
        IconKind::Microphone => microphone(color),
        IconKind::VolumeMuted => volume_muted(color),
        IconKind::NetworkOffline => network_offline(color),
        IconKind::Executable => executable(color),
        IconKind::ViewToggle => view_toggle(color),
        IconKind::Sort => sort(color),
        IconKind::NewFolder => new_folder(color),
        IconKind::Trash => trash(color),
        IconKind::EmptyTrash => empty_trash(color),
        IconKind::Library => library(color),
        IconKind::User => user(color),
        IconKind::Disk
        | IconKind::DiskHard
        | IconKind::DiskSolidState
        | IconKind::DiskUsb
        | IconKind::DiskNvme
        | IconKind::DiskSdCard
        | IconKind::DiskExternal => disk(color),
        IconKind::Generic => generic(color),
        IconKind::Pause => pause(color),
        IconKind::Resume => resume(color),
        IconKind::RotateRight => rotate(color, true),
        IconKind::RotateLeft => rotate(color, false),
        IconKind::Mirror => mirror(color),
        IconKind::ToolSelect => tool_select(color),
        IconKind::ToolPencil => tool_pencil(color),
        IconKind::ToolBrush => tool_brush(color),
        IconKind::ToolSpray => tool_spray(color),
        IconKind::ToolEraser => tool_eraser(color),
        IconKind::ToolFill => tool_fill(color),
        IconKind::ToolEyedropper => tool_eyedropper(color),
        IconKind::ToolLine => tool_line(color),
        IconKind::ToolRectangle => tool_rectangle(color),
        IconKind::ToolEllipse => tool_ellipse(color),
        IconKind::PixelGrid => pixel_grid(color),
        IconKind::ToolGradient => tool_gradient(color),
        IconKind::ToolClone => tool_clone(color),
        IconKind::ToolText => tool_text(color),
        IconKind::ToolPolygon => tool_polygon(color),
        IconKind::ToolCrop => tool_crop(color),
        IconKind::ToolHand => tool_hand(color),
        // A settings category or pane is drawn with its symbol, the same one
        // its badge carries; one that could not be built is a defect in the
        // compiled-in table, and draws the placeholder rather than nothing.
        IconKind::Settings
        | IconKind::Appearance
        | IconKind::Wallpaper
        | IconKind::Display
        | IconKind::LockScreen
        | IconKind::Screensaver
        | IconKind::Power
        | IconKind::Networking
        | IconKind::Bluetooth
        | IconKind::Sound
        | IconKind::Notifications
        | IconKind::Keyboard
        | IconKind::Mouse
        | IconKind::Trackpad
        | IconKind::Touchscreen
        | IconKind::Printer
        | IconKind::Accessibility
        | IconKind::Language
        | IconKind::Sharing
        | IconKind::Users
        | IconKind::Storage
        | IconKind::About
        | IconKind::Startup
        | IconKind::Caching
        | IconKind::DateTime
        | IconKind::Ethernet
        | IconKind::WiFi
        | IconKind::Dns
        | IconKind::TcpIp
        | IconKind::SoundDevices
        | IconKind::SoundEffects
        | IconKind::AlertError
        | IconKind::AlertWarning
        | IconKind::AlertInformation
        | IconKind::AlertQuestion
        | IconKind::Theme
        | IconKind::Computer
        | IconKind::Laptop
        | IconKind::Server
        | IconKind::Router
        | IconKind::NetworkSwitch
        | IconKind::Phone
        | IconKind::Nas
        | IconKind::IotDevice
        | IconKind::DeviceUnknown
        | IconKind::Subnet
        | IconKind::OsLinux
        | IconKind::OsWindows
        | IconKind::OsApple
        | IconKind::OsAndroid
        | IconKind::Refresh
        | IconKind::ZoomActual
        | IconKind::Info
        | IconKind::Open
        | IconKind::OpenWith
        | IconKind::NewWindow
        | IconKind::NewDocument
        | IconKind::Save
        | IconKind::SaveAs
        | IconKind::Close
        | IconKind::Quit
        | IconKind::Properties
        | IconKind::Page
        | IconKind::Edit
        | IconKind::Visible
        | IconKind::Hidden
        | IconKind::Cut
        | IconKind::Copy
        | IconKind::Paste
        | IconKind::SelectAll
        | IconKind::SelectNone
        | IconKind::Rename
        | IconKind::Duplicate
        | IconKind::Delete
        | IconKind::Remove
        | IconKind::Clear
        | IconKind::Undo
        | IconKind::Redo
        | IconKind::Indent
        | IconKind::Outdent
        | IconKind::Comment
        | IconKind::Overwrite
        | IconKind::Search
        | IconKind::Replace
        | IconKind::FindNext
        | IconKind::FindPrevious
        | IconKind::GoTo
        | IconKind::Problem
        | IconKind::ViewText
        | IconKind::ViewHex
        | IconKind::ViewData
        | IconKind::ViewCode
        | IconKind::Syntax
        | IconKind::TabWidth
        | IconKind::LineEndings
        | IconKind::FitWidth
        | IconKind::TextLarger
        | IconKind::TextSmaller
        | IconKind::TextActual
        | IconKind::RotateHalf
        | IconKind::FlipVertical
        | IconKind::Grid
        | IconKind::Panes
        | IconKind::ResetPanes
        | IconKind::Layout
        | IconKind::LargeIcons
        | IconKind::SmallIcons
        | IconKind::ListView
        | IconKind::SortByName
        | IconKind::SortByKind
        | IconKind::SortBySize
        | IconKind::Calendar
        | IconKind::ArrangeLeft
        | IconKind::ArrangeRight
        | IconKind::Shortcut
        | IconKind::Home
        | IconKind::MoveUp
        | IconKind::MoveDown
        | IconKind::Layers
        | IconKind::LayerNew
        | IconKind::LayerAbove
        | IconKind::LayerBelow
        | IconKind::LayerRaise
        | IconKind::LayerLower
        | IconKind::MergeDown
        | IconKind::Flatten
        | IconKind::Resize
        | IconKind::CanvasSize
        | IconKind::Invert
        | IconKind::ColourModel
        | IconKind::MaskAdd
        | IconKind::MaskRemove
        | IconKind::PutDown
        | IconKind::ColourPrimary
        | IconKind::ColourSecondary
        | IconKind::ColourSwap
        | IconKind::ColourReset
        | IconKind::Sliders
        | IconKind::Brightness
        | IconKind::Droplet
        | IconKind::ColourBalance
        | IconKind::Levels
        | IconKind::Curves
        | IconKind::WhiteBalance
        | IconKind::Posterise
        | IconKind::Threshold
        | IconKind::Desaturate
        | IconKind::Blur
        | IconKind::Sharpen
        | IconKind::Pixelate
        | IconKind::Noise
        | IconKind::FindEdges
        | IconKind::Pages
        | IconKind::Toolbox
        | IconKind::Activity
        | IconKind::Terminal
        | IconKind::LogOut
        | IconKind::Restart
        | IconKind::SwitchTo
        | IconKind::Gauge
        | IconKind::Logs
        | IconKind::ForceQuit
        | IconKind::RepeatOff
        | IconKind::Loudness
        | IconKind::Flag
        | IconKind::LevelEasy
        | IconKind::LevelMedium
        | IconKind::LevelHard
        | IconKind::DoorOpen
        | IconKind::NavBack
        | IconKind::NavForward
        | IconKind::NavUp
        | IconKind::ZoomIn
        | IconKind::ZoomOut
        | IconKind::ZoomFit
        | IconKind::ToolZoom => return None,
    })
}

/// Three rising signal bars.
fn network(color: Color) -> alloc::vec::Vec<IconLayer> {
    const SHORT: &[(i32, i32)] = &[(3, 15), (7, 15), (7, 20), (3, 20)];
    const MID: &[(i32, i32)] = &[(10, 10), (14, 10), (14, 20), (10, 20)];
    const TALL: &[(i32, i32)] = &[(17, 5), (21, 5), (21, 20), (17, 20)];
    vec![
        IconLayer::from_points(color, SHORT),
        IconLayer::from_points(color, MID),
        IconLayer::from_points(color, TALL),
    ]
}

/// The signal bars with a cross in the corner above the short bar.
fn network_offline(color: Color) -> alloc::vec::Vec<IconLayer> {
    const FALLING: &[(i32, i32)] = &[(2, 4), (4, 2), (10, 8), (8, 10)];
    const RISING: &[(i32, i32)] = &[(8, 2), (10, 4), (4, 10), (2, 8)];
    let mut layers = network(color);
    layers.push(IconLayer::from_points(color, FALLING));
    layers.push(IconLayer::from_points(color, RISING));
    layers
}

/// A speaker cone (a rectangle joined to a triangular horn).
fn volume(color: Color) -> alloc::vec::Vec<IconLayer> {
    const SPEAKER: &[(i32, i32)] = &[(3, 9), (7, 9), (12, 4), (12, 20), (7, 15), (3, 15)];
    vec![IconLayer::from_points(color, SPEAKER)]
}

/// The speaker cone with a cross beside it.
fn volume_muted(color: Color) -> alloc::vec::Vec<IconLayer> {
    const SPEAKER: &[(i32, i32)] = &[(3, 9), (7, 9), (12, 4), (12, 20), (7, 15), (3, 15)];
    const FALLING: &[(i32, i32)] = &[(14, 10), (16, 8), (22, 14), (20, 16)];
    const RISING: &[(i32, i32)] = &[(20, 8), (22, 10), (16, 16), (14, 14)];
    vec![
        IconLayer::from_points(color, SPEAKER),
        IconLayer::from_points(color, FALLING),
        IconLayer::from_points(color, RISING),
    ]
}

/// A microphone's head in its cradle, on a stem and a base.
fn microphone(color: Color) -> alloc::vec::Vec<IconLayer> {
    const HEAD: &[(i32, i32)] = &[
        (10, 2),
        (14, 2),
        (15, 4),
        (15, 12),
        (14, 14),
        (10, 14),
        (9, 12),
        (9, 4),
    ];
    const CRADLE_LEFT: &[(i32, i32)] = &[
        (6, 9),
        (8, 9),
        (8, 12),
        (9, 14),
        (12, 15),
        (12, 17),
        (8, 16),
        (6, 13),
    ];
    const CRADLE_RIGHT: &[(i32, i32)] = &[
        (18, 9),
        (18, 13),
        (16, 16),
        (12, 17),
        (12, 15),
        (15, 14),
        (16, 12),
        (16, 9),
    ];
    const STEM: &[(i32, i32)] = &[(11, 17), (13, 17), (13, 20), (11, 20)];
    const BASE: &[(i32, i32)] = &[(8, 20), (16, 20), (16, 22), (8, 22)];
    vec![
        IconLayer::from_points(color, HEAD),
        IconLayer::from_points(color, CRADLE_LEFT),
        IconLayer::from_points(color, CRADLE_RIGHT),
        IconLayer::from_points(color, STEM),
        IconLayer::from_points(color, BASE),
    ]
}

/// A battery body with a small terminal nub on the right.
fn battery(color: Color) -> alloc::vec::Vec<IconLayer> {
    const BODY: &[(i32, i32)] = &[(3, 8), (18, 8), (18, 17), (3, 17)];
    const TERMINAL: &[(i32, i32)] = &[(18, 11), (21, 11), (21, 14), (18, 14)];
    vec![
        IconLayer::from_points(color, BODY),
        IconLayer::from_points(color, TERMINAL),
    ]
}

/// A bell with a clapper beneath it.
fn bell(color: Color) -> alloc::vec::Vec<IconLayer> {
    const BODY: &[(i32, i32)] = &[
        (12, 2),
        (16, 5),
        (17, 15),
        (20, 18),
        (4, 18),
        (7, 15),
        (8, 5),
    ];
    const CLAPPER: &[(i32, i32)] = &[(10, 18), (14, 18), (12, 22)];
    vec![
        IconLayer::from_points(color, BODY),
        IconLayer::from_points(color, CLAPPER),
    ]
}

/// A closed folder: a body with a raised tab on its leading edge.
fn folder(color: Color) -> alloc::vec::Vec<IconLayer> {
    const BODY: &[(i32, i32)] = &[(3, 6), (9, 6), (11, 8), (21, 8), (21, 20), (3, 20)];
    vec![IconLayer::from_points(color, BODY)]
}

/// An open folder: a back panel with a splayed front flap, so it reads as
/// distinct from the closed [`folder`] silhouette.
fn folder_open(color: Color) -> alloc::vec::Vec<IconLayer> {
    const BACK: &[(i32, i32)] = &[(3, 6), (9, 6), (11, 8), (21, 8), (21, 12), (3, 12)];
    const FRONT: &[(i32, i32)] = &[(1, 13), (23, 13), (20, 20), (4, 20)];
    vec![
        IconLayer::from_points(color, BACK),
        IconLayer::from_points(color, FRONT),
    ]
}

/// A folder holding papers: a low folder body with two offset sheets standing
/// clear above it, so the stack still reads as "holds something" in one tint.
fn folder_filled(color: Color) -> alloc::vec::Vec<IconLayer> {
    const PAGE_BACK: &[(i32, i32)] = &[(10, 2), (19, 2), (19, 10), (10, 10)];
    const PAGE_FRONT: &[(i32, i32)] = &[(6, 4), (15, 4), (15, 10), (6, 10)];
    const BODY: &[(i32, i32)] = &[(2, 12), (8, 12), (10, 14), (22, 14), (22, 21), (2, 21)];
    vec![
        IconLayer::from_points(color, PAGE_BACK),
        IconLayer::from_points(color, PAGE_FRONT),
        IconLayer::from_points(color, BODY),
    ]
}

/// A folder's back plate and tab, standing taller than its front so what the
/// folder holds shows between them.
fn folder_back(color: Color) -> alloc::vec::Vec<IconLayer> {
    const BACK: &[(i32, i32)] = &[(3, 6), (9, 6), (11, 8), (21, 8), (21, 14), (3, 14)];
    vec![IconLayer::from_points(color, BACK)]
}

/// A folder's low front pocket.
fn folder_front(color: Color) -> alloc::vec::Vec<IconLayer> {
    const FRONT: &[(i32, i32)] = &[(3, 13), (21, 13), (21, 20), (3, 20)];
    vec![IconLayer::from_points(color, FRONT)]
}

/// A generic document: a page with a folded top-trailing corner.
fn file(color: Color) -> alloc::vec::Vec<IconLayer> {
    const PAGE: &[(i32, i32)] = &[(6, 3), (15, 3), (19, 7), (19, 21), (6, 21)];
    const FOLD: &[(i32, i32)] = &[(15, 3), (15, 7), (19, 7)];
    vec![
        IconLayer::from_points(color, PAGE),
        IconLayer::from_points(color, FOLD),
    ]
}

/// An application bundle: a hexagonal tile, unlike any folder or document.
fn app_bundle(color: Color) -> alloc::vec::Vec<IconLayer> {
    const TILE: &[(i32, i32)] = &[(12, 3), (20, 8), (20, 16), (12, 21), (4, 16), (4, 8)];
    vec![IconLayer::from_points(color, TILE)]
}

/// A text document: three horizontal lines suggesting lines of text, spaced
/// so the gaps between them read at small sizes.
fn text(color: Color) -> alloc::vec::Vec<IconLayer> {
    const LINE1: &[(i32, i32)] = &[(5, 6), (19, 6), (19, 8), (5, 8)];
    const LINE2: &[(i32, i32)] = &[(5, 11), (19, 11), (19, 13), (5, 13)];
    const LINE3: &[(i32, i32)] = &[(5, 16), (15, 16), (15, 18), (5, 18)];
    vec![
        IconLayer::from_points(color, LINE1),
        IconLayer::from_points(color, LINE2),
        IconLayer::from_points(color, LINE3),
    ]
}

/// An image: a small sun above a mountain ridge, the classic picture cue.
fn image(color: Color) -> alloc::vec::Vec<IconLayer> {
    const SUN: &[(i32, i32)] = &[(16, 5), (18, 7), (16, 9), (14, 7)];
    const RIDGE: &[(i32, i32)] = &[(4, 20), (10, 11), (14, 16), (17, 12), (20, 20)];
    vec![
        IconLayer::from_points(color, SUN),
        IconLayer::from_points(color, RIDGE),
    ]
}

/// An archive: a lidded package (a knob, a lid, and a body, with seams between
/// them so the parts read even in one tint).
fn archive(color: Color) -> alloc::vec::Vec<IconLayer> {
    const KNOB: &[(i32, i32)] = &[(10, 4), (14, 4), (14, 6), (10, 6)];
    const LID: &[(i32, i32)] = &[(4, 7), (20, 7), (20, 10), (4, 10)];
    const BODY: &[(i32, i32)] = &[(4, 11), (20, 11), (20, 20), (4, 20)];
    vec![
        IconLayer::from_points(color, KNOB),
        IconLayer::from_points(color, LID),
        IconLayer::from_points(color, BODY),
    ]
}

/// Sound: two notes joined by a beam.
fn audio(color: Color) -> alloc::vec::Vec<IconLayer> {
    const BEAM: &[(i32, i32)] = &[(8, 5), (19, 2), (19, 5), (8, 8)];
    const LEFT_STEM: &[(i32, i32)] = &[(8, 6), (10, 6), (10, 17), (8, 17)];
    const RIGHT_STEM: &[(i32, i32)] = &[(17, 3), (19, 3), (19, 15), (17, 15)];
    const LEFT_HEAD: &[(i32, i32)] = &[(4, 17), (6, 15), (9, 15), (10, 17), (8, 19), (5, 19)];
    const RIGHT_HEAD: &[(i32, i32)] = &[(13, 15), (15, 13), (18, 13), (19, 15), (17, 17), (14, 17)];
    vec![
        IconLayer::from_points(color, BEAM),
        IconLayer::from_points(color, LEFT_STEM),
        IconLayer::from_points(color, RIGHT_STEM),
        IconLayer::from_points(color, LEFT_HEAD),
        IconLayer::from_points(color, RIGHT_HEAD),
    ]
}

/// Video: a film camera, its body and the lens flaring out of it.
/// A bar beside a triangle, for the start of the track or the next one: the
/// triangle points away from the bar it is going to.
fn skip(color: Color, forward: bool) -> alloc::vec::Vec<IconLayer> {
    const BACK_BAR: &[(i32, i32)] = &[(5, 5), (8, 5), (8, 19), (5, 19)];
    const BACK: &[(i32, i32)] = &[(19, 5), (9, 12), (19, 19)];
    const ON: &[(i32, i32)] = &[(5, 5), (15, 12), (5, 19)];
    const ON_BAR: &[(i32, i32)] = &[(16, 5), (19, 5), (19, 19), (16, 19)];
    let (bar, triangle) = if forward {
        (ON_BAR, ON)
    } else {
        (BACK_BAR, BACK)
    };
    vec![
        IconLayer::from_points(color, bar),
        IconLayer::from_points(color, triangle),
    ]
}

/// Two crossing arrows, for playing in a shuffled order.
fn shuffle(color: Color) -> alloc::vec::Vec<IconLayer> {
    const RISING: &[(i32, i32)] = &[(3, 17), (5, 19), (16, 8), (14, 6)];
    const RISING_HEAD: &[(i32, i32)] = &[(12, 4), (20, 4), (20, 12)];
    const FALLING: &[(i32, i32)] = &[(3, 7), (5, 5), (16, 16), (14, 18)];
    const FALLING_HEAD: &[(i32, i32)] = &[(12, 20), (20, 20), (20, 12)];
    vec![
        IconLayer::from_points(color, RISING),
        IconLayer::from_points(color, RISING_HEAD),
        IconLayer::from_points(color, FALLING),
        IconLayer::from_points(color, FALLING_HEAD),
    ]
}

/// A loop of two arrows chasing each other, for playing again from the top;
/// with a stroke inside it, for playing the one track again.
fn repeat(color: Color, one: bool) -> alloc::vec::Vec<IconLayer> {
    const TOP: &[(i32, i32)] = &[(4, 13), (4, 6), (15, 6), (15, 9), (7, 9), (7, 13)];
    const TOP_HEAD: &[(i32, i32)] = &[(15, 3), (20, 7), (15, 11)];
    const BOTTOM: &[(i32, i32)] = &[(20, 11), (20, 18), (9, 18), (9, 15), (17, 15), (17, 11)];
    const BOTTOM_HEAD: &[(i32, i32)] = &[(9, 13), (4, 17), (9, 21)];
    const ONE: &[(i32, i32)] = &[(11, 10), (13, 10), (13, 14), (11, 14)];
    let mut layers = vec![
        IconLayer::from_points(color, TOP),
        IconLayer::from_points(color, TOP_HEAD),
        IconLayer::from_points(color, BOTTOM),
        IconLayer::from_points(color, BOTTOM_HEAD),
    ];
    if one {
        layers.push(IconLayer::from_points(color, ONE));
    }
    layers
}

fn video(color: Color) -> alloc::vec::Vec<IconLayer> {
    const BODY: &[(i32, i32)] = &[(3, 7), (15, 7), (15, 17), (3, 17)];
    const LENS: &[(i32, i32)] = &[(16, 10), (21, 7), (21, 17), (16, 14)];
    vec![
        IconLayer::from_points(color, BODY),
        IconLayer::from_points(color, LENS),
    ]
}

/// An executable: a lightning bolt, the run/launch cue.
fn executable(color: Color) -> alloc::vec::Vec<IconLayer> {
    const BOLT: &[(i32, i32)] = &[(13, 3), (7, 13), (11, 13), (9, 21), (17, 10), (12, 10)];
    vec![IconLayer::from_points(color, BOLT)]
}

/// A two-by-two grid of tiles, for the list/grid view toggle.
fn view_toggle(color: Color) -> alloc::vec::Vec<IconLayer> {
    const TL: &[(i32, i32)] = &[(4, 4), (10, 4), (10, 10), (4, 10)];
    const TR: &[(i32, i32)] = &[(14, 4), (20, 4), (20, 10), (14, 10)];
    const BL: &[(i32, i32)] = &[(4, 14), (10, 14), (10, 20), (4, 20)];
    const BR: &[(i32, i32)] = &[(14, 14), (20, 14), (20, 20), (14, 20)];
    vec![
        IconLayer::from_points(color, TL),
        IconLayer::from_points(color, TR),
        IconLayer::from_points(color, BL),
        IconLayer::from_points(color, BR),
    ]
}

/// Three left-aligned horizontal bars of decreasing length, for Sort.
fn sort(color: Color) -> alloc::vec::Vec<IconLayer> {
    const BAR1: &[(i32, i32)] = &[(4, 6), (20, 6), (20, 9), (4, 9)];
    const BAR2: &[(i32, i32)] = &[(4, 11), (16, 11), (16, 14), (4, 14)];
    const BAR3: &[(i32, i32)] = &[(4, 16), (11, 16), (11, 19), (4, 19)];
    vec![
        IconLayer::from_points(color, BAR1),
        IconLayer::from_points(color, BAR2),
        IconLayer::from_points(color, BAR3),
    ]
}

/// A closed folder with a plus badge in its top-trailing corner (clear of the
/// folder body so the two read as separate marks in one tint), for New Folder.
fn new_folder(color: Color) -> alloc::vec::Vec<IconLayer> {
    const BODY: &[(i32, i32)] = &[(2, 9), (8, 9), (10, 11), (15, 11), (15, 21), (2, 21)];
    const PLUS_V: &[(i32, i32)] = &[(18, 3), (21, 3), (21, 10), (18, 10)];
    const PLUS_H: &[(i32, i32)] = &[(16, 5), (23, 5), (23, 8), (16, 8)];
    vec![
        IconLayer::from_points(color, BODY),
        IconLayer::from_points(color, PLUS_V),
        IconLayer::from_points(color, PLUS_H),
    ]
}

/// A waste bin, for the Trash location: a handled lid bar over a tapering
/// bin body ribbed with two vertical staves, so the bin reads even in one
/// tint.
fn trash(color: Color) -> alloc::vec::Vec<IconLayer> {
    const HANDLE: &[(i32, i32)] = &[(9, 3), (15, 3), (15, 5), (9, 5)];
    const LID: &[(i32, i32)] = &[(4, 5), (20, 5), (20, 8), (4, 8)];
    const BODY: &[(i32, i32)] = &[(6, 8), (18, 8), (16, 21), (8, 21)];
    const RIB_LEFT: &[(i32, i32)] = &[(10, 10), (11, 10), (11, 19), (10, 19)];
    const RIB_RIGHT: &[(i32, i32)] = &[(13, 10), (14, 10), (14, 19), (13, 19)];
    vec![
        IconLayer::from_points(color, HANDLE),
        IconLayer::from_points(color, LID),
        IconLayer::from_points(color, BODY),
        IconLayer::from_points(color, RIB_LEFT),
        IconLayer::from_points(color, RIB_RIGHT),
    ]
}

/// An emptied waste bin, for Empty Trash: the same tapering bin body with its
/// lid tipped off to one side (a slanted bar clear of the mouth), so it reads
/// as "tipped out" and distinct from the closed [`trash`] bin.
fn empty_trash(color: Color) -> alloc::vec::Vec<IconLayer> {
    const LID: &[(i32, i32)] = &[(14, 2), (22, 5), (21, 8), (13, 5)];
    const BODY: &[(i32, i32)] = &[(5, 9), (17, 9), (15, 21), (7, 21)];
    vec![
        IconLayer::from_points(color, LID),
        IconLayer::from_points(color, BODY),
    ]
}

/// A three-by-three grid of small application tiles — the app-drawer cue for
/// the program-library launcher, denser than the two-by-two [`view_toggle`]
/// grid so the two never read alike.
fn library(color: Color) -> alloc::vec::Vec<IconLayer> {
    const SIDE: i32 = 4;
    const STARTS: [i32; 3] = [4, 10, 16];
    let mut layers = alloc::vec::Vec::with_capacity(9);
    for top in STARTS {
        for left in STARTS {
            layers.push(IconLayer::from_points(
                color,
                &[
                    (left, top),
                    (left + SIDE, top),
                    (left + SIDE, top + SIDE),
                    (left, top + SIDE),
                ],
            ));
        }
    }
    layers
}

/// A user account: a head over shoulders, the bust silhouette an account is
/// universally drawn as.
fn user(color: Color) -> alloc::vec::Vec<IconLayer> {
    const HEAD: &[(i32, i32)] = &[
        (12, 4),
        (15, 5),
        (16, 8),
        (15, 11),
        (12, 12),
        (9, 11),
        (8, 8),
        (9, 5),
    ];
    const SHOULDERS: &[(i32, i32)] = &[
        (12, 12),
        (16, 13),
        (19, 16),
        (20, 20),
        (4, 20),
        (5, 16),
        (8, 13),
    ];
    vec![
        IconLayer::from_points(color, HEAD),
        IconLayer::from_points(color, SHOULDERS),
    ]
}

/// A storage drive: a cylinder — a top platter disc over a drum body — the
/// classic disk/storage silhouette, so it reads at 16px and stays distinct
/// from the document and folder shapes. Shared by every fine-grained disk
/// medium (hard, solid-state, floppy, USB), which differ only in their
/// shipped raster artwork, not this fallback.
fn disk(color: Color) -> alloc::vec::Vec<IconLayer> {
    const PLATTER: &[(i32, i32)] = &[
        (4, 7),
        (8, 5),
        (12, 5),
        (16, 5),
        (20, 7),
        (16, 9),
        (12, 9),
        (8, 9),
    ];
    const BODY: &[(i32, i32)] = &[
        (4, 7),
        (8, 9),
        (12, 9),
        (16, 9),
        (20, 7),
        (20, 17),
        (16, 19),
        (12, 19),
        (8, 19),
        (4, 17),
    ];
    vec![
        IconLayer::from_points(color, PLATTER),
        IconLayer::from_points(color, BODY),
    ]
}

/// The fallback placeholder: a filled diamond.
fn generic(color: Color) -> alloc::vec::Vec<IconLayer> {
    const DIAMOND: &[(i32, i32)] = &[(12, 4), (20, 12), (12, 20), (4, 12)];
    vec![IconLayer::from_points(color, DIAMOND)]
}

/// Two upright bars: the universal pause mark.
fn pause(color: Color) -> alloc::vec::Vec<IconLayer> {
    const LEFT: &[(i32, i32)] = &[(8, 4), (11, 4), (11, 20), (8, 20)];
    const RIGHT: &[(i32, i32)] = &[(13, 4), (16, 4), (16, 20), (13, 20)];
    vec![
        IconLayer::from_points(color, LEFT),
        IconLayer::from_points(color, RIGHT),
    ]
}

/// A right-pointing triangle: the universal play/continue mark.
fn resume(color: Color) -> alloc::vec::Vec<IconLayer> {
    const PLAY: &[(i32, i32)] = &[(8, 4), (19, 12), (8, 20)];
    vec![IconLayer::from_points(color, PLAY)]
}

/// A half-turn arrow, for turning what a window displays a quarter turn.
///
/// The arc is shared and only the head moves, so the two directions cannot
/// drift apart. Deliberately a half-turn arc rather than
/// [`IconKind::Refresh`]'s near-complete ring: a toolbar carrying both must
/// not draw them alike.
fn rotate(color: Color, clockwise: bool) -> alloc::vec::Vec<IconLayer> {
    // Outer half-annulus left over the top to right, then back inside.
    const ARC: &[(i32, i32)] = &[
        (3, 12),
        (5, 7),
        (7, 5),
        (12, 3),
        (17, 5),
        (19, 7),
        (21, 12),
        (18, 12),
        (16, 8),
        (12, 6),
        (8, 8),
        (6, 12),
    ];
    const RIGHT_HEAD: &[(i32, i32)] = &[(16, 11), (23, 11), (19, 18)];
    const LEFT_HEAD: &[(i32, i32)] = &[(1, 11), (8, 11), (5, 18)];
    vec![
        IconLayer::from_points(color, ARC),
        IconLayer::from_points(color, if clockwise { RIGHT_HEAD } else { LEFT_HEAD }),
    ]
}

/// Two arrowheads facing away from a central axis, for mirroring what a
/// window displays.
fn mirror(color: Color) -> alloc::vec::Vec<IconLayer> {
    const AXIS: &[(i32, i32)] = &[(11, 2), (13, 2), (13, 22), (11, 22)];
    const LEFT: &[(i32, i32)] = &[(9, 5), (9, 19), (2, 12)];
    const RIGHT: &[(i32, i32)] = &[(15, 5), (15, 19), (22, 12)];
    vec![
        IconLayer::from_points(color, AXIS),
        IconLayer::from_points(color, LEFT),
        IconLayer::from_points(color, RIGHT),
    ]
}

/// A dashed rectangle: four corners and a dash midway along each side, so it
/// reads as a boundary still being drawn rather than as
/// [`IconKind::ZoomFit`]'s brackets.
fn tool_select(color: Color) -> alloc::vec::Vec<IconLayer> {
    const TOP_LEFT: &[(i32, i32)] = &[(3, 4), (8, 4), (8, 6), (5, 6), (5, 9), (3, 9)];
    const TOP_RIGHT: &[(i32, i32)] = &[(16, 4), (21, 4), (21, 9), (19, 9), (19, 6), (16, 6)];
    const BOTTOM_LEFT: &[(i32, i32)] = &[(3, 15), (5, 15), (5, 18), (8, 18), (8, 20), (3, 20)];
    const BOTTOM_RIGHT: &[(i32, i32)] =
        &[(19, 15), (21, 15), (21, 20), (16, 20), (16, 18), (19, 18)];
    const TOP: &[(i32, i32)] = &[(10, 4), (14, 4), (14, 6), (10, 6)];
    const BOTTOM: &[(i32, i32)] = &[(10, 18), (14, 18), (14, 20), (10, 20)];
    const LEFT: &[(i32, i32)] = &[(3, 11), (5, 11), (5, 13), (3, 13)];
    const RIGHT: &[(i32, i32)] = &[(19, 11), (21, 11), (21, 13), (19, 13)];
    [
        TOP_LEFT,
        TOP_RIGHT,
        BOTTOM_LEFT,
        BOTTOM_RIGHT,
        TOP,
        BOTTOM,
        LEFT,
        RIGHT,
    ]
    .into_iter()
    .map(|part| IconLayer::from_points(color, part))
    .collect()
}

/// A pencil laid on the diagonal: its sharpened point, its shaft, and the
/// cap at its far end.
fn tool_pencil(color: Color) -> alloc::vec::Vec<IconLayer> {
    const POINT: &[(i32, i32)] = &[(6, 15), (9, 18), (3, 21)];
    const SHAFT: &[(i32, i32)] = &[(7, 14), (15, 6), (18, 9), (10, 17)];
    const CAP: &[(i32, i32)] = &[(17, 4), (19, 2), (22, 5), (20, 7)];
    vec![
        IconLayer::from_points(color, POINT),
        IconLayer::from_points(color, SHAFT),
        IconLayer::from_points(color, CAP),
    ]
}

/// A paintbrush on the diagonal: a tapered tuft, the ferrule gripping it,
/// and a slim handle.
fn tool_brush(color: Color) -> alloc::vec::Vec<IconLayer> {
    const TUFT: &[(i32, i32)] = &[(6, 12), (11, 17), (8, 20), (3, 21), (4, 16)];
    const FERRULE: &[(i32, i32)] = &[(8, 12), (11, 9), (15, 13), (12, 16)];
    const HANDLE: &[(i32, i32)] = &[(13, 9), (20, 2), (22, 4), (15, 11)];
    vec![
        IconLayer::from_points(color, TUFT),
        IconLayer::from_points(color, FERRULE),
        IconLayer::from_points(color, HANDLE),
    ]
}

/// A spray can, its nozzle, and the scatter of paint it throws.
fn tool_spray(color: Color) -> alloc::vec::Vec<IconLayer> {
    const CAN: &[(i32, i32)] = &[(4, 10), (12, 10), (12, 22), (4, 22)];
    const SHOULDER: &[(i32, i32)] = &[(5, 7), (11, 7), (11, 9), (5, 9)];
    const NOZZLE: &[(i32, i32)] = &[(7, 4), (10, 4), (10, 6), (7, 6)];
    const SPRAY: [&[(i32, i32)]; 5] = [
        &[(14, 4), (16, 4), (16, 6), (14, 6)],
        &[(18, 2), (20, 2), (20, 4), (18, 4)],
        &[(19, 7), (21, 7), (21, 9), (19, 9)],
        &[(15, 9), (17, 9), (17, 11), (15, 11)],
        &[(20, 12), (22, 12), (22, 14), (20, 14)],
    ];
    [CAN, SHOULDER, NOZZLE]
        .into_iter()
        .chain(SPRAY)
        .map(|part| IconLayer::from_points(color, part))
        .collect()
}

/// A block eraser on the diagonal — its rubber tip apart from its sleeve —
/// over the line it has rubbed out.
fn tool_eraser(color: Color) -> alloc::vec::Vec<IconLayer> {
    const TIP: &[(i32, i32)] = &[(4, 15), (8, 11), (13, 16), (9, 20)];
    const SLEEVE: &[(i32, i32)] = &[(9, 10), (12, 7), (17, 12), (14, 15)];
    const LINE: &[(i32, i32)] = &[(12, 20), (21, 20), (21, 22), (12, 22)];
    vec![
        IconLayer::from_points(color, TIP),
        IconLayer::from_points(color, SLEEVE),
        IconLayer::from_points(color, LINE),
    ]
}

/// A paint bucket tipped towards the lower right, and the paint falling
/// from its mouth.
fn tool_fill(color: Color) -> alloc::vec::Vec<IconLayer> {
    const BUCKET: &[(i32, i32)] = &[(4, 9), (10, 3), (18, 11), (12, 17)];
    const HANDLE: &[(i32, i32)] = &[(3, 7), (8, 2), (9, 3), (4, 8)];
    const PAINT: &[(i32, i32)] = &[(16, 16), (19, 13), (21, 18), (20, 21), (18, 22), (16, 20)];
    vec![
        IconLayer::from_points(color, BUCKET),
        IconLayer::from_points(color, HANDLE),
        IconLayer::from_points(color, PAINT),
    ]
}

/// An eyedropper on the diagonal: its bulb, the collar below it, and the
/// glass tube narrowing to the tip that takes the colour.
fn tool_eyedropper(color: Color) -> alloc::vec::Vec<IconLayer> {
    const BULB: &[(i32, i32)] = &[(14, 6), (18, 2), (22, 6), (18, 10)];
    const COLLAR: &[(i32, i32)] = &[(12, 7), (13, 6), (18, 11), (17, 12)];
    const TUBE: &[(i32, i32)] = &[(13, 9), (15, 11), (7, 19), (4, 20), (5, 17)];
    vec![
        IconLayer::from_points(color, BULB),
        IconLayer::from_points(color, COLLAR),
        IconLayer::from_points(color, TUBE),
    ]
}

/// A straight line between two square handles.
fn tool_line(color: Color) -> alloc::vec::Vec<IconLayer> {
    const LINE: &[(i32, i32)] = &[(5, 17), (17, 5), (19, 7), (7, 19)];
    const START: &[(i32, i32)] = &[(3, 18), (6, 18), (6, 21), (3, 21)];
    const END: &[(i32, i32)] = &[(18, 3), (21, 3), (21, 6), (18, 6)];
    vec![
        IconLayer::from_points(color, LINE),
        IconLayer::from_points(color, START),
        IconLayer::from_points(color, END),
    ]
}

/// A rectangle's outline: outer then inner in one even-odd ring.
fn tool_rectangle(color: Color) -> alloc::vec::Vec<IconLayer> {
    const FRAME: &[(i32, i32)] = &[
        (4, 6),
        (20, 6),
        (20, 18),
        (4, 18),
        (4, 6),
        (6, 8),
        (6, 16),
        (18, 16),
        (18, 8),
        (6, 8),
    ];
    vec![IconLayer::from_points(color, FRAME)]
}

/// An ellipse's outline, wider than it is tall so it is not read as a ring:
/// outer then inner in one even-odd ring.
fn tool_ellipse(color: Color) -> alloc::vec::Vec<IconLayer> {
    const OUTLINE: &[(i32, i32)] = &[
        (12, 5),
        (17, 6),
        (20, 9),
        (21, 12),
        (20, 15),
        (17, 18),
        (12, 19),
        (7, 18),
        (4, 15),
        (3, 12),
        (4, 9),
        (7, 6),
        (12, 5),
        (12, 7),
        (8, 8),
        (6, 10),
        (5, 12),
        (6, 14),
        (8, 16),
        (12, 17),
        (16, 16),
        (18, 14),
        (19, 12),
        (18, 10),
        (16, 8),
        (12, 7),
    ];
    vec![IconLayer::from_points(color, OUTLINE)]
}

/// A frame filled with three bars, each narrower than the one before: a
/// blend from one colour to another, told in one colour.
fn tool_gradient(color: Color) -> alloc::vec::Vec<IconLayer> {
    const FRAME: &[(i32, i32)] = &[
        (3, 5),
        (21, 5),
        (21, 19),
        (3, 19),
        (3, 5),
        (5, 7),
        (5, 17),
        (19, 17),
        (19, 7),
        (5, 7),
    ];
    const BARS: [&[(i32, i32)]; 3] = [
        &[(6, 8), (11, 8), (11, 16), (6, 16)],
        &[(12, 8), (15, 8), (15, 16), (12, 16)],
        &[(16, 8), (17, 8), (17, 16), (16, 16)],
    ];
    core::iter::once(FRAME)
        .chain(BARS)
        .map(|part| IconLayer::from_points(color, part))
        .collect()
}

/// A rubber stamp: its knob, its stem, its block, and the print beneath.
fn tool_clone(color: Color) -> alloc::vec::Vec<IconLayer> {
    const KNOB: &[(i32, i32)] = &[
        (10, 2),
        (14, 2),
        (16, 4),
        (16, 7),
        (14, 9),
        (10, 9),
        (8, 7),
        (8, 4),
    ];
    const STEM: &[(i32, i32)] = &[(11, 9), (13, 9), (13, 12), (11, 12)];
    const BLOCK: &[(i32, i32)] = &[(5, 12), (19, 12), (20, 17), (4, 17)];
    const PRINT: &[(i32, i32)] = &[(4, 19), (20, 19), (20, 21), (4, 21)];
    vec![
        IconLayer::from_points(color, KNOB),
        IconLayer::from_points(color, STEM),
        IconLayer::from_points(color, BLOCK),
        IconLayer::from_points(color, PRINT),
    ]
}

/// A serifed capital T.
fn tool_text(color: Color) -> alloc::vec::Vec<IconLayer> {
    const LETTER: &[(i32, i32)] = &[
        (4, 3),
        (20, 3),
        (20, 8),
        (18, 8),
        (18, 6),
        (13, 6),
        (13, 18),
        (16, 18),
        (16, 21),
        (8, 21),
        (8, 18),
        (11, 18),
        (11, 6),
        (6, 6),
        (6, 8),
        (4, 8),
    ];
    vec![IconLayer::from_points(color, LETTER)]
}

/// An irregular pentagon's outline, its corners the points a polygon is
/// drawn through.
fn tool_polygon(color: Color) -> alloc::vec::Vec<IconLayer> {
    const OUTLINE: &[(i32, i32)] = &[
        (11, 2),
        (22, 9),
        (18, 21),
        (5, 19),
        (2, 8),
        (11, 2),
        (11, 5),
        (5, 9),
        (7, 17),
        (16, 18),
        (19, 10),
        (11, 5),
    ];
    vec![IconLayer::from_points(color, OUTLINE)]
}

/// Two crop marks, each an L, crossing at opposite corners of the part kept.
fn tool_crop(color: Color) -> alloc::vec::Vec<IconLayer> {
    const UPPER: &[(i32, i32)] = &[(6, 2), (9, 2), (9, 15), (22, 15), (22, 18), (6, 18)];
    const LOWER: &[(i32, i32)] = &[(2, 6), (18, 6), (18, 22), (15, 22), (15, 9), (2, 9)];
    vec![
        IconLayer::from_points(color, UPPER),
        IconLayer::from_points(color, LOWER),
    ]
}

/// An open hand: its palm, four fingers and the thumb held out.
fn tool_hand(color: Color) -> alloc::vec::Vec<IconLayer> {
    const PALM: &[(i32, i32)] = &[(7, 11), (19, 11), (19, 16), (16, 21), (10, 21), (7, 18)];
    const FINGERS: [&[(i32, i32)]; 4] = [
        &[(7, 6), (9, 6), (9, 12), (7, 12)],
        &[(10, 3), (12, 3), (12, 12), (10, 12)],
        &[(13, 4), (15, 4), (15, 12), (13, 12)],
        &[(16, 6), (18, 6), (18, 12), (16, 12)],
    ];
    const THUMB: &[(i32, i32)] = &[(2, 12), (4, 10), (9, 15), (7, 17)];
    core::iter::once(PALM)
        .chain(FINGERS)
        .chain(core::iter::once(THUMB))
        .map(|part| IconLayer::from_points(color, part))
        .collect()
}

/// A frame ruled into three by three cells: the lines between pixels, drawn
/// as lines so it is not read as [`library`]'s tiles.
fn pixel_grid(color: Color) -> alloc::vec::Vec<IconLayer> {
    const FRAME: &[(i32, i32)] = &[
        (2, 2),
        (22, 2),
        (22, 22),
        (2, 22),
        (2, 2),
        (4, 4),
        (4, 20),
        (20, 20),
        (20, 4),
        (4, 4),
    ];
    const COLUMNS: [&[(i32, i32)]; 2] = [
        &[(8, 4), (10, 4), (10, 20), (8, 20)],
        &[(14, 4), (16, 4), (16, 20), (14, 20)],
    ];
    const ROWS: [&[(i32, i32)]; 2] = [
        &[(4, 8), (20, 8), (20, 10), (4, 10)],
        &[(4, 14), (20, 14), (20, 16), (4, 16)],
    ];
    core::iter::once(FRAME)
        .chain(COLUMNS)
        .chain(ROWS)
        .map(|part| IconLayer::from_points(color, part))
        .collect()
}
