//! Host tests of the ribbon of light: the ember it is toned in, the course of
//! its strands, how gently it moves, how soft it is, and that a frame
//! repaints exactly what changed.

use alloc::vec::Vec;

use tairix_wm::{Pixel, Rect, Region, Surface};

use super::{
    bezier, falloff_reach, monotone, tone_entry, tone_index, Band, Course, Fall, Light, Rows,
    EMBER, EMBER_TOP, PINCH, PIXELS_PER_SAMPLE, SPREAD, STRANDS, STRAND_COUNT, SWELL, TERM_CUT,
    TONE_LEN,
};

const WIDE: (u32, u32) = (640, 360);

/// A screensaver frame apart, in seconds.
const FRAME_S: f64 = 1.0 / 30.0;

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
    let exposure = -tairix_util::mathf::ln(1.0 - luma / EMBER_TOP);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a test exposure is a small number"
    )]
    let exposure = exposure as f32;
    exposure
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

#[test]
fn every_strand_runs_from_the_left_edge_to_the_right() {
    let light = Light::new(WIDE, 0.0).expect("a ribbon");
    let (first, last) = (light.columns[0], light.columns[light.columns.len() - 1]);
    for index in 0..STRAND_COUNT {
        assert!(
            first.light[index] > 0.0,
            "strand {index} is dark at the left"
        );
        assert!(
            last.light[index] > 0.0,
            "strand {index} is dark at the right"
        );
        assert!(first.reach[index] > 0.0 && last.reach[index] > 0.0);
        for placed in [light.placed[0], light.placed[light.placed.len() - 1]] {
            let at = placed.at[index];
            assert!(
                at > 0.0 && at < 360.0,
                "strand {index} leaves the screen at {at}"
            );
        }
    }
}

/// The band closes to nothing only where it twists, and the crossing strand
/// crosses the crest exactly once, on the right.
#[test]
fn the_band_pinches_once_and_one_strand_crosses_the_crest_on_the_right() {
    let crossing = match STRANDS[1].course {
        Course::Across(fraction, _) => fraction,
        Course::Crest | Course::Edge => panic!("the second strand runs across the band"),
    };
    let mut crossed = Vec::new();
    let mut was = bezier(&crossing, 0.0);
    for step in 1..=1000 {
        let u = f64::from(step) / 1000.0;
        let spread = (u - PINCH) * (u - PINCH) * bezier(&SPREAD, u);
        assert!(spread >= 0.0, "the band inverts at {u}");
        if (u - PINCH).abs() > 0.02 {
            assert!(spread > 0.0005, "the band closes at {u}");
        }
        let now = bezier(&crossing, u);
        if (now > 0.0) != (was > 0.0) {
            crossed.push(u);
        }
        was = now;
    }
    assert_eq!(crossed.len(), 1, "crossings at {crossed:?}");
    assert!((0.75..0.9).contains(&crossed[0]), "{crossed:?}");
}

/// However the ribbon moves, its inner strands keep their order across the
/// band and none of them leaves it.
#[test]
fn the_strands_keep_their_order_as_the_ribbon_moves() {
    let mut light = Light::new(WIDE, 0.0).expect("a ribbon");
    let mut damage = Region::new();
    for second in (0..600).step_by(7) {
        light.step(f64::from(second), &mut damage);
        for placed in &light.placed {
            let [crest, crossing, middle, lower, edge] = placed.at;
            assert!(crossing <= middle && middle <= lower && lower <= edge);
            assert!(crest <= edge + 0.01, "{crest} above {edge}");
        }
    }
}

/// The ribbon rises and falls slightly and slowly: its strands move less than
/// a pixel a frame at the height they are drawn, and the crest strays no more
/// than the swell from where it rests.
#[test]
fn the_ribbon_moves_slightly_and_slowly() {
    let size = (1920, 1080);
    let mut light = Light::new(size, 0.0).expect("a ribbon");
    let rest: Vec<f32> = light.columns.iter().map(|column| column.crest).collect();
    let swell: f64 = SWELL.iter().map(|wave| wave.amplitude).sum();
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a share of the screen's height is a small number"
    )]
    let reach = (swell * 1080.0) as f32 + 0.01;
    let mut damage = Region::new();
    let mut before: Vec<[f32; STRAND_COUNT]> =
        light.placed.iter().map(|placed| placed.at).collect();
    let mut frame = 0.0;
    for _ in 0..(60 * 30) {
        frame += FRAME_S;
        light.step(frame, &mut damage);
        for (placed, was) in light.placed.iter().zip(&before) {
            for (now, then) in placed.at.iter().zip(was) {
                assert!((now - then).abs() < 1.2, "{then} to {now} in a frame");
            }
        }
        for (placed, rested) in light.placed.iter().zip(&rest) {
            assert!(
                (placed.at[0] - rested).abs() <= reach,
                "{} from {rested}",
                placed.at[0]
            );
        }
        before = light.placed.iter().map(|placed| placed.at).collect();
    }
}

/// A frame repaints its strips and nothing else, and what it leaves on the
/// screen is exactly the frame painted whole.
#[test]
fn a_frame_repainted_in_strips_matches_the_frame_painted_whole() {
    let mut light = Light::new(WIDE, 3.0).expect("a ribbon");
    let mut surface = painted(&mut light);
    let mut damage = Region::new();
    for step in 1..=12 {
        let t = 3.0 + f64::from(step) * 0.7;
        damage.clear();
        light.step(t, &mut damage);
        light.paint_moved(&mut surface, |_, _| {});
        let whole = painted(&mut Light::new(WIDE, t).expect("a ribbon"));
        assert!(surface.pixels() == whole.pixels(), "frame at {t}s");
    }
}

/// Any part of the ribbon, however it is asked for, is the same pixels as the
/// ribbon painted whole.
#[test]
fn any_area_paints_the_same_pixels_as_the_whole() {
    let mut light = Light::new(WIDE, 11.0).expect("a ribbon");
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

/// Where the light does not reach the screen is black, and every pixel it
/// lights is inside the rows it reports reaching.
#[test]
fn the_light_is_black_beyond_the_rows_it_reaches() {
    let mut light = Light::new(WIDE, 5.0).expect("a ribbon");
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
    let mut light = Light::new((1280, 720), 8.0).expect("a ribbon");
    let surface = painted(&mut light);
    let mut steepest = 0;
    for y in 0..720 {
        for x in 0..1280 {
            let here = luma(surface.get(x, y).expect("inside"));
            if x + 1 < 1280 {
                steepest =
                    steepest.max((luma(surface.get(x + 1, y).expect("inside")) - here).abs());
            }
            if y + 1 < 720 {
                steepest =
                    steepest.max((luma(surface.get(x, y + 1).expect("inside")) - here).abs());
            }
        }
    }
    assert!(steepest <= 24, "a step of {steepest} between neighbours");
}

/// A frame repaints the rows the light reaches and reached, not the screen.
#[test]
fn a_frame_repaints_well_under_half_the_screen() {
    let size = (1920, 1080);
    let mut light = Light::new(size, 0.0).expect("a ribbon");
    let mut damage = Region::new();
    for frame in 1..=90 {
        damage.clear();
        light.step(f64::from(frame) * FRAME_S, &mut damage);
        let repainted: u64 = light
            .strips
            .iter()
            .map(|strip| u64::from(strip.width) * u64::from(strip.height))
            .sum();
        assert!(repainted * 2 < 1920 * 1080, "{repainted} pixels repainted");
    }
}

#[test]
fn an_empty_screen_has_no_ribbon_and_a_tiny_one_does() {
    assert!(Light::new((0, 100), 0.0).is_none());
    assert!(Light::new((100, 0), 0.0).is_none());
    let mut tiny = Light::new((1, 1), 0.0).expect("a ribbon of one pixel");
    let _ = painted(&mut tiny);
    let mut odd = Light::new((7, 5), 1.0).expect("an odd-sized ribbon");
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

/// A curtain is drawn only as far as its drape reaches, however deep the band
/// it hangs in runs on beneath it.
#[test]
fn a_curtain_ends_where_its_drape_fades_out() {
    let band = Band {
        top: 10.0,
        bottom: 700.0,
        soft: 4.0,
    };
    let (light, path) = (0.2_f32, 20.0);
    let drape = Fall::new(8.0, f64::from(light), TERM_CUT);
    let mut sums = alloc::vec![0.0_f32; 360];
    let rows = Rows {
        start: 0,
        count: sums.len(),
    };
    band.curtain((light, drape), (path, 2.0), rows, &mut sums);
    let ends = usize::try_from(Rows::first_from(path + drape.reach)).expect("a row");
    assert!(ends < sums.len(), "the band runs on past the drape");
    assert!(sums[ends - 1] > 0.0, "lit up to its reach");
    assert!(
        sums[ends..].iter().all(|sum| *sum <= 0.0),
        "drawn past its reach"
    );
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

#[test]
fn a_bezier_starts_and_ends_on_its_end_points() {
    let points = [0.8, 0.2, 0.9, 0.4];
    assert!((bezier(&points, 0.0) - 0.8).abs() < 1e-12);
    assert!((bezier(&points, 1.0) - 0.4).abs() < 1e-12);
    // Halfway, the cubic weighs its points 1 : 3 : 3 : 1.
    assert!((bezier(&points, 0.5) - (0.8 + 0.6 + 2.7 + 0.4) / 8.0).abs() < 1e-12);
}
