//! Invariant harness for the touch gesture recogniser, fed arbitrary
//! sequences of valid touch frames — what an injector holding
//! `CAP_INPUT_INJECT` may send.
//!
//! Whatever the frames, the recogniser
//!
//! * releases only what it pressed, and once [`Recogniser::reset`] has run
//!   holds nothing pressed;
//! * runs each surface's pinch as one life — begin, updates, then an end or a
//!   cancel — never two at once and never a step outside one;
//! * never states a deadline it has already acted on;
//! * never overflows, however large the surface, the step or the time.
//!
//! A plain `cargo test` runs the [`SMOKE_ITERATIONS`] sweep once from a fresh,
//! logged seed; `cargo xtask fuzz` exports `TAIRIX_FUZZ_BUDGET_SECS` to extend
//! the loop to a wall-clock budget.

use std::collections::HashMap;

use tairix_abi::touch::{
    Contact, ContactKind, ContactPhase, PinchPhase, TouchButtons, TouchExtent, TouchFrame,
    TouchSurface, TOUCH_CONTACTS_MAX,
};
use tairix_fuzzseed::Prng;
use tairix_touch::{Gesture, Recogniser, TouchPress, TouchSettings};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 2_000;

/// Frames in one sequence.
const FRAMES: usize = 200;

const SURFACES: [TouchSurface; 3] = [
    TouchSurface::Touchpad,
    TouchSurface::Clickpad,
    TouchSurface::Screen,
];

/// What the gestures so far hold: presses per button, and a pinch.
#[derive(Default)]
struct Held {
    presses: Vec<(TouchPress, u32)>,
    pinching: bool,
}

/// Which kinds of gesture the sweep has seen, so a generator too weak to
/// reach one fails rather than passing having explored nothing.
#[derive(Default)]
struct Seen {
    kinds: u8,
}

impl Seen {
    const ALL: u8 = 0b11_1111;

    fn note(&mut self, gesture: Gesture) {
        self.kinds |= match gesture {
            Gesture::MovedBy { .. } => 1 << 0,
            Gesture::MovedTo(_) => 1 << 1,
            Gesture::Pressed(_) | Gesture::Released(_) => 1 << 2,
            Gesture::Scrolled { .. } => 1 << 3,
            Gesture::Pinch(pinch) if pinch.phase == PinchPhase::Begin => 1 << 4,
            Gesture::Pinch(_) => 1 << 5,
        };
    }
}

impl Held {
    /// Account for `gesture`, `one_surface` when every frame is one surface's
    /// so a pinch's life is checkable.
    fn take(&mut self, gesture: Gesture, one_surface: bool) {
        match gesture {
            Gesture::Pressed(press) => *self.count(press) += 1,
            Gesture::Released(press) => {
                let held = self.count(press);
                assert!(*held > 0, "{press:?} released while not pressed");
                *held -= 1;
            }
            Gesture::Pinch(pinch) if one_surface => {
                match pinch.phase {
                    PinchPhase::Begin => {
                        assert!(!self.pinching, "a pinch began inside another");
                    }
                    PinchPhase::Update | PinchPhase::End | PinchPhase::Cancel => {
                        assert!(self.pinching, "{:?} outside a pinch", pinch.phase);
                    }
                }
                self.pinching = !pinch.phase.ends();
            }
            _ => {}
        }
    }

    fn count(&mut self, press: TouchPress) -> &mut u32 {
        let index = self
            .presses
            .iter()
            .position(|(held, _)| *held == press)
            .unwrap_or_else(|| {
                self.presses.push((press, 0));
                self.presses.len() - 1
            });
        &mut self.presses[index].1
    }

    fn is_empty(&self) -> bool {
        !self.pinching && self.presses.iter().all(|&(_, held)| held == 0)
    }
}

fn settings(rng: &mut Prng) -> TouchSettings {
    TouchSettings {
        tap_to_click: rng.below(2) == 0,
        natural_scroll: rng.below(2) == 0,
        speed_percent: if rng.below(8) == 0 {
            rng.next_u16()
        } else {
            25 + u16::try_from(rng.below(376)).unwrap_or(0)
        },
    }
}

fn extent(rng: &mut Prng) -> TouchExtent {
    match rng.below(4) {
        0 => TouchExtent::default(),
        1 => TouchExtent {
            width: u16::MAX,
            height: u16::MAX,
        },
        _ => TouchExtent {
            width: 1 + rng.next_u16() % 4_000,
            height: 1 + rng.next_u16() % 4_000,
        },
    }
}

/// A finger the sequence keeps down across frames.
#[derive(Clone, Copy)]
struct Down {
    id: u16,
    x: u16,
    y: u16,
    palm: bool,
}

/// Move, lift, drop, judge and land fingers for the next frame.
fn step(rng: &mut Prng, fingers: &mut Vec<Down>) -> Vec<Contact> {
    let mut contacts = Vec::new();
    let mut kept = Vec::new();
    for mut finger in fingers.drain(..) {
        let walk = |at: u16, rng: &mut Prng| {
            let reach = if rng.below(10) == 0 { 30_000 } else { 400 };
            let delta = i32::try_from(rng.below(2 * reach + 1)).unwrap_or(0)
                - i32::try_from(reach).unwrap_or(0);
            u16::try_from((i32::from(at) + delta).clamp(0, i32::from(u16::MAX))).unwrap_or(0)
        };
        finger.x = walk(finger.x, rng);
        finger.y = walk(finger.y, rng);
        finger.palm |= rng.below(40) == 0;
        let kind = if finger.palm {
            ContactKind::Palm
        } else {
            ContactKind::Finger
        };
        match rng.below(12) {
            // Lifted, stated as such.
            0 => contacts.push(Contact {
                id: finger.id,
                phase: ContactPhase::Up,
                kind,
                x: finger.x,
                y: finger.y,
            }),
            // Lost: the frame no longer names it.
            1 => {}
            _ => {
                contacts.push(Contact {
                    id: finger.id,
                    phase: ContactPhase::Down,
                    kind,
                    x: finger.x,
                    y: finger.y,
                });
                kept.push(finger);
            }
        }
    }
    *fingers = kept;
    while contacts.len() < TOUCH_CONTACTS_MAX && rng.below(3) == 0 {
        let id = rng.next_u16() % 16;
        if contacts.iter().any(|contact| contact.id == id) {
            continue;
        }
        let finger = Down {
            id,
            x: rng.next_u16(),
            y: rng.next_u16(),
            palm: rng.below(10) == 0,
        };
        contacts.push(Contact {
            id,
            phase: ContactPhase::Down,
            kind: if finger.palm {
                ContactKind::Palm
            } else {
                ContactKind::Finger
            },
            x: finger.x,
            y: finger.y,
        });
        fingers.push(finger);
    }
    contacts
}

/// A next instant: mostly a frame interval on, sometimes a pause, a burst or
/// a step back, and rarely the far end of the clock.
fn advance(rng: &mut Prng, now: u64) -> u64 {
    match rng.below(20) {
        0 => now.saturating_sub(rng.next_u64() % 50_000_000),
        1 => now.saturating_add(rng.next_u64() % 1_000_000_000),
        2 if rng.below(50) == 0 => u64::MAX - rng.next_u64() % 1_000_000_000,
        3 => now,
        _ => now.saturating_add(4_000_000 + rng.next_u64() % 12_000_000),
    }
}

/// Run one sequence over `devices` surfaces of one injector.
fn run(rng: &mut Prng, devices: u16, seen: &mut Seen) {
    let one_surface = devices == 1;
    let mut recogniser = Recogniser::new(settings(rng));
    recogniser.set_screen(rng.next_u32(), rng.next_u32(), rng.next_u32() % 1_000);
    let mut held = Held::default();
    let mut fingers: HashMap<u16, Vec<Down>> = HashMap::new();
    let mut now: u64 = rng.next_u64() % 1_000_000_000_000;
    let mut kinds: HashMap<u16, (TouchSurface, TouchExtent)> = HashMap::new();
    for _ in 0..FRAMES {
        now = advance(rng, now);
        let device = u16::try_from(rng.below(usize::from(devices))).unwrap_or(0);
        if rng.below(100) == 0 {
            kinds.remove(&device);
        }
        let (surface, size) = *kinds
            .entry(device)
            .or_insert_with(|| (*rng.pick(&SURFACES), extent(rng)));
        let contacts = step(rng, fingers.entry(device).or_default());
        let buttons = if rng.below(3) == 0 {
            rng.next_u8() & 0b111
        } else {
            0
        };
        let mut frame = TouchFrame::new(
            device,
            surface,
            TouchButtons::from_bits(buttons).expect("defined buttons"),
            size,
        );
        for contact in contacts {
            frame.push(contact).expect("a frame holds every contact");
        }
        let frame = frame.stamped(7, now);
        let mut out = |gesture| {
            seen.note(gesture);
            held.take(gesture, one_surface);
        };
        recogniser.feed(&frame, &mut out);
        if rng.below(4) == 0 {
            let at = advance(rng, now);
            recogniser.expire(at, &mut out);
            if let Some(deadline) = recogniser.deadline_ns() {
                assert!(
                    deadline > at,
                    "a deadline at {deadline} survived expiry at {at}"
                );
            }
        }
        if rng.below(150) == 0 {
            recogniser.set_settings(settings(rng));
        }
        if rng.below(300) == 0 {
            recogniser.reset(&mut out);
            assert!(held.is_empty(), "a reset left something held");
            fingers.clear();
        }
    }
    recogniser.reset(&mut |gesture| held.take(gesture, one_surface));
    assert!(held.is_empty(), "the final reset left something held");
    assert_eq!(recogniser.deadline_ns(), None);
}

#[test]
fn any_frames_leave_presses_balanced_pinches_whole_and_deadlines_ahead() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_frames_leave_presses_balanced_pinches_whole_and_deadlines_ahead",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut seen = Seen::default();
    let mut iteration: u64 = 0;
    loop {
        run(&mut rng, 1, &mut seen);
        let devices = 1 + u16::try_from(rng.below(12)).unwrap_or(0);
        run(&mut rng, devices, &mut seen);
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
    assert_eq!(
        seen.kinds,
        Seen::ALL,
        "the sweep never reached some gesture"
    );
}
