//! The desktop settings document: its validated model, closed key registry,
//! and the two readings the registry has.
//!
//! One document is one [`DesktopSettings`], in two groups. The *backdrop*
//! keys are the user's chosen wallpaper (or none), how it is fitted to the
//! screen, the colour shown where it does not reach, the desktop icon flow,
//! and the sort order the `Desktop` folder is listed in. The *appearance*
//! keys are how every surface of the desktop is drawn: light or dark,
//! contrast, density, motion, the interface scale, and the cursor set and
//! pointer size the compositor draws with. The *notification* keys are
//! which notices reach the desktop at all, the *input* keys how the pointer
//! and keyboard behave, and the *idle* keys when the screensaver starts, what
//! it shows and how each scene draws, when the display is switched off, and
//! when the screen locks.
//! Every field is a
//! closed value set, and the document itself is a plain `lib/appconf`
//! `key = value` document — the one format engine the app-data store speaks,
//! so this crate defines the *registry* over it and no grammar of its own.
//!
//! The two groups share one document because they share one owner and one
//! published scope: the session writes both, in one round trip, and a
//! desktop half-adopted from two documents is a desktop nobody chose.
//!
//! # Where the document lives
//!
//! In the desktop session's **published** app-data scope
//! (`plans/APPDATA.md` §3.11): the session is the only principal that can
//! write it, because an application publishes only its own scope, and any
//! application of that user may read it through one request shape that
//! cannot name a private one. That is what replaces the hand-rolled
//! `~/Settings/Pinboard/pinboard.conf` path a settings surface used to
//! open directly — the concrete instance of the app-from-app defect the
//! store exists to close.
//!
//! # Two readings, deliberately different
//!
//! The [`Registry`] reading is the **tolerant** one, for a document held
//! in a store: a value the registry refuses leaves that one field at its
//! documented default and is *named* to the caller, so one stale setting
//! costs only itself and never blanks a user's desktop.
//!
//! [`merge`] is the **strict** one, for a document that arrived over a
//! channel: a line outside the grammar, a key outside the registry, or a
//! value outside a key's closed set is a defect in the *sender* rather than
//! something a person typed, and adopting a desktop the sender did not
//! describe is worse than refusing it. It lays the document over what the
//! desktop already holds rather than replacing it, because more than one
//! surface edits the desktop and none of them shows every setting.
//!
//! [`DesktopSettings::document`] renders the canonical form both readings
//! accept: every registry key, in registry order, so a render/read round
//! trip is exact. [`DesktopSettings::document_of`] renders one group, which
//! is what a surface asking for a change posts.

use alloc::format;
use alloc::string::{String, ToString};
use core::fmt;

use tairix_abi::desktop::{
    Appearance, Contrast, Density, Motion, ScreensaverKind, DOUBLE_CLICK_DEFAULT, DOUBLE_CLICK_MAX,
    DOUBLE_CLICK_MIN,
};
use tairix_abi::time::Duration64;
use tairix_appconf::{ConfError, Document, Registry};
use tairix_colour::Rgb;
use tairix_geometry::Scale;
use tairix_theme::CursorSetId;

use crate::catalog;
use crate::idle::{DisplayOffAfter, IdleAfter};
use crate::input::{
    parse_decimal, parse_millis, render_millis, PointerSpeed, PrimaryButton, RepeatRate,
    TouchpadSettings, REPEAT_DELAY_DEFAULT, REPEAT_DELAY_MAX, REPEAT_DELAY_MIN,
};
use crate::notify::NotifyPolicy;
use crate::saver::{
    CellSize, CpuUse, Pace, SceneDetail, ScreensaverOptions, SlideOrder, SlideSource,
    SlideshowOptions, StarDensity,
};
use crate::sound::SoundControls;
use crate::text::{TextFamily, TextSize};

/// Maximum length, in bytes, of a wallpaper path named by the `wallpaper`
/// key.
///
/// A fixed validation bound on untrusted input: a wallpaper path names a
/// file under the shipped store or somewhere in the user's own files, and a
/// legitimate one is a handful of path components, so this bounds how much
/// hostile work a single value can demand before the path parser even runs.
/// It is the registry's own bound and must sit inside the format engine's
/// [`tairix_appconf::MAX_VALUE_LEN`], or a path this crate accepts could
/// never be written to the store; the assertion below holds that at compile
/// time rather than leaving it to a test that could be deleted.
pub const MAX_WALLPAPER_PATH_LEN: usize = 1024;

const _: () = assert!(
    MAX_WALLPAPER_PATH_LEN <= tairix_appconf::MAX_VALUE_LEN,
    "a wallpaper path must fit one settings value"
);

/// Why a wallpaper path was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WallpaperPathError {
    /// The path exceeded [`MAX_WALLPAPER_PATH_LEN`].
    TooLong,
    /// The path was empty, relative, held an embedded control character, or
    /// otherwise failed to parse as an absolute session-view path.
    Malformed,
}

impl fmt::Display for WallpaperPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong => f.write_str("wallpaper path is too long"),
            Self::Malformed => f.write_str("wallpaper path is not an absolute path"),
        }
    }
}

/// A validated absolute path naming a wallpaper image.
///
/// The stored spelling is the shared path parser's canonical rendering, not
/// the caller's original wording, so two spellings of one path are one
/// value and a store write-back never carries a redundant `.`/`..` detour.
/// The path is rooted in the `/` session view: it is empty, relative,
/// non-view-rooted (an alias or volume-id spelling), or embedded-control-
/// character input that is refused, never "fixed up". A path surviving
/// validation still names untrusted content — the session reads it under
/// its own identity and the decoder sniffs and bounds it before drawing a
/// pixel; this is only the earlier, cheaper spelling refusal.
///
/// The refusals are the *path grammar's* alone. Nothing here refuses a
/// character on the document's account: `lib/appconf` quotes a value that
/// carries a `#`, a quote, or surrounding space, so every path the path
/// grammar admits round-trips through the store exactly as written. A file
/// the user genuinely named `sunset#2.png` is therefore choosable, where the
/// hand-rolled grammar this replaced had to refuse it to stay unambiguous.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WallpaperPath(String);

impl WallpaperPath {
    /// Validate `path` as a wallpaper path.
    ///
    /// # Errors
    ///
    /// [`WallpaperPathError::TooLong`] or [`WallpaperPathError::Malformed`].
    pub fn new(path: &str) -> Result<Self, WallpaperPathError> {
        if path.len() > MAX_WALLPAPER_PATH_LEN {
            return Err(WallpaperPathError::TooLong);
        }
        let parsed = tairix_path::parse(path).map_err(|_| WallpaperPathError::Malformed)?;
        if !matches!(parsed.root(), tairix_path::Root::View) || parsed.components().is_empty() {
            return Err(WallpaperPathError::Malformed);
        }
        Ok(Self(parsed.to_string()))
    }

    /// Build the path for the shipped default wallpaper.
    ///
    /// [`crate::catalog::default_wallpaper_path`] is a compile-time-fixed,
    /// already-canonical absolute path, so this bypasses [`Self::new`]'s
    /// runtime parse rather than re-validating a value that can never fail;
    /// `the_default_wallpaper_path_is_itself_a_valid_wallpaper_path` pins
    /// that the bypass and [`Self::new`] agree.
    fn shipped_default() -> Self {
        Self(catalog::default_wallpaper_path())
    }

    /// The canonical path text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WallpaperPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The user's wallpaper choice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WallpaperChoice {
    /// No wallpaper: the backdrop colour fills the whole desktop.
    None,
    /// The wallpaper image at this validated absolute path.
    Image(WallpaperPath),
}

impl WallpaperChoice {
    const NONE_VALUE: &'static str = "none";

    fn from_value(value: &str) -> Option<Self> {
        if value == Self::NONE_VALUE {
            return Some(Self::None);
        }
        WallpaperPath::new(value).ok().map(Self::Image)
    }

    fn render_value(&self) -> String {
        match self {
            Self::None => Self::NONE_VALUE.to_string(),
            Self::Image(path) => path.as_str().to_string(),
        }
    }
}

/// How a wallpaper's pixels are mapped onto the screen.
///
/// Shared by the settings document and [`crate::fit::place`], so the
/// desktop renderer and every preview can never disagree about what a fit
/// means.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum WallpaperFit {
    /// Cover the screen, cropping the overflow, centred.
    #[default]
    Fill,
    /// Contain the whole image, letterboxed, centred.
    Fit,
    /// Scale to the exact screen size, ignoring aspect ratio.
    Stretch,
    /// Draw at 1:1, centred, cropped when larger than the screen.
    Centre,
    /// Draw at 1:1, repeated from the origin.
    Tile,
}

impl WallpaperFit {
    /// Every fit, in the canonical listing order a gallery offers them in.
    pub const ALL: [Self; 5] = [
        Self::Fill,
        Self::Fit,
        Self::Stretch,
        Self::Centre,
        Self::Tile,
    ];

    /// The canonical value spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fill => "fill",
            Self::Fit => "fit",
            Self::Stretch => "stretch",
            Self::Centre => "centre",
            Self::Tile => "tile",
        }
    }

    /// Decode a value spelling; `None` for anything outside the closed set
    /// (case-sensitive — one canonical spelling).
    #[must_use]
    pub fn from_value(value: &str) -> Option<Self> {
        match value {
            "fill" => Some(Self::Fill),
            "fit" => Some(Self::Fit),
            "stretch" => Some(Self::Stretch),
            "centre" => Some(Self::Centre),
            "tile" => Some(Self::Tile),
            _ => None,
        }
    }
}

/// The flat colour shown wherever the wallpaper does not reach, and the
/// whole backdrop when [`WallpaperChoice::None`] is set.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Backdrop {
    /// The desktop theme's own backdrop colour.
    Theme,
    /// A user-chosen flat colour, opaque by its type: nothing lies behind
    /// the backdrop to show through it.
    Colour(Rgb),
}

impl Backdrop {
    const THEME_VALUE: &'static str = "theme";

    /// Decode the `theme` keyword or a bare [`Rgb::from_hex`] `rrggbb`
    /// colour; `None` for anything else.
    fn from_value(value: &str) -> Option<Self> {
        if value == Self::THEME_VALUE {
            return Some(Self::Theme);
        }
        Rgb::from_hex(value).map(Self::Colour)
    }

    fn render_value(self) -> String {
        match self {
            Self::Theme => Self::THEME_VALUE.to_string(),
            Self::Colour(rgb) => rgb.hex().to_string(),
        }
    }
}

/// Where the desktop icon grid grows from.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum IconFlow {
    /// From the top-left, filling downward and growing a new column to the
    /// right.
    #[default]
    Leading,
    /// Hugging the trailing edge.
    Trailing,
}

impl IconFlow {
    /// Every flow, in the canonical listing order a gallery offers them in.
    pub const ALL: [Self; 2] = [Self::Leading, Self::Trailing];

    /// The canonical value spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Leading => "leading",
            Self::Trailing => "trailing",
        }
    }

    /// Decode a value spelling; `None` for anything outside the closed set.
    #[must_use]
    pub fn from_value(value: &str) -> Option<Self> {
        match value {
            "leading" => Some(Self::Leading),
            "trailing" => Some(Self::Trailing),
            _ => None,
        }
    }
}

/// How the `Desktop` folder's icons are ordered.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum IconSort {
    /// By file name.
    #[default]
    Name,
    /// By content-type kind.
    Kind,
    /// By file size.
    Size,
    /// By modification date.
    Date,
}

impl IconSort {
    /// Every order, in the canonical listing order a gallery offers them
    /// in.
    pub const ALL: [Self; 4] = [Self::Name, Self::Kind, Self::Size, Self::Date];

    /// The canonical value spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Kind => "kind",
            Self::Size => "size",
            Self::Date => "date",
        }
    }

    /// Decode a value spelling; `None` for anything outside the closed set.
    #[must_use]
    pub fn from_value(value: &str) -> Option<Self> {
        match value {
            "name" => Some(Self::Name),
            "kind" => Some(Self::Kind),
            "size" => Some(Self::Size),
            "date" => Some(Self::Date),
            _ => None,
        }
    }
}

/// How large the pointer is drawn, as a magnification of the size the
/// desktop's own density already calls for.
///
/// A closed ladder rather than a free factor, for the reason the UI-scale
/// row is one: every step is a size a reader would choose deliberately, and
/// a continuous control would post a document per pointer sample.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum CursorSize {
    /// The size the interface scale alone implies.
    #[default]
    Normal,
    /// Half again as large.
    Large,
    /// Twice as large.
    Larger,
    /// Three times as large, for a pointer that must be findable across a
    /// whole screen.
    Largest,
}

impl CursorSize {
    /// Every size, in the canonical listing order a chooser offers them in.
    pub const ALL: [Self; 4] = [Self::Normal, Self::Large, Self::Larger, Self::Largest];

    /// The canonical value spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Large => "large",
            Self::Larger => "larger",
            Self::Largest => "largest",
        }
    }

    /// Decode a value spelling; `None` for anything outside the closed set.
    #[must_use]
    pub fn from_value(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|size| size.as_str() == value)
    }

    /// This size as a percentage of the pointer's reference side.
    ///
    /// Never zero, so multiplying a logical side by it can never collapse
    /// the pointer to nothing.
    #[must_use]
    pub const fn percent(self) -> u32 {
        match self {
            Self::Normal => 100,
            Self::Large => 150,
            Self::Larger => 200,
            Self::Largest => 300,
        }
    }

    /// The logical pointer side this size implies, from the reference side
    /// `base`.
    ///
    /// Logical, so the desktop's one logical-to-physical conversion turns
    /// it into pixels afterwards and no density arithmetic is duplicated
    /// here. Saturating, so no reference side can wrap it.
    #[must_use]
    pub const fn side(self, base: u32) -> u32 {
        base.saturating_mul(self.percent()) / 100
    }
}

/// How long a trail of fading copies of the pointer it leaves where it has
/// just been, so its path can be followed by eye.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum PointerTrail {
    /// No trail: the pointer alone.
    #[default]
    Off,
    /// A short trail, just behind the pointer.
    Short,
    /// A trail about as long as the pointer is tall at a brisk movement.
    Medium,
    /// A long trail, for a pointer that is hard to follow at all.
    Long,
}

impl PointerTrail {
    /// Every length, in the canonical listing order a chooser offers them in.
    pub const ALL: [Self; 4] = [Self::Off, Self::Short, Self::Medium, Self::Long];

    /// The canonical value spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Short => "short",
            Self::Medium => "medium",
            Self::Long => "long",
        }
    }

    /// Decode a value spelling; `None` for anything outside the closed set.
    #[must_use]
    pub fn from_value(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|trail| trail.as_str() == value)
    }
}

/// One key of the closed desktop settings registry.
///
/// Adding a key means adding a variant here, its row in [`SettingsKey::ALL`],
/// its field on [`DesktopSettings`], and its arms in this module's private
/// `set_field` and `field_value` bridges — the compiler then forces every
/// consumer to state what the new key means. There is no free-form key
/// namespace: an unknown key fails closed at parse.
///
/// The keys fall into two groups, which is a reader's distinction rather
/// than the document's: the *pinboard* keys describe the backdrop and the
/// icons standing on it, and the *appearance* keys describe how every
/// surface of the desktop is drawn. They share one document because they
/// share one owner and one published scope — the session writes both, in one
/// round trip, and a desktop half-adopted from two documents is a desktop
/// nobody chose.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SettingsKey {
    /// `wallpaper` — the wallpaper image, or `none`.
    Wallpaper,
    /// `fit` — how the wallpaper is mapped onto the screen.
    Fit,
    /// `backdrop` — the flat colour behind (or instead of) the wallpaper.
    Backdrop,
    /// `icons` — where the desktop icon grid grows from.
    Icons,
    /// `sort` — how the `Desktop` folder's icons are ordered.
    Sort,
    /// `appearance` — whether the desktop is drawn light or dark.
    Appearance,
    /// `contrast` — how much separation is drawn around a control.
    Contrast,
    /// `density` — how much room a control is given.
    Density,
    /// `motion` — whether a state change is animated.
    Motion,
    /// `scale` — the UI scale, as a percentage of the reference density.
    Scale,
    /// `font.family` — the family interface text is drawn in.
    FontFamily,
    /// `font.size` — the body size in points.
    FontSize,
    /// `cursor.set` — which cursor set the pointer is drawn from.
    CursorSet,
    /// `cursor.size` — how large the pointer is drawn.
    CursorSize,
    /// `cursor.shake` — whether shaking the pointer grows it for a moment.
    CursorShake,
    /// `cursor.trail` — how long a trail the pointer leaves behind it.
    CursorTrail,
    /// `cursor.locate` — whether a lone press of Ctrl shows where the pointer
    /// is.
    CursorLocate,
    /// `cursor.shadow` — whether the pointer casts a soft shadow.
    CursorShadow,
    /// `notify.enabled` — whether the desktop shows notifications at all.
    NotifyEnabled,
    /// `notify.sources` — the level of each source that does not show
    /// everything.
    NotifySources,
    /// `pointer.primary` — which physical button is primary.
    PointerPrimary,
    /// `pointer.double_click_ms` — how far apart a double-click's presses
    /// may be.
    DoubleClick,
    /// `pointer.speed` — how far the pointer moves for a movement of the
    /// mouse.
    PointerSpeed,
    /// `touchpad.tap` — whether a tap on a touchpad clicks.
    TouchpadTap,
    /// `touchpad.natural_scroll` — whether two fingers on a touchpad move the
    /// content rather than the view.
    TouchpadNatural,
    /// `touchpad.speed` — how far a touchpad finger moves the pointer.
    TouchpadSpeed,
    /// `key.repeat_delay_ms` — how long a key is held before it repeats.
    RepeatDelay,
    /// `key.repeat_rate` — how often a held key repeats.
    RepeatRate,
    /// `screensaver.after_min` — how long the desktop sits idle before the
    /// screensaver starts.
    ScreensaverAfter,
    /// `screensaver.kind` — what the screensaver shows.
    ScreensaverKind,
    /// `screensaver.display_off_min` — how long after the screensaver starts
    /// the display is switched off.
    DisplayOffAfter,
    /// `screensaver.slideshow.interval_s` — how long the slideshow shows one
    /// picture.
    SlideInterval,
    /// `screensaver.slideshow.order` — whether the slideshow shuffles.
    SlideOrder,
    /// `screensaver.slideshow.category` — the one category the slideshow
    /// draws from, or empty for every category.
    SlideCategory,
    /// `screensaver.clock.date` — whether the clock shows the date.
    ClockDate,
    /// `screensaver.clock.identity` — whether the clock names the account and
    /// the machine.
    ClockIdentity,
    /// `screensaver.ribbon.date` — whether the minimal clock shows the date.
    RibbonDate,
    /// `screensaver.starfield.stars` — how many stars the starfield flies
    /// through.
    StarDensity,
    /// `screensaver.starfield.warp` — whether the starfield surges into warp.
    StarWarp,
    /// `screensaver.life.cells` — how large the Game of Life's cells are.
    LifeCells,
    /// `screensaver.life.speed` — how fast its generations pass.
    LifeSpeed,
    /// `screensaver.raytrace.cpu` — how many of the machine's cores the ray
    /// tracer traces on.
    RaytraceCpu,
    /// `screensaver.raytrace.save` — whether the ray tracer keeps each
    /// finished picture in the user's pictures.
    RaytraceSave,
    /// `screensaver.raytrace.detail` — how much each of the ray tracer's
    /// scenes sets out.
    RaytraceDetail,
    /// `screensaver.retro_games.speed` — how fast the retro games' flight
    /// crosses the grid.
    RetroGamesSpeed,
    /// `screensaver.system_monitor.tasks` — whether the System Monitor names
    /// the busiest tasks.
    MonitorTasks,
    /// `lock.after_min` — how long the desktop sits idle before the screen
    /// locks.
    LockAfter,
    /// `audio.output` — the sink the user prefers as the default.
    AudioOutput,
    /// `audio.input` — the source the user prefers as the default.
    AudioInput,
    /// `audio.levels` — each endpoint's level the user set.
    AudioLevels,
    /// `audio.muted` — the endpoints the user muted.
    AudioMuted,
}

impl SettingsKey {
    /// Every registry key, in the canonical listing (and render) order.
    pub const ALL: [Self; 51] = [
        Self::Wallpaper,
        Self::Fit,
        Self::Backdrop,
        Self::Icons,
        Self::Sort,
        Self::Appearance,
        Self::Contrast,
        Self::Density,
        Self::Motion,
        Self::Scale,
        Self::FontFamily,
        Self::FontSize,
        Self::CursorSet,
        Self::CursorSize,
        Self::CursorShake,
        Self::CursorTrail,
        Self::CursorLocate,
        Self::CursorShadow,
        Self::NotifyEnabled,
        Self::NotifySources,
        Self::PointerPrimary,
        Self::DoubleClick,
        Self::PointerSpeed,
        Self::TouchpadTap,
        Self::TouchpadNatural,
        Self::TouchpadSpeed,
        Self::RepeatDelay,
        Self::RepeatRate,
        Self::ScreensaverAfter,
        Self::ScreensaverKind,
        Self::DisplayOffAfter,
        Self::SlideInterval,
        Self::SlideOrder,
        Self::SlideCategory,
        Self::ClockDate,
        Self::ClockIdentity,
        Self::RibbonDate,
        Self::StarDensity,
        Self::StarWarp,
        Self::LifeCells,
        Self::LifeSpeed,
        Self::RaytraceCpu,
        Self::RaytraceSave,
        Self::RaytraceDetail,
        Self::RetroGamesSpeed,
        Self::MonitorTasks,
        Self::LockAfter,
        Self::AudioOutput,
        Self::AudioInput,
        Self::AudioLevels,
        Self::AudioMuted,
    ];

    /// The keys describing the backdrop and the icons standing on it: what
    /// the backdrop menu and the Settings application's Wallpaper pane
    /// edit.
    pub const PINBOARD: [Self; 5] = [
        Self::Wallpaper,
        Self::Fit,
        Self::Backdrop,
        Self::Icons,
        Self::Sort,
    ];

    /// The keys describing how every surface of the desktop is drawn: what
    /// the Settings application's Appearance and Accessibility panes edit.
    pub const APPEARANCE: [Self; 13] = [
        Self::Appearance,
        Self::Contrast,
        Self::Density,
        Self::Motion,
        Self::Scale,
        Self::FontFamily,
        Self::FontSize,
        Self::CursorSet,
        Self::CursorSize,
        Self::CursorShake,
        Self::CursorTrail,
        Self::CursorLocate,
        Self::CursorShadow,
    ];

    /// The keys deciding which notices reach the desktop: what the Settings
    /// application's Notifications pane edits.
    pub const NOTIFICATIONS: [Self; 2] = [Self::NotifyEnabled, Self::NotifySources];

    /// The keys deciding how the pointer behaves: what the Settings
    /// application's Mouse pane edits.
    pub const POINTER: [Self; 3] = [Self::PointerPrimary, Self::DoubleClick, Self::PointerSpeed];

    /// The keys deciding how a touchpad behaves: what the Settings
    /// application's Touchpad pane edits.
    pub const TOUCHPAD: [Self; 3] = [
        Self::TouchpadTap,
        Self::TouchpadNatural,
        Self::TouchpadSpeed,
    ];

    /// The keys deciding how a held key repeats: what the Settings
    /// application's Keyboard pane edits.
    pub const KEYBOARD: [Self; 2] = [Self::RepeatDelay, Self::RepeatRate];

    /// The keys deciding what the screen does once the desktop is idle — and
    /// every scene's own options: what the Settings application's
    /// Screensaver pane edits, and what a screensaver preview names.
    pub const SCREENSAVER: [Self; 18] = [
        Self::ScreensaverAfter,
        Self::ScreensaverKind,
        Self::DisplayOffAfter,
        Self::SlideInterval,
        Self::SlideOrder,
        Self::SlideCategory,
        Self::ClockDate,
        Self::ClockIdentity,
        Self::RibbonDate,
        Self::StarDensity,
        Self::StarWarp,
        Self::LifeCells,
        Self::LifeSpeed,
        Self::RaytraceCpu,
        Self::RaytraceSave,
        Self::RaytraceDetail,
        Self::RetroGamesSpeed,
        Self::MonitorTasks,
    ];

    /// The key deciding when an idle desktop locks: what the Settings
    /// application's Lock Screen pane edits.
    pub const LOCK: [Self; 1] = [Self::LockAfter];

    /// The keys remembering the user's sound controls. The session alone
    /// writes them, from what the audio service shows its own room, so no
    /// application's document may carry them.
    pub const SOUND: [Self; 4] = [
        Self::AudioOutput,
        Self::AudioInput,
        Self::AudioLevels,
        Self::AudioMuted,
    ];

    /// The canonical key spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Wallpaper => "wallpaper",
            Self::Fit => "fit",
            Self::Backdrop => "backdrop",
            Self::Icons => "icons",
            Self::Sort => "sort",
            Self::Appearance => "appearance",
            Self::Contrast => "contrast",
            Self::Density => "density",
            Self::Motion => "motion",
            Self::Scale => "scale",
            Self::FontFamily => "font.family",
            Self::FontSize => "font.size",
            Self::CursorSet => "cursor.set",
            Self::CursorSize => "cursor.size",
            Self::CursorShake => "cursor.shake",
            Self::CursorTrail => "cursor.trail",
            Self::CursorLocate => "cursor.locate",
            Self::CursorShadow => "cursor.shadow",
            Self::NotifyEnabled => "notify.enabled",
            Self::NotifySources => "notify.sources",
            Self::PointerPrimary => "pointer.primary",
            Self::DoubleClick => "pointer.double_click_ms",
            Self::PointerSpeed => "pointer.speed",
            Self::TouchpadTap => "touchpad.tap",
            Self::TouchpadNatural => "touchpad.natural_scroll",
            Self::TouchpadSpeed => "touchpad.speed",
            Self::RepeatDelay => "key.repeat_delay_ms",
            Self::RepeatRate => "key.repeat_rate",
            Self::ScreensaverAfter => "screensaver.after_min",
            Self::ScreensaverKind => "screensaver.kind",
            Self::DisplayOffAfter => "screensaver.display_off_min",
            Self::SlideInterval => "screensaver.slideshow.interval_s",
            Self::SlideOrder => "screensaver.slideshow.order",
            Self::SlideCategory => "screensaver.slideshow.category",
            Self::ClockDate => "screensaver.clock.date",
            Self::ClockIdentity => "screensaver.clock.identity",
            Self::RibbonDate => "screensaver.ribbon.date",
            Self::StarDensity => "screensaver.starfield.stars",
            Self::StarWarp => "screensaver.starfield.warp",
            Self::LifeCells => "screensaver.life.cells",
            Self::LifeSpeed => "screensaver.life.speed",
            Self::RaytraceCpu => "screensaver.raytrace.cpu",
            Self::RaytraceSave => "screensaver.raytrace.save",
            Self::RaytraceDetail => "screensaver.raytrace.detail",
            Self::RetroGamesSpeed => "screensaver.retro_games.speed",
            Self::MonitorTasks => "screensaver.system_monitor.tasks",
            Self::LockAfter => "lock.after_min",
            Self::AudioOutput => "audio.output",
            Self::AudioInput => "audio.input",
            Self::AudioLevels => "audio.levels",
            Self::AudioMuted => "audio.muted",
        }
    }

    /// Decode a key spelling; `None` for anything outside the registry
    /// (keys are case-sensitive — one canonical spelling).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|key| key.name() == name)
    }

    /// The value of this key on `settings`, in its canonical spelling.
    ///
    /// The one place a setting becomes text, so a writer publishing to the
    /// store and a sender rendering a document cannot spell one differently.
    #[must_use]
    pub fn value_of(self, settings: &DesktopSettings) -> String {
        field_value(settings, self)
    }
}

impl fmt::Display for SettingsKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Why a pinboard settings document that arrived over a channel was refused.
///
/// Only [`merge`] raises these: a document held in the store is read
/// tolerantly, key by key, by [`Registry::load`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentRefusal {
    /// The document is outside the format engine's own bounds or grammar.
    Malformed(ConfError),
    /// A line the `key = value` grammar did not read as a setting, by its
    /// 1-based line number.
    Unparsed(usize),
    /// A key outside the closed [`SettingsKey`] registry.
    UnknownKey(String),
    /// A value outside its key's closed set, or malformed.
    InvalidValue(SettingsKey),
    /// A key of the registry that is not one the reading admits
    /// ([`merge_within`]).
    OutsideGroup(SettingsKey),
}

impl fmt::Display for DocumentRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(err) => write!(f, "not a settings document ({err})"),
            Self::Unparsed(line) => write!(f, "line {line}: not a setting"),
            Self::UnknownKey(key) => write!(f, "unknown pinboard settings key `{key}`"),
            Self::InvalidValue(key) => write!(f, "`{key}` is not a value that setting accepts"),
            Self::OutsideGroup(key) => write!(f, "`{key}` is not a setting this request names"),
        }
    }
}

/// The per-user pinboard settings: the desktop backdrop's wallpaper, fit,
/// colour, and the `Desktop` folder's icon flow and sort order.
///
/// [`DesktopSettings::default`] is the settings an **absent** document
/// implies, exactly the table `plans/PINBOARD.md` §2 specifies: the shipped
/// default wallpaper, `Fill`, the theme's own backdrop colour, a leading
/// icon flow, and a name sort — so a fresh account or an unusable document
/// runs on a calm, fully-specified desktop rather than a guessed one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopSettings {
    /// The wallpaper image, or [`WallpaperChoice::None`].
    pub wallpaper: WallpaperChoice,
    /// How the wallpaper is mapped onto the screen.
    pub fit: WallpaperFit,
    /// The flat colour behind, or instead of, the wallpaper.
    pub backdrop: Backdrop,
    /// Where the desktop icon grid grows from.
    pub icons: IconFlow,
    /// How the `Desktop` folder's icons are ordered.
    pub sort: IconSort,
    /// Whether the desktop is drawn light or dark.
    pub appearance: Appearance,
    /// How much separation is drawn around a control.
    pub contrast: Contrast,
    /// How much room a control is given.
    pub density: Density,
    /// Whether a state change is animated.
    pub motion: Motion,
    /// The UI scale every logical length is resolved through.
    pub scale: Scale,
    /// The family interface text is drawn in.
    pub text_family: TextFamily,
    /// The body size every text role derives from.
    pub text_size: TextSize,
    /// Which cursor set the pointer is drawn from.
    pub cursor_set: CursorSetId,
    /// How large the pointer is drawn.
    pub cursor_size: CursorSize,
    /// Whether shaking the pointer grows it for a moment, so it can be found.
    pub cursor_shake: bool,
    /// How long a trail of fading copies the pointer leaves behind it.
    pub cursor_trail: PointerTrail,
    /// Whether a lone press of Ctrl shows where the pointer is.
    pub cursor_locate: bool,
    /// Whether the pointer casts a soft shadow.
    pub cursor_shadow: bool,
    /// Which notices reach the desktop.
    pub notifications: NotifyPolicy,
    /// Which physical button is primary.
    pub primary_button: PrimaryButton,
    /// How far apart a double-click's presses may be.
    pub double_click: Duration64,
    /// How far the pointer moves for a movement of the mouse.
    pub pointer_speed: PointerSpeed,
    /// How a touchpad behaves.
    pub touchpad: TouchpadSettings,
    /// How long a key is held before it repeats.
    pub repeat_delay: Duration64,
    /// How often a held key repeats.
    pub repeat_rate: RepeatRate,
    /// How long the desktop sits idle before the screensaver starts.
    pub screensaver_after: IdleAfter,
    /// What the screensaver shows.
    pub screensaver: ScreensaverKind,
    /// How long after the screensaver starts the display is switched off.
    pub display_off_after: DisplayOffAfter,
    /// How each screensaver draws, kept whichever one is chosen.
    pub screensaver_options: ScreensaverOptions,
    /// How long the desktop sits idle before the screen locks.
    pub lock_after: IdleAfter,
    /// What the user set on the sound devices.
    pub sound: SoundControls,
}

impl Default for DesktopSettings {
    fn default() -> Self {
        Self {
            wallpaper: WallpaperChoice::Image(WallpaperPath::shipped_default()),
            fit: WallpaperFit::default(),
            backdrop: Backdrop::Theme,
            icons: IconFlow::default(),
            sort: IconSort::default(),
            appearance: Appearance::default(),
            contrast: Contrast::Normal,
            density: Density::Normal,
            motion: Motion::Full,
            scale: Scale::ONE,
            text_family: TextFamily::Theme,
            text_size: TextSize::Theme,
            cursor_set: CursorSetId::builtin(),
            cursor_size: CursorSize::default(),
            cursor_shake: true,
            cursor_trail: PointerTrail::default(),
            cursor_locate: false,
            cursor_shadow: false,
            notifications: NotifyPolicy::default(),
            primary_button: PrimaryButton::default(),
            double_click: DOUBLE_CLICK_DEFAULT,
            pointer_speed: PointerSpeed::default(),
            touchpad: TouchpadSettings::default(),
            repeat_delay: REPEAT_DELAY_DEFAULT,
            repeat_rate: RepeatRate::default(),
            screensaver_after: IdleAfter::Minutes(10),
            screensaver: ScreensaverKind::Ribbon,
            display_off_after: DisplayOffAfter::Minutes(30),
            screensaver_options: ScreensaverOptions::default(),
            lock_after: IdleAfter::Minutes(15),
            sound: SoundControls::default(),
        }
    }
}

/// The **tolerant** reading, for a document held in a store: a key the
/// source does not set keeps its documented default, so an absent document
/// and a fresh account are the same thing, and a value outside a key's closed
/// set leaves that one field at its default and is named. One stale setting
/// therefore costs only itself — a desktop is never blanked because a single
/// value predates this build. A document that arrived over a channel is read
/// strictly instead ([`merge`]).
impl Registry for DesktopSettings {
    type Key = SettingsKey;
    const KEYS: &'static [SettingsKey] = &SettingsKey::ALL;

    fn name(key: SettingsKey) -> &'static str {
        key.name()
    }

    fn read(&mut self, key: SettingsKey, text: &str) -> bool {
        set_field(self, key, text)
    }

    fn spell(&self, key: SettingsKey, out: &mut String) -> bool {
        out.push_str(&field_value(self, key));
        true
    }
}

impl DesktopSettings {
    /// These settings as the canonical document: every registry key, in
    /// registry order.
    ///
    /// Every key is written, including one still at its default, so the
    /// document is self-describing and a render/[`merge`] round trip is
    /// exact. It is what the session persists, because the store holds the
    /// whole desktop; a surface *asking* for a change renders only the keys
    /// it edits ([`Registry::document_of`]), as an apply is merged over what
    /// the desktop holds and must not reset a setting it never showed.
    #[must_use]
    pub fn document(&self) -> Document {
        self.document_of(&SettingsKey::ALL)
    }
}

/// Set `key` on `settings` to the setting `value` names.
///
/// Returns `false` when `value` is outside `key`'s closed set; `settings`
/// is left unchanged on refusal.
#[must_use]
// One arm per registry key; a split would trade exhaustiveness for a wildcard.
#[allow(clippy::too_many_lines)]
fn set_field(settings: &mut DesktopSettings, key: SettingsKey, value: &str) -> bool {
    let saver = &mut settings.screensaver_options;
    match key {
        SettingsKey::Wallpaper => put(&mut settings.wallpaper, WallpaperChoice::from_value(value)),
        SettingsKey::Fit => put(&mut settings.fit, WallpaperFit::from_value(value)),
        SettingsKey::Backdrop => put(&mut settings.backdrop, Backdrop::from_value(value)),
        SettingsKey::Icons => put(&mut settings.icons, IconFlow::from_value(value)),
        SettingsKey::Sort => put(&mut settings.sort, IconSort::from_value(value)),
        SettingsKey::Appearance => put(&mut settings.appearance, Appearance::from_value(value)),
        SettingsKey::Contrast => put(&mut settings.contrast, Contrast::from_value(value)),
        SettingsKey::Density => put(&mut settings.density, Density::from_value(value)),
        SettingsKey::Motion => put(&mut settings.motion, Motion::from_value(value)),
        SettingsKey::Scale => put(&mut settings.scale, parse_scale(value)),
        // Whether the store holds the family is asked where it is resolved:
        // a choice outlives the image that shipped it.
        SettingsKey::FontFamily => put(&mut settings.text_family, TextFamily::from_value(value)),
        SettingsKey::FontSize => put(&mut settings.text_size, TextSize::from_value(value)),
        // Refused here rather than spliced into a store path; whether a set
        // answers to it is the desktop's question, since a choice outlives its image.
        SettingsKey::CursorSet => put(&mut settings.cursor_set, CursorSetId::new(value)),
        SettingsKey::CursorSize => put(&mut settings.cursor_size, CursorSize::from_value(value)),
        SettingsKey::CursorShake => put_bool(&mut settings.cursor_shake, value),
        SettingsKey::CursorTrail => {
            put(&mut settings.cursor_trail, PointerTrail::from_value(value))
        }
        SettingsKey::CursorLocate => put_bool(&mut settings.cursor_locate, value),
        SettingsKey::CursorShadow => put_bool(&mut settings.cursor_shadow, value),
        SettingsKey::NotifyEnabled => match tairix_appconf::as_bool(value) {
            Ok(enabled) => {
                settings.notifications.set_enabled(enabled);
                true
            }
            Err(_) => false,
        },
        SettingsKey::NotifySources => settings.notifications.set_sources(value),
        SettingsKey::PointerPrimary => put(
            &mut settings.primary_button,
            PrimaryButton::from_value(value),
        ),
        SettingsKey::DoubleClick => put(
            &mut settings.double_click,
            parse_millis(value, DOUBLE_CLICK_MIN, DOUBLE_CLICK_MAX),
        ),
        SettingsKey::PointerSpeed => put(&mut settings.pointer_speed, parse_speed(value)),
        SettingsKey::TouchpadTap => put_bool(&mut settings.touchpad.tap, value),
        SettingsKey::TouchpadNatural => put_bool(&mut settings.touchpad.natural_scroll, value),
        SettingsKey::TouchpadSpeed => put(&mut settings.touchpad.speed, parse_speed(value)),
        SettingsKey::RepeatDelay => put(
            &mut settings.repeat_delay,
            parse_millis(value, REPEAT_DELAY_MIN, REPEAT_DELAY_MAX),
        ),
        SettingsKey::RepeatRate => put(&mut settings.repeat_rate, RepeatRate::from_value(value)),
        SettingsKey::ScreensaverAfter => put(
            &mut settings.screensaver_after,
            IdleAfter::from_value(value),
        ),
        SettingsKey::ScreensaverKind => put(
            &mut settings.screensaver,
            ScreensaverKind::from_value(value),
        ),
        SettingsKey::DisplayOffAfter => put(
            &mut settings.display_off_after,
            DisplayOffAfter::from_value(value),
        ),
        SettingsKey::SlideInterval => put(
            &mut saver.slideshow.interval,
            SlideshowOptions::interval_from_value(value),
        ),
        SettingsKey::SlideOrder => put(&mut saver.slideshow.order, SlideOrder::from_value(value)),
        SettingsKey::SlideCategory => {
            put(&mut saver.slideshow.source, SlideSource::from_value(value))
        }
        SettingsKey::ClockDate => put_bool(&mut saver.clock.date, value),
        SettingsKey::ClockIdentity => put_bool(&mut saver.clock.identity, value),
        SettingsKey::RibbonDate => put_bool(&mut saver.ribbon.date, value),
        SettingsKey::StarDensity => put(&mut saver.starfield.stars, StarDensity::from_value(value)),
        SettingsKey::StarWarp => put_bool(&mut saver.starfield.warp, value),
        SettingsKey::LifeCells => put(&mut saver.life.cells, CellSize::from_value(value)),
        SettingsKey::LifeSpeed => put(&mut saver.life.speed, Pace::from_value(value)),
        SettingsKey::RaytraceCpu => put(&mut saver.raytrace.cpu, CpuUse::from_value(value)),
        SettingsKey::RaytraceSave => put_bool(&mut saver.raytrace.save, value),
        SettingsKey::RaytraceDetail => {
            put(&mut saver.raytrace.detail, SceneDetail::from_value(value))
        }
        SettingsKey::RetroGamesSpeed => put(&mut saver.retro_games.speed, Pace::from_value(value)),
        SettingsKey::MonitorTasks => put_bool(&mut saver.system_monitor.tasks, value),
        SettingsKey::LockAfter => put(&mut settings.lock_after, IdleAfter::from_value(value)),
        SettingsKey::AudioOutput => settings.sound.set_output(value),
        SettingsKey::AudioInput => settings.sound.set_input(value),
        SettingsKey::AudioLevels => settings.sound.set_levels(value),
        SettingsKey::AudioMuted => settings.sound.set_muted(value),
    }
}

/// Store the switch `value` spells in `field`, answering whether it spelled
/// one.
/// A speed spelled as a bare decimal percentage, within the speeds offered.
fn parse_speed(value: &str) -> Option<PointerSpeed> {
    parse_decimal(value)
        .and_then(|percent| u16::try_from(percent).ok())
        .and_then(PointerSpeed::from_percent)
}

fn put_bool(field: &mut bool, value: &str) -> bool {
    put(field, tairix_appconf::as_bool(value).ok())
}

/// Store `parsed` in `field`, answering whether there was a value to store;
/// `field` is left unchanged when there was not.
fn put<T>(field: &mut T, parsed: Option<T>) -> bool {
    match parsed {
        Some(value) => {
            *field = value;
            true
        }
        None => false,
    }
}

/// Decode the canonical bare decimal percentage the `scale` key carries.
///
/// The range is [`Scale`]'s own, because a scale this crate accepted and the
/// geometry could not resolve would be a desktop nothing can draw. Only
/// ASCII digits are read: a signed, spaced, or radix-prefixed number is a
/// different spelling of the same value, and two spellings of one setting
/// are two ways for consumers to disagree.
fn parse_scale(value: &str) -> Option<Scale> {
    Scale::from_percent(parse_decimal(value)?)
}

/// The current value of `key` on `settings`, in its canonical spelling.
fn field_value(settings: &DesktopSettings, key: SettingsKey) -> String {
    let saver = &settings.screensaver_options;
    match key {
        SettingsKey::Wallpaper => settings.wallpaper.render_value(),
        SettingsKey::Fit => settings.fit.as_str().to_string(),
        SettingsKey::Backdrop => settings.backdrop.render_value(),
        SettingsKey::Icons => settings.icons.as_str().to_string(),
        SettingsKey::Sort => settings.sort.as_str().to_string(),
        SettingsKey::Appearance => settings.appearance.as_str().to_string(),
        SettingsKey::Contrast => settings.contrast.as_str().to_string(),
        SettingsKey::Density => settings.density.as_str().to_string(),
        SettingsKey::Motion => settings.motion.as_str().to_string(),
        SettingsKey::Scale => format!("{}", settings.scale.percent()),
        SettingsKey::FontFamily => settings.text_family.render_value(),
        SettingsKey::FontSize => settings.text_size.render_value(),
        SettingsKey::CursorSet => settings.cursor_set.name().to_string(),
        SettingsKey::CursorSize => settings.cursor_size.as_str().to_string(),
        SettingsKey::CursorShake => tairix_appconf::bool_text(settings.cursor_shake).to_string(),
        SettingsKey::CursorTrail => settings.cursor_trail.as_str().to_string(),
        SettingsKey::CursorLocate => tairix_appconf::bool_text(settings.cursor_locate).to_string(),
        SettingsKey::CursorShadow => tairix_appconf::bool_text(settings.cursor_shadow).to_string(),
        SettingsKey::NotifyEnabled => {
            tairix_appconf::bool_text(settings.notifications.enabled()).to_string()
        }
        SettingsKey::NotifySources => settings.notifications.render_sources(),
        SettingsKey::PointerPrimary => settings.primary_button.as_str().to_string(),
        SettingsKey::DoubleClick => render_millis(settings.double_click),
        SettingsKey::PointerSpeed => format!("{}", settings.pointer_speed.percent()),
        SettingsKey::TouchpadTap => tairix_appconf::bool_text(settings.touchpad.tap).to_string(),
        SettingsKey::TouchpadNatural => {
            tairix_appconf::bool_text(settings.touchpad.natural_scroll).to_string()
        }
        SettingsKey::TouchpadSpeed => format!("{}", settings.touchpad.speed.percent()),
        SettingsKey::RepeatDelay => render_millis(settings.repeat_delay),
        SettingsKey::RepeatRate => settings.repeat_rate.render_value(),
        SettingsKey::ScreensaverAfter => settings.screensaver_after.render_value(),
        SettingsKey::ScreensaverKind => settings.screensaver.as_str().to_string(),
        SettingsKey::DisplayOffAfter => settings.display_off_after.render_value(),
        SettingsKey::SlideInterval => SlideshowOptions::render_interval(saver.slideshow.interval),
        SettingsKey::SlideOrder => saver.slideshow.order.as_str().to_string(),
        SettingsKey::SlideCategory => saver.slideshow.source.as_str().to_string(),
        SettingsKey::ClockDate => tairix_appconf::bool_text(saver.clock.date).to_string(),
        SettingsKey::ClockIdentity => tairix_appconf::bool_text(saver.clock.identity).to_string(),
        SettingsKey::RibbonDate => tairix_appconf::bool_text(saver.ribbon.date).to_string(),
        SettingsKey::StarDensity => saver.starfield.stars.as_str().to_string(),
        SettingsKey::StarWarp => tairix_appconf::bool_text(saver.starfield.warp).to_string(),
        SettingsKey::LifeCells => saver.life.cells.as_str().to_string(),
        SettingsKey::LifeSpeed => saver.life.speed.as_str().to_string(),
        SettingsKey::RaytraceCpu => saver.raytrace.cpu.as_str().to_string(),
        SettingsKey::RaytraceSave => tairix_appconf::bool_text(saver.raytrace.save).to_string(),
        SettingsKey::RaytraceDetail => saver.raytrace.detail.as_str().to_string(),
        SettingsKey::RetroGamesSpeed => saver.retro_games.speed.as_str().to_string(),
        SettingsKey::MonitorTasks => {
            tairix_appconf::bool_text(saver.system_monitor.tasks).to_string()
        }
        SettingsKey::LockAfter => settings.lock_after.render_value(),
        SettingsKey::AudioOutput => settings.sound.render_output(),
        SettingsKey::AudioInput => settings.sound.render_input(),
        SettingsKey::AudioLevels => settings.sound.render_levels(),
        SettingsKey::AudioMuted => settings.sound.render_muted(),
    }
}

/// Read a desktop settings document that arrived over a channel and lay it
/// over `base`, refusing anything the registry does not fully understand.
///
/// This is the **strict** reading. A document on the wire was rendered by a
/// program from this same registry, so a line outside the grammar, a key
/// outside the registry, or a value outside a key's closed set means the
/// sender is not describing a desktop this build can show — and adopting
/// half of what it asked for would put the user in front of a desktop
/// nobody chose.
///
/// It **merges** rather than replaces, because the desktop has more than one
/// surface asking it to change: the backdrop menu edits the pinboard keys,
/// the Settings application's panes edit one group each, and none of them
/// shows every setting. A sender renders only the keys it edits
/// ([`DesktopSettings::document_of`]) and a key it did not name keeps the
/// value the desktop already has, so one surface can never silently undo the
/// other's change — which taking the absent keys as their *defaults* would
/// do on every single apply.
///
/// # Errors
///
/// The [`DocumentRefusal`] naming what was wrong. The document is refused
/// whole, never half-applied: the merge runs on a copy, so a refusal partway
/// through leaves `base` exactly as it was.
pub fn merge(base: &DesktopSettings, text: &str) -> Result<DesktopSettings, DocumentRefusal> {
    merge_admitting(base, text, |_| true)
}

/// [`merge`], for a document an application asks the desktop to adopt: every
/// key but [`SettingsKey::SOUND`], which the session alone writes from what
/// the audio service shows its own room. A document naming one is refused
/// whole with [`DocumentRefusal::OutsideGroup`].
///
/// # Errors
///
/// As [`merge_within`].
pub fn merge_applied(
    base: &DesktopSettings,
    text: &str,
) -> Result<DesktopSettings, DocumentRefusal> {
    merge_admitting(base, text, |key| !SettingsKey::SOUND.contains(&key))
}

/// [`merge`], admitting only the keys of `group`: a document naming any other
/// registry key is refused whole with [`DocumentRefusal::OutsideGroup`].
///
/// For a request that is about one group of settings alone — a screensaver
/// preview names the screensaver keys and nothing else — so a sender that
/// strayed outside them is refused rather than having the stray key quietly
/// ignored.
///
/// # Errors
///
/// Every refusal [`merge`] raises, and [`DocumentRefusal::OutsideGroup`].
pub fn merge_within(
    base: &DesktopSettings,
    text: &str,
    group: &[SettingsKey],
) -> Result<DesktopSettings, DocumentRefusal> {
    merge_admitting(base, text, |key| group.contains(&key))
}

fn merge_admitting(
    base: &DesktopSettings,
    text: &str,
    admits: impl Fn(SettingsKey) -> bool,
) -> Result<DesktopSettings, DocumentRefusal> {
    let document = Document::parse(text).map_err(DocumentRefusal::Malformed)?;
    if let Some(line) = document.unparsed().next() {
        return Err(DocumentRefusal::Unparsed(line.line));
    }
    let mut settings = base.clone();
    for setting in document.settings() {
        let key = SettingsKey::from_name(setting.key)
            .ok_or_else(|| DocumentRefusal::UnknownKey(setting.key.to_string()))?;
        if !admits(key) {
            return Err(DocumentRefusal::OutsideGroup(key));
        }
        if !set_field(&mut settings, key, setting.value) {
            return Err(DocumentRefusal::InvalidValue(key));
        }
    }
    Ok(settings)
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
