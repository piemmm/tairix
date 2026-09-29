//! Host tests of the ribbon of light: the ember it is toned in, the paths its
//! strands take and how freely and smoothly they move, how it keeps clear of
//! the clock, how soft it is, and that a frame repaints exactly what changed.

use alloc::vec::Vec;

use tairix_raster::DitherRow;
use tairix_wm::{Pixel, Rect, Region, Surface};

use crate::saver::{seconds, SAVER_FRAME_NS};

use super::{
    along, bernstein, curtain, falloff_reach, index_of, monotone, narrow, needed_pushes, pixel_of,
    reached, sample_across, tone_entry, tone_index, Fall, Light, Rows, CONTROLS, DITHER_LANES,
    EDGE_MARGIN, EMBER, EMBER_TOP, LANE_ONES, LEAST_GIVE, PIXELS_PER_SAMPLE, QUIET_LIGHT, STRANDS,
    STRAND_COUNT, TERM_CUT, TONE_LEN,
};

const WIDE: (u32, u32) = (640, 360);

/// A clock across the upper middle of the wide screen.
const CLOCK: Rect = Rect::new(190, 70, 260, 110);

/// A narrow screen, and a clock across most of its width.
const TALL: (u32, u32) = (360, 640);
const TALL_CLOCK: Rect = Rect::new(36, 100, 288, 170);

/// The screens the ribbon's paths are tested on, each with the clock's clear
/// space on it or none.
const LAYOUTS: [((u32, u32), Rect); 5] = [
    (WIDE, Rect::EMPTY),
    (WIDE, CLOCK),
    (TALL, TALL_CLOCK),
    ((1024, 768), Rect::new(270, 110, 484, 290)),
    ((1920, 1080), Rect::new(432, 156, 1056, 399)),
];

fn painted(light: &mut Light) -> Surface {
    let (width, height) = light.size;
    let mut surface = Surface::new(width, height).expect("a surface");
    light.paint(&mut surface, Rect::new(0, 0, width, height));
    surface
}

fn luma(pixel: Pixel) -> i32 {
    (299 * i32::from(pixel.r) + 587 * i32::from(pixel.g) + 114 * i32::from(pixel.b)) / 1000
}

/// A toning table entry's channels, rounded to whole levels.
fn channels(lanes: u64) -> [i32; 3] {
    let bytes = lanes.to_le_bytes();
    [0, 2, 4].map(|at| (i32::from(u16::from_le_bytes([bytes[at], bytes[at + 1]])) + 128) >> 8)
}

/// The exposure the tone curve lifts to `luma`.
fn exposure_for(luma: f64) -> f32 {
    narrow(-tairix_util::mathf::ln(1.0 - luma / EMBER_TOP))
}

/// The screen's pixel rows as a float, for comparing heights against it.
fn rows_of((_, height): (u32, u32)) -> f32 {
    narrow(f64::from(height))
}

/// Step `light` on `clock` through `seconds`, `every` seconds apart, handing
/// each frame to `look`.
fn run(light: &mut Light, clock: Rect, seconds: u32, every: f64, mut look: impl FnMut(&Light)) {
    let mut damage = Region::new();
    for step in 0..=seconds {
        light.step(f64::from(step) * every, clock, &mut damage);
        look(light);
    }
}

#[test]
fn the_ember_is_black_at_nothing_and_brightens_through_red_orange_and_gold() {
    assert_eq!(tone_entry(0), 0, "no light is black");
    let mut last = -1;
    for index in 0..TONE_LEN {
        let [r, g, b] = channels(tone_entry(index));
        let bright = 299 * r + 587 * g + 114 * b;
        assert!(
            bright >= last,
            "entry {index} is darker than the one before"
        );
        last = bright;
        // Red leads and blue trails all the way: an ember, never white-hot.
        assert!(r >= g && g >= b, "entry {index}: {r} {g} {b}");
    }
    let [r, g, b] = channels(tone_entry(TONE_LEN - 1));
    assert!(
        r >= 250 && g >= 245 && (180..=200).contains(&b),
        "{r} {g} {b}"
    );
}

/// The storyboard's measured colours are what the table tones to, each at
/// the exposure whose luma it stands at.
#[test]
fn the_ember_matches_the_storyboard_where_it_was_measured() {
    for (luma, colour) in EMBER.iter().skip(1).take(EMBER.len() - 2) {
        let got = channels(tone_entry(tone_index(exposure_for(*luma))));
        for (channel, (had, want)) in got.iter().zip(colour).enumerate() {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a measured channel is a small whole number"
            )]
            let want = *want as i32;
            assert!(
                (had - want).abs() <= 3,
                "luma {luma}: channel {channel} is {had}, the storyboard {want}"
            );
        }
    }
}

/// Every strand is lit from the left edge to the right, and wherever the
/// ribbon roams each path meets both edges on the screen: inside its margins
/// with no clock to pass, and on the screen however one bends it.
#[test]
fn every_strand_runs_from_the_left_edge_to_the_right() {
    for (size, clock) in LAYOUTS {
        let mut light = Light::new(size, clock, 0.0).expect("a ribbon");
        let (first, last) = (light.columns[0], light.columns[light.columns.len() - 1]);
        for strand in 0..STRAND_COUNT {
            assert!(
                first.light[strand] > 0.0 && last.light[strand] > 0.0,
                "{size:?}: strand {strand} is dark at an edge"
            );
            assert!(first.reach[strand] > 0.0 && last.reach[strand] > 0.0);
        }
        let tall = rows_of(size);
        let inset = if clock.is_empty() {
            narrow(EDGE_MARGIN) * tall - 0.01
        } else {
            0.0
        };
        run(&mut light, clock, 240, 1.0, |light| {
            for (sample, placed) in light.placed.iter().enumerate() {
                for (strand, at) in placed.at.iter().enumerate() {
                    assert!(
                        *at > inset && *at < tall - inset,
                        "{size:?}: strand {strand}, sample {sample} strays to {at}"
                    );
                }
            }
        });
    }
}

/// Every path is one Bézier curve across the width, bent beneath the clock
/// or not: its height at every sample column is de Casteljau's construction
/// on its control points.
#[test]
fn every_strand_is_one_bezier_curve_across_the_width() {
    let mut bent = false;
    for (size, clock) in LAYOUTS {
        let mut light = Light::new(size, clock, 0.0).expect("a ribbon");
        let tall = f64::from(size.1);
        run(&mut light, clock, 80, 3.0, |light| {
            let free = light.roam.paths(light.time, tall);
            let pushed = free
                .iter()
                .flatten()
                .zip(light.paths.iter().flatten())
                .any(|(free, placed)| (free - placed).abs() > 1e-9);
            assert!(!pushed || !clock.is_empty(), "{size:?}: bent with no clock");
            bent |= pushed;
            for (sample, placed) in light.placed.iter().enumerate() {
                let across = sample_across(u32::try_from(sample).expect("a column"), size.0);
                for (strand, path) in light.paths.iter().enumerate() {
                    let curve = de_casteljau(path, along(across));
                    assert!(
                        (f64::from(placed.at[strand]) - curve).abs() < 0.01,
                        "{size:?}: strand {strand} leaves its curve at sample {sample}"
                    );
                }
            }
        });
    }
    assert!(bent, "no path was bent beneath a clock");
}

/// No path has a kink or a corner: its curvature, in screen heights per screen
/// width squared so that every screen is held alike, stays that of a gentle
/// curve as it roams and as the clock bends it. A corner of even two degrees
/// between neighbouring sample columns would read above twenty.
#[test]
fn no_strand_has_a_kink() {
    for (size, clock) in LAYOUTS {
        let mut light = Light::new(size, clock, 0.0).expect("a ribbon");
        let step = f64::from(PIXELS_PER_SAMPLE) / f64::from(size.0);
        let per_turn = 1.0 / (f64::from(size.1) * step * step);
        run(&mut light, clock, 600, 0.4, |light| {
            for window in light.placed.windows(3) {
                for strand in 0..STRAND_COUNT {
                    let [before, here, after] = [0, 1, 2].map(|at| window[at].at[strand]);
                    let curvature = f64::from((after - 2.0 * here + before).abs()) * per_turn;
                    assert!(
                        curvature < 20.0,
                        "{size:?}: strand {strand} bends at {curvature}"
                    );
                }
            }
        });
    }
}

/// No strand moves as a copy of another: over four minutes every two strands
/// cross each other somewhere, and the gap between them keeps changing at the
/// edges and in the middle alike.
#[test]
fn the_strands_move_independently() {
    let mut light = Light::new(WIDE, Rect::EMPTY, 0.0).expect("a ribbon");
    let watched = [0, light.placed.len() / 2, light.placed.len() - 1];
    let mut crossed = [[false; STRAND_COUNT]; STRAND_COUNT];
    let mut gaps = [[[(f32::MAX, f32::MIN); 3]; STRAND_COUNT]; STRAND_COUNT];
    run(&mut light, Rect::EMPTY, 480, 0.5, |light| {
        for placed in &light.placed {
            for (upper, lower) in pairs() {
                crossed[upper][lower] |= placed.at[lower] < placed.at[upper];
            }
        }
        for (watch, sample) in watched.iter().enumerate() {
            let at = light.placed[*sample].at;
            for (upper, lower) in pairs() {
                let (least, most) = &mut gaps[upper][lower][watch];
                *least = least.min(at[lower] - at[upper]);
                *most = most.max(at[lower] - at[upper]);
            }
        }
    });
    let tall = rows_of(WIDE);
    for (upper, lower) in pairs() {
        assert!(
            crossed[upper][lower],
            "strands {upper} and {lower} never cross"
        );
        for (least, most) in gaps[upper][lower] {
            assert!(
                most - least > 0.06 * tall,
                "strands {upper} and {lower} keep within {} rows of one gap",
                most - least
            );
        }
    }
}

/// Every two strands, the one above first.
fn pairs() -> impl Iterator<Item = (usize, usize)> {
    (0..STRAND_COUNT).flat_map(|upper| (upper + 1..STRAND_COUNT).map(move |lower| (upper, lower)))
}

/// The ribbon ranges through the screen rather than trembling about one pose,
/// widest at its edges, while its strands still move smoothly from frame to
/// frame.
#[test]
fn the_ribbon_roams_freely_and_smoothly() {
    let size = (1920, 1080);
    let mut light = Light::new(size, Rect::EMPTY, 0.0).expect("a ribbon");
    let mut before: Vec<[f32; STRAND_COUNT]> =
        light.placed.iter().map(|placed| placed.at).collect();
    let frame_s = seconds(SAVER_FRAME_NS);
    run(&mut light, Rect::EMPTY, 60 * 30, frame_s, |light| {
        for (placed, was) in light.placed.iter().zip(&before) {
            for (now, then) in placed.at.iter().zip(was) {
                assert!((now - then).abs() < 2.5, "{then} to {now} in a frame");
            }
        }
        before = light.placed.iter().map(|placed| placed.at).collect();
    });
    let watched = [0, light.placed.len() / 2, light.placed.len() - 1];
    let mut ranges = [(f32::MAX, f32::MIN); 3];
    run(&mut light, Rect::EMPTY, 240, 1.0, |light| {
        for (range, sample) in ranges.iter_mut().zip(watched) {
            let brightest = light.placed[sample].at[0];
            *range = (range.0.min(brightest), range.1.max(brightest));
        }
    });
    let tall = rows_of(size);
    for ((least, most), share) in ranges.into_iter().zip([0.35, 0.3, 0.35]) {
        let roamed = most - least;
        assert!(
            roamed > share * tall,
            "the brightest strand roamed only {roamed} rows"
        );
    }
}

#[test]
fn the_ribbon_never_reaches_the_clocks_clear_space() {
    let black = Pixel {
        r: 0,
        g: 0,
        b: 0,
        a: u8::MAX,
    };
    for (size, clock) in LAYOUTS.into_iter().filter(|(_, clock)| !clock.is_empty()) {
        let (Ok(left), Ok(top), Ok(bottom)) = (
            u32::try_from(clock.left()),
            u32::try_from(clock.top()),
            u32::try_from(clock.bottom()),
        ) else {
            panic!("the clock is on the screen");
        };
        let right = left + clock.width;
        let mut light = Light::new(size, clock, 0.0).expect("a ribbon");
        let mut damage = Region::new();
        for second in (0..=240).step_by(3) {
            light.step(f64::from(second), clock, &mut damage);
            for (sample, placed) in light.placed.iter().enumerate() {
                let x = pixel_of(u32::try_from(sample).expect("a sample column"));
                if (left..right).contains(&x) {
                    assert!(
                        placed.lit.0 >= bottom,
                        "{size:?}: column {x} reaches {:?} through {clock:?}",
                        placed.lit
                    );
                }
            }
            if second % 30 == 0 {
                let surface = painted(&mut light);
                for y in top..bottom {
                    for x in left..right {
                        assert_eq!(
                            surface.get(x, y),
                            Some(black),
                            "{size:?}: light at ({x}, {y})"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_changed_clock_block_redirects_the_same_animation_frame() {
    let first = Rect::new(230, 70, 180, 110);
    let changed = CLOCK;
    let bottom = u32::try_from(changed.bottom()).expect("the clock is on screen");
    let mut light = Light::new(WIDE, first, 37.0).expect("a ribbon");
    let mut damage = Region::new();
    assert!(light.step(37.0, changed, &mut damage));
    assert!(!damage.is_empty());
    for (sample, placed) in light.placed.iter().enumerate() {
        let sample = u32::try_from(sample).expect("a sample column");
        let x = i32::try_from(pixel_of(sample)).expect("a screen coordinate");
        if x >= changed.left() && x < changed.right() {
            assert!(
                placed.lit.0 >= bottom,
                "column {x} reaches through the clock"
            );
        }
    }
    damage.clear();
    assert!(!light.step(37.0, changed, &mut damage));
    assert!(damage.is_empty());
    assert!(light.strips.is_empty());
}

/// A strand whose light would reach the clock is pushed down just clear of
/// it, however far into the text it ran, and a strand already clear is left
/// exactly where it was.
#[test]
fn only_a_strand_whose_light_would_reach_the_clock_is_pushed() {
    let mut light = Light::new(WIDE, CLOCK, 0.0).expect("a ribbon");
    let tall = f64::from(WIDE.1);
    let (reaching, clear) = (0.25 * tall, 0.92 * tall);
    light.paths =
        core::array::from_fn(|strand| [if strand == 0 { reaching } else { clear }; CONTROLS]);
    light.trace();
    let clearance = light.roam.clearance.clone().expect("the clock's clearance");
    let pushes = needed_pushes(
        &light.columns,
        &light.placed,
        &light.yields,
        &clearance,
        tall,
    );
    assert!(pushes[0] > 0.0, "the strand in the text is pushed");
    assert!(pushes[1..].iter().all(|push| *push == 0.0), "{pushes:?}");
    for (path, push) in light.paths.iter_mut().zip(pushes) {
        for (point, shadow) in path.iter_mut().zip(clearance.shadow) {
            *point += push * shadow;
        }
    }
    light.trace();
    light.shape();
    let bottom = u32::try_from(CLOCK.bottom()).expect("the clock is on screen");
    for index in clearance.columns.clone() {
        let (column, placed) = (&light.columns[index], &light.placed[index]);
        assert!(
            reached(column, placed, WIDE.1).0 >= bottom,
            "sample {index}"
        );
        assert!(
            (f64::from(placed.at[1]) - clear).abs() < 1e-3,
            "an unpushed strand moved"
        );
    }
}

/// Every column beneath the clock moves clear of it under a push however its
/// paths steepen: a shadow too narrow for that, as a clock on a slender screen
/// casts, is widened until it does.
#[test]
fn every_column_beneath_the_clock_gives_to_a_push() {
    let slender = ((120, 960), Rect::new(40, 100, 40, 200));
    for (size, clock) in LAYOUTS.into_iter().chain([slender]) {
        if clock.is_empty() {
            continue;
        }
        let light = Light::new(size, clock, 0.0).expect("a ribbon");
        let tall = f64::from(size.1);
        let clearance = light
            .roam
            .clearance
            .as_ref()
            .expect("the clock's clearance");
        for index in clearance.columns.clone() {
            let give = light.give(index, tall);
            assert!(
                give > LEAST_GIVE - 1e-9,
                "{size:?}: sample {index} gives {give}"
            );
        }
    }
    // Unwidened, the slender clock's shadow falls to nothing short of the
    // edges; widened toward an even push, no control point is left out.
    let light = Light::new(slender.0, slender.1, 0.0).expect("a ribbon");
    let shadow = light.roam.clearance.as_ref().expect("a clearance").shadow;
    assert!(
        shadow.iter().all(|weight| *weight > 0.0),
        "unwidened: {shadow:?}"
    );
}

#[test]
fn every_strands_bright_point_travels_along_it() {
    let mut light = Light::new(WIDE, Rect::EMPTY, 0.0).expect("a ribbon");
    let mut damage = Region::new();
    let mut ranges = [(usize::MAX, 0); STRAND_COUNT];
    for second in 0..=60 {
        light.step(f64::from(second), Rect::EMPTY, &mut damage);
        for strand in 0..STRAND_COUNT {
            let mut hottest = (0, f64::MIN);
            for (index, (column, placed)) in light.columns.iter().zip(&light.placed).enumerate() {
                let flare =
                    f64::from(placed.light[strand]) - f64::from(column.light[strand]) * QUIET_LIGHT;
                if flare > hottest.1 {
                    hottest = (index, flare);
                }
            }
            assert!(hottest.1 > STRANDS[strand].flare * 0.99);
            ranges[strand].0 = ranges[strand].0.min(hottest.0);
            ranges[strand].1 = ranges[strand].1.max(hottest.0);
        }
    }
    for (strand, (left, right)) in ranges.into_iter().enumerate() {
        assert!(
            (right - left) * 4 > light.columns.len() * 3,
            "strand {strand}'s bright point stayed in {left}..={right}"
        );
    }
}

/// A frame repaints its strips and nothing else, and what it leaves on the
/// screen is exactly the frame painted whole.
#[test]
fn a_frame_repainted_in_strips_matches_the_frame_painted_whole() {
    let mut light = Light::new(WIDE, Rect::EMPTY, 3.0).expect("a ribbon");
    let mut surface = painted(&mut light);
    let mut damage = Region::new();
    for step in 1..=12 {
        let t = 3.0 + f64::from(step) * 0.7;
        damage.clear();
        light.step(t, Rect::EMPTY, &mut damage);
        light.paint_moved(&mut surface, |_, _| {});
        let whole = painted(&mut Light::new(WIDE, Rect::EMPTY, t).expect("a ribbon"));
        assert!(surface.pixels() == whole.pixels(), "frame at {t}s");
    }
}

/// Any part of the ribbon, however it is asked for, is the same pixels as the
/// ribbon painted whole.
#[test]
fn any_area_paints_the_same_pixels_as_the_whole() {
    let mut light = Light::new(WIDE, Rect::EMPTY, 11.0).expect("a ribbon");
    let whole = painted(&mut light);
    for area in [
        Rect::new(0, 0, 640, 360),
        Rect::new(301, 187, 77, 91),
        Rect::new(1, 1, 1, 359),
        Rect::new(590, 0, 50, 360),
        Rect::new(0, 250, 640, 3),
        Rect::new(333, 40, 20, 20),
    ] {
        let mut part = Surface::new(640, 360).expect("a surface");
        light.paint(&mut part, area);
        let (Ok(left), Ok(top)) = (u32::try_from(area.left()), u32::try_from(area.top())) else {
            panic!("a positive area");
        };
        for y in top..top + area.height {
            for x in left..left + area.width {
                assert_eq!(part.get(x, y), whole.get(x, y), "({x}, {y}) of {area:?}");
            }
        }
    }
}

/// An area reaching off the screen paints the part of it on the screen, as
/// the ribbon painted whole has it; it once painted nothing at all.
#[test]
fn an_area_reaching_off_the_screen_paints_its_part_on_it() {
    let mut light = Light::new(WIDE, Rect::EMPTY, 11.0).expect("a ribbon");
    let whole = painted(&mut light);
    let blank = Surface::new(640, 360).expect("a surface");
    for (area, on) in [
        (Rect::new(-40, 0, 120, 360), (0..80, 0..360)),
        (Rect::new(100, -20, 60, 400), (100..160, 0..360)),
    ] {
        let mut part = Surface::new(640, 360).expect("a surface");
        light.paint(&mut part, area);
        assert!(part.pixels() != blank.pixels(), "{area:?} painted nothing");
        for y in 0..360 {
            for x in 0..640 {
                let within = on.0.contains(&x) && on.1.contains(&y);
                let expected = if within { &whole } else { &blank };
                assert_eq!(part.get(x, y), expected.get(x, y), "({x}, {y}) of {area:?}");
            }
        }
    }
}

/// A strip repaints the pixel before its first sample, which blends it, and
/// stops at the next strip's first sample, which is that strip's alone.
#[test]
fn neighbouring_strips_share_only_the_pixel_between_them() {
    let mut light = Light::new(WIDE, Rect::EMPTY, 3.0).expect("a ribbon");
    let mut damage = Region::new();
    light.step(3.7, Rect::EMPTY, &mut damage);
    let mut strips = light.strips.clone();
    strips.sort_by_key(Rect::left);
    assert!(strips.len() > 1, "the frame moved more than one strip");
    for pair in strips.windows(2) {
        assert!(
            pair[0].right() <= pair[1].left() + 1,
            "{:?} repaints past {:?}'s first pixel",
            pair[0],
            pair[1]
        );
    }
}

/// The same frames repainted in strips at a size no strip divides, so the
/// last strip is partial.
#[test]
fn a_frame_repainted_in_strips_matches_the_whole_at_an_odd_size() {
    let size = (641, 361);
    let mut light = Light::new(size, Rect::EMPTY, 3.0).expect("a ribbon");
    let mut surface = painted(&mut light);
    let mut damage = Region::new();
    for step in 1..=12 {
        let t = 3.0 + f64::from(step) * 0.7;
        damage.clear();
        light.step(t, Rect::EMPTY, &mut damage);
        light.paint_moved(&mut surface, |_, _| {});
        let whole = painted(&mut Light::new(size, Rect::EMPTY, t).expect("a ribbon"));
        assert!(surface.pixels() == whole.pixels(), "frame at {t}s");
    }
}

/// The dither is looked up from the pattern's eight rows, which must be every
/// row it has, each bias spread across the lanes.
#[test]
fn the_dither_table_holds_every_row_of_the_pattern() {
    for row in 0..64 {
        let dither = DitherRow::at(row);
        for column in 0..8 {
            assert_eq!(
                DITHER_LANES[index_of(row & 7)][index_of(column)],
                u64::from(dither.bias(column)) * LANE_ONES,
                "row {row}, column {column}"
            );
        }
    }
}

/// Where the light does not reach the screen is black, and every pixel it
/// lights is inside the rows it reports reaching.
#[test]
fn the_light_is_black_beyond_the_rows_it_reaches() {
    let mut light = Light::new(WIDE, Rect::EMPTY, 5.0).expect("a ribbon");
    let surface = painted(&mut light);
    let black = Pixel {
        r: 0,
        g: 0,
        b: 0,
        a: u8::MAX,
    };
    for x in 0..640 {
        let sample = usize::try_from(x / PIXELS_PER_SAMPLE).expect("a column");
        let lit = light.placed[sample].lit;
        for y in 0..360 {
            let reached = (lit.0..lit.1).contains(&y)
                || [
                    sample.saturating_sub(1),
                    (sample + 1).min(light.placed.len() - 1),
                ]
                .iter()
                .any(|near| (light.placed[*near].lit.0..light.placed[*near].lit.1).contains(&y));
            if !reached {
                assert_eq!(surface.get(x, y), Some(black), "({x}, {y})");
            }
        }
        assert_eq!(surface.get(x, 0), Some(black), "the sky above is dark");
    }
}

/// No edge of the ribbon is hard: neighbouring pixels differ by little, down
/// every column and along every row.
#[test]
fn the_light_is_soft_everywhere() {
    let mut light = Light::new((1280, 720), Rect::new(380, 150, 520, 220), 8.0).expect("a ribbon");
    let surface = painted(&mut light);
    let mut steepest = (0, (0, 0), (0, 0));
    for y in 0..720 {
        for x in 0..1280 {
            let here = luma(surface.get(x, y).expect("inside"));
            if x + 1 < 1280 {
                let change = (luma(surface.get(x + 1, y).expect("inside")) - here).abs();
                if change > steepest.0 {
                    steepest = (change, (x, y), (x + 1, y));
                }
            }
            if y + 1 < 720 {
                let change = (luma(surface.get(x, y + 1).expect("inside")) - here).abs();
                if change > steepest.0 {
                    steepest = (change, (x, y), (x, y + 1));
                }
            }
        }
    }
    let (step, from, to) = steepest;
    assert!(step <= 24, "a step of {step} from {from:?} to {to:?}");
}

/// A frame repaints the rows the light reaches and reached, which is less
/// than half the screen, never the whole of it.
#[test]
fn a_frame_repaints_under_half_the_screen() {
    for (size, clock) in LAYOUTS {
        let mut light = Light::new(size, clock, 0.0).expect("a ribbon");
        let screen = u64::from(size.0) * u64::from(size.1);
        let mut damage = Region::new();
        let frame_s = seconds(SAVER_FRAME_NS);
        for frame in 1..=90 {
            damage.clear();
            light.step(f64::from(frame) * frame_s, clock, &mut damage);
            let repainted: u64 = light
                .strips
                .iter()
                .map(|strip| u64::from(strip.width) * u64::from(strip.height))
                .sum();
            assert!(
                repainted * 2 < screen,
                "{size:?}: {repainted} pixels repainted"
            );
        }
    }
}

#[test]
fn an_empty_screen_has_no_ribbon_and_a_tiny_one_does() {
    assert!(Light::new((0, 100), Rect::EMPTY, 0.0).is_none());
    assert!(Light::new((100, 0), Rect::EMPTY, 0.0).is_none());
    let mut tiny = Light::new((1, 1), Rect::EMPTY, 0.0).expect("a ribbon of one pixel");
    let _ = painted(&mut tiny);
    let mut odd = Light::new((7, 5), Rect::EMPTY, 1.0).expect("an odd-sized ribbon");
    let _ = painted(&mut odd);
}

/// A falloff drawn to its reach has fallen below the cut it was drawn for.
#[test]
fn a_falloff_ends_below_its_cut() {
    for light in [0.001, 0.05, 0.3, 1.35, 4.0] {
        let x = falloff_reach(light, TERM_CUT);
        let left = light * (1.0 + x) * tairix_util::mathf::exp(-x);
        assert!(left <= TERM_CUT, "{light}: {left} left at {x}");
        let before = light * x * tairix_util::mathf::exp(1.0 - x);
        assert!(
            before > TERM_CUT * 0.5,
            "{light}: reach {x} is longer than it needs"
        );
    }
    assert!(falloff_reach(TERM_CUT / 2.0, TERM_CUT) <= 0.0);
}

/// A curtain is drawn as far as its drape reaches and no further.
#[test]
fn a_curtain_ends_where_its_drape_fades_out() {
    let (light, path) = (0.2_f32, 20.0);
    let drape = Fall::new(8.0, f64::from(light), TERM_CUT);
    let mut sums = alloc::vec![0.0_f32; 360];
    let rows = Rows {
        start: 0,
        count: sums.len(),
    };
    curtain((light, drape), (path, 2.0), rows, &mut sums);
    let ends = usize::try_from(Rows::first_from(path + drape.reach)).expect("a row");
    assert!(ends < sums.len(), "the rows run on past the drape");
    assert!(sums[ends - 1] > 0.0, "lit up to its reach");
    assert!(
        sums[ends..].iter().all(|sum| *sum <= 0.0),
        "drawn past its reach"
    );
}

/// Every strand hangs a curtain the whole width across, each fades to
/// nothing, and two that overlap add.
#[test]
fn every_strands_curtain_fades_and_overlaps_additively() {
    let light = Light::new(WIDE, Rect::EMPTY, 0.0).expect("a ribbon");
    for (sample, column) in light.columns.iter().enumerate() {
        for strand in 0..STRAND_COUNT {
            assert!(
                column.curtain[strand] > 0.0 && column.drape[strand].reach > 0.0,
                "strand {strand} hangs no curtain at sample {sample}"
            );
        }
    }
    let (light, path) = (0.2_f32, 20.0);
    let drape = Fall::new(24.0, f64::from(light), TERM_CUT);
    let rows = Rows {
        start: 0,
        count: 360,
    };
    let mut once = [0.0_f32; 360];
    let mut overlap = [0.0_f32; 360];
    curtain((light, drape), (path, 2.0), rows, &mut once);
    curtain((light, drape), (path, 2.0), rows, &mut overlap);
    curtain((light, drape), (path, 2.0), rows, &mut overlap);
    for (single, doubled) in once.iter().zip(overlap) {
        assert!((doubled - 2.0 * single).abs() < f32::EPSILON);
    }
    let peak = once.iter().copied().fold(0.0_f32, f32::max);
    let last = once
        .iter()
        .rposition(|sample| *sample > 0.0)
        .expect("a curtain");
    assert!(once[last] < peak / 100.0);
    assert!(once[last + 1..].iter().all(|sample| *sample == 0.0));
}

/// The station curve passes through every station and never strays past the
/// stations either side, so light never goes below nothing.
#[test]
fn a_station_curve_meets_its_stations_and_never_overshoots() {
    let stations = [(0.0, 0.0), (0.2, 0.5), (0.3, 0.1), (0.6, 0.1), (1.0, 2.0)];
    for (at, value) in stations {
        assert!((monotone(&stations, at) - value).abs() < 1e-12, "{at}");
    }
    for pair in stations.windows(2) {
        let (low, high) = (pair[0].1.min(pair[1].1), pair[0].1.max(pair[1].1));
        for step in 0..=50 {
            let at = pair[0].0 + (pair[1].0 - pair[0].0) * f64::from(step) / 50.0;
            let value = monotone(&stations, at);
            assert!(
                value >= low - 1e-12 && value <= high + 1e-12,
                "{at}: {value}"
            );
        }
    }
    assert!((monotone(&stations, -1.0)).abs() < 1e-12);
    assert!((monotone(&stations, 2.0) - 2.0).abs() < 1e-12);
}

/// De Casteljau's construction of the Bézier curve on `points` at `u`: the
/// reference the Bernstein weights are held to.
fn de_casteljau(points: &[f64; CONTROLS], u: f64) -> f64 {
    let mut values = *points;
    for level in (1..CONTROLS).rev() {
        for index in 0..level {
            values[index] += (values[index + 1] - values[index]) * u;
        }
    }
    values[0]
}

/// The Bernstein weights sum to one, take each end point wholly at its end,
/// and weigh the control points exactly as de Casteljau's construction does.
#[test]
fn the_bernstein_weights_are_de_casteljaus() {
    let points: [f64; CONTROLS] =
        core::array::from_fn(|k| f64::from(u32::try_from(k * 37 % 17).expect("small")) - 8.0);
    for step in 0..=64 {
        let u = f64::from(step) / 64.0;
        let weights = bernstein(u);
        assert!((weights.iter().sum::<f64>() - 1.0).abs() < 1e-12, "{u}");
        let weighed: f64 = weights
            .iter()
            .zip(&points)
            .map(|(weight, point)| weight * point)
            .sum();
        assert!((weighed - de_casteljau(&points, u)).abs() < 1e-9, "{u}");
    }
    assert!((bernstein(0.0)[0] - 1.0).abs() < 1e-12);
    assert!((bernstein(1.0)[CONTROLS - 1] - 1.0).abs() < 1e-12);
}
