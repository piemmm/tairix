//! The production, `tairix-rt`-backed halves of the protocol (feature `rt`).
//!
//! [`RtShmMapper`] is the one definition of "map a client's granted
//! shared-memory region through the kernel's `shm_map`, sized from the
//! kernel's own record of the region length — never the granting client's
//! claim" — what the desktop session's window server reads an application's
//! frames through (`plans/APPWIN.md` AW3). It narrows the runtime's
//! [`MappedGrant`] to the read-only view a frame region is.
//!
//! [`establish_display`] is a presenter's: the one way the desktop session
//! and the login screen take the frames a display driver allocated for the
//! mode its output runs, with the port the driver posts its latches to.

use tairix_abi::display_ipc::{DisplayEvent, DISPLAY_ENDPOINT};
use tairix_abi::driver::display::DisplayMode;
use tairix_abi::driver::DmaReach;
use tairix_abi::{DriverError, Errno, ProcId, ORIGIN_WIRE_LEN};
use tairix_rt::shm::{MappedGrant, SharedRegion};
use tairix_rt::PrivatePort;

use tairix_abi::driver::display::{DamageRect, DisplayPower};
use tairix_geometry::Region;

use crate::client::{BufferMapper, DisplayClient, DisplayTransport, RemoteDisplay};
use crate::memory::{BufferMemory, DeviceBuffer, SharedBuffer};
use crate::presenter::{CursorPlane, DrawInPlace, LayerPresenter, Presenter};
use crate::region::{FrameRegion, ShmMapper};

/// A client region mapped through `shm_map`, unmapped on drop.
pub struct RtShmRegion(MappedGrant);

impl FrameRegion for RtShmRegion {
    fn bytes(&self) -> &[u8] {
        self.0.bytes()
    }
}

/// The `shm_map`-backed [`ShmMapper`].
pub struct RtShmMapper;

impl ShmMapper for RtShmMapper {
    type Region = RtShmRegion;

    fn map(&mut self, grantor: ProcId, handle: u64, min_len: usize) -> Result<RtShmRegion, Errno> {
        MappedGrant::map(grantor, handle, min_len).map(RtShmRegion)
    }
}

/// A buffer a display driver granted, mapped read/write.
pub struct RtMapping(MappedGrant);

impl AsMut<[u8]> for RtMapping {
    fn as_mut(&mut self) -> &mut [u8] {
        self.0.bytes_mut()
    }
}

/// The `shm_map`-backed [`BufferMapper`].
pub struct RtBufferMapper;

impl BufferMapper for RtBufferMapper {
    type Mapping = RtMapping;

    fn map(&mut self, grantor: ProcId, grant: u64, least: usize) -> Result<RtMapping, Errno> {
        MappedGrant::map(grantor, grant, least).map(RtMapping)
    }
}

/// A region this process created through `shm_create`, for a display
/// engine whose device reaches ordinary memory.
pub struct RtSharedBuffer(SharedRegion);

impl SharedBuffer for RtSharedBuffer {
    fn id(&self) -> u64 {
        self.0.id()
    }

    fn with_bytes<R>(&self, read: impl FnOnce(&[u8]) -> R) -> R {
        read(self.0.bytes())
    }
}

/// The `shm_create`-backed [`BufferMemory`].
pub struct RtBufferMemory;

impl BufferMemory for RtBufferMemory {
    type Region = RtSharedBuffer;

    fn create(&mut self, len: usize) -> Result<RtSharedBuffer, DriverError> {
        SharedRegion::create(len)
            .map(RtSharedBuffer)
            .ok_or(DriverError::OutOfMemory)
    }
}

/// Shared memory a display device reads where it lies, carved under its
/// node's `Dma` grant for a device driving `reach` address bits: the
/// `shm_create_dma`-backed [`BufferMemory`].
pub struct RtDeviceMemory {
    grant: u64,
    reach: DmaReach,
}

impl RtDeviceMemory {
    /// Carve under the `Dma` grant `grant`, below `reach`.
    #[must_use]
    pub const fn new(grant: u64, reach: DmaReach) -> Self {
        Self { grant, reach }
    }
}

impl BufferMemory for RtDeviceMemory {
    type Region = RtDeviceBuffer;

    fn create(&mut self, len: usize) -> Result<RtDeviceBuffer, DriverError> {
        let (region, device) = SharedRegion::create_dma(self.grant, len, self.reach)
            .map_err(DriverError::from_errno)?;
        Ok(RtDeviceBuffer { region, device })
    }
}

/// A region carved for a display device, and where the device reads it.
pub struct RtDeviceBuffer {
    region: SharedRegion,
    device: u64,
}

impl SharedBuffer for RtDeviceBuffer {
    fn id(&self) -> u64 {
        self.region.id()
    }

    fn with_bytes<R>(&self, read: impl FnOnce(&[u8]) -> R) -> R {
        read(self.region.bytes())
    }
}

impl DeviceBuffer for RtDeviceBuffer {
    fn device_addr(&self) -> u64 {
        self.device
    }

    fn with_bytes_mut<R>(&mut self, write: impl FnOnce(&mut [u8]) -> R) -> R {
        write(self.region.bytes_mut())
    }
}

/// Why a presenter could not take a display's frames.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FramesRefused {
    /// The port the driver posts its latches to could not be made.
    Port(Errno),
    /// The service refused the frames, or they could not be mapped.
    Configure(Errno),
}

impl FramesRefused {
    /// A human-readable reason, for the presenter's own report.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Port(_) => "the display's event port could not be bound",
            Self::Configure(Errno::Stale) => {
                "the display output moved before the frames reached it"
            }
            Self::Configure(_) => "the display service refused the frames",
        }
    }
}

/// How many latches the port holds: one is ever outstanding, and the rest is
/// slack for a driver that restarts with one posted.
const EVENT_CAPACITY: usize = 4;

/// A [`RemoteDisplay`] over the runtime, and the port its driver posts to.
pub struct RtDisplay<T: DisplayTransport> {
    display: RemoteDisplay<RtBufferMapper, T>,
    port: PrivatePort,
}

impl<T: DisplayTransport> RtDisplay<T> {
    /// The port to wait on beside the presenter's other sources: it fires
    /// when the driver posts a latch.
    #[must_use]
    pub const fn events_port(&self) -> u64 {
        self.port.id()
    }

    /// Take every event the driver posted, answering whether a present that
    /// waited on one may now go.
    pub fn take_events(&mut self) -> bool {
        let mut released = false;
        let mut event = [0u8; DisplayEvent::WIRE_LEN];
        let mut sender = [0u8; ORIGIN_WIRE_LEN];
        while let Ok(len) = tairix_rt::ipc_recv(self.port.id(), &mut event, &mut sender) {
            // The port admits only the display's serving instance, so a
            // malformed event is that driver's fault and is dropped.
            if let Ok(event) = DisplayEvent::from_bytes(&event[..len]) {
                released |= self.display.latched(event);
            }
        }
        released
    }
}

impl<T: DisplayTransport> Presenter for RtDisplay<T> {
    fn mode(&self) -> DisplayMode {
        self.display.mode()
    }

    fn ready(&self) -> bool {
        self.display.ready()
    }

    fn present(&mut self, frame: &[u8], damage: &[DamageRect]) -> Result<(), DriverError> {
        self.display.present(frame, damage)
    }

    fn present_drawn(
        &mut self,
        damage: &Region,
        draw: &mut DrawInPlace<'_>,
    ) -> Result<(), DriverError> {
        self.display.present_drawn(damage, draw)
    }

    fn set_power(&mut self, power: DisplayPower) -> Result<(), DriverError> {
        self.display.set_power(power)
    }

    fn layers(&mut self) -> Option<&mut dyn LayerPresenter> {
        self.display.layers()
    }

    fn cursor(&mut self) -> Option<&mut dyn CursorPlane> {
        self.display.cursor()
    }
}

/// Take the frames `client`'s display allocated for `mode`.
///
/// # Errors
///
/// The [`FramesRefused`] step that failed.
pub fn establish_display<T: DisplayTransport>(
    client: DisplayClient<T>,
    mode: DisplayMode,
) -> Result<RtDisplay<T>, FramesRefused> {
    let port =
        PrivatePort::bind(DisplayEvent::WIRE_LEN, EVENT_CAPACITY).map_err(FramesRefused::Port)?;
    tairix_rt::port_admit(port.id(), DISPLAY_ENDPOINT).map_err(FramesRefused::Port)?;
    let display = RemoteDisplay::configure(client, RtBufferMapper, mode, port.id())
        .map_err(FramesRefused::Configure)?;
    Ok(RtDisplay { display, port })
}
