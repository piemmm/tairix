//! The cast: which craft come on, and when.
//!
//! Two lanes play apart: the sky, where the spaceship passes or the saucer
//! visits, and the ground, where the tanks fight or the riders duel. Each
//! lane rests a while after an act before the next, and seldom plays one
//! kind twice running, so something is only now and then on the screen and
//! seldom the same thing. Every act draws its randomness from a stream of its
//! own, seeded as it begins, so how it plays does not hang on how the frames
//! fall.

use tairix_rng::{NonCryptoRng, RandU64};

use super::blast::Blasts;
use super::riders::Duel;
use super::saucer::Saucer;
use super::ship::Ship;
use super::tanks::Battle;
use super::wire::{Camera, Models, Stage};
use super::{between, Moment};

/// How likely a lane is to play the kind it did not play last.
const CHANGE: f64 = 0.75;

/// Where a lane plays: in the sky, or on the ground.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Realm {
    Sky,
    Ground,
}

impl Realm {
    /// How long the lane waits before its first act, in seconds.
    const fn first(self) -> (f64, f64) {
        match self {
            Self::Sky => (7.0, 14.0),
            Self::Ground => (2.0, 5.0),
        }
    }

    /// How long the lane rests after an act, in seconds.
    const fn rest(self) -> (f64, f64) {
        match self {
            Self::Sky => (10.0, 26.0),
            Self::Ground => (8.0, 20.0),
        }
    }
}

/// One act on the stage.
#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "each lane holds one act at a time, so the room its largest act sets is paid \
              twice at most; boxing it would trade that for an allocation that cannot fail \
              gracefully"
)]
enum Act {
    Ship(Ship),
    Saucer(Saucer),
    Tanks(Battle),
    Riders(Duel),
}

impl Act {
    /// The act a lane playing in `realm` brings on at `time`: its first kind
    /// or its `second`, played from a stream seeded from `rng`.
    fn bring_on(
        realm: Realm,
        second: bool,
        (rng, time): (&mut NonCryptoRng, f64),
        (camera, speed): (&Camera, f64),
    ) -> Self {
        let mut own = NonCryptoRng::seed_from_u64(rng.next_u64());
        let view = camera.view();
        let tall = f64::from(view.height);
        let aspect = f64::from(view.width) / tall;
        match (realm, second) {
            (Realm::Sky, false) => Self::Ship(Ship::new(&mut own, time, aspect)),
            (Realm::Sky, true) => Self::Saucer(Saucer::new(
                &mut own,
                time,
                (aspect, f64::from(view.horizon) / tall),
            )),
            (Realm::Ground, false) => Self::Tanks(Battle::new(own, time, camera, speed)),
            (Realm::Ground, true) => Self::Riders(Duel::new(own, time, camera, speed)),
        }
    }

    /// Whether it is over by `time`.
    fn is_over(&self, time: f64) -> bool {
        match self {
            Self::Ship(ship) => ship.is_over(time),
            Self::Saucer(saucer) => saucer.is_over(time),
            Self::Tanks(battle) => battle.is_over(),
            Self::Riders(duel) => duel.is_over(time),
        }
    }
}

/// One lane: the act it plays, or when the next is due; and whether its
/// second kind played last.
#[derive(Debug)]
struct Lane {
    realm: Realm,
    act: Option<Act>,
    due: f64,
    second_last: bool,
}

/// Every craft that comes on, and when.
pub(super) struct Cast {
    rng: NonCryptoRng,
    /// Cells a second the flight goes.
    speed: f64,
    lanes: [Lane; 2],
    blasts: Blasts,
    /// Whether motion is reduced: then nothing comes on at all.
    calm: bool,
}

impl Cast {
    /// A cast scattered from the stream `stream` begins for a flight going
    /// `speed` cells a second, still when `calm`; `None` when the heap will
    /// not give it room.
    pub(super) fn new(stream: u64, speed: f64, calm: bool) -> Option<Self> {
        let mut rng = NonCryptoRng::seed_from_u64(stream);
        // Which kind a lane counts as last played is drawn too, so its first
        // act is either kind alike.
        let lanes = [Realm::Sky, Realm::Ground].map(|realm| Lane {
            realm,
            act: None,
            due: between(&mut rng, realm.first()),
            second_last: rng.next_below(2) == 1,
        });
        Some(Self {
            rng,
            speed,
            lanes,
            blasts: Blasts::new()?,
            calm,
        })
    }

    /// Play every act on to `moment`, as `camera` sees it, bringing on the
    /// next where a lane's rest is over.
    pub(super) fn advance(&mut self, moment: Moment, camera: &Camera, models: &Models) {
        if self.calm {
            return;
        }
        let time = moment.time;
        let Self {
            rng,
            speed,
            lanes,
            blasts,
            ..
        } = self;
        blasts.retire(time);
        for lane in lanes {
            if let Some(act) = &mut lane.act {
                match act {
                    Act::Tanks(battle) => battle.advance(time, camera, models, blasts),
                    Act::Riders(duel) => duel.advance(time, camera, models, blasts),
                    Act::Ship(_) | Act::Saucer(_) => {}
                }
                if act.is_over(time) {
                    lane.act = None;
                    lane.due = time + between(rng, lane.realm.rest());
                }
            } else if time >= lane.due {
                let second = lane.second_last != (rng.next_f64() < CHANGE);
                lane.second_last = second;
                lane.act = Some(Act::bring_on(
                    lane.realm,
                    second,
                    (rng, time),
                    (camera, *speed),
                ));
            }
        }
    }

    /// Set out every act and blast as it stands at `moment`.
    pub(super) fn stage(&self, moment: Moment, stage: &mut Stage) {
        let time = moment.time;
        for lane in &self.lanes {
            match &lane.act {
                Some(Act::Ship(ship)) => ship.stage(time, self.speed, stage),
                Some(Act::Saucer(saucer)) => saucer.stage(time, stage),
                Some(Act::Tanks(battle)) => battle.stage(time, stage),
                Some(Act::Riders(duel)) => duel.stage(time, stage),
                None => {}
            }
        }
        self.blasts.stage(time, stage);
    }
}

#[cfg(test)]
#[path = "cast_tests.rs"]
mod tests;
