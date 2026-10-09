//! How the player wants frames drawn, and how that choice is kept between
//! sessions.
//!
//! One setting, because "let the machine decide", "every detail at its
//! finest" and "exactly these four knobs" are answers to one question. A new
//! install draws everything at its finest; `auto` is the player's to choose.
//!
//! # What the store holds
//!
//! Only what is in force: the mode always, and the four knobs only while the
//! choice is custom, so the document never keeps a detail nobody is using and
//! two players' files mean the same thing when they say the same thing. The
//! store is the application's own per-app data, reached only through the
//! app-data service; the choice is never sent to a realm and is no input to
//! the simulation.
//!
//! # Live, then durable
//!
//! It is kept by the shared closed-registry engine ([`Registry`], [`Live`]):
//! a control that moves changes the live choice and nothing else, and where
//! the interaction settles the choice is written off the frame loop and what
//! the store then says is adopted wherever the player has not moved on.

use alloc::string::String;
use core::fmt::Write as _;

use tairix_appconf::{as_u32, overwrite, Live, Registry};
use tairix_wintersun_art::material::{Quality as MaterialQuality, MAX_OCTAVES};

use crate::quality::{Detail, Lighting, Resolution, Shadows};

/// The key the mode is stored under.
pub const MODE_KEY: &str = "graphics.mode";

/// The key a custom choice's lighting is stored under.
pub const LIGHTING_KEY: &str = "graphics.lighting";

/// The key a custom choice's shadows are stored under.
pub const SHADOWS_KEY: &str = "graphics.shadows";

/// The key a custom choice's ground texture is stored under, as an octave
/// count.
pub const GROUND_KEY: &str = "graphics.ground";

/// The key a custom choice's render scale is stored under.
pub const RESOLUTION_KEY: &str = "graphics.resolution";

/// Every knob at its plainest, at the window's own resolution: the least a
/// frame can be drawn with without being drawn smaller.
pub const BASIC: Detail = Detail {
    resolution: Resolution::Full,
    ..Detail::PLAINEST
};

/// Which kind of answer the player gave.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Mode {
    /// The machine decides, and changes its mind as the frames tell it to.
    Auto,
    /// Every detail at its finest.
    Ultra,
    /// Every detail at its plainest.
    Basic,
    /// The player's own choice of every knob.
    Custom,
}

impl Mode {
    /// Every mode, in the order a chooser lists them.
    pub const ALL: [Self; 4] = [Self::Auto, Self::Ultra, Self::Basic, Self::Custom];

    /// What a chooser calls it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Ultra => "Ultra",
            Self::Basic => "Basic",
            Self::Custom => "Custom",
        }
    }

    /// One sentence on what choosing it does.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Auto => "Eases detail off and back as frames allow",
            Self::Ultra => "Every detail at its finest",
            Self::Basic => "Every detail at its plainest, at full size",
            Self::Custom => "Your own choice for each detail below",
        }
    }

    /// Its spelling in the store.
    const fn token(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Ultra => "ultra",
            Self::Basic => "basic",
            Self::Custom => "custom",
        }
    }
}

/// How the player wants frames drawn.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Graphics {
    /// The governor decides.
    Auto,
    /// [`Detail::FINEST`].
    Ultra,
    /// [`BASIC`].
    Basic,
    /// Exactly this detail.
    Custom(Detail),
}

impl Graphics {
    /// A new install's choice: every detail at its finest.
    pub const DEFAULT: Self = Self::Ultra;

    /// Which kind of answer this is.
    #[must_use]
    pub const fn mode(self) -> Mode {
        match self {
            Self::Auto => Mode::Auto,
            Self::Ultra => Mode::Ultra,
            Self::Basic => Mode::Basic,
            Self::Custom(_) => Mode::Custom,
        }
    }

    /// The detail frames are drawn at, or `None` for `auto`, where the
    /// governor decides.
    #[must_use]
    pub const fn fixed(self) -> Option<Detail> {
        match self {
            Self::Auto => None,
            Self::Ultra => Some(Detail::FINEST),
            Self::Basic => Some(BASIC),
            Self::Custom(detail) => Some(detail),
        }
    }

    /// `mode` chosen with `shown` on screen: a preset is its own detail, and
    /// custom starts from what the player is looking at.
    #[must_use]
    pub const fn chosen(mode: Mode, shown: Detail) -> Self {
        match mode {
            Mode::Auto => Self::Auto,
            Mode::Ultra => Self::Ultra,
            Mode::Basic => Self::Basic,
            Mode::Custom => Self::Custom(shown),
        }
    }
}

impl Default for Graphics {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// One key the choice is kept under: the mode, then the four knobs, which
/// count only while the choice is custom.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GraphicsKey {
    /// [`MODE_KEY`].
    Mode,
    /// [`LIGHTING_KEY`].
    Lighting,
    /// [`SHADOWS_KEY`].
    Shadows,
    /// [`GROUND_KEY`].
    Ground,
    /// [`RESOLUTION_KEY`].
    Resolution,
}

/// A missing mode is the default, and a custom choice missing a knob takes
/// that knob's finest setting, which is what the choice started from.
impl Registry for Graphics {
    type Key = GraphicsKey;
    const KEYS: &'static [GraphicsKey] = &[
        GraphicsKey::Mode,
        GraphicsKey::Lighting,
        GraphicsKey::Shadows,
        GraphicsKey::Ground,
        GraphicsKey::Resolution,
    ];

    fn name(key: GraphicsKey) -> &'static str {
        match key {
            GraphicsKey::Mode => MODE_KEY,
            GraphicsKey::Lighting => LIGHTING_KEY,
            GraphicsKey::Shadows => SHADOWS_KEY,
            GraphicsKey::Ground => GROUND_KEY,
            GraphicsKey::Resolution => RESOLUTION_KEY,
        }
    }

    fn read(&mut self, key: GraphicsKey, text: &str) -> bool {
        if key == GraphicsKey::Mode {
            let Some(mode) = spelled(Mode::ALL, Mode::token, text) else {
                return false;
            };
            *self = Self::chosen(mode, Detail::FINEST);
            return true;
        }
        let Self::Custom(detail) = self else {
            return true;
        };
        match key {
            GraphicsKey::Lighting => spelled(Lighting::ALL, lighting_token, text)
                .map(|lighting| detail.lighting = lighting),
            GraphicsKey::Shadows => {
                spelled(Shadows::ALL, shadows_token, text).map(|shadows| detail.shadows = shadows)
            }
            GraphicsKey::Ground => ground_of(text).map(|ground| detail.ground = ground),
            GraphicsKey::Resolution => spelled(Resolution::ALL, resolution_token, text)
                .map(|resolution| detail.resolution = resolution),
            GraphicsKey::Mode => None,
        }
        .is_some()
    }

    fn spell(&self, key: GraphicsKey, out: &mut String) -> bool {
        let token = match (key, *self) {
            (GraphicsKey::Mode, graphics) => graphics.mode().token(),
            (_, Self::Auto | Self::Ultra | Self::Basic) => return false,
            (GraphicsKey::Lighting, Self::Custom(detail)) => lighting_token(detail.lighting),
            (GraphicsKey::Shadows, Self::Custom(detail)) => shadows_token(detail.shadows),
            (GraphicsKey::Ground, Self::Custom(detail)) => {
                return write!(out, "{}", detail.ground.octaves()).is_ok();
            }
            (GraphicsKey::Resolution, Self::Custom(detail)) => resolution_token(detail.resolution),
        };
        out.push_str(token);
        true
    }
}

/// A new mode is taken whole; a knob is taken only between two custom
/// choices, the one place it means anything.
impl Live for Graphics {
    fn take(&mut self, other: &Self, key: GraphicsKey) -> bool {
        if key == GraphicsKey::Mode {
            return self.mode() != other.mode() && overwrite(self, *other);
        }
        let (Self::Custom(mine), Self::Custom(theirs)) = (self, other) else {
            return false;
        };
        match key {
            GraphicsKey::Lighting => overwrite(&mut mine.lighting, theirs.lighting),
            GraphicsKey::Shadows => overwrite(&mut mine.shadows, theirs.shadows),
            GraphicsKey::Ground => overwrite(&mut mine.ground, theirs.ground),
            GraphicsKey::Resolution => overwrite(&mut mine.resolution, theirs.resolution),
            GraphicsKey::Mode => false,
        }
    }
}

/// The one of `all` that `token` spells as `text`.
fn spelled<T: Copy, const N: usize>(
    all: [T; N],
    token: fn(T) -> &'static str,
    text: &str,
) -> Option<T> {
    all.into_iter().find(|value| token(*value) == text)
}

const fn lighting_token(lighting: Lighting) -> &'static str {
    match lighting {
        Lighting::Fine => "fine",
        Lighting::Medium => "medium",
        Lighting::Coarse => "coarse",
    }
}

const fn shadows_token(shadows: Shadows) -> &'static str {
    match shadows {
        Shadows::Soft => "soft",
        Shadows::Hard => "hard",
        Shadows::Flat => "flat",
    }
}

/// An octave count in range, refused rather than capped: a stored value past
/// the synthesis's own ceiling is not a detail anything wrote.
fn ground_of(text: &str) -> Option<MaterialQuality> {
    as_u32(text)
        .ok()
        .filter(|&octaves| octaves <= MAX_OCTAVES)
        .map(MaterialQuality::new)
}

const fn resolution_token(resolution: Resolution) -> &'static str {
    match resolution {
        Resolution::Full => "full",
        Resolution::FourFifths => "four-fifths",
        Resolution::TwoThirds => "two-thirds",
        Resolution::Half => "half",
    }
}

#[cfg(test)]
#[path = "graphics_tests.rs"]
mod tests;
