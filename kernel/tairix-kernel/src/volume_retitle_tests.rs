//! Retitles over the real service: the user at the seat retitles a removable
//! device's volume whose root they may write and the mount authority any
//! device's volume, everyone else refused; a busy, unserved or read-only
//! volume is refused too. A retitle keeps the volume's key, its identity and
//! its files' ids, is listed with a mount notice and audited, and names the
//! volume at its next attach. The one write path's own rules are unit tested
//! here as well.

use tairix_abi::driver::filesystem::{
    FilesystemLabel, FilesystemWrite, NodeId, NodeKind, NodeSecurity, VolumeTitle, VOLUME_TITLE_MAX,
};
use tairix_abi::{DriverError, FileId, NoticeTopic};
use tairix_fsprobe::name::{candidates, NameBase};

use super::*;
use crate::volume_service::{
    driver_refusal, probed_title, recognise_extent, write_title, VolumeClaim,
};

/// Where an ext4 superblock's `s_volume_name` lies on its device.
const EXT4_VOLUME_NAME: usize = 1024 + tairix_fsprobe::EXT4_VOLUME_NAME_AT;

/// Every retitle scenario, in turn.
pub(super) fn run(window_ptr: *mut u8, facility: &'static TestFacility) {
    the_user_at_the_seat_retitles_a_removable_stick_scenario(window_ptr, facility);
    a_root_owned_ext4_stick_is_the_mount_authoritys_to_retitle_scenario(window_ptr, facility);
    a_fixed_volume_needs_the_mount_authority_scenario(window_ptr, facility);
    a_volume_its_driver_will_not_write_keeps_its_title_scenario(window_ptr, facility);
    a_volume_no_driver_serves_takes_no_title_scenario(window_ptr, facility);
    an_arxfs_volume_is_attached_by_its_new_title_scenario(window_ptr, facility);
}

/// Serve `image` over slot `slot`'s endpoint and attach its volume of
/// `fstype` asking `name`, published from the hardware node `port`, leaving
/// it to await a driver.
fn plug_unserved(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
    slot: u64,
    (name, fstype): (&[u8], VolumeFsType),
    image: RamBlock,
    port: u32,
) -> Inserted {
    let endpoint_id = 0xB1D0_5E17_0000_0000 + slot;
    let endpoint = register_endpoint(endpoint_id, 0x70_1000 + slot);
    let blocks = (image.data.len() / BLOCK_SIZE) as u64;
    let device = StdArc::new(Mutex::new(ServedDevice::new(image)));
    let server = serve(
        StdArc::clone(&endpoint),
        Window(window_ptr),
        StdArc::clone(&device),
    );
    let (_va, region) =
        sharedreg::create(facility, ProcessId(0x70_0000 + slot), 8).expect("region");
    let outcome = VOLUME_SERVICE.attach(
        &VolumeAttachRequest {
            endpoint: endpoint_id,
            window: region,
            first_lba: 0,
            blocks,
            fstype,
            name,
        },
        Some(port),
    );
    Inserted {
        device,
        server,
        outcome,
    }
}

/// [`plug_unserved`], its volume then served by its format's driver.
fn plug(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
    slot: u64,
    volume: (&[u8], VolumeFsType),
    image: RamBlock,
    port: u32,
) -> Inserted {
    let plugged = plug_unserved(window_ptr, facility, slot, volume, image, port);
    serve_published(facility, port);
    plugged
}

/// Title the volume published as `key` `title`, as `caller`.
fn retitle(caller: &Principal<'_>, key: [u8; 16], title: &str) -> Result<(), Errno> {
    VOLUME_SERVICE.retitle(caller, &key, &VolumeTitle::new(title).expect("a title"))
}

/// The id of the file at `path`.
fn file_id(path: &str) -> FileId {
    Descriptor::open(0, path, OpenFlags::READ)
        .and_then(|held| held.stat())
        .unwrap_or_else(|err| std::panic!("stat {path}: {err:?}"))
        .id
}

/// The mount table's notice generation.
fn mounts_notices() -> u64 {
    tairix_kernel_core::notice::generation(
        NoticeTopic::Mounts,
        &tairix_kernel_core::seat::NULL_SEAT_REGISTRY,
    )
}

/// Hold the volume published as `key` as an operation holds it, until the
/// claim drops.
fn hold(key: [u8; 16]) -> VolumeClaim<'static> {
    let mut state = VOLUME_SERVICE.state.lock();
    let volume = state
        .iter_mut()
        .find(|volume| volume.id == key)
        .expect("the volume is attached");
    assert!(!volume.claimed, "nothing else holds it");
    volume.claimed = true;
    VolumeClaim {
        service: &VOLUME_SERVICE,
        id: key,
    }
}

/// The catalog name the volume manager asks for the volume `image` holds:
/// its label's, as `volmgr` derives it from the probe.
fn label_name(image: &mut RamBlock, fstype: VolumeFsType) -> String {
    let request = VolumeAttachRequest {
        endpoint: 0,
        window: 0,
        first_lba: 0,
        blocks: (image.data.len() / BLOCK_SIZE) as u64,
        fstype,
        name: b"probe",
    };
    let probed = recognise_extent(image, &request).expect("the volume is recognised");
    let name = candidates(NameBase::of(probed.label(), None), probed.identity)
        .next()
        .expect("a name");
    String::from_utf8(name.as_bytes().to_vec()).expect("a name is text")
}

/// The user at the seat retitles a removable FAT32 stick whose root they may
/// write: the title is written and listed with a mount notice, audited as the
/// seat's, and the stick keeps its key, its identity and its files' ids. A
/// user away from the seat is refused, and a title FAT cannot spell writes
/// nothing.
fn the_user_at_the_seat_retitles_a_removable_stick_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (image, identity) = fat32_image(0x0D15_C1E0, b"titled payload");
    let round = plug(
        window_ptr,
        facility,
        0x1E0,
        (b"retitle-seat", VolumeFsType::Fat32),
        image,
        ON_REMOVABLE_PORT,
    );
    round.outcome.expect("the stick attaches");
    let listed = mount_record(b"retitle-seat");
    assert!(listed.titled(), "a FAT volume takes a title");
    assert!(listed.title().is_empty(), "formatted with none");
    let file = file_id("/Storage/retitle-seat/hello.txt");
    assert_eq!(
        retitle(&member(), identity, "HOLIDAY"),
        Err(Errno::PermissionDenied),
        "a user away from the seat"
    );
    assert_eq!(
        retitle(&seated(member()), identity, "Holiday"),
        Err(Errno::OutOfRange),
        "FAT spells no lower case"
    );
    assert!(mount_record(b"retitle-seat").title().is_empty());
    let before = mounts_notices();
    retitle(&seated(member()), identity, "HOLIDAY").expect("the user at the seat retitles it");
    assert!(mounts_notices() > before, "the new title is announced");
    let titled = mount_record(b"retitle-seat");
    assert_eq!(titled.title().as_str(), "HOLIDAY");
    assert_eq!(
        (titled.volume_id(), titled.identity()),
        (identity, identity),
        "its key and identity stay"
    );
    assert_eq!(
        file_id("/Storage/retitle-seat/hello.txt"),
        file,
        "and its files' ids"
    );
    assert_eq!(
        audited(VOLUME_RETITLE_REFUSED, "retitle-seat"),
        [
            Some(String::from("no_mount_authority")),
            Some(String::from("title_refused"))
        ]
    );
    assert_eq!(
        audited_levels(VOLUME_RETITLE_REFUSED, "retitle-seat"),
        [Level::Warn, Level::Info],
        "only a refused authority is a warning"
    );
    assert_eq!(
        audited(VOLUME_RETITLED, "retitle-seat"),
        [Some(String::from("seat"))]
    );
    retire(identity, DetachMode::Plain, round);
}

/// A removable ext4 stick whose root is the system's, `0755`, is not the user
/// at the seat's to retitle, its own records keeping them from writing it; the
/// mount authority retitles it, the title landing in `s_volume_name`, which
/// names it at its next attach.
fn a_root_owned_ext4_stick_is_the_mount_authoritys_to_retitle_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let identity = [0xE7; 16];
    let round = plug(
        window_ptr,
        facility,
        0x1E1,
        (b"retitle-ext", VolumeFsType::Ext4),
        ext4_image(identity),
        ON_REMOVABLE_PORT,
    );
    round.outcome.expect("the stick attaches");
    assert_eq!(
        retitle(&seated(member()), identity, "Retitled Ext"),
        Err(Errno::PermissionDenied),
        "the seat reaches only a root it may write"
    );
    retitle(&admin(), identity, "Retitled Ext").expect("the mount authority retitles it");
    assert_eq!(
        mount_record(b"retitle-ext").title().as_str(),
        "Retitled Ext"
    );
    assert_eq!(
        audited(VOLUME_RETITLE_REFUSED, "retitle-ext"),
        [Some(String::from("root_not_writable"))]
    );
    assert_eq!(
        audited(VOLUME_RETITLED, "retitle-ext"),
        [Some(String::from("mount_authority"))]
    );
    let device = StdArc::clone(&round.device);
    retire(identity, DetachMode::Plain, round);
    let mut image = RamBlock {
        data: device.lock().unwrap().block.data.clone(),
    };
    assert_eq!(
        &image.data[EXT4_VOLUME_NAME..EXT4_VOLUME_NAME + 12],
        b"Retitled Ext",
        "the title is the superblock's own"
    );
    assert_eq!(label_name(&mut image, VolumeFsType::Ext4), "retitled-ext");
}

/// A fixed disk's volume is the mount authority's alone to retitle, which is
/// what the administrator's prompt asks for; a volume another operation holds
/// is refused busy, and an identity no volume carries is not found.
fn a_fixed_volume_needs_the_mount_authority_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (image, identity) = fat32_image(0x0D15_C1E3, b"fixed payload");
    let round = plug(
        window_ptr,
        facility,
        0x1E3,
        (b"retitle-fixed", VolumeFsType::Fat32),
        image,
        PUBLISHER,
    );
    round.outcome.expect("the volume attaches");
    assert_eq!(
        retitle(&seated(member()), identity, "FIXED"),
        Err(Errno::PermissionDenied),
        "the seat reaches no fixed disk"
    );
    let held = hold(identity);
    assert_eq!(
        retitle(&admin(), identity, "HELD"),
        Err(Errno::Busy),
        "nothing acts on a volume another operation holds"
    );
    drop(held);
    retitle(&admin(), identity, "FIXED").expect("the mount authority retitles it");
    assert_eq!(mount_record(b"retitle-fixed").title().as_str(), "FIXED");
    assert_eq!(
        audited(VOLUME_RETITLE_REFUSED, "retitle-fixed"),
        [
            Some(String::from("no_mount_authority")),
            Some(String::from("volume_busy"))
        ]
    );
    assert_eq!(retitle(&admin(), [0x9E; 16], "GONE"), Err(Errno::NotFound));
    assert!(audited(VOLUME_RETITLE_REFUSED, "unknown")
        .contains(&Some(String::from("identity_not_attached"))));
    retire(identity, DetachMode::Plain, round);
}

/// A volume its driver will not write keeps its title: the retitle is refused
/// read-only, and nothing is listed.
fn a_volume_its_driver_will_not_write_keeps_its_title_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let identity = [0xE8; 16];
    let mut image = ext4_image(identity);
    image.data[EXT4_STATE_BYTE] |= EXT4_STATE_ERRORS;
    let round = plug(
        window_ptr,
        facility,
        0x1E4,
        (b"retitle-ro", VolumeFsType::Ext4),
        image,
        PUBLISHER,
    );
    round.outcome.expect("the volume attaches");
    assert_eq!(retitle(&admin(), identity, "Kept"), Err(Errno::ReadOnly));
    assert!(
        mount_record(b"retitle-ro").title().is_empty(),
        "nothing is listed"
    );
    assert_eq!(
        audited(VOLUME_RETITLE_REFUSED, "retitle-ro"),
        [Some(String::from("read_only"))]
    );
    retire(identity, DetachMode::Plain, round);
}

/// A volume no driver serves yet takes no title, since nothing could write
/// one: the retitle is refused as the device being offline.
fn a_volume_no_driver_serves_takes_no_title_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (image, identity) = fat32_image(0x0D15_C1E5, b"pending payload");
    let round = plug_unserved(
        window_ptr,
        facility,
        0x1E5,
        (b"retitle-pending", VolumeFsType::Fat32),
        image,
        PUBLISHER,
    );
    round
        .outcome
        .expect("the volume attaches, awaiting its driver");
    assert_eq!(
        retitle(&admin(), identity, "EARLY"),
        Err(Errno::DeviceOffline)
    );
    assert_eq!(
        audited(VOLUME_RETITLE_REFUSED, "retitle-pending"),
        [Some(String::from("volume_unserved"))]
    );
    retire(identity, DetachMode::Plain, round);
}

/// An ARXFS volume takes a title in its superblock: listed once written, its
/// files keeping their ids, and read back by the probe at its next attach,
/// which names it by it while it stays the volume it was.
fn an_arxfs_volume_is_attached_by_its_new_title_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (image, identity) = arxfs_image(b"arxfs payload");
    let round = plug(
        window_ptr,
        facility,
        0x1E6,
        (b"retitle-arx", VolumeFsType::ARXFS),
        image,
        PUBLISHER,
    );
    round.outcome.expect("the volume attaches");
    assert!(mount_record(b"retitle-arx").titled());
    let file = file_id("/Storage/retitle-arx/target.txt");
    retitle(&admin(), identity, "Retitled Arx").expect("the mount authority retitles it");
    assert_eq!(
        mount_record(b"retitle-arx").title().as_str(),
        "Retitled Arx"
    );
    assert_eq!(
        file_id("/Storage/retitle-arx/target.txt"),
        file,
        "its files keep their ids"
    );
    let device = StdArc::clone(&round.device);
    retire(identity, DetachMode::Plain, round);
    let mut image = RamBlock {
        data: device.lock().unwrap().block.data.clone(),
    };
    let name = label_name(&mut image, VolumeFsType::ARXFS);
    assert_eq!(name, "retitled-arx", "the next attach is named by it");
    let again = plug(
        window_ptr,
        facility,
        0x1E7,
        (name.as_bytes(), VolumeFsType::ARXFS),
        image,
        PUBLISHER,
    );
    again.outcome.expect("it attaches again");
    let listed = mount_record(name.as_bytes());
    assert_eq!(
        listed.title().as_str(),
        "Retitled Arx",
        "titled from its superblock"
    );
    assert_eq!(listed.volume_id(), identity, "and the volume it was");
    retire(identity, DetachMode::Plain, again);
}

/// A driver keeping the title it is given, or refusing it, whose label reads
/// as no title at all, and whose flush answers `flushed`.
struct Titling {
    set: Result<(), DriverError>,
    flushed: Result<(), DriverError>,
    flushes: usize,
}

impl Titling {
    fn new(set: Result<(), DriverError>, flushed: Result<(), DriverError>) -> Self {
        Self {
            set,
            flushed,
            flushes: 0,
        }
    }
}

impl FilesystemLabel for Titling {
    fn label(&mut self) -> Result<VolumeTitle, DriverError> {
        Err(DriverError::OutOfRange)
    }

    fn set_label(&mut self, _title: &VolumeTitle) -> Result<(), DriverError> {
        self.set
    }
}

impl FilesystemWrite for Titling {
    fn create(
        &mut self,
        _dir: NodeId,
        _name: &[u8],
        _kind: NodeKind,
        _record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        Err(DriverError::Unsupported)
    }

    fn write_at(
        &mut self,
        _file: NodeId,
        _offset: u64,
        _data: &[u8],
    ) -> Result<usize, DriverError> {
        Err(DriverError::Unsupported)
    }

    fn truncate(&mut self, _file: NodeId, _size: u64) -> Result<(), DriverError> {
        Err(DriverError::Unsupported)
    }

    fn remove(&mut self, _dir: NodeId, _name: &[u8]) -> Result<(), DriverError> {
        Err(DriverError::Unsupported)
    }

    fn rename(
        &mut self,
        _src_dir: NodeId,
        _src_name: &[u8],
        _dst_dir: NodeId,
        _dst_name: &[u8],
    ) -> Result<(), DriverError> {
        Err(DriverError::Unsupported)
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.flushes += 1;
        self.flushed
    }
}

/// A title is written without the one it replaces being read: a label no
/// title spells stands in the way of nothing.
#[test]
fn a_title_is_written_without_reading_the_one_it_replaces() {
    let title = VolumeTitle::new("Work").expect("a title");
    let mut fs = Titling::new(Ok(()), Ok(()));
    let mut listed = false;
    assert_eq!(write_title(&mut fs, &title, || listed = true), Ok(()));
    assert!(listed);
    assert_eq!(fs.flushes, 1);
}

/// A title its driver holds is listed though the flush fails: the driver's
/// writeback publishes it.
#[test]
fn a_title_its_driver_holds_is_listed_though_the_flush_fails() {
    let title = VolumeTitle::new("Work").expect("a title");
    let mut fs = Titling::new(Ok(()), Err(DriverError::MediumError));
    let mut listed = false;
    assert_eq!(
        write_title(&mut fs, &title, || listed = true),
        Err(("flush_failed", Errno::MediumError))
    );
    assert!(listed);
}

/// A title the driver refuses is neither listed nor flushed.
#[test]
fn a_refused_title_is_neither_listed_nor_flushed() {
    let title = VolumeTitle::new("Work").expect("a title");
    let mut fs = Titling::new(Err(DriverError::ReadOnly), Ok(()));
    let mut listed = false;
    assert_eq!(
        write_title(&mut fs, &title, || listed = true),
        Err(("read_only", Errno::ReadOnly))
    );
    assert!(!listed);
    assert_eq!(fs.flushes, 0);
}

#[test]
fn a_drivers_refusal_of_a_title_reads_as_what_it_refused() {
    assert_eq!(
        driver_refusal(DriverError::Unsupported),
        ("untitled_format", Errno::NotSupported),
        "a format keeping none, not a service missing"
    );
    assert_eq!(
        driver_refusal(DriverError::ReadOnly),
        ("read_only", Errno::ReadOnly)
    );
    for (why, errno) in [
        (DriverError::LengthOutOfRange, Errno::LengthOutOfRange),
        (DriverError::OutOfRange, Errno::OutOfRange),
        (DriverError::NoSpace, Errno::NoSpace),
        (DriverError::MediumError, Errno::MediumError),
    ] {
        assert_eq!(driver_refusal(why), ("title_refused", errno), "{why:?}");
    }
}

#[test]
fn a_probed_label_is_titled_as_much_of_it_as_a_title_holds() {
    assert_eq!(probed_title(b"Workbench").as_str(), "Workbench");
    assert!(probed_title(b"").is_empty());
    assert!(
        probed_title(&[0xFF, 0xFE]).is_empty(),
        "a label that is no text is no title"
    );
    assert!(
        probed_title(b"tab\there").is_empty(),
        "nor one carrying a control character"
    );
    let label = alloc::format!("x{}", "é".repeat(VOLUME_TITLE_MAX));
    let cut = probed_title(label.as_bytes());
    assert_eq!(
        cut.as_str().len(),
        VOLUME_TITLE_MAX - 1,
        "cut between characters"
    );
    assert!(label.starts_with(cut.as_str()));
}
