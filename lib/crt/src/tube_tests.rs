//! Unit tests for the tube's strengths, the passes they resolve to, and the
//! clock the moving ones run on.

use super::{Pass, Phase, Tube, FULL, GLOW_REACH_PX, MAX_GLOW_INTENSITY};

#[test]
fn a_zero_strength_effect_contributes_no_pass() {
    let effects = Tube {
        wobble: 0,
        phosphor: 500,
        scanlines: 500,
        glow: 500,
        fuzz: 500,
    };
    let (passes, len) = effects.passes(100);
    assert!(!passes
        .iter()
        .take(len)
        .any(|pass| matches!(pass, Pass::Wobble { .. })));
}

#[test]
fn passes_are_ordered_wobble_phosphor_scanlines_glow_fuzz() {
    let effects = Tube {
        wobble: 500,
        phosphor: 500,
        scanlines: 500,
        glow: 500,
        fuzz: 500,
    };
    let (passes, len) = effects.passes(100);
    assert_eq!(len, 5);
    assert!(matches!(passes[0], Pass::Wobble { .. }));
    assert!(matches!(passes[1], Pass::Phosphor { .. }));
    assert!(matches!(passes[2], Pass::ScanLines { .. }));
    // The glow spreads the light the passes above settled on, so it follows
    // them and reaches into the scan lines' dark rows.
    assert!(matches!(passes[3], Pass::Glow { .. }));
    assert!(matches!(passes[4], Pass::Fuzz { .. }));
}

#[test]
fn wobble_amplitude_is_at_least_one_pixel_and_grows_with_scale() {
    let effects = Tube {
        wobble: 1,
        ..Tube::default()
    };
    let (passes, len) = effects.passes(100);
    assert_eq!(len, 1);
    let Pass::Wobble {
        amplitude_px: small_amplitude,
    } = passes[0]
    else {
        panic!("expected a wobble pass");
    };
    assert!(small_amplitude >= 1);

    let effects = Tube {
        wobble: 500,
        ..Tube::default()
    };
    let (passes_100, _) = effects.passes(100);
    let (passes_200, _) = effects.passes(200);
    let Pass::Wobble {
        amplitude_px: at_100,
    } = passes_100[0]
    else {
        panic!("expected a wobble pass");
    };
    let Pass::Wobble {
        amplitude_px: at_200,
    } = passes_200[0]
    else {
        panic!("expected a wobble pass");
    };
    assert!(at_200 > at_100);
}

/// Every effect off but the glow, spelled in full so a new effect has to be
/// considered here rather than defaulted into these tests.
fn glow_only(strength: u16) -> Tube {
    Tube {
        scanlines: 0,
        glow: strength,
        fuzz: 0,
        phosphor: 0,
        wobble: 0,
    }
}

/// The glow pass `strength` resolves to at `scale_percent`.
fn glow_pass(strength: u16, scale_percent: u32) -> Option<Pass> {
    let (passes, len) = glow_only(strength).passes(scale_percent);
    passes
        .iter()
        .take(len)
        .find(|pass| matches!(pass, Pass::Glow { .. }))
        .copied()
}

#[test]
fn glow_reach_is_density_scaled_and_never_nothing() {
    let reach = |scale: u32| match glow_pass(FULL, scale) {
        Some(Pass::Glow { reach_px, .. }) => reach_px,
        _ => panic!("a glow strength should contribute a glow pass"),
    };
    assert_eq!(reach(100), GLOW_REACH_PX);
    assert!(reach(200) > reach(100), "a denser screen spreads further");
    assert!(reach(1) >= 1, "and a halo is never nothing");
}

#[test]
fn glow_intensity_is_capped_so_the_picture_stays_legible() {
    // A bright field clears the knee on its own, so at full strength the
    // whole field glows; the cap is what keeps dark detail standing off it.
    match glow_pass(FULL, 100) {
        Some(Pass::Glow { intensity, .. }) => assert_eq!(intensity, MAX_GLOW_INTENSITY),
        _ => panic!("a glow strength should contribute a glow pass"),
    }
}

#[test]
fn a_zero_glow_contributes_no_pass() {
    assert_eq!(glow_pass(0, 100), None);
}

#[test]
fn phase_advance_wraps_at_the_maximum_without_panicking() {
    let phase = Phase(u32::MAX);
    assert_eq!(phase.advance(), Phase(0));
}

/// A wake for anything else never pushes the next frame back: the deadline
/// stands until it is met, so a busy window still animates.
#[test]
fn a_wake_before_the_frame_never_pushes_it_back() {
    use super::{frame_due, frame_interval_ns};
    use tairix_abi::driver::display::RefreshRate;

    let rate = RefreshRate::ASSUMED;
    let interval = frame_interval_ns(rate);
    let armed = frame_due(true, None, 1_000, rate);
    assert_eq!(armed, Some(1_000 + interval));
    for early in [1_001, 1_000 + interval / 2, 1_000 + interval - 1] {
        assert_eq!(
            frame_due(true, armed, early, rate),
            armed,
            "woken at {early}"
        );
    }
    assert_eq!(
        frame_due(false, armed, 2_000, rate),
        None,
        "nothing animates"
    );
}

/// An effect's frame is whole frames of the display it is shown on, never
/// faster than the effects draw.
#[test]
fn an_effect_frame_is_whole_frames_of_its_display() {
    use super::{frame_interval_ns, FASTEST_MHZ};
    use tairix_abi::driver::display::RefreshRate;

    for (mhz, frames) in [(60_000, 3), (144_000, 8), (50_000, 3), (20_000, 1)] {
        let rate = RefreshRate::from_mhz(mhz).expect("in bounds");
        assert_eq!(frame_interval_ns(rate), frames * rate.frame_ns(), "{mhz}");
        assert!(u64::from(mhz) <= frames * u64::from(FASTEST_MHZ));
    }
}
