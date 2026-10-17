//! A bank's light against light followed through its cloud as it is: photons
//! drawn in from the sun and the sky, scattered through the cloud's full
//! phase, given back by the ground, and counted each time one crosses the
//! floor going down, so the light a deck's base sends down is measured with
//! no volume to hold it. The bank's own reading of the same base, at the
//! scenes' detail, is held to it.

extern crate std;

use alloc::vec::Vec;
use core::f64::consts::{PI, TAU};
use std::sync::Mutex;

use tairix_parallel::JobRunner;

use super::*;
use crate::cloud::form::Edges;
use crate::cloud::{Reader, BACK_LOBE, DROPLET_LOBES};
use crate::detail::Detail;
use crate::vector::cell_of;

/// The two tallies a followed photon is counted in, by where it came from.
const FROM_SUN: usize = 0;
const FROM_SKY: usize = 1;

/// The cosine within which of the zenith the light coming down is counted.
const CONE: f64 = 0.966;

/// The finest footprint the cloud is read at, as an eye a kilometre below a
/// base sees it.
const FOOTPRINT: f64 = 4.0;

/// The most steps a followed photon takes.
const MOST_STEPS: u32 = 2_000_000;

/// A bank of `deck` lit from `sun` at the scenes' *Simple* detail, the
/// ground given the sunlight that falls on it in the open.
fn lit_as_scenes_are(deck: Deck, sun: Vec3) -> Cloudbank {
    let resolution = Detail::Simple.densities().clouds.low;
    let mut bank = Cloudbank::new(
        [Some(deck), None],
        (4000.0, resolution),
        sun,
        GROUND * 1000.0,
    )
    .expect("fits");
    bank.stand(Vec3::ZERO, SCREEN_PIXEL).expect("levels fit");
    let cosines = bank.sun_cosines(sun);
    bank.light_by(Lighting {
        sunlight: alloc::vec![Vec3::splat(SUNLIGHT); SUNLIGHT_LEVELS * SUN_COSINES],
        cosines,
        toward: sun,
        above: SKY,
        ground: (Vec3::splat(SUNLIGHT * sun.y), ALBEDO),
    });
    while !bank
        .step(&tairix_parallel::Threaded::new(8), None)
        .expect("held")
    {}
    bank
}

/// What a photon is followed down through: the bank, read as finely as an
/// eye a kilometre below its base sees it, against the most extinction any
/// of its cloud holds; the share of what it meets that scatters; the
/// level's square it wraps round; and the grid of `cells` a side its
/// crossings of the floor are counted on.
struct Descent<'a> {
    bank: &'a Cloudbank,
    reader: Reader<'a>,
    densest: f64,
    survives: f64,
    half: f64,
    cells: usize,
}

impl Descent<'_> {
    /// The cloud's extinction at `(x, altitude, z)`, held to its bound.
    fn extinction(&self, (x, altitude, z): (f64, f64, f64)) -> f64 {
        if !self.bank.may_stand((x, z), altitude) {
            return 0.0;
        }
        let sampled = self
            .reader
            .density_at(
                point(self.bank, (x, altitude, z)),
                altitude,
                Edges::Resolved(FOOTPRINT),
            )
            .map_or(0.0, |sample| sample.extinction);
        assert!(
            sampled <= self.densest,
            "{sampled} past the bound {}",
            self.densest
        );
        sampled
    }

    /// The cell of the grid `(x, z)` lies in, the square wrapped round.
    fn cell(&self, x: f64, z: f64) -> usize {
        let side = 2.0 * self.half;
        let index = |along: f64| {
            let at = around(along + self.half, side) / side * real(self.cells);
            (cell_of(at).0).min(self.cells - 1)
        };
        index(z) * self.cells + index(x)
    }

    /// Photon `index` of `kind` followed in from the ceiling under `sun`
    /// over ground of `albedo` until it leaves by the ceiling or is played
    /// for and lost, each crossing of the floor within `CONE` of the zenith
    /// going down counted in `tally`.
    fn follow(&self, (kind, index): (usize, u32), (sun, albedo): (Vec3, f64), tally: &mut [f64]) {
        let (floor, ceiling) = (self.bank.floor, self.bank.ceiling);
        let (half, side) = (self.half, 2.0 * self.half);
        let mut draws = Draws::new(
            0x5eed ^ u32::try_from(kind).expect("a kind"),
            u64::from(index),
        );
        let (mut x, mut z) = ((draws.next() - 0.5) * side, (draws.next() - 0.5) * side);
        let mut altitude = ceiling;
        let mut way = if kind == FROM_SUN {
            -sun
        } else {
            let up = lambertian(&mut draws);
            Vec3::new(up.x, -up.y, up.z)
        };
        let mut weight = 1.0;
        for _ in 0..MOST_STEPS {
            let reach = -mathf::ln((1.0 - draws.next()).max(1e-300)) / self.densest;
            let next = altitude + way.y * reach;
            if next >= ceiling {
                return;
            }
            if next < floor {
                // Down through the floor: counted, then to the ground and
                // back up as it reflects.
                let fall = (altitude - floor) / -way.y;
                x += way.x * fall;
                z += way.z * fall;
                if -way.y >= CONE {
                    tally[self.cell(x, z)] += weight / -way.y;
                }
                let down = floor / -way.y;
                weight *= albedo;
                if weight < 0.05 {
                    if draws.next() * 0.05 >= weight {
                        return;
                    }
                    weight = 0.05;
                }
                let up = lambertian(&mut draws);
                let rise = floor / up.y.max(1e-9);
                x += way.x * down + up.x * rise;
                z += way.z * down + up.z * rise;
                (altitude, way) = (floor + 1e-6, up);
                continue;
            }
            x += way.x * reach;
            z += way.z * reach;
            x = around(x + half, side) - half;
            z = around(z + half, side) - half;
            altitude = next;
            if draws.next() * self.densest < self.extinction((x, altitude, z)) {
                weight *= self.survives;
                let g = if draws.next() < DROPLET_LOBES.1 {
                    BACK_LOBE
                } else {
                    DROPLET_LOBES.0
                };
                way = scattered(way, g, &mut draws);
            }
        }
    }
}

/// The light coming down through each cell of a `cells`-a-side grid over
/// the bank's finest light square within `CONE` of the zenith, followed from
/// `photons` of the sun's and as many of the sky's: per unit of the sun's
/// light, and per unit of the sky's.
fn followed(
    bank: &Cloudbank,
    (sun, albedo): (Vec3, f64),
    photons: u32,
    cells: usize,
) -> [Vec<f64>; 2] {
    let (floor, ceiling) = (bank.floor, bank.ceiling);
    let descent = Descent {
        bank,
        reader: Reader { bank },
        densest: bank
            .decks
            .iter()
            .flatten()
            .map(|deck| {
                deck.thickness * mathf::exp(2.0 / 3.0 * mathf::ln((ceiling - floor) / 1000.0))
            })
            .fold(0.0, f64::max),
        survives: bank.scattering().albedo,
        half: bank.lights.volumes[0].grid.half,
        cells,
    };
    let solid = TAU * (1.0 - CONE);
    let jobs = 64u32;
    let tallies = Mutex::new([
        alloc::vec![0.0; cells * cells],
        alloc::vec![0.0; cells * cells],
    ]);
    tairix_parallel::Threaded::new(16).run(usize::try_from(jobs).expect("few"), &|job| {
        let mut own = [
            alloc::vec![0.0; cells * cells],
            alloc::vec![0.0; cells * cells],
        ];
        let job = u32::try_from(job).expect("few");
        for kind in [FROM_SUN, FROM_SKY] {
            for index in (job..photons).step_by(usize::try_from(jobs).expect("few")) {
                descent.follow((kind, index), (sun, albedo), &mut own[kind]);
            }
        }
        let mut shared = tallies.lock().expect("unpoisoned");
        for kind in [FROM_SUN, FROM_SKY] {
            for (total, mine) in shared[kind].iter_mut().zip(&own[kind]) {
                *total += mine;
            }
        }
    });
    let [sun_tally, sky_tally] = tallies.into_inner().expect("unpoisoned");
    let cells_area = real(cells * cells);
    let per = |tally: Vec<f64>, power: f64| -> Vec<f64> {
        tally
            .into_iter()
            .map(|sum| sum * power * cells_area / (f64::from(photons) * solid))
            .collect()
    };
    [per(sun_tally, sun.y), per(sky_tally, PI)]
}

/// The light coming down through each cell's middle within `CONE` of the
/// zenith as a ray from the ground reads the bank, the sky behind its cloud
/// showing as far as the cloud lets it, in luminance.
fn read(bank: &Cloudbank, cells: usize) -> Vec<f64> {
    let half = bank.lights.volumes[0].grid.half;
    let side = 2.0 * half;
    let (eyes, ways) = (4u32, 4u32);
    (0..cells * cells)
        .map(|index| {
            let (column, row) = (index % cells, index / cells);
            let key = u32::try_from(index).unwrap_or(0);
            let mut summed = 0.0;
            for eye in 0..eyes * eyes {
                let (across, down) = (
                    (f64::from(eye % eyes) + unit(mix32(key ^ eye))) / f64::from(eyes),
                    (f64::from(eye / eyes) + unit(mix32(key ^ eye ^ 0x51ed))) / f64::from(eyes),
                );
                let (x, z) = (
                    -half + (real(column) + across) / real(cells) * side,
                    -half + (real(row) + down) / real(cells) * side,
                );
                let origin = point(bank, (x, 0.0, z));
                for way in 0..ways {
                    let draw = key.wrapping_mul(31).wrapping_add(eye * ways + way);
                    let rise = 1.0 - (1.0 - CONE) * unit(mix32(draw ^ 0x7a11));
                    let level = mathf::sqrt((1.0 - rise * rise).max(0.0));
                    let turn = TAU * unit(mix32(draw ^ 0x2b3d));
                    let dir = Vec3::new(level * mathf::cos(turn), rise, level * mathf::sin(turn));
                    let jitter = unit(mix32(draw ^ 0x9c4f));
                    summed += bank
                        .seen(origin, dir, Some(SCREEN_PIXEL), jitter)
                        .and_then(|seen| seen.cloud)
                        .map_or(SKY.luminance(), |(light, kept, _)| {
                            light.luminance() + kept * SKY.luminance()
                        });
                }
            }
            summed / f64::from(eyes * eyes * ways)
        })
        .collect()
}

/// The light followed through a deck down to its base, in luminance.
fn followed_light(bank: &Cloudbank, sun: Vec3, photons: u32, cells: usize) -> Vec<f64> {
    let [from_sun, from_sky] = followed(bank, (sun, ALBEDO), photons, cells);
    from_sun
        .iter()
        .zip(&from_sky)
        .map(|(sun_share, sky_share)| {
            (Vec3::splat(SUNLIGHT) * *sun_share + SKY * *sky_share).luminance()
        })
        .collect()
}

/// A ceiling of cloud a few tens deep.
const CEILING: Deck = Deck {
    thickness: 0.03,
    depth: (900.0, 1000.0),
    ..OVERCAST
};

/// Heaps a few hundred metres to a kilometre deep, a third of the sky open
/// between them.
const BROKEN: Deck = Deck {
    cover: 0.65,
    heap: 0.55,
    thickness: 0.06,
    depth: (300.0, 1000.0),
    scale: 3000.0,
    billow: 450.0,
    ..CEILING
};

/// The light a bank of `deck` lit from a high sun sends down through its
/// floor, as a ray from the ground reads it and as the light followed through
/// its cloud from `photons` of the sun's and the sky's lets it down, each
/// over `cells` a side of the bank's finest light square.
fn base_read_and_followed(deck: Deck, photons: u32, cells: usize) -> (f64, f64) {
    let sun = high_sun();
    let bank = lit_as_scenes_are(deck, sun);
    let mean = |values: Vec<f64>| values.iter().sum::<f64>() / real(cells * cells);
    (
        mean(read(&bank, cells)),
        mean(followed_light(&bank, sun, photons, cells)),
    )
}

/// An unbroken overcast's base reads as the light followed through it lets
/// down.
#[test]
fn an_overcasts_base_reads_as_the_light_followed_through_it() {
    let (read, followed) = base_read_and_followed(CEILING, 200_000, 8);
    assert!(
        (read / followed - 1.0).abs() < 0.05,
        "read {read} against {followed}"
    );
}

/// Under broken heaps the base, gaps and walls together, reads as the light
/// followed through them lets down.
#[test]
fn a_broken_decks_base_reads_as_the_light_followed_through_it() {
    let (read, followed) = base_read_and_followed(BROKEN, 400_000, 16);
    assert!(
        (read / followed - 1.0).abs() < 0.12,
        "read {read} against {followed}"
    );
}
