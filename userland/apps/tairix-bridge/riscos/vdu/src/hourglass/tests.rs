use alloc::vec::Vec;

use super::*;

const PROGRAM_COLOUR: [u32; 2] = [0x3020_1019; 2];
const OWN_COLOUR: [u32; 2] = [0xFFFF_0019; 2];

fn drawn(phase: u32, percentage: Option<u8>, leds: u8) -> Vec<u8> {
    let mut pixels = Vec::new();
    draw(
        Frame {
            phase,
            percentage,
            leds,
        },
        &mut pixels,
    );
    pixels
}

fn at(pixels: &[u8], x: u32, y: u32) -> u8 {
    pixels[(y * WIDTH + x) as usize]
}

fn count(pixels: &[u8], lines: Range<u32>, colour: u8) -> usize {
    lines
        .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
        .filter(|&(x, y)| at(pixels, x, y) == colour)
        .count()
}

#[test]
fn it_appears_once_its_delay_passes_then_turns_over_every_frame() {
    let mut glass = Hourglass::new();
    glass.on(100, DELAY);
    assert_eq!(glass.turn(100 + DELAY - 1), None, "not yet");
    assert_eq!(
        glass.turn(100 + DELAY),
        Some(Turn {
            shape: 4,
            appearing: true
        })
    );
    assert_eq!(glass.turn(100 + DELAY + FRAME - 1), None);
    assert_eq!(
        glass.turn(100 + DELAY + FRAME),
        Some(Turn {
            shape: 3,
            appearing: false
        })
    );
}

#[test]
fn its_frames_fall_on_its_own_clock_however_late_they_are_looked_at() {
    let mut glass = Hourglass::new();
    glass.on(0, 1);
    assert_eq!(
        glass.turn(46),
        Some(Turn {
            shape: 3,
            appearing: true
        }),
        "four frames by then, at 1, 16, 31 and 46"
    );
    assert_eq!(glass.due, Some(61));
    assert_eq!(glass.frame().map(|frame| frame.phase), Some(3));
}

#[test]
fn a_later_call_keeps_the_first_delay_and_a_start_of_none_holds_it_back() {
    let mut glass = Hourglass::new();
    glass.on(0, DELAY);
    glass.on(20, DELAY);
    glass.on(25, 100);
    assert_eq!(glass.due, Some(DELAY), "the first call's delay stands");
    glass.on(30, 0);
    assert_eq!(glass.due, None, "held back");
    assert_eq!(glass.turn(1000), None);
    for _ in 0..3 {
        assert_eq!(glass.off(), None);
    }
    assert_eq!(glass.calls, 1);
    assert_eq!(glass.off(), None, "never shown, so nothing to give back");
    assert_eq!(glass.off(), None, "an off no call is left for");
    assert_eq!(glass.calls, 0);
}

#[test]
fn a_start_of_no_delay_freezes_one_showing_until_its_calls_are_matched() {
    let mut glass = Hourglass::new();
    glass.on(0, 5);
    assert!(glass.turn(5).is_some());
    glass.on(10, 0);
    assert_eq!(glass.turn(500), None, "frozen");
    glass.on(20, 10);
    assert_eq!(glass.turn(500), None, "a start with a delay leaves it so");
    assert_eq!(glass.off(), None);
    assert_eq!(glass.off(), None);
    assert!(glass.off().is_some(), "the last call matched gives back");
    assert_eq!(glass.calls, 0);
}

#[test]
fn smash_matches_every_call_at_once() {
    let mut glass = Hourglass::new();
    glass.on(0, 1);
    glass.on(0, 1);
    assert!(glass.turn(1).is_some());
    assert!(glass.smash().is_some());
    assert_eq!((glass.calls, glass.due, glass.shown), (0, None, None));
    assert_eq!(glass.smash(), None);
}

#[test]
fn the_lights_are_a_byte_anded_then_exclusive_ored() {
    let mut glass = Hourglass::new();
    assert_eq!(glass.leds(1, 0), 0);
    assert_eq!(glass.leds(0, u32::MAX), 1);
    assert_eq!(glass.leds(u32::MAX, 0), 1);
    assert_eq!(glass.leds(2, 0xFFFF_FFFE), 0xFF, "kept as a byte");
    assert_eq!(glass.leds(0, u32::MAX), 0xFC);
}

#[test]
fn what_it_gives_back_is_what_the_program_last_set_of_the_pointer() {
    let mut glass = Hourglass::new();
    glass.on(0, 1);
    let frame = |glass: &mut Hourglass, now: u64, selection: u32, colour: [u32; 2]| {
        let turn = glass.turn(now).expect("a frame");
        glass.selected_over(turn.appearing, selection);
        glass.colour_over(0, colour);
        glass.colour_set(0, OWN_COLOUR);
        turn.shape
    };
    let shape = frame(&mut glass, 1, 1, [0x1111_1119; 2]);
    let shape = frame(&mut glass, 16, u32::from(shape), OWN_COLOUR);
    assert_eq!(glass.shown.map(|shown| shown.selection), Some(1));
    frame(&mut glass, 31, 2 | UNLINKED, PROGRAM_COLOUR);
    frame(&mut glass, 46, u32::from(shape), OWN_COLOUR);
    let shown = glass.smash().expect("it showed");
    assert_eq!(
        shown.selection,
        2 | UNLINKED,
        "the program's, set meanwhile"
    );
    assert_eq!(shown.saved[0], PROGRAM_COLOUR);
    assert_eq!(shown.saved[1], [0; 2], "a colour it never took");
}

#[test]
fn a_killed_incarnation_s_hourglass_is_let_go_what_it_showed_with_handed_back() {
    let mut glass = Hourglass::new();
    assert_eq!(glass.adopt(1), None, "a fresh one has nothing to give back");
    glass.on(0, 1);
    assert!(glass.turn(1).is_some());
    assert_eq!(glass.adopt(1), None, "its own incarnation");
    assert!(glass.adopt(0).is_some(), "the incarnation killed");
    assert_eq!(
        glass,
        Hourglass {
            generation: 0,
            ..Hourglass::new()
        }
    );
    assert_eq!(glass.frame(), None);
}

#[test]
fn a_frame_is_a_glass_its_sand_running_from_the_upper_bulb_to_the_lower() {
    let mut upper = Vec::new();
    let mut lower = Vec::new();
    for phase in 0..PHASES {
        let pixels = drawn(phase, None, 0);
        assert_eq!(pixels.len(), (WIDTH * HEIGHT) as usize);
        for y in TOP_BULB {
            let mirrored = BOTTOM_BULB.end - 1 - (y - TOP_BULB.start);
            for x in 0..WIDTH {
                assert_eq!(
                    at(&pixels, x, y) == GLASS,
                    at(&pixels, x, mirrored) == GLASS,
                    "the glass mirrored about its waist"
                );
            }
        }
        upper.push(count(&pixels, TOP_BULB, SAND));
        lower.push(count(&pixels, BOTTOM_BULB, SAND));
        assert_eq!(count(&pixels, FIGURES, GLASS), 0, "no figures");
        assert_eq!(
            at(&pixels, ACTIVE.0, ACTIVE.1),
            SAND,
            "sand runs at the waist"
        );
    }
    assert!(upper.windows(2).all(|pair| pair[0] > pair[1]), "{upper:?}");
    assert!(lower.windows(2).all(|pair| pair[0] < pair[1]), "{lower:?}");
}

#[test]
fn a_percentage_shows_beneath_in_the_system_font_and_the_lights_in_the_bars() {
    let five = drawn(0, Some(5), 0);
    let fifty = drawn(0, Some(50), 0);
    for (digit, pixels, left) in [('5', &five, 4), ('5', &fifty, 0), ('0', &fifty, 8)] {
        let glyph = tairix_vdu::font::glyph_of(digit).expect("a digit's glyph");
        for (row, bits) in (FIGURES.start..).zip(glyph) {
            for column in 0..8 {
                let lit = bits & (0x80 >> column) != 0;
                assert_eq!(at(pixels, left + column, row) == GLASS, lit, "{digit}");
            }
        }
    }
    let both = drawn(0, None, 3);
    let upper = drawn(0, None, 1);
    let corner = LIGHT.start;
    assert_eq!(at(&both, corner, TOP_BAR.start), SAND);
    assert_eq!(at(&both, corner, BOTTOM_BAR.start), SAND);
    assert_eq!(at(&upper, corner, TOP_BAR.start), SAND, "bit 0 the upper");
    assert_eq!(at(&upper, corner, BOTTOM_BAR.start), GLASS);
}
