//! A touchscreen: one finger is the pointer at the place touched, two scroll
//! and pinch at their centre.

use tairix_input::PointerButton;

use crate::fingers::Fingers;
use crate::two::{ScrollRate, Two};
use crate::{Gesture, SurfacePoint, TouchPress};

/// How far a finger moves before its press is a drag rather than waiting.
const PRESS_SLOP_UM: i64 = 1_500;

/// How long a still finger waits before pressing: long enough for a second
/// finger landing with it to make a gesture instead of a click.
const PRESS_HOLD_NS: u64 = 100_000_000;

/// The content follows the fingers about as far as they travel, a wheel
/// detent scrolling roughly 12 mm of screen.
const SCROLL: ScrollRate = ScrollRate {
    units_per_mm: 10,
    natural: true,
};

const PRESS: TouchPress = TouchPress::Fingers(PointerButton::Primary);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Idle,
    /// One finger landed and the pointer is at `place`; its press waits.
    Landing {
        id: u16,
        began_ns: u64,
        place: SurfacePoint,
    },
    /// One finger pressing, the pointer following it.
    Pressing {
        id: u16,
        place: SurfacePoint,
    },
    /// Two fingers.
    Gesture,
    /// Fingers left after a gesture, or more than two: nothing until all lift.
    Spent,
}

/// A touchscreen's recogniser.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Screen {
    state: State,
    two: Two,
}

impl Screen {
    pub const fn new() -> Self {
        Self {
            state: State::Idle,
            two: Two::Off,
        }
    }

    pub fn frame(&mut self, fingers: &Fingers, now_ns: u64, out: &mut dyn FnMut(Gesture)) {
        let count = fingers.count();
        self.state = match self.state {
            State::Idle | State::Spent if count == 0 => State::Idle,
            State::Spent => State::Spent,
            State::Idle => match fingers.touching().next() {
                Some(finger) if count == 1 => {
                    out(Gesture::MovedTo(finger.place));
                    State::Landing {
                        id: finger.id,
                        began_ns: now_ns,
                        place: finger.place,
                    }
                }
                _ => self.gesture(fingers, out),
            },
            State::Landing {
                id,
                began_ns,
                place,
            } => match fingers.find(id) {
                _ if count > 1 => self.gesture(fingers, out),
                Some(finger) if finger.palm => settled(count),
                Some(finger) if finger.touching() => {
                    if finger.travelled().exceeds(PRESS_SLOP_UM)
                        || now_ns.saturating_sub(began_ns) >= PRESS_HOLD_NS
                    {
                        out(Gesture::Pressed(PRESS));
                        follow(finger.place, place, out);
                        State::Pressing {
                            id,
                            place: finger.place,
                        }
                    } else {
                        State::Landing {
                            id,
                            began_ns,
                            place,
                        }
                    }
                }
                _ => {
                    out(Gesture::Pressed(PRESS));
                    out(Gesture::Released(PRESS));
                    settled(count)
                }
            },
            State::Pressing { id, place } => match fingers.find(id) {
                _ if count > 1 => {
                    out(Gesture::Released(PRESS));
                    self.gesture(fingers, out)
                }
                Some(finger) if finger.touching() => {
                    follow(finger.place, place, out);
                    State::Pressing {
                        id,
                        place: finger.place,
                    }
                }
                lifted => {
                    if let Some(finger) = lifted {
                        follow(finger.place, place, out);
                    }
                    out(Gesture::Released(PRESS));
                    settled(count)
                }
            },
            State::Gesture => {
                if let Some(pair) = fingers.pair() {
                    self.two.follow(&pair, SCROLL, true, out);
                    State::Gesture
                } else {
                    self.two.stop(out);
                    settled(count)
                }
            }
        };
    }

    /// Two fingers begin a gesture with the pointer at their centre; more
    /// begin nothing.
    fn gesture(&mut self, fingers: &Fingers, out: &mut dyn FnMut(Gesture)) -> State {
        let Some(pair) = fingers.pair() else {
            return State::Spent;
        };
        out(Gesture::MovedTo(pair.place));
        self.two.follow(&pair, SCROLL, true, out);
        State::Gesture
    }

    pub const fn deadline_ns(&self) -> Option<u64> {
        match self.state {
            State::Landing { began_ns, .. } => Some(began_ns.saturating_add(PRESS_HOLD_NS)),
            _ => None,
        }
    }

    pub fn expire(&mut self, now_ns: u64, out: &mut dyn FnMut(Gesture)) {
        if let State::Landing {
            id,
            began_ns,
            place,
        } = self.state
        {
            if began_ns.saturating_add(PRESS_HOLD_NS) <= now_ns {
                out(Gesture::Pressed(PRESS));
                self.state = State::Pressing { id, place };
            }
        }
    }

    /// Release a press and undo a pinch.
    pub fn end(&mut self, out: &mut dyn FnMut(Gesture)) {
        self.two.cancel(out);
        if let State::Pressing { .. } = self.state {
            out(Gesture::Released(PRESS));
        }
        *self = Self::new();
    }

    pub fn is_idle(&self) -> bool {
        self.state == State::Idle && self.two.is_off()
    }
}

/// Move the pointer to `now` when it is not already there.
fn follow(now: SurfacePoint, was: SurfacePoint, out: &mut dyn FnMut(Gesture)) {
    if now != was {
        out(Gesture::MovedTo(now));
    }
}

/// Where a touch that ended its press or gesture stands: idle once every
/// finger is up, spent while any is left.
const fn settled(count: usize) -> State {
    if count == 0 {
        State::Idle
    } else {
        State::Spent
    }
}
