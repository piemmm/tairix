//! Host tests of the coarse land: its rivers run downhill, its road's
//! bridges clear their water, its build's progress only ever grows, and a
//! land brought from outside refines as one the crate wore itself.

use super::*;
use crate::attributes::SEDIMENT;
use crate::plan::{Lattice, Level, LEVELS};
use crate::relief::Landform;
use crate::testing::{hills, refined, runner, worn};

/// Level 0 at twice the coarse lattice's density, and no finer level.
fn far_only() -> [Option<Level>; LEVELS] {
    let mut levels = [None; LEVELS];
    levels[0] = Some(Level {
        ratio: 2,
        droplets: 0.02,
    });
    levels
}

#[test]
fn rivers_run_downhill() {
    let coarse = worn(hills((1500.0, 128), true, far_only(), 7), &runner());
    let rivers = coarse.rivers();
    assert!(
        !rivers.is_empty(),
        "a land with a catchment drains through rivers"
    );
    for course in 0..rivers.len() {
        for pair in rivers.course(course).windows(2) {
            if let [upstream, downstream] = pair {
                assert!(
                    downstream.level <= upstream.level + 0.05,
                    "a river climbs: {upstream:?} {downstream:?}"
                );
            }
        }
    }
}

/// A road's bridge stands its clearance and more over the water it spans,
/// its road level along the deck from end to end.
#[test]
fn a_bridges_deck_clears_its_river_and_carries_its_road_level() {
    let mut decks = 0;
    for seed in [7, 11, 13] {
        let coarse = worn(hills((1500.0, 128), true, far_only(), seed), &runner());
        let track = coarse.ways().of(Surface::Track);
        if track.is_empty() {
            continue;
        }
        let road = track.course(0);
        for crossing in coarse.crossings() {
            assert!(
                crossing.deck >= crossing.water + CLEARANCE + 0.08 * crossing.width - 1e-9,
                "{seed}: a deck {} over water {}",
                crossing.deck,
                crossing.water
            );
            let at = |mark: &Mark| {
                road.iter()
                    .position(|other| {
                        other.x.to_bits() == mark.x.to_bits()
                            && other.z.to_bits() == mark.z.to_bits()
                    })
                    .expect("a crossing's ends lie on its road")
            };
            let (from, to) = (at(&crossing.from), at(&crossing.to));
            for mark in &road[from.min(to)..=from.max(to)] {
                assert!(
                    (mark.level - crossing.deck).abs() < 1e-9,
                    "{seed}: the road at {} on a deck at {}",
                    mark.level,
                    crossing.deck
                );
            }
            decks += 1;
        }
    }
    assert!(decks > 0, "some road crosses its land's rivers");
}

/// However its stages fall, a build reports more of its work done after
/// each unit than before it, and all of it once built: a farmed land under
/// snow, its countryside laid out before its shelter from the wind is read.
#[test]
fn a_builds_progress_only_ever_grows() {
    let runner = runner();
    let mut plan = hills((1500.0, 128), true, far_only(), 3);
    plan.snowpack = Some(crate::snow::Snowpack {
        heading: 0.4,
        fallen: 0.3,
        seed: 5,
    });
    plan.farming = Some(Farming {
        spacing: 500.0,
        villages: tairix_countryside::site::Villages {
            spacing: 2500.0,
            exclusion: 0.75,
            gathers: 0.28,
        },
        style: tairix_countryside::boundary::Style {
            hedge: 0.5,
            wall: 0.3,
            fence: 0.2,
            ditch: 0.1,
            open: 0.1,
        },
        mix: tairix_countryside::usage::Mix {
            arable: 0.4,
            pasture: 0.3,
            meadow: 0.2,
            orchard: 0.05,
            vineyard: 0.0,
            woodlot: 0.05,
            overgrown: 0.0,
        },
        wooded: 0.3,
        stone: 0.2,
        reach: 1000.0,
        lanes: (0.3, 0.5),
        season: crate::Season::Winter,
    });
    let mut build = Build::new(plan).expect("a build");
    let mut last = build.done();
    assert!(
        last < 0.05,
        "a build begun has done next to nothing: {last}"
    );
    while !build.step(&runner).expect("builds") {
        let now = build.done();
        assert!(now >= last, "its progress fell from {last} to {now}");
        last = now;
    }
    assert!((build.done() - 1.0).abs() < f64::EPSILON);
}

/// The land's square lies on whole steps of a lattice fixed to the land, so
/// a scene's square moved by less than half a step wears the same samples.
#[test]
fn a_lands_square_snaps_to_its_lattice() {
    let mut plan = hills((1500.0, 128), false, far_only(), 3);
    let lattice = plan.lattice().expect("a lattice");
    assert_eq!(lattice.origin, (-64, -64));
    plan.relief.centre = (0.3 * lattice.step, -0.4 * lattice.step);
    assert_eq!(plan.lattice().expect("a lattice"), lattice);
    plan.relief.centre = (0.6 * lattice.step, 0.0);
    assert_eq!(plan.lattice().expect("a lattice").origin, (-63, -64));
}

/// A river a quarter of a cell wide, slanting across a water grid at many
/// angles, stands at all four corners of every cell its course crosses.
#[test]
fn a_narrow_slanting_river_wets_every_corner_of_every_cell_it_crosses() {
    let (step, width) = (4.0, 1.0);
    let origin = (-128.0, -128.0);
    let mut coarse = worn(hills((1500.0, 32), true, far_only(), 1), &runner());
    // The river's water alone, no lake's.
    coarse.lakes.clear();
    for turn in 0..24 {
        let angle = f64::from(turn) * 0.13 + 0.05;
        let (dx, dz) = (mathf::cos(angle), mathf::sin(angle));
        let through = |t: f64| (dx * t + 0.37, dz * t - 0.21);
        let line: alloc::vec::Vec<Mark> = (0..=40)
            .map(|mark| {
                let (x, z) = through(f64::from(mark) * 6.0 - 120.0);
                Mark {
                    x,
                    z,
                    level: 10.0,
                    width,
                    depth: 0.5,
                    ..Mark::default()
                }
            })
            .collect();
        let reach = Reach {
            per_width: 2.5,
            beyond: 16.0,
        };
        coarse.rivers = Courses::new(&[line], (origin, 256.0), reach).expect("indexed");
        for along in 0..400 {
            let (x, z) = through(f64::from(along) * 0.1 - 20.0);
            let (column, row) = (
                mathf::floor((x - origin.0) / step),
                mathf::floor((z - origin.1) / step),
            );
            for (right, down) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                let corner = (
                    origin.0 + (column + right) * step,
                    origin.1 + (row + down) * step,
                );
                assert!(
                    coarse.water_surface(corner.0, corner.1, step).is_finite(),
                    "{angle}: the cell at ({column}, {row}) is dry at its corner ({right}, {down})"
                );
            }
        }
    }
}

/// The field a built coarse land brings: its lattices, heights, wetness, grain
/// and character, its drainage filled to its own heights.
fn field_of(coarse: &Coarse) -> Field {
    Field {
        lattices: *coarse.lattices(),
        heights: coarse.height.clone(),
        filled: coarse.height.clone(),
        wetness: coarse.wetness.clone(),
        grains: coarse.grains.clone(),
        character: coarse.character,
    }
}

/// A land with no rivers, ways or fields, brought back as a field, refines to
/// the same heights to the bit, and carries the same of all but the silt the
/// crate's own wear laid, which a field does not record. Its levels read no
/// lake: what stands on a land is for its consumer to read.
#[test]
fn a_land_brought_from_outside_refines_as_the_crates_own() {
    let runner = runner();
    let mut plan = hills((1500.0, 128), false, far_only(), 4);
    plan.road = None;
    let own = worn(plan, &runner);
    let brought = Coarse::of_field(field_of(&own), &runner).expect("a field");
    let domain = own.lattices().domain(0).expect("a domain");
    let (a, b) = (
        refined(&own, (0, domain), None, &runner),
        refined(&brought, (0, domain), None, &runner),
    );
    for (index, (one, other)) in a.heights.iter().zip(&b.heights).enumerate() {
        assert_eq!(
            one.to_bits(),
            other.to_bits(),
            "vertex {index}: {one} against {other}"
        );
    }
    for (one, other) in a.attributes.iter().zip(&b.attributes) {
        let without = |carried: &crate::attributes::Attributes| {
            let mut kept = *carried;
            kept[SEDIMENT] = 0;
            kept
        };
        assert_eq!(without(one), without(other));
    }
}

/// A sample its drainage fills deeper than a lake stands holds one, and so do
/// the samples beside it; ground at or under the sea holds none however deep
/// it is filled, the sea standing there instead.
#[test]
fn a_field_holds_a_lake_where_its_drainage_fills_it_deep() {
    let runner = runner();
    let lattices = Lattices::new(
        Lattice {
            step: 10.0,
            origin: (0, 0),
            cells: 8,
        },
        far_only(),
    )
    .expect("lattices");
    let mut field = Field {
        lattices,
        heights: alloc::vec![5.0; 81],
        filled: alloc::vec![5.0; 81],
        wetness: alloc::vec![0.0; 81],
        grains: Grains::Even(Grain::hummocks(0.0)),
        character: Character {
            longest: 30.0,
            growth: 1.0,
            snow_line: None,
            sea: Some(0.0),
            snowpack: None,
            seed: 1,
        },
    };
    field.filled[4 * 9 + 4] = 7.0;
    field.heights[2 * 9 + 6] = -1.0;
    field.filled[2 * 9 + 6] = 3.0;
    let land = Coarse::of_field(field, &runner).expect("a field");
    let lake = |index: usize| {
        land.lakes
            .get(index)
            .copied()
            .filter(|level| level.is_finite())
    };
    assert_eq!(lake(4 * 9 + 4), Some(7.0), "the pit holds its lake");
    assert_eq!(
        lake(4 * 9 + 5),
        Some(7.0),
        "and so does the shore beside it"
    );
    assert_eq!(
        lake(2 * 9 + 6),
        None,
        "the sea, not a lake, stands under its level"
    );
    assert_eq!(lake(0), None, "dry ground holds none");
}

/// A plan any of whose numbers is not one a land is built from is refused
/// before any of it is built, as a field's are: never built into a land of
/// heights that are no numbers.
#[test]
fn a_plan_that_does_not_fit_is_refused() {
    let drawn = || hills((1500.0, 32), true, far_only(), 2);
    assert!(Build::new(drawn()).is_ok(), "the plan as drawn fits");
    let refused = |what: &str, change: &dyn Fn(&mut Plan)| {
        let mut plan = drawn();
        change(&mut plan);
        assert_eq!(Build::new(plan).err(), Some(Error::Shape), "{what}");
    };
    refused("roughness", &|plan| plan.roughness = f64::NAN);
    refused("ridges", &|plan| plan.ridges = 1.5);
    refused("growth", &|plan| plan.growth = -0.1);
    refused("sea", &|plan| plan.sea = Some(f64::NAN));
    refused("datum", &|plan| plan.relief.datum = f64::INFINITY);
    refused("scale", &|plan| {
        plan.relief.form = Landform::Hills {
            scale: 0.0,
            height: 60.0,
            seed: 1,
        };
    });
    refused("repose", &|plan| plan.wear.repose = 0.0);
    refused("flow", &|plan| {
        if let Some(rivers) = plan.rivers.as_mut() {
            rivers.flowing = 2.0;
        }
    });
    refused("road", &|plan| {
        if let Some(road) = plan.road.as_mut() {
            road.width = f64::NAN;
        }
    });
    refused("pond", &|plan| plan.pond = Some(((0.0, f64::NAN), 30.0)));
}

/// A field whose samples do not fill its lattice, or whose heights, wetness,
/// grain or character are not numbers in their ranges, is refused.
#[test]
fn a_field_that_does_not_fit_is_refused() {
    let runner = runner();
    let own = worn(hills((1500.0, 32), false, far_only(), 2), &runner);
    let refused = |change: &dyn Fn(&mut Field)| {
        let mut field = field_of(&own);
        change(&mut field);
        Coarse::of_field(field, &runner).err()
    };
    assert!(refused(&|_| {}).is_none(), "the field as built fits");
    assert_eq!(
        refused(&|field| {
            field.heights.pop();
        }),
        Some(Error::Shape)
    );
    assert_eq!(
        refused(&|field| field.filled[3] = f64::NAN),
        Some(Error::Shape)
    );
    assert_eq!(refused(&|field| field.wetness[0] = 1.5), Some(Error::Shape));
    assert_eq!(
        refused(&|field| {
            field.grains = Grains::Even(Grain {
                ridges: 0.7,
                dunes: 0.4,
                ..Grain::hummocks(1.0)
            });
        }),
        Some(Error::Shape),
        "shares past one"
    );
    assert_eq!(
        refused(&|field| field.grains = Grains::Sampled(alloc::vec::Vec::new())),
        Some(Error::Shape),
        "a grain short of its samples"
    );
    assert_eq!(
        refused(&|field| field.character.longest = 0.0),
        Some(Error::Shape)
    );
}
