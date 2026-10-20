//! Preferences: how the player likes to play, kept in the game's own store.
//!
//! A closed registry, reached through the app-data service and so private to
//! this application. Every choice is a menu row, made in one click, and is
//! written off the event loop with only the keys it changed.

use alloc::string::String;
use core::fmt::Write as _;

use tairix_appconf::{as_bool, bool_text, overwrite, Live, Registry};

use crate::score::Scoring;
use crate::variant::{Draw, Variant};

/// The colour of the card backs.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Back {
    /// Deep blue.
    #[default]
    Sapphire,
    /// Dark red.
    Crimson,
    /// Forest green.
    Emerald,
    /// Charcoal.
    Graphite,
}

impl Back {
    /// Every back, in menu order.
    pub const ALL: [Self; 4] = [Self::Sapphire, Self::Crimson, Self::Emerald, Self::Graphite];

    /// Its spelling in the store.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Sapphire => "sapphire",
            Self::Crimson => "crimson",
            Self::Emerald => "emerald",
            Self::Graphite => "graphite",
        }
    }

    /// The menu's word for it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sapphire => "Sapphire backs",
            Self::Crimson => "Crimson backs",
            Self::Emerald => "Emerald backs",
            Self::Graphite => "Graphite backs",
        }
    }
}

/// The colour of the table.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Baize {
    /// The card-room green.
    #[default]
    Green,
    /// Slate blue.
    Slate,
    /// Deep claret.
    Claret,
    /// The desktop's own surface, light or dark as the theme is.
    Desktop,
}

impl Baize {
    /// Every table, in menu order.
    pub const ALL: [Self; 4] = [Self::Green, Self::Slate, Self::Claret, Self::Desktop];

    /// Its spelling in the store.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Slate => "slate",
            Self::Claret => "claret",
            Self::Desktop => "desktop",
        }
    }

    /// The menu's word for it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Green => "Green baize",
            Self::Slate => "Slate baize",
            Self::Claret => "Claret baize",
            Self::Desktop => "Desktop colours",
        }
    }
}

/// How the player likes to play.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Preferences {
    /// The game dealt next.
    pub variant: Variant,
    /// How Klondike is scored.
    pub scoring: Scoring,
    /// Deal only games the solver has shown can be won.
    pub winnable: bool,
    /// Send cards home whenever no later play could want them.
    pub auto_home: bool,
    /// Draw clubs green and diamonds blue, so all four suits differ at a
    /// glance.
    pub four_colours: bool,
    /// The card backs.
    pub back: Back,
    /// The table.
    pub baize: Baize,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            variant: Variant::Klondike(Draw::One),
            scoring: Scoring::Standard,
            winnable: true,
            auto_home: true,
            four_colours: false,
            back: Back::default(),
            baize: Baize::default(),
        }
    }
}

/// One preference.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Pref {
    /// [`Preferences::variant`].
    Variant,
    /// [`Preferences::scoring`].
    Scoring,
    /// [`Preferences::winnable`].
    Winnable,
    /// [`Preferences::auto_home`].
    AutoHome,
    /// [`Preferences::four_colours`].
    FourColours,
    /// [`Preferences::back`].
    Back,
    /// [`Preferences::baize`].
    Baize,
}

impl Registry for Preferences {
    type Key = Pref;
    const KEYS: &'static [Pref] = &[
        Pref::Variant,
        Pref::Scoring,
        Pref::Winnable,
        Pref::AutoHome,
        Pref::FourColours,
        Pref::Back,
        Pref::Baize,
    ];

    fn name(key: Pref) -> &'static str {
        match key {
            Pref::Variant => "game",
            Pref::Scoring => "scoring",
            Pref::Winnable => "deals.winnable",
            Pref::AutoHome => "play.auto-home",
            Pref::FourColours => "cards.four-colour",
            Pref::Back => "cards.back",
            Pref::Baize => "table.baize",
        }
    }

    fn read(&mut self, key: Pref, text: &str) -> bool {
        match key {
            Pref::Variant => Variant::named(text).map(|v| self.variant = v).is_some(),
            Pref::Scoring => Scoring::named(text).map(|s| self.scoring = s).is_some(),
            Pref::Winnable => as_bool(text).map(|b| self.winnable = b).is_ok(),
            Pref::AutoHome => as_bool(text).map(|b| self.auto_home = b).is_ok(),
            Pref::FourColours => as_bool(text).map(|b| self.four_colours = b).is_ok(),
            Pref::Back => Back::ALL
                .into_iter()
                .find(|back| back.name() == text)
                .map(|back| self.back = back)
                .is_some(),
            Pref::Baize => Baize::ALL
                .into_iter()
                .find(|baize| baize.name() == text)
                .map(|baize| self.baize = baize)
                .is_some(),
        }
    }

    fn spell(&self, key: Pref, out: &mut String) -> bool {
        let text = match key {
            Pref::Variant => self.variant.name(),
            Pref::Scoring => self.scoring.name(),
            Pref::Winnable => bool_text(self.winnable),
            Pref::AutoHome => bool_text(self.auto_home),
            Pref::FourColours => bool_text(self.four_colours),
            Pref::Back => self.back.name(),
            Pref::Baize => self.baize.name(),
        };
        write!(out, "{text}").is_ok()
    }
}

impl Live for Preferences {
    fn take(&mut self, other: &Self, key: Pref) -> bool {
        match key {
            Pref::Variant => overwrite(&mut self.variant, other.variant),
            Pref::Scoring => overwrite(&mut self.scoring, other.scoring),
            Pref::Winnable => overwrite(&mut self.winnable, other.winnable),
            Pref::AutoHome => overwrite(&mut self.auto_home, other.auto_home),
            Pref::FourColours => overwrite(&mut self.four_colours, other.four_colours),
            Pref::Back => overwrite(&mut self.back, other.back),
            Pref::Baize => overwrite(&mut self.baize, other.baize),
        }
    }
}

#[cfg(test)]
#[path = "prefs_tests.rs"]
mod tests;
