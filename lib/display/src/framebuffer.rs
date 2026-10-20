//! The generic linear-framebuffer surface engine.
//!
//! Implements [`Display`] over a single linear pixel surface the hosting
//! process discovered and mapped: the `drivers/display/framebuffer` `Run`
//! binary from its kernel-issued grant (`sole_framebuffer`), the VESA driver
//! from the VBE mode block, the QEMU verticals from the board they synthesise.
//! A presenter fills a buffer the engine allocated in shared memory, and a
//! present copies its damaged rows into the surface, so the buffer is free
//! again when the present returns.
//!
//! # Capabilities
//!
//! Mapping the surface requires
//! [`CapabilityId::MMIO_MAP`](tairix_abi::CapabilityId::MMIO_MAP): the
//! framebuffer is device-visible memory reached only through the
//! capability-gated [`MmioMapper`], never through a pointer the engine
//! synthesises itself.

use tairix_abi::display_ipc::DISPLAY_MAX_BUFFERS;
use tairix_abi::driver::display::{
    AllocatedBuffer, BufferId, DamageRect, Display, DisplayFormat, DisplayMode, Presented, SeatGate,
};
use tairix_abi::driver::mmio::{MmioMapError, WindowError};
use tairix_abi::{CapabilityId, DriverError, DriverHost, MmioMapper, RegisterWindow};
use tairix_inline::ArrayVec;

use crate::memory::{frames_budget, BufferIds, BufferMemory, SharedBuffer};

/// Discovered description of a linear framebuffer.
///
/// The hosting process fills this in from the platform's framebuffer
/// hand-off and passes it to [`Framebuffer::open`]. The engine never invents
/// these values; it only validates them and maps the region they describe.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FramebufferConfig {
    /// Device-visible physical base address of scanline 0, pixel 0.
    pub phys_base: u64,
    /// Surface width in pixels.
    pub width_px: u32,
    /// Surface height in pixels.
    pub height_px: u32,
    /// Distance in bytes between the start of consecutive scanlines; may
    /// exceed `width_px * format.bytes_per_pixel()` where the firmware pads.
    pub stride_bytes: u32,
    /// Pixel encoding the firmware programmed the surface for.
    pub format: DisplayFormat,
}

impl FramebufferConfig {
    /// Validate the geometry and return the surface length in bytes:
    /// refused for a zero-sized surface or a stride that cannot hold one
    /// scanline, so every later access stays inside the mapped window.
    fn validate(&self) -> Result<usize, DriverError> {
        let min_stride = self
            .width_px
            .checked_mul(self.format.bytes_per_pixel())
            .ok_or(DriverError::LengthOutOfRange)?;
        if self.width_px == 0 || self.height_px == 0 || self.stride_bytes < min_stride {
            return Err(DriverError::LengthOutOfRange);
        }
        bytes(self.stride_bytes, self.height_px)
    }
}

/// `stride` × `rows` bytes, checked against the host's address width.
fn bytes(stride: u32, rows: u32) -> Result<usize, DriverError> {
    usize::try_from(u64::from(stride) * u64::from(rows)).map_err(|_| DriverError::LengthOutOfRange)
}

/// One buffer the engine allocated.
struct Buffer<R> {
    id: BufferId,
    region: R,
    width_px: u32,
    height_px: u32,
    stride_bytes: u32,
}

/// A linear framebuffer surface mapped through the host's [`MmioMapper`],
/// and the buffers its presenter fills.
///
/// `'h` borrows the opening [`DriverHost`]: the engine captures the host's
/// [`SeatGate`], so every present re-checks the presenting client's live seat
/// lease before touching the surface. A host with no gate wired (a service
/// that gates presents through the display-protocol lease check) presents
/// ungated here.
pub struct Framebuffer<'h, M: BufferMemory> {
    window: RegisterWindow,
    mode: DisplayMode,
    seat: Option<&'h dyn SeatGate>,
    memory: M,
    buffers: ArrayVec<Buffer<M::Region>, DISPLAY_MAX_BUFFERS>,
    /// Bytes the buffers hold together, bounded by a few surfaces' worth.
    held_bytes: usize,
    ids: BufferIds,
}

impl<'h, M: BufferMemory> Framebuffer<'h, M> {
    /// Map the framebuffer `config` describes and allocate its presenter's
    /// buffers in `memory`.
    ///
    /// # Errors
    ///
    /// * [`DriverError::LengthOutOfRange`] if `config` describes a zero-sized
    ///   surface or a stride too small for one scanline.
    /// * [`DriverError::PermissionDenied`] if the host did not grant
    ///   [`CapabilityId::MMIO_MAP`].
    /// * [`DriverError::Unsupported`] if the host exposes no [`MmioMapper`],
    ///   or the platform cannot map the region.
    pub fn open(
        host: &'h dyn DriverHost,
        config: FramebufferConfig,
        memory: M,
    ) -> Result<Self, DriverError> {
        let surface_len = config.validate()?;
        if !host.has_capability(CapabilityId::MMIO_MAP) {
            return Err(DriverError::PermissionDenied);
        }
        let mapper: &dyn MmioMapper = host.mmio_mapper().ok_or(DriverError::Unsupported)?;
        let window = mapper
            .map_window(config.phys_base, surface_len)
            .map_err(MmioMapError::as_driver_error)?;
        Ok(Self {
            window,
            mode: DisplayMode {
                width_px: config.width_px,
                height_px: config.height_px,
                stride_bytes: config.stride_bytes,
                format: config.format,
            },
            seat: host.seat_gate(),
            memory,
            buffers: ArrayVec::new(),
            held_bytes: 0,
            ids: BufferIds::new(),
        })
    }

    /// Copy `len` bytes of `source` from `from` into the window at `to`.
    fn blit(&self, source: &[u8], from: usize, to: usize, len: usize) -> Result<(), DriverError> {
        let end = from.checked_add(len).ok_or(DriverError::BufferTooSmall)?;
        let src = source.get(from..end).ok_or(DriverError::BufferTooSmall)?;
        self.window
            .write_bytes(to, src)
            .map_err(WindowError::as_driver_error)
    }
}

impl<M: BufferMemory> Display for Framebuffer<'_, M> {
    fn output(&self) -> Option<DisplayMode> {
        Some(self.mode)
    }

    fn frame_count(&self) -> u32 {
        1
    }

    fn alloc_buffer(
        &mut self,
        width_px: u32,
        height_px: u32,
    ) -> Result<AllocatedBuffer, DriverError> {
        if width_px == 0
            || height_px == 0
            || width_px > self.mode.width_px
            || height_px > self.mode.height_px
        {
            return Err(DriverError::LengthOutOfRange);
        }
        let stride_bytes = width_px
            .checked_mul(self.mode.format.bytes_per_pixel())
            .ok_or(DriverError::LengthOutOfRange)?;
        let len = bytes(stride_bytes, height_px)?;
        let held = self.held_bytes.saturating_add(len);
        if self.buffers.is_full() || held > frames_budget(&self.mode).unwrap_or(0) {
            return Err(DriverError::NoSpace);
        }
        let taken = &self.buffers;
        let id = self
            .ids
            .mint(|id| taken.iter().any(|buffer| buffer.id == id))
            .ok_or(DriverError::NoSpace)?;
        let region = self.memory.create(len)?;
        let allocated = AllocatedBuffer {
            id,
            region: region.id(),
            stride_bytes,
        };
        self.buffers
            .try_push(Buffer {
                id,
                region,
                width_px,
                height_px,
                stride_bytes,
            })
            .map_err(|_| DriverError::NoSpace)?;
        self.held_bytes = held;
        Ok(allocated)
    }

    fn free_buffer(&mut self, id: BufferId) -> Result<(), DriverError> {
        // A present has finished with its buffer by the time it returns, so
        // nothing the engine holds is still being read.
        let at = self
            .buffers
            .iter()
            .position(|buffer| buffer.id == id)
            .ok_or(DriverError::NotFound)?;
        if let Some(buffer) = self.buffers.swap_remove(at) {
            let len = bytes(buffer.stride_bytes, buffer.height_px)?;
            self.held_bytes = self.held_bytes.saturating_sub(len);
        }
        Ok(())
    }

    fn present(&mut self, id: BufferId, damage: &[DamageRect]) -> Result<Presented, DriverError> {
        // The present right is derived from the live seat lease, before any
        // other validation or surface access.
        if let Some(gate) = self.seat {
            gate.check_present()?;
        }
        DamageRect::validate_list(damage, &self.mode)?;
        let buffer = self
            .buffers
            .iter()
            .find(|buffer| buffer.id == id)
            .ok_or(DriverError::NotFound)?;
        if (buffer.width_px, buffer.height_px) != (self.mode.width_px, self.mode.height_px) {
            return Err(DriverError::Stale);
        }
        let (from_stride, to_stride) = (
            buffer.stride_bytes as usize,
            self.mode.stride_bytes as usize,
        );
        let bpp = self.mode.format.bytes_per_pixel() as usize;
        buffer.region.with_bytes(|source| {
            for rect in damage {
                let (x, y, rows) = (rect.x as usize, rect.y as usize, rect.height_px as usize);
                let span = rect.width_px as usize * bpp;
                if from_stride == to_stride && rect.width_px == self.mode.width_px {
                    // Whole rows with matching strides are one span.
                    let at = y * from_stride;
                    self.blit(source, at, at, from_stride * (rows - 1) + span)?;
                    continue;
                }
                for row in y..y + rows {
                    self.blit(
                        source,
                        row * from_stride + x * bpp,
                        row * to_stride + x * bpp,
                        span,
                    )?;
                }
            }
            Ok(Presented::Shown)
        })
    }
}
