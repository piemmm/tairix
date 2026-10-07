use super::*;

#[test]
fn every_growth_keeps_its_byte_and_a_byte_naming_nothing_is_wild() {
    let mut seen = alloc::vec::Vec::new();
    for unsown in UNSOWN {
        seen.push(unsown);
    }
    for crop in CROPS {
        for stage in STAGES {
            seen.push(Grown::Sown(crop, stage));
        }
    }
    let mut codes: alloc::vec::Vec<u8> = seen.iter().map(|grown| grown.code()).collect();
    for (grown, &code) in seen.iter().zip(&codes) {
        assert_eq!(Grown::of(code), *grown, "{code}");
    }
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), seen.len(), "two growths share a byte");
    for code in [8, 15, 23, 255] {
        assert_eq!(Grown::of(code), Grown::Wild, "{code}");
    }
}

/// Spring stands cereals green, rape in flower and maize just drilled;
/// summer ripens the cereals and has cut some; autumn has cut, ploughed or
/// drilled again; winter leaves young shoots or a ploughed field.
#[test]
fn each_season_stands_each_crop_where_it_would_be() {
    let arable = |crop| Usage {
        used: Use::Arable(crop),
        bale: None,
    };
    let stages = |crop: Crop, season: Season| -> alloc::vec::Vec<Stage> {
        (0..100)
            .filter_map(
                |step| match grown(arable(crop), season, f64::from(step) / 100.0) {
                    Grown::Sown(_, stage) => Some(stage),
                    _ => None,
                },
            )
            .collect()
    };
    assert!(stages(Crop::Wheat, Season::Spring)
        .iter()
        .all(|&stage| matches!(stage, Stage::Green | Stage::Shooting)));
    assert!(stages(Crop::Rapeseed, Season::Spring)
        .iter()
        .all(|&stage| stage == Stage::Flowering));
    assert!(stages(Crop::Maize, Season::Spring)
        .iter()
        .all(|&stage| stage == Stage::Drilled));
    let summer = stages(Crop::Barley, Season::Summer);
    assert!(summer.contains(&Stage::Ripe) && summer.contains(&Stage::Stubble));
    let autumn = stages(Crop::Oats, Season::Autumn { fallen: 10 });
    assert!([Stage::Stubble, Stage::Ploughed, Stage::Shooting]
        .iter()
        .all(|stage| autumn.contains(stage)));
    assert!(stages(Crop::Wheat, Season::Winter)
        .iter()
        .all(|&stage| matches!(stage, Stage::Shooting | Stage::Ploughed)));
    let pasture = Usage {
        used: Use::Pasture,
        bale: None,
    };
    assert_eq!(grown(pasture, Season::Summer, 0.1), Grown::Grazed);
    assert!(!Grown::Grazed.tilled() && Grown::Sown(Crop::Oats, Stage::Ploughed).tilled());
}

#[test]
fn a_fields_rows_keep_their_way_to_within_a_byte() {
    for step in 0..64 {
        let heading = -3.0 + 0.1 * f64::from(step);
        let (x, z) = rows_way(rows_code(heading));
        let (wx, wz) = (mathf::sin(heading), mathf::cos(heading));
        // Rows run either way along their heading.
        let along = (x * wx + z * wz).abs();
        assert!(
            along > mathf::cos(0.5 * PI / 255.0) - 1e-9,
            "{heading}: {along}"
        );
    }
}
