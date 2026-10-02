//! Scenario tests for the touch gesture recogniser.

extern crate std;

use std::vec::Vec;

use tairix_abi::input::PointerButtonCode;
use tairix_abi::touch::{
    Contact, ContactKind, PinchPhase, TouchButtons, TouchExtent, TouchFrame, TouchSurface,
    PINCH_SCALE_ONE,
};
use tairix_input::PointerButton;

use super::{Gesture, Pinch, Recogniser, SurfacePoint, TouchPress, TouchSettings};

const MS: u64 = 1_000_000;

/// A surface so large a normalised step is exactly 0.1 mm, so every distance
/// below is exact.
const EXACT: TouchExtent = TouchExtent {
    width: u16::MAX,
    height: u16::MAX,
};

/// Steps of the exact surface in a millimetre.
const STEPS_PER_MM: u16 = 10;

const PRIMARY: TouchPress = TouchPress::Fingers(PointerButton::Primary);
const SECONDARY: TouchPress = TouchPress::Fingers(PointerButton::Secondary);
const MIDDLE: TouchPress = TouchPress::Fingers(PointerButton::Middle);

const NO_TAPPING: TouchSettings = TouchSettings {
    tap_to_click: false,
    ..TouchSettings::DEFAULT
};

const TRADITIONAL: TouchSettings = TouchSettings {
    natural_scroll: false,
    ..TouchSettings::DEFAULT
};

/// A finger `id` at `(x, y)`, in tenths of a millimetre on [`EXACT`].
const fn finger(id: u16, x: u16, y: u16) -> Contact {
    Contact::finger(id, x, y)
}

const fn mm(millimetres: u16) -> u16 {
    millimetres * STEPS_PER_MM
}

struct Sim {
    recogniser: Recogniser,
    surface: TouchSurface,
    extent: TouchExtent,
    device: u16,
}

impl Sim {
    fn new(surface: TouchSurface, settings: TouchSettings) -> Self {
        Self {
            recogniser: Recogniser::new(settings),
            surface,
            extent: EXACT,
            device: 0,
        }
    }

    fn pad(settings: TouchSettings) -> Self {
        Self::new(TouchSurface::Touchpad, settings)
    }

    fn screen() -> Self {
        Self::new(TouchSurface::Screen, TouchSettings::DEFAULT)
    }

    fn frame(&self, at_ms: u64, buttons: u8, contacts: &[Contact]) -> TouchFrame {
        let mut frame = TouchFrame::new(
            self.device,
            self.surface,
            TouchButtons::from_bits(buttons).expect("defined buttons"),
            self.extent,
        );
        for &contact in contacts {
            frame.push(contact).expect("a frame holds the contact");
        }
        frame.stamped(1, at_ms * MS)
    }

    fn pressing(&mut self, at_ms: u64, buttons: u8, contacts: &[Contact]) -> Vec<Gesture> {
        let frame = self.frame(at_ms, buttons, contacts);
        let mut out = Vec::new();
        self.recogniser
            .feed(&frame, &mut |gesture| out.push(gesture));
        out
    }

    fn at(&mut self, at_ms: u64, contacts: &[Contact]) -> Vec<Gesture> {
        self.pressing(at_ms, 0, contacts)
    }

    fn expire(&mut self, at_ms: u64) -> Vec<Gesture> {
        let mut out = Vec::new();
        self.recogniser
            .expire(at_ms * MS, &mut |gesture| out.push(gesture));
        out
    }

    fn reset(&mut self) -> Vec<Gesture> {
        let mut out = Vec::new();
        self.recogniser.reset(&mut |gesture| out.push(gesture));
        out
    }

    /// One finger dragged `steps` frames 10 ms apart, `step` tenths of a
    /// millimetre rightward each, from `start_ms`.
    fn drag(&mut self, start_ms: u64, steps: u16, step: u16) -> Vec<Gesture> {
        let mut out = self.at(start_ms, &[finger(1, mm(10), mm(10))]);
        for index in 1..=steps {
            out.extend(self.at(
                start_ms + u64::from(index) * 10,
                &[finger(1, mm(10) + index * step, mm(10))],
            ));
        }
        out
    }
}

fn moved(gestures: &[Gesture]) -> (i32, i32) {
    gestures
        .iter()
        .fold((0, 0), |(x, y), gesture| match gesture {
            Gesture::MovedBy { dx, dy } => (x + dx, y + dy),
            _ => (x, y),
        })
}

fn scrolled(gestures: &[Gesture]) -> (i32, i32) {
    gestures
        .iter()
        .fold((0, 0), |(x, y), gesture| match gesture {
            Gesture::Scrolled { dx, dy } => (x + dx, y + dy),
            _ => (x, y),
        })
}

fn presses(gestures: &[Gesture]) -> Vec<Gesture> {
    gestures
        .iter()
        .copied()
        .filter(|gesture| matches!(gesture, Gesture::Pressed(_) | Gesture::Released(_)))
        .collect()
}

fn pinches(gestures: &[Gesture]) -> Vec<Pinch> {
    gestures
        .iter()
        .filter_map(|gesture| match gesture {
            Gesture::Pinch(pinch) => Some(*pinch),
            _ => None,
        })
        .collect()
}

#[test]
fn one_finger_moves_the_pointer_further_the_faster_it_moves() {
    // Five millimetres, slowly: four pixels a millimetre.
    let slow = moved(&Sim::pad(NO_TAPPING).drag(0, 50, 1));
    assert_eq!(slow, (20, 0));
    // Fifty millimetres in a flick moves far more than fifty slowly would.
    let fast = moved(&Sim::pad(NO_TAPPING).drag(0, 10, 50));
    assert!(fast.0 > 600, "a flick crosses the screen: {fast:?}");
    assert_eq!(fast.1, 0);
}

#[test]
fn the_speed_setting_scales_the_pointer() {
    let doubled = TouchSettings {
        speed_percent: 200,
        ..NO_TAPPING
    };
    assert_eq!(moved(&Sim::pad(doubled).drag(0, 50, 1)), (40, 0));
}

#[test]
fn a_tap_clicks_and_holds_its_press_for_a_drag_that_never_comes() {
    let mut pad = Sim::pad(TouchSettings::DEFAULT);
    assert_eq!(pad.at(0, &[finger(1, 100, 100)]), []);
    assert_eq!(pad.at(50, &[]), [Gesture::Pressed(PRIMARY)]);
    assert_eq!(pad.recogniser.deadline_ns(), Some(230 * MS));
    assert_eq!(pad.expire(229), []);
    assert_eq!(pad.expire(230), [Gesture::Released(PRIMARY)]);
    assert_eq!(pad.recogniser.deadline_ns(), None);
}

#[test]
fn a_touch_that_follows_a_tap_drags_with_its_press_held() {
    let mut pad = Sim::pad(TouchSettings::DEFAULT);
    pad.at(0, &[finger(1, 100, 100)]);
    assert_eq!(pad.at(50, &[]), [Gesture::Pressed(PRIMARY)]);
    let dragged = pad.drag(100, 30, 1);
    assert_eq!(presses(&dragged), [], "the press stays held");
    assert_eq!(moved(&dragged), (12, 0), "three millimetres, none lost");
    assert_eq!(pad.at(500, &[]), [Gesture::Released(PRIMARY)]);
    assert_eq!(pad.recogniser.deadline_ns(), None);
}

#[test]
fn a_second_tap_makes_a_double_click() {
    let mut pad = Sim::pad(TouchSettings::DEFAULT);
    pad.at(0, &[finger(1, 100, 100)]);
    pad.at(50, &[]);
    assert_eq!(pad.at(100, &[finger(2, 101, 100)]), []);
    assert_eq!(
        pad.at(140, &[]),
        [
            Gesture::Released(PRIMARY),
            Gesture::Pressed(PRIMARY),
            Gesture::Released(PRIMARY)
        ]
    );
    assert_eq!(pad.recogniser.deadline_ns(), None);
}

#[test]
fn two_and_three_finger_taps_click_secondary_and_middle() {
    let mut pad = Sim::pad(TouchSettings::DEFAULT);
    pad.at(0, &[finger(1, mm(10), mm(10))]);
    pad.at(10, &[finger(1, mm(10), mm(10)), finger(2, mm(30), mm(10))]);
    pad.at(60, &[finger(2, mm(30), mm(10))]);
    assert_eq!(
        pad.at(70, &[]),
        [Gesture::Pressed(SECONDARY), Gesture::Released(SECONDARY)]
    );
    let three = [
        finger(1, mm(10), mm(10)),
        finger(2, mm(30), mm(10)),
        finger(3, mm(50), mm(10)),
    ];
    pad.at(1_000, &three);
    assert_eq!(
        pad.at(1_050, &[]),
        [Gesture::Pressed(MIDDLE), Gesture::Released(MIDDLE)]
    );
}

#[test]
fn a_tap_that_moves_or_lasts_is_no_click_and_its_motion_is_kept() {
    let mut pad = Sim::pad(TouchSettings::DEFAULT);
    let travelled = pad.drag(0, 10, 2);
    assert_eq!(presses(&travelled), []);
    assert_eq!(moved(&travelled), (8, 0), "the held-back motion arrives");
    assert_eq!(pad.at(110, &[]), []);

    pad.at(1_000, &[finger(1, 100, 100)]);
    assert_eq!(pad.at(1_300, &[]), [], "a rest is no tap");
}

#[test]
fn tapping_off_moves_at_once_and_never_clicks() {
    // Six tenths of a millimetre: less than a tap may move.
    assert_eq!(moved(&Sim::pad(NO_TAPPING).drag(0, 3, 2)), (2, 0));
    assert_eq!(
        moved(&Sim::pad(TouchSettings::DEFAULT).drag(0, 3, 2)),
        (0, 0),
        "held back while it may still be a tap"
    );
    let mut pad = Sim::pad(NO_TAPPING);
    pad.at(0, &[finger(1, 100, 100)]);
    assert_eq!(pad.at(30, &[]), []);
    assert_eq!(pad.recogniser.deadline_ns(), None);
}

/// Two fingers 20 mm apart moved together by `step` tenths a frame.
fn two_finger_scroll(sim: &mut Sim, frames: u16, step: (i16, i16)) -> Vec<Gesture> {
    let place = |index: u16, base: u16, step: i16| {
        base.checked_add_signed(step * i16::try_from(index).expect("few frames"))
            .expect("on the surface")
    };
    let mut out = Vec::new();
    for index in 0..=frames {
        let (x, y) = (place(index, mm(20), step.0), place(index, mm(20), step.1));
        out.extend(sim.at(
            u64::from(index) * 10,
            &[finger(1, x, y), finger(2, x + mm(20), y)],
        ));
    }
    out
}

#[test]
fn two_fingers_scroll_both_ways_and_natural_reverses_them() {
    // Ten millimetres down: 25 units a millimetre.
    let natural = two_finger_scroll(&mut Sim::pad(TouchSettings::DEFAULT), 10, (0, 10));
    assert_eq!(scrolled(&natural), (0, -250));
    let traditional = two_finger_scroll(&mut Sim::pad(TRADITIONAL), 10, (0, 10));
    assert_eq!(scrolled(&traditional), (0, 250));
    assert_eq!(moved(&traditional), (0, 0), "two fingers move no pointer");
    let sideways = two_finger_scroll(&mut Sim::pad(TRADITIONAL), 10, (-10, 0));
    assert_eq!(scrolled(&sideways), (-250, 0));
}

#[test]
fn a_scroll_keeps_to_the_axis_it_began_on_unless_it_began_diagonal() {
    let mut pad = Sim::pad(TRADITIONAL);
    let mut out = two_finger_scroll(&mut pad, 2, (1, 10));
    let fingers = |x: u16, y: u16| [finger(1, x, y), finger(2, x + mm(20), y)];
    out.extend(pad.at(30, &fingers(mm(20) + mm(5), mm(20) + 20)));
    assert_eq!(scrolled(&out), (0, 50), "the sideways drift is dropped");

    let diagonal = two_finger_scroll(&mut Sim::pad(TRADITIONAL), 10, (10, 10));
    assert_eq!(scrolled(&diagonal), (250, 250));
}

#[test]
fn a_slow_scroll_is_carried_not_lost() {
    let slow = two_finger_scroll(&mut Sim::pad(TRADITIONAL), 100, (0, 1));
    assert_eq!(scrolled(&slow), (0, 250));
}

/// Two fingers about `centre`, spread `spread` tenths apart in frame order.
fn spread_frames(sim: &mut Sim, centre: u16, spreads: &[u16]) -> Vec<Gesture> {
    let mut out = Vec::new();
    for (index, &spread) in spreads.iter().enumerate() {
        out.extend(sim.at(
            u64::try_from(index).expect("few frames") * 10,
            &[
                finger(1, centre - spread / 2, mm(30)),
                finger(2, centre + spread / 2, mm(30)),
            ],
        ));
    }
    out
}

#[test]
fn two_fingers_spreading_pinch_from_one_and_end_when_one_lifts() {
    let mut pad = Sim::pad(TouchSettings::DEFAULT);
    let out = spread_frames(&mut pad, mm(50), &[mm(20), mm(22), mm(24), mm(44)]);
    assert_eq!(
        pinches(&out),
        [
            Pinch {
                phase: PinchPhase::Begin,
                scale: PINCH_SCALE_ONE,
                at: None,
            },
            Pinch {
                phase: PinchPhase::Update,
                scale: PINCH_SCALE_ONE * 24 / 22,
                at: None,
            },
            Pinch {
                phase: PinchPhase::Update,
                scale: PINCH_SCALE_ONE * 2,
                at: None,
            },
        ]
    );
    assert_eq!(scrolled(&out), (0, 0), "a pinch scrolls nothing");
    assert_eq!(
        pinches(&pad.at(40, &[finger(1, mm(28), mm(30))])),
        [Pinch {
            phase: PinchPhase::End,
            scale: PINCH_SCALE_ONE * 2,
            at: None,
        }]
    );
}

#[test]
fn a_reset_undoes_a_pinch_and_releases_every_press() {
    let mut pad = Sim::pad(TouchSettings::DEFAULT);
    spread_frames(&mut pad, mm(50), &[mm(20), mm(24)]);
    assert_eq!(
        pinches(&pad.reset()),
        [Pinch {
            phase: PinchPhase::Cancel,
            scale: PINCH_SCALE_ONE,
            at: None,
        }]
    );
    pad.at(1_000, &[finger(1, 100, 100)]);
    pad.at(1_050, &[]);
    assert_eq!(pad.reset(), [Gesture::Released(PRIMARY)], "the held tap");
    assert_eq!(pad.recogniser.deadline_ns(), None);
    assert_eq!(pad.reset(), []);
}

#[test]
fn a_clickpad_press_counts_its_fingers() {
    let mut pad = Sim::new(TouchSurface::Clickpad, TouchSettings::DEFAULT);
    let one = [finger(1, mm(10), mm(10))];
    pad.at(0, &one);
    assert_eq!(
        pad.pressing(10, TouchButtons::PRIMARY, &one),
        [Gesture::Pressed(PRIMARY)]
    );
    assert_eq!(pad.at(20, &one), [Gesture::Released(PRIMARY)]);
    let two = [finger(1, mm(10), mm(10)), finger(2, mm(30), mm(10))];
    pad.at(1_000, &two);
    assert_eq!(
        presses(&pad.pressing(1_010, TouchButtons::PRIMARY, &two)),
        [Gesture::Pressed(SECONDARY)]
    );
    // Moving with the press held scrolls nothing.
    let moved_two = [finger(1, mm(10), mm(20)), finger(2, mm(30), mm(20))];
    assert_eq!(
        scrolled(&pad.pressing(1_020, TouchButtons::PRIMARY, &moved_two)),
        (0, 0)
    );
    assert_eq!(
        presses(&pad.at(1_030, &moved_two)),
        [Gesture::Released(SECONDARY)],
        "a release is the press it ends"
    );
    let three = [
        finger(1, mm(10), mm(10)),
        finger(2, mm(30), mm(10)),
        finger(3, mm(50), mm(10)),
    ];
    pad.at(2_000, &three);
    assert_eq!(
        presses(&pad.pressing(2_010, TouchButtons::PRIMARY, &three)),
        [Gesture::Pressed(MIDDLE)]
    );
}

#[test]
fn a_touchpads_own_buttons_are_device_buttons_and_end_a_held_tap() {
    let mut pad = Sim::pad(TouchSettings::DEFAULT);
    pad.at(0, &[finger(1, 100, 100)]);
    pad.at(50, &[]);
    assert_eq!(
        pad.pressing(100, TouchButtons::SECONDARY, &[]),
        [
            Gesture::Released(PRIMARY),
            Gesture::Pressed(TouchPress::Device(PointerButtonCode::Secondary))
        ]
    );
    assert_eq!(
        pad.at(150, &[]),
        [Gesture::Released(TouchPress::Device(
            PointerButtonCode::Secondary
        ))]
    );
}

#[test]
fn a_palm_takes_part_in_nothing() {
    let palm = |id: u16, x: u16, y: u16| Contact {
        kind: ContactKind::Palm,
        ..finger(id, x, y)
    };
    let mut pad = Sim::pad(NO_TAPPING);
    pad.at(0, &[palm(9, mm(50), mm(50))]);
    assert_eq!(
        pad.at(10, &[palm(9, mm(60), mm(50))]),
        [],
        "a palm moves nothing"
    );
    // Beside a resting palm one finger is one finger: it moves the pointer.
    pad.at(20, &[palm(9, mm(60), mm(50)), finger(1, mm(10), mm(10))]);
    let out = pad.at(30, &[palm(9, mm(60), mm(50)), finger(1, mm(20), mm(10))]);
    assert_eq!(scrolled(&out), (0, 0));
    assert!(moved(&out).0 > 0);
    // A finger the device comes to judge a palm stops moving the pointer.
    assert_eq!(
        pad.at(40, &[palm(9, mm(60), mm(50)), palm(1, mm(30), mm(10))]),
        []
    );
}

#[test]
fn a_contact_a_frame_no_longer_names_has_lifted() {
    let mut screen = Sim::screen();
    screen.at(0, &[finger(1, 100, 100)]);
    screen.expire(100);
    assert_eq!(
        screen.at(200, &[finger(2, 900, 900)]),
        [Gesture::Released(PRIMARY)],
        "the finger the frame lost lifted where it last was"
    );
    assert_eq!(
        screen.at(210, &[finger(2, 950, 900)]),
        [],
        "a finger left over does nothing until every finger lifts"
    );
    assert_eq!(screen.at(220, &[]), []);
    assert_eq!(screen.recogniser.deadline_ns(), None);
}

#[test]
fn a_touch_puts_the_pointer_there_and_presses_once_it_moves() {
    let mut screen = Sim::screen();
    let landed = SurfacePoint { x: 500, y: 500 };
    assert_eq!(
        screen.at(0, &[finger(1, 500, 500)]),
        [Gesture::MovedTo(landed)]
    );
    assert_eq!(screen.at(10, &[finger(1, 510, 505)]), [], "within the slop");
    assert_eq!(
        screen.at(20, &[finger(1, 520, 500)]),
        [
            Gesture::Pressed(PRIMARY),
            Gesture::MovedTo(SurfacePoint { x: 520, y: 500 })
        ]
    );
    assert_eq!(
        screen.at(30, &[finger(1, 540, 500)]),
        [Gesture::MovedTo(SurfacePoint { x: 540, y: 500 })]
    );
    assert_eq!(screen.at(40, &[]), [Gesture::Released(PRIMARY)]);
}

#[test]
fn a_still_touch_presses_when_its_wait_runs_out() {
    let mut screen = Sim::screen();
    screen.at(0, &[finger(1, 500, 500)]);
    assert_eq!(screen.recogniser.deadline_ns(), Some(100 * MS));
    assert_eq!(screen.expire(99), []);
    assert_eq!(screen.expire(100), [Gesture::Pressed(PRIMARY)]);
    assert_eq!(screen.recogniser.deadline_ns(), None);
    assert_eq!(screen.at(400, &[]), [Gesture::Released(PRIMARY)]);
}

#[test]
fn a_quick_touch_is_a_click_at_its_place() {
    let mut screen = Sim::screen();
    screen.at(0, &[finger(1, 500, 500)]);
    assert_eq!(
        screen.at(30, &[]),
        [Gesture::Pressed(PRIMARY), Gesture::Released(PRIMARY)]
    );
}

#[test]
fn a_second_finger_landing_makes_a_gesture_not_a_click() {
    let mut screen = Sim::screen();
    screen.at(0, &[finger(1, mm(100), mm(100))]);
    let out = screen.at(
        30,
        &[finger(1, mm(100), mm(100)), finger(2, mm(120), mm(100))],
    );
    assert_eq!(
        out,
        [Gesture::MovedTo(SurfacePoint {
            x: mm(110),
            y: mm(100)
        })]
    );
    // Upward, so the content follows them up and the view moves on.
    let out = screen.at(
        40,
        &[finger(1, mm(100), mm(90)), finger(2, mm(120), mm(90))],
    );
    assert_eq!(scrolled(&out), (0, 100), "ten units a millimetre");
    assert_eq!(presses(&out), []);
    assert_eq!(screen.at(50, &[finger(2, mm(120), mm(90))]), []);
    assert_eq!(screen.at(60, &[finger(2, mm(140), mm(90))]), [], "spent");
    assert_eq!(screen.at(70, &[]), []);
}

#[test]
fn a_second_finger_ends_a_press_before_the_gesture() {
    let mut screen = Sim::screen();
    screen.at(0, &[finger(1, mm(100), mm(100))]);
    screen.expire(100);
    assert_eq!(
        screen.at(
            200,
            &[finger(1, mm(100), mm(100)), finger(2, mm(120), mm(100))]
        ),
        [
            Gesture::Released(PRIMARY),
            Gesture::MovedTo(SurfacePoint {
                x: mm(110),
                y: mm(100)
            })
        ]
    );
}

#[test]
fn a_touchscreen_pinch_is_placed_at_the_fingers_centre() {
    let mut screen = Sim::screen();
    let out = spread_frames(&mut screen, mm(100), &[mm(20), mm(24), mm(40)]);
    let centre = Some(SurfacePoint {
        x: mm(100),
        y: mm(30),
    });
    assert_eq!(
        pinches(&out),
        [
            Pinch {
                phase: PinchPhase::Begin,
                scale: PINCH_SCALE_ONE,
                at: centre,
            },
            Pinch {
                phase: PinchPhase::Update,
                scale: PINCH_SCALE_ONE * 40 / 24,
                at: centre,
            },
        ]
    );
    // The fingers' centre moving pans the pinch without changing its scale.
    let panned = screen.at(30, &[finger(1, mm(90), mm(40)), finger(2, mm(130), mm(40))]);
    assert_eq!(
        pinches(&panned),
        [Pinch {
            phase: PinchPhase::Update,
            scale: PINCH_SCALE_ONE * 40 / 24,
            at: Some(SurfacePoint {
                x: mm(110),
                y: mm(40)
            }),
        }]
    );
}

#[test]
fn a_touchscreen_without_its_own_size_covers_the_screen() {
    let still = |screen_width: u16| {
        let mut sim = Sim::screen();
        sim.extent = TouchExtent::default();
        // At 254 DPI a pixel is a tenth of a millimetre.
        sim.recogniser
            .set_screen(u32::from(screen_width), u32::from(screen_width), 254);
        sim.at(0, &[finger(1, 0, 0)]);
        presses(&sim.at(10, &[finger(1, 1_000, 0)]))
    };
    // A thousand steps across a 65 mm screen is a millimetre, inside the
    // slop; across a 650 mm one it is ten.
    assert_eq!(still(650), []);
    assert_eq!(still(6_500), [Gesture::Pressed(PRIMARY)]);
}

#[test]
fn two_surfaces_are_followed_apart() {
    let mut sim = Sim::pad(NO_TAPPING);
    sim.at(0, &[finger(1, mm(10), mm(10))]);
    sim.device = 1;
    sim.at(5, &[finger(1, mm(10), mm(10))]);
    sim.device = 0;
    let first = sim.at(10, &[finger(1, mm(20), mm(10))]);
    sim.device = 1;
    let second = sim.at(15, &[finger(1, mm(10), mm(20))]);
    assert_eq!(scrolled(&first), (0, 0), "one finger each is no scroll");
    assert!(moved(&first).0 > 0 && moved(&second).1 > 0);
}

#[test]
fn the_least_recently_fed_surface_is_let_go_releasing_what_it_held() {
    let mut sim = Sim::screen();
    for device in 0..=7 {
        sim.device = device;
        sim.at(u64::from(device), &[finger(1, 100, 100)]);
    }
    sim.expire(200);
    sim.device = 8;
    assert_eq!(
        sim.at(300, &[finger(1, 100, 100)]),
        [
            Gesture::Released(PRIMARY),
            Gesture::MovedTo(SurfacePoint { x: 100, y: 100 })
        ],
        "device 0 let go, device 8 followed"
    );
    sim.device = 1;
    assert_eq!(
        sim.at(310, &[]),
        [Gesture::Released(PRIMARY)],
        "still followed"
    );
}

#[test]
fn a_frame_stamped_earlier_is_read_at_the_newest_time() {
    let mut pad = Sim::pad(TouchSettings::DEFAULT);
    pad.at(500, &[finger(1, 100, 100)]);
    assert_eq!(pad.at(400, &[]), [Gesture::Pressed(PRIMARY)]);
    assert_eq!(pad.recogniser.deadline_ns(), Some(680 * MS));
}

#[test]
fn a_surface_that_changes_its_kind_is_a_new_surface() {
    let mut sim = Sim::screen();
    sim.at(0, &[finger(1, 100, 100)]);
    sim.expire(100);
    sim.surface = TouchSurface::Touchpad;
    assert_eq!(
        sim.at(200, &[finger(1, 100, 100)]),
        [Gesture::Released(PRIMARY)]
    );
}

#[test]
fn a_place_maps_edge_to_edge_onto_any_screen() {
    let corner = SurfacePoint {
        x: u16::MAX,
        y: u16::MAX,
    };
    assert_eq!(corner.on_screen(640, 480), (639, 479));
    assert_eq!(SurfacePoint { x: 0, y: 0 }.on_screen(640, 480), (0, 0));
    let middle = SurfacePoint {
        x: u16::MAX / 2 + 1,
        y: u16::MAX / 2 + 1,
    };
    assert_eq!(middle.on_screen(641, 481), (320, 240));
    assert_eq!(corner.on_screen(0, 1), (0, 0), "no screen is pixel zero");
    assert_eq!(
        corner.on_screen(u32::MAX, u32::MAX),
        (u32::MAX - 1, u32::MAX - 1)
    );
}
