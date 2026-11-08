//! The fixed bounds a drawing is held to.
//!
//! These are containment bounds, sized for documents rather than for desktop
//! assets: what a hostile file can make a reader hold, and what a drawing
//! made by hand never reaches. They do not scale with the machine.

/// The most objects one drawing holds, layers and groups included.
pub const MAX_OBJECTS: usize = 1 << 20;

/// The deepest groups nest, layers counted.
pub const MAX_DEPTH: usize = 128;

/// The most nodes one path holds.
pub const MAX_PATH_NODES: usize = 1 << 20;

/// The most path nodes one drawing holds across all its paths.
pub const MAX_TOTAL_PATH_NODES: usize = 1 << 22;

/// The largest magnitude any coordinate, length or radius may have: a
/// billion units, past which the double's precision no longer resolves a
/// hair of a unit.
pub const MAX_COORDINATE: f64 = 1e9;

/// The largest magnitude a transform's linear coefficients may have.
pub const MAX_SCALE: f64 = 1e6;

/// The most stops one gradient holds.
pub const MAX_STOPS: usize = 256;

/// The most entries one dash pattern holds.
pub const MAX_DASH_ENTRIES: usize = 256;

/// The longest one text object's content, in bytes.
pub const MAX_TEXT_BYTES: usize = 1 << 16;

/// The longest an object's name, in bytes.
pub const MAX_NAME_BYTES: usize = 256;

/// The longest a font family list, in bytes.
pub const MAX_FAMILY_BYTES: usize = 256;

/// The most runs one text object sets in styles of their own.
pub const MAX_TEXT_RUNS: usize = 1 << 12;

/// The most font families one text object's runs name besides its own.
pub const MAX_TEXT_FAMILIES: usize = 32;

/// The furthest, in ems of its own size, a run's glyphs are raised or
/// lowered from the baseline.
pub const MAX_BASELINE_SHIFT: f64 = 4.0;

/// The most nodes the path a text is set along holds.
pub const MAX_TEXT_PATH_NODES: usize = 1 << 12;

/// The most guides one drawing holds.
pub const MAX_GUIDES: usize = 4096;

/// The most swatches one drawing holds.
pub const MAX_SWATCHES: usize = 1024;

/// The most pixels to the inch a raster copy of a drawing is made at.
pub const MAX_RESOLUTION: u32 = 10_000;

/// The fewest points a star or polygon has.
pub const MIN_STAR_POINTS: u32 = 3;

/// The most points a star or polygon has.
pub const MAX_STAR_POINTS: u32 = 1000;

/// The most cells the grid cuts each of its major cells into.
pub const MAX_SUBDIVISIONS: u32 = 64;

/// The most bytes a picture keeps of what it was read from: the most a
/// document read in may be.
pub const MAX_IMAGE_SOURCE_BYTES: usize = 64 << 20;

/// The longest side a picture in a drawing has, weighed before its pixels
/// can be counted. Its pixels are bounded by what a surface holds
/// (`tairix_raster::MAX_SURFACE_PIXELS`).
pub const MAX_IMAGE_SIDE: u32 = 1 << 16;

/// The most pixels the pictures of one drawing hold together, each counted
/// once however many objects show it: four as large as one may be.
pub const MAX_TOTAL_IMAGE_PIXELS: u64 = 4 * tairix_raster::MAX_SURFACE_PIXELS as u64;

/// Whether `value` is a coordinate or length the drawing may hold.
#[must_use]
pub fn coordinate(value: f64) -> bool {
    value.is_finite() && value.abs() <= MAX_COORDINATE
}

/// Whether `value` is a length the drawing may hold: a coordinate that is not
/// negative.
#[must_use]
pub fn length(value: f64) -> bool {
    coordinate(value) && value >= 0.0
}

/// Whether `transform` is one the drawing may hold: finite, its linear part
/// within [`MAX_SCALE`], its translation a coordinate.
#[must_use]
pub fn transform(transform: tairix_bezier::Affine) -> bool {
    let linear = [transform.a, transform.b, transform.c, transform.d];
    linear
        .iter()
        .all(|value| value.is_finite() && value.abs() <= MAX_SCALE)
        && coordinate(transform.e)
        && coordinate(transform.f)
}

/// Whether `point` is one the drawing may hold.
#[must_use]
pub fn point(point: tairix_bezier::Point) -> bool {
    coordinate(point.x) && coordinate(point.y)
}
