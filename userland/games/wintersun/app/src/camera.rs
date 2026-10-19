//! Where the player looks from: a perspective view across the land, a
//! distance back from the character and an angle down onto it.
//!
//! The camera holds what the player sets — how far back it stands, how far
//! it looks down, which way it faces — and the realm it is confined to, and
//! turns them into the [`Perspective`] a frame is drawn through.
//!
//! The focus is clamped into the realm, so a player at the map's edge walks
//! toward the edge of the screen rather than the camera leaving the world;
//! the view past the edge is sky. The eye is kept above the ground beneath
//! it, so a hill behind the character never puts the camera inside the land
//! it looks over.

use tairix_raster::view::Perspective;
use tairix_util::mathf;
use tairix_wintersun_art::decal::Bounds;
use tairix_wintersun_net::value::{Facing, WorldPoint};
use tairix_wintersun_world::geom::{CELL_SUB_UNITS, CHUNK_CELLS};
use tairix_wintersun_world::params::RealmParams;

use crate::error::ClientError;
use crate::view::Viewport;

/// How far back the camera stands, as a power of two: the zoom control's
/// five stops, each a doubling.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Zoom(u32);

/// Closest the camera comes.
const MIN_LEVEL: u32 = 3;
/// Furthest it goes.
const MAX_LEVEL: u32 = 7;
/// The view the game is authored at.
const DEFAULT_LEVEL: u32 = 5;

/// The distance of the stop a level below the nearest, in world sub-units:
/// three quarters of a cell, so the nearest stop stands six cells back, the
/// default twenty-four and the furthest ninety-six.
const STAND_OFF: i32 = CELL_SUB_UNITS * 3 / 4;

impl Zoom {
    /// The view the game is authored at.
    pub const DEFAULT: Self = Self(DEFAULT_LEVEL);
    /// Closest in.
    pub const NEAREST: Self = Self(MIN_LEVEL);
    /// Furthest out.
    pub const FURTHEST: Self = Self(MAX_LEVEL);

    /// The stop `level` doublings out, clamped to the range the camera has.
    #[must_use]
    pub const fn new(level: u32) -> Self {
        if level < MIN_LEVEL {
            Self(MIN_LEVEL)
        } else if level > MAX_LEVEL {
            Self(MAX_LEVEL)
        } else {
            Self(level)
        }
    }

    /// One stop closer, or `None` at the nearest.
    #[must_use]
    pub const fn nearer(self) -> Option<Self> {
        if self.0 > MIN_LEVEL {
            Some(Self(self.0 - 1))
        } else {
            None
        }
    }

    /// One stop further out, or `None` at the furthest.
    #[must_use]
    pub const fn further(self) -> Option<Self> {
        if self.0 < MAX_LEVEL {
            Some(Self(self.0 + 1))
        } else {
            None
        }
    }

    /// How many doublings out it is.
    #[must_use]
    pub const fn level(self) -> u32 {
        self.0
    }

    /// How far the eye stands from what it looks at, in world sub-units.
    #[must_use]
    pub const fn distance(self) -> i32 {
        STAND_OFF << self.0
    }
}

impl Default for Zoom {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// How far the camera looks down from level, in [`Facing`]'s units of a
/// turn.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Tilt(u16);

impl Tilt {
    /// The lowest the camera looks: across the land, with the horizon in
    /// view.
    pub const LOWEST: Self = Self(degrees(12));
    /// The angle the game is authored at.
    pub const DEFAULT: Self = Self(degrees(50));
    /// Straight down, as the map was drawn.
    pub const STRAIGHT_DOWN: Self = Self(degrees(90));

    /// A tilt of `units`, clamped to the range the camera has.
    #[must_use]
    pub const fn new(units: u16) -> Self {
        if units < Self::LOWEST.0 {
            Self::LOWEST
        } else if units > Self::STRAIGHT_DOWN.0 {
            Self::STRAIGHT_DOWN
        } else {
            Self(units)
        }
    }

    /// The angle, in [`Facing`]'s units.
    #[must_use]
    pub const fn units(self) -> u16 {
        self.0
    }
}

impl Default for Tilt {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// `degrees` of a turn in [`Facing`]'s units, rounded down.
const fn degrees(degrees: u32) -> u16 {
    let units = degrees * 0x1_0000 / 360;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a tilt is at most a quarter turn, far inside a u16"
    )]
    let units = units as u16;
    units
}

/// The way the camera faces until the player can turn it: north, so the
/// view reads as the map always did.
const NORTH: Facing = Facing(0xC000);

/// A quarter of a turn, which takes the camera's heading to its right.
const QUARTER_TURN: u16 = 0x4000;

/// How far above the focus's ground the camera looks: a cell, about a
/// figure's middle, so the figure stands centred rather than its feet.
const FOCUS_LIFT: i32 = CELL_SUB_UNITS;

/// The least the eye stands above the ground under it.
const CLEARANCE: i32 = 2 * CELL_SUB_UNITS;

/// The nearest the camera sees, in world sub-units: a quarter of a cell,
/// nearer than the eye ever is to the ground it stands clear of.
pub const NEAR: i32 = CELL_SUB_UNITS / 4;

/// The focal length a render target of a given height is seen at, as a
/// ratio to that height: a fifty-degree field of view up the screen.
const FOCAL_PER_HEIGHT: (u32, u32) = (1072, 1000);

/// The focal length `view`'s render target is drawn at, in its pixels.
#[must_use]
pub fn focal(view: &Viewport) -> u32 {
    let (_, height) = view.render();
    let scaled = u64::from(height) * u64::from(FOCAL_PER_HEIGHT.0) / u64::from(FOCAL_PER_HEIGHT.1);
    u32::try_from(scaled).unwrap_or(u32::MAX).max(1)
}

/// The extent of a realm in world sub-units.
///
/// Derived from the realm's own chunk extent rather than stated
/// separately, so a realm of a different size needs no second number.
#[must_use]
pub fn realm_bounds(params: RealmParams) -> Bounds {
    let cell = i64::from(CELL_SUB_UNITS);
    let cells = i64::from(CHUNK_CELLS);
    let min = i64::from(params.min_chunk()) * cells * cell;
    let max = i64::from(params.max_chunk()) * cells * cell;
    let clamp = |v: i64| i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX });
    Bounds {
        min_x: clamp(min),
        min_y: clamp(min),
        max_x: clamp(max),
        max_y: clamp(max),
    }
}

/// What the player is looking at, and from where.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Camera {
    target: WorldPoint,
    zoom: Zoom,
    tilt: Tilt,
    heading: Facing,
    bounds: Bounds,
}

impl Camera {
    /// A camera aimed at `target` from `zoom`'s distance, at the authored
    /// tilt and heading, inside `bounds`.
    #[must_use]
    pub const fn new(target: WorldPoint, zoom: Zoom, bounds: Bounds) -> Self {
        Self {
            target,
            zoom,
            tilt: Tilt::DEFAULT,
            heading: NORTH,
            bounds,
        }
    }

    /// What it has been aimed at, which is not necessarily what it looks
    /// at: a target outside the realm is settled onto its edge.
    #[must_use]
    pub const fn target(&self) -> WorldPoint {
        self.target
    }

    /// The realm it is confined to.
    #[must_use]
    pub const fn bounds(&self) -> Bounds {
        self.bounds
    }

    /// Aim it.
    pub fn look_at(&mut self, target: WorldPoint) {
        self.target = target;
    }

    /// How far back it stands.
    #[must_use]
    pub const fn zoom(&self) -> Zoom {
        self.zoom
    }

    /// Stand it back by `zoom`, keeping what it looks at.
    pub fn set_zoom(&mut self, zoom: Zoom) {
        self.zoom = zoom;
    }

    /// How far it looks down.
    #[must_use]
    pub const fn tilt(&self) -> Tilt {
        self.tilt
    }

    /// Look down by `tilt`.
    pub fn set_tilt(&mut self, tilt: Tilt) {
        self.tilt = tilt;
    }

    /// Which way it faces.
    #[must_use]
    pub const fn heading(&self) -> Facing {
        self.heading
    }

    /// What it looks at: the target, settled into the realm.
    #[must_use]
    pub fn focus(&self) -> WorldPoint {
        // Bounds of no extent hold nothing to settle on.
        let settle = |value: i32, low: i32, high: i32| {
            if low >= high {
                value
            } else {
                value.clamp(low, high - 1)
            }
        };
        WorldPoint {
            x: settle(self.target.x, self.bounds.min_x, self.bounds.max_x),
            y: settle(self.target.y, self.bounds.min_y, self.bounds.max_y),
        }
    }

    /// World sub-units one of `view`'s render pixels spans at the focus,
    /// rounded up: the size a figure standing there is drawn at.
    #[must_use]
    pub fn focus_step(&self, view: &Viewport) -> i32 {
        focus_step(self.zoom, view)
    }

    /// The perspective `view` is drawn through, with the focus standing on
    /// ground `floor` world sub-units high and the eye kept clear of the
    /// ground `under` reports beneath it, where it reports any.
    ///
    /// # Errors
    ///
    /// [`ClientError::Viewport`] for a view no camera can project onto.
    #[allow(
        clippy::float_arithmetic,
        reason = "the camera's basis is trigonometry, taken once a frame over \
                  the shared bounded maths; every vertex and pixel it then \
                  projects goes through the one perspective, and every pass \
                  past it stays in integers"
    )]
    pub fn perspective(
        &self,
        view: &Viewport,
        floor: i32,
        under: impl Fn(WorldPoint) -> Option<i32>,
    ) -> Result<Perspective, ClientError> {
        let (east, south) = self.heading.unit_vector();
        let (right_east, right_south) =
            Facing(self.heading.0.wrapping_add(QUARTER_TURN)).unit_vector();
        let (level, down) = Facing(self.tilt.0).unit_vector();
        let forward = [level * east, level * south, -down];
        let below = [-down * east, -down * south, -level];
        let right = [right_east, right_south, 0.0];
        let focus = self.focus();
        let target = [
            f64::from(focus.x),
            f64::from(focus.y),
            f64::from(floor.saturating_add(FOCUS_LIFT)),
        ];
        let distance = f64::from(self.zoom.distance());
        let mut eye = [
            target[0] - forward[0] * distance,
            target[1] - forward[1] * distance,
            target[2] - forward[2] * distance,
        ];
        let stands = WorldPoint {
            x: mathf::round_i32(eye[0]),
            y: mathf::round_i32(eye[1]),
        };
        // A raised eye looks back down at the focus, so the character stays
        // where the player put it on the screen.
        let mut look = forward;
        if let Some(ground) = under(stands) {
            let lowest = f64::from(ground.saturating_add(CLEARANCE));
            if eye[2] < lowest {
                eye[2] = lowest;
                look = [target[0] - eye[0], target[1] - eye[1], target[2] - eye[2]];
            }
        }
        let (width, height) = view.render();
        Perspective::new(
            eye,
            look,
            right,
            below,
            f64::from(focal(view)),
            (width, height),
            f64::from(NEAR),
        )
        .ok_or(ClientError::Viewport)
    }
}

/// World sub-units one of `view`'s render pixels spans `zoom`'s distance
/// from the eye, rounded up.
#[must_use]
pub fn focus_step(zoom: Zoom, view: &Viewport) -> i32 {
    let span = u32::try_from(zoom.distance())
        .unwrap_or(u32::MAX)
        .div_ceil(focal(view));
    i32::try_from(span).unwrap_or(i32::MAX).max(1)
}

#[cfg(test)]
#[path = "camera_tests.rs"]
mod tests;
