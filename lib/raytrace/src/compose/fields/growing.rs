//! A plant built part by part into a prototype, keeping how tall it stands
//! and how far it reaches about its foot as it grows, which a stand of it
//! walks by.

use tairix_util::mathf;

use crate::prototype::{Assembly, Building, Part, Tube};
use crate::vector::{share, Vec3};

/// A plant being built: its parts, and how tall it stands over its foot and
/// how far it reaches about it so far.
pub(super) struct Growing {
    pub(super) assembly: Assembly,
    top: f64,
    reach: f64,
}

impl Growing {
    /// A plant with room made for `parts` parts and `vertices` vertices;
    /// `None` when the heap will not hold them.
    pub(super) fn with_room(parts: usize, vertices: usize) -> Option<Self> {
        Some(Self {
            assembly: Assembly::with_room(parts, vertices)?,
            top: 0.0,
            reach: 0.0,
        })
    }

    /// Take in what stands within `radius` of `point` as part of the plant.
    pub(super) fn reaching(&mut self, point: Vec3, radius: f64) {
        self.top = self.top.max(point.y + radius);
        self.reach = self.reach.max(mathf::hypot(point.x, point.z) + radius);
    }

    /// A limb running through `points` from its foot, tapering from
    /// `radii.0` to `radii.1`, in `material` and keyed `key`.
    pub(super) fn limb(
        &mut self,
        points: &[Vec3],
        radii: (f64, f64),
        (material, key): (u16, u32),
    ) -> Option<()> {
        let pieces = points.len().saturating_sub(1).max(1);
        let mut stem = 0.0;
        for (index, pair) in points.windows(2).enumerate() {
            let [from, to] = pair else {
                continue;
            };
            let thick = |at: usize| radii.0 + (radii.1 - radii.0) * share(at, pieces);
            let (near, far) = (thick(index), thick(index + 1));
            self.reaching(*from, near);
            self.reaching(*to, far);
            let along = (*to - *from).length();
            let across = (*to - *from).cross(Vec3::UP);
            let side = if across.length() > 1e-9 {
                across.normalized()
            } else {
                Vec3::new(1.0, 0.0, 0.0)
            };
            self.assembly.push(Part::Tube(Tube::new(
                (*from, *to),
                ((near, far), (stem, stem + along)),
                (material, key),
                side,
            )))?;
            stem += along;
        }
        Some(())
    }

    /// What it is built of, and how tall it stands and how far it reaches
    /// about its foot; `None` when the heap will not hold it.
    pub(super) fn finish(self) -> Option<(Building, f64, f64)> {
        Some((self.assembly.finish()?, self.top, self.reach))
    }
}

#[cfg(test)]
#[path = "growing_tests.rs"]
mod tests;
