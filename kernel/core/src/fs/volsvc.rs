//! The runtime volume attach/detach service seam (`plans/DEVICES.md` D3b).
//!
//! The `volume_attach` / `volume_detach` syscalls validate the caller's
//! authority and the request frame, then delegate the actual work —
//! connecting the kernel blkio client, opening the filesystem, mounting,
//! and publishing/unpublishing the volume's root — to the installed
//! [`VolumeService`]. The concrete service lives in the kernel binary
//! crate (it names the filesystem driver crates, which `kernel/core`'s
//! layering does not), installed through the boot handover exactly like
//! the [`FilesystemService`](super::FilesystemService); until it is
//! installed every attach/detach fails closed.
//!
//! The same service answers the filesystem driver serving an attached volume
//! from user space (`plans/FSSERVE.md` §4–§5): its block transfers, and its
//! offer to serve the volume or its refusal.

use alloc::sync::Arc;

use tairix_abi::fsserve::ServeOffer;
use tairix_abi::volume::{VolumeAttachRequest, VolumeCtl, VolumeDetachRequest, VolumeInfo};
use tairix_abi::{DriverError, Errno};
use tairix_kernel_ipc::CallEndpoint;

use crate::sharedreg::KernelHold;

/// What a served-volume read hands each chunk to, with the chunk's offset into
/// the transfer.
pub type ChunkSink<'a> = dyn FnMut(usize, &[u8]) -> Result<(), Errno> + 'a;

/// What fills each chunk of a served-volume write, given its offset into the
/// transfer.
pub type ChunkSource<'a> = dyn FnMut(usize, &mut [u8]) -> Result<(), Errno> + 'a;

/// The channel the kernel made for a driver it accepts to serve a volume.
pub struct ServeChannel {
    /// The endpoint the driver receives the kernel's calls on.
    pub endpoint: Arc<CallEndpoint>,
    /// The kernel's hold on the data window it mapped for the driver.
    pub window: KernelHold,
}

/// The runtime volume attach/detach operations the syscall handlers
/// delegate to.
///
/// The caller's `CAP_FS_MOUNT` and its endpoint/window resource grants
/// have already been verified by the dispatcher and the handler; the
/// service re-validates everything against live state (the endpoint, the
/// window, the device geometry, name/identity collisions) and fails
/// closed.
pub trait VolumeService: Sync {
    /// Attach a filesystem to the runtime block source `request` names,
    /// mount it under its catalog view location, and publish its stable
    /// identity.
    ///
    /// # Errors
    ///
    /// A stable [`Errno`] for every refusal: an unreachable endpoint or
    /// window, an unusable geometry, an unmountable volume, a name or
    /// identity collision, or [`Errno::NotImplemented`] before a service
    /// is installed.
    ///
    /// `publisher` is the hardware node the caller was loaded for, which the
    /// volume's own node is published under.
    fn attach(
        &self,
        request: &VolumeAttachRequest<'_>,
        publisher: Option<u32>,
    ) -> Result<(), Errno>;

    /// Flush, unmount, and unpublish the runtime-attached volume
    /// `request` names.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for an identity this service did not attach
    /// (the boot volumes are permanent), a flush failure (the volume
    /// stays attached — data is never silently discarded),
    /// [`Errno::Busy`] for a plain detach while a descriptor holds one of
    /// the volume's nodes or another volume is shown beneath it, the
    /// refusal of a volume beneath that a force detach could not detach
    /// first, or [`Errno::NotImplemented`] before a service is installed.
    fn detach(&self, request: &VolumeDetachRequest) -> Result<(), Errno>;

    /// Read `len` bytes of whole blocks from `lba` of served volume `volume`,
    /// handing `deliver` each chunk with its offset into the transfer.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for no such volume, [`Errno::DeviceOffline`] for
    /// one retired, [`Errno::LengthOutOfRange`] for a transfer that is not
    /// whole blocks or runs past the volume, the device's error, or what
    /// `deliver` refused.
    fn read(
        &self,
        volume: u64,
        lba: u64,
        len: usize,
        deliver: &mut ChunkSink<'_>,
    ) -> Result<(), Errno>;

    /// Write `len` bytes of whole blocks to `lba` of served volume `volume`,
    /// `fill` supplying each chunk at its offset into the transfer.
    ///
    /// # Errors
    ///
    /// As [`Self::read`], and [`Errno::ReadOnly`] for a volume that may not
    /// be written.
    fn write(
        &self,
        volume: u64,
        lba: u64,
        len: usize,
        fill: &mut ChunkSource<'_>,
    ) -> Result<(), Errno>;

    /// Describe, flush or discard served volume `volume`; a description
    /// answers [`Some`].
    ///
    /// # Errors
    ///
    /// As [`Self::read`].
    fn ctl(&self, volume: u64, ctl: &VolumeCtl) -> Result<Option<VolumeInfo>, Errno>;

    /// Whether served volume `volume` awaits a driver to serve it: asked
    /// before the kernel builds a channel for one.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for no such volume; [`Errno::AlreadyExists`] for
    /// one a driver serves already.
    fn awaiting_server(&self, volume: u64) -> Result<(), Errno>;

    /// Serve the filesystem a driver `offer`ed on served volume `volume` over
    /// `channel`: build the kernel's proxy for it and mount it.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for no such volume, [`Errno::AlreadyExists`] for
    /// one already served, and the refusals of mounting it.
    fn serve(&self, volume: u64, offer: &ServeOffer, channel: ServeChannel) -> Result<(), Errno>;

    /// The driver of served volume `volume` could not open it, for `why`:
    /// retract the volume.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for no volume awaiting a driver under that id.
    fn refuse(&self, volume: u64, why: DriverError) -> Result<(), Errno>;
}

/// The fail-closed default: every operation reports
/// [`Errno::NotImplemented`] until a boot path installs a real service.
pub struct NullVolumeService;

impl VolumeService for NullVolumeService {
    fn attach(
        &self,
        _request: &VolumeAttachRequest<'_>,
        _publisher: Option<u32>,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn detach(&self, _request: &VolumeDetachRequest) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn read(
        &self,
        _volume: u64,
        _lba: u64,
        _len: usize,
        _deliver: &mut ChunkSink<'_>,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn write(
        &self,
        _volume: u64,
        _lba: u64,
        _len: usize,
        _fill: &mut ChunkSource<'_>,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn ctl(&self, _volume: u64, _ctl: &VolumeCtl) -> Result<Option<VolumeInfo>, Errno> {
        Err(Errno::NotImplemented)
    }

    fn awaiting_server(&self, _volume: u64) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn serve(
        &self,
        _volume: u64,
        _offer: &ServeOffer,
        _channel: ServeChannel,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn refuse(&self, _volume: u64, _why: DriverError) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }
}

/// The shared [`NullVolumeService`] the syscall handlers default to.
pub static NULL_VOLUME_SERVICE: NullVolumeService = NullVolumeService;

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_abi::volume::VolumeFsType;

    #[test]
    fn the_null_service_fails_closed() {
        let attach = VolumeAttachRequest {
            endpoint: 1,
            window: 1,
            first_lba: 0,
            blocks: 8,
            fstype: VolumeFsType::Fat32,
            name: b"usb1",
        };
        assert_eq!(
            NULL_VOLUME_SERVICE.attach(&attach, Some(3)),
            Err(Errno::NotImplemented)
        );
        assert_eq!(
            NULL_VOLUME_SERVICE.awaiting_server(1),
            Err(Errno::NotImplemented)
        );
        assert_eq!(
            NULL_VOLUME_SERVICE.refuse(1, DriverError::BadMagic),
            Err(Errno::NotImplemented)
        );
        assert_eq!(
            NULL_VOLUME_SERVICE.detach(&VolumeDetachRequest {
                volume_id: [7; 16],
                force: false
            }),
            Err(Errno::NotImplemented)
        );
    }
}
