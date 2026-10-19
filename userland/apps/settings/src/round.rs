//! What one round of the window's event loop lays out and presents
//! ([`Round`]).
//!
//! Decided here rather than in the bare-metal loop so it is tested on the
//! host: a round that adopted an answer rebuilt the pane, whose rows report
//! nothing of their own, and a round that concluded anything is presented
//! even where it reported no rectangle.

use tairix_geometry::{Rect, Region};
use tairix_window::Repaint;

/// How one round of the event loop ended, as far as presenting it goes.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Round {
    /// Every pixel may have changed: the desktop moved, another pane came on
    /// show, or the client was resized.
    pub whole: bool,
    /// The event concluded anything at all.
    pub acted: bool,
    /// An answer was adopted during the round, rebuilding the pane.
    pub answered: bool,
}

impl Round {
    /// Whether the shell must be laid out again before it is drawn: an
    /// adopted answer rebuilt rows no layout has measured.
    #[must_use]
    pub const fn relays(self) -> bool {
        self.answered
    }

    /// What the round presents, `damage` widened to `pane` where an answer
    /// rebuilt it.
    #[must_use]
    pub fn repaint(self, damage: &mut Region, pane: Rect) -> Repaint {
        if self.answered {
            damage.add(pane);
        }
        if self.whole {
            Repaint::Whole
        } else {
            Repaint::reported_if(self.acted || !damage.is_empty())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANE: Rect = Rect::new(224, 40, 556, 560);

    #[test]
    fn an_answered_round_lays_out_and_presents_the_pane_it_rebuilt() {
        let round = Round {
            answered: true,
            ..Round::default()
        };
        assert!(round.relays());
        let mut damage = Region::new();
        assert_eq!(round.repaint(&mut damage, PANE), Repaint::Reported);
        assert_eq!(damage.bounds(), PANE);
    }

    #[test]
    fn a_round_that_acted_is_presented_though_it_reported_nothing() {
        let round = Round {
            acted: true,
            ..Round::default()
        };
        assert!(!round.relays());
        let mut damage = Region::new();
        assert_eq!(round.repaint(&mut damage, PANE), Repaint::Reported);
        assert!(damage.is_empty(), "the present covers the window instead");
    }

    #[test]
    fn a_whole_round_presents_the_window_and_an_idle_one_nothing() {
        let whole = Round {
            whole: true,
            ..Round::default()
        };
        assert_eq!(whole.repaint(&mut Region::new(), PANE), Repaint::Whole);
        assert_eq!(
            Round::default().repaint(&mut Region::new(), PANE),
            Repaint::Nothing
        );
        let mut reported = Region::new();
        reported.add(Rect::new(0, 0, 10, 10));
        assert_eq!(
            Round::default().repaint(&mut reported, PANE),
            Repaint::Reported,
            "what an idle round still reported is presented"
        );
    }
}
