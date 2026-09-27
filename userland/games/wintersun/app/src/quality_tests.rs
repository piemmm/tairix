//! Every knob's settings run finest first, and the ladder sheds the knobs
//! that cost frame time in the stated order, one at a time, every step
//! reachable and reversible.

use super::*;
use tairix_wintersun_art::material::MAX_OCTAVES;

/// A render scale as a comparable fraction of a thousand.
fn permille(scale: RenderScale) -> u64 {
    u64::from(scale.numerator()) * 1000 / u64::from(scale.denominator())
}

#[test]
fn every_knob_lists_its_settings_finest_first() {
    let shifts: alloc::vec::Vec<u32> = Lighting::ALL.iter().map(|l| l.shift()).collect();
    assert_eq!(shifts, alloc::vec![1, 2, 3], "the light buffer coarsens");
    let scales: alloc::vec::Vec<u64> = Resolution::ALL
        .iter()
        .map(|r| permille(r.scale()))
        .collect();
    assert!(
        scales.windows(2).all(|pair| pair[0] > pair[1]),
        "{scales:?}"
    );
    assert!(Resolution::Full.scale().is_native());
    assert_eq!(Detail::FINEST.ground.octaves(), MAX_OCTAVES);
    assert_eq!(Detail::PLAINEST.ground.octaves(), 0);
}

#[test]
fn a_full_ladder_draws_the_finest_detail() {
    let full = Ladder::FULL;
    assert_eq!(full.step(), 0);
    assert_eq!(full.rung(), Rung::Full);
    assert_eq!(full.detail(), Detail::FINEST);
    assert_eq!(full.restore(), None, "there is nothing above full");
    assert_eq!(Ladder::default(), full);
}

#[test]
fn the_bottom_of_the_ladder_is_the_plainest_detail_on_the_finest_ground() {
    assert_eq!(
        Ladder::new(Ladder::MAX_STEP).detail(),
        Detail {
            ground: Detail::FINEST.ground,
            ..Detail::PLAINEST
        }
    );
}

#[test]
fn the_ladder_never_touches_the_ground_texture() {
    // Its octaves cost a synthesis, not a frame: shedding one frees nothing.
    for step in 0..=Ladder::MAX_STEP {
        assert_eq!(Ladder::new(step).detail().ground, Detail::FINEST.ground);
    }
}

#[test]
fn shedding_walks_every_step_once_and_stops_at_the_bottom() {
    let mut ladder = Ladder::FULL;
    let mut steps = 0u8;
    while let Some(next) = ladder.shed() {
        assert_eq!(next.step(), ladder.step() + 1);
        ladder = next;
        steps += 1;
        assert!(steps <= Ladder::MAX_STEP, "the ladder did not terminate");
    }
    assert_eq!(steps, Ladder::MAX_STEP);
    assert_eq!(usize::from(Ladder::MAX_STEP) + 1, Ladder::STEPS);
    assert_eq!(
        ladder.shed(),
        None,
        "the bottom rung reports it has nothing left"
    );
}

#[test]
fn each_step_turns_exactly_one_knob() {
    let mut ladder = Ladder::FULL;
    while let Some(next) = ladder.shed() {
        let (a, b) = (ladder.detail(), next.detail());
        let differing = usize::from(a.lighting != b.lighting)
            + usize::from(a.shadows != b.shadows)
            + usize::from(a.ground != b.ground)
            + usize::from(a.resolution != b.resolution);
        assert_eq!(
            differing,
            1,
            "step {} -> {} turned {differing} knobs: {a:?} then {b:?}",
            ladder.step(),
            next.step()
        );
        ladder = next;
    }
}

#[test]
fn the_rungs_give_way_in_the_stated_order() {
    let mut seen = alloc::vec::Vec::new();
    let mut ladder = Ladder::FULL;
    while let Some(next) = ladder.shed() {
        ladder = next;
        let rung = ladder.rung();
        if seen.last() != Some(&rung) {
            seen.push(rung);
        }
    }
    assert_eq!(
        seen,
        alloc::vec![
            Rung::LightResolution,
            Rung::ShadowSoftness,
            Rung::RenderScale
        ],
        "light buffer, shadow softness, render scale"
    );
}

#[test]
fn a_rung_is_fully_shed_before_the_next_is_touched() {
    // The first render-scale notch may only appear once the light is at its
    // coarsest and the ground's relief flat.
    let mut ladder = Ladder::FULL;
    while ladder.detail().resolution == Resolution::Full {
        match ladder.shed() {
            Some(next) => ladder = next,
            None => unreachable!("the render-scale rung is reachable"),
        }
    }
    assert_eq!(ladder.detail().lighting, Lighting::Coarse);
    assert_eq!(ladder.detail().shadows, Shadows::Flat);
}

#[test]
fn shedding_and_restoring_are_inverses() {
    let mut ladder = Ladder::FULL;
    while let Some(next) = ladder.shed() {
        assert_eq!(next.restore(), Some(ladder), "restore did not undo shed");
        assert_eq!(next.restore().map(Ladder::detail), Some(ladder.detail()));
        ladder = next;
    }
}

#[test]
fn every_knob_is_monotone_down_the_ladder() {
    let mut ladder = Ladder::FULL;
    while let Some(next) = ladder.shed() {
        let (a, b) = (ladder.detail(), next.detail());
        assert!(
            b.lighting >= a.lighting,
            "the light got finer while shedding"
        );
        assert!(
            b.shadows >= a.shadows,
            "the shadows softened while shedding"
        );
        assert!(
            permille(b.resolution.scale()) <= permille(a.resolution.scale()),
            "the render target grew while shedding"
        );
        ladder = next;
    }
}

#[test]
fn a_step_beyond_the_bottom_clamps_rather_than_wrapping() {
    assert_eq!(Ladder::new(u8::MAX).step(), Ladder::MAX_STEP);
    assert_eq!(Ladder::new(Ladder::MAX_STEP), Ladder::new(u8::MAX));
}

#[test]
fn render_scale_never_scales_a_length_to_nothing() {
    for resolution in Resolution::ALL {
        let scale = resolution.scale();
        assert!(
            scale.apply(1) >= 1,
            "a one-pixel window lost its only pixel"
        );
        assert!(scale.apply(0) >= 1, "a zero length must still floor at one");
        assert!(scale.apply(1280) <= 1280);
        assert!(
            scale.apply(u32::MAX) > 0,
            "the widest window still has pixels"
        );
    }
    assert_eq!(RenderScale::ONE.apply(1280), 1280);
}

#[test]
fn a_contact_shadow_hardens_and_never_goes() {
    let shades: alloc::vec::Vec<Shade> = Shadows::ALL.iter().map(|s| s.shade()).collect();
    assert_eq!(shades, alloc::vec![Shade::Soft, Shade::Hard, Shade::Hard]);
}

#[test]
fn the_relief_narrows_then_flattens() {
    let reliefs: alloc::vec::Vec<Relief> = Shadows::ALL.iter().map(|s| s.relief()).collect();
    assert_eq!(
        reliefs,
        alloc::vec![Relief::Wide, Relief::Narrow, Relief::Flat]
    );
}

#[test]
fn every_render_scale_keeps_every_zoom_whole() {
    let mut zooms = alloc::vec![Zoom::NEAREST];
    while let Some(next) = zooms.last().and_then(|zoom| zoom.further()) {
        zooms.push(next);
    }
    for cap in CAPS {
        for resolution in Resolution::ALL {
            let scale = cap.of(resolution.scale());
            for zoom in &zooms {
                let base = zoom.sub_units_per_pixel();
                let whole = scale.step(base).expect("a whole step");
                assert_eq!(
                    i64::from(whole) * i64::from(scale.numerator()),
                    i64::from(base) * i64::from(scale.denominator()),
                    "{scale:?} at {zoom:?} is not the zoom's own step widened"
                );
            }
        }
    }
}

#[test]
fn a_fraction_that_splits_a_sub_unit_is_refused() {
    let three_quarters = RenderScale {
        numerator: 3,
        denominator: 4,
    };
    assert_eq!(
        three_quarters.step(8),
        None,
        "eight sub-units widened by a third split one"
    );
    assert_eq!(three_quarters.step(12), Some(16));
    assert_eq!(RenderScale::ONE.step(8), Some(8));
}

/// The floor falls where the smallest figure a record describes is drawn at
/// the art harness's own floor, and a view the player's zoom has already
/// drawn smaller than that leaves the render scale alone.
#[test]
fn the_floor_holds_the_smallest_figure_at_the_readable_size() {
    let notches = u8::try_from(Resolution::ALL.len() - 1).expect("a handful of fractions");
    let native_floor = Ladder::new(Ladder::MAX_STEP - notches);
    for zoom in [Zoom::NEAREST, Zoom::DEFAULT, Zoom::FURTHEST] {
        let floor = Ladder::floor(1280, 720, zoom);
        let scale = floor.detail().resolution.scale();
        let view = Viewport::new(1280, 720, scale).expect("a real window");
        if scale.is_native() {
            assert_eq!(floor, native_floor, "{zoom:?} shed past the native scale");
        } else {
            assert!(
                readable(view.step(zoom)),
                "{zoom:?}'s floor draws figures unreadably"
            );
        }
        if let Some(deeper) = floor.shed() {
            let past = Viewport::new(1280, 720, deeper.detail().resolution.scale())
                .expect("a real window");
            assert!(
                !readable(past.step(zoom)),
                "{zoom:?} stopped short of a readable notch"
            );
        }
    }
    assert_eq!(
        Ladder::floor(1280, 720, Zoom::DEFAULT),
        Ladder::new(Ladder::MAX_STEP),
        "the view the game is authored at stays readable down the whole ladder"
    );
    assert!(
        Ladder::floor(1280, 720, Zoom::FURTHEST) < Ladder::floor(1280, 720, Zoom::DEFAULT),
        "a further view did not stop the render scale sooner"
    );
}
