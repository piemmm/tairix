//! A window's view: a picture its client's frames carry after the window's
//! own rows, which the window shows enlarged into a rectangle of its content.
//!
//! The picture is kept as the client presented it, opaque. Where the display
//! engine shows it on a layer of its own the engine enlarges it; everywhere
//! else it is enlarged into the window's content here, through the one frame
//! scaler the emulators' own pictures go through, so every reader of the
//! content sees it.

use core::ops::Range;

use tairix_abi::driver::display::{DamageRect, DisplayFormat, DisplayMode};
use tairix_abi::window_ipc::{ViewFilter, WindowView};
use tairix_abi::Errno;
use tairix_crt::screen::{Scaling, Screen};
use tairix_raster::resample::Region as Target;

use crate::geometry::Rect;
use crate::surface::Surface;

/// A window's view: what its client last presented of it, and whether the
/// window's content holds that enlarged.
#[derive(Debug)]
pub(crate) struct View {
    spec: WindowView,
    picture: Screen,
    /// The content's placement holds the picture as last presented.
    overlaid: bool,
}

impl View {
    /// A view shaped as `spec`, its picture black until presented.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] where its picture cannot be held.
    pub(crate) fn new(spec: WindowView) -> Result<Self, Errno> {
        let mut picture = Screen::default();
        if !picture.reshape(spec.width_px, spec.height_px) {
            return Err(Errno::OutOfMemory);
        }
        picture.set_scaling(scaling(spec.filter));
        Ok(Self {
            spec,
            picture,
            overlaid: false,
        })
    }

    /// Be shaped as `spec` from now on, the picture kept while its size is.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] where a picture of a new size cannot be held,
    /// the view left as it was.
    pub(crate) fn respec(&mut self, spec: WindowView) -> Result<(), Errno> {
        if !self.picture.reshape(spec.width_px, spec.height_px) {
            return Err(Errno::OutOfMemory);
        }
        self.picture.set_scaling(scaling(spec.filter));
        self.spec = spec;
        self.invalidate();
        Ok(())
    }

    /// How the client shaped it.
    pub(crate) const fn spec(&self) -> &WindowView {
        &self.spec
    }

    /// Where the window shows it, in the window's content.
    pub(crate) fn placement(&self) -> Rect {
        let at = |value: u32| i32::try_from(value).unwrap_or(i32::MAX);
        let placement = self.spec.placement;
        Rect::new(
            at(placement.x),
            at(placement.y),
            placement.width_px,
            placement.height_px,
        )
    }

    /// The picture's width and height.
    pub(crate) const fn size(&self) -> (u32, u32) {
        (self.spec.width_px, self.spec.height_px)
    }

    /// The picture's opaque RGBA8 dots, row after row.
    pub(crate) fn pixels(&self) -> &[u8] {
        self.picture.pixels()
    }

    /// Whether the content's placement holds the picture as last presented.
    pub(crate) const fn overlaid(&self) -> bool {
        self.overlaid
    }

    /// Take `damage` of `frame`, the view's rows shaped as `surface`, into
    /// the picture, opaque whatever alpha they carry. Every rectangle is
    /// checked before any is taken, so a refusal changes nothing.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for damage past the picture or rows past
    /// the frame, and [`Errno::OutOfRange`] for a surface not of the
    /// picture's size.
    pub(crate) fn take(
        &mut self,
        surface: &DisplayMode,
        frame: &[u8],
        damage: &[DamageRect],
    ) -> Result<(), Errno> {
        if (surface.width_px, surface.height_px) != self.size() {
            return Err(Errno::OutOfRange);
        }
        let stride = usize::try_from(surface.stride_bytes).map_err(|_| Errno::LengthOutOfRange)?;
        let (width, height) = self.size();
        let span = |rect: &DamageRect| -> Option<(Range<usize>, Range<u32>)> {
            let within = |at: u32, len: u32, extent: u32| {
                u64::from(at) + u64::from(len) <= u64::from(extent)
            };
            if !within(rect.x, rect.width_px, width) || !within(rect.y, rect.height_px, height) {
                return None;
            }
            let x = usize::try_from(rect.x).ok()?.checked_mul(4)?;
            let columns = x..x.checked_add(usize::try_from(rect.width_px).ok()?.checked_mul(4)?)?;
            let last = usize::try_from(rect.y.checked_add(rect.height_px)?.checked_sub(1)?).ok()?;
            let end = last.checked_mul(stride)?.checked_add(columns.end)?;
            (end <= frame.len()).then(|| (columns, rect.y..rect.y + rect.height_px))
        };
        if damage.iter().any(|rect| span(rect).is_none()) {
            return Err(Errno::LengthOutOfRange);
        }
        let swap = surface.format == DisplayFormat::Bgra8888;
        for (columns, rows) in damage.iter().filter_map(span) {
            for row in rows {
                let start = usize::try_from(row)
                    .unwrap_or(usize::MAX)
                    .saturating_mul(stride);
                let (Some(from), Some(dots)) = (
                    frame.get(start + columns.start..start + columns.end),
                    self.picture.row_mut(row),
                ) else {
                    continue;
                };
                let Some(into) = dots.get_mut(columns.clone()) else {
                    continue;
                };
                for (out, dot) in into
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(from.as_chunks::<4>().0)
                {
                    let [first, green, third, _] = *dot;
                    *out = if swap {
                        [third, green, first, u8::MAX]
                    } else {
                        [first, green, third, u8::MAX]
                    };
                }
            }
        }
        self.overlaid = false;
        Ok(())
    }

    /// Enlarge into `content` at the placement what it lacks of the picture,
    /// answering the rectangle of the content drawn, or `None` for none.
    pub(crate) fn overlay(&mut self, content: &mut Surface) -> Option<Rect> {
        let placement = self.spec.placement;
        let target = Target {
            x: placement.x,
            y: placement.y,
            width: placement.width_px,
            height: placement.height_px,
        };
        let rows = self.picture.draw(content, target).ok()?;
        self.overlaid = true;
        let top = self.placement().top().saturating_add_unsigned(rows.start);
        (!rows.is_empty()).then(|| {
            Rect::new(
                self.placement().left(),
                top,
                placement.width_px,
                rows.end - rows.start,
            )
        })
    }

    /// Forget what the content holds of the picture, its placement having been
    /// overwritten or lost: the next overlay draws it whole.
    pub(crate) fn invalidate(&mut self) {
        self.picture.invalidate();
        self.overlaid = false;
    }

    /// The picture, overwritten, for the caller to drop: a client's screen
    /// leaves no copy in memory the window manager lets go.
    fn take_picture_wiped(&mut self) -> Screen {
        let mut picture = core::mem::take(&mut self.picture);
        picture.wipe();
        picture
    }
}

impl Drop for View {
    fn drop(&mut self) {
        drop(self.take_picture_wiped());
    }
}

/// How software enlarges a view the client asked to be resampled `filter`.
const fn scaling(filter: ViewFilter) -> Scaling {
    match filter {
        ViewFilter::Sharp => Scaling::Sharp,
        ViewFilter::Smooth => Scaling::Smooth,
    }
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
