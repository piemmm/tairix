//! Mud the sun dried and cracked: a square tile of the plates it split into,
//! each curling up at its rim as its face shrank faster than the mud beneath,
//! undercut below the curl and parted from its neighbours by a crack as wide
//! as the crust is thick.
//!
//! The plates are the cells of a Voronoi pattern over feature points jittered
//! one to a lattice cell, wrapping at the tile's edges. Every tile shares the
//! points in a band two cells deep along its edges; only its interior points
//! are its own. A plate belongs to the tile its point lies in and keeps its
//! whole shape where it runs over an edge, and its neighbours across that edge
//! are band points every tile agrees on, so any two tiles laid side by side
//! meet in whole plates without a seam.

use alloc::vec::Vec;

use tairix_util::mathf;

use crate::noise::{cells2, hash2, noise3, smoothstep};
use crate::pigment::Spot;
use crate::prototype::{Assembly, Building, Mapping};
use crate::sample::{mix32, unit};
use crate::vector::{power, single, Vec3};

/// A crust of dried mud: the side of the square tile it is laid in, the
/// plates it splits into across that side, and how thick it dried — the
/// thicker, the wider its cracks.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Crust {
    pub(crate) side: f64,
    pub(crate) plates: u16,
    pub(crate) thick: f64,
}

/// The fewest plates across a tile: fewer and a plate's neighbours two cells
/// over would be its own images across the wrap.
pub(crate) const FEWEST_PLATES: u16 = 6;
/// How deep the band of feature points every tile shares runs in from each
/// edge, in lattice cells.
const BAND: u32 = 2;
/// How far a feature point is jittered from its cell's middle, as a share of
/// the cell.
const JITTER: f64 = 0.8;
/// How much of its size a plate's corners curl up, the least and the most.
const CURL: (f64, f64) = (0.01, 0.11);
/// The rings a plate's face is laid in, as shares of the way out from its
/// middle to its rim, the rim last.
const RINGS: [f64; 4] = [0.3, 0.6, 0.85, 1.0];
/// The pieces each straight side of a plate's polygon is cut into before it
/// is bent.
const PIECES: usize = 2;
/// How far a crack wanders off its straight line, as a share of a cell, and
/// how many of its wanders span a cell.
const WANDER: f64 = 0.12;
const WANDERS: u32 = 2;
/// How far below the ground a plate's foot reaches, so no gap shows under
/// it on ground not quite level.
const FOOTING: f64 = 0.003;
/// The most corners a plate's polygon is clipped to: a square and a corner
/// for every neighbour within two cells.
const CORNERS: usize = 32;
/// The most points round a plate's rim once its sides are cut in pieces.
const RIM: usize = CORNERS * PIECES;

/// A plate's polygon as it is clipped, its corners in turn about it.
#[derive(Copy, Clone, Debug)]
struct Polygon {
    corners: [(f64, f64); CORNERS],
    count: usize,
}

impl Polygon {
    /// The square `half` either way of `centre`.
    fn square(centre: (f64, f64), half: f64) -> Self {
        let mut corners = [(0.0, 0.0); CORNERS];
        for (corner, (dx, dz)) in
            corners
                .iter_mut()
                .zip([(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)])
        {
            *corner = (centre.0 + half * dx, centre.1 + half * dz);
        }
        Self { corners, count: 4 }
    }

    fn corners(&self) -> &[(f64, f64)] {
        self.corners.get(..self.count).unwrap_or(&[])
    }

    /// What of it lies where `(x, z)·way <= reach` (Sutherland and Hodgman,
    /// "Reentrant polygon clipping", 1974): `None` once nothing does, or
    /// too many corners would.
    fn clipped(&self, way: (f64, f64), reach: f64) -> Option<Self> {
        let inside = |(x, z): (f64, f64)| x * way.0 + z * way.1 - reach;
        let mut kept = Self {
            corners: [(0.0, 0.0); CORNERS],
            count: 0,
        };
        let corners = self.corners();
        let mut push = |corner: (f64, f64)| -> Option<()> {
            *kept.corners.get_mut(kept.count)? = corner;
            kept.count += 1;
            Some(())
        };
        for (index, &here) in corners.iter().enumerate() {
            let next = *corners.get((index + 1) % corners.len())?;
            let (from, to) = (inside(here), inside(next));
            if from <= 0.0 {
                push(here)?;
            }
            if (from <= 0.0) != (to <= 0.0) {
                let share = from / (from - to);
                push((
                    here.0 + (next.0 - here.0) * share,
                    here.1 + (next.1 - here.1) * share,
                ))?;
            }
        }
        (kept.count >= 3).then_some(kept)
    }
}

/// The feature points of one tile among those of a seed.
struct Lattice {
    plates: u32,
    cell: f64,
    side: f64,
    seed: u32,
    variant: u32,
}

impl Lattice {
    /// Lattice cell `(column, row)`, wrapped onto the tile: its own point,
    /// shared with every tile where it lies in the band along an edge.
    fn point(&self, (column, row): (i64, i64)) -> ((f64, f64), u32) {
        let plates = i64::from(self.plates);
        let (wrapped_column, wrapped_row) = (column.rem_euclid(plates), row.rem_euclid(plates));
        let (i, j) = (
            u32::try_from(wrapped_column).unwrap_or(0),
            u32::try_from(wrapped_row).unwrap_or(0),
        );
        let banded = i < BAND || j < BAND || i >= self.plates - BAND || j >= self.plates - BAND;
        let salt = if banded {
            self.seed
        } else {
            self.seed ^ mix32(self.variant.wrapping_add(0x51))
        };
        let id = hash2(i, j, salt);
        let offset = |whole: i64, wrapped: i64| {
            f64::from(i32::try_from((whole - wrapped) / plates).unwrap_or(0)) * self.side
        };
        let jitter = |bits: u32| JITTER * (unit(bits) - 0.5);
        (
            (
                (f64::from(i) + 0.5 + jitter(id)) * self.cell + offset(column, wrapped_column),
                (f64::from(j) + 0.5 + jitter(mix32(id))) * self.cell + offset(row, wrapped_row),
            ),
            id,
        )
    }

    /// The plate of the point in cell `(column, row)`, its cracks cut
    /// `width` wide about it at their widest.
    fn plate(&self, (column, row): (i64, i64), width: f64) -> Option<Polygon> {
        let (point, id) = self.point((column, row));
        let mut plate = Polygon::square(point, 3.0 * self.cell);
        let reach = i64::from(BAND);
        for dz in -reach..=reach {
            for dx in -reach..=reach {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let (other, other_id) = self.point((column + dx, row + dz));
                let (ox, oz) = (other.0 - point.0, other.1 - point.1);
                let apart = mathf::hypot(ox, oz);
                if apart < 1e-9 {
                    continue;
                }
                let way = (ox / apart, oz / apart);
                // The crack between two plates is as wide seen from either,
                // a few opening wide and most hairlines.
                let pair = mix32(id.min(other_id) ^ other_id.max(id).rotate_left(16));
                let opened = unit(pair);
                let crack = width * (0.15 + 0.85 * opened * opened);
                let reach = (point.0 * way.0 + point.1 * way.1) + 0.5 * apart - 0.5 * crack;
                plate = plate.clipped(way, reach)?;
            }
        }
        Some(plate)
    }

    /// How far the cracks' wandering carries the point `(x, z)` of the tile:
    /// a smooth field repeating with the tile, so a crack bends alike on
    /// either side of it and across the tile's edges.
    fn wander(&self, (x, z): (f64, f64)) -> (f64, f64) {
        let period = self.plates * WANDERS;
        let scale = f64::from(period) / self.side;
        let amount = WANDER * self.cell;
        let at = (x * scale, z * scale);
        (
            amount * periodic(at, period, self.seed ^ 0x7761),
            amount * periodic(at, period, self.seed ^ 0x7762),
        )
    }
}

/// Smooth value noise over the plane, `-1.0..=1.0`, repeating every
/// `period` of its cells either way.
fn periodic((x, z): (f64, f64), period: u32, seed: u32) -> f64 {
    let period = i64::from(period.max(1));
    let (column, row) = (mathf::floor(x), mathf::floor(z));
    let (across, down) = (
        smoothstep(0.0, 1.0, x - column),
        smoothstep(0.0, 1.0, z - row),
    );
    let lattice = |offset: f64| {
        let whole = mathf::round_i32(offset);
        u32::try_from(i64::from(whole).rem_euclid(period)).unwrap_or(0)
    };
    let value =
        |dx: f64, dz: f64| 2.0 * unit(hash2(lattice(column + dx), lattice(row + dz), seed)) - 1.0;
    let near = value(0.0, 0.0) + (value(1.0, 0.0) - value(0.0, 0.0)) * across;
    let far = value(0.0, 1.0) + (value(1.0, 1.0) - value(0.0, 1.0)) * across;
    near + (far - near) * down
}

/// The tile of `crust` drawn under `variant` among the tiles of `seed`,
/// about its own middle, in `material`; its hierarchy still to build.
/// `None` when the heap will not hold it.
pub(crate) fn tile(crust: Crust, material: u16, (seed, variant): (u32, u32)) -> Option<Building> {
    let plates = u32::from(crust.plates.max(FEWEST_PLATES));
    let cell = crust.side / f64::from(plates);
    let lattice = Lattice {
        plates,
        cell,
        side: crust.side,
        seed,
        variant,
    };
    let width = crack_width(crust.thick);
    let count = usize::try_from(plates * plates).ok()?;
    // A plate's rim takes about six sides' points.
    let rim = 6 * PIECES;
    let parts = (2 + 2 * RINGS.len()) * rim;
    let vertices = 1 + (RINGS.len() + 1) * rim;
    let mut assembly = Assembly::with_room(count * parts, count * vertices)?;
    let half = 0.5 * crust.side;
    for row in 0..i64::from(plates) {
        for column in 0..i64::from(plates) {
            let Some(plate) = lattice.plate((column, row), width) else {
                continue;
            };
            let (_, id) = lattice.point((column, row));
            lay(
                &mut assembly,
                (&lattice, &plate, half),
                (crust.thick, material, id),
            )?;
        }
    }
    assembly.finish()
}

/// How wide a crust `thick` cracks at its widest.
pub(crate) fn crack_width(thick: f64) -> f64 {
    0.002 + 0.35 * thick
}

/// Lay the plate `plate` of `lattice`'s tile, the tile's middle `half` along
/// either way from its corner, `thick` thick, in `material` and keyed `key`:
/// its sides cut in pieces and bent as the cracks wander, its face curled up
/// toward its rim, most at its corners, and dished at its middle, and its
/// wall run down beneath the curl, undercut.
fn lay(
    assembly: &mut Assembly,
    (lattice, plate, half): (&Lattice, &Polygon, f64),
    (thick, material, key): (f64, u16, u32),
) -> Option<()> {
    let corners = plate.corners();
    let mut rim = [(0.0, 0.0); RIM];
    let mut sides = 0;
    for (index, &corner) in corners.iter().enumerate() {
        let next = *corners.get((index + 1) % corners.len())?;
        for piece in 0..PIECES {
            let share =
                f64::from(u32::try_from(piece).ok()?) / f64::from(u32::try_from(PIECES).ok()?);
            let at = (
                corner.0 + (next.0 - corner.0) * share,
                corner.1 + (next.1 - corner.1) * share,
            );
            let (dx, dz) = lattice.wander(at);
            *rim.get_mut(sides)? = (at.0 + dx, at.1 + dz);
            sides += 1;
        }
    }
    let rim = rim.get(..sides)?;
    let count = f64::from(u32::try_from(sides).ok()?);
    let middle = rim.iter().fold((0.0, 0.0), |(sx, sz), &(x, z)| {
        (sx + x / count, sz + z / count)
    });
    let reach = |(x, z): (f64, f64)| mathf::hypot(x - middle.0, z - middle.1);
    let size = rim.iter().copied().map(reach).fold(1e-9, f64::max);
    // Most plates curl a little, a few much.
    let curled = unit(mix32(key ^ 0xc0));
    let lift = size * (CURL.0 + (CURL.1 - CURL.0) * curled * curled);
    let undercut = 0.25 * thick + 0.6 * lift;
    let rings = RINGS.len();
    let mut points = Vec::new();
    points.try_reserve_exact(1 + (rings + 1) * sides).ok()?;
    let mut coords = Vec::new();
    coords.try_reserve_exact(1 + (rings + 1) * sides).ok()?;
    let at = |(x, z): (f64, f64), y: f64| Vec3::new(x - half, y, z - half);
    let dish = 0.06 * lift;
    points.push(at(middle, thick - dish));
    coords.push([single(size), 0.0]);
    let toward = |(x, z): (f64, f64), share: f64| {
        (
            middle.0 + (x - middle.0) * share,
            middle.1 + (z - middle.1) * share,
        )
    };
    for share in RINGS {
        for &point in rim {
            let cornered = power(reach(point) / size, 1.5);
            let height = thick + lift * cornered * power(share, 2.5) - dish * (1.0 - share);
            points.push(at(toward(point, share), height));
            coords.push([single((1.0 - share) * size), 0.0]);
        }
    }
    for &point in rim {
        let out = reach(point).max(1e-9);
        let cornered = power(out / size, 1.5);
        let under = undercut * (0.5 + 0.5 * cornered);
        points.push(at(toward(point, (1.0 - under / out).max(0.5)), -FOOTING));
        coords.push([0.0, single(size)]);
    }
    let ring = |ring: usize, corner: usize| {
        u32::try_from(1 + ring * sides + corner % sides).unwrap_or(u32::MAX)
    };
    let mut faces = Vec::new();
    faces.try_reserve_exact((1 + 2 * rings) * sides).ok()?;
    for corner in 0..sides {
        faces.push(([0, ring(0, corner + 1), ring(0, corner)], material));
        for band in 0..rings {
            let (a, b) = (ring(band, corner), ring(band, corner + 1));
            let (c, d) = (ring(band + 1, corner + 1), ring(band + 1, corner));
            faces.push(([a, b, c], material));
            faces.push(([a, c, d], material));
        }
    }
    assembly.mesh_mapped(
        &points,
        &faces,
        Mapping {
            coords: &coords,
            key,
            size,
            trim: None,
        },
    )
}

/// Dried mud's colours: its face bleached pale and dry, and the damp mud
/// beneath it its crack walls show.
#[derive(Clone, Debug)]
pub(crate) struct Mud {
    pub(crate) dry: Vec3,
    pub(crate) damp: Vec3,
}

/// How many grains of a dried face's grit span a metre, and how many of the
/// fine cracks crazing it.
const GRIT: f64 = 180.0;
const CRAZING: f64 = 30.0;

impl Mud {
    /// The colour at `spot`, its plate keyed as its mark is beneath its
    /// placing's: palest on the curled rim drying off the damp below,
    /// crazed with fine cracks of its own, damp down its wall.
    pub(crate) fn colour(&self, spot: &Spot) -> Vec3 {
        let key = spot.mark ^ spot.instance;
        let size = spot.girth.max(1e-4);
        let (inward, down) = (spot.uv.0 / size, spot.uv.1 / size);
        let tone = 0.86 + 0.26 * unit(mix32(key ^ 0x3d));
        let face = self.dry * (1.0 + 0.1 * (1.0 - smoothstep(0.0, 0.5, inward)));
        let fine = 1.0 - smoothstep(0.002, 0.01, spot.width);
        // Only some plates crazed as they dried, and faintly.
        let crazing = fine * smoothstep(0.5, 0.9, unit(mix32(key ^ 0xc7a1)));
        let crazed = if crazing > 0.0 {
            let cells = cells2(spot.p.x * CRAZING, spot.p.z * CRAZING, key ^ 0xc7a2, 0.9);
            crazing * (1.0 - smoothstep(0.004, 0.018, cells.wall()))
        } else {
            0.0
        };
        let colour = face
            .lerp(self.damp, 0.3 * crazed)
            .lerp(self.damp, smoothstep(0.0, 0.35, down));
        let grit = noise3(spot.p * GRIT, key ^ 0x9) * fine;
        colour * (tone * (1.0 + 0.07 * grit))
    }
}

#[cfg(test)]
#[path = "mud_tests.rs"]
mod tests;
