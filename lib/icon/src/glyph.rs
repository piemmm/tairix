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
//! badge carries ([`crate::badge`]).
//!
//! [`disk_icon`] maps the storage medium a mounted volume reports onto the
//! drive kind that represents it, so the file manager and the desktop draw
//! the same icon for the same medium rather than each guessing.

use alloc::vec;

use tairix_abi::blkio::BlkDeviceClass;
use tairix_raster::Color;

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
    /// A run/bolt mark, for an executable.
    Executable,
    /// A left arrow, for the file manager's Back navigation command.
    NavBack,
    /// A right arrow, for the file manager's Forward navigation command.
    NavForward,
    /// An up arrow, for the file manager's Up (climb-to-parent) command.
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
    /// A USB mass-storage disk; shares the disk glyph.
    DiskUsb,
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
    /// A painter's palette, for the desktop-theme category.
    Theme,
    /// A dashed rectangle, for choosing an area of a picture.
    ToolSelect,
    /// A pencil, for setting single pixels.
    ToolPencil,
    /// A paintbrush, for painting soft strokes.
    ToolBrush,
    /// A spray can and its spray, for scattering paint.
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
}

impl IconKind {
    /// Resolve a theme asset identifier to a glyph, falling back to
    /// [`Generic`](Self::Generic) for an unknown id so an unexpected
    /// notification still draws a placeholder.
    #[must_use]
    pub fn for_asset(asset: &str) -> Self {
        match asset {
            "network" => Self::Network,
            "volume" => Self::Volume,
            "battery" => Self::Battery,
            "bell" => Self::Bell,
            "folder" => Self::Folder,
            "folder-open" => Self::FolderOpen,
            "folder-filled" => Self::FolderFilled,
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
        }
    }
}

/// The icon that represents a mounted volume's storage medium.
///
/// An unknown medium and a paravirtual device both resolve to the generic
/// drive icon rather than a guessed one.
#[must_use]
pub const fn disk_icon(medium: Option<BlkDeviceClass>) -> IconKind {
    match medium {
        Some(BlkDeviceClass::Rotational) => IconKind::DiskHard,
        Some(BlkDeviceClass::SolidState) => IconKind::DiskSolidState,
        Some(BlkDeviceClass::Removable) => IconKind::DiskUsb,
        Some(BlkDeviceClass::Virtual) | None => IconKind::Disk,
    }
}

/// Build the built-in glyph for `kind`, tinted with `color`.
///
/// The returned [`VectorIcon`] is authored on a fixed square design grid; the
/// caller rasterises it to whatever pixel size the notification slot needs.
#[must_use]
pub fn builtin_icon(kind: IconKind, color: Color) -> VectorIcon {
    let layers = match kind {
        IconKind::Network => network(color),
        IconKind::Volume => volume(color),
        IconKind::Battery => battery(color),
        IconKind::Bell => bell(color),
        IconKind::Folder => folder(color),
        IconKind::FolderOpen => folder_open(color),
        IconKind::FolderFilled => folder_filled(color),
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
        IconKind::Executable => executable(color),
        IconKind::NavBack => nav_back(color),
        IconKind::NavForward => nav_forward(color),
        IconKind::NavUp => nav_up(color),
        IconKind::Refresh => refresh(color),
        IconKind::ViewToggle => view_toggle(color),
        IconKind::Sort => sort(color),
        IconKind::NewFolder => new_folder(color),
        IconKind::Trash => trash(color),
        IconKind::EmptyTrash => empty_trash(color),
        IconKind::Library => library(color),
        IconKind::User => user(color),
        IconKind::Disk | IconKind::DiskHard | IconKind::DiskSolidState | IconKind::DiskUsb => {
            disk(color)
        }
        IconKind::Generic => generic(color),
        IconKind::Pause => pause(color),
        IconKind::Resume => resume(color),
        IconKind::ZoomIn => magnifier(color, true),
        IconKind::ZoomOut => magnifier(color, false),
        IconKind::ZoomFit => zoom_fit(color),
        IconKind::ZoomActual => zoom_actual(color),
        IconKind::RotateRight => rotate(color, true),
        IconKind::RotateLeft => rotate(color, false),
        IconKind::Mirror => mirror(color),
        IconKind::Info => info(color),
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
        | IconKind::Theme => {
            return symbol::glyph(kind, color)
                .unwrap_or_else(|| VectorIcon::new(DESIGN, generic(color)));
        }
    };
    VectorIcon::new(DESIGN, layers)
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

/// A speaker cone (a rectangle joined to a triangular horn).
fn volume(color: Color) -> alloc::vec::Vec<IconLayer> {
    const SPEAKER: &[(i32, i32)] = &[(3, 9), (7, 9), (12, 4), (12, 20), (7, 15), (3, 15)];
    vec![IconLayer::from_points(color, SPEAKER)]
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

/// An executable: a lightning bolt, the run/launch cue.
fn executable(color: Color) -> alloc::vec::Vec<IconLayer> {
    const BOLT: &[(i32, i32)] = &[(13, 3), (7, 13), (11, 13), (9, 21), (17, 10), (12, 10)];
    vec![IconLayer::from_points(color, BOLT)]
}

/// A left-pointing arrow (a shaft ending in a head), for Back.
fn nav_back(color: Color) -> alloc::vec::Vec<IconLayer> {
    const ARROW: &[(i32, i32)] = &[
        (4, 12),
        (11, 5),
        (11, 9),
        (20, 9),
        (20, 15),
        (11, 15),
        (11, 19),
    ];
    vec![IconLayer::from_points(color, ARROW)]
}

/// A right-pointing arrow, the mirror of [`nav_back`], for Forward.
fn nav_forward(color: Color) -> alloc::vec::Vec<IconLayer> {
    const ARROW: &[(i32, i32)] = &[
        (20, 12),
        (13, 5),
        (13, 9),
        (4, 9),
        (4, 15),
        (13, 15),
        (13, 19),
    ];
    vec![IconLayer::from_points(color, ARROW)]
}

/// An up-pointing arrow, for Up (climb to parent).
fn nav_up(color: Color) -> alloc::vec::Vec<IconLayer> {
    const ARROW: &[(i32, i32)] = &[
        (12, 4),
        (19, 11),
        (15, 11),
        (15, 20),
        (9, 20),
        (9, 11),
        (5, 11),
    ];
    vec![IconLayer::from_points(color, ARROW)]
}

/// A circular arrow, for Refresh: an annular sector (a ring with a gap at the
/// top) plus an arrowhead at the gap so it reads as a rotation.
fn refresh(color: Color) -> alloc::vec::Vec<IconLayer> {
    const RING: &[(i32, i32)] = &[
        (18, 6),
        (21, 12),
        (18, 18),
        (12, 21),
        (6, 18),
        (3, 12),
        (6, 6),
        (8, 8),
        (7, 12),
        (8, 16),
        (12, 17),
        (16, 16),
        (17, 12),
        (16, 8),
    ];
    const HEAD: &[(i32, i32)] = &[(18, 2), (22, 8), (14, 8)];
    vec![
        IconLayer::from_points(color, RING),
        IconLayer::from_points(color, HEAD),
    ]
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

/// A magnifier over a plus or a minus, for magnifying or reducing what a
/// window displays.
///
/// One table for both, because the two glyphs differ only in the bar the
/// lens holds; drawing them as separate coordinate sets would be two lenses
/// to keep identical.
fn magnifier(color: Color, magnify: bool) -> alloc::vec::Vec<IconLayer> {
    // Lens: an octagonal annulus, outer then inner in one even-odd ring.
    const LENS: &[(i32, i32)] = &[
        (10, 3),
        (15, 5),
        (17, 10),
        (15, 15),
        (10, 17),
        (5, 15),
        (3, 10),
        (5, 5),
        (10, 5),
        (13, 7),
        (14, 10),
        (13, 13),
        (10, 14),
        (7, 13),
        (6, 10),
        (7, 7),
    ];
    const HANDLE: &[(i32, i32)] = &[(14, 16), (16, 14), (22, 20), (20, 22)];
    const BAR: &[(i32, i32)] = &[(7, 9), (13, 9), (13, 11), (7, 11)];
    const STEM: &[(i32, i32)] = &[(9, 7), (11, 7), (11, 13), (9, 13)];
    let mut layers = alloc::vec![
        IconLayer::from_points(color, LENS),
        IconLayer::from_points(color, HANDLE),
        IconLayer::from_points(color, BAR),
    ];
    if magnify {
        layers.push(IconLayer::from_points(color, STEM));
    }
    layers
}

/// Four corner brackets facing outward, for scaling a picture to fill the
/// window it is shown in.
fn zoom_fit(color: Color) -> alloc::vec::Vec<IconLayer> {
    const TOP_LEFT: &[(i32, i32)] = &[(3, 3), (11, 3), (11, 6), (6, 6), (6, 11), (3, 11)];
    const TOP_RIGHT: &[(i32, i32)] = &[(21, 3), (21, 11), (18, 11), (18, 6), (13, 6), (13, 3)];
    const BOTTOM_LEFT: &[(i32, i32)] = &[(3, 21), (3, 13), (6, 13), (6, 18), (11, 18), (11, 21)];
    const BOTTOM_RIGHT: &[(i32, i32)] =
        &[(21, 21), (13, 21), (13, 18), (18, 18), (18, 13), (21, 13)];
    vec![
        IconLayer::from_points(color, TOP_LEFT),
        IconLayer::from_points(color, TOP_RIGHT),
        IconLayer::from_points(color, BOTTOM_LEFT),
        IconLayer::from_points(color, BOTTOM_RIGHT),
    ]
}

/// Callipers closed on a fixed box, for showing a picture at its true pixel
/// size: what is measured is the picture, not the window, so the jaws do not
/// move.
fn zoom_actual(color: Color) -> alloc::vec::Vec<IconLayer> {
    const LEFT_JAW: &[(i32, i32)] = &[
        (3, 4),
        (8, 4),
        (8, 7),
        (6, 7),
        (6, 17),
        (8, 17),
        (8, 20),
        (3, 20),
    ];
    const RIGHT_JAW: &[(i32, i32)] = &[
        (21, 4),
        (21, 20),
        (16, 20),
        (16, 17),
        (18, 17),
        (18, 7),
        (16, 7),
        (21, 7),
    ];
    const BOX: &[(i32, i32)] = &[(10, 9), (14, 9), (14, 15), (10, 15)];
    vec![
        IconLayer::from_points(color, LEFT_JAW),
        IconLayer::from_points(color, RIGHT_JAW),
        IconLayer::from_points(color, BOX),
    ]
}

/// A half-turn arrow, for turning what a window displays a quarter turn.
///
/// The arc is shared and only the head moves, so the two directions cannot
/// drift apart. Deliberately a half-turn arc rather than [`refresh`]'s
/// near-complete ring: a toolbar carrying both must not draw them alike.
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

/// An `i` in a ring, for what is known about the thing on display.
fn info(color: Color) -> alloc::vec::Vec<IconLayer> {
    const RING: &[(i32, i32)] = &[
        (12, 2),
        (19, 5),
        (22, 12),
        (19, 19),
        (12, 22),
        (5, 19),
        (2, 12),
        (5, 5),
        (12, 5),
        (17, 7),
        (19, 12),
        (17, 17),
        (12, 19),
        (7, 17),
        (5, 12),
        (7, 7),
    ];
    const DOT: &[(i32, i32)] = &[(10, 7), (14, 7), (14, 10), (10, 10)];
    const STEM: &[(i32, i32)] = &[(10, 12), (14, 12), (14, 18), (10, 18)];
    vec![
        IconLayer::from_points(color, RING),
        IconLayer::from_points(color, DOT),
        IconLayer::from_points(color, STEM),
    ]
}

/// A dashed rectangle: four corners and a dash midway along each side, so it
/// reads as a boundary still being drawn rather than as [`zoom_fit`]'s
/// brackets.
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
