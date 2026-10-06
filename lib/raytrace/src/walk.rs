//! A ray's walk across the square cells of a grid laid over the ground, one
//! cell at a time in the order it crosses them.

use tairix_util::mathf::fmin;

use crate::noise::cell;
use crate::vector::Ray;

/// Where a walk across a grid's cells has come: the cell it is in, where
/// along the ray it next crosses a wall across x and one across z, how far it
/// goes between such walls, and which way it steps across each.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct Walk {
    pub(crate) cell: (u32, u32),
    next: (f64, f64),
    delta: (f64, f64),
    step: (u32, u32),
}

impl Walk {
    /// The walk of `ray` from `t` along it across the grid whose first
    /// corner lies at `origin` and whose cells are `side` across.
    pub(crate) fn from((origin, side): ((f64, f64), f64), ray: &Ray, t: f64) -> Self {
        let first = ray.at(t);
        Self::starting(
            (
                cell((first.x - origin.0) / side),
                cell((first.z - origin.1) / side),
            ),
            side,
            (ray, t),
        )
    }

    /// [`Walk::from`] across a grid `cells` a side, set on its nearest cell
    /// where rounding leaves a ray entering at an edge a hair beyond it.
    pub(crate) fn within(
        ((origin, side), cells): (((f64, f64), f64), u32),
        ray: &Ray,
        t: f64,
    ) -> Self {
        let first = ray.at(t);
        let onto = |place: f64| {
            let at = if place < 0.0 {
                0
            } else {
                cell(place).0.min(cells.saturating_sub(1))
            };
            (at, place - f64::from(at))
        };
        Self::starting(
            (
                onto((first.x - origin.0) / side),
                onto((first.z - origin.1) / side),
            ),
            side,
            (ray, t),
        )
    }

    /// The walk of `ray` from `t` along it, starting in the cells of its
    /// columns and rows `(cx, fx)` and `(cz, fz)` give, each how far across
    /// its cell the ray starts, on a grid of cells `side` across.
    fn starting(
        ((cx, fx), (cz, fz)): ((u32, f64), (u32, f64)),
        side: f64,
        (ray, t): (&Ray, f64),
    ) -> Self {
        let wall = |fraction: f64, dir: f64| {
            if dir.abs() < 1e-12 {
                f64::INFINITY
            } else {
                let to = if dir > 0.0 { 1.0 - fraction } else { fraction };
                t + to * side / dir.abs()
            }
        };
        let delta = |dir: f64| {
            if dir.abs() < 1e-12 {
                f64::INFINITY
            } else {
                side / dir.abs()
            }
        };
        let step = |dir: f64| if dir > 0.0 { 1 } else { u32::MAX };
        Self {
            cell: (cx, cz),
            next: (wall(fx, ray.dir.x), wall(fz, ray.dir.z)),
            delta: (delta(ray.dir.x), delta(ray.dir.z)),
            step: (step(ray.dir.x), step(ray.dir.z)),
        }
    }

    /// Where along the ray the walk leaves its cell.
    pub(crate) fn exit(&self) -> f64 {
        fmin(self.next.0, self.next.1)
    }

    /// On to the cell the ray is over `t` along it.
    pub(crate) fn advance(&mut self, t: f64) {
        while self.exit() <= t {
            self.step();
        }
    }

    /// On into the next cell: across x, or across z.
    pub(crate) fn step(&mut self) -> Stepped {
        if self.next.0 <= self.next.1 {
            self.cell.0 = self.cell.0.wrapping_add(self.step.0);
            self.next.0 += self.delta.0;
            Stepped::X(self.step.0)
        } else {
            self.cell.1 = self.cell.1.wrapping_add(self.step.1);
            self.next.1 += self.delta.1;
            Stepped::Z(self.step.1)
        }
    }
}

/// Which way a walk stepped, and by which wrapping increment: `1` forward or
/// `u32::MAX` back.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Stepped {
    X(u32),
    Z(u32),
}

#[cfg(test)]
#[path = "walk_tests.rs"]
mod tests;
