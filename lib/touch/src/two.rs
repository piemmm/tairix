//! Two fingers: a scroll or a pinch, decided by which they do first.

use tairix_abi::touch::{PinchPhase, PINCH_SCALE_ONE};

use crate::fingers::{Pair, Um};
use crate::{Gesture, Pinch, SurfacePoint};

/// How far the fingers' centre travels before two fingers are a scroll.
const SCROLL_START_UM: i64 = 1_000;

/// How far the fingers' spread changes before two fingers are a pinch: twice
/// the scroll's travel, as each finger moves half of it.
const PINCH_START_UM: i64 = 2 * SCROLL_START_UM;

/// The least spread a pinch begins at: closer than this, two contacts are
/// too near for their spread to be measured reliably.
const PINCH_SPREAD_MIN_UM: i64 = 8_000;

/// How finger travel becomes scroll units.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ScrollRate {
    pub units_per_mm: i64,
    /// The content follows the fingers, so the view moves against them.
    pub natural: bool,
}

/// The axis a scroll keeps to, chosen by the direction it began in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Lock {
    Vertical,
    Horizontal,
    Free,
}

impl Lock {
    /// A start at least twice as far along one axis as the other keeps to it.
    const fn of(travel: Um) -> Self {
        let (across, down) = (travel.x.unsigned_abs(), travel.y.unsigned_abs());
        if down >= 2 * across {
            Self::Vertical
        } else if across >= 2 * down {
            Self::Horizontal
        } else {
            Self::Free
        }
    }

    const fn keep(self, travel: Um) -> Um {
        match self {
            Self::Vertical => Um::new(0, travel.y),
            Self::Horizontal => Um::new(travel.x, 0),
            Self::Free => travel,
        }
    }
}

/// What two fingers are doing.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Two {
    Off,
    /// Neither a scroll nor a pinch yet: where they were when they began.
    Deciding {
        centre: Um,
        spread: i64,
    },
    Scrolling {
        lock: Lock,
        last: Um,
        /// Thousandths of a scroll unit not yet sent, per axis.
        carry: Um,
    },
    Pinching {
        spread: i64,
        scale: u32,
        at: Option<SurfacePoint>,
    },
}

impl Two {
    /// Follow the two fingers of a frame, a pinch placed on the surface when
    /// `direct`. Answers whether this frame decided what they do.
    pub fn follow(
        &mut self,
        pair: &Pair,
        rate: ScrollRate,
        direct: bool,
        out: &mut dyn FnMut(Gesture),
    ) -> bool {
        let at = direct.then_some(pair.place);
        match self {
            Self::Off => {
                *self = Self::Deciding {
                    centre: pair.centre,
                    spread: pair.span,
                };
                false
            }
            Self::Deciding { centre, spread } => {
                let travel = pair.centre.minus(*centre);
                let spread_change = (pair.span - *spread).abs();
                let travelled = travel.length();
                if travelled < SCROLL_START_UM && spread_change < PINCH_START_UM {
                    return false;
                }
                if *spread >= PINCH_SPREAD_MIN_UM
                    && spread_change * SCROLL_START_UM >= travelled * PINCH_START_UM
                {
                    *self = Self::Pinching {
                        spread: pair.span,
                        scale: PINCH_SCALE_ONE,
                        at,
                    };
                    out(Gesture::Pinch(Pinch {
                        phase: PinchPhase::Begin,
                        scale: PINCH_SCALE_ONE,
                        at,
                    }));
                } else {
                    let lock = Lock::of(travel);
                    let mut carry = Um::default();
                    scroll(lock.keep(travel), rate, &mut carry, out);
                    *self = Self::Scrolling {
                        lock,
                        last: pair.centre,
                        carry,
                    };
                }
                true
            }
            Self::Scrolling { lock, last, carry } => {
                let travel = pair.centre.minus(*last);
                *last = pair.centre;
                scroll(lock.keep(travel), rate, carry, out);
                false
            }
            Self::Pinching {
                spread,
                scale,
                at: placed,
            } => {
                let now = scale_of(pair.span, *spread);
                if now != *scale || at != *placed {
                    *scale = now;
                    *placed = at;
                    out(Gesture::Pinch(Pinch {
                        phase: PinchPhase::Update,
                        scale: now,
                        at,
                    }));
                }
                false
            }
        }
    }

    /// The fingers stopped being two: a pinch ends where it stands.
    pub fn stop(&mut self, out: &mut dyn FnMut(Gesture)) {
        self.close(PinchPhase::End, out);
    }

    /// The seat ended the gesture: a pinch is undone.
    pub fn cancel(&mut self, out: &mut dyn FnMut(Gesture)) {
        self.close(PinchPhase::Cancel, out);
    }

    fn close(&mut self, phase: PinchPhase, out: &mut dyn FnMut(Gesture)) {
        if let Self::Pinching { scale, at, .. } = *self {
            out(Gesture::Pinch(Pinch { phase, scale, at }));
        }
        *self = Self::Off;
    }

    pub const fn is_off(&self) -> bool {
        matches!(self, Self::Off)
    }
}

/// The spread `now` relative to `began`, in 16.16 fixed point.
fn scale_of(now: i64, began: i64) -> u32 {
    let ratio = i128::from(now.max(0)) * i128::from(PINCH_SCALE_ONE) / i128::from(began.max(1));
    u32::try_from(ratio.max(1)).unwrap_or(u32::MAX)
}

fn scroll(travel: Um, rate: ScrollRate, carry: &mut Um, out: &mut dyn FnMut(Gesture)) {
    let travel = if rate.natural {
        Um::new(-travel.x, -travel.y)
    } else {
        travel
    };
    let dx = units(travel.x, rate.units_per_mm, &mut carry.x);
    let dy = units(travel.y, rate.units_per_mm, &mut carry.y);
    if dx != 0 || dy != 0 {
        out(Gesture::Scrolled { dx, dy });
    }
}

/// `um` of travel in scroll units, carrying the thousandths left over.
fn units(um: i64, per_mm: i64, carry: &mut i64) -> i32 {
    let total = *carry + um * per_mm;
    let whole = total / 1_000;
    *carry = total - whole * 1_000;
    i32::try_from(whole).unwrap_or(if whole < 0 { i32::MIN } else { i32::MAX })
}
