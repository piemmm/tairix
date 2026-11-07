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

/// The most guides one drawing holds.
pub const MAX_GUIDES: usize = 4096;

/// The most points a star or polygon has.
pub const MAX_STAR_POINTS: u32 = 1000;

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
