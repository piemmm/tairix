//! A picture drawn through an affine transform: the one transformed blit.
//!
//! Each destination pixel the transformed picture can touch maps its centre
//! back into the source and samples it bilinearly in premultiplied space, with
//! everything outside the source transparent. Sampling premultiplied keeps a
//! translucent edge's colour honest, and treating the outside as transparent is
//! what anti-aliases a turned edge against whatever lies beneath it.

use tairix_util::mathf;

use crate::affine::Affine;
use crate::color::Pixel;
use crate::surface::Surface;

impl Surface {
    /// Composite `src` over this surface through `transform`, which maps
    /// `src`'s pixel coordinates to this surface's, clipped to the surface and
    /// its clip window. A transform that collapses area draws nothing.
    pub fn blit_transformed(&mut self, src: &Surface, transform: Affine) {
        let Some(inverse) = transform.invert() else {
            return;
        };
        let Some((x0, y0, x1, y1)) = reach(src, transform) else {
            return;
        };
        let (x1, y1) = (x1.min(self.width()), y1.min(self.height()));
        if x1 <= x0 {
            return;
        }
        for y in y0..y1 {
            let Some((first, span)) = self.row_span_mut(y, x0, x1 - x0) else {
                continue;
            };
            for (x, dst) in (first..).zip(span.iter_mut()) {
                let (u, v) = inverse.apply((f64::from(x) + 0.5, f64::from(y) + 0.5));
                let sample = bilinear(src, u - 0.5, v - 0.5);
                if sample.a != 0 {
                    *dst = sample.over(*dst);
                }
            }
        }
    }
}

/// The destination pixels `src` drawn through `transform` can touch, as
/// `(x0, y0, x1, y1)` with the far edges exclusive, or `None` when it touches
/// none: the bounding box of `src` grown by the half pixel a bilinear edge
/// blends into, mapped — so the edge is whole at any scale — and widened by a
/// pixel for rounding.
fn reach(src: &Surface, transform: Affine) -> Option<(u32, u32, u32, u32)> {
    let (w, h) = (f64::from(src.width()) + 0.5, f64::from(src.height()) + 0.5);
    let corners =
        [(-0.5, -0.5), (w, -0.5), (-0.5, h), (w, h)].map(|corner| transform.apply(corner));
    let (mut left, mut top, mut right, mut bottom) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y) in corners {
        left = mathf::fmin(left, x);
        top = mathf::fmin(top, y);
        right = mathf::fmax(right, x);
        bottom = mathf::fmax(bottom, y);
    }
    // A corner far off either side clamps to the edge rather than wrapping.
    let edge = |value: f64| u32::try_from(mathf::round_i32(value).max(0)).unwrap_or(0);
    let (x0, y0) = (
        edge(mathf::floor(left) - 1.0),
        edge(mathf::floor(top) - 1.0),
    );
    let (x1, y1) = (
        edge(mathf::ceil(right) + 1.0),
        edge(mathf::ceil(bottom) + 1.0),
    );
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
}

/// `src` sampled at `(u, v)` — pixel `(i, j)`'s centre is `(i, j)` here — by
/// weighing the four pixels around it, everything outside `src` transparent.
fn bilinear(src: &Surface, u: f64, v: f64) -> Pixel {
    let (fu, fv) = (mathf::floor(u), mathf::floor(v));
    let (left, top) = (
        i64::from(mathf::round_i32(fu)),
        i64::from(mathf::round_i32(fv)),
    );
    let (width, height) = (i64::from(src.width()), i64::from(src.height()));
    if left < -1 || top < -1 || left >= width || top >= height {
        return Pixel::TRANSPARENT;
    }
    let weight = |fraction: f64| {
        u32::try_from(mathf::round_i32(fraction * 256.0).clamp(0, 256)).unwrap_or(0)
    };
    let (wx, wy) = (weight(u - fu), weight(v - fv));
    let at = |x: i64, y: i64| -> Pixel {
        if x < 0 || y < 0 || x >= width || y >= height {
            return Pixel::TRANSPARENT;
        }
        usize::try_from(y * width + x)
            .ok()
            .and_then(|index| src.pixels().get(index).copied())
            .unwrap_or(Pixel::TRANSPARENT)
    };
    let quad = [
        (at(left, top), (256 - wx) * (256 - wy)),
        (at(left + 1, top), wx * (256 - wy)),
        (at(left, top + 1), (256 - wx) * wy),
        (at(left + 1, top + 1), wx * wy),
    ];
    let channel = |of: fn(Pixel) -> u8| {
        let sum: u32 = quad
            .iter()
            .map(|&(pixel, w)| u32::from(of(pixel)) * w)
            .sum();
        u8::try_from((sum + (1 << 15)) >> 16).unwrap_or(u8::MAX)
    };
    Pixel {
        r: channel(|p| p.r),
        g: channel(|p| p.g),
        b: channel(|p| p.b),
        a: channel(|p| p.a),
    }
}

#[cfg(test)]
#[path = "transformed_tests.rs"]
mod tests;
