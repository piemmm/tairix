use alloc::vec;

use super::*;

const MATERIALS: Materials = Materials {
    wood: 1,
    shoot: 2,
    leaf: 3,
    grape: 4,
    post: 5,
    wire: 6,
};

/// How many leaves and how many grapes a vine is built with.
fn counted(building: &Building) -> (usize, usize) {
    building
        .parts()
        .iter()
        .fold((0, 0), |(leaves, grapes), part| match part {
            Part::Leaf(_) => (leaves + 1, grapes),
            Part::Tube(tube) if tube.material == MATERIALS.grape => (leaves, grapes + 1),
            _ => (leaves, grapes),
        })
}

/// In summer a vine stands in leaf to its top wire with small grapes; in
/// autumn its grapes hang ripe; in winter it is pruned back to its wire.
#[test]
fn a_vine_grows_with_the_season() {
    for seed in 0..4 {
        let grown = |leafing: Leafing| {
            let mut dice = Dice::keyed(seed, 0);
            vine(&mut dice, leafing, &MATERIALS).expect("a vine")
        };
        let (summer, top, reach) = grown(Leafing::Leafy);
        assert!(
            top > CATCH[1].0 && top < 2.3,
            "{seed}: in summer it stands {top} tall"
        );
        assert!(reach > 0.4 && reach < 1.0, "{seed}: it reaches {reach}");
        let (leaves, _) = counted(&summer);
        assert!(leaves > 40, "{seed}: {leaves} leaves in summer");
        let (autumn, ..) = grown(Leafing::Turning);
        let (_, grapes) = counted(&autumn);
        assert!(grapes > 60, "{seed}: {grapes} grapes in autumn");
        let (winter, bare, _) = grown(Leafing::Bare);
        assert!(
            bare < FRUITING + 0.25,
            "{seed}: pruned, it stands {bare} tall"
        );
        assert_eq!(
            counted(&winter),
            (0, 0),
            "{seed}: leaves or grapes in winter"
        );
    }
}

/// A trellis's wires run between posts standing next to one another along
/// a row, and never across a gap where a post is missing or to the next
/// row.
#[test]
fn a_trellis_is_wired_only_between_neighbouring_posts() {
    let span = f64::from(POSTED) * APART;
    let along = |row: i32, at: f64| (row, Vec3::new(at, 0.0, 2.2 * f64::from(row)));
    let posts = vec![
        along(0, 0.0),
        along(0, span),
        along(0, 2.0 * span),
        along(1, 2.0 * span + 1.0),
        along(2, 0.0),
        along(2, 2.0 * span),
    ];
    let mut assembly = Assembly::with_room(64, 0).expect("room");
    wires(
        &mut assembly,
        &posts,
        (Vec3::new(0.0, 0.0, 1.0), span),
        (MATERIALS.wire, 1),
    )
    .expect("wired");
    assert_eq!(assembly.parts(), 2 * STRANDS.len() * SAG_PIECES);
    let building = assembly.finish().expect("built");
    for part in building.parts() {
        let Part::Tube(wire) = part else {
            panic!("a wire is a tube");
        };
        let (a, b) = (point(wire.a), point(wire.b));
        assert!(
            a.z.abs() < 0.1 && b.z.abs() < 0.1,
            "a wire off its row: {a:?} to {b:?}"
        );
        let lowest = a.y.min(b.y);
        assert!(lowest > FRUITING - SAG - 1e-6, "a wire sags to {lowest}");
    }
}

/// A point of a part, in double precision.
fn point(at: [f32; 3]) -> Vec3 {
    crate::prototype::point(at)
}
