//! A complete theme and the two built-in themes.
//!
//! A [`Theme`] bundles one [`Palette`], one set of [`Metrics`], one set of
//! [`Fonts`], and one [`CursorSet`] under a stable [`ThemeId`]. The charter
//! requires a default light theme and a dark theme switchable at runtime,
//! and that "adding a theme is data, not new code": a new
//! theme is just another [`Theme`] value registered with the
//! [`ThemeRegistry`](crate::ThemeRegistry).

use alloc::string::String;

use crate::cursor::CursorSet;
use crate::metrics::Metrics;
use crate::motion::{Contrast, Density, Motion, MotionTheme};
use crate::palette::Palette;
use crate::typography::{DesktopText, FamilyKey, Fonts};
use tairix_abi::desktop::DesktopInfo;

/// A stable identifier for a theme.
///
/// The two built-in themes have reserved ids ([`ThemeId::DARK`] and
/// [`ThemeId::LIGHT`]); custom themes pick any other value.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ThemeId(pub u32);

impl ThemeId {
    /// The id of the built-in dark theme.
    pub const DARK: Self = Self(1);
    /// The id of the built-in light theme (the default).
    pub const LIGHT: Self = Self(2);
}

/// Whether a theme is dark-on-light or light-on-dark.
///
/// This is the axis a "switch to light/dark" control toggles; it is
/// independent of the concrete palette so a custom theme can declare which
/// side it belongs to.
///
/// The enum itself is `tairix_abi::desktop::Appearance`, imported rather
/// than restated: the desktop session reports the active appearance to
/// every application over the window channel, and the byte on that wire
/// must be the same value a theme carries.
pub use tairix_abi::desktop::Appearance;

/// What lies under the surfaces drawn with a theme, and so whether their
/// backgrounds cover it or let it through.
///
/// This is a property of *where a surface is put on screen*, not of what it
/// is: one menu is an opaque plate in a window and frosted glass when the
/// taskbar opens it over the wallpaper. It rides on the theme a surface is
/// drawn with ([`Theme::floating`]) rather than on each control, so everything
/// drawn on one surface agrees without any of them being told twice — and
/// nothing can be forgotten and left as an opaque patch.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum SurfaceGround {
    /// An ordinary surface — a window, a dialog: backgrounds are the palette's
    /// own colours and cover what is behind them.
    #[default]
    Opaque,
    /// Floating desktop chrome — the taskbar and the popups it opens — over a
    /// backdrop the compositor blurs first: backgrounds keep their colour and
    /// take the palette's chrome alphas, so the wallpaper and windows behind
    /// read through as a wash of their colours.
    Floating,
    /// An application window cut from the same glass, frosted deeper: its own
    /// ground takes the palette's chrome alpha over a backdrop blurred by
    /// [`Metrics::window_backdrop_blur`], while everything laid on it — rows
    /// and plates alike — stays solid, so the content the window exists to
    /// show never reads through to the desktop.
    Frosted,
}

/// The desktop's accessibility axes: how a theme is drawn, as distinct from
/// which theme it is.
///
/// One value because they are one decision a user makes and one repaint an
/// application owes: the session publishes all three together over the
/// window channel, and a surface that adopted them separately could draw a
/// frame on half of what the user asked for.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Accessibility {
    /// How much separation is drawn around a control.
    pub contrast: Contrast,
    /// How much room a control is given.
    pub density: Density,
    /// Whether a state change is animated.
    pub motion: Motion,
}

impl Accessibility {
    /// The axes the desktop reports in `info`.
    #[must_use]
    pub const fn of(info: &DesktopInfo) -> Self {
        Self {
            contrast: info.contrast(),
            density: info.density(),
            motion: info.motion(),
        }
    }
}

/// A complete, named theme: colours, metrics, fonts, and cursors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Theme {
    id: ThemeId,
    name: String,
    appearance: Appearance,
    palette: Palette,
    metrics: Metrics,
    fonts: Fonts,
    cursors: CursorSet,
    motion: MotionTheme,
    density: Density,
    contrast: Contrast,
    ground: SurfaceGround,
}

impl Theme {
    /// Assemble a theme from its parts.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: ThemeId,
        name: impl Into<String>,
        appearance: Appearance,
        palette: Palette,
        metrics: Metrics,
        fonts: Fonts,
        cursors: CursorSet,
        motion: MotionTheme,
        density: Density,
        contrast: Contrast,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            appearance,
            palette,
            metrics,
            fonts,
            cursors,
            motion,
            density,
            contrast,
            ground: SurfaceGround::Opaque,
        }
    }

    /// The same theme, for drawing *floating desktop chrome*: the taskbar and
    /// the popups it opens, over a backdrop the compositor blurs.
    ///
    /// Whoever puts a surface on screen is the only party that knows what is
    /// behind it, so it renders with this rather than each control being told
    /// separately — which is also what makes it impossible to leave one
    /// control on the surface opaque.
    #[must_use]
    pub fn floating(self) -> Self {
        self.on(SurfaceGround::Floating)
    }

    /// The same theme, for drawing an application window whose ground is the
    /// desktop's glass ([`SurfaceGround::Frosted`]).
    ///
    /// Only the window knows it asked the compositor to blur what is behind
    /// it, so it renders with this; a popup it draws over its own content is
    /// not on that ground and keeps the opaque theme.
    #[must_use]
    pub fn frosted(self) -> Self {
        self.on(SurfaceGround::Frosted)
    }

    /// The same theme drawn on `ground`.
    pub(crate) fn on(self, ground: SurfaceGround) -> Self {
        Self { ground, ..self }
    }

    /// What lies under the surfaces drawn with this theme.
    #[must_use]
    pub fn ground(&self) -> SurfaceGround {
        self.ground
    }

    /// How far the compositor must blur what is behind a surface drawn with
    /// this theme, in logical pixels: `0` on an opaque ground, which shows
    /// none of it, [`Metrics::chrome_backdrop_blur`] under floating chrome,
    /// and [`Metrics::window_backdrop_blur`] under a frosted window.
    ///
    /// One answer for the fills and the blur, so a surface cannot be drawn
    /// see-through over a sharp backdrop. A radius wider than the window
    /// channel carries saturates, which the compositor then refuses rather
    /// than frosting by some other amount.
    #[must_use]
    pub fn backdrop_blur(&self) -> u16 {
        let radius = match self.ground {
            SurfaceGround::Opaque => return 0,
            SurfaceGround::Floating => self.metrics.chrome_backdrop_blur,
            SurfaceGround::Frosted => self.metrics.window_backdrop_blur,
        };
        u16::try_from(radius).unwrap_or(u16::MAX)
    }

    /// The theme's stable identifier.
    #[must_use]
    pub fn id(&self) -> ThemeId {
        self.id
    }

    /// The theme's human-readable name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the theme is [`Appearance::Dark`] or [`Appearance::Light`].
    #[must_use]
    pub fn appearance(&self) -> Appearance {
        self.appearance
    }

    /// The theme's colour roles.
    #[must_use]
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// The theme's geometric metrics.
    #[must_use]
    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// The theme's fonts.
    #[must_use]
    pub fn fonts(&self) -> &Fonts {
        &self.fonts
    }

    /// The theme's cursors.
    #[must_use]
    pub fn cursors(&self) -> &CursorSet {
        &self.cursors
    }

    /// The theme's motion timings and reduced-motion policy.
    #[must_use]
    pub fn motion(&self) -> MotionTheme {
        self.motion
    }

    /// Whether `other` draws every resting pixel as this does: the two differ,
    /// if at all, only in how their movements are timed.
    ///
    /// What a surface asks before repainting for a theme it is handed, since a
    /// change of motion alone moves nothing drawn.
    #[must_use]
    pub fn draws_like(&self, other: &Self) -> bool {
        // Destructured, so a field added to the theme cannot be left out.
        let Self {
            id,
            name,
            appearance,
            palette,
            metrics,
            fonts,
            cursors,
            motion: _,
            density,
            contrast,
            ground,
        } = self;
        *id == other.id
            && *name == other.name
            && *appearance == other.appearance
            && *palette == other.palette
            && *metrics == other.metrics
            && *fonts == other.fonts
            && *cursors == other.cursors
            && *density == other.density
            && *contrast == other.contrast
            && *ground == other.ground
    }

    /// The theme's information density.
    #[must_use]
    pub fn density(&self) -> Density {
        self.density
    }

    /// The theme's contrast policy.
    #[must_use]
    pub fn contrast(&self) -> Contrast {
        self.contrast
    }

    /// The same theme drawn on `axes`.
    ///
    /// The one place the desktop's accessibility axes are laid over a
    /// theme: the motion policy is carried as it is, and the density and the
    /// contrast are applied by deriving this theme's own metric table at them
    /// ([`Metrics::at_density`], [`Metrics::at_contrast`]) rather than
    /// swapping in a second table. A custom theme therefore gets the same treatment as a
    /// built-in with no work of its own, and nothing anywhere else has to
    /// know how an axis reaches the pixels.
    #[must_use]
    pub fn with_axes(self, axes: Accessibility) -> Self {
        Self {
            metrics: self
                .metrics
                .at_density(axes.density)
                .at_contrast(axes.contrast),
            motion: self.motion.with_reduced_motion(axes.motion.is_reduced()),
            density: axes.density,
            contrast: axes.contrast,
            ..self
        }
    }

    /// The same theme with its text drawn as the user chose, or as it stands
    /// where `text` is `None`: the one place a desktop text choice reaches a
    /// face.
    #[must_use]
    pub fn with_text(self, text: Option<DesktopText>) -> Self {
        match text {
            Some(text) => Self {
                fonts: self.fonts.with_text(text),
                ..self
            },
            None => self,
        }
    }

    /// The built-in **dark** theme: [`Palette::DARK`] over the metrics, fonts,
    /// cursors, and motion both built-ins share.
    #[must_use]
    pub fn dark() -> Self {
        Self::new(
            ThemeId::DARK,
            "TAIRiX Dark",
            Appearance::Dark,
            Palette::DARK,
            common_metrics(),
            common_fonts(),
            CursorSet::canonical(),
            common_motion(),
            Density::Normal,
            Contrast::Normal,
        )
    }

    /// The built-in **light** theme — TAIRiX's default: [`Palette::LIGHT`]
    /// over the same shared parts.
    #[must_use]
    pub fn light() -> Self {
        Self::new(
            ThemeId::LIGHT,
            "TAIRiX Light",
            Appearance::Light,
            Palette::LIGHT,
            common_metrics(),
            common_fonts(),
            CursorSet::canonical(),
            common_motion(),
            Density::Normal,
            Contrast::Normal,
        )
    }
}

/// The metrics shared by both built-in themes. Corner radii and border
/// thickness are an appearance-independent house style, so the dark and
/// light themes share them rather than restate identical numbers.
fn common_metrics() -> Metrics {
    Metrics {
        window_corner_radius: 8,
        taskbar_margin: 5,
        chrome_backdrop_blur: 7,
        window_backdrop_blur: 43,
        popup_corner_radius: 6,
        drop_shadow_reach: 6,
        inactive_drop_shadow_reach: 4,
        icon_shadow_reach: 3,
        border_thickness: 1,
        scrollbar_breadth: 14,
        min_thumb_length: 24,
        control_height: 24,
        control_inset: 9,
        control_gap: 7,
        control_corner_radius: 6,
        selection_backdrop_blur: 6,
        seam_thickness: 2,
        rail_thickness: 3,
        bead_size: 8,
        measured_thickness: 4,
        slider_knob: 16,
        progress_thickness: 6,
        composition_thickness: 16,
        chart_height: 40,
        selector_extent: 16,
        toggle_track_length: 28,
        sidebar_icon_extent: 22,
        picture_width: 144,
        title_bar_height: 28,
        tool_title_bar_height: 20,
        frame_inset: 1,
        resize_grabber_extent: 16,
        resize_edge_grab: 8,
        resize_corner_grab: 16,
        hit_slop: 4,
        title_hue_reach: 500,
    }
}

/// The motion timings shared by both built-in themes, tuned to the middle of
/// the spec §9 targets. Reduced motion is derived from this by a consumer
/// (or a variant theme) via [`MotionTheme::with_reduced_motion`].
fn common_motion() -> MotionTheme {
    MotionTheme::new([
        100,  // hover enter
        95,   // hover exit
        75,   // press compress
        110,  // release settle
        210,  // panel open
        140,  // panel close
        150,  // menu open
        150,  // job progress pulse
        220,  // recovery latch reveal
        115,  // window activate
        200,  // window size transition
        95,   // scrollbar wake
        100,  // selection change
        240,  // stage transition
        420,  // attempt rejected
        1000, // session fade
        600,  // backdrop change
        160,  // pointer enlarge
        360,  // pointer restore
        1500, // progress sweep
        220,  // drag gather
    ])
}

/// The fonts shared by both built-in themes.
///
/// One authored base size drives the whole role ladder, and both appearances
/// use the same type — the boards change colour between light and dark, never
/// the type scale.
fn common_fonts() -> Fonts {
    Fonts::ladder(UI_FAMILY, MONOSPACE_FAMILY, BASE_TEXT_SIZE_PX)
}

/// The logical-pixel body size both built-in themes author their ladder at:
/// ten points of Inter, whose line box is 1.21 of its em.
const BASE_TEXT_SIZE_PX: u16 = 16;

/// The proportional family the shipped themes draw interface text in: the
/// humanist sans the design boards are set in, installed as `/System/Fonts`
/// `inter`. A user's own choice replaces it through
/// [`Theme::with_text`].
///
/// A spelling the key grammar refuses would leave the desktop with no UI
/// family at all, so the fallback is the fixed-pitch family every image
/// ships; the crate's tests assert the shipped spelling resolves.
const UI_FAMILY: FamilyKey = match FamilyKey::new("inter") {
    Ok(key) => key,
    Err(_) => FamilyKey::MONO,
};

/// The fixed-pitch family the shipped themes draw terminal and code text in.
const MONOSPACE_FAMILY: FamilyKey = FamilyKey::MONO;
