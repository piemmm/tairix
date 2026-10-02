//! A touchpad: pointer motion, taps, its buttons, and two-finger gestures.

use tairix_abi::input::PointerButtonCode;
use tairix_abi::touch::TouchButtons;
use tairix_input::PointerButton;

use crate::fingers::{Fingers, Um};
use crate::two::{ScrollRate, Two};
use crate::{Gesture, TouchPress, TouchSettings};

/// The longest a touch can last and still be a tap.
const TAP_TIME_NS: u64 = 180_000_000;

/// How far a finger can move and still be tapping.
const TAP_MOVE_UM: i64 = 1_300;

/// How long a one-finger tap's press is held for a touch that follows to
/// drag with it.
const TAP_DRAG_NS: u64 = 180_000_000;

/// The most fingers a tap counts: one, two or three click the primary,
/// secondary or middle button.
const TAP_FINGERS_MAX: usize = 3;

/// Two fingers scroll faster than they travel, as a touchpad is smaller than
/// the screen it scrolls.
const SCROLL_UNITS_PER_MM: i64 = 25;

/// The pointer gain, in pixels per metre of finger travel, at or below the
/// slow speed and at or above the fast one, scaled linearly between: slow
/// motion is fine enough to place the pointer on a pixel, a flick crosses
/// the screen.
const GAIN_SLOW: i64 = 4_000;
const GAIN_FAST: i64 = 16_000;

/// Finger speeds, in micrometres per millisecond (millimetres a second), the
/// gain ramps between.
const SPEED_SLOW: i64 = 30;
const SPEED_FAST: i64 = 300;

/// The shortest and longest frame interval a speed is measured over: a burst
/// of frames is not a flick, and a pause is not a crawl.
const INTERVAL_MIN_NS: u64 = 1_000_000;
const INTERVAL_MAX_NS: u64 = 40_000_000;

/// Pixels times this are what `um × gain × percent` comes to.
const PIXEL_SCALE: i64 = 100_000_000;

/// The primary button the fingers make on a touchpad.
const PRIMARY: TouchPress = TouchPress::Fingers(PointerButton::Primary);

/// What a touch sequence may still be.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Tap {
    Idle,
    /// Fingers are down that might still lift as a tap of `fingers`.
    Possible {
        began_ns: u64,
        fingers: usize,
    },
    /// Fingers are down that cannot.
    Spent,
    /// A one-finger tap's press is held, for a touch that follows to drag.
    Held {
        until_ns: u64,
    },
    /// A finger landed while the press was held: a second tap if it lifts
    /// at once, a drag otherwise.
    Following {
        began_ns: u64,
    },
    /// Dragging with the press held until the finger lifts.
    Dragging,
}

impl Tap {
    /// Whether finger motion waits, because a tap would not have moved.
    const fn withholds(self) -> bool {
        matches!(self, Self::Possible { .. } | Self::Following { .. })
    }

    /// Whether the tap's primary press is down.
    const fn holds_press(self) -> bool {
        matches!(
            self,
            Self::Held { .. } | Self::Following { .. } | Self::Dragging
        )
    }
}

/// How the pointer follows a finger: the speed it has been moving at, and the
/// part of a pixel not yet moved.
#[derive(Clone, Copy, Debug)]
struct Glide {
    finger: Option<u16>,
    moved_ns: u64,
    speed: i64,
    carry: Um,
}

impl Glide {
    const fn new() -> Self {
        Self {
            finger: None,
            moved_ns: 0,
            speed: 0,
            carry: Um::new(0, 0),
        }
    }

    /// The pixels `travel` is worth, the finger `id` having moved `step` of
    /// it in this frame at `now_ns`.
    fn pixels(&mut self, id: u16, travel: Um, step: Um, now_ns: u64, percent: u16) -> (i32, i32) {
        let same = self.finger == Some(id);
        let interval = if same {
            now_ns
                .saturating_sub(self.moved_ns)
                .clamp(INTERVAL_MIN_NS, INTERVAL_MAX_NS)
        } else {
            INTERVAL_MAX_NS
        };
        let instant = i64::try_from(
            u128::from(step.length().unsigned_abs()) * 1_000_000 / u128::from(interval),
        )
        .unwrap_or(i64::MAX);
        self.speed = if same {
            self.speed.midpoint(instant)
        } else {
            instant
        };
        self.finger = Some(id);
        self.moved_ns = now_ns;
        let gain = gain(self.speed) * i64::from(percent);
        (
            pixel(travel.x, gain, &mut self.carry.x),
            pixel(travel.y, gain, &mut self.carry.y),
        )
    }
}

/// The gain at finger speed `speed`.
const fn gain(speed: i64) -> i64 {
    if speed <= SPEED_SLOW {
        GAIN_SLOW
    } else if speed >= SPEED_FAST {
        GAIN_FAST
    } else {
        GAIN_SLOW + (GAIN_FAST - GAIN_SLOW) * (speed - SPEED_SLOW) / (SPEED_FAST - SPEED_SLOW)
    }
}

fn pixel(um: i64, gain: i64, carry: &mut i64) -> i32 {
    let total = carry.saturating_add(um.saturating_mul(gain));
    let whole = total / PIXEL_SCALE;
    *carry = total - whole * PIXEL_SCALE;
    i32::try_from(whole).unwrap_or(if whole < 0 { i32::MIN } else { i32::MAX })
}

/// The physical button bits, in the order their edges are read.
const BUTTONS: [(u8, PointerButtonCode); 3] = [
    (TouchButtons::PRIMARY, PointerButtonCode::Primary),
    (TouchButtons::SECONDARY, PointerButtonCode::Secondary),
    (TouchButtons::MIDDLE, PointerButtonCode::Middle),
];

/// A touchpad's recogniser.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pad {
    clickpad: bool,
    tap: Tap,
    two: Two,
    buttons: TouchButtons,
    /// The press each of the surface's buttons made, in [`BUTTONS`] order.
    pressed: [Option<TouchPress>; 3],
    /// Finger motion a possible tap has held back.
    withheld: Um,
    glide: Glide,
}

impl Pad {
    pub const fn new(clickpad: bool) -> Self {
        Self {
            clickpad,
            tap: Tap::Idle,
            two: Two::Off,
            buttons: TouchButtons::NONE,
            pressed: [None; 3],
            withheld: Um::new(0, 0),
            glide: Glide::new(),
        }
    }

    pub fn frame(
        &mut self,
        fingers: &Fingers,
        buttons: TouchButtons,
        now_ns: u64,
        settings: TouchSettings,
        out: &mut dyn FnMut(Gesture),
    ) {
        let rising = buttons.bits() & !self.buttons.bits();
        let falling = self.buttons.bits() & !buttons.bits();
        self.buttons = buttons;
        let held = buttons.bits() != 0;
        let rate = ScrollRate {
            units_per_mm: SCROLL_UNITS_PER_MM,
            natural: settings.natural_scroll,
        };
        let decided = match fingers.pair() {
            Some(pair) if !held => self.two.follow(&pair, rate, false, out),
            _ => {
                self.two.stop(out);
                false
            }
        };
        self.follow_tap(fingers, now_ns, settings, rising != 0 || decided, out);
        self.move_pointer(fingers, held, now_ns, settings, out);
        self.press(fingers.count(), rising, falling, out);
    }

    fn follow_tap(
        &mut self,
        fingers: &Fingers,
        now_ns: u64,
        settings: TouchSettings,
        disqualified: bool,
        out: &mut dyn FnMut(Gesture),
    ) {
        let count = fingers.count();
        let moved = fingers
            .all()
            .iter()
            .any(|finger| !finger.palm && finger.travelled().exceeds(TAP_MOVE_UM));
        self.tap = match self.tap {
            Tap::Idle | Tap::Spent if count == 0 => Tap::Idle,
            Tap::Idle => self.begin(count, now_ns, settings, disqualified),
            Tap::Spent => Tap::Spent,
            Tap::Possible { began_ns, fingers } => {
                let fingers = fingers.max(count);
                if disqualified
                    || moved
                    || fingers > TAP_FINGERS_MAX
                    || now_ns.saturating_sub(began_ns) > TAP_TIME_NS
                {
                    Tap::Spent
                } else if count == 0 {
                    self.click(fingers, now_ns, out)
                } else {
                    Tap::Possible { began_ns, fingers }
                }
            }
            Tap::Held { until_ns } => {
                if count == 0 && !disqualified {
                    Tap::Held { until_ns }
                } else if count == 1 && !disqualified {
                    Tap::Following { began_ns: now_ns }
                } else {
                    out(Gesture::Released(PRIMARY));
                    if count == 0 {
                        Tap::Idle
                    } else {
                        self.begin(count, now_ns, settings, disqualified)
                    }
                }
            }
            Tap::Following { began_ns } => {
                let quick = now_ns.saturating_sub(began_ns) <= TAP_TIME_NS;
                if disqualified || count > 1 {
                    out(Gesture::Released(PRIMARY));
                    Tap::Spent
                } else if count == 0 {
                    if quick && !moved {
                        self.withheld = Um::default();
                        out(Gesture::Released(PRIMARY));
                        out(Gesture::Pressed(PRIMARY));
                    } else {
                        self.flush(now_ns, settings, out);
                    }
                    out(Gesture::Released(PRIMARY));
                    Tap::Idle
                } else if moved || !quick {
                    Tap::Dragging
                } else {
                    Tap::Following { began_ns }
                }
            }
            Tap::Dragging => {
                if count == 1 && !disqualified {
                    Tap::Dragging
                } else {
                    out(Gesture::Released(PRIMARY));
                    if count == 0 {
                        Tap::Idle
                    } else {
                        Tap::Spent
                    }
                }
            }
        };
    }

    /// The tap state a touch of `count` fingers begins.
    fn begin(&self, count: usize, now_ns: u64, settings: TouchSettings, disqualified: bool) -> Tap {
        if settings.tap_to_click
            && !disqualified
            && self.buttons == TouchButtons::NONE
            && count <= TAP_FINGERS_MAX
        {
            Tap::Possible {
                began_ns: now_ns,
                fingers: count,
            }
        } else {
            Tap::Spent
        }
    }

    /// A tap of `fingers` lifted: click.
    fn click(&mut self, fingers: usize, now_ns: u64, out: &mut dyn FnMut(Gesture)) -> Tap {
        self.withheld = Um::default();
        let button = match fingers {
            1 => {
                out(Gesture::Pressed(PRIMARY));
                return Tap::Held {
                    until_ns: now_ns.saturating_add(TAP_DRAG_NS),
                };
            }
            2 => PointerButton::Secondary,
            _ => PointerButton::Middle,
        };
        out(Gesture::Pressed(TouchPress::Fingers(button)));
        out(Gesture::Released(TouchPress::Fingers(button)));
        Tap::Idle
    }

    /// Send the motion a possible tap held back.
    fn flush(&mut self, now_ns: u64, settings: TouchSettings, out: &mut dyn FnMut(Gesture)) {
        let withheld = core::mem::take(&mut self.withheld);
        if withheld != Um::default() {
            let id = self.glide.finger.unwrap_or_default();
            let (dx, dy) =
                self.glide
                    .pixels(id, withheld, Um::default(), now_ns, settings.speed_percent);
            if dx != 0 || dy != 0 {
                out(Gesture::MovedBy { dx, dy });
            }
        }
    }

    /// One finger moves the pointer; while a button is held, whichever finger
    /// moves does, so a thumb can hold a clickpad down while a finger drags.
    fn move_pointer(
        &mut self,
        fingers: &Fingers,
        held: bool,
        now_ns: u64,
        settings: TouchSettings,
        out: &mut dyn FnMut(Gesture),
    ) {
        let mut touching = fingers.touching();
        let driver = if held {
            touching.max_by_key(|finger| finger.moved().length())
        } else {
            match (touching.next(), touching.next()) {
                (Some(only), None) => Some(only),
                _ => None,
            }
        };
        let Some(finger) = driver else {
            self.glide.finger = None;
            return;
        };
        let step = finger.moved();
        if self.tap.withholds() {
            self.withheld = self.withheld.plus(step);
            return;
        }
        let travel = core::mem::take(&mut self.withheld).plus(step);
        let (dx, dy) = self
            .glide
            .pixels(finger.id, travel, step, now_ns, settings.speed_percent);
        if dx != 0 || dy != 0 {
            out(Gesture::MovedBy { dx, dy });
        }
    }

    /// The surface's own buttons: releases first, then presses. A clickpad's
    /// press is the button its fingers count.
    fn press(&mut self, count: usize, rising: u8, falling: u8, out: &mut dyn FnMut(Gesture)) {
        for (index, (bit, _)) in BUTTONS.iter().enumerate() {
            if falling & bit != 0 {
                if let Some(press) = self.pressed[index].take() {
                    out(Gesture::Released(press));
                }
            }
        }
        for (index, &(bit, code)) in BUTTONS.iter().enumerate() {
            if rising & bit == 0 {
                continue;
            }
            let press = if self.clickpad && bit == TouchButtons::PRIMARY {
                TouchPress::Fingers(match count {
                    2 => PointerButton::Secondary,
                    3 => PointerButton::Middle,
                    _ => PointerButton::Primary,
                })
            } else {
                TouchPress::Device(code)
            };
            self.pressed[index] = Some(press);
            out(Gesture::Pressed(press));
        }
    }

    pub const fn deadline_ns(&self) -> Option<u64> {
        match self.tap {
            Tap::Held { until_ns } => Some(until_ns),
            _ => None,
        }
    }

    pub fn expire(&mut self, now_ns: u64, out: &mut dyn FnMut(Gesture)) {
        if let Tap::Held { until_ns } = self.tap {
            if until_ns <= now_ns {
                out(Gesture::Released(PRIMARY));
                self.tap = Tap::Idle;
            }
        }
    }

    /// Release everything held and undo a pinch.
    pub fn end(&mut self, out: &mut dyn FnMut(Gesture)) {
        self.two.cancel(out);
        if self.tap.holds_press() {
            out(Gesture::Released(PRIMARY));
        }
        for press in self.pressed.iter_mut().filter_map(Option::take) {
            out(Gesture::Released(press));
        }
        *self = Self::new(self.clickpad);
    }

    pub fn is_idle(&self) -> bool {
        self.tap == Tap::Idle && self.two.is_off() && self.pressed.iter().all(Option::is_none)
    }
}
