//! The body frame, and the lenses a figure is seen through.
//!
//! A part carries a [`Body`] offset and reaches the screen through a [`Lens`]:
//! the scene's perspective camera, or the oblique drawing the art harness and
//! the designer's preview make, whose depth axis is foreshortened by
//! [`FORESHORTEN`] and whose figure is one size at every depth.
//!
//! # Why the turnaround is free
//!
//! A part is painted far-first by the depth its lens gives it. Facing away, a
//! face sorts behind the skull and is covered; facing the camera it comes
//! forward. Nothing asks which way the figure points, so eight or sixteen
//! headings need no artwork and no `cfg`.
//!
//! The depth key is deliberately not the screen row: a raised hand draws
//! higher without becoming further away, so sorting on the row would put it
//! behind the body it belongs to.
//!
//! # Why the projection is enough on its own
//!
//! A figure's surfaces are carried through this projection vertex by vertex,
//! so nothing anywhere needs a screen angle for a part: a limb's far end is
//! wherever its child joint projects to, at whatever length the heading
//! leaves it. The billboard this replaced had to approximate a three-axis
//! rotation as one screen turn, and could not — measured against the shipped
//! walk, a thigh's drawn end missed its knee by a third of the figure's
//! height.

use tairix_raster::mesh::Depth;
use tairix_raster::view::Perspective;
use tairix_util::mathf;
use tairix_wintersun_net::value::Facing;

use crate::error::FigureError;

/// How much of a step *into* the scene shows as vertical screen movement.
///
/// The tangent of the figure's drawing elevation, about 35°: shallow enough
/// that a figure still reads in silhouette, steep enough that walking
/// up-screen reads as crossing a floor rather than rising into the air. The
/// authored value is this ratio rather than the angle, because the ratio is
/// what every line of the projection uses; the angle it corresponds to is
/// held to the doc by this module's tests.
pub const FORESHORTEN: f64 = 0.7;

/// A position or direction in the figure's own frame.
///
/// Authoring a body in these terms is what lets one skeleton serve every
/// heading. Lengths are figure-local pixels at [`STANDING_HEIGHT`].
///
/// [`STANDING_HEIGHT`]: crate::humanoid::STANDING_HEIGHT
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Body {
    /// Along the heading; positive is the way the figure faces.
    pub forward: f64,
    /// Across the heading; positive is to the figure's left.
    pub side: f64,
    /// Away from the ground.
    pub up: f64,
}

impl Body {
    /// The frame's origin.
    pub const ORIGIN: Self = Self::new(0.0, 0.0, 0.0);

    /// The frame's forward axis.
    pub const FORWARD: Self = Self::new(1.0, 0.0, 0.0);

    /// The frame's side axis, pointing to the figure's left.
    pub const SIDE: Self = Self::new(0.0, 1.0, 0.0);

    /// The frame's up axis.
    pub const UP: Self = Self::new(0.0, 0.0, 1.0);

    /// An offset in the figure's frame.
    #[must_use]
    pub const fn new(forward: f64, side: f64, up: f64) -> Self {
        Self { forward, side, up }
    }

    /// Whether every component is a finite number.
    #[must_use]
    pub fn is_real(self) -> bool {
        self.forward.is_finite() && self.side.is_finite() && self.up.is_finite()
    }

    /// Component-wise sum.
    #[must_use]
    pub fn plus(self, other: Self) -> Self {
        Self::new(
            self.forward + other.forward,
            self.side + other.side,
            self.up + other.up,
        )
    }

    /// This offset scaled uniformly.
    #[must_use]
    pub fn scaled(self, factor: f64) -> Self {
        Self::new(self.forward * factor, self.side * factor, self.up * factor)
    }

    /// Distance from the origin.
    #[must_use]
    pub fn length(self) -> f64 {
        mathf::hypot(mathf::hypot(self.forward, self.side), self.up)
    }

    /// Dot product.
    #[must_use]
    pub fn dot(self, other: Self) -> f64 {
        self.forward * other.forward + self.side * other.side + self.up * other.up
    }

    /// Cross product, right-handed over (`forward`, `side`, `up`).
    #[must_use]
    pub fn cross(self, other: Self) -> Self {
        Self::new(
            self.side * other.up - self.up * other.side,
            self.up * other.forward - self.forward * other.up,
            self.forward * other.side - self.side * other.forward,
        )
    }

    /// This direction turned `angle` radians about unit `axis`, right-handed.
    ///
    /// Rodrigues' rotation, so composing rotations needs no quaternion and no
    /// matrix inverse. `axis` must be a unit direction, which every axis of an
    /// orthonormal [`Basis`] is — the only caller.
    #[must_use]
    fn turned_about(self, axis: Self, angle: f64) -> Self {
        let (sin, cos) = (mathf::sin(angle), mathf::cos(angle));
        self.scaled(cos)
            .plus(axis.cross(self).scaled(sin))
            .plus(axis.scaled(axis.dot(self) * (1.0 - cos)))
    }
}

/// A rotation of one joint, about its own axes.
///
/// Applied intrinsically in the order yaw, pitch, roll — each about the axis
/// the previous rotation left — so a name keeps its anatomical meaning
/// whatever the others are doing: `pitch` always swings a limb fore-and-aft
/// in its own plane, never sideways.
///
/// Each is right-handed about the axis it names, with no sign flipped
/// anywhere in the transform. The consequence worth stating, because it
/// decides the sign of every limit a rig declares: a part *hangs below* its
/// joint, so it turns the opposite way to the joint's own `forward`. A
/// positive `pitch` tips the top forward and therefore swings a hanging limb
/// **backward**; a positive `roll` tips the top to the figure's right and
/// therefore swings a hanging limb to its **left**.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Rotation {
    /// About the `side` axis: a limb's fore-and-aft swing, a spine's bend.
    /// Positive leans the top forward and swings a hanging limb back.
    pub pitch: f64,
    /// About the `up` axis: a head's turn. Positive turns `forward` toward
    /// the figure's left.
    pub yaw: f64,
    /// About the `forward` axis: a limb splayed outward, a head tilted.
    /// Positive leans the top to the figure's right and swings a hanging limb
    /// to its left — so an outward splay is signed by which side it is on.
    pub roll: f64,
}

impl Rotation {
    /// No rotation.
    pub const REST: Self = Self {
        pitch: 0.0,
        yaw: 0.0,
        roll: 0.0,
    };

    /// A rotation about the three axes.
    #[must_use]
    pub const fn new(pitch: f64, yaw: f64, roll: f64) -> Self {
        Self { pitch, yaw, roll }
    }

    /// Whether every angle is a finite number.
    #[must_use]
    pub fn is_real(self) -> bool {
        self.pitch.is_finite() && self.yaw.is_finite() && self.roll.is_finite()
    }
}

/// An orthonormal frame, held as the images of the three body axes.
///
/// Three named directions rather than nine numbers in a row, so which
/// component means what is legible at every use and a composition cannot
/// transpose itself silently.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Basis {
    /// Where this frame's forward axis points in the frame it is held in.
    pub forward: Body,
    /// Where its side axis points.
    pub side: Body,
    /// Where its up axis points.
    pub up: Body,
}

impl Basis {
    /// The unrotated frame.
    pub const IDENTITY: Self = Self {
        forward: Body::FORWARD,
        side: Body::SIDE,
        up: Body::UP,
    };

    /// The frame `rotation` leaves.
    #[must_use]
    pub fn of(rotation: Rotation) -> Self {
        // Each axis is skipped when its angle is zero, which is the common
        // case: most joints drive one axis and rest on the other two.
        let mut basis = Self::IDENTITY;
        if rotation.yaw != 0.0 {
            basis = basis.turned_about(basis.up, rotation.yaw);
        }
        if rotation.pitch != 0.0 {
            basis = basis.turned_about(basis.side, rotation.pitch);
        }
        if rotation.roll != 0.0 {
            basis = basis.turned_about(basis.forward, rotation.roll);
        }
        basis
    }

    /// This frame turned `angle` radians about unit `axis`.
    #[must_use]
    fn turned_about(self, axis: Body, angle: f64) -> Self {
        Self {
            forward: self.forward.turned_about(axis, angle),
            side: self.side.turned_about(axis, angle),
            up: self.up.turned_about(axis, angle),
        }
    }

    /// `local`, which is stated in this frame, expressed in the frame this
    /// one is held in.
    #[must_use]
    pub fn apply(self, local: Body) -> Body {
        self.forward
            .scaled(local.forward)
            .plus(self.side.scaled(local.side))
            .plus(self.up.scaled(local.up))
    }

    /// `held`, which is stated in the frame this one is held in, expressed
    /// in this frame.
    ///
    /// The inverse of [`Self::apply`]. A basis is orthonormal, so the inverse
    /// is the transpose and needs no solve — which is what lets a layer state
    /// a target in the body frame and hand a joint the direction in its
    /// parent's.
    #[must_use]
    pub fn unapply(self, held: Body) -> Body {
        Body::new(
            self.forward.dot(held),
            self.side.dot(held),
            self.up.dot(held),
        )
    }

    /// `child`, which is stated in this frame, expressed in the frame this
    /// one is held in.
    #[must_use]
    pub fn compose(self, child: Self) -> Self {
        Self {
            forward: self.apply(child.forward),
            side: self.apply(child.side),
            up: self.apply(child.up),
        }
    }
}

/// Where a body-frame offset lands relative to the figure's ground point, and
/// how far into the scene it is.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Projected {
    /// Columns right of the figure's ground point.
    pub dx: f64,
    /// Rows below it.
    pub dy: f64,
    /// Distance toward the camera; larger is nearer.
    pub depth: f64,
}

/// The ground direction a heading points along, worked out once per figure
/// because every point placed under it needs the same cosine and sine.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Heading {
    pub(crate) east: f64,
    pub(crate) south: f64,
}

impl Heading {
    /// The direction `facing` points along.
    #[must_use]
    pub fn of(facing: Facing) -> Self {
        let (east, south) = facing.unit_vector();
        Self { east, south }
    }
}

/// Project body-frame `offset` for a figure facing along `heading`, as the
/// oblique drawing does.
///
/// The answer is in the same figure-local pixels `offset` is, relative to the
/// figure's ground point; scaling to the surface and adding the anchor is the
/// caller's one conversion.
#[must_use]
pub fn project(heading: Heading, offset: Body) -> Projected {
    let (across, into) = plan(heading, offset);
    Projected {
        // Into the scene moves down the screen, foreshortened; height moves
        // up it unforeshortened.
        dx: across,
        dy: into * FORESHORTEN - offset.up,
        depth: into,
    }
}

/// The direction from a surface point toward the oblique drawing's eye, in
/// the frame of a figure facing along `heading`.
///
/// The one direction a point may move along without moving on screen, which
/// is what makes it the axis a silhouette is taken against: a surface is on
/// the near side exactly where its normal leans toward this.
#[must_use]
pub fn toward_camera(heading: Heading) -> Body {
    let Heading { east, south } = heading;
    let toward = Body::new(south, -east, FORESHORTEN);
    // The ground part is a unit vector turned by the heading, so the length
    // is the constant `hypot(1, FORESHORTEN)` and never zero.
    toward.scaled(1.0 / toward.length())
}

/// Where body-frame `offset` lies east and south of the figure's ground
/// point, for a figure facing along `heading`, in the units `offset` is.
#[must_use]
pub fn plan(heading: Heading, offset: Body) -> (f64, f64) {
    // Zero faces east and the turn advances toward south, which is the sense
    // the world's own axes have. The figure's left is therefore a quarter
    // turn back from its heading: east when facing south.
    let Heading { east, south } = heading;
    (
        offset.forward * east + offset.side * south,
        offset.forward * south - offset.side * east,
    )
}

/// How a figure's offsets reach the screen.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Lens<'a> {
    /// The drawing the art harness and the designer's preview make: one size
    /// at every depth, the depth axis foreshortened by [`FORESHORTEN`].
    Oblique {
        /// Surface pixels a figure unit.
        scale: f64,
        /// The surface point the ground contact sits on.
        at: (f64, f64),
    },
    /// The scene's camera, which draws a figure larger as it nears.
    Perspective {
        /// The view.
        camera: &'a Perspective,
        /// The ground point in the world: east, south and up.
        ground: [f64; 3],
        /// World units a figure unit.
        units: f64,
    },
}

/// Where a body-frame offset lands through a [`Lens`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Landed {
    /// Surface pixels across.
    pub x: f64,
    /// Surface pixels down.
    pub y: f64,
    /// What it is painted in order of; larger is nearer.
    pub key: f64,
    /// Its reciprocal depth, which a scene's depth buffer is compared with.
    /// The oblique drawing has no scene to stand in, so it lands nearest.
    pub depth: f64,
}

impl Lens<'_> {
    /// Refuse a lens no figure can be seen through.
    ///
    /// # Errors
    ///
    /// [`FigureError::ScaleUnreal`] for a scale or a unit that is not finite
    /// and positive, and [`FigureError::GeometryUnreal`] for a ground point
    /// that is not finite.
    pub fn check(&self) -> Result<(), FigureError> {
        let (size, finite) = match *self {
            Self::Oblique { scale, at } => (scale, at.0.is_finite() && at.1.is_finite()),
            Self::Perspective { ground, units, .. } => {
                (units, ground.iter().all(|value| value.is_finite()))
            }
        };
        if !size.is_finite() || size <= 0.0 {
            return Err(FigureError::ScaleUnreal);
        }
        if !finite {
            return Err(FigureError::GeometryUnreal);
        }
        Ok(())
    }

    /// Where body-frame `offset` lands for a figure facing along `heading`,
    /// or `None` nearer than the camera can see.
    #[must_use]
    pub fn land(&self, heading: Heading, offset: Body) -> Option<Landed> {
        match *self {
            Self::Oblique { scale, at } => {
                let placed = project(heading, offset);
                Some(Landed {
                    x: at.0 + placed.dx * scale,
                    y: at.1 + placed.dy * scale,
                    key: placed.depth,
                    depth: f64::from(Depth::NEAREST.0),
                })
            }
            Self::Perspective {
                camera,
                ground,
                units,
            } => {
                let view = camera.view(in_world(ground, units, heading, offset));
                let at = camera.project(view)?;
                Some(Landed {
                    x: at.x,
                    y: at.y,
                    key: -view[2],
                    depth: at.depth,
                })
            }
        }
    }

    /// The unit direction from body-frame point `at` toward the eye, in the
    /// frame of a figure facing along `heading`.
    #[must_use]
    pub fn toward(&self, heading: Heading, at: Body) -> Body {
        let Self::Perspective {
            camera,
            ground,
            units,
        } = *self
        else {
            return toward_camera(heading);
        };
        let (eye, world) = (camera.eye(), in_world(ground, units, heading, at));
        let (east, south) = (eye[0] - world[0], eye[1] - world[1]);
        let Heading {
            east: ex,
            south: sx,
        } = heading;
        let toward = Body::new(
            east * ex + south * sx,
            east * sx - south * ex,
            eye[2] - world[2],
        );
        let length = toward.length();
        if length > 0.0 {
            toward.scaled(1.0 / length)
        } else {
            toward_camera(heading)
        }
    }
}

/// Where body-frame `offset` is in the world, for a figure standing at
/// `ground` facing along `heading`, `units` world units a figure unit.
fn in_world(ground: [f64; 3], units: f64, heading: Heading, offset: Body) -> [f64; 3] {
    let (east, south) = plan(heading, offset);
    [
        ground[0] + east * units,
        ground[1] + south * units,
        ground[2] + offset.up * units,
    ]
}

#[cfg(test)]
#[path = "frame/tests.rs"]
mod tests;
