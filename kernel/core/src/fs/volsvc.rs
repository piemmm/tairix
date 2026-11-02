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
//! offer to serve the volume or its refusal; a network filesystem server's
//! offer of a volume to one principal, that principal's attach of it, and the
//! server's word on it (`plans/FSSERVE.md` §6); and the volume a disk-image
//! file holds, attached for the principal that handed over the file
//! (`plans/DISK-IMAGES.md`).

use alloc::string::String;
use alloc::sync::Arc;

use tairix_abi::fsserve::ServeOffer;
use tairix_abi::imageattach::{ImageAttach, ImageAttachReply};
use tairix_abi::netattach::{AttachReply, FsserveCtl, NetworkAttach, NetworkOffer, OfferId};
use tairix_abi::volume::{VolumeAttachRequest, VolumeCtl, VolumeDetachRequest, VolumeInfo};
use tairix_abi::{AppIdentity, CapabilityId, CapabilityQuery, DriverError, Errno, ProcId};
use tairix_kernel_ipc::CallEndpoint;

use super::NodeHandle;
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

/// The kernel-attested facts a decision about a volume is made against: who
/// asks, and with what authority.
#[derive(Copy, Clone)]
pub struct Principal<'a> {
    /// The caller's uid.
    pub uid: u32,
    /// Its primary group.
    pub gid: u32,
    /// Its effective capabilities.
    pub caps: &'a dyn CapabilityQuery,
}

impl Principal<'_> {
    /// Whether it holds the mount authority.
    #[must_use]
    pub fn may_mount(&self) -> bool {
        self.caps.holds(CapabilityId::FS_MOUNT)
    }
}

/// The server making an offer: its process, which alone speaks for the offer
/// afterwards, and the application identity the kernel attested for it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct OfferingServer<'a> {
    /// The serving process.
    pub process: u64,
    /// Its attested application identity.
    pub app: &'a AppIdentity,
}

/// The disk-image file an `image_attach` caller handed over.
pub struct ImageSource {
    /// The file, acting as the caller's identity frozen at the attach.
    pub file: NodeHandle,
    /// Whether the caller's descriptor may write it.
    pub writable: bool,
    /// The directory the caller named the file in, which a writable archive
    /// is replaced in.
    pub directory: Option<ImageDirectory>,
    /// The caller's login session, whose end detaches the volume.
    pub session: ProcId,
    /// The calling task, which waits for the volume's driver.
    pub task: u64,
}

/// The directory holding an image, as an `image_attach` caller handed it
/// over: what a writable archive's commit replaces the file in.
pub struct ImageDirectory {
    /// The directory, acting as the caller's identity frozen at the attach.
    pub dir: NodeHandle,
    /// The name the caller said the file has in it, which the attach holds
    /// to the file.
    pub name: String,
    /// A key drawn from the kernel's random reserve for this attach alone,
    /// from which the names of the archive's work files are derived, so no
    /// one sharing the directory can claim one first.
    pub name_key: [u8; 32],
}

impl Drop for ImageDirectory {
    fn drop(&mut self) {
        tairix_util::secret::wipe(&mut self.name_key);
    }
}

/// The runtime volume attach/detach operations the syscall handlers
/// delegate to.
///
/// The dispatcher and the handler have verified the syscall's own capability
/// and the caller's endpoint/window grants; the service decides what turns on
/// the volume itself — who may detach it, what presentation a principal may
/// ask for — and re-validates everything against live state (the endpoint,
/// the window, the device geometry, name/identity collisions), failing
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
    /// `request` names, for `caller`: the principal a network or disk-image
    /// volume was attached for, or any caller holding the mount authority.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for an identity this service did not attach
    /// (the boot volumes are permanent), [`Errno::PermissionDenied`] for a
    /// caller with no authority over the volume, a flush failure (the
    /// volume stays attached — data is never silently discarded),
    /// [`Errno::Busy`] for a plain detach while a descriptor holds one of
    /// the volume's nodes or another volume is shown beneath it, the
    /// refusal of a volume beneath that a force detach could not detach
    /// first, or [`Errno::NotImplemented`] before a service is installed. A
    /// [`DetachMode::Lost`](tairix_abi::volume::DetachMode::Lost) detach
    /// forces only a volume recorded lost, decided under the volume's claim,
    /// and is refused as a plain detach of any other.
    fn detach(&self, request: &VolumeDetachRequest, caller: &Principal<'_>) -> Result<(), Errno>;

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

    /// Describe, flush, discard, copy within, truncate or commit served
    /// volume `volume`; a description answers [`Some`].
    ///
    /// # Errors
    ///
    /// As [`Self::read`], and for an archive's operations as
    /// [`Self::archive_read`] and the refusals of the commit.
    fn ctl(&self, volume: u64, ctl: &VolumeCtl) -> Result<Option<VolumeInfo>, Errno>;

    /// Read up to `len` bytes at `offset` of region `region` of served archive
    /// attachment `volume`, handing `deliver` each chunk at its offset into
    /// the transfer. Answers how many it read: fewer only where the region
    /// ends.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for no such volume, [`Errno::NotSupported`] for
    /// one that is no archive, [`Errno::OutOfRange`] for a region it does not
    /// have, [`Errno::DeviceOffline`] for one retired, the file's own error,
    /// or what `deliver` refused.
    fn archive_read(
        &self,
        volume: u64,
        region: u8,
        offset: u64,
        len: usize,
        deliver: &mut ChunkSink<'_>,
    ) -> Result<usize, Errno>;

    /// Write `len` bytes at `offset` of work file `region` of served archive
    /// attachment `volume`, `fill` supplying each chunk at its offset into the
    /// transfer.
    ///
    /// # Errors
    ///
    /// As [`Self::archive_read`], and [`Errno::ReadOnly`] for a volume that
    /// takes no writes.
    fn archive_write(
        &self,
        volume: u64,
        region: u8,
        offset: u64,
        len: usize,
        fill: &mut ChunkSource<'_>,
    ) -> Result<(), Errno>;

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

    /// Hold the network volume `server` offers, named `id`, served over
    /// `channel`, for the one principal `offer` names.
    ///
    /// # Errors
    ///
    /// [`Errno::AlreadyExists`] for a volume of that identity offered or
    /// attached already, [`Errno::OutOfMemory`], or
    /// [`Errno::NotImplemented`] before a service is installed.
    fn offer(
        &self,
        server: &OfferingServer<'_>,
        offer: &NetworkOffer<'_>,
        id: OfferId,
        channel: ServeChannel,
    ) -> Result<(), Errno>;

    /// Attach the network volume `offer` names for `caller`, as `attach`
    /// asks.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for no offer made for `caller`;
    /// [`Errno::AlreadyExists`] for one attached already, or every name it
    /// could take in use; [`Errno::PermissionDenied`] for a presentation or
    /// target `caller` may not ask for; the refusals of mounting it.
    fn attach_offered(
        &self,
        caller: &Principal<'_>,
        offer: OfferId,
        attach: &NetworkAttach<'_>,
    ) -> Result<AttachReply, Errno>;

    /// `op` on the network volume `offer` names, from the server process
    /// `server`.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for no such offer made by `server`.
    fn offer_ctl(&self, server: u64, offer: OfferId, op: FsserveCtl) -> Result<(), Errno>;

    /// Attach the volume the image file `source` holds for `caller`, as
    /// `attach` asks, waiting until its driver serves or refuses it, and hand
    /// the answer to `answer`. What an answer `answer` refuses would have
    /// left is undone, so a caller never told of a volume is not given one. A
    /// file the kernel will not attach, and a refused target or asked name,
    /// are answers, not errors.
    ///
    /// # Errors
    ///
    /// [`Errno::PermissionDenied`] for a presentation `caller` may not ask
    /// for; [`Errno::AlreadyExists`] when no name it could take is free;
    /// [`Errno::ReadOnly`] for an attach asking `Writable` of a descriptor or
    /// format that will not write; [`Errno::Busy`] for a file locked against
    /// it — any lock refuses a writable attach, a writer's any attach — or a
    /// volume of it being attached or detached; [`Errno::LimitExceeded`] past the
    /// principal's image bound or the nesting bound; [`Errno::TimedOut`] for a
    /// driver that never answered; [`Errno::Interrupted`] for a caller killed
    /// while it waited; [`Errno::NotAttached`] for a volume detached before it
    /// could be answered; the refusals of mounting it and of `answer`; or
    /// [`Errno::NotImplemented`] before a service is installed.
    fn attach_image(
        &self,
        caller: &Principal<'_>,
        source: ImageSource,
        attach: &ImageAttach<'_>,
        answer: AnswerSink<'_>,
    ) -> Result<(), Errno>;
}

/// Where an image attach's answer is delivered to its caller.
pub type AnswerSink<'a> = &'a mut dyn FnMut(&ImageAttachReply) -> Result<(), Errno>;

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

    fn detach(&self, _request: &VolumeDetachRequest, _caller: &Principal<'_>) -> Result<(), Errno> {
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

    fn archive_read(
        &self,
        _volume: u64,
        _region: u8,
        _offset: u64,
        _len: usize,
        _deliver: &mut ChunkSink<'_>,
    ) -> Result<usize, Errno> {
        Err(Errno::NotImplemented)
    }

    fn archive_write(
        &self,
        _volume: u64,
        _region: u8,
        _offset: u64,
        _len: usize,
        _fill: &mut ChunkSource<'_>,
    ) -> Result<(), Errno> {
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

    fn offer(
        &self,
        _server: &OfferingServer<'_>,
        _offer: &NetworkOffer<'_>,
        _id: OfferId,
        _channel: ServeChannel,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn attach_offered(
        &self,
        _caller: &Principal<'_>,
        _offer: OfferId,
        _attach: &NetworkAttach<'_>,
    ) -> Result<AttachReply, Errno> {
        Err(Errno::NotImplemented)
    }

    fn offer_ctl(&self, _server: u64, _offer: OfferId, _op: FsserveCtl) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn attach_image(
        &self,
        _caller: &Principal<'_>,
        _source: ImageSource,
        _attach: &ImageAttach<'_>,
        _answer: AnswerSink<'_>,
    ) -> Result<(), Errno> {
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
        let caller = Principal {
            uid: 1000,
            gid: 1000,
            caps: &tairix_caps::CapabilitySet::empty(),
        };
        assert_eq!(
            NULL_VOLUME_SERVICE.detach(
                &VolumeDetachRequest {
                    volume_id: [7; 16],
                    mode: tairix_abi::volume::DetachMode::Plain,
                },
                &caller
            ),
            Err(Errno::NotImplemented)
        );
        let offer = OfferId::new(1).unwrap();
        assert_eq!(
            NULL_VOLUME_SERVICE.offer_ctl(9, offer, FsserveCtl::Alive),
            Err(Errno::NotImplemented)
        );
        let attach = NetworkAttach {
            name: "share",
            presentation: tairix_abi::netattach::Presentation::Private,
            read_only: false,
            target: None,
        };
        assert_eq!(
            NULL_VOLUME_SERVICE.attach_offered(&caller, offer, &attach),
            Err(Errno::NotImplemented)
        );
        let volume = Arc::new(super::super::VolumeNodes::new(1));
        let file = crate::test_bundle::node_handle(crate::test_bundle::opened_node(
            &volume,
            9,
            tairix_abi::driver::filesystem::NodeKind::RegularFile,
            "/Users/a/disk.adf",
        ));
        let source = ImageSource {
            file,
            writable: true,
            directory: None,
            session: tairix_kernel_sec::session::ROOT_SESSION,
            task: 1,
        };
        let image = ImageAttach {
            name: None,
            presentation: tairix_abi::netattach::Presentation::Private,
            access: tairix_abi::imageattach::ImageAccess::WritableWherePossible,
            target: None,
            expects: None,
            adopt: false,
            file_name: None,
        };
        let mut answered = false;
        assert_eq!(
            NULL_VOLUME_SERVICE.attach_image(&caller, source, &image, &mut |_| {
                answered = true;
                Ok(())
            }),
            Err(Errno::NotImplemented)
        );
        assert!(!answered, "nothing is answered");
    }
}
