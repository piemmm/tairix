//! The realm parameter document.
//!
//! A realm is a `u64` seed and this: eight numbers that decide how big the
//! world is, how finely it is solved, and what kind of place it is. Nothing
//! here is content — spells, items and biome tuning are declarative
//! documents elsewhere — and nothing here is a capacity the machine should
//! be choosing.
//!
//! A realm's climate is a *latitude span*, not a temperature range: the
//! world lies between two latitudes on an Earth-like planet, and its
//! temperatures, seasons, winds and rain follow from where it lies. A cold
//! realm is a span near a pole, not a code path.
//!
//! # Every field is bounded, because this document is untrusted
//!
//! A client is handed its realm's parameters by the realm, and a realm is
//! no more trusted by a client than a client is by a realm. So this type
//! has one validating constructor and no public fields: an out-of-range
//! extent, a resolution that would not tile, or a latitude off the planet
//! is refused with a reason rather than clamped into something the two
//! ends might disagree about. The bounds are fixed security bounds on
//! untrusted input and do not scale with the machine.
//!
//! The document's one spelling, [`RealmSpec`], is the wire's: this crate
//! decodes no bytes, and validates what `wintersun/net` decoded.

use tairix_wintersun_net::value::Facing;

pub use tairix_wintersun_net::value::RealmSpec;

use crate::geom::{lerp, signed, CellCoord, CHUNK_CELLS};

/// Smallest realm, in chunks along one edge.
pub const MIN_EXTENT_CHUNKS: u32 = 4;

/// Largest realm, in chunks along one edge.
///
/// At 64 cells to a chunk and one world unit to a cell this is a realm
/// 262 144 units on a side. Its half-extent in wire sub-units is an eighth
/// of `i32`'s range, so no position inside a legal realm can overflow a
/// coordinate.
pub const MAX_EXTENT_CHUNKS: u32 = 4096;

/// Fewest coarse samples along one edge of the realm field.
pub const MIN_COARSE_SAMPLES: u32 = 32;

/// Most coarse samples along one edge of the realm field.
///
/// This bounds the one structure whose size does not follow the working
/// set: the realm field is solved globally and held for the realm's life,
/// so its cost must not follow the realm's extent. It does not — a realm
/// four thousand chunks across and one four chunks across get the same
/// grid, the former simply at a coarser step. At the ceiling the field is
/// 512 × 512 samples, a few mebibytes, which is the same on every machine.
pub const MAX_COARSE_SAMPLES: u32 = 512;

/// Fewest continental plates.
pub const MIN_PLATES: u32 = 4;

/// Most continental plates.
pub const MAX_PLATES: u32 = 64;

/// Highest permitted peak relief, in world units above sea level.
///
/// Below the elevation field's own ±4096-unit range, so erosion, uplift and
/// the channel carve all have headroom above the tallest legal peak and
/// cannot saturate the field.
pub const MAX_RELIEF_UNITS: u16 = 3000;

/// The south pole, in degrees north.
pub const MIN_LATITUDE: i16 = -90;

/// The north pole, in degrees north.
pub const MAX_LATITUDE: i16 = 90;

/// Why a parameter document was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ParamsError {
    /// The realm extent is outside [`MIN_EXTENT_CHUNKS`]..=[`MAX_EXTENT_CHUNKS`]
    /// or is not a power of two.
    Extent,
    /// The coarse resolution is outside
    /// [`MIN_COARSE_SAMPLES`]..=[`MAX_COARSE_SAMPLES`], is not a power of
    /// two, or is finer than the cell grid it samples.
    CoarseResolution,
    /// The plate count is outside [`MIN_PLATES`]..=[`MAX_PLATES`].
    PlateCount,
    /// The submerged fraction exceeds one thousand parts per thousand.
    OceanFraction,
    /// The peak relief is zero or above [`MAX_RELIEF_UNITS`].
    Relief,
    /// An edge latitude is off the planet — outside
    /// [`MIN_LATITUDE`]..=[`MAX_LATITUDE`] — or the northern edge lies south
    /// of the southern one.
    Latitude,
}

/// A validated realm parameter document.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RealmParams {
    spec: RealmSpec,
}

impl RealmParams {
    /// Validate a specification.
    ///
    /// # Errors
    ///
    /// The first field that fails its bound, so a refusal names what to
    /// correct rather than that something was wrong.
    pub fn new(spec: RealmSpec) -> Result<Self, ParamsError> {
        if !(MIN_EXTENT_CHUNKS..=MAX_EXTENT_CHUNKS).contains(&spec.extent_chunks)
            || !spec.extent_chunks.is_power_of_two()
        {
            return Err(ParamsError::Extent);
        }
        let cells_per_edge = spec.extent_chunks * CHUNK_CELLS;
        if !(MIN_COARSE_SAMPLES..=MAX_COARSE_SAMPLES).contains(&spec.coarse_samples)
            || !spec.coarse_samples.is_power_of_two()
            || spec.coarse_samples > cells_per_edge
        {
            return Err(ParamsError::CoarseResolution);
        }
        if !(MIN_PLATES..=MAX_PLATES).contains(&spec.plates) {
            return Err(ParamsError::PlateCount);
        }
        if spec.ocean_permille > 1000 {
            return Err(ParamsError::OceanFraction);
        }
        if spec.relief_units == 0 || spec.relief_units > MAX_RELIEF_UNITS {
            return Err(ParamsError::Relief);
        }
        let planet = MIN_LATITUDE..=MAX_LATITUDE;
        if !planet.contains(&spec.north_latitude)
            || !planet.contains(&spec.south_latitude)
            || spec.north_latitude < spec.south_latitude
        {
            return Err(ParamsError::Latitude);
        }
        Ok(Self { spec })
    }

    /// The default realm for `seed`: from the ice sheet of the high north
    /// to the rainforest just beyond the equator, mostly land, with
    /// westerlies veering a thirty-second of a turn north of east.
    ///
    /// Infallible by construction — the constants below are inside every
    /// bound above, and a test holds them there.
    #[must_use]
    pub fn default_realm(seed: u64) -> Self {
        Self {
            spec: RealmSpec {
                seed,
                extent_chunks: 256,
                coarse_samples: 256,
                plates: 12,
                ocean_permille: 380,
                relief_units: 1800,
                north_latitude: 76,
                south_latitude: -6,
                westerlies: Facing(0xF800),
            },
        }
    }

    /// The specification these parameters validated.
    #[must_use]
    pub const fn spec(self) -> RealmSpec {
        self.spec
    }

    /// The realm seed.
    #[must_use]
    pub const fn seed(self) -> u64 {
        self.spec.seed
    }

    /// Chunks along one edge of the realm.
    #[must_use]
    pub const fn extent_chunks(self) -> u32 {
        self.spec.extent_chunks
    }

    /// Cells along one edge of the realm.
    #[must_use]
    pub const fn extent_cells(self) -> u32 {
        self.spec.extent_chunks * CHUNK_CELLS
    }

    /// Coarse samples along one edge of the realm field.
    #[must_use]
    pub const fn coarse_samples(self) -> u32 {
        self.spec.coarse_samples
    }

    /// Cells between adjacent coarse samples. Never zero, and exact: both
    /// extents are powers of two and the resolution is the finer.
    #[must_use]
    pub const fn cells_per_coarse(self) -> u32 {
        self.extent_cells() / self.spec.coarse_samples
    }

    /// The world cell coarse sample `(sx, sy)` stands at: the first cell of
    /// its step.
    #[must_use]
    pub fn sample_cell(self, sx: i32, sy: i32) -> CellCoord {
        let origin = self.min_chunk() * signed(CHUNK_CELLS);
        let step = signed(self.cells_per_coarse());
        CellCoord::new(origin + sx * step, origin + sy * step)
    }

    /// Continental plates.
    #[must_use]
    pub const fn plates(self) -> u32 {
        self.spec.plates
    }

    /// Target fraction of the realm below sea level, in parts per thousand.
    #[must_use]
    pub const fn ocean_permille(self) -> u16 {
        self.spec.ocean_permille
    }

    /// Peak relief above sea level, in world units.
    #[must_use]
    pub const fn relief_units(self) -> u16 {
        self.spec.relief_units
    }

    /// Latitude of the northern edge, in degrees north.
    #[must_use]
    pub fn north_latitude(self) -> f64 {
        f64::from(self.spec.north_latitude)
    }

    /// Latitude of the southern edge, in degrees north.
    #[must_use]
    pub fn south_latitude(self) -> f64 {
        f64::from(self.spec.south_latitude)
    }

    /// Latitude at the fraction `v` of the way from the northern edge to
    /// the southern, in degrees north.
    ///
    /// Linear, so a degree is the same breadth of ground at any latitude:
    /// the realm is a map of a band of the planet, not a globe.
    #[must_use]
    pub fn latitude_at(self, v: f64) -> f64 {
        lerp(self.north_latitude(), self.south_latitude(), v)
    }

    /// Where the northern hemisphere's mid-latitude westerlies blow toward.
    #[must_use]
    pub const fn westerlies(self) -> Facing {
        self.spec.westerlies
    }

    /// Half the realm's edge, in chunks.
    ///
    /// The one place the extent crosses into signed indices, so the one
    /// place the conversion has to be argued.
    #[must_use]
    #[allow(
        clippy::cast_possible_wrap,
        reason = "validation caps the extent at MAX_EXTENT_CHUNKS, so half \
                  of it is at most 2048"
    )]
    pub const fn half_extent_chunks(self) -> i32 {
        (self.spec.extent_chunks / 2) as i32
    }

    /// The lowest chunk index the realm covers on either axis.
    ///
    /// The realm is centred on the origin, so a fresh character spawning at
    /// `(0, 0)` is in the middle of it.
    #[must_use]
    pub const fn min_chunk(self) -> i32 {
        -self.half_extent_chunks()
    }

    /// One past the highest chunk index the realm covers on either axis.
    #[must_use]
    pub const fn max_chunk(self) -> i32 {
        self.half_extent_chunks()
    }

    /// Whether `chunk` lies inside the realm.
    #[must_use]
    pub const fn holds_chunk(self, x: i32, y: i32) -> bool {
        x >= self.min_chunk()
            && x < self.max_chunk()
            && y >= self.min_chunk()
            && y < self.max_chunk()
    }
}

#[cfg(test)]
mod tests;
