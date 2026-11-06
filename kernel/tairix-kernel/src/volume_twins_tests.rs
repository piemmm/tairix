//! Twins over the real service (`docs/src/filesystem/drives.md` §8): media
//! formatted alike attach side by side, each under a key and a catalog name
//! of its own, the volume attached first keeping its identity's; the same
//! medium twice is busy, an identity's attachments are bounded, and twin names
//! hold against hostile names. A re-insert finding two lookalikes unavailable
//! is replayed onto neither and attaches read-only, unless its device proves
//! which it is, as a RAID array its composer restarted proves itself by the
//! identity its members compose; a medium nothing proves is held read-only
//! for an answer, which alone writes or discards what it holds: the mount
//! authority's for any volume, from wherever it runs, and the seat's for a
//! removable one.

use tairix_abi::volume::VOLUME_TWINS_MAX;

use super::*;
use crate::volume_service::{answering_authority, extents_meet, DeviceAuthority};

/// The service's audited twin attachment.
const TWIN_ATTACHED: u32 = 4219;

/// The service's audited recovery whose medium its evidence refused.
const REINSERT_CONFLICT: u32 = 4186;

/// The serial every FAT32 stick here is formatted under, so each is the
/// others' twin.
const TWIN_SERIAL: u32 = 0x0D15_C170;

/// Blocks of a floppy-sized ADFS volume.
const FLOPPY_BLOCKS: u64 = 1600;

/// Every twin scenario, in turn: those serving their volumes first, then
/// those leaving them to await a driver no scenario starts.
pub(super) fn run(window_ptr: *mut u8, facility: &'static TestFacility) {
    identical_sticks_attach_side_by_side_scenario(window_ptr, facility);
    identical_discs_attach_side_by_side_scenario(window_ptr, facility);
    the_first_keeps_its_identity_and_a_twin_its_key_scenario(window_ptr, facility);
    two_lookalikes_left_unavailable_are_replayed_onto_neither_scenario(window_ptr, facility);
    a_device_proves_which_lookalike_a_medium_is_scenario(window_ptr, facility);
    a_cloned_stick_is_never_written_unasked_scenario(window_ptr, facility);
    an_array_its_composer_restarted_is_replayed_on_proof_scenario(window_ptr, facility);
    the_seat_has_the_held_writes_written_scenario(window_ptr, facility);
    the_seat_has_the_held_writes_discarded_scenario(window_ptr, facility);
    the_mount_authority_answers_for_a_fixed_volume_scenario(window_ptr, facility);
    the_seat_does_not_answer_for_a_fixed_volume_scenario(window_ptr, facility);
    only_an_authority_answers_for_a_volume_awaiting_it_scenario(window_ptr, facility);
    a_medium_changed_since_the_question_is_not_written_scenario(window_ptr, facility);
    the_same_medium_twice_is_busy_scenario(window_ptr, facility);
    an_identitys_attachments_are_bounded_scenario(window_ptr, facility);
    twin_names_hold_against_hostile_names_scenario(window_ptr, facility);
}

/// Two extents meet exactly where they share a block.
#[test]
fn extents_meet_where_they_share_a_block() {
    let meeting = [
        ((0, 10), (9, 1)),
        ((5, 10), (0, 6)),
        ((0, 10), (2, 3)),
        ((u64::MAX - 1, 4), (u64::MAX - 2, 2)),
    ];
    for (one, other) in meeting {
        assert!(extents_meet(one, other), "{one:?} {other:?}");
        assert!(extents_meet(other, one), "{other:?} {one:?}");
    }
    for (one, other) in [((0, 10), (10, 5)), ((3, 1), (4, 1)), ((0, 0), (0, 10))] {
        assert!(!extents_meet(one, other), "{one:?} {other:?}");
        assert!(!extents_meet(other, one), "{other:?} {one:?}");
    }
}

/// A caller holding only `CAP_DISPLAY`, the seat's own capability.
struct SeatCapability;
impl CapabilityQuery for SeatCapability {
    fn holds(&self, cap: CapabilityId) -> bool {
        cap == CapabilityId::DISPLAY
    }
}

/// The desktop holding the seat for the member at it, as the kernel attests
/// an answer from it.
fn desktop() -> Principal<'static> {
    Principal {
        caps: &SeatCapability,
        ..seated(member())
    }
}

/// The mount authority answers wherever it runs, the seat's holder only while
/// it holds the seat's own capability, and anyone else never.
#[test]
fn an_answer_rests_on_the_mount_authority_or_the_seat() {
    assert_eq!(answering_authority(&admin()), Some(DeviceAuthority::Mount));
    assert_eq!(
        answering_authority(&seated(admin())),
        Some(DeviceAuthority::Mount)
    );
    assert_eq!(answering_authority(&desktop()), Some(DeviceAuthority::Seat));
    let away = Principal {
        at_seat: false,
        ..desktop()
    };
    for nobody in [member(), seated(member()), away] {
        assert_eq!(answering_authority(&nobody), None);
    }
}

/// The endpoint, owner and window creator of slot `slot`, apart from every
/// other scenario's.
const fn ids(slot: u64) -> (u64, u64, u64) {
    (
        0xB1D0_5E17_0000_0000 + slot,
        0x70_1000 + slot,
        0x70_0000 + slot,
    )
}

/// How the medium in slot `slot` is served and attached, asking `name`.
fn insertion(slot: u64, name: &'static [u8], fstype: VolumeFsType) -> Insertion {
    let (endpoint_id, owner, region_task) = ids(slot);
    Insertion {
        endpoint_id,
        owner,
        region_task,
        name,
        fstype,
    }
}

/// One device carrying a floppy-sized ADFS volume of each of `variants`, back
/// to back, and the identity each carries: discs of one variant are formatted
/// alike, so they are twins.
fn floppies(variants: impl IntoIterator<Item = AdfsVariant>) -> (RamBlock, Vec<[u8; 16]>) {
    let mut data = Vec::new();
    let mut identities = Vec::new();
    for variant in variants {
        let disc = Adfs::format(RamBlock::new(FLOPPY_BLOCKS), variant).expect("formats");
        identities.push(disc.volume_identity());
        data.extend_from_slice(&disc.into_device().data);
    }
    (RamBlock { data }, identities)
}

/// A device served over slot `slot`'s endpoint, whose volumes a scenario
/// attaches itself, extent by extent.
struct Device {
    endpoint: u64,
    region: u64,
    server: StoppableTask,
}

impl Device {
    /// Serve `block` over slot `slot`.
    fn up(
        window_ptr: *mut u8,
        facility: &'static TestFacility,
        slot: u64,
        block: RamBlock,
    ) -> Self {
        let (endpoint, owner, region_task) = ids(slot);
        let server = serve(
            register_endpoint(endpoint, owner),
            Window(window_ptr),
            StdArc::new(Mutex::new(ServedDevice::new(block))),
        );
        let (_va, region) = sharedreg::create(facility, ProcessId(region_task), 8).expect("region");
        Self {
            endpoint,
            region,
            server,
        }
    }

    /// The request attaching `fstype` over `extent`, its first LBA and block
    /// count, asking `name`.
    fn request<'a>(
        &self,
        (first_lba, blocks): (u64, u64),
        fstype: VolumeFsType,
        name: &'a [u8],
    ) -> VolumeAttachRequest<'a> {
        VolumeAttachRequest {
            endpoint: self.endpoint,
            window: self.region,
            first_lba,
            blocks,
            fstype,
            name,
        }
    }

    /// The request attaching the floppy at `at` on the device, asking `name`.
    fn floppy<'a>(&self, at: u64, name: &'a [u8]) -> VolumeAttachRequest<'a> {
        self.request(
            (at * FLOPPY_BLOCKS, FLOPPY_BLOCKS),
            VolumeFsType::Adfs,
            name,
        )
    }

    /// Detach every volume attached over the device, then stop serving it.
    fn down(self) {
        for (_, name, volume_id) in attached_on(self.endpoint) {
            VOLUME_SERVICE
                .detach(
                    &VolumeDetachRequest {
                        volume_id,
                        mode: DetachMode::Plain,
                    },
                    &admin(),
                )
                .unwrap_or_else(|err| std::panic!("detach {name}: {err:?}"));
        }
        self.server.stop();
    }
}

/// The first block, catalog name and key of each volume attached over
/// `endpoint`, in the order they entered.
fn attached_on(endpoint: u64) -> Vec<(u64, String, [u8; 16])> {
    VOLUME_SERVICE
        .state
        .lock()
        .iter()
        .filter(|volume| volume.endpoint == endpoint)
        .map(|volume| (volume.first_lba, volume.name.clone(), volume.id))
        .collect()
}

/// The catalog names of the volumes attached over `endpoint`, in the order
/// they entered.
fn names_on(endpoint: u64) -> Vec<String> {
    attached_on(endpoint)
        .into_iter()
        .map(|(_, name, _)| name)
        .collect()
}

/// Attach `request` as the volume manager does, leaving the volume to await
/// the driver no scenario here starts for it.
fn pending(request: &VolumeAttachRequest<'_>) -> Result<(), Errno> {
    VOLUME_SERVICE.attach(request, Some(PUBLISHER))
}

/// Whether a live volume feeds the change table of `key`, so no other may
/// claim it.
fn watched(key: [u8; 16]) -> bool {
    let wiring = VOLUME_SERVICE.wiring().expect("booted");
    let watches = wiring.watches.expect("the change tables are wired");
    watches
        .claim(key, NameMatching::Exact, wiring.pressure)
        .is_none()
}

/// `first` is listed under `identity` and `twin` beside it under a key of its
/// own, both carrying `identity`, each key resolving to its own catalog entry
/// and feeding a change table of its own: the twin's key.
fn assert_twins(first: &str, twin: &str, identity: [u8; 16]) -> [u8; 16] {
    let held = mount_record(first.as_bytes());
    assert_eq!(
        (held.volume_id(), held.identity()),
        (identity, identity),
        "{first} keeps its identity's key"
    );
    let beside = mount_record(twin.as_bytes());
    let key = beside.volume_id();
    assert_ne!(key, identity, "{twin} is published under a key of its own");
    assert_eq!(beside.identity(), identity, "{twin} carries the identity");
    for (published, name) in [(identity, first), (key, twin)] {
        assert_eq!(
            resolved(&published),
            Some(vec![String::from("Storage"), String::from(name)]),
            "{name}'s key resolves to its own entry"
        );
        assert!(watched(published), "{name} feeds a table of its own");
    }
    key
}

/// The volume published as `key` and shown as `name` has left: its key
/// resolves nothing, no listing names it, and its change table is let go.
fn assert_gone(key: [u8; 16], name: &str) {
    assert_eq!(VOLUME_FOREST.resolve(&key), None, "{name}'s key");
    assert!(
        FS_SERVICE
            .mount_snapshot()
            .expect("snapshot")
            .iter()
            .all(|record| record.source_bytes() != name.as_bytes()),
        "{name} is listed no more"
    );
    assert!(!watched(key), "{name}'s change table is let go");
}

/// The volume part of the id of the file at `path`.
fn file_volume(path: &str) -> [u8; 16] {
    Descriptor::open(0, path, OpenFlags::READ)
        .and_then(|held| held.stat())
        .unwrap_or_else(|err| std::panic!("stat {path}: {err:?}"))
        .id
        .volume
}

/// Two FAT32 sticks formatted under one serial attach side by side: the first
/// under its identity and the name it asked, the second under a key of its
/// own and that name with `_2`, each serving its own files under file ids of
/// its own and detached by its own key.
fn identical_sticks_attach_side_by_side_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (image, identity) = fat32_image(TWIN_SERIAL, b"first stick");
    let (second, second_identity) = fat32_image(TWIN_SERIAL, b"second stick");
    assert_eq!(second_identity, identity, "formatted alike, one identity");
    let first = insert(
        window_ptr,
        facility,
        &insertion(0x170, b"twins", VolumeFsType::Fat32),
        ServedDevice::new(image),
    );
    first.outcome.expect("the first stick attaches");
    let twin = insert(
        window_ptr,
        facility,
        &insertion(0x171, b"twins", VolumeFsType::Fat32),
        ServedDevice::new(second),
    );
    twin.outcome.expect("its twin attaches beside it");
    let key = assert_twins("twins", "twins_2", identity);
    assert_eq!(
        audited_text(TWIN_ATTACHED, "twins_2", "asked"),
        [Some(String::from("twins"))]
    );
    let paths = ["/Storage/twins/hello.txt", "/Storage/twins_2/hello.txt"];
    assert_volume_reads(paths[0], b"first stick");
    assert_volume_reads(paths[1], b"second stick");
    assert_eq!(
        paths.map(file_volume),
        [identity, key],
        "a file's id says which twin holds it"
    );
    retire(key, DetachMode::Plain, twin);
    assert_gone(key, "twins_2");
    assert_volume_reads(paths[0], b"first stick");
    retire(identity, DetachMode::Plain, first);
    assert_gone(identity, "twins");
}

/// Two ADFS discs formatted alike attach side by side, each written and read
/// back through its own volume.
fn identical_discs_attach_side_by_side_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (first_disc, identities) = floppies([AdfsVariant::E]);
    let (second_disc, second) = floppies([AdfsVariant::E]);
    assert_eq!(second, identities, "formatted alike, one identity");
    let identity = identities[0];
    let first = insert(
        window_ptr,
        facility,
        &insertion(0x172, b"twin-disc", VolumeFsType::Adfs),
        ServedDevice::new(first_disc),
    );
    first.outcome.expect("the first disc attaches");
    let twin = insert(
        window_ptr,
        facility,
        &insertion(0x173, b"twin-disc", VolumeFsType::Adfs),
        ServedDevice::new(second_disc),
    );
    twin.outcome.expect("its twin attaches beside it");
    let key = assert_twins("twin-disc", "twin-disc_2", identity);
    let create = OpenFlags::WRITE.union(OpenFlags::CREATE);
    let paths = ["/Storage/twin-disc/ReadMe", "/Storage/twin-disc_2/ReadMe"];
    for (path, text) in paths.into_iter().zip([&b"first disc"[..], b"second disc"]) {
        open_as(0, path, create).expect("a file is made");
        write_as(0, path, 0, text).expect("and written");
    }
    assert_volume_reads(paths[0], b"first disc");
    assert_volume_reads(paths[1], b"second disc");
    assert_eq!(
        paths.map(file_volume),
        [identity, key],
        "a file's id says which twin holds it"
    );
    retire(key, DetachMode::Plain, twin);
    assert_gone(key, "twin-disc_2");
    assert_volume_reads(paths[0], b"first disc");
    retire(identity, DetachMode::Plain, first);
}

/// The volume attached first keeps `id::` of its identity, and a twin its own
/// key only while attached: once the first leaves its identity resolves
/// nothing and the twin keeps its key, the next medium of the identity
/// attaches first, and a twin attached again takes a key no earlier
/// attachment had.
fn the_first_keeps_its_identity_and_a_twin_its_key_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let identity = floppies([AdfsVariant::E]).1[0];
    let attach = |slot| {
        let (disc, _) = floppies([AdfsVariant::E]);
        let round = insert(
            window_ptr,
            facility,
            &insertion(slot, b"twin-pair", VolumeFsType::Adfs),
            ServedDevice::new(disc),
        );
        round.outcome.expect("the disc attaches");
        round
    };
    let first = attach(0x174);
    let twin = attach(0x175);
    let key = assert_twins("twin-pair", "twin-pair_2", identity);
    retire(identity, DetachMode::Plain, first);
    assert_eq!(VOLUME_FOREST.resolve(&identity), None, "the first has left");
    assert_eq!(
        mount_record(b"twin-pair_2").volume_id(),
        key,
        "a live volume is never re-keyed"
    );
    let next = attach(0x176);
    assert_eq!(
        mount_record(b"twin-pair").volume_id(),
        identity,
        "the next medium of the identity attaches first"
    );
    retire(key, DetachMode::Plain, twin);
    assert_gone(key, "twin-pair_2");
    let again = attach(0x177);
    let rekeyed = assert_twins("twin-pair", "twin-pair_2", identity);
    assert_ne!(rekeyed, key, "a twin's key is never reused");
    assert_eq!(VOLUME_FOREST.resolve(&key), None);
    retire(rekeyed, DetachMode::Plain, again);
    retire(identity, DetachMode::Plain, next);
}

/// Two lookalike sticks pulled with a write held each leave a volume of one
/// identity unavailable. A third stick of the identity is proven the medium of
/// neither, so nothing is replayed onto it: it attaches afresh as their twin,
/// read-only, so no write of its own strands either set, and both retained
/// sets are kept for their own sticks.
fn two_lookalikes_left_unavailable_are_replayed_onto_neither_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (image, identity) = fat32_image(TWIN_SERIAL, b"lookalike payload");
    let pristine = image.data.clone();
    let first = insert(
        window_ptr,
        facility,
        &insertion(0x17A, b"twin-look", VolumeFsType::Fat32),
        ServedDevice::new(image),
    );
    first.outcome.expect("the first stick attaches");
    let second = insert(
        window_ptr,
        facility,
        &insertion(0x17B, b"twin-look", VolumeFsType::Fat32),
        ServedDevice::new(RamBlock {
            data: pristine.clone(),
        }),
    );
    second.outcome.expect("its lookalike attaches beside it");
    let key = assert_twins("twin-look", "twin-look_2", identity);
    for (name, slot, round) in [("twin-look", 0x17A, first), ("twin-look_2", 0x17B, second)] {
        let path = std::format!("/Storage/{name}/hello.txt");
        write_as(0, &path, 0, b"D").expect("a write the stick takes into its cache");
        tairix_kernel_core::callreg::teardown_owned_by(ids(slot).1, &ASPACES, &SINK);
        round.server.stop();
        assert_eq!(
            mount_record(name.as_bytes()).availability(),
            MountAvailability::UnavailableDirty,
            "{name} is pulled with its write held"
        );
    }
    let third = insert(
        window_ptr,
        facility,
        &insertion(0x17C, b"twin-look", VolumeFsType::Fat32),
        ServedDevice::new(RamBlock { data: pristine }),
    );
    third.outcome.expect("a third lookalike attaches");
    let fresh = mount_record(b"twin-look_3");
    assert_eq!(fresh.availability(), MountAvailability::Available);
    assert_eq!(fresh.identity(), identity);
    assert!(
        ![identity, key].contains(&fresh.volume_id()),
        "the third is published under a key of its own"
    );
    assert_volume_reads("/Storage/twin-look_3/hello.txt", b"lookalike payload");
    assert_eq!(
        write_as(0, "/Storage/twin-look_3/hello.txt", 0, b"X").err(),
        Some(Errno::ReadOnly),
        "a medium either stick's writes may belong on takes none of its own"
    );
    assert_eq!(
        audited(ATTACHED_READ_ONLY, "twin-look_3"),
        [Some(String::from("lookalikes_hold_writes"))]
    );
    for name in ["twin-look", "twin-look_2"] {
        assert_eq!(
            mount_record(name.as_bytes()).availability(),
            MountAvailability::UnavailableDirty,
            "{name} keeps its retained set for its own stick"
        );
    }
    for volume_id in [identity, key] {
        VOLUME_SERVICE
            .detach(
                &VolumeDetachRequest {
                    volume_id,
                    mode: DetachMode::Force,
                },
                &admin(),
            )
            .expect("the audited force discards a retained set");
    }
    retire(fresh.volume_id(), DetachMode::Plain, third);
}

/// Of two lookalikes pulled with writes held, a medium whose device states the
/// first's identity is that one: its write is replayed onto it, and the
/// second's stays held for its own stick.
fn a_device_proves_which_lookalike_a_medium_is_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (image, identity) = fat32_image(0x0D15_C18A, b"proven payload");
    let pristine = image.data.clone();
    let first = insert(
        window_ptr,
        facility,
        &insertion(0x18A, b"proven", VolumeFsType::Fat32),
        ServedDevice {
            identity: UNIT,
            ..ServedDevice::new(image)
        },
    );
    first.outcome.expect("the first stick attaches");
    let second = insert(
        window_ptr,
        facility,
        &insertion(0x18B, b"proven", VolumeFsType::Fat32),
        ServedDevice {
            identity: OTHER_UNIT,
            ..ServedDevice::new(RamBlock {
                data: pristine.clone(),
            })
        },
    );
    second.outcome.expect("its lookalike attaches beside it");
    let key = assert_twins("proven", "proven_2", identity);
    for (name, slot, round) in [("proven", 0x18A, first), ("proven_2", 0x18B, second)] {
        let path = std::format!("/Storage/{name}/hello.txt");
        write_as(0, &path, 0, b"D").expect("a write the stick takes into its cache");
        tairix_kernel_core::callreg::teardown_owned_by(ids(slot).1, &ASPACES, &SINK);
        round.server.stop();
    }
    let back = insert(
        window_ptr,
        facility,
        &insertion(0x18C, b"proven", VolumeFsType::Fat32),
        ServedDevice {
            identity: UNIT,
            ..ServedDevice::new(RamBlock { data: pristine })
        },
    );
    back.outcome.expect("the first stick comes back");
    assert_eq!(
        mount_record(b"proven").availability(),
        MountAvailability::Available
    );
    assert_eq!(
        mount_record(b"proven_2").availability(),
        MountAvailability::UnavailableDirty,
        "the second's write stays held for its own stick"
    );
    let mut first_byte = [0u8; 1];
    read_as(0, "/Storage/proven/hello.txt", 0, &mut first_byte).expect("it serves reads");
    assert_eq!(&first_byte, b"D", "the first's write is on its own stick");
    retire(identity, DetachMode::Plain, back);
    VOLUME_SERVICE
        .detach(
            &VolumeDetachRequest {
                volume_id: key,
                mode: DetachMode::Force,
            },
            &admin(),
        )
        .expect("the audited force discards the second's retained set");
}

/// A stick named `name`, formatted under `serial` and served from slot
/// `slots.0` by a device stating `stated.0`, written so the write is held,
/// and pulled; then its image as it stood before the write put back from slot
/// `slots.1` by a device stating `stated.1`, each over the block-service node
/// `node`. Answers the round serving the second, the volume's key, and that
/// image.
fn pulled_and_put_back(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
    (serial, name): (u32, &'static str),
    slots: (u64, u64),
    stated: (Option<BlkDeviceIdentity>, Option<BlkDeviceIdentity>),
    node: u32,
) -> (Inserted, [u8; 16], Vec<u8>) {
    let (image, identity) = fat32_image(serial, b"held payload");
    let pristine = image.data.clone();
    let first = insertion(slots.0, name.as_bytes(), VolumeFsType::Fat32);
    let pulled = insert_on(
        window_ptr,
        facility,
        (&first, node),
        ServedDevice {
            identity: stated.0,
            ..ServedDevice::new(image)
        },
    );
    pulled.outcome.expect("the stick attaches");
    let path = std::format!("/Storage/{name}/hello.txt");
    write_as(0, &path, 0, b"D").expect("a write the stick takes into its cache");
    tairix_kernel_core::callreg::teardown_owned_by(ids(slots.0).1, &ASPACES, &SINK);
    pulled.server.stop();
    assert_eq!(
        mount_record(name.as_bytes()).availability(),
        MountAvailability::UnavailableDirty
    );
    let second = insertion(slots.1, name.as_bytes(), VolumeFsType::Fat32);
    let back = insert_on(
        window_ptr,
        facility,
        (&second, node),
        ServedDevice {
            identity: stated.1,
            ..ServedDevice::new(RamBlock {
                data: pristine.clone(),
            })
        },
    );
    back.outcome.expect("the medium is attached again");
    (back, identity, pristine)
}

/// A clone of a pulled stick — its content the stick's before the held write —
/// on a device stating another's identity, or on one stating none, is never
/// written unasked: it returns read-only awaiting the answer of the user at
/// the seat, the clone untouched and the write still held. The evidence alone
/// once replayed the write onto the clone.
fn a_cloned_stick_is_never_written_unasked_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    for (stick, slots, stated) in [
        (
            (0x0D15_C180, "clone-other"),
            (0x180, 0x181),
            (UNIT, OTHER_UNIT),
        ),
        ((0x0D15_C182, "clone-none"), (0x182, 0x183), (None, None)),
    ] {
        let name = stick.1;
        let (back, key, pristine) =
            pulled_and_put_back(window_ptr, facility, stick, slots, stated, PUBLISHER);
        assert_eq!(
            mount_record(name.as_bytes()).availability(),
            MountAvailability::ReplayUnconfirmed,
            "{name} awaits the seat's answer"
        );
        assert_eq!(audited(REINSERT_UNCONFIRMED, name), [None]);
        assert!(
            back.device.lock().unwrap().block.data == pristine,
            "nothing was written to {name}"
        );
        let path = std::format!("/Storage/{name}/hello.txt");
        assert_volume_reads(&path, b"held payload");
        assert_eq!(write_as(0, &path, 0, b"X").err(), Some(Errno::ReadOnly));
        retire(key, DetachMode::Force, back);
    }
}

/// What a RAID array states while composed of the devices it was, and once a
/// member is another device.
const ARRAY_MEMBERS: Option<BlkDeviceIdentity> =
    BlkDeviceIdentity::new([0xA7; BLK_DEVICE_IDENTITY_LEN]);
const ARRAY_RECOMPOSED: Option<BlkDeviceIdentity> =
    BlkDeviceIdentity::new([0x7A; BLK_DEVICE_IDENTITY_LEN]);

/// A RAID array whose composer exits with a write held comes back from the
/// restarted composer on an endpoint of its own. Composed of the devices it
/// was, it states the identity it stated, so the write is replayed onto it
/// unasked; with a member replaced it states another, so the write is held for
/// an answer and nothing is written.
fn an_array_its_composer_restarted_is_replayed_on_proof_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let name = "array-same";
    let (same, same_key, _) = pulled_and_put_back(
        window_ptr,
        facility,
        (0x0D15_C194, name),
        (0x194, 0x195),
        (ARRAY_MEMBERS, ARRAY_MEMBERS),
        PUBLISHER,
    );
    assert_eq!(
        mount_record(name.as_bytes()).availability(),
        MountAvailability::Available,
        "the same devices prove the array the medium its write was held for"
    );
    let path = std::format!("/Storage/{name}/hello.txt");
    let mut first_byte = [0u8; 1];
    read_as(0, &path, 0, &mut first_byte).expect("it serves reads");
    assert_eq!(&first_byte, b"D", "the held write is on the array");
    retire(same_key, DetachMode::Plain, same);

    let name = "array-other";
    let (other, other_key, pristine) = pulled_and_put_back(
        window_ptr,
        facility,
        (0x0D15_C196, name),
        (0x196, 0x197),
        (ARRAY_MEMBERS, ARRAY_RECOMPOSED),
        PUBLISHER,
    );
    assert_eq!(
        mount_record(name.as_bytes()).availability(),
        MountAvailability::ReplayUnconfirmed,
        "an array with another member proves nothing"
    );
    assert_eq!(audited(REINSERT_UNCONFIRMED, name), [None]);
    assert!(
        other.device.lock().unwrap().block.data == pristine,
        "nothing was written to it"
    );
    retire(other_key, DetachMode::Force, other);
}

/// The user at the seat answers *Write* for a removable stick: the held write
/// lands on the medium, whose content still proves it unmutated, and the
/// volume is served writable again under its key, the answer audited as the
/// seat's.
fn the_seat_has_the_held_writes_written_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let name = "held-write";
    let (back, key, _) = pulled_and_put_back(
        window_ptr,
        facility,
        (0x0D15_C184, name),
        (0x184, 0x185),
        (None, None),
        ON_REMOVABLE_PORT,
    );
    let write = VolumeReplayRequest {
        volume_id: key,
        answer: ReplayAnswer::Write,
    };
    VOLUME_SERVICE
        .replay(&desktop(), &write)
        .expect("the seat's answer is taken");
    assert_eq!(
        audited(REPLAY_ANSWERED, name),
        [Some(String::from("written"))]
    );
    assert_eq!(
        audited_text(REPLAY_ANSWERED, name, "authority"),
        [Some(String::from("seat"))]
    );
    serve_published(facility, ON_REMOVABLE_PORT);
    assert_eq!(
        mount_record(name.as_bytes()).availability(),
        MountAvailability::Available
    );
    let path = std::format!("/Storage/{name}/hello.txt");
    let mut first_byte = [0u8; 1];
    read_as(0, &path, 0, &mut first_byte).expect("it serves reads");
    assert_eq!(&first_byte, b"D", "the held write is on the medium");
    write_as(0, &path, 0, b"E").expect("it takes writes again");
    retire(key, DetachMode::Plain, back);
}

/// The user at the seat answers *Discard* for a removable stick: nothing held
/// reaches the medium, and the volume is served writable again as the medium
/// stands.
fn the_seat_has_the_held_writes_discarded_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let name = "held-discard";
    let (back, key, pristine) = pulled_and_put_back(
        window_ptr,
        facility,
        (0x0D15_C186, name),
        (0x186, 0x187),
        (None, None),
        ON_REMOVABLE_PORT,
    );
    let discard = VolumeReplayRequest {
        volume_id: key,
        answer: ReplayAnswer::Discard,
    };
    VOLUME_SERVICE
        .replay(&desktop(), &discard)
        .expect("the seat's answer is taken");
    assert_eq!(
        audited(REPLAY_ANSWERED, name),
        [Some(String::from("discarded"))]
    );
    assert!(
        back.device.lock().unwrap().block.data == pristine,
        "nothing held reached the medium"
    );
    serve_published(facility, ON_REMOVABLE_PORT);
    assert_eq!(
        mount_record(name.as_bytes()).availability(),
        MountAvailability::Available
    );
    let path = std::format!("/Storage/{name}/hello.txt");
    assert_volume_reads(&path, b"held payload");
    write_as(0, &path, 0, b"E").expect("it takes writes again");
    retire(key, DetachMode::Plain, back);
}

/// The mount authority answers for a fixed device's volume from wherever it
/// runs, no seat needed: its *Write* lands the held write and serves the volume
/// writable again, audited as the mount authority's. A machine with no seat
/// once had only a forced detach, which discards the writes, to end the hold.
fn the_mount_authority_answers_for_a_fixed_volume_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let name = "held-fixed";
    let (back, key, _) = pulled_and_put_back(
        window_ptr,
        facility,
        (0x0D15_C190, name),
        (0x190, 0x191),
        (None, None),
        PUBLISHER,
    );
    assert_eq!(mount_record(name.as_bytes()).class(), VolumeClass::Local);
    let write = VolumeReplayRequest {
        volume_id: key,
        answer: ReplayAnswer::Write,
    };
    VOLUME_SERVICE
        .replay(&admin(), &write)
        .expect("the mount authority's answer is taken");
    assert_eq!(
        audited(REPLAY_ANSWERED, name),
        [Some(String::from("written"))]
    );
    assert_eq!(
        audited_text(REPLAY_ANSWERED, name, "authority"),
        [Some(String::from("mount_authority"))]
    );
    serve_published(facility, PUBLISHER);
    assert_eq!(
        mount_record(name.as_bytes()).availability(),
        MountAvailability::Available
    );
    let path = std::format!("/Storage/{name}/hello.txt");
    let mut first_byte = [0u8; 1];
    read_as(0, &path, 0, &mut first_byte).expect("it serves reads");
    assert_eq!(&first_byte, b"D", "the held write is on the medium");
    write_as(0, &path, 0, b"E").expect("it takes writes again");
    retire(key, DetachMode::Plain, back);
}

/// The user at the seat answers only for a removable device's volume: an
/// answer for a fixed one is refused and gives nothing up, so a desktop user
/// without the mount authority cannot discard the writes other principals made
/// on an internal disk.
fn the_seat_does_not_answer_for_a_fixed_volume_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let name = "held-internal";
    let (back, key, pristine) = pulled_and_put_back(
        window_ptr,
        facility,
        (0x0D15_C192, name),
        (0x192, 0x193),
        (None, None),
        PUBLISHER,
    );
    let discard = VolumeReplayRequest {
        volume_id: key,
        answer: ReplayAnswer::Discard,
    };
    assert_eq!(
        VOLUME_SERVICE.replay(&desktop(), &discard),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        audited(REPLAY_REFUSED, name),
        [Some(String::from("no_mount_authority"))]
    );
    assert_eq!(
        mount_record(name.as_bytes()).availability(),
        MountAvailability::ReplayUnconfirmed,
        "the writes are still held"
    );
    assert!(
        back.device.lock().unwrap().block.data == pristine,
        "nothing reached the medium"
    );
    retire(key, DetachMode::Force, back);
}

/// An answer is taken only on an authority reaching its volume, and only for a
/// volume awaiting one: a user away from the seat, and a process at it without
/// the seat's own capability, are refused and change nothing, an unknown key
/// is not found, and an answered question is stale to the seat and the mount
/// authority alike.
fn only_an_authority_answers_for_a_volume_awaiting_it_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let name = "held-refuse";
    let (back, key, pristine) = pulled_and_put_back(
        window_ptr,
        facility,
        (0x0D15_C188, name),
        (0x188, 0x189),
        (None, None),
        ON_REMOVABLE_PORT,
    );
    let write = VolumeReplayRequest {
        volume_id: key,
        answer: ReplayAnswer::Write,
    };
    for caller in [member(), seated(member())] {
        assert_eq!(
            VOLUME_SERVICE.replay(&caller, &write),
            Err(Errno::PermissionDenied)
        );
    }
    assert_eq!(
        audited(REPLAY_REFUSED, name),
        [
            Some(String::from("no_mount_authority")),
            Some(String::from("no_mount_authority"))
        ]
    );
    assert_eq!(
        mount_record(name.as_bytes()).availability(),
        MountAvailability::ReplayUnconfirmed,
        "a refused answer writes and gives up nothing"
    );
    assert_eq!(back.device.lock().unwrap().block.data, pristine);
    let unknown = VolumeReplayRequest {
        volume_id: [0x3C; 16],
        ..write
    };
    assert_eq!(
        VOLUME_SERVICE.replay(&desktop(), &unknown),
        Err(Errno::NotFound)
    );
    let discard = VolumeReplayRequest {
        answer: ReplayAnswer::Discard,
        ..write
    };
    VOLUME_SERVICE
        .replay(&desktop(), &discard)
        .expect("the seat answers");
    serve_published(facility, ON_REMOVABLE_PORT);
    for caller in [desktop(), admin()] {
        assert_eq!(
            VOLUME_SERVICE.replay(&caller, &write),
            Err(Errno::Stale),
            "an answered question is not asked again"
        );
    }
    retire(key, DetachMode::Plain, back);
}

/// A medium that changed while it awaited the answer — its evidence no longer
/// what the held writes were made over — is not written at the seat's word:
/// the answer is refused read-only, and the volume is served read-only in
/// conflict, its writes kept for the audited force.
fn a_medium_changed_since_the_question_is_not_written_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let name = "held-changed";
    let (back, key, _) = pulled_and_put_back(
        window_ptr,
        facility,
        (0x0D15_C18D, name),
        (0x18D, 0x18E),
        (None, None),
        ON_REMOVABLE_PORT,
    );
    // The FSInfo free-cluster hint: inside the evidence window.
    let changed = {
        let mut device = back.device.lock().unwrap();
        device.block.data[512 + 488..512 + 492].copy_from_slice(&4321u32.to_le_bytes());
        device.block.data.clone()
    };
    let write = VolumeReplayRequest {
        volume_id: key,
        answer: ReplayAnswer::Write,
    };
    assert_eq!(
        VOLUME_SERVICE.replay(&desktop(), &write),
        Err(Errno::ReadOnly),
        "the held writes are not written to a medium that changed"
    );
    assert_eq!(
        audited(REINSERT_CONFLICT, name),
        [Some(String::from("evidence_mismatch"))]
    );
    assert!(
        back.device.lock().unwrap().block.data == changed,
        "nothing reached the medium"
    );
    serve_published(facility, ON_REMOVABLE_PORT);
    assert_eq!(
        mount_record(name.as_bytes()).availability(),
        MountAvailability::RecoveryConflict
    );
    let path = std::format!("/Storage/{name}/hello.txt");
    assert_eq!(write_as(0, &path, 0, b"X").err(), Some(Errno::ReadOnly));
    retire(key, DetachMode::Force, back);
}

/// The same medium attached again is busy whatever name it asks, no other
/// name helping, and so is any extent meeting one a volume is served over on
/// its device; a twin on an extent of its own on that device attaches beside
/// it.
fn the_same_medium_twice_is_busy_scenario(window_ptr: *mut u8, facility: &'static TestFacility) {
    let (discs, identities) = floppies([AdfsVariant::E, AdfsVariant::E]);
    let discs = Device::up(window_ptr, facility, 0x178, discs);
    pending(&discs.floppy(0, b"twin-same")).expect("the disc attaches");
    for name in [&b"twin-same"[..], b"twin-same-again"] {
        assert_eq!(
            pending(&discs.floppy(0, name)),
            Err(Errno::Busy),
            "{name:?}"
        );
    }
    let straddling = discs.request(
        (FLOPPY_BLOCKS / 2, FLOPPY_BLOCKS),
        VolumeFsType::Adfs,
        b"twin-half",
    );
    assert_eq!(
        pending(&straddling),
        Err(Errno::Busy),
        "an extent meeting it is that medium too"
    );
    assert_eq!(
        audited(ATTACH_REFUSED, "twin-same"),
        [Some(String::from("volume_attached"))]
    );
    pending(&discs.floppy(1, b"twin-same")).expect("its twin on the device attaches");
    assert_eq!(names_on(discs.endpoint), ["twin-same", "twin-same_2"]);
    let keys: Vec<[u8; 16]> = attached_on(discs.endpoint)
        .into_iter()
        .map(|(_, _, key)| key)
        .collect();
    assert_eq!(keys[0], identities[0]);
    assert_ne!(keys[1], identities[1], "the twin is keyed apart");
    discs.down();
}

/// An identity is attached at most `VOLUME_TWINS_MAX` times at once, its twins
/// named from `_2` up; the next is refused as it enters and leaves no served
/// volume behind.
fn an_identitys_attachments_are_bounded_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let bound = u64::from(VOLUME_TWINS_MAX);
    let (discs, _) = floppies((0..=bound).map(|_| AdfsVariant::E));
    let discs = Device::up(window_ptr, facility, 0x179, discs);
    for at in 0..bound {
        pending(&discs.floppy(at, b"twin-limit")).expect("a twin attaches");
    }
    let served = VOLUME_SERVICE.served.lock().len();
    assert_eq!(
        pending(&discs.floppy(bound, b"twin-limit")),
        Err(Errno::LimitExceeded)
    );
    assert_eq!(
        audited(ATTACH_REFUSED, "twin-limit"),
        [Some(String::from("twins_full"))]
    );
    assert_eq!(
        VOLUME_SERVICE.served.lock().len(),
        served,
        "the refused twin leaves no served volume behind"
    );
    let mut expected = vec![String::from("twin-limit")];
    for ordinal in 2..=bound {
        expected.push(std::format!("twin-limit_{ordinal}"));
    }
    assert_eq!(names_on(discs.endpoint), expected);
    discs.down();
}

/// Twin names hold against hostile names: one already ending in `_2` takes a
/// further `_2`, a twin name another identity holds is passed over, a name
/// another identity holds is still answered taken, and a stem at the bound is
/// cut, never to end in a separator.
fn twin_names_hold_against_hostile_names_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (discs, _) = floppies(core::iter::repeat_n(AdfsVariant::E, 6));
    let discs = Device::up(window_ptr, facility, 0x17D, discs);
    let other = ext4_image([0x7A; 16]);
    let extent = u64::try_from(other.data.len() / BLOCK_SIZE).expect("small");
    let others = Device::up(
        window_ptr,
        facility,
        0x17E,
        RamBlock {
            data: other.data.repeat(2),
        },
    );
    let ext4 = |at: u64, name: &'static [u8]| {
        others.request((at * extent, extent), VolumeFsType::Ext4, name)
    };
    let long = &b"twinaaaaaaaaaaaaaaaaaaaaaaaaa-bc"[..];
    for (at, name) in [(0, &b"twin-dup_2"[..]), (1, b"twin-dup_2")] {
        pending(&discs.floppy(at, name)).expect("a disc attaches");
    }
    pending(&ext4(0, b"twin-lone_2")).expect("another identity takes a twin's spelling");
    for at in [2, 3] {
        pending(&discs.floppy(at, b"twin-lone")).expect("a disc attaches");
    }
    assert_eq!(
        pending(&ext4(1, b"twin-lone")),
        Err(Errno::AlreadyExists),
        "a name another identity holds is answered taken"
    );
    assert_eq!(audited(NAME_TAKEN, "twin-lone"), [None]);
    for at in [4, 5] {
        pending(&discs.floppy(at, long)).expect("a disc attaches");
    }
    assert_eq!(
        names_on(discs.endpoint),
        [
            "twin-dup_2",
            "twin-dup_2_2",
            "twin-lone",
            "twin-lone_3",
            "twinaaaaaaaaaaaaaaaaaaaaaaaaa-bc",
            "twinaaaaaaaaaaaaaaaaaaaaaaaaa_2",
        ]
    );
    discs.down();
    others.down();
}
