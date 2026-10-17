//! What the land generator's host tests build lands from.

use tairix_parallel::{JobRunner, Threaded};

use crate::coarse::{Build, Coarse};
use crate::level::{Keeps, Piece, Refining};
use crate::plan::{Level, Plan, Rivers, Roadway, Surface, Wear, LEVELS};
use crate::relief::{Landform, Terrain};
use tairix_terrain::droplet::{Placed, Unkept};

/// Rolling hills `reach` either way of the middle over `cells` coarse cells,
/// drained by rivers if `rivers`, a track crossing them, refined through
/// `levels`: coarse enough to build in a moment.
pub(crate) fn hills(
    (reach, cells): (f64, usize),
    rivers: bool,
    levels: [Option<Level>; LEVELS],
    seed: u32,
) -> Plan {
    Plan {
        relief: Terrain {
            form: Landform::Hills {
                scale: 400.0,
                height: 60.0,
                seed,
            },
            datum: 0.0,
            centre: (0.0, 0.0),
            radius: reach,
            rim: Some(0.0),
            tilt: (0.01, 0.004),
            clearing: None,
        },
        reach,
        cells,
        levels,
        sea: None,
        wear: Wear {
            passes: 8,
            incision: 4.0e-4,
            creep: 0.09,
            repose: 0.9,
            infill: 0.3,
            strata: None,
        },
        rivers: rivers.then_some(Rivers {
            catchment: 2.0e5,
            width: 3.0,
            meander: 1.2,
            flowing: 1.0,
            ledges: 0.0,
            outcrops: 0.1,
        }),
        road: Some(Roadway {
            surface: Surface::Track,
            width: 3.0,
            heading: 0.7,
        }),
        farming: None,
        roughness: 1.0,
        ridges: 0.4,
        snow_line: None,
        snowpack: None,
        pond: None,
        growth: 1.0,
        seed: seed ^ 0x55,
    }
}

/// The runner a test shares its work across.
pub(crate) fn runner() -> Threaded {
    Threaded::new(8)
}

/// `plan`'s coarse land, built.
pub(crate) fn worn(plan: Plan, runner: &dyn JobRunner) -> Coarse {
    let mut build = Build::new(plan).expect("a build");
    while !build.step(runner).expect("builds") {}
    build.finish().expect("a coarse land")
}

/// The square `square` of level `level` of `coarse`, refined from `parent`.
pub(crate) fn refined(
    coarse: &Coarse,
    (level, square): (usize, Placed),
    parent: Option<&Piece>,
    runner: &dyn JobRunner,
) -> Piece {
    let mut refining = Refining::new(coarse, (level, square), Keeps::Everything, &mut Unkept)
        .expect("a refinement");
    while !refining
        .step(coarse, parent.map(Piece::parent), &mut Unkept, runner)
        .expect("refines")
    {}
    refining.finish().expect("a piece")
}
