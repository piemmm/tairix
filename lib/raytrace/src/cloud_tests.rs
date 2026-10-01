//! Host tests of volumetric cloud: its noises tile, a ray meets the bank
//! where it lies and not past it, and its shadow is a transmittance.

use super::*;

const CUMULUS: Deck = Deck {
    base: 1200.0,
    base_spread: 150.0,
    depth: (600.0, 1800.0),
    cover: 0.45,
    heap: 1.0,
    scale: 2500.0,
    stretch: 1.2,
    heading: 0.4,
    thickness: 0.05,
    billow: 400.0,
    seed: 17,
};

fn built(runner: &dyn JobRunner) -> Cloudbank {
    let sun = Vec3::new(0.3, 0.8, 0.2).normalized();
    let mut bank = Cloudbank::new([Some(CUMULUS), None], (0.0, 0.0), 12_000.0, sun).expect("fits");
    while !bank.step(runner).expect("held") {}
    bank.light_by(Lighting {
        sunlight: alloc::vec![Vec3::splat(20.0); SUNLIGHT_LEVELS],
        above: Vec3::new(0.6, 0.8, 1.2),
        below: Vec3::splat(0.3),
    });
    bank
}

#[test]
fn the_noises_repeat_across_their_period() {
    for step in 0..24u32 {
        let p = Vec3::new(
            f64::from(step) * 0.37,
            f64::from(step) * 0.91,
            f64::from(step) * 1.53,
        );
        let shifted = p + Vec3::new(4.0, 8.0, 12.0);
        assert!((worley_tiled(p, 4.0, 3) - worley_tiled(shifted, 4.0, 3)).abs() < 1e-9);
        assert!((perlin_tiled(p, 4.0, 3) - perlin_tiled(shifted, 4.0, 3)).abs() < 1e-9);
    }
}

#[test]
fn rays_up_through_the_bank_meet_cloud_and_rays_beside_it_do_not() {
    let bank = built(&tairix_parallel::SERIAL);
    let mut met = 0;
    let mut hidden = 0;
    for step in 0..400u32 {
        let x = f64::from(step % 20) * 400.0 - 4000.0;
        let z = f64::from(step / 20) * 400.0 - 4000.0;
        if let Some((light, kept, depth)) = bank.seen(Vec3::new(x, 0.0, z), Vec3::UP, true, 0.5) {
            met += 1;
            assert!(light.is_finite() && light.x.min(light.y).min(light.z) >= 0.0);
            assert!((0.0..=1.0).contains(&kept));
            assert!(depth > 900.0 && depth < 4000.0, "{depth}");
            if kept < 0.1 {
                hidden += 1;
            }
        }
    }
    assert!(met > 40, "{met} of 400 columns hold cloud");
    assert!(hidden > 10, "{hidden} are thick enough to hide the sky");
    assert!(met < 400, "and some are clear");
    let far = Vec3::new(20_000.0, 0.0, 0.0);
    assert!(
        bank.seen(far, Vec3::UP, true, 0.5).is_none(),
        "past the bank's edge"
    );
}

#[test]
fn the_bank_shades_some_of_the_ground_and_never_brightens_it() {
    let bank = built(&tairix_parallel::SERIAL);
    let sun = Vec3::new(0.3, 0.8, 0.2).normalized();
    let mut shaded = 0;
    for step in 0..900u32 {
        let x = f64::from(step % 30) * 300.0 - 4500.0;
        let z = f64::from(step / 30) * 300.0 - 4500.0;
        let kept = bank.shadow(Vec3::new(x, 0.0, z), sun);
        assert!((0.0..=1.0 + 1e-6).contains(&kept), "{kept}");
        if kept < 0.5 {
            shaded += 1;
        }
    }
    assert!(shaded > 50, "{shaded} of 900 points lie in cloud shadow");
    assert!(
        (bank.shadow(Vec3::new(0.0, 9_000.0, 0.0), sun) - 1.0).abs() < 1e-12,
        "nothing above the bank is shaded"
    );
}

#[test]
fn a_bank_built_across_workers_matches_one_built_alone() {
    let alone = built(&tairix_parallel::SERIAL);
    let spread = built(&tairix_parallel::Threaded::new(3));
    for step in 0..50u32 {
        let origin = Vec3::new(
            f64::from(step) * 97.0 - 2400.0,
            5.0,
            f64::from(step) * -61.0,
        );
        let dir = Vec3::new(0.3, 0.7, f64::from(step % 7) * 0.1).normalized();
        assert_eq!(
            alone.seen(origin, dir, true, 0.25),
            spread.seen(origin, dir, true, 0.25)
        );
        assert_eq!(
            alone.shadow(origin, dir).to_bits(),
            spread.shadow(origin, dir).to_bits()
        );
    }
}

/// What fraction of the light from beyond a ray's stretch through the bank
/// comes through it, integrated in two-metre steps: the reference a march
/// must agree with.
fn integrated(bank: &Cloudbank, origin: Vec3, dir: Vec3) -> f64 {
    let Some((enter, leave)) = bank.slab(origin, dir) else {
        return 1.0;
    };
    let reader = Reader { bank };
    let step = 2.0;
    let mut optical = 0.0;
    let mut t = enter + 0.5 * step;
    while t < leave {
        if let Some(sample) = reader.density(origin + dir * t, true) {
            optical += sample.density * sample.thickness * step;
        }
        t += step;
    }
    mathf::exp(-optical)
}

/// Seen from the ground across the bank, low and high, a march lets through
/// what the cloud it crosses lets through: were its steps long enough to fall
/// wherever they happened to at a cloud's edge, neighbouring rays would show
/// that edge in slices, and some would miss clouds altogether.
#[test]
fn a_march_through_the_bank_agrees_with_one_taken_in_fine_steps() {
    let bank = built(&tairix_parallel::SERIAL);
    let (mut crossed, mut worst) = (0u32, 0.0f64);
    for step in 0..240u32 {
        let heading = f64::from(step) * 2.399_963;
        let rise = 0.03 + 0.9 * f64::from(step % 40) / 40.0;
        let dir = Vec3::new(mathf::cos(heading), rise, mathf::sin(heading)).normalized();
        let origin = Vec3::new(
            f64::from(step % 13) * 300.0 - 1800.0,
            2.0,
            f64::from(step % 11) * -250.0,
        );
        let reference = integrated(&bank, origin, dir);
        let kept = bank
            .seen(origin, dir, true, 0.5)
            .map_or(1.0, |(_, kept, _)| kept);
        // Past the transmittance a march takes as opaque, both hide the sky.
        let expected = if reference < OPAQUE { 0.0 } else { reference };
        worst = worst.max((kept - expected).abs());
        crossed += u32::from(reference < 0.9);
    }
    assert!(crossed > 40, "{crossed} of 240 rays cross cloud");
    assert!(
        worst < 0.06,
        "a march strays {worst} from the cloud it crosses"
    );
}

/// However the samples of a pixel stagger a march, it finds the same cloud.
#[test]
fn a_march_sees_the_same_cloud_however_its_steps_are_staggered() {
    let bank = built(&tairix_parallel::SERIAL);
    for step in 0..120u32 {
        let heading = f64::from(step) * 1.1;
        let dir = Vec3::new(
            mathf::cos(heading),
            0.05 + 0.01 * f64::from(step % 9),
            mathf::sin(heading),
        )
        .normalized();
        let origin = Vec3::new(f64::from(step % 7) * 400.0, 2.0, -1500.0);
        let kept = |jitter: f64| {
            bank.seen(origin, dir, true, jitter)
                .map_or(1.0, |(_, kept, _)| kept)
        };
        let (low, high) = (kept(0.05), kept(0.95));
        assert!(
            (low - high).abs() < 0.08,
            "ray {step}: {low} or {high} by its stagger"
        );
    }
}
