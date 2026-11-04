//! Network volumes (`plans/FSSERVE.md` §6): what a network filesystem server
//! offers the kernel for one principal, that principal's attach of it, and its
//! retirement by its owner, by its server, or with its server.
//!
//! An offer holds the server's channel and mounts nothing. Its attach mounts
//! the server's lanes, uncached, at `/Storage/<name>` under the presentation
//! the principal may ask for, and binds it at an explicit target in the same
//! call. The volume's identity is derived here from the server's attested
//! application, the principal and the offer's key, so a server never names
//! another's volume, or a local one.
//!
//! A volume its server retires — withdrawn or lost — leaves the namespace at
//! once, unless the server last said it held writes it had not published:
//! that one stays, its stand-in answering a device fault and the mount
//! reporting the loss, until its owner detaches it. However a volume leaves,
//! what lies on it leaves with it ([`Departed`]). The registry is a spin
//! lock, and a retirement never waits on the server's own lanes, so a server
//! that cannot answer cannot hold up its own retirement.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use tairix_abi::driver::filesystem::MountFlags;
use tairix_abi::fsserve::{Lanes, ServeOffer};
use tairix_abi::netattach::{
    AttachReply, FsserveCtl, NetworkAttach, NetworkOffer, OfferId, OFFER_KEY_LEN,
};
use tairix_abi::sysinfo::{MountAvailability, MOUNT_SOURCE_MAX};
use tairix_abi::volume::{FormatName, ViewBindRequest, VolumeDetachRequest, VOLUME_NAME_MAX};
use tairix_abi::{AppIdentity, DriverHandle, Errno};
use tairix_crypto::Sha256Stream;
use tairix_fsprobe::name::{candidates, NameBase};
use tairix_inline::ArrayString;
use tairix_kernel_core::fs::served::{ServedFs, ServedState, ServerActivity};
use tairix_kernel_core::fs::{OfferingServer, Principal, ServeChannel};
use tairix_kernel_core::sharedreg::KernelHold;
use tairix_kernel_core::{
    Attachment, BindsWithin, FilesystemService, VfsError, VolumePublishError,
};
use tairix_kernel_ipc::{CallEndpoint, EndpointId};
use tairix_log::{log, Event, EventId, Field, FieldValue, Level, Sink};
use tairix_sync::SpinLock;

use super::{
    mount_flags, record, record_retired, refusal_level, storage_path, take_down, Wiring, Writes,
};
use crate::kernel_fs::KernelFs;
use crate::system_mount::{publish_volume_identity, FS_SERVICE, LATE_FILESYSTEM, VOLUME_FOREST};
use crate::volume_policy::{presentation_word, Presented, PresentedFs};

/// Audit event: a server offered a network volume to a principal.
const NETWORK_VOLUME_OFFERED: EventId = EventId(4300);
/// Audit event: a principal attached a network volume offered to it.
const NETWORK_VOLUME_ATTACHED: EventId = EventId(4301);
/// Audit event: a network volume's attach was refused.
const NETWORK_ATTACH_REFUSED: EventId = EventId(4302);
/// Audit event: a network volume was detached by its owner or the mount
/// authority.
const NETWORK_VOLUME_DETACHED: EventId = EventId(4303);
/// Audit event: a network volume's detach was refused.
const NETWORK_DETACH_REFUSED: EventId = EventId(4304);
/// Audit event: a server withdrew its offer or its volume, or was lost.
const NETWORK_VOLUME_RETIRED: EventId = EventId(4305);

/// Where network volumes' mount handles are numbered from: disjoint from the
/// runtime and the boot volumes' handles.
const NETWORK_HANDLE_BASE: u64 = 0x4E45_5400_0000_0000;

/// The domain-separation label of a network volume's identity.
const IDENTITY_CONTEXT: &[u8] = b"tairix-network-volume/v1";

/// The network volumes offered and attached.
pub(super) struct NetworkVolumes {
    /// In ascending offer order.
    volumes: SpinLock<Vec<NetworkVolume>>,
    next_handle: AtomicU64,
}

/// One network volume.
struct NetworkVolume {
    offer: OfferId,
    /// The process that offered it, which alone speaks for it.
    server: u64,
    /// The endpoint it is served over.
    endpoint: u64,
    /// The principal it was offered to.
    for_uid: u32,
    volume_id: [u8; 16],
    source: ArrayString<MOUNT_SOURCE_MAX>,
    serve: ServeOffer,
    lanes: Lanes,
    activity: Arc<ServerActivity>,
    stage: Stage,
    /// An attach or detach holds it: nothing else acts on it until that ends.
    claimed: bool,
    /// Its server withdrew it or was lost while it was claimed; the claim's
    /// holder retires it as it lets go, still holding it.
    retiring: bool,
}

/// Where a network volume stands.
enum Stage {
    /// Offered, its channel held for the attach.
    Offered {
        endpoint: Arc<CallEndpoint>,
        window: Arc<KernelHold>,
    },
    /// Attached.
    Attached(Attached),
    /// Its server left with writes unpublished; its stand-in answers until its
    /// owner detaches it.
    Lost(Attached),
}

/// What an attached network volume is in the namespace.
#[derive(Clone, Copy)]
struct Attached {
    handle: DriverHandle,
    name: ArrayString<VOLUME_NAME_MAX>,
    owner: u32,
}

/// The facts of an offer an attach builds on, read out of the registry.
struct Claimed {
    endpoint: Arc<CallEndpoint>,
    window: Arc<KernelHold>,
    volume_id: [u8; 16],
    source: ArrayString<MOUNT_SOURCE_MAX>,
    serve: ServeOffer,
    lanes: Lanes,
    activity: Arc<ServerActivity>,
}

/// An operation's hold on one network volume, let go on drop: one its server
/// retired while it was held is retired then, and `departed` told.
struct Claim<'a> {
    volumes: &'a NetworkVolumes,
    offer: OfferId,
    audit: &'static (dyn Sink + Sync),
    departed: Departed<'a>,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        {
            let mut volumes = self.volumes.volumes.lock();
            let Some(at) = position(&volumes, self.offer) else {
                return;
            };
            if !core::mem::take(&mut volumes[at].retiring) {
                volumes[at].claimed = false;
                return;
            }
        }
        self.volumes
            .retire_claimed(self.offer, self.audit, "retired_while_held", self.departed);
    }
}

impl NetworkVolumes {
    /// An empty registry.
    pub(super) const fn new() -> Self {
        Self {
            volumes: SpinLock::new(Vec::new()),
            next_handle: AtomicU64::new(NETWORK_HANDLE_BASE + 1),
        }
    }

    /// Hold the volume `server` offers as `id` over `channel`.
    ///
    /// # Errors
    ///
    /// [`Errno::AlreadyExists`] for a volume of the same identity offered,
    /// attached or published already; [`Errno::OutOfMemory`].
    pub(super) fn offer(
        &self,
        wiring: &Wiring,
        server: &OfferingServer<'_>,
        offer: &NetworkOffer<'_>,
        id: OfferId,
        channel: ServeChannel,
    ) -> Result<(), Errno> {
        let volume_id = identity(server.app, offer.for_uid, &offer.key);
        let window = Arc::try_new(channel.window).map_err(|_| Errno::OutOfMemory)?;
        let activity = Arc::try_new(ServerActivity::default()).map_err(|_| Errno::OutOfMemory)?;
        let volume = NetworkVolume {
            offer: id,
            server: server.process,
            endpoint: channel.endpoint.id().0,
            for_uid: offer.for_uid,
            volume_id,
            source: ArrayString::try_from(offer.source).map_err(|_| Errno::OutOfRange)?,
            serve: offer.serve,
            lanes: offer.lanes,
            activity,
            stage: Stage::Offered {
                endpoint: channel.endpoint,
                window,
            },
            claimed: false,
            retiring: false,
        };
        {
            let mut volumes = self.volumes.lock();
            if volumes.iter().any(|v| v.volume_id == volume_id) {
                return Err(Errno::AlreadyExists);
            }
            volumes.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
            let at = volumes.partition_point(|v| v.offer < id);
            volumes.insert(at, volume);
        }
        log(
            wiring.audit,
            &Event {
                level: Level::Info,
                id: NETWORK_VOLUME_OFFERED,
                message: "network-volume: a server offered a volume",
                fields: &[
                    Field {
                        key: "source",
                        value: FieldValue::Str(offer.source),
                    },
                    Field {
                        key: "uid",
                        value: FieldValue::UnsignedInt(u64::from(offer.for_uid)),
                    },
                ],
            },
        );
        Ok(())
    }

    /// `op` on the volume `server` offered as `offer`, telling `departed` of
    /// an attached volume a withdrawal retired.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for no such offer of `server`'s.
    pub(super) fn ctl(
        &self,
        wiring: &Wiring,
        server: u64,
        offer: OfferId,
        op: FsserveCtl,
        departed: Departed<'_>,
    ) -> Result<(), Errno> {
        {
            let mut volumes = self.volumes.lock();
            let at = position(&volumes, offer)
                .filter(|&at| volumes[at].server == server)
                .ok_or(Errno::NotFound)?;
            let volume = &mut volumes[at];
            match op {
                FsserveCtl::Alive => {
                    volume.activity.touch();
                    return Ok(());
                }
                FsserveCtl::Withdraw if volume.claimed => {
                    volume.retiring = true;
                    return Ok(());
                }
                FsserveCtl::Withdraw => volume.claimed = true,
            }
        }
        self.retire_claimed(offer, wiring.audit, "withdrawn", departed);
        Ok(())
    }

    /// Whether the volume published as `volume_id` is a network volume.
    pub(super) fn holds(&self, volume_id: &[u8; 16]) -> bool {
        self.volumes
            .lock()
            .iter()
            .any(|v| v.volume_id == *volume_id)
    }

    /// The server serving over `endpoint` is gone: retire each volume it
    /// offered, telling `departed` of each that leaves the namespace or stays
    /// lost. Takes no sleeping lock.
    pub(super) fn server_lost(
        &self,
        endpoint: u64,
        audit: &'static (dyn Sink + Sync),
        departed: Departed<'_>,
    ) {
        loop {
            let offer = {
                let mut volumes = self.volumes.lock();
                // A lost one has been retired already: its owner's detach is
                // what remains of it.
                let Some(volume) = volumes.iter_mut().find(|v| {
                    v.endpoint == endpoint && !v.retiring && !matches!(v.stage, Stage::Lost(_))
                }) else {
                    return;
                };
                if volume.claimed {
                    volume.retiring = true;
                    continue;
                }
                volume.claimed = true;
                volume.offer
            };
            self.retire_claimed(offer, audit, "server_lost", departed);
        }
    }

    /// Attach the volume offered as `offer` for `caller`, as `attach` asks.
    /// One whose target is refused leaves again, `held` settling what was
    /// held on it meanwhile.
    ///
    /// # Errors
    ///
    /// As [`tairix_kernel_core::VolumeService::attach_offered`].
    pub(super) fn attach(
        &self,
        wiring: &Wiring,
        caller: &Principal<'_>,
        offer: OfferId,
        attach: &NetworkAttach<'_>,
        name_reserved: &dyn Fn(&str) -> bool,
        held: HeldOn<'_>,
    ) -> Result<AttachReply, Errno> {
        let audit = wiring.audit;
        let refused = |cause: &'static str, errno: Errno| {
            record(
                audit,
                Level::Warn,
                NETWORK_ATTACH_REFUSED,
                ATTACH_REFUSED,
                attach.name,
                cause,
            );
            errno
        };
        let (claim, offered) = self.claim_offer(offer, caller.uid, audit, held.departed)?;
        let presented = Presented::asked_by(caller, attach.presentation)
            .map_err(|errno| refused("presentation_refused", errno))?;
        let read_only = attach.read_only || offered.serve.write_refusal.is_some();
        let handle = self.mint_handle()?;
        let lanes = Self::lanes(&offered, presented, audit)
            .map_err(|errno| refused("lanes_unbuilt", errno))?;
        let state = lanes.state;
        let name = Self::mount(
            handle,
            read_only,
            presented,
            attach.name,
            offered.volume_id,
            name_reserved,
        )
        .map_err(|errno| refused("name_unavailable", errno))?;
        let unmount = || {
            if let Ok(vfs) = LATE_FILESYSTEM.vfs() {
                let _ = vfs.mounts_write().detach(handle, BindsWithin::Withdraw);
            }
        };
        let reply = AttachReply::new(offered.volume_id, &name, read_only).map_err(|errno| {
            unmount();
            refused("name_unrepliable", errno)
        })?;
        if let Err(err) =
            publish_volume_identity(offered.volume_id, &["Storage", &name], &name, audit)
        {
            unmount();
            return Err(refused(
                "identity_unpublishable",
                match err {
                    VolumePublishError::NilIdentity => Errno::OutOfRange,
                    VolumePublishError::AlreadyPublished => Errno::AlreadyExists,
                },
            ));
        }
        let registered = LATE_FILESYSTEM.register_network(
            handle,
            lanes.drivers,
            &offered.source,
            offered.serve.format.as_str(),
            offered.volume_id,
            Some(presented.owner(caller.uid)),
        );
        if registered.is_err() {
            unmount();
            let _ = VOLUME_FOREST.unpublish(&offered.volume_id);
            return Err(refused("handle_in_use", Errno::Busy));
        }
        let _ = LATE_FILESYSTEM.set_served_state(handle, Some(state));
        let attached = Attached {
            handle,
            name,
            owner: caller.uid,
        };
        if let Some(target) = attach.target {
            let bind = ViewBindRequest {
                volume_id: offered.volume_id,
                subtree: "/",
                target,
                narrowing: MountFlags::default(),
            };
            if let Err(errno) = FS_SERVICE.view_bind(caller.uid, caller.caps, &bind) {
                // Reachable from the moment it was registered, so an image
                // attached on it meanwhile is detached first, and what anyone
                // left held on the server is let go with it.
                let _ = (held.settle)(attached.handle, true);
                if take_down(attached.handle, offered.volume_id, true, Writes::Served).is_ok() {
                    (held.departed)(attached.handle);
                }
                return Err(refused("target_refused", errno));
            }
        }
        record(
            audit,
            Level::Info,
            NETWORK_VOLUME_ATTACHED,
            "network-volume: attached for its principal",
            &attached.name,
            presentation_word(attach.presentation),
        );
        {
            let mut volumes = self.volumes.lock();
            if let Some(at) = position(&volumes, offer) {
                volumes[at].stage = Stage::Attached(attached);
            }
        }
        drop(claim);
        Ok(reply)
    }

    /// Detach the network volume `request` names, for `caller`, once `held`
    /// has settled what is held on it, telling it once the volume has left:
    /// what entered on it as it was detached leaves with it.
    ///
    /// # Errors
    ///
    /// As [`tairix_kernel_core::VolumeService::detach`].
    pub(super) fn detach(
        &self,
        wiring: &Wiring,
        request: &VolumeDetachRequest,
        caller: &Principal<'_>,
        held: HeldOn<'_>,
    ) -> Result<(), Errno> {
        let audit = wiring.audit;
        let departed = held.departed;
        let (claim, attached, lost, endpoint) = {
            let mut volumes = self.volumes.lock();
            let volume = volumes
                .iter_mut()
                .find(|v| v.volume_id == request.volume_id)
                .ok_or(Errno::NotFound)?;
            let (attached, lost) = match volume.stage {
                Stage::Attached(attached) => (attached, false),
                Stage::Lost(attached) => (attached, true),
                Stage::Offered { .. } => return Err(Errno::NotFound),
            };
            if attached.owner != caller.uid && !caller.may_mount() {
                drop(volumes);
                record(
                    audit,
                    Level::Warn,
                    NETWORK_DETACH_REFUSED,
                    DETACH_REFUSED,
                    &attached.name,
                    "not_owner",
                );
                return Err(Errno::PermissionDenied);
            }
            if volume.claimed {
                return Err(Errno::Busy);
            }
            volume.claimed = true;
            let claim = Claim {
                volumes: self,
                offer: volume.offer,
                audit,
                departed,
            };
            (claim, attached, lost, volume.endpoint)
        };
        let force = request.mode.forces(lost);
        let given_up = match (held.settle)(attached.handle, force).and_then(|()| {
            let writes = if lost { Writes::Lost } else { Writes::Served };
            take_down(attached.handle, request.volume_id, force, writes)
        }) {
            Ok(given_up) => given_up,
            Err((cause, errno)) => {
                record(
                    audit,
                    refusal_level(request.mode, errno),
                    NETWORK_DETACH_REFUSED,
                    DETACH_REFUSED,
                    &attached.name,
                    cause,
                );
                return Err(errno);
            }
        };
        {
            let mut volumes = self.volumes.lock();
            if let Some(at) = volumes
                .iter()
                .position(|v| v.volume_id == request.volume_id)
            {
                volumes.remove(at);
            }
        }
        drop(claim);
        departed(attached.handle);
        end_endpoint(endpoint, audit);
        record(
            audit,
            if force { Level::Warn } else { Level::Info },
            NETWORK_VOLUME_DETACHED,
            "network-volume: detached",
            &attached.name,
            if given_up.unflushed {
                "forced_unflushed"
            } else if force {
                "forced"
            } else {
                "clean"
            },
        );
        Ok(())
    }

    /// Retire the volume offered as `offer`, which the caller has claimed,
    /// for `cause`, and let the claim go: an offer goes with its channel; an
    /// attached volume leaves the namespace, or stays lost while its server
    /// held writes it had not published, and `departed` is told once the
    /// registry lock is let go. Takes no sleeping lock.
    fn retire_claimed(
        &self,
        offer: OfferId,
        audit: &'static (dyn Sink + Sync),
        cause: &str,
        departed: Departed<'_>,
    ) {
        let retiring = {
            let mut volumes = self.volumes.lock();
            let Some(at) = position(&volumes, offer) else {
                return;
            };
            let volume = &mut volumes[at];
            match volume.stage {
                Stage::Attached(attached) => Retiring {
                    attached,
                    volume_id: volume.volume_id,
                    source: volume.source,
                    format: volume.serve.format,
                    endpoint: volume.endpoint,
                },
                // Retired already; its owner's detach is what remains.
                Stage::Lost(_) => {
                    volume.claimed = false;
                    volume.retiring = false;
                    return;
                }
                Stage::Offered { .. } => {
                    let dropped = volumes.remove(at);
                    drop(volumes);
                    record_retired(
                        audit,
                        NETWORK_VOLUME_RETIRED,
                        RETIRED,
                        &dropped.source,
                        cause,
                        "offer_dropped",
                    );
                    end_endpoint(dropped.endpoint, audit);
                    return;
                }
            }
        };
        let handle = retiring.attached.handle;
        let lost = LATE_FILESYSTEM.holds_unpublished(handle);
        if lost {
            retiring.stand_in();
        } else {
            if let Ok(vfs) = LATE_FILESYSTEM.vfs() {
                let _ = vfs.mounts_write().detach(handle, BindsWithin::Withdraw);
            }
            let _ = LATE_FILESYSTEM.unregister(handle);
            let _ = VOLUME_FOREST.unpublish(&retiring.volume_id);
        }
        {
            let mut volumes = self.volumes.lock();
            if let Some(at) = position(&volumes, offer) {
                if lost {
                    let volume = &mut volumes[at];
                    volume.stage = Stage::Lost(retiring.attached);
                    volume.claimed = false;
                    volume.retiring = false;
                } else {
                    volumes.remove(at);
                }
            }
        }
        record_retired(
            audit,
            NETWORK_VOLUME_RETIRED,
            RETIRED,
            &retiring.attached.name,
            cause,
            if lost {
                "unpublished_writes_lost"
            } else {
                "retracted"
            },
        );
        end_endpoint(retiring.endpoint, audit);
        departed(handle);
    }

    /// Claim the offer `offer` made to `uid`.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for no offer made to `uid` under that id, and
    /// [`Errno::AlreadyExists`] for one attached already; [`Errno::Busy`] for
    /// one another operation holds.
    fn claim_offer<'a>(
        &'a self,
        offer: OfferId,
        uid: u32,
        audit: &'static (dyn Sink + Sync),
        departed: Departed<'a>,
    ) -> Result<(Claim<'a>, Claimed), Errno> {
        let mut volumes = self.volumes.lock();
        let at = position(&volumes, offer)
            .filter(|&at| volumes[at].for_uid == uid)
            .ok_or(Errno::NotFound)?;
        let volume = &mut volumes[at];
        let Stage::Offered { endpoint, window } = &volume.stage else {
            return Err(Errno::AlreadyExists);
        };
        if volume.claimed {
            return Err(Errno::Busy);
        }
        let claimed = Claimed {
            endpoint: Arc::clone(endpoint),
            window: Arc::clone(window),
            volume_id: volume.volume_id,
            source: volume.source,
            serve: volume.serve,
            lanes: volume.lanes,
            activity: Arc::clone(&volume.activity),
        };
        volume.claimed = true;
        Ok((
            Claim {
                volumes: self,
                offer,
                audit,
                departed,
            },
            claimed,
        ))
    }

    /// A mount handle no volume has held this boot.
    fn mint_handle(&self) -> Result<DriverHandle, Errno> {
        let serial = self.next_handle.fetch_add(1, Ordering::Relaxed);
        DriverHandle::from_raw(serial).map_err(|_| Errno::NoSpace)
    }

    /// The lanes `offered` is served by, each presented as `presented` says.
    fn lanes(
        offered: &Claimed,
        presented: Presented,
        audit: &'static (dyn Sink + Sync),
    ) -> Result<Built, Errno> {
        let built = ServedFs::lanes(
            &offered.endpoint,
            &offered.window,
            offered.lanes,
            offered.serve,
            &offered.activity,
            audit,
        )?;
        let state = built
            .first()
            .map(ServedFs::state)
            .ok_or(Errno::OutOfRange)?;
        let mut drivers: Vec<Box<dyn KernelFs>> = Vec::new();
        drivers
            .try_reserve_exact(built.len())
            .map_err(|_| Errno::OutOfMemory)?;
        for fs in built {
            let lane =
                Box::try_new(PresentedFs::whole(fs, presented)).map_err(|_| Errno::OutOfMemory)?;
            drivers.push(lane);
        }
        Ok(Built { drivers, state })
    }

    /// Show the volume `handle` serves in the catalog under the first name
    /// the naming policy gives for `asked` that is free.
    ///
    /// # Errors
    ///
    /// [`Errno::AlreadyExists`] once every name is taken; the mount table's
    /// other refusals.
    fn mount(
        handle: DriverHandle,
        read_only: bool,
        presented: Presented,
        asked: &str,
        volume_id: [u8; 16],
        name_reserved: &dyn Fn(&str) -> bool,
    ) -> Result<ArrayString<VOLUME_NAME_MAX>, Errno> {
        let vfs = LATE_FILESYSTEM.vfs()?;
        for candidate in candidates(NameBase::of(asked.as_bytes(), None), volume_id) {
            let name = core::str::from_utf8(candidate.as_bytes()).map_err(|_| Errno::OutOfRange)?;
            if name_reserved(name) {
                continue;
            }
            let held = ArrayString::try_from(name).map_err(|_| Errno::OutOfRange)?;
            let path = storage_path(name).map_err(|(_, errno)| errno)?;
            let attachment = Attachment::new(handle, None, mount_flags(read_only));
            match vfs
                .mounts_write()
                .attach_at(attachment, path, presented.template())
            {
                Ok(()) => return Ok(held),
                Err(VfsError::AlreadyExists) => {}
                Err(err) => return Err(err.to_errno()),
            }
        }
        Err(Errno::AlreadyExists)
    }
}

#[cfg(test)]
impl NetworkVolumes {
    /// Run `during` while network volume `id` is held as an attach or detach
    /// holds it, then let it go, telling `departed` if it was retired
    /// meanwhile.
    pub(super) fn while_held<T>(
        &self,
        id: &[u8; 16],
        audit: &'static (dyn Sink + Sync),
        departed: Departed<'_>,
        during: impl FnOnce() -> T,
    ) -> T {
        let held = {
            let mut volumes = self.volumes.lock();
            volumes
                .iter_mut()
                .find(|v| v.volume_id == *id && !v.claimed)
                .map(|volume| {
                    volume.claimed = true;
                    Claim {
                        volumes: self,
                        offer: volume.offer,
                        audit,
                        departed,
                    }
                })
        };
        assert!(held.is_some(), "a network volume nothing holds");
        let outcome = during();
        drop(held);
        outcome
    }
}

/// What the volumes held on a network volume need as it leaves.
#[derive(Clone, Copy)]
pub(super) struct HeldOn<'a> {
    /// Settles them before the volume, registered as the handle, is
    /// detached, forced or not: `(cause, errno)` for what keeps it.
    pub(super) settle: &'a dyn Fn(DriverHandle, bool) -> Result<(), (&'static str, Errno)>,
    /// Retires them once it has left.
    pub(super) departed: Departed<'a>,
}

/// What retires the volumes held on a network volume that has left the
/// namespace or stands in lost, told the handle it was registered as,
/// whichever path it left by. Called with no registry lock held.
pub(super) type Departed<'a> = &'a dyn Fn(DriverHandle);

/// What a network attach mounts: its lanes, and the state their server
/// reports.
struct Built {
    drivers: Vec<Box<dyn KernelFs>>,
    state: Arc<ServedState>,
}

/// Where offer `offer` sits in `volumes`.
fn position(volumes: &[NetworkVolume], offer: OfferId) -> Option<usize> {
    volumes.binary_search_by_key(&offer, |v| v.offer).ok()
}

/// The identity of the volume the application `app` serves `uid` under
/// `key`.
fn identity(app: &AppIdentity, uid: u32, key: &[u8; OFFER_KEY_LEN]) -> [u8; 16] {
    let bundle = app.bundle_id().as_bytes();
    let mut hash = Sha256Stream::new();
    hash.update(IDENTITY_CONTEXT);
    hash.update(app.publisher().as_bytes());
    hash.update(
        &u64::try_from(bundle.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    hash.update(bundle);
    hash.update(&uid.to_le_bytes());
    hash.update(key);
    let digest = hash.finalize();
    let mut id = [0; 16];
    id.copy_from_slice(&digest[..16]);
    id
}

/// An attached network volume its retirement acts on, read out of the
/// registry under the retirement's claim.
struct Retiring {
    attached: Attached,
    volume_id: [u8; 16],
    source: ArrayString<MOUNT_SOURCE_MAX>,
    format: FormatName,
    endpoint: u64,
}

impl Retiring {
    /// Put the fail-closed stand-in in the place of its lanes, under its
    /// registration, and report it lost. Parks on nothing.
    fn stand_in(&self) {
        super::RuntimeVolumeService::stand_in(
            self.attached.handle,
            (self.source.as_str(), self.format, self.volume_id),
            MountAvailability::UnavailableLost,
        );
    }
}

/// Take the served endpoint `endpoint` down, if it is still up: its server
/// learns its volume is gone by the endpoint going.
fn end_endpoint(endpoint: u64, audit: &'static (dyn Sink + Sync)) {
    if let Some(endpoint) = tairix_kernel_core::callreg::unregister(EndpointId(endpoint)) {
        endpoint.destroy(audit);
    }
}

/// The message of a refused attach.
const ATTACH_REFUSED: &str = "network-volume: attach refused";

/// The message of a refused detach.
const DETACH_REFUSED: &str = "network-volume: detach refused";

/// The message of a retirement.
const RETIRED: &str = "network-volume: its server retired it";

#[cfg(test)]
#[path = "network_volume_tests.rs"]
mod tests;
