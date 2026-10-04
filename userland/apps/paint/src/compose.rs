//! Laying a picture's layers together: what the window shows of them, what a
//! merge makes of two, and what a format of one picture is written as.
//!
//! Each visible layer is laid over those beneath it source-over, its opacity
//! weighing its alpha. Source-over is associative, so layers laid together
//! onto nothing and then over the rest show as they did apart, to within the
//! rounding of a level, which is what lets a merge keep the picture's look.

use tairix_image::{over, Rgba8};
use tairix_raster::div255;
use tairix_util::fallible;

use crate::canvas::{Canvas, CanvasBuilder, Kind, OutOfMemory, Sample};
use crate::document::Layer;
use crate::mask::scale;

/// `above`, a layer's colour shown at `opacity`, laid over `below`.
#[must_use]
pub fn laid(below: Rgba8, above: Rgba8, opacity: u8) -> Rgba8 {
    over(
        below,
        [above[0], above[1], above[2], scale(above[3], opacity)],
    )
}

/// The colour `share` 255ths of the way from `from` to `to`, each colour
/// weighed by its alpha, so one fading to clear keeps its hue rather than
/// taking the clear one's meaningless colour.
#[must_use]
pub fn between(from: Rgba8, to: Rgba8, share: u8) -> Rgba8 {
    let (near, far) = (u32::from(u8::MAX - share), u32::from(share));
    let weight = u32::from(from[3]) * near + u32::from(to[3]) * far;
    if weight == 0 {
        return [0; 4];
    }
    let channel = |at: usize| {
        let sum = u32::from(from[at]) * u32::from(from[3]) * near
            + u32::from(to[at]) * u32::from(to[3]) * far;
        u8::try_from((sum + weight / 2) / weight).unwrap_or(u8::MAX)
    };
    [channel(0), channel(1), channel(2), div255(weight)]
}

/// What `layers` show together across a run of pixels, into `out`: each
/// visible layer's colours there, read through `read` into `scratch` — save
/// the layer `active` names, whose colours are given as it is shown — laid
/// over those beneath.
pub fn compose_run(
    layers: &[Layer],
    active: Option<(usize, &[Rgba8])>,
    out: &mut [Rgba8],
    scratch: &mut [Rgba8],
    mut read: impl FnMut(&Canvas, &mut [Rgba8]),
) {
    out.fill([0; 4]);
    for (index, layer) in layers.iter().enumerate() {
        if !layer.visible || layer.opacity == 0 {
            continue;
        }
        let colours: &[Rgba8] = match active {
            Some((at, shown)) if at == index => shown,
            _ => {
                read(&layer.canvas, scratch);
                scratch
            }
        };
        for (colour, &above) in out.iter_mut().zip(colours) {
            *colour = laid(*colour, above, layer.opacity);
        }
    }
}

/// `layers` laid together onto nothing, as one canvas: the one layer that
/// shows, its pixels shared, where it shows them alone and wholly.
///
/// # Errors
///
/// [`OutOfMemory`] when the canvas or a row of it cannot be had.
pub fn flatten(layers: &[Layer]) -> Result<Canvas, OutOfMemory> {
    let Some(first) = layers.first() else {
        return Err(OutOfMemory);
    };
    let mut shown = layers
        .iter()
        .filter(|layer| layer.visible && layer.opacity > 0);
    if let (Some(only), None) = (shown.next(), shown.next()) {
        if only.opacity == u8::MAX {
            return only.canvas.try_clone();
        }
    }
    let (width, height) = (first.canvas.width(), first.canvas.height());
    let mut built = CanvasBuilder::new(width, height, Kind::Rgba, Sample::Rgba([0; 4]))
        .map_err(|_| OutOfMemory)?;
    let length = width as usize;
    let mut out = fallible::filled(length, [0u8; 4]).ok_or(OutOfMemory)?;
    let mut scratch = fallible::filled(length, [0u8; 4]).ok_or(OutOfMemory)?;
    for y in 0..height {
        compose_run(layers, None, &mut out, &mut scratch, |canvas, into| {
            canvas.row_colours(y, 0, into);
        });
        built.row(y, out.as_flattened(), &[]);
    }
    Ok(built.finish())
}

#[cfg(test)]
#[path = "compose_tests.rs"]
mod tests;
