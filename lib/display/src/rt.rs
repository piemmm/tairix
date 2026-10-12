//! The production, `tairix-rt`-backed halves of the protocol (feature `rt`).
//!
//! [`RtShmMapper`] is the one definition of "map a client's granted
//! shared-memory region through the kernel's `shm_map`, sized from the
//! kernel's own record of the region length — never the granting client's
//! claim" — shared by the framebuffer display service's serve loop and the
//! desktop session's window server (`plans/APPWIN.md` AW3), so the mapping
//! discipline cannot drift between the two. The mapping itself is the
//! runtime's [`MappedGrant`]; this narrows it to the read-only view a frame
//! region is, so nothing reached through a [`FrameRegion`] can write into a
//! client's frames.
//!
//! [`establish_frames`] is the client's: the one way a presenter — the
//! desktop session, the login screen — gets frames for the mode an output
//! runs, so no two can size, grant, or configure them differently.

use tairix_abi::display_ipc::DISPLAY_ENDPOINT;
use tairix_abi::driver::display::DisplayMode;
use tairix_abi::{Errno, ProcId};
use tairix_rt::shm::{MappedGrant, SharedRegion};

use crate::client::{DisplayClient, DisplayTransport, RemoteDisplay};
use crate::server::{FrameRegion, ShmMapper};

/// A client region mapped through `shm_map`, unmapped on drop (a
/// reconfigure, a closed window, or an observed lease loss releases the
/// old mapping).
pub struct RtShmRegion(MappedGrant);

impl FrameRegion for RtShmRegion {
    fn bytes(&self) -> &[u8] {
        self.0.bytes()
    }
}

/// The production [`ShmMapper`]: the kernel's `shm_map` of a granted
/// handle carried in-band by a `Configure` or window `Create`. Mapping
/// happens exactly once per region; the present hot path only indexes
/// the mapped bytes.
pub struct RtShmMapper;

impl ShmMapper for RtShmMapper {
    type Region = RtShmRegion;

    fn map(&mut self, grantor: ProcId, handle: u64, min_len: usize) -> Result<RtShmRegion, Errno> {
        MappedGrant::map(grantor, handle, min_len).map(RtShmRegion)
    }
}

/// Why frames could not be established for a mode.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FramesRefused {
    /// The mode's frames are empty, overflow the address width, or are not
    /// a ring this client can present.
    Geometry,
    /// The kernel refused the shared region.
    Region,
    /// The kernel refused the region's grant to the display service.
    Grant,
    /// The display service refused the frames: [`Errno::Stale`] when the
    /// output no longer runs the mode.
    Configure(Errno),
}

impl FramesRefused {
    /// The refusal as a line of a diagnosis.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Geometry => "the display mode describes no frames this client can present",
            Self::Region => "the shared frame region was refused",
            Self::Grant => "the frame region's grant was refused",
            Self::Configure(Errno::Stale) => {
                "the display output moved before the frames reached it"
            }
            Self::Configure(_) => "the display service refused the frames",
        }
    }
}

/// Establish `frame_count` frames shaped as `mode` for the seat `client`
/// acts for: create the shared region, grant it to the display service,
/// configure the service over it, and answer the ring presenting through it,
/// which owns the region from then on.
///
/// A refusal after the region exists drops it, so a failed attempt leaves
/// nothing mapped.
///
/// # Errors
///
/// The [`FramesRefused`] naming the step that refused.
pub fn establish_frames<T: DisplayTransport>(
    client: DisplayClient<T>,
    mode: &DisplayMode,
    frame_count: u32,
) -> Result<RemoteDisplay<SharedRegion, T>, FramesRefused> {
    let total = u64::from(mode.stride_bytes)
        .checked_mul(u64::from(mode.height_px))
        .and_then(|frame| frame.checked_mul(u64::from(frame_count)))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .filter(|bytes| *bytes != 0)
        .ok_or(FramesRefused::Geometry)?;
    let region = SharedRegion::create(total).ok_or(FramesRefused::Region)?;
    let grant = u64::try_from(tairix_rt::shm_grant(region.id(), DISPLAY_ENDPOINT))
        .ok()
        .filter(|handle| *handle != 0)
        .ok_or(FramesRefused::Grant)?;
    let mut ring = RemoteDisplay::new(client, *mode, region, frame_count)
        .map_err(|_| FramesRefused::Geometry)?;
    ring.configure(grant).map_err(FramesRefused::Configure)?;
    Ok(ring)
}
