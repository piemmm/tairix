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
//! A control that moves changes [`Choice::live`] and nothing else; the frame
//! follows it at once. Where the interaction settles, the choice is written
//! off the frame loop, and what the store then says is adopted — unless the
//! player has moved on since, in which case their newer choice stands and its
//! own write is on the way. A refused write says why and puts the store's
//! choice back.

use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_appconf::{as_u32, ConfError, Lookup};
use tairix_appdata::{AppDataHost, Settings};
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

/// The four knobs' keys, which a non-custom choice leaves unset.
const KNOB_KEYS: [&str; 4] = [LIGHTING_KEY, SHADOWS_KEY, GROUND_KEY, RESOLUTION_KEY];

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

/// A stored choice, and every key whose value was refused on the way.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Stored {
    /// The choice the store implies.
    pub graphics: Graphics,
    /// Each key whose stored value meant nothing here. It is read as unset,
    /// so one broken line costs only itself.
    pub refused: Vec<&'static str>,
}

/// Read the choice `store` holds.
///
/// A missing mode is the default. A custom choice missing a knob takes that
/// knob's finest setting, which is what the choice started from.
#[must_use]
pub fn read(store: &impl Lookup) -> Stored {
    let mut refused = Vec::new();
    let mode = take(
        store,
        MODE_KEY,
        |text| spelled(Mode::ALL, Mode::token, text),
        &mut refused,
    );
    let graphics = match mode {
        Some(Mode::Custom) => {
            let finest = Detail::FINEST;
            Graphics::Custom(Detail {
                lighting: take(
                    store,
                    LIGHTING_KEY,
                    |text| spelled(Lighting::ALL, lighting_token, text),
                    &mut refused,
                )
                .unwrap_or(finest.lighting),
                shadows: take(
                    store,
                    SHADOWS_KEY,
                    |text| spelled(Shadows::ALL, shadows_token, text),
                    &mut refused,
                )
                .unwrap_or(finest.shadows),
                ground: take(store, GROUND_KEY, ground_of, &mut refused).unwrap_or(finest.ground),
                resolution: take(
                    store,
                    RESOLUTION_KEY,
                    |text| spelled(Resolution::ALL, resolution_token, text),
                    &mut refused,
                )
                .unwrap_or(finest.resolution),
            })
        }
        Some(mode) => Graphics::chosen(mode, Detail::FINEST),
        None => Graphics::DEFAULT,
    };
    Stored { graphics, refused }
}

/// The value `store` holds for `key`, read through `parse`, noting the key
/// in `refused` when the text is there and means nothing.
fn take<T>(
    store: &impl Lookup,
    key: &'static str,
    parse: fn(&str) -> Option<T>,
    refused: &mut Vec<&'static str>,
) -> Option<T> {
    let value = parse(store.get(key)?);
    if value.is_none() {
        refused.push(key);
    }
    value
}

/// Stage `graphics` in `settings`: the mode, and the knobs only for a custom
/// choice.
///
/// # Errors
///
/// Whatever the document engine refuses, which for these fixed keys and
/// tokens would mean its bounds changed under them.
pub fn stage(graphics: Graphics, settings: &mut Settings<'_>) -> Result<(), ConfError> {
    settings.set(MODE_KEY, graphics.mode().token())?;
    match graphics {
        Graphics::Custom(detail) => {
            settings.set(LIGHTING_KEY, lighting_token(detail.lighting))?;
            settings.set(SHADOWS_KEY, shadows_token(detail.shadows))?;
            settings.set_u32(GROUND_KEY, detail.ground.octaves())?;
            settings.set(RESOLUTION_KEY, resolution_token(detail.resolution))?;
        }
        Graphics::Auto | Graphics::Ultra | Graphics::Basic => {
            for key in KNOB_KEYS {
                settings.unset(key);
            }
        }
    }
    Ok(())
}

/// Open the application's own store over `host` and read the choice it holds.
///
/// Opening never fails: a store the service cannot serve reads as the
/// default, and the reason comes back beside it for the caller to state.
pub fn load(host: &mut dyn AppDataHost) -> (Stored, Option<Errno>) {
    let settings = Settings::open_without_defaults(host);
    (read(&settings), settings.store_refusal())
}

/// Write `graphics` to the application's own store over `host`, answering
/// what the store holds afterwards.
///
/// # Errors
///
/// The service's refusal of the write, or [`Errno::OutOfRange`] where the
/// document engine would not stage it.
pub fn publish(host: &mut dyn AppDataHost, graphics: Graphics) -> Result<Stored, Errno> {
    let mut settings = Settings::open_without_defaults(host);
    if let Some(refusal) = settings.store_refusal() {
        return Err(refusal);
    }
    stage(graphics, &mut settings).map_err(|_| Errno::OutOfRange)?;
    settings.commit()?;
    Ok(read(&settings))
}

/// The choice frames are drawn with, and the one the store last said.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Choice {
    adopted: Graphics,
    live: Graphics,
}

/// What an answered write did to the choice.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Adopted {
    /// The live choice is unchanged.
    Standing,
    /// The live choice became the one the store holds.
    Moved,
}

impl Choice {
    /// The choice the store held at start-up, in force.
    #[must_use]
    pub const fn new(stored: Graphics) -> Self {
        Self {
            adopted: stored,
            live: stored,
        }
    }

    /// What frames are drawn with.
    #[must_use]
    pub const fn live(&self) -> Graphics {
        self.live
    }

    /// Draw with `graphics` from the next frame on, and write nothing,
    /// answering whether anything changed.
    pub fn preview(&mut self, graphics: Graphics) -> bool {
        core::mem::replace(&mut self.live, graphics) != graphics
    }

    /// The store answered the write of `wrote` with `answer`.
    ///
    /// What it holds is adopted as what the store says; it becomes the live
    /// choice only when the player has not moved on since `wrote` was asked
    /// for, since otherwise a newer write is on its way and a control the
    /// player is using would jump back under them.
    pub fn answered(&mut self, wrote: Graphics, answer: Result<Graphics, Errno>) -> Adopted {
        if let Ok(stored) = answer {
            self.adopted = stored;
        }
        if self.live != wrote || self.live == self.adopted {
            return Adopted::Standing;
        }
        self.live = self.adopted;
        Adopted::Moved
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
