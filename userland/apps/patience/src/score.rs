//! Scores: what a line of play is worth.
//!
//! Klondike scores one of two classic ways, chosen before the deal because
//! Vegas also limits how often the waste may be turned. Spider scores by its
//! own one rule; FreeCell keeps no score, only moves and time. A score is a
//! function of the moves now on the table, so an undo takes a move's points
//! back with it and no line of play can be farmed.

use crate::table::{Action, Applied};
use crate::variant::{Draw, Recycles, Role, Rules, Shape, Variant};

/// How Klondike is scored.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Scoring {
    /// Points for each useful move, less a little for time.
    #[default]
    Standard,
    /// A dollar a card staked, five back for each card home, and the waste
    /// turned once drawing one or three times drawing three.
    Vegas,
    /// Vegas, with the winnings carried from game to game.
    Cumulative,
}

impl Scoring {
    /// Every scoring, in menu order.
    pub const ALL: [Self; 3] = [Self::Standard, Self::Vegas, Self::Cumulative];

    /// Its spelling in the game's store.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Vegas => "vegas",
            Self::Cumulative => "cumulative",
        }
    }

    /// The scoring `name` spells.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|scoring| scoring.name() == name)
    }

    /// The menu's words for it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Standard => "Standard scoring",
            Self::Vegas => "Vegas scoring",
            Self::Cumulative => "Cumulative Vegas",
        }
    }

    /// Whether it is one of the Vegas rules.
    #[must_use]
    pub const fn is_vegas(self) -> bool {
        matches!(self, Self::Vegas | Self::Cumulative)
    }

    /// The rules `variant` is dealt under with this scoring.
    #[must_use]
    pub const fn rules(self, variant: Variant) -> Rules {
        let recycles = match (variant, self.is_vegas()) {
            (Variant::Klondike(Draw::One), true) => Recycles::AtMost(0),
            (Variant::Klondike(Draw::Three), true) => Recycles::AtMost(2),
            _ => Recycles::Unlimited,
        };
        Rules { variant, recycles }
    }
}

/// A score as it is shown.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Score {
    /// Points.
    Points(i64),
    /// Vegas winnings, which may be negative.
    Dollars(i64),
}

impl Score {
    /// The number shown.
    #[must_use]
    pub const fn value(self) -> i64 {
        match self {
            Self::Points(value) | Self::Dollars(value) => value,
        }
    }
}

/// Points a Klondike move earns under [`Scoring::Standard`].
#[must_use]
pub fn standard_points(shape: Shape, draw: Draw, applied: &Applied) -> i64 {
    let mut points = match applied.action {
        Action::Move { from, to, .. } => match (shape.role(from), shape.role(to)) {
            (Some(Role::Waste), Some(Role::Tableau)) => 5,
            (Some(Role::Waste | Role::Tableau), Some(Role::Foundation)) => 10,
            (Some(Role::Foundation), Some(Role::Tableau)) => -15,
            _ => 0,
        },
        Action::Draw => 0,
        Action::Recycle => match draw {
            Draw::One => -100,
            Draw::Three => -20,
        },
    };
    if applied.turned {
        points += 5;
    }
    points
}

/// What standard scoring takes for `secs` of play: two points a ten seconds.
#[must_use]
pub fn time_penalty(secs: u64) -> i64 {
    i64::try_from((secs / 10).saturating_mul(2)).unwrap_or(i64::MAX)
}

/// The bonus a standard win earns for its speed, for games of thirty seconds
/// or more.
#[must_use]
pub fn time_bonus(secs: u64) -> i64 {
    if secs < 30 {
        return 0;
    }
    i64::try_from(700_000 / secs).unwrap_or(0)
}

/// The Vegas stake: a dollar a card.
pub const VEGAS_STAKE: i64 = 52;
/// What Vegas pays for each card home.
pub const VEGAS_PAYOUT: i64 = 5;

/// What a Spider deal starts with.
pub const SPIDER_START: i64 = 500;
/// What each completed Spider run earns.
pub const SPIDER_RUN: i64 = 100;

#[cfg(test)]
#[path = "score_tests.rs"]
mod tests;
