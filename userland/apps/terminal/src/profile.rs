//! The terminal profile: the settings a user keeps between sessions, and how
//! they reach the app-data store.
//!
//! One profile is one [`Profile`]: the colour scheme in force, the sixteen
//! ANSI colours and three screen roles of the user's own custom scheme, the
//! text size, and the strength of every screen effect. It is a closed registry
//! of dotted keys ([`ProfileKey`]) in the OS app-data store, reached through
//! [`tairix_appdata`] — so it is private to this application, gated on the
//! kernel-attested bundle identity, and readable or writable by no other app
//! the user launches.
//!
//! Because `#` begins a comment in the store's format, no value carries one: a
//! colour is spelled as bare `rrggbb` digits ([`Rgb::from_hex`]) rather than
//! `#rrggbb`.
//!
//! It is kept by the shared closed-registry engine ([`Registry`], [`Live`]):
//! a save writes only what the store's layers do not already imply, a restore
//! removes the user's opinions so the layers beneath apply again, and a value
//! the registry refuses — a size outside its bounds, a malformed colour —
//! leaves that one field at its default and is named.

use alloc::string::String;
use core::fmt::{self, Write as _};

use tairix_appconf::{overwrite, Live, Registry};
use tairix_colour::Rgb;

use crate::effects::{Effects, FULL, MIN_OPACITY};
use crate::scheme::{ColorScheme, Scheme, ANSI_COLORS};

/// The smallest text size the profile may name, in logical pixels.
///
/// Below this the shared monospace face loses the strokes that keep a
/// character grid legible.
pub const MIN_FONT_SIZE_PX: u16 = 8;

/// The largest text size the profile may name, in logical pixels.
pub const MAX_FONT_SIZE_PX: u16 = 48;

/// The text size a terminal opens at when the user has not chosen one, in
/// logical pixels.
///
/// Sized so the conventional 80×25 screen, drawn with the shared monospace
/// face and wrapped in the theme's window furniture, fits inside a 640×480
/// display without covering it: the face advances seven physical pixels per
/// column at this height, so the grid is 560×350 and the framed window
/// 562×380. A denser display multiplies this through the desktop scale, and a
/// display too small even for this steps the size down until the grid fits
/// (`fit_font_size`).
pub const DEFAULT_FONT_SIZE_PX: u16 = 14;

/// The step a *Larger text* / *Smaller text* command moves the text size by,
/// in logical pixels.
pub const FONT_SIZE_STEP_PX: u16 = 1;

/// One key of the closed profile registry.
///
/// Adding a key means adding a variant here, its row in [`ProfileKey::ALL`],
/// and its arms in the profile's [`Registry`] and [`Live`] bridges — the
/// compiler then forces every consumer to state what the new key means.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProfileKey {
    /// `scheme` — which colour scheme is in force.
    Scheme,
    /// `font.size` — the text size in logical pixels.
    FontSize,
    /// `effects.opacity` — how opaque the background is, in permille.
    Opacity,
    /// `effects.blur` — how strongly the compositor blurs the backdrop, in
    /// permille.
    Blur,
    /// `effects.scanlines` — how deeply alternate rows are dimmed, in permille.
    ScanLines,
    /// `effects.glow` — how brightly lit pixels glow into their
    /// neighbourhood, in permille.
    Glow,
    /// `effects.fuzz` — how much per-pixel jitter is added, in permille.
    Fuzz,
    /// `effects.phosphor` — how long a lit pixel persists, in permille.
    Phosphor,
    /// `effects.wobble` — how far rows are displaced, in permille.
    Wobble,
    /// `custom.background` — the custom scheme's default background.
    CustomBackground,
    /// `custom.foreground` — the custom scheme's default foreground.
    CustomForeground,
    /// `custom.cursor` — the custom scheme's cursor block.
    CustomCursor,
    /// `custom.cursor-text` — the glyph colour inside the cursor block.
    CustomCursorText,
    /// `custom.ansi` — the custom scheme's sixteen ANSI colours, in order.
    CustomAnsi,
}

impl ProfileKey {
    /// Every registry key, in the canonical listing (and render) order.
    pub const ALL: [Self; 14] = [
        Self::Scheme,
        Self::FontSize,
        Self::Opacity,
        Self::Blur,
        Self::ScanLines,
        Self::Glow,
        Self::Fuzz,
        Self::Phosphor,
        Self::Wobble,
        Self::CustomBackground,
        Self::CustomForeground,
        Self::CustomCursor,
        Self::CustomCursorText,
        Self::CustomAnsi,
    ];

    /// The canonical key spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Scheme => "scheme",
            Self::FontSize => "font.size",
            Self::Opacity => "effects.opacity",
            Self::Blur => "effects.blur",
            Self::ScanLines => "effects.scanlines",
            Self::Glow => "effects.glow",
            Self::Fuzz => "effects.fuzz",
            Self::Phosphor => "effects.phosphor",
            Self::Wobble => "effects.wobble",
            Self::CustomBackground => "custom.background",
            Self::CustomForeground => "custom.foreground",
            Self::CustomCursor => "custom.cursor",
            Self::CustomCursorText => "custom.cursor-text",
            Self::CustomAnsi => "custom.ansi",
        }
    }
}

/// The user's terminal profile.
///
/// [`Profile::default`] is what an absent document implies: the system colour
/// scheme, the default text size, and every screen effect off.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Profile {
    /// The colour scheme in force.
    pub scheme: Scheme,
    /// The text size, in logical pixels, always within
    /// [`MIN_FONT_SIZE_PX`]..=[`MAX_FONT_SIZE_PX`].
    pub font_size_px: u16,
    /// The strength of every screen effect.
    pub effects: Effects,
    /// The user's own scheme, used when [`scheme`](Self::scheme) is
    /// [`Scheme::Custom`] and editable whether or not it is in force.
    pub custom: ColorScheme,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            scheme: Scheme::System,
            font_size_px: DEFAULT_FONT_SIZE_PX,
            effects: Effects::default(),
            custom: default_custom_scheme(),
        }
    }
}

impl Profile {
    /// Clamp every field into its valid range.
    ///
    /// Applied after parsing and after any in-app edit, so a profile can
    /// never carry a size the font will not render or an opacity that would
    /// make text unreadable.
    pub fn clamp(&mut self) {
        self.font_size_px = self.font_size_px.clamp(MIN_FONT_SIZE_PX, MAX_FONT_SIZE_PX);
        self.effects.opacity = self.effects.opacity.clamp(MIN_OPACITY, FULL);
        self.effects.blur = self.effects.blur.min(FULL);
        self.effects.scanlines = self.effects.scanlines.min(FULL);
        self.effects.glow = self.effects.glow.min(FULL);
        self.effects.fuzz = self.effects.fuzz.min(FULL);
        self.effects.phosphor = self.effects.phosphor.min(FULL);
        self.effects.wobble = self.effects.wobble.min(FULL);
    }

    /// The text size one step larger, clamped.
    pub fn enlarge(&mut self) {
        self.font_size_px = self
            .font_size_px
            .saturating_add(FONT_SIZE_STEP_PX)
            .min(MAX_FONT_SIZE_PX);
    }

    /// The text size one step smaller, clamped.
    pub fn reduce(&mut self) {
        self.font_size_px = self
            .font_size_px
            .saturating_sub(FONT_SIZE_STEP_PX)
            .max(MIN_FONT_SIZE_PX);
    }
}

/// The custom scheme a user starts from before editing one of their own: the
/// xterm palette on the classic black ground, so every slot is already a
/// sensible colour rather than an undifferentiated block.
fn default_custom_scheme() -> ColorScheme {
    let mut scheme = Scheme::Contrast.palette().unwrap_or(ColorScheme {
        background: Rgb::new(0, 0, 0),
        foreground: Rgb::new(0xe8, 0xe8, 0xe8),
        cursor: Rgb::new(0xe8, 0xe8, 0xe8),
        cursor_text: Rgb::new(0, 0, 0),
        ansi: [Rgb::new(0, 0, 0); ANSI_COLORS],
    });
    scheme.cursor = Rgb::new(0x7a, 0xc8, 0xff);
    scheme.cursor_text = Rgb::new(0x00, 0x00, 0x00);
    scheme
}

/// Set `key` on `profile` to the setting `value` names.
///
/// Returns `false` when `value` is outside `key`'s closed set; `profile` is
/// left unchanged on refusal.
#[must_use]
fn set_field(profile: &mut Profile, key: ProfileKey, value: &str) -> bool {
    match key {
        ProfileKey::Scheme => match Scheme::from_name(value) {
            Some(scheme) => profile.scheme = scheme,
            None => return false,
        },
        ProfileKey::FontSize => match parse_bounded(value, MIN_FONT_SIZE_PX, MAX_FONT_SIZE_PX) {
            Some(size) => profile.font_size_px = size,
            None => return false,
        },
        ProfileKey::Opacity => match parse_bounded(value, MIN_OPACITY, FULL) {
            Some(permille) => profile.effects.opacity = permille,
            None => return false,
        },
        ProfileKey::Blur => match parse_bounded(value, 0, FULL) {
            Some(permille) => profile.effects.blur = permille,
            None => return false,
        },
        ProfileKey::ScanLines => match parse_bounded(value, 0, FULL) {
            Some(permille) => profile.effects.scanlines = permille,
            None => return false,
        },
        ProfileKey::Glow => match parse_bounded(value, 0, FULL) {
            Some(permille) => profile.effects.glow = permille,
            None => return false,
        },
        ProfileKey::Fuzz => match parse_bounded(value, 0, FULL) {
            Some(permille) => profile.effects.fuzz = permille,
            None => return false,
        },
        ProfileKey::Phosphor => match parse_bounded(value, 0, FULL) {
            Some(permille) => profile.effects.phosphor = permille,
            None => return false,
        },
        ProfileKey::Wobble => match parse_bounded(value, 0, FULL) {
            Some(permille) => profile.effects.wobble = permille,
            None => return false,
        },
        ProfileKey::CustomBackground => match Rgb::from_hex(value) {
            Some(color) => profile.custom.background = color,
            None => return false,
        },
        ProfileKey::CustomForeground => match Rgb::from_hex(value) {
            Some(color) => profile.custom.foreground = color,
            None => return false,
        },
        ProfileKey::CustomCursor => match Rgb::from_hex(value) {
            Some(color) => profile.custom.cursor = color,
            None => return false,
        },
        ProfileKey::CustomCursorText => match Rgb::from_hex(value) {
            Some(color) => profile.custom.cursor_text = color,
            None => return false,
        },
        ProfileKey::CustomAnsi => match parse_ansi(value) {
            Some(ansi) => profile.custom.ansi = ansi,
            None => return false,
        },
    }
    true
}

impl Registry for Profile {
    type Key = ProfileKey;
    const KEYS: &'static [ProfileKey] = &ProfileKey::ALL;

    fn name(key: ProfileKey) -> &'static str {
        key.name()
    }

    fn read(&mut self, key: ProfileKey, text: &str) -> bool {
        set_field(self, key, text)
    }

    fn spell(&self, key: ProfileKey, out: &mut String) -> bool {
        let written = match key {
            ProfileKey::Scheme => out.write_str(self.scheme.name()),
            ProfileKey::FontSize => write!(out, "{}", self.font_size_px),
            ProfileKey::Opacity => write!(out, "{}", self.effects.opacity),
            ProfileKey::Blur => write!(out, "{}", self.effects.blur),
            ProfileKey::ScanLines => write!(out, "{}", self.effects.scanlines),
            ProfileKey::Glow => write!(out, "{}", self.effects.glow),
            ProfileKey::Fuzz => write!(out, "{}", self.effects.fuzz),
            ProfileKey::Phosphor => write!(out, "{}", self.effects.phosphor),
            ProfileKey::Wobble => write!(out, "{}", self.effects.wobble),
            ProfileKey::CustomBackground => write!(out, "{}", self.custom.background.hex()),
            ProfileKey::CustomForeground => write!(out, "{}", self.custom.foreground.hex()),
            ProfileKey::CustomCursor => write!(out, "{}", self.custom.cursor.hex()),
            ProfileKey::CustomCursorText => write!(out, "{}", self.custom.cursor_text.hex()),
            ProfileKey::CustomAnsi => spell_ansi(&self.custom.ansi, out),
        };
        written.is_ok()
    }

    fn normalise(&mut self) {
        self.clamp();
    }
}

impl Live for Profile {
    fn take(&mut self, from: &Self, key: ProfileKey) -> bool {
        match key {
            ProfileKey::Scheme => overwrite(&mut self.scheme, from.scheme),
            ProfileKey::FontSize => overwrite(&mut self.font_size_px, from.font_size_px),
            ProfileKey::Opacity => overwrite(&mut self.effects.opacity, from.effects.opacity),
            ProfileKey::Blur => overwrite(&mut self.effects.blur, from.effects.blur),
            ProfileKey::ScanLines => overwrite(&mut self.effects.scanlines, from.effects.scanlines),
            ProfileKey::Glow => overwrite(&mut self.effects.glow, from.effects.glow),
            ProfileKey::Fuzz => overwrite(&mut self.effects.fuzz, from.effects.fuzz),
            ProfileKey::Phosphor => overwrite(&mut self.effects.phosphor, from.effects.phosphor),
            ProfileKey::Wobble => overwrite(&mut self.effects.wobble, from.effects.wobble),
            ProfileKey::CustomBackground => {
                overwrite(&mut self.custom.background, from.custom.background)
            }
            ProfileKey::CustomForeground => {
                overwrite(&mut self.custom.foreground, from.custom.foreground)
            }
            ProfileKey::CustomCursor => overwrite(&mut self.custom.cursor, from.custom.cursor),
            ProfileKey::CustomCursorText => {
                overwrite(&mut self.custom.cursor_text, from.custom.cursor_text)
            }
            ProfileKey::CustomAnsi => overwrite(&mut self.custom.ansi, from.custom.ansi),
        }
    }
}

/// Decode a decimal `value` bounded to `min..=max`; `None` for a
/// non-decimal, an empty value, or one outside the range.
///
/// A value outside the range is refused rather than clamped: the document
/// said something the registry does not allow, and silently changing it would
/// hide the mistake from the user who wrote it.
fn parse_bounded(value: &str, min: u16, max: u16) -> Option<u16> {
    let parsed: u16 = value.parse().ok()?;
    (min..=max).contains(&parsed).then_some(parsed)
}

/// Decode the sixteen space-separated bare `rrggbb` colours of a custom ANSI
/// palette; `None` unless exactly sixteen well-formed colours are present.
fn parse_ansi(value: &str) -> Option<[Rgb; ANSI_COLORS]> {
    let mut colors = [Rgb::default(); ANSI_COLORS];
    let mut seen = 0;
    for field in value.split_whitespace() {
        let slot = colors.get_mut(seen)?;
        *slot = Rgb::from_hex(field)?;
        seen += 1;
    }
    (seen == ANSI_COLORS).then_some(colors)
}

/// Spell sixteen ANSI colours into `out` as the space-separated bare `rrggbb`
/// list [`parse_ansi`] reads back.
fn spell_ansi(ansi: &[Rgb; ANSI_COLORS], out: &mut String) -> fmt::Result {
    for (index, color) in ansi.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        write!(out, "{}", color.hex())?;
    }
    Ok(())
}

/// What replacing one [`Profile`] with another makes stale — the whole of it,
/// and no more.
///
/// A drag delivers one profile change per pointer-motion sample, so a surface
/// that re-derived everything on each would make one slider cost a screenful
/// per sample. Comparing the two profiles says which kinds of work the change
/// actually implies, and the surface does only those.
///
/// A set rather than a record of flags: the kinds are combined and tested, not
/// assigned, so no caller can build one positionally and get two of them the
/// wrong way round.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Invalidation(u8);

impl Invalidation {
    /// The cell geometry: re-fit the face, re-derive the grid, and tell the
    /// hosted shell its new size.
    const METRICS: u8 = 1 << 0;
    /// The resolved colours: every retained pixel is stale.
    const PAINTED: u8 = 1 << 1;
    /// The effect pipeline: the same cells post-process differently, so the
    /// frame must be built again.
    const PASSES: u8 = 1 << 2;
    /// The backdrop-blur radius: tell the compositor. The surface's own pixels
    /// are unaffected, because the blur is behind them.
    const BLUR: u8 = 1 << 3;

    /// What replacing `was` with `now` makes stale.
    #[must_use]
    pub fn between(was: &Profile, now: &Profile) -> Self {
        let mut stale = 0;
        if was.font_size_px != now.font_size_px {
            stale |= Self::METRICS;
        }
        // The custom colours paint nothing while another scheme is in force.
        let custom_shows = now.scheme == Scheme::Custom;
        if was.scheme != now.scheme
            || (custom_shows && was.custom != now.custom)
            || was.effects.opacity != now.effects.opacity
        {
            stale |= Self::PAINTED;
        }
        if was.effects.scanlines != now.effects.scanlines
            || was.effects.glow != now.effects.glow
            || was.effects.fuzz != now.effects.fuzz
            || was.effects.phosphor != now.effects.phosphor
            || was.effects.wobble != now.effects.wobble
        {
            stale |= Self::PASSES;
        }
        // The radius rather than the strength: it is a quantised handful of
        // pixel widths, so most samples of a blur drag ask for what the
        // compositor already shows and need not be sent at all. It also folds
        // in the opacity an opaque window zeroes the blur by.
        if was.effects.blur_radius_px() != now.effects.blur_radius_px() {
            stale |= Self::BLUR;
        }
        Self(stale)
    }

    /// Whether anything at all is stale.
    #[must_use]
    pub const fn any(self) -> bool {
        self.0 != 0
    }

    /// Whether the cell geometry moved.
    #[must_use]
    pub const fn metrics(self) -> bool {
        self.0 & Self::METRICS != 0
    }

    /// Whether the resolved colours changed.
    #[must_use]
    pub const fn painted(self) -> bool {
        self.0 & Self::PAINTED != 0
    }

    /// Whether the effect pipeline changed.
    ///
    /// Deliberately *not* one of the kinds that forgets what the stateful
    /// passes remember: turning an effect up or down leaves what was lit still
    /// true, and dropping the trail there would flicker the persistence away
    /// on every sample of its own slider.
    #[must_use]
    pub const fn passes(self) -> bool {
        self.0 & Self::PASSES != 0
    }

    /// Whether the backdrop-blur radius changed.
    #[must_use]
    pub const fn blur(self) -> bool {
        self.0 & Self::BLUR != 0
    }

    /// Whether the surface's own pixels must be built again.
    ///
    /// A blur-only change is the one that need not be: the compositor draws it
    /// behind a window whose picture is unchanged.
    #[must_use]
    pub const fn repaints(self) -> bool {
        self.0 & (Self::METRICS | Self::PAINTED | Self::PASSES) != 0
    }
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
