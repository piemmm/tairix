use crate::{chromaticity_of, uv_of, white_of, Illuminant, Xyz, KELVIN_MAX, KELVIN_MIN};

fn near(a: f64, b: f64, by: f64) -> bool {
    (a - b).abs() <= by
}

#[test]
fn srgb_white_is_six_thousand_five_hundred_kelvin_a_little_green() {
    let d65 = Illuminant::of_linear([1.0, 1.0, 1.0]).expect("white has a colour");
    assert!(near(d65.kelvin, 6504.0, 4.0), "{d65:?}");
    assert!(near(d65.duv, 0.0032, 2e-4), "{d65:?}");
    // The white it names is the one it was measured from.
    let [r, g, b] = d65.white();
    assert!(
        near(r, 1.0, 1e-3) && near(g, 1.0, 1e-3) && near(b, 1.0, 1e-3),
        "{r} {g} {b}"
    );
}

#[test]
fn a_tungsten_light_sits_on_the_locus_at_its_temperature() {
    // CIE illuminant A, the 2856 K incandescent standard.
    let (x, y) = (0.447_57, 0.407_45);
    let xyz = Xyz {
        x: x / y,
        y: 1.0,
        z: (1.0 - x - y) / y,
    };
    let measured = Illuminant::of_linear(xyz.to_linear()).expect("a colour");
    assert!(near(measured.kelvin, 2856.0, 6.0), "{measured:?}");
    assert!(near(measured.duv, 0.0, 5e-4), "{measured:?}");
}

#[test]
fn a_light_comes_back_as_the_temperature_and_tint_it_was_named_by() {
    for kelvin in [
        1200.0, 2000.0, 3200.0, 4000.0, 5500.0, 6500.0, 9000.0, 12000.0, 14500.0,
    ] {
        for duv in [-0.02, -0.005, 0.0, 0.005, 0.02] {
            let named = Illuminant::new(kelvin, duv);
            let back = Illuminant::of_linear(named.white()).expect("a colour");
            assert!(
                near(back.kelvin, kelvin, kelvin * 2e-3),
                "{named:?} came back {back:?}"
            );
            assert!(near(back.duv, duv, 2e-4), "{named:?} came back {back:?}");
        }
    }
}

#[test]
fn a_warmer_light_is_redder_and_a_greener_one_greener() {
    let [warm_r, _, warm_b] = Illuminant::new(3000.0, 0.0).white();
    let [cool_r, _, cool_b] = Illuminant::new(10000.0, 0.0).white();
    assert!(warm_r > cool_r && warm_b < cool_b);
    let [_, green, _] = Illuminant::new(6500.0, 0.01).white();
    let [_, magenta, _] = Illuminant::new(6500.0, -0.01).white();
    assert!(green > magenta);
}

#[test]
fn a_temperature_outside_the_fit_is_held_to_it_and_black_has_none() {
    assert!(near(Illuminant::new(100.0, 0.0).kelvin, KELVIN_MIN, 0.0));
    assert!(near(Illuminant::new(1e9, 0.0).kelvin, KELVIN_MAX, 0.0));
    assert_eq!(Illuminant::of_linear([0.0, 0.0, 0.0]), None);
}

#[test]
fn the_uv_plane_and_chromaticity_turn_into_each_other() {
    for (x, y) in [(0.3127, 0.3290), (0.44757, 0.40745), (0.25, 0.27)] {
        let uv = uv_of((x, y)).expect("a projection");
        let (back_x, back_y) = chromaticity_of(uv);
        assert!(near(back_x, x, 1e-12) && near(back_y, y, 1e-12));
    }
    let [r, g, b] = white_of(Xyz::D65.chromaticity().expect("a colour"));
    assert!(near(r, 1.0, 1e-4) && near(g, 1.0, 1e-4) && near(b, 1.0, 1e-4));
    let named = Illuminant::new(4200.0, -0.008);
    let back = Illuminant::of_uv(named.uv());
    assert!(
        near(back.kelvin, 4200.0, 5.0) && near(back.duv, -0.008, 1e-5),
        "{back:?}"
    );
}
