//! The application's half of a window's pixels: the shared frame region it
//! presents from.
//!
//! Every windowed app needs the same three-step dance — create a region,
//! grant it to the reserved window endpoint, hand the grant to `Create` — and
//! the same `unsafe` slice over the mapping it got back. It lives here so
//! there is one of each rather than one per app, and so the *release* path
//! below exists once.
//!
//! # Releasing, and coming back
//!
//! A window's pixels exist three times over: the app's render target, this
//! region, and the session's own converted copy. When nobody can see the
//! window and the machine is short of memory the session gives its copy back
//! and unmaps this region — but the pages only go when *both* sides let go,
//! so it tells the app ([`tairix_abi::window_ipc::WindowEvent::ContentReleased`])
//! and the app answers
//! with [`WindowFrames::release`]. A hidden 4K window then costs nothing
//! instead of sixty megabytes.
//!
//! Coming back is the present that follows the session's next redraw request:
//! it finds the region released, makes a fresh one, and re-attaches it with
//! the ordinary resize request. The app's paint code does not change.

use tairix_abi::driver::display::DisplayMode;
use tairix_abi::window_ipc::WINDOW_ENDPOINT;
use tairix_rt::shm::SharedRegion;
#[cfg(any(test, freestanding))]
use {
    crate::client::{WindowClient, WindowTransport},
    tairix_abi::driver::display::DamageList,
    tairix_abi::Errno,
    tairix_display::winframe::{self, damage_in},
    tairix_display::{add_damage, stale_frames, SERIAL},
    tairix_geometry::{Rect, Region, StaleFrames},
    tairix_raster::Surface,
};

/// One live mapping of a window's frame region, with the grant the session
/// maps it through.
struct Mapped {
    region: SharedRegion,
    grant: u64,
}

/// A window's shared frame region, or nothing while released.
///
/// Dropping it unmaps, so a window's frames never outlive the value that owns
/// them.
pub struct WindowFrames {
    mapped: Option<Mapped>,
    len: usize,
}

impl WindowFrames {
    /// Create a `len`-byte region and grant it to the window endpoint.
    ///
    /// `None` on any refusal, unmapping a region that mapped but could not be
    /// granted, so a refused allocation never leaves pinned memory behind.
    #[must_use]
    pub fn create(len: usize) -> Option<Self> {
        Some(Self {
            mapped: Some(Self::map(len)?),
            len,
        })
    }

    /// The grant handle a `Create`, `CreatePopup`, or `Resize` request names,
    /// or `None` while released.
    #[must_use]
    pub const fn grant(&self) -> Option<u64> {
        match &self.mapped {
            Some(mapped) => Some(mapped.grant),
            None => None,
        }
    }

    /// Whether the region holds no mapping, because the app released it after
    /// the session did.
    #[must_use]
    pub const fn is_released(&self) -> bool {
        self.mapped.is_none()
    }

    /// The region's bytes, or `None` while released.
    pub fn pixels(&mut self) -> Option<&mut [u8]> {
        Some(self.mapped.as_mut()?.region.bytes_mut())
    }

    /// Give the region back, because the session has released its side.
    ///
    /// Both halves have to go for the pages to be freed at all, which is the
    /// whole point: a mapping either side keeps holds every page of it.
    pub fn release(&mut self) {
        self.mapped = None;
    }

    /// Re-create the region at the size it was and grant it again, reporting
    /// the fresh grant handle for the caller to re-attach with.
    ///
    /// `None` if the region could not be re-created, which leaves it released
    /// rather than half-attached: the caller draws nothing this frame and the
    /// window shows through, exactly as it did while released.
    pub fn reattach(&mut self) -> Option<u64> {
        self.release();
        let mapped = Self::map(self.len)?;
        let grant = mapped.grant;
        self.mapped = Some(mapped);
        Some(grant)
    }

    fn map(len: usize) -> Option<Mapped> {
        let (region, grant) = granted_region(len)?;
        Some(Mapped { region, grant })
    }
}

/// A fresh `len`-byte region granted to the session's window endpoint, and
/// the grant's handle; `None` when the kernel refused either.
pub(crate) fn granted_region(len: usize) -> Option<(SharedRegion, u64)> {
    let region = SharedRegion::create(len)?;
    let grant = tairix_rt::shm_grant(region.id(), WINDOW_ENDPOINT);
    if grant < 1 {
        // Dropping the region unmaps it, so a grant refusal never leaves
        // pinned memory behind.
        return None;
    }
    #[allow(clippy::cast_sign_loss)] // `grant >= 1` checked above; it is a kernel handle.
    Some((region, grant as u64))
}

/// Total bytes a `frame_count`-frame region shaped as `mode` needs, or `None`
/// when that product does not fit an address.
///
/// Checked rather than saturating, because either failure mode is fatal to the
/// caller: a wrapped length asks for a region too small for the window it
/// describes, and a saturated one for a region no machine can map. On a 32-bit
/// target the product of two `u32` dimensions genuinely does not fit.
#[must_use]
pub fn region_bytes(mode: &DisplayMode, frame_count: u32) -> Option<usize> {
    usize::try_from(mode.stride_bytes)
        .ok()?
        .checked_mul(usize::try_from(mode.height_px).ok()?)?
        .checked_mul(usize::try_from(frame_count).ok()?)
}

/// Frames in a window's shared region: the one the session may still be
/// reading, and the one the app draws its next present into.
pub const FRAME_COUNT: u32 = 2;

/// What an application keeps of its window's frames between presents: the
/// frame the next present is drawn into, and what each lacks of the picture.
#[cfg(any(test, freestanding))]
pub(crate) struct FrameRing {
    stale: StaleFrames,
    back: usize,
    /// The change a present carries, held so a present allocates none.
    changed: Region,
}

#[cfg(any(test, freestanding))]
impl FrameRing {
    /// The ring of a fresh region shaped as `mode`.
    pub(crate) fn new(mode: &DisplayMode) -> Self {
        Self {
            stale: stale_frames(FRAME_COUNT as usize, mode),
            back: 0,
            changed: Region::new(),
        }
    }

    /// Every frame lacks all of a fresh region shaped as `mode`, and the
    /// first is drawn next.
    pub(crate) fn reset(&mut self, mode: &DisplayMode) {
        self.stale
            .reset(Rect::new(0, 0, mode.width_px, mode.height_px));
        self.back = 0;
    }

    /// Present `damage` as window `window`'s next frame, which `draw` brings
    /// up to date given its index in the region and the ring's record.
    ///
    /// The session reads the frame a present names until a later present
    /// replaces it, so the window's previous present is answered before the
    /// other frame is drawn.
    ///
    /// # Errors
    ///
    /// `draw`'s refusal, and the session's refusal of the present.
    pub(crate) fn present<T: WindowTransport>(
        &mut self,
        client: &mut WindowClient<T>,
        window: u64,
        damage: DamageList,
        draw: impl FnOnce(usize, (&mut StaleFrames, &mut Region)) -> Result<(), Errno>,
    ) -> Result<(), Errno> {
        client.await_present(window)?;
        draw(self.back, (&mut self.stale, &mut self.changed))?;
        let index = u32::try_from(self.back).map_err(|_| Errno::OutOfRange)?;
        client.present(window, index, damage)?;
        self.back = (self.back + 1) % FRAME_COUNT as usize;
        Ok(())
    }
}

/// Bring frame `index` of `pixels`, a region of frames shaped as `mode`, up to
/// date from `surface` for a present of `damage`: everything the frame lacks,
/// as `stale` records it, and `damage`, encoded from `surface`. `changed` is
/// scratch the caller keeps, so a present allocates nothing.
///
/// # Errors
///
/// [`Errno::OutOfRange`] for a frame the region does not hold, and whatever
/// the frame codec refuses — the frame then still lacks all of it.
#[cfg(any(test, freestanding))]
pub(crate) fn draw_frame(
    pixels: &mut [u8],
    index: usize,
    (stale, changed): (&mut StaleFrames, &mut Region),
    surface: &Surface,
    mode: &DisplayMode,
    damage: &DamageList,
) -> Result<(), Errno> {
    let frame_len = region_bytes(mode, 1).ok_or(Errno::OutOfRange)?;
    let start = index.checked_mul(frame_len).ok_or(Errno::OutOfRange)?;
    let frame = pixels
        .get_mut(start..start.saturating_add(frame_len))
        .ok_or(Errno::OutOfRange)?;
    changed.clear();
    add_damage(
        changed,
        damage.rects(),
        Rect::new(0, 0, mode.width_px, mode.height_px),
    );
    stale.bring_up_to_date(index, changed, Errno::OutOfRange, |refresh| {
        for rect in refresh.rects() {
            if let Some(rect) = damage_in(mode, *rect) {
                winframe::encode(surface, frame, mode, rect, &SERIAL)?;
            }
        }
        Ok(())
    })
}

#[cfg(test)]
#[path = "frames_tests.rs"]
mod tests;
