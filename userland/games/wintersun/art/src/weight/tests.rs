use super::{WeightField, TOTAL};
use tairix_wintersun_world::blend::{Blend, Kind, BLEND_SLOTS};
use tairix_wintersun_world::ground::Ground;

#[test]
fn a_solid_field_is_one_material_at_full_weight() {
    let field = WeightField::solid(Ground::Granite);
    assert_eq!(field.slots().len(), 1);
    assert_eq!(field.dominant(), Ground::Granite);
    assert_eq!(field.total(), TOTAL);
    assert_eq!(field.weight_of(Ground::Granite), TOTAL);
    assert_eq!(field.weight_of(Ground::GoldenSand), 0);
}

#[test]
fn a_generated_blend_round_trips() {
    let blend = Blend::solid(Ground::NeedleLitter);
    let field = WeightField::from_blend(&blend);
    assert_eq!(field.slots().len(), 1);
    assert_eq!(field.dominant(), Ground::NeedleLitter);
    assert_eq!(field.total(), TOTAL);
}

#[test]
fn covering_reaches_at_least_the_coverage_asked_for() {
    for coverage in [1u16, 17, 64, 128, 200, 254, 255] {
        let mut field = WeightField::solid(Ground::LeafLitter);
        assert!(field.cover(Ground::Gravel, coverage));
        assert!(
            field.weight_of(Ground::Gravel) >= coverage,
            "asked {coverage}, got {}",
            field.weight_of(Ground::Gravel),
        );
        assert_eq!(field.total(), TOTAL);
    }
}

#[test]
fn covering_twice_at_the_same_coverage_is_a_no_op() {
    let mut field = WeightField::solid(Ground::DryGrass);
    assert!(field.cover(Ground::Gravel, 180));
    let once = field;
    assert!(!field.cover(Ground::Gravel, 180));
    assert_eq!(field, once);
}

#[test]
fn covering_is_order_independent_for_one_material() {
    // Two roads crossing: stamped either way round, the junction is the
    // same road.
    let mut a = WeightField::solid(Ground::Peat);
    a.cover(Ground::Gravel, 120);
    a.cover(Ground::Gravel, 200);

    let mut b = WeightField::solid(Ground::Peat);
    b.cover(Ground::Gravel, 200);
    b.cover(Ground::Gravel, 120);

    assert_eq!(a, b);
    assert!(a.weight_of(Ground::Gravel) >= 200);
}

#[test]
fn covering_fully_replaces_the_field() {
    let mut field = WeightField::solid(Ground::Mud);
    assert!(field.cover(Ground::Water, TOTAL));
    assert_eq!(field.slots().len(), 1);
    assert_eq!(field.dominant(), Ground::Water);
    assert_eq!(field.total(), TOTAL);
}

#[test]
fn covering_below_the_current_share_changes_nothing() {
    let mut field = WeightField::solid(Ground::GoldenSand);
    assert!(!field.cover(Ground::GoldenSand, 200));
    assert_eq!(field.weight_of(Ground::GoldenSand), TOTAL);
}

#[test]
fn a_faint_stamp_on_a_full_field_is_refused() {
    let mut field = WeightField::solid(Ground::Granite);
    for (material, coverage) in [
        (Ground::Gravel, 200u16),
        (Ground::GoldenSand, 120),
        (Ground::Peat, 60),
    ] {
        assert!(field.cover(material, coverage));
    }
    assert_eq!(field.slots().len(), BLEND_SLOTS);
    let lightest = field.slots()[BLEND_SLOTS - 1].weight;
    let before = field;
    assert!(!field.cover(Ground::Ash, lightest));
    assert_eq!(
        field, before,
        "a stamp lighter than the field displaced one"
    );
}

#[test]
fn a_heavy_stamp_on_a_full_field_displaces_the_lightest() {
    let mut field = WeightField::solid(Ground::Granite);
    for (material, coverage) in [
        (Ground::Gravel, 200u16),
        (Ground::GoldenSand, 120),
        (Ground::Peat, 60),
    ] {
        assert!(field.cover(material, coverage));
    }
    let displaced = field.slots()[BLEND_SLOTS - 1].ground;
    assert!(field.cover(Ground::Snow, 250));
    assert_eq!(field.slots().len(), BLEND_SLOTS);
    assert_eq!(field.weight_of(displaced), 0);
    assert!(field.weight_of(Ground::Snow) >= 250);
    assert_eq!(field.total(), TOTAL);
}

#[test]
fn covering_never_exceeds_four_materials() {
    let mut field = WeightField::solid(Ground::Lichen);
    for (i, material) in Ground::ALL.iter().enumerate() {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "fewer grounds than a u16 holds"
        )]
        field.cover(*material, 40 + (i as u16) * 13);
        assert!(field.slots().len() <= BLEND_SLOTS);
        assert_eq!(field.total(), TOTAL);
    }
}

#[test]
fn slots_are_ordered_heaviest_first() {
    let mut field = WeightField::solid(Ground::Heath);
    field.cover(Ground::Gravel, 90);
    field.cover(Ground::Snow, 150);
    field.cover(Ground::GoldenSand, 30);
    let weights: alloc::vec::Vec<u16> = field.slots().iter().map(|s| s.weight).collect();
    let mut sorted = weights.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(weights, sorted);
}

#[test]
fn lerp_reproduces_its_endpoints() {
    let a = WeightField::solid(Ground::Ice);
    let mut b = WeightField::solid(Ground::Ash);
    b.cover(Ground::RiftGround, 100);

    assert_eq!(a.lerp(&b, 0), a);
    assert_eq!(a.lerp(&b, 255), b);
}

#[test]
fn lerp_fades_a_material_in_rather_than_switching_to_it() {
    let a = WeightField::solid(Ground::Snow);
    let b = WeightField::solid(Ground::Granite);
    let mut previous = 0;
    for t in [0u8, 32, 64, 96, 128, 160, 192, 224, 255] {
        let mixed = a.lerp(&b, t);
        let rock = mixed.weight_of(Ground::Granite);
        assert!(rock >= previous, "rock went backwards at t={t}");
        assert_eq!(mixed.total(), TOTAL);
        previous = rock;
    }
    assert_eq!(a.lerp(&b, 128).weight_of(Ground::Granite), 128);
}

#[test]
fn lerp_stays_normalised_over_disjoint_material_sets() {
    let mut a = WeightField::solid(Ground::Water);
    a.cover(Ground::Mud, 100);
    let mut b = WeightField::solid(Ground::Ice);
    b.cover(Ground::Snow, 100);

    for t in 0..=u8::MAX {
        let mixed = a.lerp(&b, t);
        assert_eq!(mixed.total(), TOTAL, "unnormalised at t={t}");
        assert!(!mixed.slots().is_empty());
        assert!(mixed.slots().iter().all(|s| s.weight > 0));
    }
}

#[test]
fn equality_ignores_the_unused_tail() {
    // `solid` fills every slot; `cover` back to solid leaves the tail at
    // zero. Both are one material at full weight and must compare equal.
    let direct = WeightField::solid(Ground::Gravel);
    let mut covered = WeightField::solid(Ground::Peat);
    covered.cover(Ground::Gravel, TOTAL);
    assert_eq!(direct, covered);
}
