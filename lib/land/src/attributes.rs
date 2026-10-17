//! What a land is like at each vertex of a level, a byte to a channel.
//!
//! First its quantities, each blended between vertices where it is read: how
//! wet, what the water laid down or wore away, how much of a road and of a
//! path is there, how much grows, and how deep snow lies. Then what a farmed
//! land grows there and the way its rows run (`grown`): categories, read at
//! the nearest vertex and never blended.

use tairix_util::mathf;

/// How many of the channels are quantities.
pub const QUANTITIES: usize = 6;
/// How many channels a vertex carries.
pub const CHANNELS: usize = QUANTITIES + 2;
/// How wet: dry at nought, standing water at one.
pub const WET: usize = 0;
/// What the water laid down or wore away: worn at nought, untouched at a
/// half, built up at one.
pub const SEDIMENT: usize = 1;
/// How much of a road's surface.
pub const ROAD: usize = 2;
/// How much of a trodden path.
pub const PATH: usize = 3;
/// How much grows.
pub const GREEN: usize = 4;
/// How deep snow lies, as `snow::kept` keeps it.
pub const SNOW: usize = QUANTITIES - 1;
/// What a farmed land grows, as `grown::Grown::code` names it.
pub const GROWN: usize = QUANTITIES;
/// The way a field's rows run, as `grown::rows_code` keeps it.
pub const ROWS: usize = QUANTITIES + 1;

/// What a land is like at one vertex.
pub type Attributes = [u8; CHANNELS];

/// What ground carrying no attributes is like everywhere: dry, neither worn
/// nor built up, on no road or path, green enough for anything to grow, and
/// bare of snow.
pub const PLAIN: [f64; QUANTITIES] = [0.0, 0.5, 0.0, 0.0, 1.0, 0.0];

/// A sediment, worn at `-1.0` to built up at `1.0`, as the share of its
/// channel that keeps it.
#[must_use]
pub fn sediment_share(sediment: f64) -> f64 {
    0.5 + 0.5 * sediment
}

/// The sediment a share of its channel keeps, worn at `-1.0` to built up at
/// `1.0`.
#[must_use]
pub fn sediment_of(share: f64) -> f64 {
    2.0 * share - 1.0
}

/// Quantity `channel` of `corners`, the four vertices about a place in the
/// order `(0, 0)`, `(1, 0)`, `(0, 1)`, `(1, 1)`, blended as the place lies
/// `(across, down)` between them, `0.0..=1.0`; nought for a channel they do
/// not carry.
#[must_use]
pub fn blended(corners: [Attributes; 4], channel: usize, (across, down): (f64, f64)) -> f64 {
    let value = |corner: Attributes| {
        corner
            .get(channel)
            .map_or(0.0, |&value| f64::from(value) / 255.0)
    };
    mathf::bilinear(corners.map(value), (across, down))
}

/// What a place on a land is like.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Lie {
    /// Its height.
    pub height: f64,
    /// How upright the ground stands: its normal's upward part.
    pub upright: f64,
    /// How wet, `0.0..=1.0`.
    pub wet: f64,
    /// What the water laid down there, or wore away where negative,
    /// `-1.0..=1.0`.
    pub sediment: f64,
    /// How much of a road the place lies on.
    pub road: f64,
    /// How much of a path.
    pub path: f64,
    /// How much can grow there, `0.0..=1.0`.
    pub green: f64,
    /// How deep snow lies there, in metres.
    pub snow: f64,
    /// What a farmed land grows there, as `grown::Grown::code` names it.
    pub grown: u8,
    /// The way its rows run, as `grown::rows_code` keeps it.
    pub rows: u8,
}
