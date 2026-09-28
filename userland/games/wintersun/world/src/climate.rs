//! Temperature, seasons, and the rain the prevailing winds carry, over the
//! whole realm at once.
//!
//! All of it is computed over the whole coarse grid, for the same reason
//! drainage is: a rain shadow is a record of everything the wind crossed
//! before it arrived, so it cannot be answered from a window.
//!
//! # Temperature
//!
//! The realm is a band of an Earth-like planet between two latitudes. A
//! maritime surface's mean annual temperature follows latitude through the
//! profile insolation and the circulation give it — flat across the tropics,
//! falling fastest through the middle latitudes — and then an environmental
//! lapse rate with elevation, a continentality term that cools an interior
//! more the further it is from the equator, and a little keyed noise so
//! isotherms are not drawn with a ruler.
//!
//! # Seasons
//!
//! The world has no calendar, so seasonality is a property of a place, not
//! of a date. Each sample carries a seasonal range — an interior's summers
//! and winters are far apart, a coast's are not — and a rain season: whether
//! its rain falls in summer, in winter, or through the year. Those two are
//! what tell Mediterranean scrub from steppe and savanna from rainforest.
//!
//! # Rain
//!
//! Precipitation follows a real planet's belts: the equatorial rain belt,
//! the subtropical dry belts where the deserts sit, the wet westerlies and
//! the dry polar high. Four prevailing airflows carry the moisture — each
//! hemisphere's westerlies, and the easterly return flow that is its trades
//! near the equator and its polar easterlies near the pole, blowing back
//! along the westerlies' own line. Each airflow is advected by one sweep in
//! the upwind-first order hydrology's downstream-first order mirrors, and the
//! sweeps are blended by where each flow prevails.
//!
//! The belts follow the sun, so the rain is solved twice — at the June and
//! December solstices, with every belt shifted toward the summer hemisphere
//! — and a sample's rain season is how unequally the two fall. A coast on
//! the poleward flank of a dry belt is under the westerlies in winter and
//! the high in summer, and so is winter-wet; one on the equatorward flank
//! is under the rain belt in summer and the high in winter.
//!
//! Along every sweep, air leaves water saturated, loses a fraction of what
//! it carries at every step, and loses much more where it is forced to rise;
//! what is left crosses the ridge, which is why the lee is dry. A share of
//! what falls on land evaporates and transpires back into the air — most of
//! it where the land is warm — which is what carries rain a continent's
//! width inland and keeps a rainforest wet far from its coast.

use alloc::vec::Vec;

use tairix_util::mathf;
use tairix_wintersun_net::value::Facing;

use crate::error::WorldError;
use crate::geom::{quantise_u8, rise, signed, Precipitation, RainSeason, Temperature};
use crate::noise;
use crate::params::RealmParams;
use crate::realm::{clamped_index, try_filled, CoarseSample};
use crate::seed::{SeedKey, Stage};

/// A maritime surface's mean annual sea-level temperature, in degrees
/// Celsius, every ten degrees of latitude from the equator to the pole.
///
/// Earth's own profile over its oceans and coasts: level across the tropics,
/// where the Hadley cells spread the equator's heat, steepest through the
/// middle latitudes where the storm tracks carry it poleward.
const ZONAL_CELSIUS: [f64; 10] = [27.0, 27.0, 25.5, 21.5, 16.0, 10.0, 4.5, -4.0, -14.0, -22.0];

/// Environmental lapse rate, in degrees Celsius per world unit of
/// elevation — 6.5 K/km at a unit to the metre.
pub(crate) const LAPSE_RATE: f64 = 0.0065;

/// How much colder a deep interior's year runs than its coast's, at the
/// pole. It falls to nothing at the equator, where a continent's summers
/// are as much hotter as its winters are colder.
const CONTINENTALITY_COOLING: f64 = 8.0;

/// Distance from water, in coarse samples, at which continentality reaches
/// half its effect.
const CONTINENTALITY_HALF: f64 = 12.0;

/// Amplitude of the temperature jitter, in degrees.
const TEMPERATURE_JITTER: f64 = 1.6;

/// Cycles of that jitter across the realm's edge.
const JITTER_CYCLES: f64 = 6.0;

/// A deep interior's seasonal range at the equator, in degrees.
const EQUATORIAL_RANGE: f64 = 2.0;

/// What that range gains toward the pole, in degrees.
const POLAR_RANGE: f64 = 40.0;

/// The share of an interior's seasonal range a coast keeps.
const OCEANIC_RANGE: f64 = 0.35;

/// Degrees of latitude the belts follow the sun into the summer hemisphere.
const SEASON_SHIFT: f64 = 7.0;

/// Degrees either side of the equator over which the two hemispheres'
/// circulations, and their seasons, hand over.
const EQUATOR_BLEND: f64 = 6.0;

/// Latitude, from the season's thermal equator, where the trades give way
/// to the westerlies.
const TRADES_EDGE: f64 = 30.0;

/// Latitude where the westerlies give way to the polar easterlies.
const POLAR_EDGE: f64 = 60.0;

/// Degrees over which one prevailing wind gives way to the next.
const BELT_BLEND: f64 = 8.0;

/// How readily air rains on level ground, as a multiple of the baseline, by
/// degrees of latitude from the season's thermal equator: the rain belt,
/// the subtropical high, the storm tracks and the polar high.
const BELT_RAIN: [(f64, f64); 12] = [
    (0.0, 2.6),
    (8.0, 2.0),
    (15.0, 0.8),
    (20.0, 0.3),
    (24.0, 0.1),
    (30.0, 0.1),
    (36.0, 0.4),
    (44.0, 0.95),
    (52.0, 1.1),
    (62.0, 0.75),
    (72.0, 0.4),
    (90.0, 0.3),
];

/// Fraction of its moisture the air keeps crossing one coarse sample of
/// level ground.
const TRANSPORT: f64 = 0.995;

/// Rise, in world units across one coarse step, that wrings out all the
/// air can lose to orographic lift.
const OROGRAPHIC_RISE: f64 = 400.0;

/// Fraction of the carried moisture orographic lift can take at most: four
/// times what level ground takes, which is how much wetter a windward slope
/// runs than the plain below it.
const OROGRAPHIC_STRENGTH: f64 = 0.12;

/// Fraction of the carried moisture that falls on level ground anyway,
/// under a belt that neither helps nor hinders it.
const BASELINE_RAIN: f64 = 0.03;

/// Moisture the air picks up crossing one coarse sample of open water.
const EVAPORATION: f64 = 0.5;

/// Humidity of the air arriving over the realm's upwind edge.
///
/// The world does not stop at the rim, and a realm whose windward edge is
/// land would otherwise open with a desert strip that nothing in the
/// terrain explains.
const RIM_HUMIDITY: f64 = 0.55;

/// Share of what falls on frozen land that returns to the air: cold ground
/// evaporates little, which is what leaves a continent's heart a desert.
const RECYCLING_COLD: f64 = 0.1;

/// Share of what falls on land that transpires back into the air at
/// [`RECYCLING_FULL`] and above.
const RECYCLING_WARM: f64 = 0.7;

/// The season temperature, in degrees, at which recycling is full.
const RECYCLING_FULL: f64 = 25.0;

/// What saturated air drops on level ground in a year under a belt that
/// neither helps nor hinders it, in millimetres.
const LEVEL_RAIN_MILLIMETRES: f64 = 1000.0;

/// Fill in temperature, seasonal range, precipitation and rain season.
///
/// # Errors
///
/// [`WorldError::OutOfMemory`] if the working vectors do not fit.
pub fn solve(
    params: RealmParams,
    key: SeedKey,
    samples: &mut [CoarseSample],
) -> Result<(), WorldError> {
    let side = params.coarse_samples();
    let distance = water_distance(samples, side)?;
    thermal(params, key, samples, &distance);
    rain(params, samples, side)
}

/// Mean annual sea-level temperature on a maritime surface at `latitude`.
fn zonal_celsius(latitude: f64) -> f64 {
    let at = mathf::clamp(mathf::fabs(latitude) / 10.0, 0.0, 9.0);
    // The last knot is the pole itself, so a query there interpolates the
    // final span to its end rather than starting a span past the table.
    let index = usize::try_from(mathf::round_i32(mathf::floor(at)))
        .unwrap_or(0)
        .min(ZONAL_CELSIUS.len() - 2);
    #[allow(
        clippy::cast_precision_loss,
        reason = "the index is below the table's ten knots"
    )]
    let within = at - index as f64;
    crate::geom::lerp(ZONAL_CELSIUS[index], ZONAL_CELSIUS[index + 1], within)
}

/// How readily air rains on level ground `from_equator` degrees from the
/// season's thermal equator, as a multiple of the baseline.
fn belt_rain(from_equator: f64) -> f64 {
    let at = mathf::clamp(from_equator, 0.0, 90.0);
    let mut previous = BELT_RAIN[0];
    for &knot in &BELT_RAIN[1..] {
        if at <= knot.0 {
            let t = (at - previous.0) / (knot.0 - previous.0);
            return crate::geom::lerp(previous.1, knot.1, t);
        }
        previous = knot;
    }
    previous.1
}

/// `|sin(latitude)|`: zero at the equator, one at the pole.
fn poleward(latitude: f64) -> f64 {
    mathf::fabs(mathf::sin(latitude * (core::f64::consts::PI / 180.0)))
}

/// How far toward a deep interior a sample is, `0.0` on water through `1.0`.
fn inland(distance: u32) -> f64 {
    if distance == u32::MAX {
        return 1.0;
    }
    let d = f64::from(distance);
    d / (d + CONTINENTALITY_HALF)
}

/// Four-neighbour distance from each sample to the nearest open water, in
/// coarse samples.
///
/// A breadth-first flood from every water sample at once, so the result is
/// a pure function of the water mask and not of the order the sources were
/// found in.
fn water_distance(samples: &[CoarseSample], side: u32) -> Result<Vec<u32>, WorldError> {
    let mut distance = try_filled(samples.len(), u32::MAX)?;
    let mut frontier = Vec::new();
    frontier
        .try_reserve(samples.len())
        .map_err(|_| WorldError::OutOfMemory)?;

    for (index, sample) in samples.iter().enumerate() {
        if sample.is_water() {
            distance[index] = 0;
            frontier.push(index);
        }
    }

    let mut read = 0;
    while read < frontier.len() {
        let index = frontier[read];
        read += 1;
        let step = distance[index] + 1;
        let (sx, sy) = grid_of(index, side);
        for (dx, dy) in [(1_i32, 0_i32), (0, 1), (-1, 0), (0, -1)] {
            let (Some(nx), Some(ny)) = (sx.checked_add_signed(dx), sy.checked_add_signed(dy))
            else {
                continue;
            };
            if nx >= side || ny >= side {
                continue;
            }
            let next = (ny as usize) * (side as usize) + (nx as usize);
            if distance[next] <= step {
                continue;
            }
            distance[next] = step;
            frontier.push(next);
        }
    }
    Ok(distance)
}

/// The latitude of coarse row `sy`, in degrees north.
fn row_latitude(params: RealmParams, sy: u32) -> f64 {
    let span = f64::from(params.coarse_samples().saturating_sub(1)).max(1.0);
    params.latitude_at(f64::from(sy) / span)
}

/// Latitude, lapse rate, continentality, jitter; and the seasonal range.
fn thermal(params: RealmParams, key: SeedKey, samples: &mut [CoarseSample], distance: &[u32]) {
    let side = params.coarse_samples();
    let span = f64::from(side.saturating_sub(1)).max(1.0);

    for (index, sample) in samples.iter_mut().enumerate() {
        let (sx, sy) = grid_of(index, side);
        let (u, v) = (f64::from(sx) / span, f64::from(sy) / span);
        let latitude = row_latitude(params, sy);
        let reach = poleward(latitude);
        let interior = inland(distance[index]);

        let altitude = mathf::fmax(sample.elevation.units(), 0.0) * LAPSE_RATE;
        let jitter = noise::fbm(key, Stage::Climate, u * JITTER_CYCLES, v * JITTER_CYCLES)
            * TEMPERATURE_JITTER;
        let mean =
            zonal_celsius(latitude) - interior * CONTINENTALITY_COOLING * reach * reach - altitude
                + jitter;
        let range = (EQUATORIAL_RANGE + POLAR_RANGE * reach * mathf::sqrt(reach))
            * (OCEANIC_RANGE + (1.0 - OCEANIC_RANGE) * interior);

        sample.temperature = Temperature::from_celsius(mean);
        sample.range = Temperature::from_celsius(range);
        sample.continentality = quantise_u8(interior * f64::from(u8::MAX));
    }
}

/// A solstice: the belts shifted toward one hemisphere's summer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Season {
    /// The belts shifted north.
    June,
    /// The belts shifted south.
    December,
}

impl Season {
    const BOTH: [Self; 2] = [Self::June, Self::December];

    /// Degrees north the belts stand this season.
    const fn shift(self) -> f64 {
        match self {
            Self::June => SEASON_SHIFT,
            Self::December => -SEASON_SHIFT,
        }
    }

    /// `1.0` where this season is summer, `-1.0` where it is winter, and a
    /// handover through zero across the equator, which has neither.
    fn summer(self, latitude: f64) -> f64 {
        let north = mathf::clamp(latitude / EQUATOR_BLEND, -1.0, 1.0);
        match self {
            Self::June => north,
            Self::December => -north,
        }
    }
}

/// One prevailing airflow: where it blows, and where it prevails.
#[derive(Copy, Clone, Debug)]
struct Airflow {
    /// The unit vector it blows along.
    heading: (f64, f64),
    /// Whether it is the northern hemisphere's.
    northern: bool,
    /// Whether it is a westerly rather than the easterly return flow.
    westerly: bool,
}

/// The four prevailing airflows the westerlies decide.
///
/// The southern hemisphere's westerlies are the northern's mirrored across
/// the equator's line, so each hemisphere's blow poleward; each
/// hemisphere's trades and polar easterlies blow back along its westerlies'
/// line, which is the return flow that sets them converging on the
/// equatorial rain belt.
fn airflows(westerlies: Facing) -> [Airflow; 4] {
    let (x, y) = westerlies.unit_vector();
    [
        Airflow {
            heading: (x, y),
            northern: true,
            westerly: true,
        },
        Airflow {
            heading: (-x, -y),
            northern: true,
            westerly: false,
        },
        Airflow {
            heading: (x, -y),
            northern: false,
            westerly: true,
        },
        Airflow {
            heading: (-x, y),
            northern: false,
            westerly: false,
        },
    ]
}

/// How much of the air at `from_equator` degrees north of the season's
/// thermal equator is `flow`. The four flows' shares sum to one everywhere.
fn prevalence(flow: Airflow, from_equator: f64) -> f64 {
    let north = rise(from_equator, 0.0, 2.0 * EQUATOR_BLEND);
    let hemisphere = if flow.northern { north } else { 1.0 - north };
    let off = mathf::fabs(from_equator);
    let westerly = rise(off, TRADES_EDGE, BELT_BLEND) * (1.0 - rise(off, POLAR_EDGE, BELT_BLEND));
    hemisphere
        * if flow.westerly {
            westerly
        } else {
            1.0 - westerly
        }
}

/// Every sample's precipitation and rain season, from both seasons' sweeps
/// of every airflow that prevails somewhere in the realm.
fn rain(params: RealmParams, samples: &mut [CoarseSample], side: u32) -> Result<(), WorldError> {
    let area = samples.len();
    let mut latitudes = try_filled(side as usize, 0.0_f64)?;
    for (sy, latitude) in (0..side).zip(latitudes.iter_mut()) {
        *latitude = row_latitude(params, sy);
    }

    let mut june = try_filled(area, 0.0_f64)?;
    let mut december = try_filled(area, 0.0_f64)?;
    // What the air is carrying, as distinct from what fell out of it: a
    // sample's rain reads the one, the sample downwind the other.
    let mut carried = try_filled(area, 0.0_f64)?;

    for flow in airflows(params.westerlies()) {
        let prevails = Season::BOTH.iter().any(|season| {
            latitudes
                .iter()
                .any(|&latitude| prevalence(flow, latitude - season.shift()) > 0.0)
        });
        if !prevails {
            continue;
        }
        let order = wind_order(area, side, flow.heading.0, flow.heading.1)?;
        let upwind = (step_of(flow.heading.0), step_of(flow.heading.1));
        for season in Season::BOTH {
            let totals = match season {
                Season::June => &mut june,
                Season::December => &mut december,
            };
            let sweep = Sweep {
                samples,
                latitudes: &latitudes,
                side,
                flow,
                season,
                upwind,
            };
            sweep.run(&order, &mut carried, totals);
        }
    }

    for (index, sample) in samples.iter_mut().enumerate() {
        let (_, sy) = grid_of(index, side);
        let latitude = latitudes[sy as usize];
        let (summer, winter) = if latitude >= 0.0 {
            (june[index], december[index])
        } else {
            (december[index], june[index])
        };
        let total = summer + winter;
        let contrast = if total > 0.0 {
            (summer - winter) / total
        } else {
            0.0
        };
        let seasonal = contrast * mathf::clamp(mathf::fabs(latitude) / EQUATOR_BLEND, 0.0, 1.0);
        sample.precipitation = Precipitation::from_millimetres(total / 2.0);
        sample.rain_season = RainSeason::from_fraction(seasonal);
    }
    Ok(())
}

/// One airflow's advection in one season.
struct Sweep<'a> {
    samples: &'a [CoarseSample],
    latitudes: &'a [f64],
    side: u32,
    flow: Airflow,
    season: Season,
    upwind: (i32, i32),
}

impl Sweep<'_> {
    /// Advect the air along `order`, adding this flow's share of what falls
    /// at each sample to `totals`, in millimetres.
    fn run(&self, order: &[u32], carried: &mut [f64], totals: &mut [f64]) {
        for &raw in order {
            let index = raw as usize;
            let (sx, sy) = grid_of(index, self.side);
            let latitude = self.latitudes[sy as usize];
            let from_equator = latitude - self.season.shift();
            let belt = belt_rain(mathf::fabs(from_equator));
            let here = self.samples[index];
            let source = clamped_index(
                signed(sx) - self.upwind.0,
                signed(sy) - self.upwind.1,
                self.side,
            );

            let fell = if here.is_water() {
                // Open water saturates the air crossing it. What rains on it
                // is recorded, so a coast interpolates toward the sea's rain
                // rather than toward none, and the sea gives it back.
                let upstream = if source == index {
                    RIM_HUMIDITY
                } else {
                    carried[source]
                };
                carried[index] = mathf::fmin(upstream + EVAPORATION, 1.0);
                carried[index] * BASELINE_RAIN * belt
            } else {
                // A sample on the upwind rim has no modelled neighbour to
                // take air from — its source clamps onto itself — so it is
                // given air arriving over the realm's edge rather than none.
                let incoming = if source == index {
                    RIM_HUMIDITY
                } else {
                    carried[source] * TRANSPORT
                };
                let climb = here.elevation.units() - self.samples[source].elevation.units();
                let lift = mathf::clamp(climb / OROGRAPHIC_RISE, 0.0, 1.0);
                let share = mathf::fmin(BASELINE_RAIN * belt + lift * OROGRAPHIC_STRENGTH, 1.0);
                let fell = incoming * share;
                let celsius = here.temperature.celsius()
                    + here.range.celsius() / 2.0 * self.season.summer(latitude);
                let recycled = crate::geom::lerp(
                    RECYCLING_COLD,
                    RECYCLING_WARM,
                    mathf::clamp(celsius / RECYCLING_FULL, 0.0, 1.0),
                );
                carried[index] = incoming - fell * (1.0 - recycled);
                fell
            };
            totals[index] +=
                prevalence(self.flow, from_equator) * fell / BASELINE_RAIN * LEVEL_RAIN_MILLIMETRES;
        }
    }
}

/// The grid position of a row-major index.
fn grid_of(index: usize, side: u32) -> (u32, u32) {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the index is below the grid's area, so each component is \
                  below MAX_COARSE_SAMPLES"
    )]
    {
        (
            (index % (side as usize)) as u32,
            (index / (side as usize)) as u32,
        )
    }
}

/// One axis of a wind direction, as the grid step it favours.
fn step_of(component: f64) -> i32 {
    /// Below this the wind is treated as having no component on the axis,
    /// so a near-axial wind advects along one row rather than staggering.
    const DEADBAND: f64 = 0.3827;
    if component > DEADBAND {
        1
    } else if component < -DEADBAND {
        -1
    } else {
        0
    }
}

/// Cell indices ordered by their projection onto the wind, upwind first.
///
/// The projection is quantised to an integer before sorting, for the same
/// reason the flood's key is: an `f64` comparator has no total order, and a
/// sort that pretends otherwise depends on the input order. The index is
/// the tiebreak, so equal projections resolve the same way everywhere.
fn wind_order(area: usize, side: u32, wx: f64, wy: f64) -> Result<Vec<u32>, WorldError> {
    /// Steps per coarse sample in the projection key.
    const STEPS: f64 = 4096.0;

    let mut keyed = try_filled(area, (0_i64, 0_u32))?;
    for (index, slot) in keyed.iter_mut().enumerate() {
        #[allow(
            clippy::cast_precision_loss,
            reason = "a grid coordinate is below MAX_COARSE_SAMPLES"
        )]
        let (x, y) = (
            (index % (side as usize)) as f64,
            (index / (side as usize)) as f64,
        );
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the projection is bounded by the grid's diagonal times \
                      STEPS, far inside i64"
        )]
        let key = mathf::round((x * wx + y * wy) * STEPS) as i64;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the area is at most MAX_COARSE_SAMPLES squared"
        )]
        {
            *slot = (key, index as u32);
        }
    }
    keyed.sort_unstable();

    let mut order = Vec::new();
    order
        .try_reserve_exact(area)
        .map_err(|_| WorldError::OutOfMemory)?;
    order.extend(keyed.iter().map(|&(_, index)| index));
    Ok(order)
}

#[cfg(test)]
mod tests;
