//! Host tests of the snowpack: shelter read from the ground upwind, snow
//! drifting deep in the lee and scoured thin where exposed, never past what
//! a slope holds, the wind's forms cut along it from the snow alone and only
//! as finely as a grid resolves them, and depths kept finely about nought.

use super::*;

const PACK: Snowpack = Snowpack {
    heading: 0.0,
    fallen: 0.4,
    seed: 7,
};

/// A ridge across the wind, which blows toward +z: rising to 10 m at z = 0.
fn ridge(_: f64, z: f64) -> f64 {
    10.0 * mathf::exp(-(z * z) / (2.0 * 30.0 * 30.0))
}

#[test]
fn the_lee_of_a_rise_is_sheltered_and_its_crest_and_windward_face_exposed() {
    let lee = PACK.shelter(&ridge, (0.0, 40.0));
    let windward = PACK.shelter(&ridge, (0.0, -40.0));
    let crest = PACK.shelter(&ridge, (0.0, 0.0));
    let level = PACK.shelter(&|_, _| 3.0, (0.0, 0.0));
    assert!(lee > 0.15, "{lee}");
    assert!(windward < -0.03, "{windward}");
    assert!(
        crest < windward,
        "the crest the most exposed: {crest} against {windward}"
    );
    assert!(level.abs() < 1e-12, "{level}");
}

#[test]
fn snow_drifts_deep_in_the_lee_and_is_scoured_from_what_stands_exposed() {
    let at = (0.0, 40.0);
    let lee = PACK.depth(PACK.shelter(&ridge, at), at, 0.1);
    let level = PACK.depth(0.0, at, 0.0);
    let windward = PACK.depth(PACK.shelter(&ridge, (0.0, -40.0)), at, 0.1);
    let crest = PACK.depth(PACK.shelter(&ridge, (0.0, 0.0)), at, 0.0);
    assert!(lee > 1.8 * level, "{lee} against {level}");
    assert!(windward < 0.75 * level, "{windward} against {level}");
    assert!(crest < 0.25 * level, "{crest} against {level}");
    // The wind never takes the crust it cannot lift.
    let bare = PACK.depth(-10.0, at, 0.0);
    assert!(bare > 0.0 && bare < 0.1 * PACK.fallen, "{bare}");
    // What fell is what lies where nothing moved it, give or take its
    // uneven fall.
    assert!(
        (level / PACK.fallen - 1.0).abs() <= UNEVEN + 1e-9,
        "{level}"
    );
}

#[test]
fn snow_sloughs_off_ground_steeper_than_it_rests_on() {
    let gentle = PACK.depth(0.3, (5.0, 5.0), 0.2);
    let steep = PACK.depth(0.3, (5.0, 5.0), 1.3);
    assert!(gentle > 0.3, "{gentle}");
    assert!(steep.abs() < 1e-12, "{steep}");
}

#[test]
fn the_wind_carves_scoured_snow_along_its_way_and_never_through_it() {
    let (mut deepest, mut along, mut across) = (0.0f64, 0.0, 0.0);
    for step in 0..4000u32 {
        let (x, z) = (f64::from(step % 64) * 0.13, f64::from(step / 64) * 0.13);
        let thin = 0.12;
        let cut = PACK.carved((x, z), thin, 0.05);
        assert!(
            cut <= 0.0 && cut >= -0.8 * thin - 1e-12,
            "{cut} at {x}, {z}"
        );
        deepest = deepest.min(cut);
        // Along the wind the form changes far more slowly than across it.
        along += (PACK.carved((x, z + 0.05), thin, 0.05) - cut).abs();
        across += (PACK.carved((x + 0.05, z), thin, 0.05) - cut).abs();
    }
    assert!(deepest < -0.02, "{deepest}");
    assert!(
        across > 1.5 * along,
        "{across} across against {along} along"
    );
    // Soft drifts are left as the wind dropped them.
    assert!(PACK.carved((1.0, 2.0), 2.0 * PACK.fallen, 0.05).abs() < 1e-12);
}

#[test]
fn a_grid_too_coarse_for_a_form_keeps_none_of_it() {
    for step in 0..500u32 {
        let at = (f64::from(step) * 0.37, f64::from(step) * 0.61);
        assert!(PACK.carved(at, 0.1, 10.0).abs() < 1e-12, "{at:?}");
    }
}

#[test]
fn depths_are_kept_finely_about_nought() {
    for depth in [0.0, 0.01, 0.03, 0.1, 0.5, 2.0, DEEPEST] {
        let back = depth_of(f64::from(kept(depth)) / 255.0);
        let allowed = 0.004f64.max(0.02 * depth);
        assert!((back - depth).abs() <= allowed, "{depth} came back {back}");
    }
    assert_eq!(kept(-1.0), 0);
    assert_eq!(kept(2.0 * DEEPEST), u8::MAX);
}

#[test]
fn snow_buries_what_stands_lower_than_it_lies() {
    assert!(buries(0.0, 0.1).abs() < 1e-12);
    assert!((buries(0.2, 0.1) - 1.0).abs() < 1e-12);
    assert!(buries(0.05, 0.1) > 0.0 && buries(0.05, 0.1) < 1.0);
}
