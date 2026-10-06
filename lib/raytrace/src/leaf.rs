//! The outlines of leaves and needles: what part of a flat blade's
//! plane a leaf covers.
//!
//! A leaf is drawn in its own coordinates: `u` from `0.0` where its stalk
//! meets it to `1.0` at its tip along the midrib, and `v` across it from
//! `-1.0` to `1.0` of its greatest half-width. Each outline is a half-width
//! profile along the midrib, shaped by the species — serrated, lobed, palmate
//! — or, for needles, the strips a shoot or a bundle of them covers.

use core::f64::consts::PI;

use tairix_util::mathf;

use crate::vector::power;

/// A leaf's outline.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Outline {
    /// Widest below the middle, rounded at the stalk, pointed at the tip, its
    /// edge cut in `teeth` teeth a side: birch, beech, cherry, poplar.
    Ovate { teeth: u8 },
    /// Long and narrow, widest at the middle: willow, olive, a palm's leaflet.
    Lanceolate,
    /// Widest toward the tip, its edge in `lobes` rounded lobes a side: oak.
    Lobed { lobes: u8 },
    /// Spread like a hand from a point near its base into `lobes` pointed
    /// lobes: maple.
    Palmate { lobes: u8 },
    /// A spruce's shoot: a twig down the midrib, needles set along it
    /// sloping forward, `count` a side.
    Shoot { count: u8 },
    /// A pine's bundle: `count` needles spreading from the base.
    Fascicle { count: u8 },
    /// Three heart-shaped leaflets from one point: clover.
    Trefoil,
    /// A dandelion's leaf: long, cut into teeth that point back to the base.
    Runcinate,
    /// A long leaf of even width drawn to a point, as a reed's or a
    /// reedmace's, from `from` to `to` 255ths of the way along it: a leaf
    /// arching through several flat pieces keeps the one outline.
    Strap { from: u8, to: u8 },
}

impl Outline {
    /// Whether the point `(u, v)` of the leaf's plane lies on the leaf.
    pub(crate) fn covers(self, u: f64, v: f64) -> bool {
        if !(0.0..=1.0).contains(&u) || !(-1.0..=1.0).contains(&v) {
            return false;
        }
        let across = v.abs();
        match self {
            Self::Ovate { teeth } => across <= ovate(u) * serrate(u, teeth),
            Self::Lanceolate => across <= lanceolate(u),
            Self::Lobed { lobes } => across <= lobed(u, lobes),
            Self::Palmate { lobes } => palmate(u, v, lobes),
            Self::Shoot { count } => shoot(u, across, count),
            Self::Fascicle { count } => fascicle(u, v, count),
            Self::Trefoil => trefoil(u, v),
            Self::Runcinate => across <= runcinate(u),
            Self::Strap { from, to } => across <= strap(along(u, (from, to))),
        }
    }

    /// How far across the midrib a point is, from `0.0` on it to `1.0` at the
    /// edge, for the veins and the fold a leaf's shading follows.
    pub(crate) fn off_midrib(self, u: f64, v: f64) -> f64 {
        let edge = match self {
            Self::Ovate { .. } => ovate(u),
            Self::Lanceolate => lanceolate(u),
            Self::Lobed { lobes } => lobed(u, lobes),
            Self::Runcinate => runcinate(u),
            Self::Strap { from, to } => strap(along(u, (from, to))),
            Self::Palmate { .. } | Self::Shoot { .. } | Self::Fascicle { .. } | Self::Trefoil => {
                1.0
            }
        };
        (v.abs() / edge.max(1e-6)).min(1.0)
    }
}

/// How far along a whole strap leaf a piece of it from `from` to `to`
/// 255ths of the way lies at `u` of its own length.
fn along(u: f64, (from, to): (u8, u8)) -> f64 {
    let (from, to) = (f64::from(from) / 255.0, f64::from(to) / 255.0);
    from + (to - from) * u
}

/// A strap leaf's half-width `w` of the way along it: full almost from its
/// sheathing base, drawn in over its last third to its point.
fn strap(w: f64) -> f64 {
    let base = (0.55 + 0.45 * w / 0.06).min(1.0);
    base * power(((1.0 - w) / 0.32).clamp(0.0, 1.0), 0.85)
}

fn ovate(u: f64) -> f64 {
    power(mathf::sin(PI * power(u, 0.78)), 0.85)
}

/// A serrated edge: `teeth` notches a side, each biting a little into the
/// profile.
fn serrate(u: f64, teeth: u8) -> f64 {
    if teeth == 0 {
        return 1.0;
    }
    let tooth = u * f64::from(teeth);
    let rise = tooth - mathf::floor(tooth);
    1.0 - 0.09 * rise
}

fn lanceolate(u: f64) -> f64 {
    power(mathf::sin(PI * u), 1.25) * (0.35 + 0.65 * power(u, 0.2))
}

fn lobed(u: f64, lobes: u8) -> f64 {
    let body = power(mathf::sin(PI * power(u, 1.35)), 0.8);
    let wave = mathf::cos(PI * (f64::from(lobes) + 0.5) * u).abs();
    body * (0.52 + 0.48 * power(wave, 0.55)) * (0.6 + 0.4 * u)
}

/// A palmate leaf, measured about the point a third of the way up its midrib
/// where its main veins spread from.
fn palmate(u: f64, v: f64, lobes: u8) -> bool {
    let (x, y) = (u - 0.3, v * 0.5);
    let reach = mathf::sqrt(x * x + y * y) / 0.7;
    if reach > 1.0 {
        return false;
    }
    // The angle from the midrib, `0` pointing to the tip; the base between
    // the lowest lobes is cut away.
    let angle = mathf::atan2(y, x).abs();
    if angle > 0.86 * PI {
        return reach < 0.12;
    }
    let lobe = mathf::cos(f64::from(lobes) * angle * 0.5).abs();
    reach <= 0.42 + 0.58 * power(lobe, 0.7)
}

/// A shoot: the twig within a sliver of the midrib, and needles either side
/// set at a forward slope.
fn shoot(u: f64, across: f64, count: u8) -> bool {
    if across < 0.08 {
        return true;
    }
    // Needles shorten toward the shoot's tip and its base.
    let reach = 0.35 + 0.65 * mathf::sin(PI * (0.08 + 0.88 * u));
    if across > reach {
        return false;
    }
    let along = u - across * 0.35;
    let needle = along * f64::from(count.max(1));
    needle - mathf::floor(needle) < 0.34
}

/// A bundle of needles fanning from the base.
fn fascicle(u: f64, v: f64, count: u8) -> bool {
    let spread = 0.5 * v;
    let angle = mathf::atan2(spread, u.max(1e-6));
    let reach = mathf::sqrt(u * u + spread * spread);
    if reach > 1.0 || angle.abs() > 0.5 {
        return false;
    }
    // Needles centred on angles set evenly either side of the midrib.
    let needle = (angle + 0.5) * f64::from(count.max(1)) - 0.5;
    (needle - mathf::round(needle)).abs() < 0.15 + 0.175 * (1.0 - reach)
}

/// Three leaflets about the leaf's middle, each notched at its end.
fn trefoil(u: f64, v: f64) -> bool {
    let (x, y) = (u - 0.5, v * 0.5);
    let reach = mathf::sqrt(x * x + y * y) / 0.5;
    let angle = mathf::atan2(y, x);
    let leaflet = mathf::cos(1.5 * angle).abs();
    let notch = 1.0 - 0.25 * power(mathf::cos(3.0 * angle).abs(), 8.0);
    reach <= power(leaflet, 0.6) * notch
}

fn runcinate(u: f64) -> f64 {
    let tooth = u * 5.0;
    let back = 1.0 - (tooth - mathf::floor(tooth));
    lanceolate(u) * (0.45 + 0.55 * power(back, 1.5))
}

#[cfg(test)]
#[path = "leaf_tests.rs"]
mod tests;
