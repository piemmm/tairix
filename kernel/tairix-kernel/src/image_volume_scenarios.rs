//! Image volumes over the real service: attached from a file on a served
//! volume for the principal that opened it, refused with their reason, held
//! to one writer, ejected, ended with their sessions, settled ahead of the
//! volume their file lies on and retired with it, nested, bounded, named and
//! bound as asked, refused by their driver, kept lost while their driver held
//! writes, served again after their driver is lost, and undone when their
//! answer cannot be delivered.

use tairix_abi::driver::filesystem::MountFlags;
use tairix_abi::imageattach::{ImageAccess, ImageAttach, ImageAttachReply, ImageRefusal};
use tairix_abi::netattach::{AttachReply, FsserveCtl, Presentation};
use tairix_abi::sysinfo::{VolumeClass, MOUNT_SOURCE_MAX};
use tairix_abi::volume::{
    ViewBindRequest, VolumeCtl, VolumeFsTypes, ARCHIVE_NEXT, ARCHIVE_STAGING,
};
use tairix_abi::{DriverError, ProcId};
use tairix_kernel_core::fs::{ImageDirectory, ImageSource};
use tairix_kernel_core::procsignal::{self, DeferredTeardown};
use tairix_kernel_core::session_reaper::SessionObserver as _;
use tairix_kernel_sec::ProcessId;
use tairix_test_amiga_image::{ffs_volume, rdb_disk, FLOPPY_BLOCKS};

use super::*;
use crate::hwtree_node_ids::IMAGE_VOLUMES_NODE_ID;
use crate::volume_service::image::{IMAGE_NESTING_MAX, IMAGE_VOLUMES_PER_PRINCIPAL};

const IMAGE_ATTACH_REFUSED: u32 = 4322;
const IMAGE_VOLUME_DETACHED: u32 = 4323;
const IMAGE_DETACH_REFUSED: u32 = 4324;
const IMAGE_VOLUME_RETIRED: u32 = 4325;

/// Every image scenario, in turn.
pub(super) fn run(window_ptr: *mut u8, facility: &'static TestFacility) {
    an_amiga_image_attaches_for_its_owner_alone_scenario(window_ptr, facility);
    an_image_is_refused_with_its_reason_scenario(window_ptr, facility);
    a_writable_image_has_one_writer_scenario(window_ptr, facility);
    a_disc_image_is_read_only_and_shared_scenario(window_ptr, facility);
    an_acorn_floppy_image_attaches_as_adfs_scenario(window_ptr, facility);
    an_image_leaves_with_its_session_scenario(window_ptr, facility);
    an_image_is_settled_before_its_backing_volume_detaches_scenario(window_ptr, facility);
    an_image_leaves_with_its_pulled_backing_volume_scenario(window_ptr, facility);
    an_image_within_an_image_is_held_and_settled_scenario(window_ptr, facility);
    an_image_whose_driver_is_lost_is_served_again_scenario(window_ptr, facility);
    an_image_whose_driver_is_lost_ejects_without_a_force_scenario(window_ptr, facility);
    a_detach_taking_the_lost_forces_no_healthy_volume_scenario(window_ptr, facility);
    an_image_count_is_bounded_per_principal_scenario(window_ptr, facility);
    an_image_asked_writable_is_writable_or_refused_scenario(window_ptr, facility);
    an_image_nesting_is_bounded_scenario(window_ptr, facility);
    a_drivers_refusal_reaches_the_attach_scenario(window_ptr, facility);
    an_image_takes_the_name_and_target_asked_scenario(window_ptr, facility);
    an_image_held_by_two_sessions_leaves_with_the_last_scenario(window_ptr, facility);
    an_image_holding_unpublished_writes_stays_lost_until_ejected_scenario(window_ptr, facility);
    an_image_whose_restarted_driver_refuses_it_leaves_scenario(window_ptr, facility);
    an_image_on_a_withdrawn_share_leaves_with_it_scenario(facility);
    an_image_is_settled_before_its_backing_share_detaches_scenario(facility);
    an_image_on_a_share_withdrawn_while_held_leaves_with_it_scenario(facility);
    an_image_on_a_share_whose_target_is_refused_leaves_with_it_scenario(facility);
    a_killed_attach_takes_its_volume_out_scenario(window_ptr, facility);
    an_undelivered_answer_leaves_nothing_behind_scenario(window_ptr, facility);
    an_owed_image_taken_up_again_lives_on_scenario(window_ptr, facility);
    a_name_the_naming_policy_keeps_is_never_taken_scenario(window_ptr, facility);
    an_image_within_an_image_leaves_with_their_pulled_volume_scenario(window_ptr, facility);
    an_attach_that_does_not_adopt_leaves_its_volume_as_it_is_scenario(window_ptr, facility);
    a_read_only_attach_takes_up_no_writable_volume_scenario(window_ptr, facility);
    an_image_named_past_the_record_is_listed_cut_scenario(window_ptr, facility);
    a_teardown_failing_beneath_its_volume_waits_for_the_holder_scenario(window_ptr, facility);
    an_image_held_as_its_volume_is_pulled_leaves_with_what_lies_on_it_scenario(
        window_ptr, facility,
    );
    a_name_an_image_awaits_its_driver_under_is_held_for_it_scenario(window_ptr, facility);
    an_image_opened_through_a_bind_outlives_its_unbind_scenario(window_ptr, facility);
    an_image_opened_through_a_bind_outlives_the_volume_it_stood_in_scenario(window_ptr, facility);
    an_archive_without_its_directory_attaches_read_only_scenario(window_ptr, facility);
    an_archive_is_written_by_replacing_its_file_scenario(window_ptr, facility);
    an_archives_work_files_grow_only_by_what_is_written_scenario(window_ptr, facility);
    an_archives_lost_driver_leaves_no_work_files_scenario(window_ptr, facility);
    an_archive_reached_through_a_read_only_view_attaches_read_only_scenario(window_ptr, facility);
}

/// A served FAT32 volume shown as `/Storage/<name>`, the `n`th these
/// scenarios insert, with its identity.
fn host(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
    n: u32,
    name: &'static [u8],
) -> ([u8; 16], Inserted) {
    let (image, identity) = fat32_image(0x1A6E_0000 + n, b"host");
    let round = insert(
        window_ptr,
        facility,
        &Insertion {
            endpoint_id: 0xB1D0_5E17_0000_0100 + u64::from(n),
            owner: host_owner(n),
            region_task: 0x70_0100 + u64::from(n),
            name,
            fstype: VolumeFsType::Fat32,
        },
        ServedDevice::new(image),
    );
    assert_eq!(round.outcome, Ok(()), "the host attaches");
    (identity, round)
}

/// The process the `n`th host's block driver runs as.
fn host_owner(n: u32) -> u64 {
    0x70_1100 + u64::from(n)
}

/// Write all of `bytes` to `path` as `uid`, creating it.
fn put(uid: u32, path: &str, bytes: &[u8]) {
    let file = Descriptor::open(uid, path, OpenFlags::WRITE.union(OpenFlags::CREATE))
        .unwrap_or_else(|err| std::panic!("create {path}: {err:?}"));
    let mut at = 0;
    while at < bytes.len() {
        at += file.write(at as u64, &bytes[at..]).expect("written");
    }
}

/// An 880 KiB AmigaDOS floppy, FFS, labelled `label`, holding `ReadMe`.
fn amiga_floppy(label: &[u8], text: &[u8]) -> Vec<u8> {
    amiga_volume(FLOPPY_BLOCKS, label, text)
}

/// An AmigaDOS FFS volume of `blocks` blocks, labelled `label`, holding
/// `ReadMe`.
fn amiga_volume(blocks: u64, label: &[u8], text: &[u8]) -> Vec<u8> {
    ffs_volume(RamBlock::new(blocks), label, b"ReadMe", text)
        .expect("lays out")
        .data
}

fn read_write() -> OpenFlags {
    OpenFlags::READ.union(OpenFlags::WRITE)
}

/// What an attach of `path`, opened as `uid` under `flags`, hands the
/// service: the caller's own open, frozen, closed once handed over.
fn source(uid: u32, path: &str, flags: OpenFlags, session: ProcId, task: u64) -> ImageSource {
    let opened = Descriptor::open(uid, path, flags)
        .unwrap_or_else(|err| std::panic!("open {path}: {err:?}"));
    ImageSource {
        file: NodeHandle {
            authority: Authority::Frozen(FrozenAuthority {
                uid,
                caps: CapabilitySet::empty(),
                grant: None,
            }),
            ..opened.handle().clone()
        },
        writable: flags.is_write(),
        directory: None,
        session,
        task,
    }
}

/// The attach a double-click makes: private, named from the volume.
fn private(read_only: bool) -> ImageAttach<'static> {
    ImageAttach {
        name: None,
        presentation: Presentation::Private,
        access: if read_only {
            ImageAccess::ReadOnly
        } else {
            ImageAccess::WritableWherePossible
        },
        target: None,
        expects: None,
        adopt: true,
        file_name: None,
    }
}

/// The session the scenarios attach in unless they end one.
fn session() -> ProcId {
    ProcId::from_raw([0x51; 16])
}

/// Run `attach` while a stand-in for `devmgr` hands each image volume node
/// published meanwhile to `start`, as it loads the format's driver.
fn with_devmgr<T>(
    start: impl Fn(u32, u64, VolumeFsType) + Send + 'static,
    attach: impl FnOnce() -> T,
) -> T {
    let done = StdArc::new(AtomicBool::new(false));
    let devmgr = {
        let done = StdArc::clone(&done);
        thread::spawn(move || {
            while !done.load(Ordering::Acquire) {
                for (node, volume, fstype) in undriven_volume_nodes(IMAGE_VOLUMES_NODE_ID) {
                    start(node, volume, fstype);
                }
                thread::yield_now();
            }
        })
    };
    let answer = attach();
    done.store(true, Ordering::Release);
    devmgr.join().expect("the devmgr stand-in");
    answer
}

/// What `devmgr` does with a published node: start the format's driver.
fn driving(facility: &'static TestFacility) -> impl Fn(u32, u64, VolumeFsType) + Send + 'static {
    move |node, volume, fstype| start_driver(facility, node, volume, fstype)
}

/// Refuse volume `volume`, published as `node`, for `why`, as a driver that
/// cannot open it does.
fn refusing(node: u32, volume: u64, why: DriverError) {
    DRIVEN.lock().unwrap().push((node, 0));
    VOLUME_SERVICE
        .refuse(volume, why)
        .expect("the volume awaits its driver");
}

/// Attach `source` for `caller` as `attach` asks, starting the volume's
/// driver as `devmgr` does once its node is published.
fn attached(
    facility: &'static TestFacility,
    caller: &Principal<'_>,
    source: ImageSource,
    attach: &ImageAttach<'_>,
) -> Result<ImageAttachReply, Errno> {
    with_devmgr(driving(facility), || answered(caller, source, attach))
}

/// The answer an attach of `source` for `caller`, as `attach` asks, delivers.
fn answered(
    caller: &Principal<'_>,
    source: ImageSource,
    attach: &ImageAttach<'_>,
) -> Result<ImageAttachReply, Errno> {
    let mut delivered = None;
    VOLUME_SERVICE.attach_image(caller, source, attach, &mut |answer| {
        delivered = Some(*answer);
        Ok(())
    })?;
    Ok(delivered.expect("an attach that succeeds delivers its answer"))
}

/// Do every teardown owed, as the session notifier's tasks would.
fn settled() {
    while VOLUME_SERVICE.settle() {}
    assert!(!VOLUME_SERVICE.owes(), "nothing owed is left undone");
}

/// Attach every file of `paths` read-only for `uid` in `gid`, all at once,
/// each on a task of its own.
fn attached_together(
    facility: &'static TestFacility,
    uid: u32,
    gid: u32,
    paths: Vec<String>,
) -> Vec<Result<ImageAttachReply, Errno>> {
    with_devmgr(driving(facility), || {
        let attaches: Vec<_> = paths
            .into_iter()
            .zip(0x1A00_0100u64..)
            .map(|(path, task)| {
                thread::spawn(move || {
                    let from = source(uid, &path, OpenFlags::READ, session(), task);
                    answered(&plain(uid, gid), from, &private(true))
                })
            })
            .collect();
        attaches
            .into_iter()
            .map(|attach| attach.join().expect("an attach"))
            .collect()
    })
}

/// The volume an attach that must succeed attached.
fn mounted(answer: Result<ImageAttachReply, Errno>) -> AttachReply {
    match answer {
        Ok(ImageAttachReply::Attached(volume)) => volume,
        other => std::panic!("attached, not {other:?}"),
    }
}

/// Detach the image volume `volume` for `caller`.
fn eject(volume: &AttachReply, mode: DetachMode, caller: &Principal<'_>) -> Result<(), Errno> {
    VOLUME_SERVICE.detach(
        &VolumeDetachRequest {
            volume_id: volume.volume_id,
            mode,
        },
        caller,
    )
}

/// Whether `/Storage/<name>` is shown.
fn shown(name: &str) -> bool {
    let at = Path::parse(&std::format!("/Storage/{name}")).expect("a path");
    let vfs = LATE_FILESYSTEM.vfs().expect("installed");
    vfs.mounts().resolve(&at).path() == &at
}

/// An Amiga floppy image on a served volume attaches for the member who
/// opened it, private to them, writable through their descriptor and kept
/// past its close; the same file answers the volume it has; only its owner,
/// or the mount authority, ejects it; and what was written is in the file
/// afterwards.
fn an_amiga_image_attaches_for_its_owner_alone_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 0, b"imgs0");
    let path = "/Storage/imgs0/Lemmings.adf";
    put(0, path, &amiga_floppy(b"Lemmings", b"amiga payload"));
    let owner = plain(MEMBER_UID, STORAGE_GID);
    let open = |flags| source(MEMBER_UID, path, flags, session(), 0x1A00_0001);
    let volume = mounted(attached(
        facility,
        &owner,
        open(read_write()),
        &private(false),
    ));
    assert_eq!(volume.name(), "lemmings", "named from the volume's label");
    assert!(!volume.read_only(), "its reply says it takes writes");
    let record = mount_record(b"Lemmings.adf");
    assert_eq!(record.class(), VolumeClass::Image);
    assert_eq!(record.owner(), Some(MEMBER_UID));
    assert_eq!(record.fstype_bytes(), b"affs");
    assert_eq!(record.volume_id(), volume.volume_id);

    let mut buf = [0u8; 32];
    let read = read_as(MEMBER_UID, "/Storage/lemmings/ReadMe", 0, &mut buf).expect("reads");
    assert_eq!(&buf[..read], b"amiga payload");
    assert_eq!(
        read_as(OUTSIDER_UID, "/Storage/lemmings/ReadMe", 0, &mut buf).err(),
        Some(Errno::PermissionDenied),
        "private to its owner"
    );
    let stat = Descriptor::open(MEMBER_UID, "/Storage/lemmings/ReadMe", OpenFlags::READ)
        .and_then(|file| file.stat())
        .expect("stats");
    assert_eq!((stat.uid, stat.mode & 0o777), (MEMBER_UID, 0o600));
    write_as(MEMBER_UID, "/Storage/lemmings/ReadMe", 0, b"AMIGA").expect("writes");

    assert_eq!(
        attached(facility, &owner, open(read_write()), &private(false)),
        Ok(ImageAttachReply::AlreadyAttached(volume)),
        "one file holds one volume"
    );
    assert_eq!(
        eject(
            &volume,
            DetachMode::Plain,
            &plain(OUTSIDER_UID, OUTSIDER_UID)
        ),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        audited(IMAGE_DETACH_REFUSED, "lemmings"),
        [Some(String::from("not_owner"))]
    );
    eject(&volume, DetachMode::Plain, &owner).expect("its owner ejects it");
    assert!(!shown("lemmings"));
    assert_eq!(
        audited(IMAGE_VOLUME_DETACHED, "lemmings"),
        [Some(String::from("ejected"))]
    );

    let again = mounted(attached(
        facility,
        &owner,
        open(OpenFlags::READ),
        &private(false),
    ));
    assert_eq!(
        again.volume_id, volume.volume_id,
        "the same file, the same identity"
    );
    assert!(again.read_only(), "a read-only descriptor's volume says so");
    let read = read_as(MEMBER_UID, "/Storage/lemmings/ReadMe", 0, &mut buf).expect("reads");
    assert_eq!(&buf[..read], b"AMIGA payload", "the write reached the file");
    assert_eq!(
        write_as(MEMBER_UID, "/Storage/lemmings/ReadMe", 0, b"x").err(),
        Some(Errno::ReadOnly),
        "a read-only descriptor makes a read-only volume"
    );
    eject(&again, DetachMode::Plain, &admin()).expect("the mount authority ejects any");
    retire(host_id, DetachMode::Plain, round);
}

/// A file holding no volume the kernel attaches is refused with the reason,
/// and nothing is published for it.
fn an_image_is_refused_with_its_reason_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 1, b"imgs1");
    put(0, "/Storage/imgs1/noise.img", &vec![0x5Au8; 64 * 1024]);
    put(
        0,
        "/Storage/imgs1/linux.img",
        &ext_fixture::sparse(EXT2_FIXTURE),
    );
    put(0, "/Storage/imgs1/workbench.hdf", &rdb_disk(128, 0));
    put(0, "/Storage/imgs1/floppy.adf", &amiga_floppy(b"Disk", b"x"));
    put(
        0,
        "/Storage/imgs1/dvd.iso",
        &ext_fixture::sparse(ISO_FIXTURE),
    );
    let caller = plain(MEMBER_UID, STORAGE_GID);
    let expecting_a_disc = ImageAttach {
        expects: VolumeFsTypes::of([VolumeFsType::Iso9660]),
        ..private(true)
    };
    // A set the disc's filesystem is not in.
    let expecting_another = ImageAttach {
        expects: VolumeFsTypes::of([VolumeFsType::Udf, VolumeFsType::Fat32]),
        ..private(true)
    };
    for (file, attach, why) in [
        ("noise.img", private(true), ImageRefusal::Unrecognised),
        ("linux.img", private(true), ImageRefusal::RecordsOwners),
        ("workbench.hdf", private(true), ImageRefusal::Partitioned),
        ("floppy.adf", expecting_a_disc, ImageRefusal::NotExpected),
        ("dvd.iso", expecting_another, ImageRefusal::NotExpected),
    ] {
        let path = std::format!("/Storage/imgs1/{file}");
        let from = source(MEMBER_UID, &path, OpenFlags::READ, session(), 0x1A00_0002);
        assert_eq!(
            attached(facility, &caller, from, &attach),
            Ok(ImageAttachReply::Refused(why)),
            "{file}"
        );
        assert_eq!(
            audited(IMAGE_ATTACH_REFUSED, file),
            [Some(String::from(why.as_str()))]
        );
    }
    assert!(
        undriven_volume_nodes(IMAGE_VOLUMES_NODE_ID).is_empty(),
        "a refused file publishes nothing"
    );
    retire(host_id, DetachMode::Plain, round);
}

/// A writable attachment locks its file exclusively: a second attach of it,
/// writable or not, waits its turn, and takes it once the first is ejected.
fn a_writable_image_has_one_writer_scenario(window_ptr: *mut u8, facility: &'static TestFacility) {
    let (host_id, round) = host(window_ptr, facility, 2, b"imgs2");
    let path = "/Storage/imgs2/shared.adf";
    put(0, path, &amiga_floppy(b"Shared", b"one writer"));
    let member = plain(MEMBER_UID, STORAGE_GID);
    let first = mounted(attached(
        facility,
        &member,
        source(MEMBER_UID, path, read_write(), session(), 0x1A00_0003),
        &private(false),
    ));
    let system = plain(0, 0);
    for flags in [read_write(), OpenFlags::READ] {
        assert_eq!(
            attached(
                facility,
                &system,
                source(0, path, flags, session(), 0x1A00_0004),
                &private(false),
            ),
            Err(Errno::Busy),
            "the file has its writer"
        );
    }
    assert_eq!(
        audited(IMAGE_ATTACH_REFUSED, "shared.adf"),
        [
            Some(String::from("file_locked")),
            Some(String::from("file_locked"))
        ]
    );
    eject(&first, DetachMode::Plain, &member).expect("ejects");
    let second = mounted(attached(
        facility,
        &system,
        source(0, path, read_write(), session(), 0x1A00_0004),
        &private(false),
    ));
    assert_ne!(
        second.volume_id, first.volume_id,
        "another principal's volume"
    );
    eject(&second, DetachMode::Plain, &system).expect("ejects");
    retire(host_id, DetachMode::Plain, round);
}

/// A disc image attaches read-only whatever its descriptor allows, so its
/// lock is shared: a second principal attaches the same file beside it.
fn a_disc_image_is_read_only_and_shared_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 3, b"imgs3");
    let path = "/Storage/imgs3/disc.iso";
    put(0, path, &ext_fixture::sparse(ISO_FIXTURE));
    let member = plain(MEMBER_UID, STORAGE_GID);
    // A util-linux list naming either face a disc may have.
    let either_face = ImageAttach {
        expects: VolumeFsTypes::of([VolumeFsType::Udf, VolumeFsType::Iso9660]),
        ..private(false)
    };
    let first = mounted(attached(
        facility,
        &member,
        source(MEMBER_UID, path, read_write(), session(), 0x1A00_0005),
        &either_face,
    ));
    let target = std::format!("/Storage/{}/README.TXT", first.name());
    let mut buf = [0u8; 16];
    let read = read_as(MEMBER_UID, &target, 0, &mut buf).expect("reads");
    assert_eq!(&buf[..read], b"hello\n");
    assert_eq!(
        write_as(MEMBER_UID, &target, 0, b"X").err(),
        Some(Errno::ReadOnly)
    );
    let outsider = plain(OUTSIDER_UID, OUTSIDER_UID);
    let second = mounted(attached(
        facility,
        &outsider,
        source(OUTSIDER_UID, path, OpenFlags::READ, session(), 0x1A00_0006),
        &private(true),
    ));
    assert_ne!(second.name(), first.name(), "two volumes of one label");
    // Rock Ridge records its own owners; each attacher is shown the disc as
    // theirs whatever it records.
    for (uid, volume) in [(MEMBER_UID, &first), (OUTSIDER_UID, &second)] {
        let path = std::format!("/Storage/{}/README.TXT", volume.name());
        let stat = Descriptor::open(uid, &path, OpenFlags::READ)
            .and_then(|file| file.stat())
            .expect("stats");
        assert_eq!(stat.uid, uid, "{path}: presented as its attacher's");
    }
    eject(&second, DetachMode::Plain, &outsider).expect("ejects");
    eject(&first, DetachMode::Plain, &member).expect("ejects");
    retire(host_id, DetachMode::Plain, round);
}

/// An Acorn floppy image is told from an Amiga one by the probe alone, and
/// attaches writable as ADFS.
fn an_acorn_floppy_image_attaches_as_adfs_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 4, b"imgs4");
    let path = "/Storage/imgs4/Elite.adf";
    let acorn = Adfs::format(RamBlock::new(1600), AdfsVariant::E).expect("formats");
    put(0, path, &acorn.into_device().data);
    let member = plain(MEMBER_UID, STORAGE_GID);
    let volume = mounted(attached(
        facility,
        &member,
        source(MEMBER_UID, path, read_write(), session(), 0x1A00_0007),
        &private(false),
    ));
    assert_eq!(mount_record(b"Elite.adf").fstype_bytes(), b"adfs");
    let file = std::format!("/Storage/{}/ReadMe", volume.name());
    put(MEMBER_UID, &file, b"filecore");
    let mut buf = [0u8; 16];
    let read = read_as(MEMBER_UID, &file, 0, &mut buf).expect("reads");
    assert_eq!(&buf[..read], b"filecore");
    eject(&volume, DetachMode::Plain, &member).expect("ejects");
    retire(host_id, DetachMode::Plain, round);
}

/// An image volume ends with its attacher's login session, forced, and with
/// no other.
fn an_image_leaves_with_its_session_scenario(window_ptr: *mut u8, facility: &'static TestFacility) {
    let (host_id, round) = host(window_ptr, facility, 5, b"imgs5");
    let path = "/Storage/imgs5/Session.adf";
    put(0, path, &amiga_floppy(b"Session", b"logout"));
    let login = ProcId::from_raw([0x5E; 16]);
    let volume = mounted(attached(
        facility,
        &plain(MEMBER_UID, STORAGE_GID),
        source(MEMBER_UID, path, read_write(), login, 0x1A00_0008),
        &private(false),
    ));
    VOLUME_SERVICE.session_ended(&|ended| ended == session());
    assert!(!VOLUME_SERVICE.owes(), "another session ended");
    VOLUME_SERVICE.session_ended(&|ended| ended == login);
    assert!(
        shown(volume.name()) && VOLUME_SERVICE.owes(),
        "the telling only owes the detach, so no flush holds up a session's end"
    );
    settled();
    assert!(!shown(volume.name()), "its own session ended");
    assert_eq!(
        audited(IMAGE_VOLUME_DETACHED, volume.name()),
        [Some(String::from("session_ended"))]
    );
    retire(host_id, DetachMode::Plain, round);
}

/// The volume an image's file lies on is not plainly detached while the
/// image is attached; a force detaches the image first.
fn an_image_is_settled_before_its_backing_volume_detaches_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 6, b"imgs6");
    let path = "/Storage/imgs6/Held.adf";
    put(0, path, &amiga_floppy(b"Held", b"held"));
    let volume = mounted(attached(
        facility,
        &plain(MEMBER_UID, STORAGE_GID),
        source(MEMBER_UID, path, read_write(), session(), 0x1A00_0009),
        &private(false),
    ));
    let detach = |mode| {
        VOLUME_SERVICE.detach(
            &VolumeDetachRequest {
                volume_id: host_id,
                mode,
            },
            &admin(),
        )
    };
    assert_eq!(detach(DetachMode::Plain), Err(Errno::Busy));
    assert_eq!(
        audited(DETACH_REFUSED, "imgs6"),
        [Some(String::from("image_attached"))]
    );
    assert!(shown(volume.name()), "the refusal changed nothing");
    detach(DetachMode::Force).expect("a force detaches the image first");
    assert!(!shown(volume.name()) && !shown("imgs6"));
    assert_eq!(
        audited(IMAGE_VOLUME_DETACHED, volume.name()),
        [Some(String::from("backing_detached"))]
    );
    round.stop.store(true, Ordering::Relaxed);
    round.server.join().expect("server thread");
}

/// The volume an image's file lies on being pulled retires the image with it.
fn an_image_leaves_with_its_pulled_backing_volume_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (_, round) = host(window_ptr, facility, 7, b"imgs7");
    let path = "/Storage/imgs7/Pulled.adf";
    put(0, path, &amiga_floppy(b"Pulled", b"pulled"));
    let volume = mounted(attached(
        facility,
        &plain(MEMBER_UID, STORAGE_GID),
        source(MEMBER_UID, path, OpenFlags::READ, session(), 0x1A00_000A),
        &private(true),
    ));
    tairix_kernel_core::callreg::teardown_owned_by(host_owner(7), &ASPACES, &SINK);
    round.stop.store(true, Ordering::Relaxed);
    round.server.join().expect("server thread");
    assert!(!shown(volume.name()), "retired with its backing");
    assert_eq!(
        audited(IMAGE_VOLUME_RETIRED, volume.name()),
        [Some(String::from("backing_gone"))]
    );
    assert_eq!(
        eject(&volume, DetachMode::Plain, &admin()),
        Err(Errno::NotFound)
    );
}

/// An image whose file lies on another image's volume attaches, and holds
/// that volume attached as a file on a device volume does.
fn an_image_within_an_image_is_held_and_settled_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 8, b"imgs8");
    let options = FormatOptions {
        label: "Outer",
        revision: 0x0201,
        serial: [8; 8],
        now: tairix_abi::time::Time64::from_secs(1_700_000_000),
    };
    let udf = Udf::format(RamBlock::new(SECTORS_8MIB), &options).expect("formats");
    put(0, "/Storage/imgs8/outer.udf", &udf.into_device().data);
    let member = plain(MEMBER_UID, STORAGE_GID);
    let outer = mounted(attached(
        facility,
        &member,
        source(
            MEMBER_UID,
            "/Storage/imgs8/outer.udf",
            read_write(),
            session(),
            0x1A00_000B,
        ),
        &private(false),
    ));
    let inner_path = std::format!("/Storage/{}/inner.adf", outer.name());
    put(
        MEMBER_UID,
        &inner_path,
        &amiga_floppy(b"Inner", b"nested payload"),
    );
    let inner = mounted(attached(
        facility,
        &member,
        source(
            MEMBER_UID,
            &inner_path,
            read_write(),
            session(),
            0x1A00_000C,
        ),
        &private(false),
    ));
    let mut buf = [0u8; 32];
    let read = read_as(MEMBER_UID, "/Storage/inner/ReadMe", 0, &mut buf).expect("reads");
    assert_eq!(&buf[..read], b"nested payload");
    assert_eq!(eject(&outer, DetachMode::Plain, &member), Err(Errno::Busy));
    assert_eq!(
        audited(IMAGE_DETACH_REFUSED, outer.name()),
        [Some(String::from("image_attached"))]
    );
    eject(&outer, DetachMode::Force, &member).expect("a force ejects the inner image first");
    assert!(!shown(inner.name()) && !shown(outer.name()));
    assert_eq!(
        audited(IMAGE_VOLUME_DETACHED, inner.name()),
        [Some(String::from("backing_detached"))]
    );
    retire(host_id, DetachMode::Plain, round);
}

/// An image volume whose driver dies stays mounted behind the stand-in,
/// recovering, until the driver `devmgr` restarts serves it again.
fn an_image_whose_driver_is_lost_is_served_again_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 9, b"imgs9");
    let path = "/Storage/imgs9/Restart.adf";
    put(0, path, &amiga_floppy(b"Restart", b"served again"));
    let member = plain(MEMBER_UID, STORAGE_GID);
    let volume = mounted(attached(
        facility,
        &member,
        source(MEMBER_UID, path, read_write(), session(), 0x1A00_000D),
        &private(false),
    ));
    let (node, driver) = *DRIVEN.lock().unwrap().last().expect("a driver serves it");
    tairix_kernel_core::callreg::teardown_owned_by(driver, &ASPACES, &SINK);
    let file = std::format!("/Storage/{}/ReadMe", volume.name());
    let mut buf = [0u8; 32];
    assert_eq!(
        read_as(MEMBER_UID, &file, 0, &mut buf).err(),
        Some(Errno::DeviceFault),
        "the stand-in answers"
    );
    assert_eq!(
        mount_record(b"Restart.adf").availability(),
        MountAvailability::Recovering
    );
    let served = TREE
        .node(node)
        .and_then(|node| node.resources().iter().find_map(HwResource::volume_id))
        .expect("its node stays published");
    start_driver(facility, node, served, VolumeFsType::Affs);
    assert_eq!(
        mount_record(b"Restart.adf").availability(),
        MountAvailability::Available
    );
    let read = read_as(MEMBER_UID, &file, 0, &mut buf).expect("served again");
    assert_eq!(&buf[..read], b"served again");
    eject(&volume, DetachMode::Plain, &member).expect("ejects");
    retire(host_id, DetachMode::Plain, round);
}

/// An image volume whose driver died has nothing left to flush, so a plain
/// eject takes it out, as a stick whose driver went is detached.
fn an_image_whose_driver_is_lost_ejects_without_a_force_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 31, b"imgs31");
    let path = "/Storage/imgs31/Orphan.adf";
    put(0, path, &amiga_floppy(b"Orphan", b"x"));
    let member = plain(MEMBER_UID, STORAGE_GID);
    let volume = mounted(attached(
        facility,
        &member,
        source(MEMBER_UID, path, read_write(), session(), 0x1A00_0170),
        &private(false),
    ));
    let (_, driver) = *DRIVEN.lock().unwrap().last().expect("a driver serves it");
    tairix_kernel_core::callreg::teardown_owned_by(driver, &ASPACES, &SINK);
    assert_eq!(
        mount_record(b"Orphan.adf").availability(),
        MountAvailability::Recovering
    );
    eject(&volume, DetachMode::Plain, &member).expect("a plain eject takes it out");
    assert!(!shown(volume.name()));
    assert_eq!(
        audited(IMAGE_VOLUME_DETACHED, volume.name()),
        [Some(String::from("ejected"))]
    );
    assert!(
        audited(IMAGE_DETACH_REFUSED, volume.name()).is_empty(),
        "never refused for a flush the stand-in cannot make"
    );
    retire(host_id, DetachMode::Plain, round);
}

/// A detach that takes the lost is a plain one of a healthy volume: it
/// revokes no descriptor and flushes first, so a stale view of a lost volume
/// whose identity a healthy one has taken since cannot force it. The floppy's
/// label and file are this scenario's alone.
fn a_detach_taking_the_lost_forces_no_healthy_volume_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 32, b"imgs32");
    let path = "/Storage/imgs32/Healthy.adf";
    put(0, path, &amiga_floppy(b"Healthy", b"kept"));
    let member = plain(MEMBER_UID, STORAGE_GID);
    let volume = mounted(attached(
        facility,
        &member,
        source(MEMBER_UID, path, read_write(), session(), 0x1A00_0180),
        &private(false),
    ));
    let file = std::format!("/Storage/{}/ReadMe", volume.name());
    let held = Descriptor::open(MEMBER_UID, &file, OpenFlags::READ).expect("opens");
    assert_eq!(eject(&volume, DetachMode::Lost, &member), Err(Errno::Busy));
    assert_eq!(
        audited(IMAGE_DETACH_REFUSED, volume.name()),
        [Some(String::from("nodes_open"))]
    );
    let mut buf = [0u8; 8];
    let read = held
        .read(0, &mut buf)
        .expect("the descriptor was not revoked");
    assert_eq!(&buf[..read], b"kept");
    drop(held);
    eject(&volume, DetachMode::Lost, &member).expect("then a plain eject takes it out");
    assert!(!shown(volume.name()));
    assert_eq!(
        audited_text(IMAGE_VOLUME_DETACHED, volume.name(), "outcome"),
        [Some(String::from("clean"))]
    );
    retire(host_id, DetachMode::Plain, round);
}

/// A principal holds at most a bounded number of image volumes, judged as
/// each enters, so two attaches racing for the last place cannot both take
/// it; another principal's are its own, and ejecting one frees its place.
fn an_image_count_is_bounded_per_principal_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 10, b"imgs10");
    let file = |at: usize| std::format!("/Storage/imgs10/disk{at}.hdf");
    for at in 0..=IMAGE_VOLUMES_PER_PRINCIPAL + 1 {
        put(
            0,
            &file(at),
            &amiga_volume(64, std::format!("Disk{at}").as_bytes(), b"x"),
        );
    }
    let member = plain(MEMBER_UID, STORAGE_GID);
    let open = |at| {
        source(
            MEMBER_UID,
            &file(at),
            OpenFlags::READ,
            session(),
            0x1A00_0010,
        )
    };
    let last = IMAGE_VOLUMES_PER_PRINCIPAL - 1;
    let mut held: Vec<AttachReply> = (0..last)
        .map(|at| mounted(attached(facility, &member, open(at), &private(true))))
        .collect();
    let raced = attached_together(
        facility,
        MEMBER_UID,
        STORAGE_GID,
        vec![file(last), file(last + 1)],
    );
    let (won, refused): (Vec<_>, Vec<_>) = raced.into_iter().partition(Result::is_ok);
    assert_eq!(
        refused,
        [Err(Errno::LimitExceeded)],
        "one place, one attach"
    );
    held.extend(won.into_iter().map(mounted));
    assert_eq!(
        attached(facility, &member, open(0), &private(true)),
        Ok(ImageAttachReply::AlreadyAttached(held[0])),
        "at the bound, a file attached already still answers its volume"
    );
    let over = IMAGE_VOLUMES_PER_PRINCIPAL + 1;
    assert_eq!(
        attached(facility, &member, open(over), &private(true)),
        Err(Errno::LimitExceeded)
    );
    assert_eq!(
        audited(IMAGE_ATTACH_REFUSED, &std::format!("disk{over}.hdf")),
        [Some(String::from("too_many_images"))]
    );
    let system = plain(0, 0);
    let theirs = mounted(attached(
        facility,
        &system,
        source(0, &file(over), OpenFlags::READ, session(), 0x1A00_0011),
        &private(true),
    ));
    eject(&theirs, DetachMode::Plain, &system).expect("ejects");
    let freed = held.pop().expect("a volume held");
    eject(&freed, DetachMode::Plain, &member).expect("ejects");
    held.push(mounted(attached(
        facility,
        &member,
        open(over),
        &private(true),
    )));
    for volume in &held {
        eject(volume, DetachMode::Plain, &member).expect("ejects");
    }
    retire(host_id, DetachMode::Plain, round);
}

/// An image volume within image volumes is bounded in depth: the file one
/// level too deep is refused before it is read.
fn an_image_nesting_is_bounded_scenario(window_ptr: *mut u8, facility: &'static TestFacility) {
    let (host_id, round) = host(window_ptr, facility, 11, b"imgs11");
    let member = plain(MEMBER_UID, STORAGE_GID);
    let mut dir = String::from("/Storage/imgs11");
    let mut nested: Vec<AttachReply> = Vec::new();
    // Each level holds the next, so each is a little larger than it.
    for depth in 1..=IMAGE_NESTING_MAX {
        let path = std::format!("{dir}/nest{depth}.hdf");
        let blocks = 128 * u64::from(IMAGE_NESTING_MAX + 1 - depth);
        let writer = if depth == 1 { 0 } else { MEMBER_UID };
        put(
            writer,
            &path,
            &amiga_volume(blocks, std::format!("Nest{depth}").as_bytes(), b"x"),
        );
        let task = 0x1A00_0020 + u64::from(depth);
        let volume = mounted(attached(
            facility,
            &member,
            source(MEMBER_UID, &path, read_write(), session(), task),
            &private(false),
        ));
        dir = std::format!("/Storage/{}", volume.name());
        nested.push(volume);
    }
    let deepest = std::format!("{dir}/deepest.hdf");
    put(MEMBER_UID, &deepest, &amiga_volume(16, b"Deepest", b"x"));
    assert_eq!(
        attached(
            facility,
            &member,
            source(MEMBER_UID, &deepest, read_write(), session(), 0x1A00_002F),
            &private(false),
        ),
        Err(Errno::LimitExceeded)
    );
    assert_eq!(
        audited(IMAGE_ATTACH_REFUSED, "deepest.hdf"),
        [Some(String::from("nested_too_deep"))]
    );
    eject(&nested[0], DetachMode::Force, &member)
        .expect("a force ejects every level within it first");
    assert!(nested.iter().all(|volume| !shown(volume.name())));
    retire(host_id, DetachMode::Plain, round);
}

/// A driver that cannot open the volume it was started for refuses it, and
/// the attach answers that reason; the volume's node goes, and its file is
/// free again.
fn a_drivers_refusal_reaches_the_attach_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 12, b"imgs12");
    let path = "/Storage/imgs12/Refused.adf";
    put(0, path, &amiga_floppy(b"Refused", b"x"));
    for (why, reason) in [
        (DriverError::BadMagic, ImageRefusal::Damaged),
        (DriverError::Unsupported, ImageRefusal::UnsupportedVariant),
    ] {
        let answer = with_devmgr(
            move |node, volume, _| refusing(node, volume, why),
            || {
                answered(
                    &plain(MEMBER_UID, STORAGE_GID),
                    source(MEMBER_UID, path, read_write(), session(), 0x1A00_0030),
                    &private(false),
                )
            },
        );
        assert_eq!(answer, Ok(ImageAttachReply::Refused(reason)), "{why:?}");
        let (node, _) = *DRIVEN.lock().unwrap().last().expect("refused by a driver");
        assert!(
            !TREE.is_live(node),
            "the refused volume's node is withdrawn"
        );
    }
    assert_eq!(
        audited(IMAGE_ATTACH_REFUSED, "Refused.adf"),
        [
            Some(String::from(ImageRefusal::Damaged.as_str())),
            Some(String::from(ImageRefusal::UnsupportedVariant.as_str()))
        ]
    );
    let member = plain(MEMBER_UID, STORAGE_GID);
    let volume = mounted(attached(
        facility,
        &member,
        source(MEMBER_UID, path, read_write(), session(), 0x1A00_0030),
        &private(false),
    ));
    eject(&volume, DetachMode::Plain, &member).expect("ejects");
    retire(host_id, DetachMode::Plain, round);
}

/// An attach of `second.adf`, opened by `open`, asking a target its caller
/// may not bind at is refused for it, and taken back before it answers: the
/// file attaches again at once.
fn a_refused_target_is_taken_back(
    facility: &'static TestFacility,
    member: &Principal<'_>,
    open: &dyn Fn() -> ImageSource,
) {
    assert_eq!(
        attached(
            facility,
            member,
            open(),
            &ImageAttach {
                target: Some("/System/second"),
                ..private(true)
            }
        ),
        Ok(ImageAttachReply::TargetRefused(Errno::PermissionDenied)),
        "the directory is what refused it, and says so"
    );
    assert_eq!(
        audited(IMAGE_ATTACH_REFUSED, "second.adf"),
        [Some(String::from("target_refused"))]
    );
    assert!(
        !shown("second") && !VOLUME_SERVICE.owes(),
        "a refused target takes the attach back before it answers"
    );
    assert_eq!(
        audited(IMAGE_VOLUME_DETACHED, "second"),
        [Some(String::from("abandoned"))]
    );
    let retried = mounted(attached(
        facility,
        member,
        open(),
        &ImageAttach {
            adopt: false,
            ..private(true)
        },
    ));
    eject(&retried, DetachMode::Plain, member).expect("ejects");
}

/// An attach asking a name takes it only while nothing holds it; one asking
/// a target is shown there too, and is taken out again when it may not be.
fn an_image_takes_the_name_and_target_asked_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 13, b"imgs13");
    put(
        0,
        "/Storage/imgs13/first.adf",
        &amiga_floppy(b"First", b"first"),
    );
    put(
        0,
        "/Storage/imgs13/second.adf",
        &amiga_floppy(b"Second", b"second"),
    );
    let member = plain(MEMBER_UID, STORAGE_GID);
    let open = |file: &str, flags| {
        source(
            MEMBER_UID,
            &std::format!("/Storage/imgs13/{file}"),
            flags,
            session(),
            0x1A00_0040,
        )
    };
    let named = |name| ImageAttach {
        name: Some(name),
        ..private(false)
    };
    let first = mounted(attached(
        facility,
        &member,
        open("first.adf", read_write()),
        &named("chosen"),
    ));
    assert_eq!(first.name(), "chosen");
    for name in ["chosen", "imgs13"] {
        assert_eq!(
            attached(
                facility,
                &member,
                open("second.adf", OpenFlags::READ),
                &named(name)
            ),
            Ok(ImageAttachReply::NameRefused(Errno::AlreadyExists)),
            "{name} is held"
        );
        assert_eq!(
            audited(IMAGE_ATTACH_REFUSED, name),
            [Some(String::from("name_in_use"))]
        );
    }

    let targeted = |target| ImageAttach {
        target: Some(target),
        ..private(true)
    };
    a_refused_target_is_taken_back(facility, &member, &|| open("second.adf", OpenFlags::READ));

    FS_SERVICE
        .mkdir(MEMBER_UID, &NoCaps, root("/Storage/chosen/mnt"), new_dir())
        .expect("a directory of the member's own volume");
    let second = mounted(attached(
        facility,
        &member,
        open("second.adf", OpenFlags::READ),
        &targeted("/Storage/chosen/mnt"),
    ));
    let mut buf = [0u8; 16];
    let read = read_as(MEMBER_UID, "/Storage/chosen/mnt/ReadMe", 0, &mut buf).expect("bound");
    assert_eq!(&buf[..read], b"second");
    assert_eq!(
        eject(&first, DetachMode::Plain, &member),
        Err(Errno::NotEmpty),
        "a bind stands within it"
    );
    eject(&second, DetachMode::Plain, &member).expect("ejects, and its bind with it");
    eject(&first, DetachMode::Plain, &member).expect("ejects");
    retire(host_id, DetachMode::Plain, round);
}

/// An image volume one principal attached in two login sessions lives until
/// both have ended.
fn an_image_held_by_two_sessions_leaves_with_the_last_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 14, b"imgs14");
    let path = "/Storage/imgs14/Both.adf";
    put(0, path, &amiga_floppy(b"Both", b"x"));
    let member = plain(MEMBER_UID, STORAGE_GID);
    let (desk, shell) = (ProcId::from_raw([0xA1; 16]), ProcId::from_raw([0xB2; 16]));
    let open = |session, task| source(MEMBER_UID, path, OpenFlags::READ, session, task);
    let volume = mounted(attached(
        facility,
        &member,
        open(desk, 0x1A00_0050),
        &private(true),
    ));
    assert_eq!(
        attached(facility, &member, open(shell, 0x1A00_0051), &private(true)),
        Ok(ImageAttachReply::AlreadyAttached(volume))
    );
    VOLUME_SERVICE.session_ended(&|ended| ended == desk);
    settled();
    assert!(shown(volume.name()), "its other session holds it");
    VOLUME_SERVICE.session_ended(&|ended| ended == shell);
    settled();
    assert!(!shown(volume.name()), "both sessions ended");
    assert_eq!(
        audited(IMAGE_VOLUME_DETACHED, volume.name()),
        [Some(String::from("session_ended"))]
    );
    retire(host_id, DetachMode::Plain, round);
}

/// An image volume whose file's volume is pulled while its driver held
/// writes it had not published stays, lost, answering a device fault, until
/// its owner ejects it with a force.
fn an_image_holding_unpublished_writes_stays_lost_until_ejected_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (_, round) = host(window_ptr, facility, 15, b"imgs15");
    let path = "/Storage/imgs15/Lost.adf";
    put(0, path, &amiga_floppy(b"Lost", b"held"));
    let member = plain(MEMBER_UID, STORAGE_GID);
    let volume = mounted(attached(
        facility,
        &member,
        source(MEMBER_UID, path, read_write(), session(), 0x1A00_0060),
        &private(false),
    ));
    let (node, _) = *DRIVEN.lock().unwrap().last().expect("a driver serves it");
    hold_unpublished(node);
    let file = std::format!("/Storage/{}/ReadMe", volume.name());
    let mut buf = [0u8; 8];
    read_as(MEMBER_UID, &file, 0, &mut buf).expect("the reply carries the held writes");
    tairix_kernel_core::callreg::teardown_owned_by(host_owner(15), &ASPACES, &SINK);
    round.stop.store(true, Ordering::Relaxed);
    round.server.join().expect("server thread");
    assert!(
        shown(volume.name()),
        "a volume that lost writes stays visible"
    );
    assert_eq!(
        mount_record(b"Lost.adf").availability(),
        MountAvailability::UnavailableLost
    );
    assert_eq!(
        audited(IMAGE_VOLUME_RETIRED, volume.name()),
        [Some(String::from("backing_gone"))]
    );
    assert_eq!(
        read_as(MEMBER_UID, &file, 0, &mut buf).err(),
        Some(Errno::DeviceFault)
    );
    assert_eq!(
        eject(&volume, DetachMode::Plain, &member),
        Err(Errno::DeviceFault)
    );
    assert_eq!(
        audited(IMAGE_DETACH_REFUSED, volume.name()),
        [Some(String::from("volume_unavailable_lost"))]
    );
    eject(&volume, DetachMode::Lost, &member).expect("taking the lost ejects it");
    assert!(!shown(volume.name()));
    assert_eq!(
        audited_text(IMAGE_VOLUME_DETACHED, volume.name(), "outcome"),
        [Some(String::from("forced"))]
    );
}

/// An image volume whose driver is lost leaves when the driver started again
/// refuses it.
fn an_image_whose_restarted_driver_refuses_it_leaves_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 16, b"imgs16");
    let path = "/Storage/imgs16/Again.adf";
    put(0, path, &amiga_floppy(b"Again", b"x"));
    let member = plain(MEMBER_UID, STORAGE_GID);
    let volume = mounted(attached(
        facility,
        &member,
        source(MEMBER_UID, path, read_write(), session(), 0x1A00_0070),
        &private(false),
    ));
    let (node, driver) = *DRIVEN.lock().unwrap().last().expect("a driver serves it");
    let inner_path = std::format!("/Storage/{}/inner.hdf", volume.name());
    put(MEMBER_UID, &inner_path, &amiga_volume(64, b"Within", b"x"));
    let inner = mounted(attached(
        facility,
        &member,
        source(
            MEMBER_UID,
            &inner_path,
            OpenFlags::READ,
            session(),
            0x1A00_0071,
        ),
        &private(true),
    ));
    tairix_kernel_core::callreg::teardown_owned_by(driver, &ASPACES, &SINK);
    assert_eq!(
        mount_record(b"Again.adf").availability(),
        MountAvailability::Recovering
    );
    assert!(
        !shown(inner.name()),
        "an image within it goes as its file does"
    );
    assert_eq!(
        audited(IMAGE_VOLUME_RETIRED, inner.name()),
        [Some(String::from("backing_gone"))]
    );
    let served = TREE
        .node(node)
        .and_then(|node| node.resources().iter().find_map(HwResource::volume_id))
        .expect("its node stays published");
    refusing(node, served, DriverError::BadMagic);
    assert!(!shown(volume.name()), "it leaves as its driver did");
    assert_eq!(
        audited(IMAGE_ATTACH_REFUSED, volume.name()),
        [Some(String::from("driver_refused"))]
    );
    assert_eq!(
        audited(IMAGE_VOLUME_RETIRED, volume.name()),
        [
            Some(String::from("server_lost")),
            Some(String::from("driver_refused"))
        ]
    );
    assert_eq!(
        eject(&volume, DetachMode::Plain, &member),
        Err(Errno::NotFound)
    );
    retire(host_id, DetachMode::Plain, round);
}

/// An image volume whose file lies on a network share leaves when the
/// share's server withdraws it.
fn an_image_on_a_withdrawn_share_leaves_with_it_scenario(facility: &'static TestFacility) {
    let mut server = NetServer::offer(
        facility,
        *b"scenario-key-img",
        MEMBER_UID,
        "//nas/images",
        share_fs(),
    );
    VOLUME_SERVICE
        .attach_offered(&member(), server.offer, &private_attach("images", None))
        .expect("attached");
    let path = "/Storage/images/OnShare.adf";
    put(MEMBER_UID, path, &amiga_floppy(b"OnShare", b"x"));
    let volume = mounted(attached(
        facility,
        &member(),
        source(MEMBER_UID, path, OpenFlags::READ, session(), 0x1A00_0080),
        &private(true),
    ));
    assert_eq!(
        VOLUME_SERVICE.offer_ctl(server.owner, server.offer, FsserveCtl::Withdraw),
        Ok(())
    );
    server.join();
    assert!(
        !shown(volume.name()),
        "retired with the share its file lies on"
    );
    assert_eq!(
        audited(IMAGE_VOLUME_RETIRED, volume.name()),
        [Some(String::from("backing_gone"))]
    );
}

/// A share's detach settles the image volumes whose files lie on it first: a
/// plain one is refused while one is attached, and so is a forced one that
/// cannot detach it, both leaving the image as it was; a forced one that can
/// detaches it, flushed.
fn an_image_is_settled_before_its_backing_share_detaches_scenario(facility: &'static TestFacility) {
    let mut server = NetServer::offer(
        facility,
        *b"scenario-key-hld",
        MEMBER_UID,
        "//nas/heldshare",
        share_fs(),
    );
    let share = VOLUME_SERVICE
        .attach_offered(&member(), server.offer, &private_attach("heldshare", None))
        .expect("attached");
    let path = "/Storage/heldshare/Anchored.adf";
    put(MEMBER_UID, path, &amiga_floppy(b"Anchored", b"anchored"));
    let image = mounted(attached(
        facility,
        &member(),
        source(MEMBER_UID, path, OpenFlags::READ, session(), 0x1A00_0150),
        &private(true),
    ));
    let still_there = || {
        let mut buf = [0u8; 16];
        let read = read_as(MEMBER_UID, "/Storage/anchored/ReadMe", 0, &mut buf);
        assert_eq!(read, Ok(8), "the image reads on");
        assert_eq!(&buf[..8], b"anchored");
        assert!(shown("heldshare") && shown(image.name()));
        assert!(audited(IMAGE_VOLUME_RETIRED, image.name()).is_empty());
    };
    assert_eq!(
        eject(&share, DetachMode::Plain, &member()),
        Err(Errno::Busy)
    );
    still_there();
    VOLUME_SERVICE.while_image_held(&image.volume_id, || {
        assert_eq!(
            eject(&share, DetachMode::Force, &member()),
            Err(Errno::Busy)
        );
    });
    still_there();
    assert_eq!(
        audited(NETWORK_DETACH_REFUSED, "heldshare"),
        [
            Some(String::from("image_attached")),
            Some(String::from("image_detach_refused"))
        ]
    );
    assert_eq!(eject(&share, DetachMode::Force, &member()), Ok(()));
    assert!(!shown("heldshare") && !shown(image.name()));
    assert_eq!(
        audited(IMAGE_VOLUME_DETACHED, image.name()),
        [Some(String::from("backing_detached"))]
    );
    server.join();
}

/// A share its server withdraws while an operation holds it is retired as
/// that operation lets go, and the image volumes on it with it.
fn an_image_on_a_share_withdrawn_while_held_leaves_with_it_scenario(
    facility: &'static TestFacility,
) {
    let mut server = NetServer::offer(
        facility,
        *b"scenario-key-whl",
        MEMBER_UID,
        "//nas/whileheld",
        share_fs(),
    );
    let share = VOLUME_SERVICE
        .attach_offered(&member(), server.offer, &private_attach("whileheld", None))
        .expect("attached");
    let path = "/Storage/whileheld/Stranded.adf";
    put(MEMBER_UID, path, &amiga_floppy(b"Stranded", b"x"));
    let image = mounted(attached(
        facility,
        &member(),
        source(MEMBER_UID, path, OpenFlags::READ, session(), 0x1A00_0151),
        &private(true),
    ));
    VOLUME_SERVICE.while_share_held(&share.volume_id, || {
        assert_eq!(
            VOLUME_SERVICE.offer_ctl(server.owner, server.offer, FsserveCtl::Withdraw),
            Ok(())
        );
        assert!(
            shown("whileheld") && shown(image.name()),
            "its holder retires it"
        );
    });
    server.join();
    assert_eq!(
        audited(NETWORK_VOLUME_RETIRED, "whileheld"),
        [Some(String::from("retired_while_held"))]
    );
    assert!(
        !shown(image.name()),
        "retired with the share its file lies on"
    );
    assert_eq!(
        audited(IMAGE_VOLUME_RETIRED, image.name()),
        [Some(String::from("backing_gone"))]
    );
}

/// A share whose bind target is refused leaves again, and an image volume
/// attached on it while the bind was tried is detached with it, flushed.
fn an_image_on_a_share_whose_target_is_refused_leaves_with_it_scenario(
    facility: &'static TestFacility,
) {
    let mut outer_fs = share_fs();
    let root = outer_fs.root();
    let full = outer_fs
        .create(
            root,
            b"full",
            NodeKind::Directory,
            &NodeSecurity::new(0o755, 0, 0),
        )
        .expect("a directory");
    let inside = outer_fs
        .create(
            full,
            b"inside",
            NodeKind::RegularFile,
            &NodeSecurity::new(0o644, 0, 0),
        )
        .expect("a file in it");
    outer_fs.forget_ownership(full);
    outer_fs.forget_ownership(inside);
    let (arrived, arrivals) = mpsc::channel();
    let gate = Gate::new(arrived.clone());
    let mut outer = NetServer::offer_with(
        facility,
        *b"scenario-key-gat",
        MEMBER_UID,
        "//nas/gateouter",
        outer_fs,
        Some(StdArc::clone(&gate)),
    );
    let outer_share = VOLUME_SERVICE
        .attach_offered(&member(), outer.offer, &private_attach("gateouter", None))
        .expect("attached");
    let mut server = NetServer::offer(
        facility,
        *b"scenario-key-gto",
        MEMBER_UID,
        "//nas/gated",
        share_fs(),
    );
    gate.shut();
    let offer = server.offer;
    let binding = thread::spawn(move || {
        let answer = VOLUME_SERVICE.attach_offered(
            &member(),
            offer,
            &private_attach("gated", Some("/Storage/gateouter/full")),
        );
        let _ = arrived.send(Arrived::Returned(answer));
    });
    assert!(
        matches!(arrivals.recv(), Ok(Arrived::Call)),
        "the bind reached the target's server"
    );
    let path = "/Storage/gated/Latecomer.adf";
    put(MEMBER_UID, path, &amiga_floppy(b"Latecomer", b"x"));
    let image = mounted(attached(
        facility,
        &member(),
        source(MEMBER_UID, path, OpenFlags::READ, session(), 0x1A00_0152),
        &private(true),
    ));
    gate.open();
    let Ok(Arrived::Returned(answer)) = arrivals.recv() else {
        std::panic!("the attach returned");
    };
    binding.join().expect("the attaching thread");
    assert_eq!(answer.err(), Some(Errno::NotEmpty));
    assert_eq!(
        audited(NETWORK_ATTACH_REFUSED, "gated"),
        [Some(String::from("target_refused"))]
    );
    assert!(!shown("gated") && !shown(image.name()));
    assert_eq!(
        audited(IMAGE_VOLUME_DETACHED, image.name()),
        [Some(String::from("backing_detached"))]
    );
    assert!(audited(IMAGE_VOLUME_RETIRED, image.name()).is_empty());
    let share = VOLUME_SERVICE
        .attach_offered(&member(), offer, &private_attach("gated", None))
        .expect("the offer attaches again");
    assert_eq!(eject(&share, DetachMode::Plain, &member()), Ok(()));
    assert_eq!(eject(&outer_share, DetachMode::Plain, &member()), Ok(()));
    server.join();
    outer.join();
}

/// An attach whose caller is killed while it waits — before its driver
/// serves, or while that serve holds the volume — takes the volume out with
/// it, and the file is free again.
fn a_killed_attach_takes_its_volume_out_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 17, b"imgs17");
    let path = "/Storage/imgs17/Killed.adf";
    put(0, path, &amiga_floppy(b"Killed", b"x"));
    let task = 0x1A00_0090;
    procsignal::install_gate(task, ProcessId(task)).expect("the attacher's gate");
    let answer = with_devmgr(
        move |node, volume, fstype| {
            let _ = procsignal::claim_group_kill(
                None,
                DeferredTeardown::Plain {
                    process: ProcessId(task),
                },
                None,
            );
            start_driver(facility, node, volume, fstype);
        },
        || {
            answered(
                &plain(MEMBER_UID, STORAGE_GID),
                source(MEMBER_UID, path, read_write(), session(), task),
                &private(false),
            )
        },
    );
    procsignal::clear_kill_gate(task);
    assert_eq!(answer, Err(Errno::Interrupted));
    assert_eq!(
        audited(IMAGE_ATTACH_REFUSED, "Killed.adf"),
        [Some(String::from("interrupted"))]
    );
    settled();
    assert!(!shown("killed"), "nothing is left mounted");
    let member = plain(MEMBER_UID, STORAGE_GID);
    let again = mounted(attached(
        facility,
        &member,
        source(MEMBER_UID, path, read_write(), session(), 0x1A00_0091),
        &private(false),
    ));
    eject(&again, DetachMode::Plain, &member).expect("ejects");
    retire(host_id, DetachMode::Plain, round);
}

/// An attach asking for a writable volume gets one or none: a descriptor or
/// a format that will not take writes refuses it, and nothing stays mounted.
fn an_image_asked_writable_is_writable_or_refused_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 18, b"imgs18");
    put(
        0,
        "/Storage/imgs18/Floppy.adf",
        &amiga_floppy(b"Floppy", b"x"),
    );
    put(
        0,
        "/Storage/imgs18/disc.iso",
        &ext_fixture::sparse(ISO_FIXTURE),
    );
    let member = plain(MEMBER_UID, STORAGE_GID);
    let writable = ImageAttach {
        access: ImageAccess::Writable,
        ..private(false)
    };
    let open = |file: &str, flags| {
        source(
            MEMBER_UID,
            &std::format!("/Storage/imgs18/{file}"),
            flags,
            session(),
            0x1A00_00A0,
        )
    };
    assert_eq!(
        attached(
            facility,
            &member,
            open("Floppy.adf", OpenFlags::READ),
            &writable
        ),
        Err(Errno::ReadOnly),
        "a descriptor that cannot write"
    );
    assert_eq!(
        attached(facility, &member, open("disc.iso", read_write()), &writable),
        Err(Errno::ReadOnly),
        "a format that cannot be written"
    );
    assert_eq!(
        audited(IMAGE_ATTACH_REFUSED, "Floppy.adf"),
        [Some(String::from("write_protected"))]
    );
    assert_eq!(
        audited(IMAGE_ATTACH_REFUSED, "disc.iso"),
        [Some(String::from("write_protected"))]
    );
    let mounted_from = |file: &[u8]| {
        FS_SERVICE
            .mount_snapshot()
            .expect("snapshot")
            .iter()
            .any(|record| record.source_bytes() == file)
    };
    assert!(
        !mounted_from(b"Floppy.adf") && !mounted_from(b"disc.iso"),
        "nothing stays mounted"
    );
    let volume = mounted(attached(
        facility,
        &member,
        open("Floppy.adf", read_write()),
        &writable,
    ));
    assert!(!volume.read_only());
    eject(&volume, DetachMode::Plain, &member).expect("ejects");
    retire(host_id, DetachMode::Plain, round);
}

/// An attach whose answer cannot be delivered leaves nothing it would not
/// have left had it been refused: a volume it attached is taken out, and the
/// hold it gave a session on one attached already is let go, while a hold
/// that session had before stays.
fn an_undelivered_answer_leaves_nothing_behind_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 19, b"imgs19");
    let path = "/Storage/imgs19/Undelivered.adf";
    put(0, path, &amiga_floppy(b"Undelivered", b"x"));
    let member = plain(MEMBER_UID, STORAGE_GID);
    let (desk, shell) = (ProcId::from_raw([0xC3; 16]), ProcId::from_raw([0xD4; 16]));
    let open = |session, task| source(MEMBER_UID, path, OpenFlags::READ, session, task);
    let undelivered = |from| {
        with_devmgr(driving(facility), || {
            VOLUME_SERVICE.attach_image(&member, from, &private(true), &mut |_| {
                Err(Errno::BadAddress)
            })
        })
    };
    assert_eq!(undelivered(open(desk, 0x1A00_00B0)), Err(Errno::BadAddress));
    assert!(
        !shown("undelivered") && !VOLUME_SERVICE.owes(),
        "a volume no one was told of is taken out before the call returns"
    );
    assert_eq!(
        audited(IMAGE_VOLUME_DETACHED, "undelivered"),
        [Some(String::from("abandoned"))]
    );

    let volume = mounted(attached(
        facility,
        &member,
        open(desk, 0x1A00_00B1),
        &private(true),
    ));
    assert_eq!(
        undelivered(open(shell, 0x1A00_00B2)),
        Err(Errno::BadAddress)
    );
    assert_eq!(undelivered(open(desk, 0x1A00_00B3)), Err(Errno::BadAddress));
    settled();
    assert!(shown(volume.name()), "the hold its first answer gave stays");
    assert_eq!(
        audited(IMAGE_ATTACH_REFUSED, "Undelivered.adf"),
        vec![Some(String::from("answer_undelivered")); 3]
    );
    VOLUME_SERVICE.session_ended(&|ended| ended == desk);
    settled();
    assert!(
        !shown(volume.name()),
        "the session never told of it holds nothing"
    );
    retire(host_id, DetachMode::Plain, round);
}

/// An image volume its session's end left owed its detach is taken up by
/// another of its owner's sessions before the detach is done, and lives on
/// with that session.
fn an_owed_image_taken_up_again_lives_on_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 20, b"imgs20");
    let path = "/Storage/imgs20/Again.adf";
    put(0, path, &amiga_floppy(b"Again", b"x"));
    let member = plain(MEMBER_UID, STORAGE_GID);
    let (first, second) = (ProcId::from_raw([0xE5; 16]), ProcId::from_raw([0xF6; 16]));
    let open = |session, task| source(MEMBER_UID, path, OpenFlags::READ, session, task);
    let volume = mounted(attached(
        facility,
        &member,
        open(first, 0x1A00_00C0),
        &private(true),
    ));
    VOLUME_SERVICE.session_ended(&|ended| ended == first);
    assert!(VOLUME_SERVICE.owes(), "its session ended");
    assert_eq!(
        attached(facility, &member, open(second, 0x1A00_00C1), &private(true)),
        Ok(ImageAttachReply::AlreadyAttached(volume))
    );
    settled();
    assert!(
        shown(volume.name()),
        "taken up again before its detach was done"
    );
    VOLUME_SERVICE.session_ended(&|ended| ended == second);
    settled();
    assert!(!shown(volume.name()), "its last session ended");
    retire(host_id, DetachMode::Plain, round);
}

/// A name the naming policy gives volumes it names itself, a reserved word,
/// and one no label is spelled as are never taken by choice, so no attach
/// can squat another volume's name.
fn a_name_the_naming_policy_keeps_is_never_taken_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 21, b"imgs21");
    let path = "/Storage/imgs21/Squat.adf";
    put(0, path, &amiga_floppy(b"Squat", b"x"));
    let member = plain(MEMBER_UID, STORAGE_GID);
    for name in ["backup_7f3a", "system", "Backup"] {
        let named = ImageAttach {
            name: Some(name),
            ..private(true)
        };
        assert_eq!(
            attached(
                facility,
                &member,
                source(MEMBER_UID, path, OpenFlags::READ, session(), 0x1A00_00D0),
                &named
            ),
            Ok(ImageAttachReply::NameRefused(Errno::OutOfRange)),
            "{name} is not a chosen name"
        );
        assert_eq!(
            audited(IMAGE_ATTACH_REFUSED, name),
            [Some(String::from("name_reserved"))]
        );
        assert!(!shown(name), "nothing is mounted as {name}");
    }
    retire(host_id, DetachMode::Plain, round);
}

/// The volume an image within an image lies on is pulled: the outer image
/// retires with it and the inner one with the outer, in one pass.
fn an_image_within_an_image_leaves_with_their_pulled_volume_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (_, round) = host(window_ptr, facility, 22, b"imgs22");
    let options = FormatOptions {
        label: "Carrier",
        revision: 0x0201,
        serial: [22; 8],
        now: tairix_abi::time::Time64::from_secs(1_700_000_000),
    };
    let udf = Udf::format(RamBlock::new(SECTORS_8MIB), &options).expect("formats");
    put(0, "/Storage/imgs22/carrier.udf", &udf.into_device().data);
    let member = plain(MEMBER_UID, STORAGE_GID);
    let outer = mounted(attached(
        facility,
        &member,
        source(
            MEMBER_UID,
            "/Storage/imgs22/carrier.udf",
            read_write(),
            session(),
            0x1A00_00E0,
        ),
        &private(false),
    ));
    let inner_path = std::format!("/Storage/{}/carried.adf", outer.name());
    put(MEMBER_UID, &inner_path, &amiga_floppy(b"Carried", b"x"));
    let inner = mounted(attached(
        facility,
        &member,
        source(
            MEMBER_UID,
            &inner_path,
            OpenFlags::READ,
            session(),
            0x1A00_00E1,
        ),
        &private(true),
    ));
    tairix_kernel_core::callreg::teardown_owned_by(host_owner(22), &ASPACES, &SINK);
    round.stop.store(true, Ordering::Relaxed);
    round.server.join().expect("server thread");
    for volume in [&outer, &inner] {
        assert!(!shown(volume.name()), "{} retired", volume.name());
        assert_eq!(
            audited(IMAGE_VOLUME_RETIRED, volume.name()),
            [Some(String::from("backing_gone"))]
        );
    }
}

/// An attach that does not ask to take its volume up is answered with the
/// volume there is and changes nothing: it lives no longer for that session.
fn an_attach_that_does_not_adopt_leaves_its_volume_as_it_is_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 23, b"imgs23");
    let path = "/Storage/imgs23/Kept.adf";
    put(0, path, &amiga_floppy(b"Kept", b"x"));
    let member = plain(MEMBER_UID, STORAGE_GID);
    let (desk, shell) = (ProcId::from_raw([0xA7; 16]), ProcId::from_raw([0xB8; 16]));
    let open = |session, task| source(MEMBER_UID, path, OpenFlags::READ, session, task);
    let volume = mounted(attached(
        facility,
        &member,
        open(desk, 0x1A00_00F0),
        &private(true),
    ));
    let asked = ImageAttach {
        adopt: false,
        ..private(true)
    };
    assert_eq!(
        attached(facility, &member, open(shell, 0x1A00_00F1), &asked),
        Ok(ImageAttachReply::AlreadyAttached(volume))
    );
    VOLUME_SERVICE.session_ended(&|ended| ended == desk);
    assert_eq!(
        attached(facility, &member, open(shell, 0x1A00_00F2), &asked),
        Err(Errno::Busy),
        "one owing its teardown is leaving, never answered as there"
    );
    assert_eq!(
        audited(IMAGE_ATTACH_REFUSED, "Kept.adf"),
        [Some(String::from("volume_leaving"))]
    );
    settled();
    assert!(
        !shown(volume.name()),
        "the session that only asked holds nothing"
    );
    retire(host_id, DetachMode::Plain, round);
}

/// An adopting attach takes up only a volume as writable as it asks: a
/// read-only one meeting a writable volume holds it for no session, and is
/// leaving to it like one that does not adopt; one that may write takes up a
/// read-only volume too. Each part's floppy has a label and file of its own.
fn a_read_only_attach_takes_up_no_writable_volume_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 33, b"imgs33");
    let member = plain(MEMBER_UID, STORAGE_GID);
    let session = |byte| ProcId::from_raw([byte; 16]);
    let open = |path: &str, flags, at, task| source(MEMBER_UID, path, flags, session(at), task);
    let ends = |at| VOLUME_SERVICE.session_ended(&|ended| ended == session(at));

    let path = "/Storage/imgs33/Writable.adf";
    put(0, path, &amiga_floppy(b"Writable", b"x"));
    let writable = mounted(attached(
        facility,
        &member,
        open(path, read_write(), 0xC1, 0x1A00_0190),
        &private(false),
    ));
    assert!(!writable.read_only());
    assert_eq!(
        attached(
            facility,
            &member,
            open(path, OpenFlags::READ, 0xC2, 0x1A00_0191),
            &private(true)
        ),
        Ok(ImageAttachReply::AlreadyAttached(writable))
    );
    ends(0xC1);
    settled();
    assert!(
        !shown(writable.name()),
        "the read-only attach took up nothing"
    );

    let path = "/Storage/imgs33/Owing.adf";
    put(0, path, &amiga_floppy(b"Owing", b"x"));
    let owing = mounted(attached(
        facility,
        &member,
        open(path, read_write(), 0xC3, 0x1A00_0192),
        &private(false),
    ));
    ends(0xC3);
    assert_eq!(
        attached(
            facility,
            &member,
            open(path, OpenFlags::READ, 0xC4, 0x1A00_0193),
            &private(true)
        ),
        Err(Errno::Busy),
        "leaving to a read-only attach, which never cancels its teardown"
    );
    assert_eq!(
        audited(IMAGE_ATTACH_REFUSED, "Owing.adf"),
        [Some(String::from("volume_leaving"))]
    );
    settled();
    assert!(!shown(owing.name()));

    let path = "/Storage/imgs33/Locked.adf";
    put(0, path, &amiga_floppy(b"Locked", b"x"));
    let locked = mounted(attached(
        facility,
        &member,
        open(path, OpenFlags::READ, 0xC5, 0x1A00_0194),
        &private(true),
    ));
    assert_eq!(
        attached(
            facility,
            &member,
            open(path, read_write(), 0xC6, 0x1A00_0195),
            &private(false)
        ),
        Ok(ImageAttachReply::AlreadyAttached(locked)),
        "one that may write takes up a read-only volume"
    );
    ends(0xC5);
    settled();
    assert!(shown(locked.name()), "held for the session that took it up");
    ends(0xC6);
    settled();
    assert!(!shown(locked.name()));
    retire(host_id, DetachMode::Plain, round);
}

/// An image whose file name outgrows the mount record is listed by the name's
/// start, marked cut, so nothing reading the listing takes it for the whole.
fn an_image_named_past_the_record_is_listed_cut_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 24, b"imgs24");
    let file = "Amiga Workbench 1.3 Install and Extras Collection (Commodore 1988) Disk 1.adf";
    assert!(file.len() > MOUNT_SOURCE_MAX);
    let path = std::format!("/Storage/imgs24/{file}");
    put(0, &path, &amiga_floppy(b"WB13", b"x"));
    let member = plain(MEMBER_UID, STORAGE_GID);
    let open = source(MEMBER_UID, &path, OpenFlags::READ, session(), 0x1A00_0100);
    let volume = mounted(attached(facility, &member, open, &private(true)));
    let listed = FS_SERVICE
        .mount_snapshot()
        .expect("snapshot")
        .into_iter()
        .find(|record| record.volume_id() == volume.volume_id)
        .expect("listed");
    assert_eq!(listed.source_bytes(), &file.as_bytes()[..MOUNT_SOURCE_MAX]);
    assert!(listed.source_cut());
    eject(&volume, DetachMode::Plain, &member).expect("ejects");
    retire(host_id, DetachMode::Plain, round);
}

/// A teardown failing beneath its own volume, on an image held two levels
/// down, waits for that hold: its own holds on what lies on its volume are not
/// something else letting go, so it is never retried at once, forever.
fn a_teardown_failing_beneath_its_volume_waits_for_the_holder_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 25, b"imgs25");
    let member = plain(MEMBER_UID, STORAGE_GID);
    let login = ProcId::from_raw([0x25; 16]);
    let mut dir = String::from("/Storage/imgs25");
    let mut nested: Vec<AttachReply> = Vec::new();
    // Each level holds the next, so each is a little larger than it.
    for depth in 1..=3u32 {
        let path = std::format!("{dir}/below{depth}.hdf");
        let writer = if depth == 1 { 0 } else { MEMBER_UID };
        put(
            writer,
            &path,
            &amiga_volume(
                128 * u64::from(4 - depth),
                std::format!("Below{depth}").as_bytes(),
                b"x",
            ),
        );
        // Only the outermost is held by the session that ends.
        let held_in = if depth == 1 { login } else { session() };
        let volume = mounted(attached(
            facility,
            &member,
            source(
                MEMBER_UID,
                &path,
                read_write(),
                held_in,
                0x1A00_0120 + u64::from(depth),
            ),
            &private(false),
        ));
        dir = std::format!("/Storage/{}", volume.name());
        nested.push(volume);
    }
    let (outer, middle, inner) = (&nested[0], &nested[1], &nested[2]);
    VOLUME_SERVICE.while_image_held(&inner.volume_id, || {
        VOLUME_SERVICE.session_ended(&|ended| ended == login);
        assert!(
            VOLUME_SERVICE.settle(),
            "the outer volume's teardown is taken up"
        );
        assert!(
            !VOLUME_SERVICE.settle() && !VOLUME_SERVICE.owes(),
            "failing beneath its own volume, it waits for the hold"
        );
    });
    for volume in [outer, middle] {
        assert_eq!(
            audited(IMAGE_DETACH_REFUSED, volume.name()),
            [Some(String::from("image_detach_refused"))],
            "{} refused once",
            volume.name()
        );
    }
    assert!(VOLUME_SERVICE.owes(), "the hold letting go is a release");
    settled();
    assert!(nested.iter().all(|volume| !shown(volume.name())));
    assert_eq!(
        audited(IMAGE_VOLUME_DETACHED, outer.name()),
        [Some(String::from("session_ended"))]
    );
    for volume in [middle, inner] {
        assert_eq!(
            audited(IMAGE_VOLUME_DETACHED, volume.name()),
            [Some(String::from("backing_detached"))]
        );
    }
    retire(host_id, DetachMode::Plain, round);
}

/// An image held as the volume its file lies on is pulled is retired by its
/// holder as it lets go, and what lies on it leaves with it.
fn an_image_held_as_its_volume_is_pulled_leaves_with_what_lies_on_it_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (_, round) = host(window_ptr, facility, 26, b"imgs26");
    let member = plain(MEMBER_UID, STORAGE_GID);
    let outer_path = "/Storage/imgs26/holder.hdf";
    put(0, outer_path, &amiga_volume(256, b"Holder", b"x"));
    let outer = mounted(attached(
        facility,
        &member,
        source(MEMBER_UID, outer_path, read_write(), session(), 0x1A00_0130),
        &private(false),
    ));
    let inner_path = std::format!("/Storage/{}/held.hdf", outer.name());
    put(MEMBER_UID, &inner_path, &amiga_volume(128, b"Held", b"x"));
    let inner = mounted(attached(
        facility,
        &member,
        source(
            MEMBER_UID,
            &inner_path,
            OpenFlags::READ,
            session(),
            0x1A00_0131,
        ),
        &private(true),
    ));
    VOLUME_SERVICE.while_image_held(&outer.volume_id, || {
        tairix_kernel_core::callreg::teardown_owned_by(host_owner(26), &ASPACES, &SINK);
        round.stop.store(true, Ordering::Relaxed);
        round.server.join().expect("server thread");
        assert!(
            shown(outer.name()) && shown(inner.name()),
            "its holder retires it as it lets go"
        );
    });
    for volume in [&outer, &inner] {
        assert!(!shown(volume.name()), "{} retired", volume.name());
        assert_eq!(
            audited(IMAGE_VOLUME_RETIRED, volume.name()),
            [Some(String::from("backing_gone"))]
        );
    }
}

/// A name an image asked for is held for it while it awaits its driver: no
/// device volume takes it meanwhile, and the image is shown under it.
fn a_name_an_image_awaits_its_driver_under_is_held_for_it_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 27, b"imgs27");
    let path = "/Storage/imgs27/Late.adf";
    put(0, path, &amiga_floppy(b"Late", b"x"));
    let attach = thread::spawn(move || {
        let asked = ImageAttach {
            name: Some("held27"),
            ..private(true)
        };
        answered(
            &plain(MEMBER_UID, STORAGE_GID),
            source(MEMBER_UID, path, OpenFlags::READ, session(), 0x1A00_0140),
            &asked,
        )
    });
    // Published once its name is judged free; a device asks for that name
    // before the image's driver is started.
    let (node, volume, fstype) = loop {
        if let Some(published) = undriven_volume_nodes(IMAGE_VOLUMES_NODE_ID).pop() {
            break published;
        }
        thread::yield_now();
    };
    let (device, _) = fat32_image(0x1A6E_0000 + 28, b"device");
    let refused = insert(
        window_ptr,
        facility,
        &Insertion {
            endpoint_id: 0xB1D0_5E17_0000_0100 + 28,
            owner: host_owner(28),
            region_task: 0x70_0100 + 28,
            name: b"held27",
            fstype: VolumeFsType::Fat32,
        },
        ServedDevice::new(device),
    );
    assert_eq!(
        refused.outcome,
        Err(Errno::AlreadyExists),
        "held for the image"
    );
    refused.stop.store(true, Ordering::Relaxed);
    refused.server.join().expect("server thread");
    start_driver(facility, node, volume, fstype);
    let image = mounted(attach.join().expect("the attach"));
    assert_eq!(image.name(), "held27");
    eject(&image, DetachMode::Plain, &plain(MEMBER_UID, STORAGE_GID)).expect("ejects");
    retire(host_id, DetachMode::Plain, round);
}

/// An image attached from a file reached through a bind, on host `n`: the
/// volume labelled `stand`, attached writable, holds `mnt`; the volume
/// labelled `carry` is bound over it; the volume labelled `inner` is attached
/// writable from a file put through that bind. Returns the host, the three
/// volumes in that order, and the bind's path.
fn an_image_through_a_bind(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
    host_at: (u32, &'static [u8]),
    labels: [&'static [u8]; 3],
    task: u64,
) -> (([u8; 16], Inserted), [AttachReply; 3], String) {
    let (n, host_name) = host_at;
    let hosted = host(window_ptr, facility, n, host_name);
    let host_dir = std::format!(
        "/Storage/{}",
        core::str::from_utf8(host_name).expect("text")
    );
    let [stand_label, carry_label, inner_label] = labels;
    put(
        0,
        &std::format!("{host_dir}/stand.hdf"),
        &amiga_volume(128, stand_label, b"x"),
    );
    put(
        0,
        &std::format!("{host_dir}/carry.hdf"),
        &amiga_volume(4096, carry_label, b"x"),
    );
    let member = member();
    let open = |path: &str, task| source(MEMBER_UID, path, read_write(), session(), task);
    let stand = mounted(attached(
        facility,
        &member,
        open(&std::format!("{host_dir}/stand.hdf"), task),
        &private(false),
    ));
    let over = std::format!("/Storage/{}/mnt", stand.name());
    FS_SERVICE
        .mkdir(MEMBER_UID, &NoCaps, root(&over), new_dir())
        .expect("a directory of the member's own volume");
    let carry = mounted(attached(
        facility,
        &member,
        open(&std::format!("{host_dir}/carry.hdf"), task + 1),
        &ImageAttach {
            target: Some(over.as_str()),
            ..private(false)
        },
    ));
    let through = std::format!("{over}/inner.hdf");
    put(MEMBER_UID, &through, &amiga_volume(64, inner_label, b"x"));
    let inner = mounted(attached(
        facility,
        &member,
        open(&through, task + 2),
        &private(false),
    ));
    (hosted, [stand, carry, inner], over)
}

/// What `inner.hdf` on `carry` holds in its `ReadMe`, read through a fresh
/// read-only attach.
fn carried_text(facility: &'static TestFacility, carry: &AttachReply, task: u64) -> Vec<u8> {
    let path = std::format!("/Storage/{}/inner.hdf", carry.name());
    let member = member();
    let again = mounted(attached(
        facility,
        &member,
        source(MEMBER_UID, &path, OpenFlags::READ, session(), task),
        &private(true),
    ));
    let mut buf = [0u8; 16];
    let read = read_as(
        MEMBER_UID,
        &std::format!("/Storage/{}/ReadMe", again.name()),
        0,
        &mut buf,
    )
    .expect("reads");
    eject(&again, DetachMode::Plain, &member).expect("ejects");
    buf[..read].to_vec()
}

/// An image attached from a file reached through a bind holds the file
/// through its own volume, so the bind's binder unbinding it takes nothing
/// from the image: it is written and ejected cleanly afterwards.
fn an_image_opened_through_a_bind_outlives_its_unbind_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let ((host_id, round), [stand, carry, inner], over) = an_image_through_a_bind(
        window_ptr,
        facility,
        (29, b"imgs29"),
        [b"Ramp", b"Barrow", b"Parcel"],
        0x1A00_0160,
    );
    let member = member();
    FS_SERVICE
        .view_unbind(MEMBER_UID, &NoCaps, &over)
        .expect("its binder unbinds it");
    let readme = std::format!("/Storage/{}/ReadMe", inner.name());
    assert_eq!(write_as(MEMBER_UID, &readme, 0, b"unbound"), Ok(7));
    eject(&inner, DetachMode::Plain, &member).expect("ejects, flushed");
    assert_eq!(carried_text(facility, &carry, 0x1A00_0163), b"unbound");
    eject(&carry, DetachMode::Plain, &member).expect("ejects");
    eject(&stand, DetachMode::Plain, &member).expect("ejects");
    retire(host_id, DetachMode::Plain, round);
}

/// An image attached from a file reached through a bind outlives the volume
/// that bind stood in being ejected, which withdraws the bind with it.
fn an_image_opened_through_a_bind_outlives_the_volume_it_stood_in_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let ((host_id, round), [stand, carry, inner], _) = an_image_through_a_bind(
        window_ptr,
        facility,
        (30, b"imgs30"),
        [b"Plinth", b"Satchel", b"Packet"],
        0x1A00_0168,
    );
    let member = member();
    eject(&stand, DetachMode::Force, &member).expect("forced out, its bind withdrawn with it");
    let readme = std::format!("/Storage/{}/ReadMe", inner.name());
    assert_eq!(write_as(MEMBER_UID, &readme, 0, b"ejected"), Ok(7));
    eject(&inner, DetachMode::Plain, &member).expect("ejects, flushed");
    assert_eq!(carried_text(facility, &carry, 0x1A00_016B), b"ejected");
    eject(&carry, DetachMode::Plain, &member).expect("ejects");
    retire(host_id, DetachMode::Plain, round);
}

/// A tar of `members`, each a ustar header and its bytes, ended by its two
/// zero blocks.
fn tar(members: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(name, data) in members {
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name);
        header[100..107].copy_from_slice(b"0000644");
        header[108..115].copy_from_slice(b"0000000");
        header[116..123].copy_from_slice(b"0000000");
        header[124..135].copy_from_slice(std::format!("{:011o}", data.len()).as_bytes());
        header[136..147].copy_from_slice(b"00000000000");
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        header[148..156].fill(b' ');
        let sum: u32 = header.iter().map(|&byte| u32::from(byte)).sum();
        header[148..155].copy_from_slice(std::format!("{sum:06o}\0").as_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(data);
        out.resize(out.len().next_multiple_of(512), 0);
    }
    out.extend_from_slice(&[0; 1024]);
    out
}

/// The directory `path` holding an archive, as an `image_attach` caller
/// hands it over with the archive's name in it.
fn directory(uid: u32, path: &str, name: &str) -> ImageDirectory {
    let opened = Descriptor::open(uid, path, OpenFlags::READ.union(OpenFlags::DIRECTORY))
        .unwrap_or_else(|err| std::panic!("open {path}: {err:?}"));
    ImageDirectory {
        dir: NodeHandle {
            authority: Authority::Frozen(FrozenAuthority {
                uid,
                caps: CapabilitySet::empty(),
                grant: None,
            }),
            ..opened.handle().clone()
        },
        name: String::from(name),
        name_key: [0x5A; 32],
    }
}

/// An archive attached with no directory to replace it in is served whole,
/// at its exact length, and read only.
fn an_archive_without_its_directory_attaches_read_only_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 34, b"imgs34");
    let path = "/Storage/imgs34/notes.tar";
    // A length no block divides: the archive is read to its last byte.
    let mut archive = tar(&[(b"ReadMe", b"read only")]);
    archive.truncate(archive.len() - 7);
    put(MEMBER_UID, path, &archive);
    let member = plain(MEMBER_UID, STORAGE_GID);
    let volume = mounted(attached(
        facility,
        &member,
        source(MEMBER_UID, path, read_write(), session(), 0x1A00_0100),
        &private(false),
    ));
    assert_eq!(mount_record(b"notes.tar").fstype_bytes(), b"tar");
    let file = std::format!("/Storage/{}/ReadMe", volume.name());
    let mut buf = [0u8; 16];
    let read = read_as(MEMBER_UID, &file, 0, &mut buf).expect("reads");
    assert_eq!(&buf[..read], b"read only");
    assert_eq!(write_as(MEMBER_UID, &file, 0, b"x"), Err(Errno::ReadOnly));
    eject(&volume, DetachMode::Plain, &member).expect("ejects");
    retire(host_id, DetachMode::Plain, round);
}

/// A writable archive is never written in place: each change is committed
/// by writing the next archive and putting it in place of the file under
/// its name, which then holds the change, and no work file outlasts the
/// volume.
fn an_archive_is_written_by_replacing_its_file_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 35, b"imgs35");
    let path = "/Storage/imgs35/notes.tar";
    put(MEMBER_UID, path, &tar(&[(b"ReadMe", b"before")]));
    let member = plain(MEMBER_UID, STORAGE_GID);
    let mut from = source(MEMBER_UID, path, read_write(), session(), 0x1A00_0101);
    from.directory = Some(directory(MEMBER_UID, "/Storage/imgs35", "notes.tar"));
    let attach = ImageAttach {
        file_name: Some("notes.tar"),
        ..private(false)
    };
    let volume = mounted(attached(facility, &member, from, &attach));
    let added = std::format!("/Storage/{}/Added", volume.name());
    put(MEMBER_UID, &added, b"written through the volume");
    eject(&volume, DetachMode::Plain, &member).expect("ejects");
    let mut archive = vec![0u8; 8192];
    let len = read_as(MEMBER_UID, path, 0, &mut archive).expect("reads the archive");
    let archive = &archive[..len];
    assert!(
        archive.windows(5).any(|window| window == b"Added"),
        "the commit put the next archive in place"
    );
    assert!(archive
        .windows(26)
        .any(|window| window == b"written through the volume"));
    assert!(
        archive.windows(6).any(|window| window == b"before"),
        "the unchanged member is kept"
    );
    retire(host_id, DetachMode::Plain, round);
}

/// The names the directory `path` lists, as `uid`.
fn listed(uid: u32, path: &str) -> Vec<Vec<u8>> {
    FS_SERVICE
        .readdir_bounded(uid, &NoCaps, root(path), FinalLink::Follow, 64)
        .expect("lists")
        .iter()
        .map(|entry| entry.name.as_bytes().to_vec())
        .collect()
}

/// A work file is only ever cut, and written from no further than its end:
/// a hole, or a lengthening, would have the volume fill zeros for as long as
/// its driver asked. None outlasts the volume.
fn an_archives_work_files_grow_only_by_what_is_written_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 36, b"imgs36");
    let path = "/Storage/imgs36/notes.tar";
    put(MEMBER_UID, path, &tar(&[(b"ReadMe", b"before")]));
    let member = plain(MEMBER_UID, STORAGE_GID);
    let mut from = source(MEMBER_UID, path, read_write(), session(), 0x1A00_0102);
    from.directory = Some(directory(MEMBER_UID, "/Storage/imgs36", "notes.tar"));
    let attach = ImageAttach {
        file_name: Some("notes.tar"),
        ..private(false)
    };
    let volume = mounted(attached(facility, &member, from, &attach));
    let served = ServiceAccess(*STARTED.lock().unwrap().last().expect("its driver"));
    let cut = |len| {
        served.ctl(&VolumeCtl::Truncate {
            region: ARCHIVE_STAGING,
            len,
        })
    };
    assert_eq!(
        cut(1 << 20),
        Err(Errno::LengthOutOfRange),
        "never lengthened"
    );
    assert_eq!(
        served.region_write(ARCHIVE_STAGING, 1 << 20, b"x"),
        Err(Errno::LengthOutOfRange),
        "never written past its end"
    );
    served
        .region_write(ARCHIVE_STAGING, 0, b"staged")
        .expect("written from its start");
    assert_eq!(cut(7), Err(Errno::LengthOutOfRange));
    assert_eq!(cut(2), Ok(None));
    let before = listed(MEMBER_UID, "/Storage/imgs36");
    // A next archive the attachment ends without committing.
    served
        .region_write(ARCHIVE_NEXT, 0, b"next")
        .expect("the next archive begun");
    assert_eq!(
        listed(MEMBER_UID, "/Storage/imgs36").len(),
        before.len() + 1
    );
    eject(&volume, DetachMode::Plain, &member).expect("ejects");
    assert_eq!(listed(MEMBER_UID, "/Storage/imgs36"), before);
    retire(host_id, DetachMode::Plain, round);
}

/// A driver serving an archive in place of one lost finds none of the lost
/// one's work files, and its own change is committed as any is.
fn an_archives_lost_driver_leaves_no_work_files_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 38, b"imgs38");
    let path = "/Storage/imgs38/notes.tar";
    put(MEMBER_UID, path, &tar(&[(b"ReadMe", b"before")]));
    let member = plain(MEMBER_UID, STORAGE_GID);
    let mut from = source(MEMBER_UID, path, read_write(), session(), 0x1A00_0104);
    from.directory = Some(directory(MEMBER_UID, "/Storage/imgs38", "notes.tar"));
    let attach = ImageAttach {
        file_name: Some("notes.tar"),
        ..private(false)
    };
    let volume = mounted(attached(facility, &member, from, &attach));
    let served = *STARTED.lock().unwrap().last().expect("its driver");
    let regions = ServiceAccess(served);
    regions
        .region_write(ARCHIVE_STAGING, 0, b"staged")
        .expect("the lost driver's staging");
    let before = listed(MEMBER_UID, "/Storage/imgs38");
    regions
        .region_write(ARCHIVE_NEXT, 0, b"next")
        .expect("the lost driver's next archive");
    let (node, driver) = *DRIVEN.lock().unwrap().last().expect("a driver serves it");
    tairix_kernel_core::callreg::teardown_owned_by(driver, &ASPACES, &SINK);
    start_driver(facility, node, served, VolumeFsType::Tar);
    assert_eq!(
        listed(MEMBER_UID, "/Storage/imgs38"),
        before,
        "the lost driver's next archive is out of the directory"
    );
    let mut staged = [0u8; 8];
    assert_eq!(
        regions.region_read(ARCHIVE_STAGING, 0, &mut staged),
        Ok(0),
        "and its staging let go"
    );
    let added = std::format!("/Storage/{}/Added", volume.name());
    put(MEMBER_UID, &added, b"after the restart");
    eject(&volume, DetachMode::Plain, &member).expect("ejects");
    assert_eq!(listed(MEMBER_UID, "/Storage/imgs38"), before);
    let mut archive = vec![0u8; 8192];
    let len = read_as(MEMBER_UID, path, 0, &mut archive).expect("reads the archive");
    assert!(
        archive[..len]
            .windows(17)
            .any(|window| window == b"after the restart"),
        "the new driver's change is committed"
    );
    retire(host_id, DetachMode::Plain, round);
}

/// An archive whose directory was reached through a read-only view attaches
/// read-only: the commit would hold the directory through its volume's own
/// projection, past what the view allowed.
fn an_archive_reached_through_a_read_only_view_attaches_read_only_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (host_id, round) = host(window_ptr, facility, 37, b"imgs37");
    FS_SERVICE
        .mkdir(
            MEMBER_UID,
            &NoCaps,
            root("/Storage/imgs37/shelf"),
            new_dir(),
        )
        .expect("the member's own directory");
    let path = "/Storage/imgs37/shelf/notes.tar";
    put(MEMBER_UID, path, &tar(&[(b"ReadMe", b"kept")]));
    // The mount authority shows the shelf read-only to those it holds for.
    FS_SERVICE
        .view_bind(
            0,
            &MountAuthority,
            &ViewBindRequest {
                volume_id: host_id,
                subtree: "/shelf",
                target: "/Storage/shelf37",
                narrowing: MountFlags::READ_ONLY,
            },
        )
        .expect("a read-only view of the shelf");
    let member = plain(MEMBER_UID, STORAGE_GID);
    let mut from = source(MEMBER_UID, path, read_write(), session(), 0x1A00_0103);
    from.directory = Some(directory(MEMBER_UID, "/Storage/shelf37", "notes.tar"));
    let attach = ImageAttach {
        file_name: Some("notes.tar"),
        ..private(false)
    };
    let volume = mounted(attached(facility, &member, from, &attach));
    let file = std::format!("/Storage/{}/ReadMe", volume.name());
    assert_eq!(write_as(MEMBER_UID, &file, 0, b"x"), Err(Errno::ReadOnly));
    eject(&volume, DetachMode::Plain, &member).expect("ejects");
    FS_SERVICE
        .view_unbind(0, &MountAuthority, "/Storage/shelf37")
        .expect("unbinds");
    retire(host_id, DetachMode::Plain, round);
}
