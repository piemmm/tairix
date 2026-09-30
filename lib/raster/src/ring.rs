//! Rounded rings: the band between a rounded rectangle and its concentric
//! inset.
//!
//! A ring is what an edge is drawn with — a window's bevelled rim, a focus
//! ring that follows a plate's corners, the rim a surface lays last as its own
//! edge. All of them walk the same band: each row only from where the outer
//! shape first reaches in to where the inner one covers it wholly, so a ring
//! costs its own area rather than the rectangle it encloses — a circle's as
//! much as a plate's. Coverage comes from the one [`round_rect_coverage`],
//! taken per corner from the quadrant of a `2r`-sided square, which is what
//! lets the top and bottom corners round by different radii.

use crate::color::{div255, mix, Color, Pixel};
use crate::dither::DitherRow;
use crate::round::{round_rect_coverage, round_rect_radius, round_rect_row_reach, RowReach};
use crate::scan::MAX_DRAWING_EXTENT;
use crate::surface::Surface;

/// The band between a rounded rectangle and its concentric inset, `thickness`
/// in from it on every side.
///
/// Each radius is clamped to half the rectangle's shorter side, exactly as
/// [`round_rect_radius`] clamps one, and the inset's corners round by the
/// outer radius less the thickness, so the band keeps its weight around every
/// arc.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Ring {
    /// Radius of the two top corners; `0` is square.
    pub top_radius: u32,
    /// Radius of the two bottom corners; `0` is square.
    pub bottom_radius: u32,
    /// How far the inner edge sits inside the outer one.
    pub thickness: u32,
}

impl Ring {
    /// A ring whose four corners all round by `radius`.
    #[must_use]
    pub const fn uniform(radius: u32, thickness: u32) -> Self {
        Self {
            top_radius: radius,
            bottom_radius: radius,
            thickness,
        }
    }
}

/// What [`Surface::wash_ring`] composites over the band.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RingInk {
    /// One colour, all the way round.
    Solid(Color),
    /// Lit by a key light at the upper left: `light` where the band's outward
    /// normal faces it and `shade` where it faces away, each in proportion to
    /// how squarely, and neither where the normal is edge-on to it.
    ///
    /// So the top and left edges take `light` and the bottom and right
    /// `shade`, the top-right and bottom-left arcs turn from one to the other
    /// through nothing at their midpoints, and a square corner is mitred: each
    /// half takes its own edge's tone, and the diagonal between them neither.
    Bevel {
        /// The wash of an edge facing the light.
        light: Color,
        /// The wash of an edge facing away from it.
        shade: Color,
    },
}

impl RingInk {
    /// Whether this ink would leave every pixel it reaches unchanged.
    const fn is_clear(self) -> bool {
        match self {
            Self::Solid(color) => color.a == 0,
            Self::Bevel { light, shade } => light.a == 0 && shade.a == 0,
        }
    }
}

/// One pixel of a placed ring: how much of it the outer and the inner shape
/// cover, and how squarely its edge faces the light.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct RingPixel {
    /// Coverage of the outer shape.
    pub(crate) outer: u8,
    /// Coverage of the inner shape; never more than `outer`.
    pub(crate) inner: u8,
    /// `255` where the outward normal faces the upper-left key light, `-255`
    /// where it faces away, `0` where it is edge-on to it.
    pub(crate) tone: i32,
}

impl RingPixel {
    /// A pixel neither shape reaches.
    const NOTHING: Self = Self {
        outer: 0,
        inner: 0,
        tone: 0,
    };

    /// Coverage of the band itself.
    pub(crate) const fn band(self) -> u8 {
        self.outer.saturating_sub(self.inner)
    }
}

/// A rounded rectangle whose top and bottom corners may round differently,
/// its radii already clamped.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Shape {
    width: u32,
    height: u32,
    top: u32,
    bottom: u32,
}

impl Shape {
    fn new(width: u32, height: u32, top: u32, bottom: u32) -> Self {
        Self {
            width,
            height,
            top: round_rect_radius(width, height, top),
            bottom: round_rect_radius(width, height, bottom),
        }
    }

    /// The radius of the corner zone row `ly` lies in, or `0` between them.
    const fn corner_radius(self, ly: u32) -> u32 {
        if ly < self.top {
            self.top
        } else if self.bottom > 0 && ly >= self.height - self.bottom {
            self.bottom
        } else {
            0
        }
    }

    /// The radius of row `ly`'s corner and the row of that corner's `2r`-sided
    /// square it samples, or `None` between the corners.
    fn corner_row(self, ly: u32) -> Option<(u32, u32)> {
        let r = self.corner_radius(ly);
        if r == 0 {
            return None;
        }
        let qy = if ly < r {
            ly
        } else {
            ly - (self.height - 2 * r)
        };
        Some((r, qy))
    }

    /// Coverage at `(lx, ly)`, which must lie inside the rectangle.
    ///
    /// A corner of radius `r` is the matching quadrant of a `2r`-sided square
    /// rounded by `r`, which is bit-for-bit that corner of any larger
    /// rectangle rounded by the same `r`.
    fn coverage(self, lx: u32, ly: u32) -> u8 {
        let Some((r, qy)) = self.corner_row(ly) else {
            return u8::MAX;
        };
        let qx = if lx < r {
            lx
        } else if lx >= self.width - r {
            lx - (self.width - 2 * r)
        } else {
            return u8::MAX;
        };
        round_rect_coverage(qx, qy, 2 * r, 2 * r, r)
    }

    /// How far in from either side row `ly` is reached and wholly covered.
    ///
    /// Between its corners the row is solid, so the corner square's own reach
    /// is the whole row's.
    fn reach(self, ly: u32) -> RowReach {
        self.corner_row(ly).map_or(RowReach::WHOLE, |(r, qy)| {
            round_rect_row_reach(qy, 2 * r, 2 * r, r)
        })
    }
}

/// A ring resolved against the rectangle it is drawn in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct RingGeometry {
    outer: Shape,
    /// The inset, or `None` where the band is as thick as the shape is deep
    /// and so is the whole of it.
    inner: Option<Shape>,
    thickness: u32,
    /// How far the corner centres stand in from the sides in the top and the
    /// bottom half: the radius, or the thickness where that is larger, which
    /// is what mitres a corner squarer than the band is thick.
    margin_top: u32,
    margin_bottom: u32,
}

/// One run of a ring row: where it starts and how long it is, in columns of
/// the ring's rectangle, and the pixel every column of it shares when it has
/// one.
type RingSpan = (u32, u32, Option<RingPixel>);

impl RingGeometry {
    pub(crate) fn new(width: u32, height: u32, ring: Ring) -> Self {
        let outer = Shape::new(width, height, ring.top_radius, ring.bottom_radius);
        // Bounded as a radius is, so the normals `tone` squares stay exact.
        let t = ring.thickness.min(MAX_DRAWING_EXTENT);
        let inner = width
            .checked_sub(t.saturating_mul(2))
            .zip(height.checked_sub(t.saturating_mul(2)))
            .filter(|&(w, h)| w > 0 && h > 0)
            .map(|(w, h)| {
                Shape::new(
                    w,
                    h,
                    outer.top.saturating_sub(t),
                    outer.bottom.saturating_sub(t),
                )
            });
        Self {
            outer,
            inner,
            thickness: t,
            margin_top: outer.top.max(t),
            margin_bottom: outer.bottom.max(t),
        }
    }

    /// Whether row `ly` is in the upper half, whose corners the top radius
    /// rounds and whose normals the top margin measures.
    fn in_top_half(&self, ly: u32) -> bool {
        u64::from(ly) * 2 + 1 < u64::from(self.outer.height)
    }

    /// The side margin of row `ly`'s half.
    fn margin(&self, ly: u32) -> u32 {
        if self.in_top_half(ly) {
            self.margin_top
        } else {
            self.margin_bottom
        }
    }

    /// Whether row `ly` lies in the ring's top or bottom band, where every
    /// column is ring.
    fn is_band_row(&self, ly: u32) -> bool {
        self.inner.is_none() || ly < self.thickness || ly >= self.outer.height - self.thickness
    }

    /// How far in from either side row `ly` is covered wholly by both shapes,
    /// the outer one from `outer_solid` in, so the ring changes nothing there;
    /// `None` for a row with no such stretch.
    fn hole(&self, ly: u32, outer_solid: u32) -> Option<u32> {
        let inner = self.inner.filter(|_| !self.is_band_row(ly))?;
        let t = self.thickness;
        let hole = (t + inner.reach(ly - t).solid).max(outer_solid);
        (hole.saturating_mul(2) < self.outer.width).then_some(hole)
    }

    /// The runs of row `ly` a walk visits, left to right.
    ///
    /// Each row is ring from where the outer shape first reaches in to where
    /// the inner one covers it wholly; `outside` adds the stretch past the
    /// outer shape, as the one pixel neither shape reaches. A band row is ring
    /// end to end, and between its two corners every column is the same
    /// pixel: wholly outer, not inner, facing straight up or down.
    fn spans(&self, ly: u32, outside: bool) -> [RingSpan; 5] {
        let w = self.outer.width;
        let none = (0, 0, None);
        let outer = self.outer.reach(ly);
        let reached = outer.reached.min(w / 2);
        let [beyond_left, beyond_right] = if outside && reached > 0 {
            let nothing = Some(RingPixel::NOTHING);
            [(0, reached, nothing), (w - reached, reached, nothing)]
        } else {
            [none, none]
        };
        let whole = [(reached, w - 2 * reached, None), none, none];
        let [left, middle, right] = if self.is_band_row(ly) {
            let edge = self.margin(ly).max(reached);
            if edge.saturating_mul(2) >= w {
                whole
            } else {
                let middle = RingPixel {
                    outer: u8::MAX,
                    inner: 0,
                    tone: self.tone(edge, ly),
                };
                let corner = edge - reached;
                [
                    (reached, corner, None),
                    (edge, w - 2 * edge, Some(middle)),
                    (w - edge, corner, None),
                ]
            }
        } else {
            match self.hole(ly, outer.solid) {
                Some(hole) => {
                    let side = hole.saturating_sub(reached);
                    [(reached, side, None), (w - hole, side, None), none]
                }
                None => whole,
            }
        };
        [beyond_left, left, middle, right, beyond_right]
    }

    /// Everything about the ring at `(lx, ly)`, which must lie inside the
    /// rectangle.
    pub(crate) fn pixel(&self, lx: u32, ly: u32) -> RingPixel {
        RingPixel {
            tone: self.tone(lx, ly),
            ..self.cover(lx, ly)
        }
    }

    /// How much of `(lx, ly)` the outer and the inner shape cover, its tone
    /// left edge-on for an ink that does not shade by it.
    fn cover(&self, lx: u32, ly: u32) -> RingPixel {
        let outer = self.outer.coverage(lx, ly);
        let t = self.thickness;
        let inner = match self.inner {
            Some(shape) if lx >= t && ly >= t && lx - t < shape.width && ly - t < shape.height => {
                shape.coverage(lx - t, ly - t).min(outer)
            }
            _ => 0,
        };
        RingPixel {
            outer,
            inner,
            tone: 0,
        }
    }

    /// How squarely the outward normal at `(lx, ly)` faces the upper-left key
    /// light: `-nx - ny` over the normal's length, the normal taken from the
    /// nearest point of the rectangle the corner centres span.
    ///
    /// In half-pixel units a pixel centre is an integer, and the length is
    /// taken 256 times finer than that so the integer square root costs no
    /// visible precision.
    fn tone(&self, lx: u32, ly: u32) -> i32 {
        let width = i64::from(self.outer.width) * 2;
        let height = i64::from(self.outer.height) * 2;
        let (px, py) = (i64::from(lx) * 2 + 1, i64::from(ly) * 2 + 1);
        let side = i64::from(self.margin(ly)) * 2;
        let top = i64::from(self.margin_top) * 2;
        let bottom = height - i64::from(self.margin_bottom) * 2;
        let nx = px - px.clamp(side, (width - side).max(side));
        let ny = py - py.clamp(top, bottom.max(top));
        let squared = (nx * nx + ny * ny).unsigned_abs().saturating_mul(1 << 16);
        let Ok(length) = i64::try_from(squared.isqrt()) else {
            return 0;
        };
        if length == 0 {
            return 0;
        }
        let tone = ((-nx - ny) * 255 * 256 / length).clamp(-255, 255);
        i32::try_from(tone).unwrap_or(0)
    }
}

impl Surface {
    /// Composite `ink` over `ring`, inset within `[x, x+w) × [y, y+h)`.
    ///
    /// Each pixel takes the ink scaled by the fraction of it the band covers,
    /// rounded at the surface row's ordered dither like every other wash, so a
    /// pixel wholly inside or outside the band is left bit-identical. The walk
    /// honours the clip window and the stated origin
    /// ([`with_origin`](Self::with_origin)), so a strip of a larger drawing
    /// holds exactly the pixels a whole-drawing paint would put there.
    pub fn wash_ring(&mut self, x: u32, y: u32, w: u32, h: u32, ring: Ring, ink: RingInk) {
        if ring.thickness == 0 || ink.is_clear() {
            return;
        }
        let walk = Walk {
            outside: false,
            toned: matches!(ink, RingInk::Bevel { .. }),
        };
        self.walk_ring((x, y, w, h), ring, walk, |pixel, dst, bias| {
            let band = pixel.band();
            let (color, weight) = match ink {
                RingInk::Solid(color) => (color, band),
                RingInk::Bevel { light, shade } => {
                    let (color, lit) = if pixel.tone >= 0 {
                        (light, pixel.tone)
                    } else {
                        (shade, -pixel.tone)
                    };
                    let lit = u32::try_from(lit).unwrap_or(0);
                    (color, div255(u32::from(band) * lit))
                }
            };
            if weight == 0 || color.a == 0 {
                return;
            }
            let alpha = div255(u32::from(color.a) * u32::from(weight));
            *dst = Color::rgba(color.r, color.g, color.b, alpha).over_biased(*dst, bias);
        });
    }

    /// Lay `ring` in `color` as the edge of the plate it encloses within
    /// `[x, x+w) × [y, y+h)`, over whatever has already been drawn there.
    ///
    /// The band is replaced by the rim, what lies inside survives only as far
    /// as the inner shape covers it, and nothing survives outside the outer
    /// one: each pixel is the rim laid over nothing, mixed toward what was
    /// there by the inner shape's coverage. That is the order a surface on a
    /// transparent buffer lays its plate in when what it draws does not shape
    /// itself — its ground square, its content, then this. Every pixel comes
    /// out as a plate laid first would have left it with the content kept
    /// inside, and content that strayed onto the rim or past the corners is cut
    /// away with one anti-aliased edge rather than two.
    pub fn frame_ring(&mut self, x: u32, y: u32, w: u32, h: u32, ring: Ring, color: Color) {
        let rim = color.premultiply();
        let walk = Walk {
            outside: true,
            toned: false,
        };
        self.walk_ring((x, y, w, h), ring, walk, |pixel, dst, bias| {
            let edge = mix(Pixel::TRANSPARENT, rim, pixel.outer, bias);
            *dst = mix(edge, *dst, pixel.inner, bias);
        });
    }

    /// Hand `paint` every pixel of `rect` the inner shape of `ring` does not
    /// wholly cover — and, unless `walk` takes the outside, that the outer
    /// shape reaches — with what the ring is there and the ordered-dither bias
    /// the pixel rounds at.
    fn walk_ring(
        &mut self,
        rect: (u32, u32, u32, u32),
        ring: Ring,
        walk: Walk,
        mut paint: impl FnMut(RingPixel, &mut Pixel, u32),
    ) {
        let (x, y, w, h) = rect;
        if w == 0 || h == 0 {
            return;
        }
        let Some((_, rows)) = self.admitted(x, y, w, h) else {
            return;
        };
        let geometry = RingGeometry::new(w, h, ring);
        for row in rows {
            let ly = row - y;
            let dither = DitherRow::at(row);
            for (from, len, shared) in geometry.spans(ly, walk.outside) {
                if len == 0 {
                    continue;
                }
                let Some((first, span)) = self.row_span_mut(row, x.saturating_add(from), len)
                else {
                    continue;
                };
                for (column, dst) in (first..).zip(span.iter_mut()) {
                    let pixel = shared.unwrap_or_else(|| {
                        if walk.toned {
                            geometry.pixel(column - x, ly)
                        } else {
                            geometry.cover(column - x, ly)
                        }
                    });
                    paint(pixel, dst, dither.bias(column));
                }
            }
        }
    }
}

/// What a walk over a ring visits and works out for each pixel.
#[derive(Copy, Clone, Debug)]
struct Walk {
    /// Whether the stretch past the outer shape is visited, for an ink that
    /// clears it.
    outside: bool,
    /// Whether each pixel's tone is worked out, for an ink that shades by it.
    toned: bool,
}

#[cfg(test)]
#[path = "ring_tests.rs"]
mod tests;
