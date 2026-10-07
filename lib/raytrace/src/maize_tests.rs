use super::*;

const LEAVES: Maize = Maize {
    greens: [Vec3::new(0.1, 0.3, 0.06), Vec3::new(0.12, 0.32, 0.07)],
    midrib: Vec3::new(0.3, 0.42, 0.2),
    straw: Vec3::new(0.6, 0.5, 0.3),
    dead: Vec3::new(0.3, 0.2, 0.1),
};

/// A leaf `length` long dried `dry`, placed under the plant's key `plant`, as
/// it shows `along` its length and `across` it, from its front or not.
fn at(dry: f64, plant: u32, (along, across): (f64, f64), front: bool) -> Vec3 {
    let length = 0.8;
    LEAVES.colour(&Spot {
        mark: drying(0x5a50, dry) ^ plant,
        instance: plant,
        uv: (along * length, across),
        girth: length,
        width: 1e-3,
        front,
        ..Spot::default()
    })
}

/// How green a colour is against its red.
fn greenness(colour: Vec3) -> f64 {
    colour.y / colour.x.max(1e-6)
}

/// A green leaf is green, paler along its midrib.
#[test]
fn a_green_leaf_is_paler_along_its_midrib() {
    let blade = at(0.0, 0x77, (0.4, 0.5), true);
    let midrib = at(0.0, 0x77, (0.4, 0.0), true);
    assert!(greenness(blade) > 2.0, "{blade:?}");
    assert!(
        midrib.luminance() > 1.15 * blade.luminance(),
        "{midrib:?} against {blade:?}"
    );
}

/// A drying leaf withers from its tip back while its base stays green, and
/// a dead one is brown all through, whatever plant it grows on.
#[test]
fn a_leaf_dries_from_its_tip() {
    for plant in [0, 0x1234_5678, u32::MAX] {
        let (base, tip) = (
            at(0.5, plant, (0.2, 0.3), true),
            at(0.5, plant, (0.97, 0.0), true),
        );
        assert!(greenness(base) > 2.0, "its base {base:?}");
        assert!(greenness(tip) < 1.4, "its tip {tip:?}");
        let dead = at(1.0, plant, (0.3, 0.2), true);
        assert!(greenness(dead) < 1.2, "a dead leaf {dead:?}");
    }
}

/// A leaf's underside is paler and greyer than its face.
#[test]
fn a_leafs_underside_is_greyer() {
    let (face, under) = (at(0.0, 9, (0.5, 0.4), true), at(0.0, 9, (0.5, 0.4), false));
    assert!(
        greenness(under) < greenness(face),
        "{under:?} against {face:?}"
    );
}
