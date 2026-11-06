//! Stills: composed into a buffer the heap may refuse, wiped once let go, and
//! kept off the loop as a PNG in the chosen folder or on the clipboard.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::time::CivilTime;
use tairix_abi::window_ipc::CLIPBOARD_MAX_BYTES;
use tairix_geometry::REFERENCE_DPI;
use tairix_image::{Density, DensityUnit};
use tairix_tz::CivilClock;
use tairix_util::secret::{wipe, wipe_with};
use tairix_wm::{CaptureError, CaptureTap, Compositor, Pixel, Region, Scale, Surface};

use super::KeptFolder;
use crate::keep::{
    encode_png, folder_path, home_of, keep_bytes, keep_png, settle_leftovers, KeptFiles, Unkept,
};

/// What a still is called before the moment it was taken.
pub const SCREENSHOT_NAME: &str = "Screenshot";

/// A capture's pixels, wiped as they are let go: a capture may show anything
/// the user had on screen.
pub struct CaptureBuffer(Surface);

impl CaptureBuffer {
    /// A buffer of `width` × `height` pixels, or `None` where the heap refuses
    /// it.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Option<Self> {
        Surface::new(width, height).map(Self)
    }

    /// The pixels.
    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.0
    }

    /// The pixels, to compose into.
    pub fn surface_mut(&mut self) -> &mut Surface {
        &mut self.0
    }
}

impl Drop for CaptureBuffer {
    fn drop(&mut self) {
        wipe_with(self.0.pixels_mut(), Pixel::TRANSPARENT);
    }
}

impl fmt::Debug for CaptureBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CaptureBuffer({}x{})", self.0.width(), self.0.height())
    }
}

/// A still composed and waiting to be kept.
#[derive(Debug)]
pub struct StillShot {
    /// The picture.
    pub pixels: CaptureBuffer,
    /// The density its PNG records.
    pub density: Option<Density>,
    /// Whether it goes on the clipboard.
    pub clipboard: bool,
    /// The folder it is saved in — instead of the clipboard, where it is too
    /// large for it.
    pub folder: KeptFolder,
}

/// A still's PNG bound for the clipboard, wiped if it is let go instead.
pub struct ClipboardPng(Vec<u8>);

impl ClipboardPng {
    /// The bytes, for the clipboard to hold, which wipes them when it lets
    /// them go.
    #[must_use]
    pub fn into_bytes(mut self) -> Vec<u8> {
        core::mem::take(&mut self.0)
    }
}

impl Drop for ClipboardPng {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl fmt::Debug for ClipboardPng {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClipboardPng({} bytes)", self.0.len())
    }
}

/// What became of a still.
#[derive(Debug)]
pub enum StillKept {
    /// Saved at this path.
    Saved(String),
    /// Bound for the clipboard.
    Clipboard(ClipboardPng),
    /// Too large for the clipboard, so saved at this path instead.
    Oversize(String),
}

/// Compose the still `tap` asks for into a fresh buffer, the tap armed for
/// this one compose and disarmed after.
///
/// # Errors
///
/// [`CaptureError::OutOfMemory`] where the buffer is refused, and whatever
/// arming or composing refused.
pub fn compose_still(
    compositor: &mut Compositor,
    tap: CaptureTap,
) -> Result<CaptureBuffer, CaptureError> {
    let area = tap.area;
    let mut pixels =
        CaptureBuffer::new(area.width, area.height).ok_or(CaptureError::OutOfMemory)?;
    compositor.arm_capture(tap)?;
    let composed = compositor.compose_capture(pixels.surface_mut(), &Region::from(area));
    compositor.disarm_capture();
    composed.map(|()| pixels)
}

/// The density a still records: the reference density at the desktop's UI
/// scale, so the picture prints at the size it was seen.
#[must_use]
pub fn still_density(scale: Scale) -> Option<Density> {
    let dpi = scale.scale_length(REFERENCE_DPI);
    Density::whole(dpi, dpi, DensityUnit::Inch)
}

/// Keep `shot` in `home`, kept at `when` if the clock is set: on the
/// worker, since it encodes and writes.
///
/// # Errors
///
/// Why it was not kept.
pub fn keep_still(
    files: &mut dyn KeptFiles,
    home: Option<&str>,
    shot: &StillShot,
    when: Option<CivilTime>,
) -> Result<StillKept, Unkept> {
    keep_bounded(files, home, shot, when, CLIPBOARD_MAX_BYTES)
}

/// [`keep_still`], a still for the clipboard past `bound` bytes saved instead.
pub(super) fn keep_bounded(
    files: &mut dyn KeptFiles,
    home: Option<&str>,
    shot: &StillShot,
    when: Option<CivilTime>,
    bound: usize,
) -> Result<StillKept, Unkept> {
    let keeping = shot.folder.keeping(false);
    let picture = shot.pixels.surface();
    if !shot.clipboard {
        return keep_png(files, home, &keeping, picture, shot.density, when).map(StillKept::Saved);
    }
    let png = ClipboardPng(encode_png(picture, shot.density)?);
    if png.0.len() <= bound {
        return Ok(StillKept::Clipboard(png));
    }
    keep_bytes(files, home, &keeping, &png.0, when).map(StillKept::Oversize)
}

/// Settle what a session ended mid-capture left behind: every scratch file in
/// a capture folder is whole as far as it got, so it is given its name for
/// the moment it last changed, and an empty one — a recorder that died before
/// it wrote — is removed. The paths it took.
pub fn sweep_captures(
    files: &mut dyn KeptFiles,
    home: Option<&str>,
    civil: &CivilClock<'_>,
) -> Vec<String> {
    let mut swept = Vec::new();
    let Some(home) = home_of(home) else {
        return swept;
    };
    for folder in KeptFolder::ALL {
        let path = folder_path(home, folder.folders());
        for keeping in [folder.keeping(false), folder.keeping(true)] {
            settle_leftovers(files, &path, &keeping, civil, |kept| {
                if swept.try_reserve(1).is_ok() {
                    swept.push(kept);
                }
            });
        }
    }
    swept
}
