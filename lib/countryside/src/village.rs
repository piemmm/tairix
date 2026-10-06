//! A village's layout: plots by frontage along each street through it, on
//! both sides, each with its house at its front, its front garden and back
//! garden, the fence about it and its garden gate; and now and then a green
//! at its middle no plot takes.

use alloc::vec::Vec;

use crate::farm::{rectangle, Footprint, Standing};
use crate::ground::{self, Ground};
use crate::key::{Key, Stage};
use crate::network::{Rank, WayId};
use crate::plane::{Convex, Point};
use crate::route::Line;
use crate::site::{Settled, Settlement};
use crate::Error;

/// What a garden is fenced with.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Fencing {
    /// Boards set edge to edge on rails.
    Closeboard,
    /// Posts and rails.
    PostAndRail,
    /// A hedge.
    Hedge,
    /// A low wall.
    Wall,
    /// Nothing: an open front.
    Open,
}

/// A plot along a village's street.
#[derive(Clone, Debug, PartialEq)]
pub struct Plot {
    /// The street it fronts.
    pub street: WayId,
    /// Its whole outline.
    pub outline: Convex,
    /// Its frontage on the street, from end to end.
    pub frontage: (Point, Point),
    /// Its house.
    pub house: Footprint,
    /// Its garden between the street and the house.
    pub front: Convex,
    /// The garden behind the house.
    pub back: Convex,
    /// What it is fenced with.
    pub fencing: Fencing,
    /// Where its garden gate stands on its frontage.
    pub gate: Point,
}

/// A village laid out.
#[derive(Clone, Debug, PartialEq)]
pub struct Village {
    /// The settlement it is.
    pub settled: Settled,
    /// Its plots, street by street.
    pub plots: Vec<Plot>,
    /// Its green, where it has one.
    pub green: Option<Convex>,
}

/// How far a village's plots reach from its middle along its streets.
pub(crate) const REACH: (f64, f64) = (150.0, 260.0);

/// The layout of `village` along the bounded ways `streets` that pass it,
/// over `ground`, its plots clear of `barred`.
pub(crate) fn lay_out(
    key: Key,
    village: &Settlement,
    (streets, barred): (&[(WayId, &Line)], &[Convex]),
    ground: &dyn Ground,
) -> Result<Village, Error> {
    let mut draws = key.draws(Stage::Plot, village.settled.place());
    let reach = draws.range(REACH.0, REACH.1);
    let green = draws.chance(0.5).then(|| {
        let side = draws.range(30.0, 60.0);
        rectangle(village.at, Point::toward(draws.range(0.0, core::f64::consts::PI)), (side, side * draws.range(0.6, 1.0)))
    });
    let mut plots: Vec<Plot> = Vec::new();
    for (ordinal, &(street, line)) in streets.iter().enumerate() {
        if !street.rank.bounded() || street.rank == Rank::Track {
            continue;
        }
        let ordinal = i64::try_from(ordinal).unwrap_or(0);
        for (side, sign) in [(0u8, 1.0), (1, -1.0)] {
            let mut walked = 0.0;
            let mut next = draws.range(0.0, 8.0);
            for (index, pair) in line.stations.windows(2).enumerate() {
                let (a, b) = (pair[0], pair[1]);
                let span = (b.at - a.at).length();
                walked += span;
                if walked < next || (a.at - village.at).length() > reach {
                    continue;
                }
                let (x, y) = village.settled.place();
                let mut place = key.draws(
                    Stage::Plot,
                    (
                        x.wrapping_mul(1 << 20) ^ (ordinal << 8) ^ i64::from(side),
                        y.wrapping_mul(1 << 20) ^ i64::try_from(index).unwrap_or(0),
                    ),
                );
                let frontage = place.range(9.0, 18.0);
                next = walked + frontage + if place.chance(0.12) { place.range(4.0, 14.0) } else { 0.6 };
                let way = (b.at - a.at).normalized();
                let out = way.left() * sign;
                let edge = a.at + out * (0.5 * a.width + 1.0);
                let toward = 1.0 - 0.45 * (a.at - village.at).length() / reach;
                let wanted = place.range(26.0, 50.0) * toward;
                let Some(plot) = [1.0, 0.75, 0.55]
                    .into_iter()
                    .map(|share| wanted * share)
                    .filter(|&depth| depth >= 18.0)
                    .find_map(|depth| {
                        let among = Among {
                            plots: &plots,
                            green: green.as_ref(),
                            streets,
                            barred,
                            ground,
                        };
                        fitted((street, edge, way, out), (frontage, depth), &among, &mut place)
                    })
                else {
                    continue;
                };
                plots.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                plots.push(plot);
            }
        }
    }
    Ok(Village {
        settled: village.settled,
        plots,
        green,
    })
}

/// Every fencing, each as likely as another.
const FENCINGS: [Fencing; 5] = [
    Fencing::Closeboard,
    Fencing::PostAndRail,
    Fencing::Hedge,
    Fencing::Wall,
    Fencing::Open,
];

/// What a plot is fitted among: the plots laid before it, the green, the
/// village's streets, what is barred to it, and the ground.
struct Among<'a> {
    plots: &'a [Plot],
    green: Option<&'a Convex>,
    streets: &'a [(WayId, &'a Line)],
    barred: &'a [Convex],
    ground: &'a dyn Ground,
}

/// A plot `width` along the street and `depth` out from it, fronting `edge`
/// with its street running `way` and its land lying `out` of it; `None` where
/// it would overlap anything it is fitted `among`, another street or water.
fn fitted(
    (street, edge, way, out): (WayId, Point, Point, Point),
    (width, depth): (f64, f64),
    among: &Among<'_>,
    draws: &mut crate::key::Draws,
) -> Option<Plot> {
    let middle = edge + way * (0.5 * width) + out * (0.5 * depth);
    let outline = rectangle(middle, way, (width, depth));
    if among.plots.iter().any(|plot| plot.outline.overlaps(&outline))
        || among.green.is_some_and(|green| green.overlaps(&outline))
        || among.barred.iter().any(|shape| shape.overlaps(&outline))
        || outline
            .corners
            .iter()
            .chain(core::iter::once(&middle))
            .any(|&at| ground::wet(among.ground, at))
    {
        return None;
    }
    let crosses = among.streets.iter().any(|&(other, line)| {
        other != street
            && outline.corners.iter().chain(core::iter::once(&middle)).any(|&at| {
                line.nearest(at).is_some_and(|(near, station)| near.distance < 0.5 * station.width + 1.5)
            })
    });
    if crosses {
        return None;
    }
    let setback = draws.range(1.0, 6.0);
    let house_depth = draws.range(6.0, 9.0);
    let house_length = width * draws.range(0.55, 0.85);
    let offset = draws.range(-0.5, 0.5) * (width - house_length);
    let house = Footprint {
        middle: edge + way * (0.5 * width + offset) + out * (setback + 0.5 * house_depth),
        along: way,
        length: house_length,
        depth: house_depth,
        front: -out,
        standing: Standing::Dwelling,
    };
    let front = rectangle(
        edge + way * (0.5 * width) + out * (0.5 * setback),
        way,
        (width, setback),
    );
    let behind = depth - setback - house_depth;
    let back = rectangle(
        edge + way * (0.5 * width) + out * (setback + house_depth + 0.5 * behind),
        way,
        (width, behind),
    );
    let fencing = FENCINGS[draws.below(FENCINGS.len())];
    Some(Plot {
        street,
        outline,
        frontage: (edge, edge + way * width),
        house,
        front,
        back,
        fencing,
        gate: edge + way * (0.5 * width + offset),
    })
}

#[cfg(test)]
#[path = "village_tests.rs"]
mod tests;
