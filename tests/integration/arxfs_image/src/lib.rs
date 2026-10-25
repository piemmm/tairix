//! Single-source-of-truth arxfs disk-image fixture shared by the
//! end-to-end QEMU arxfs-over-virtio_blk vertical.
//!
//! Unlike the hand-built FAT32 fixture, this image is laid down by the
//! **real** arxfs driver: [`build_image`] formats an in-memory volume
//! through [`ARXFS::format`](tairix_drv_fs_arxfs::ARXFS::format), plants
//! [`PLANTED_FILE_NAME`] / [`PLANTED_FILE_CONTENT`] through the driver's
//! own write path, and returns the resulting bytes. The on-disk layout
//! therefore has exactly one author — the driver — so the fixture and the
//! driver can never drift.
//!
//! The host harness (`tools/xtask`) plants those bytes on the test's
//! backing disk before the guest boots. The freestanding guest tail
//! (`tests/integration/virtio_qemu_support`) mounts that very volume
//! through the real arxfs driver, verifies the planted file, then
//! creates and writes a fresh file and reads it back. Both sides name the
//! same fixed files through the constants below, so the on-disk contract
//! lives in exactly one place.
//!
//! The image is a genuine arxfs volume — 1 MiB, 512-byte blocks, 64
//! inodes — laid out so the real `ARXFS::open` validator accepts it. It
//! is `no_std` + `alloc` so it links into both the host build tool and
//! the freestanding guest test.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::driver::filesystem::{FilesystemRead, FilesystemWrite, NodeId, NodeKind};
use tairix_abi::home::HOME_FILES_DIR;
use tairix_abi::DriverError;
use tairix_drv_fs_arxfs::{EntropySource, Keying, Security, VolumeKey, ARXFS, VOLUME_KEY_LEN};
use tairix_users::{
    AccountState, Gid, GroupRecord, GroupsDb, Identity, ParseError, Salt, Uid, UserRecord, UsersDb,
};

/// The record the fixture's own skeleton is laid with: the system user's.
const SYSTEM_DIR: Security = Security::created(NodeKind::Directory, 0, 0);

/// The record of a file the fixture plants for the system.
const SYSTEM_FILE: Security = Security::created(NodeKind::RegularFile, 0, 0);

/// Logical block (sector) size of the produced image, in bytes. Matches
/// both the 512-byte sector QEMU's virtio-blk reports by default and the
/// arxfs minimum block size, so the volume the driver formats here maps
/// directly onto the device the guest mounts.
pub const SECTOR_BYTES: usize = 512;

/// Total size of the produced image, in 512-byte sectors (1 MiB). Large
/// enough for the inode table, bitmap, journal, and a non-trivial data
/// region, matching the FAT32 fixture's footprint.
pub const TOTAL_SECTORS: u64 = 2048;

/// Number of inodes the volume is formatted with. Two-per-block at the
/// 512-byte block size, comfortably more than the root plus the planted
/// and written files need.
const INODE_COUNT: u32 = 64;

/// Volume key the encrypted fixtures are formatted and mounted with: one
/// shared key for the host builder and the guest tail.
pub const FIXTURE_VOLUME_KEY: VolumeKey = [0x5a; VOLUME_KEY_LEN];

/// How the encrypted fixtures are formatted and mounted.
pub const FIXTURE_KEYING: Keying<'static> = Keying::Key(&FIXTURE_VOLUME_KEY);

/// Deterministic stand-in for the platform RNG seam used to provision the
/// fixture volume. A fixed sequence keeps the built image **reproducible**; it is fixture scaffolding, never a production source.
struct FixtureEntropy {
    next: u8,
}

impl EntropySource for FixtureEntropy {
    fn fill(&mut self, out: &mut [u8]) -> Result<(), DriverError> {
        for byte in out.iter_mut() {
            *byte = self.next;
            self.next = self.next.wrapping_add(1);
        }
        Ok(())
    }
}

/// File planted in the root directory before boot. The guest tail looks
/// it up and verifies [`PLANTED_FILE_CONTENT`].
pub const PLANTED_FILE_NAME: &[u8] = b"hello.txt";

/// Contents of [`PLANTED_FILE_NAME`].
pub const PLANTED_FILE_CONTENT: &[u8] = b"Hello from a planted arxfs volume on virtio-blk.\n";

/// File the guest tail creates and writes after mounting.
pub const NEW_FILE_NAME: &[u8] = b"written.txt";

/// Contents the guest tail writes to [`NEW_FILE_NAME`] and reads back.
pub const NEW_FILE_CONTENT: &[u8] = b"TAIRiX wrote this file to arxfs over virtio-blk.\n";

/// A document planted among the fixture account's own files
/// (`/Users/root/Home`) on the users-root volume, so the desktop
/// session's trusted file picker — which opens there — has a real regular
/// file to choose. Choosing it
/// drives the CU6 one-shot `fd_grant`/`fd_redeem` delegation into `view`
/// (`plans/NEW-FILEMANAGER.md` FM9-b).
pub const HOME_DOC_NAME: &[u8] = b"Welcome.txt";

/// Contents of [`HOME_DOC_NAME`], read by `view` once the picked file's
/// delegated descriptor is redeemed. A picture viewer states why it cannot
/// draw a text document rather than blanking, so the delegation the vertical
/// is about is what the read proves, not a rendered page.
pub const HOME_DOC_CONTENT: &[u8] =
    b"Welcome to TAIRiX.\nThis document was opened through the trusted file picker.\n";

/// A picture planted beside [`HOME_DOC_NAME`] among the fixture account's own
/// files, so a gesture in the **file manager's** window has a document whose type an
/// installed application claims. `view` declares `image/svg+xml`, so
/// activating this resolves to that bundle and drives the three-principal
/// hand-over — the manager mints the delegation, the session relays it, and
/// the already-running viewer redeems it (`plans/VIEW.md`).
///
/// Vector rather than raster because the whole document is then legible here:
/// a format the viewer draws completely, written out in full, with no encoder
/// or binary blob between the fixture and what the guest reads.
pub const HOME_PICTURE_NAME: &[u8] = b"Picture.svg";

/// Contents of [`HOME_PICTURE_NAME`]: one filled square over the whole
/// viewBox, the shape `lib/sandbox`'s own rasterisation tests use, so the
/// document the guest decodes is known-good input for the desktop's SVG
/// subset.
pub const HOME_PICTURE_CONTENT: &[u8] =
    br##"<svg viewBox="0 0 10 10"><polygon points="0,0 10,0 10,10 0,10" fill="#3070f0"/></svg>"##;

/// Username of the interactive account planted on the users-root volume
/// ([`build_users_root_image`]) on top of the canonical default
/// system/service set.
pub const USERS_FIXTURE_USERNAME: &str = "root";

/// Password of the planted [`USERS_FIXTURE_USERNAME`] account.
pub const USERS_FIXTURE_PASSWORD: &str = "root";

/// PBKDF2 cost of the planted account's password record: the format's
/// floor, so the guest-side authentication proof stays fast under QEMU
/// TCG. Fixture scaffolding only — a real database uses
/// [`tairix_users::DEFAULT_ITERATIONS`].
pub const USERS_FIXTURE_ITERATIONS: u32 = tairix_users::MIN_ITERATIONS;

/// Fixed salt of the planted account's password record, keeping the
/// built image reproducible.
const USERS_FIXTURE_SALT: Salt = [0xa5; tairix_users::SALT_LEN];

/// Serialise the users-root volume's `/System/Security/Users` database:
/// the active [`USERS_FIXTURE_USERNAME`] account (uid
/// [`tairix_users::FIRST_USER_UID`]) granted the shared administrator
/// capability ceiling (`tairix_users::administrator_ceiling` — the session
/// baseline plus the administrative set), exactly as the real debug
/// image's `tools/mkimage` seeding lays it down, so the end-to-end session
/// vertical exercises the same grant the shipped debug profile carries
/// (`plans/CAPABILITY_USE.md` CU3). The on-disk database holds **human**
/// accounts only: the system/service identity is compiled into the kernel
/// (`tairix_users::system_accounts`, `plans/USERS.md`) and the kernel's
/// identity merge refuses any on-disk record colliding with it.
///
/// # Errors
///
/// Propagates the [`ParseError`] if a fixture constant violates the
/// `users-v1` bounds — a programming error in this fixture, surfaced
/// rather than panicked.
pub fn users_db_text() -> Result<String, ParseError> {
    let records = vec![UserRecord::with_password(
        Identity {
            username: USERS_FIXTURE_USERNAME,
            uid: Uid(tairix_users::FIRST_USER_UID),
            primary_gid: Gid(tairix_users::FIRST_USER_GID),
            supplementary_gids: &[tairix_users::STORAGE_GID],
            display_name: "System Administrator",
            home: Some("/Users/root"),
            shell: Some("/System/Commands/elsh.app/Run"),
            capabilities: tairix_users::administrator_ceiling(),
            state: AccountState::Active,
        },
        USERS_FIXTURE_PASSWORD.as_bytes(),
        USERS_FIXTURE_SALT,
        USERS_FIXTURE_ITERATIONS,
    )?];
    Ok(UsersDb::new(records)?.serialise())
}

/// Serialise the users-root volume's `/System/Security/Groups` registry:
/// the well-known removable-storage group (whose by-name resolution arms
/// the hotplug-volume identity map, `plans/DEVICES.md` D3d) plus the
/// `wheel` group (gid [`tairix_users::FIRST_USER_GID`]) the planted
/// [`USERS_FIXTURE_USERNAME`] account names as its primary group — so the
/// kernel's identity merge (`tairix_kernel_core::build_identity_table`)
/// resolves every on-disk account's gid against a real registry rather
/// than failing closed on a dangling reference — exactly as
/// `tools/mkimage` seeds the shipped debug profile. The `system` and
/// `services` groups are compiled into the kernel beside the system
/// accounts and never written to disk.
///
/// # Errors
///
/// Propagates the [`ParseError`] if a fixture constant violates the
/// `groups-v1` bounds — a programming error in this fixture, surfaced
/// rather than panicked.
pub fn groups_db_text() -> Result<String, ParseError> {
    let records = vec![
        GroupRecord::new(tairix_users::STORAGE_GROUP, tairix_users::STORAGE_GID)?,
        GroupRecord::new("wheel", Gid(tairix_users::FIRST_USER_GID))?,
    ];
    Ok(GroupsDb::new(records)?.serialise())
}

/// In-memory [`Block`] device backing the fixture build and the host
/// round-trip tests. It addresses [`SECTOR_BYTES`]-byte sectors exactly
/// as the guest's virtio-blk device does.
pub struct VecBlock {
    store: Vec<u8>,
}

impl VecBlock {
    /// A zeroed device of `sectors` sectors.
    fn new(sectors: u64) -> Self {
        let len = usize::try_from(sectors).unwrap_or(0) * SECTOR_BYTES;
        Self {
            store: vec![0u8; len],
        }
    }

    /// Wrap an already-laid-out image (e.g. the bytes returned by
    /// [`build_users_root_image_keyed`]) as a mountable device, so a
    /// consumer can re-open it through the real `ARXFS::open` without
    /// re-deriving the on-disk layout (one block-device
    /// double, shared by the fixture and its consumers).
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self { store: bytes }
    }

    /// Byte span `[start, end)` for `len` bytes at sector `lba`, or an
    /// error if the access is unaligned or out of range.
    fn span(&self, lba: u64, len: usize) -> Result<(usize, usize), DriverError> {
        if len == 0 || !len.is_multiple_of(SECTOR_BYTES) {
            return Err(DriverError::BufferTooSmall);
        }
        let start = usize::try_from(lba)
            .ok()
            .and_then(|l| l.checked_mul(SECTOR_BYTES))
            .ok_or(DriverError::LengthOutOfRange)?;
        let end = start
            .checked_add(len)
            .ok_or(DriverError::LengthOutOfRange)?;
        if end > self.store.len() {
            return Err(DriverError::LengthOutOfRange);
        }
        Ok((start, end))
    }
}

impl Block for VecBlock {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: u32::try_from(SECTOR_BYTES).unwrap_or(0),
            block_count: TOTAL_SECTORS,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let (start, end) = self.span(lba, buf.len())?;
        buf.copy_from_slice(&self.store[start..end]);
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        let (start, end) = self.span(lba, buf.len())?;
        self.store[start..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

/// Build the arxfs image described in the module docs by driving the
/// real arxfs driver: format a fresh in-memory volume, plant
/// [`PLANTED_FILE_NAME`] with [`PLANTED_FILE_CONTENT`], flush, and return
/// the resulting on-disk bytes.
///
/// # Errors
///
/// Propagates any [`DriverError`] from the driver. The fixed geometry and
/// payload sizes make a failure a programming error in this fixture, but
/// the result is surfaced rather than panicked so the builder holds to
/// in every path it links into.
pub fn build_image() -> Result<Vec<u8>, DriverError> {
    let dev = VecBlock::new(TOTAL_SECTORS);
    let mut fs = ARXFS::format(
        dev,
        INODE_COUNT,
        FIXTURE_KEYING,
        &mut FixtureEntropy { next: 1 },
    )?;
    let root = fs.root();
    let planted = fs.create(root, PLANTED_FILE_NAME, NodeKind::RegularFile, &SYSTEM_FILE)?;
    let written = fs.write_at(planted, 0, PLANTED_FILE_CONTENT)?;
    if written != PLANTED_FILE_CONTENT.len() {
        return Err(DriverError::DeviceFault);
    }
    fs.flush()?;
    Ok(fs.into_block()?.into_bytes())
}

/// Build the users-root volume: a arxfs image carrying the top-level directories with `/System/Security/Users` holding the
/// [`users_db_text`] database — the on-disk shape the production root
/// volume gives the kernel's boot-time users-database load
/// (`tairix_kernel_core::users`, `plans/PI.md` P11).
///
/// The volume is keyed by the same [`FIXTURE_VOLUME_KEY`] and geometry as
/// [`build_image`]; only the planted tree differs.
///
/// # Errors
///
/// Propagates any [`DriverError`] from the driver; a fixture users
/// database that violates the `users-v1` bounds surfaces as
/// [`DriverError::Unsupported`] (a programming error in this fixture,
/// surfaced rather than panicked).
pub fn build_users_root_image() -> Result<Vec<u8>, DriverError> {
    build_users_root_image_keyed(FIXTURE_KEYING, &[])
}

/// A shape refusal as the driver error this fixture reports.
fn shape_error(err: tairix_users::ShapeError) -> DriverError {
    match err {
        tairix_users::ShapeError::Driver(err) => err,
        tairix_users::ShapeError::Occupied => DriverError::Unsupported,
    }
}

/// Plant the regular file `name` holding `content` in `dir`, a folder of the
/// account's home.
///
/// Owned by the account and world-unreadable-but-owner-readable (0644 under
/// the owner-only home), so only a process running as the user reaches it —
/// exactly as a user's own document is, which is what makes a file reaching a
/// capability-less viewer proof of a delegation rather than of ambient
/// access.
fn plant_home_file(
    fs: &mut ARXFS<VecBlock>,
    dir: NodeId,
    name: &[u8],
    content: &[u8],
) -> Result<(), DriverError> {
    let record = Security::new(
        0o644,
        tairix_users::FIRST_USER_UID,
        tairix_users::FIRST_USER_GID,
    );
    let node = fs.create(dir, name, NodeKind::RegularFile, &record)?;
    if fs.write_at(node, 0, content)? != content.len() {
        return Err(DriverError::DeviceFault);
    }
    Ok(())
}

/// Build the users-root volume under `keying` — the same layout as
/// [`build_users_root_image`] but protected as the caller asks, so a consumer
/// can exercise the production root mount (`plans/PI.md` P11;
/// `kernel/tairix-kernel::root_mount`) on a passphrase-derived key or on a
/// plaintext root, against a real on-disk volume.
/// [`build_users_root_image`] delegates here with [`FIXTURE_KEYING`] (one
/// authoring path).
///
/// `root_files` are extra documents planted on this volume, each
/// `(path_components, bytes)` relative to the volume root with every
/// intermediate directory created ([`plant_nested_file`]) — how a vertical
/// lays root-volume state the writable `/System/Settings` child mount
/// rebases onto (e.g. the seeded program-library catalog,
/// `plans/NEW-TASKBAR.md` T3), exactly where `tools/mkimage` writes it on
/// a shipped image.
///
/// # Errors
///
/// Propagates any [`DriverError`] from the driver; a fixture users
/// database that violates the `users-v1` bounds surfaces as
/// [`DriverError::Unsupported`] (a programming error in this fixture,
/// surfaced rather than panicked).
pub fn build_users_root_image_keyed(
    keying: Keying<'_>,
    root_files: &[(&[&[u8]], &[u8])],
) -> Result<Vec<u8>, DriverError> {
    let text = users_db_text().map_err(|_| DriverError::Unsupported)?;
    let groups_text = groups_db_text().map_err(|_| DriverError::Unsupported)?;
    let dev = VecBlock::new(TOTAL_SECTORS);
    let mut fs = ARXFS::format(dev, INODE_COUNT, keying, &mut FixtureEntropy { next: 1 })?;
    let root = fs.root();
    for name in tairix_abi::stored_root_entries() {
        let node = fs.create(root, name.as_bytes(), NodeKind::Directory, &SYSTEM_DIR)?;
        if name == "System" {
            let settings = fs.create(node, b"Settings", NodeKind::Directory, &SYSTEM_DIR)?;
            tairix_users::provision_settings_dirs(&mut fs, settings).map_err(shape_error)?;
        }
        if name == "Users" {
            // The planted account's recorded home directory with the fixed
            // home shape, through the one walk the real provisioning path
            // takes, so a fixture home is not a shape no installed system
            // would ever have.
            let home = fs.create(
                node,
                b"root",
                NodeKind::Directory,
                &Security::new(
                    tairix_users::HOME_MODE,
                    tairix_users::FIRST_USER_UID,
                    tairix_users::FIRST_USER_GID,
                ),
            )?;
            tairix_users::provision_home_shape(
                &mut fs,
                home,
                tairix_users::FIRST_USER_UID,
                tairix_users::FIRST_USER_GID,
            )
            .map_err(shape_error)?;
            // Two readable documents among the account's own files, where a
            // bare file-manager window and the trusted picker both open, so
            // both user-mediated routes to a file have something real to
            // reach: the picker shows a regular file to choose, and the file
            // manager's window has a picture whose type an installed
            // application claims.
            let files = fs.lookup(home, HOME_FILES_DIR.as_bytes())?;
            plant_home_file(&mut fs, files, HOME_DOC_NAME, HOME_DOC_CONTENT)?;
            plant_home_file(&mut fs, files, HOME_PICTURE_NAME, HOME_PICTURE_CONTENT)?;
        }
        if name == "System" {
            let security = fs.create(node, b"Security", NodeKind::Directory, &SYSTEM_DIR)?;
            let users = fs.create(security, b"Users", NodeKind::RegularFile, &SYSTEM_FILE)?;
            let written = fs.write_at(users, 0, text.as_bytes())?;
            if written != text.len() {
                return Err(DriverError::DeviceFault);
            }
            // The group registry the kernel identity-table build resolves
            // the planted account's primary gid against; without it the
            // build fails closed on the dangling gid 0 reference.
            let groups = fs.create(security, b"Groups", NodeKind::RegularFile, &SYSTEM_FILE)?;
            let written = fs.write_at(groups, 0, groups_text.as_bytes())?;
            if written != groups_text.len() {
                return Err(DriverError::DeviceFault);
            }
        }
    }
    for (components, bytes) in root_files {
        plant_nested_file(&mut fs, root, components, bytes)?;
        own_settings_document(&mut fs, root, components)?;
    }
    fs.flush()?;
    Ok(fs.into_block()?.into_bytes())
}

/// Give a document planted in a service's `/System/Settings` directory to
/// that service, as it would own one it wrote itself.
fn own_settings_document(
    fs: &mut ARXFS<VecBlock>,
    root: NodeId,
    components: &[&[u8]],
) -> Result<(), DriverError> {
    let [system, settings, dir, file] = components else {
        return Ok(());
    };
    let Some(owned) = tairix_users::SETTINGS_DIRS
        .iter()
        .find(|owned| owned.name().as_bytes() == *dir)
    else {
        return Ok(());
    };
    if (*system, *settings) != (&b"System"[..], &b"Settings"[..]) {
        return Ok(());
    }
    let node = fs.lookup(root, system)?;
    let node = fs.lookup(node, settings)?;
    let node = fs.lookup(node, dir)?;
    let node = fs.lookup(node, file)?;
    fs.set_security(node, owned.document_security())
}

/// Re-export of the single store-planting helper. The
/// definition lives in the arxfs driver (`tairix_drv_fs_arxfs`) so the
/// image builder (`tools/mkimage`) and these fixtures share one routine that
/// gives the autoload scan an identical on-disk shape.
pub use tairix_drv_fs_arxfs::plant_nested_file;

impl VecBlock {
    /// Consume the device, yielding its raw image bytes.
    fn into_bytes(self) -> Vec<u8> {
        self.store
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mount() -> ARXFS<VecBlock> {
        let bytes = build_image().expect("the fixture builds a valid arxfs volume");
        let dev = VecBlock { store: bytes };
        ARXFS::open(dev, FIXTURE_KEYING).expect("the built image is a valid arxfs volume")
    }

    #[test]
    fn a_document_planted_in_a_service_s_settings_belongs_to_that_service() {
        use tairix_abi::driver::filesystem::FilesystemSecurity;
        use tairix_abi::driver_store::SystemConfigFile;

        let components: [&[u8]; 4] = [b"System", b"Settings", b"Configuration", b"system.conf"];
        let bytes =
            build_users_root_image_keyed(FIXTURE_KEYING, &[(&components, b"time.network off\n")])
                .expect("the image builds");
        let mut fs = ARXFS::open(VecBlock { store: bytes }, FIXTURE_KEYING).expect("mounts");
        let mut node = fs.root();
        for component in components {
            node = fs.lookup(node, component).expect("planted");
        }
        let found = fs.security(node).expect("a record");
        let wanted = tairix_users::document_dir(SystemConfigFile::System).document_security();
        assert_eq!(
            (found.mode & 0o7777, found.uid, found.gid),
            (wanted.mode, wanted.uid, wanted.gid)
        );
    }

    #[test]
    fn image_is_exactly_the_advertised_size() {
        let bytes = build_image().expect("build image");
        let expected =
            usize::try_from(TOTAL_SECTORS).expect("sector count fits usize") * SECTOR_BYTES;
        assert_eq!(bytes.len(), expected);
    }

    #[test]
    fn driver_mounts_the_built_image() {
        let _fs = mount();
    }

    #[test]
    fn planted_file_reads_back_its_known_contents() {
        let mut fs = mount();
        let root = fs.root();
        let node = fs.lookup(root, PLANTED_FILE_NAME).expect("planted present");
        let mut buf = [0u8; 128];
        let n = fs.read_at(node, 0, &mut buf).expect("read planted file");
        assert_eq!(&buf[..n], PLANTED_FILE_CONTENT);
    }

    #[test]
    fn a_fresh_file_round_trips_through_create_write_and_read() {
        let mut fs = mount();
        let root = fs.root();
        let file = fs
            .create(root, NEW_FILE_NAME, NodeKind::RegularFile, &SYSTEM_FILE)
            .expect("create new file");
        let written = fs
            .write_at(file, 0, NEW_FILE_CONTENT)
            .expect("write new file");
        assert_eq!(written, NEW_FILE_CONTENT.len());

        let node = fs.lookup(root, NEW_FILE_NAME).expect("new file present");
        let mut buf = [0u8; 128];
        let n = fs.read_at(node, 0, &mut buf).expect("read new file");
        assert_eq!(&buf[..n], NEW_FILE_CONTENT);
    }

    /// No-op audit sink: the round-trip test asserts behaviour through
    /// the returned database, not the audit stream (the audit records
    /// are covered by kernel/core's own loader tests).
    struct DiscardSink;

    impl tairix_log::Sink for DiscardSink {
        fn write_event(&self, _event: &tairix_log::Event<'_>) {}
    }

    #[test]
    fn users_root_image_mounts_and_the_kernel_loader_reads_the_database() {
        use tairix_abi::driver::filesystem::FilesystemSecurity;

        let bytes = build_users_root_image().expect("users-root image builds");
        let dev = VecBlock { store: bytes };
        let mut fs = ARXFS::open(dev, FIXTURE_KEYING).expect("users-root image is a valid volume");

        let sink = DiscardSink;
        let db = tairix_kernel_core::load_users_db(&mut fs, &sink)
            .expect("the kernel loader reads /System/Security/Users");
        // The one interactive human account and nothing else: the
        // system/service identity is compiled into the kernel
        // (`tairix_users::system_accounts`), never seeded to disk, and the
        // identity merge would refuse any colliding record here.
        assert_eq!(db.records().len(), 1);
        for account in tairix_users::system_accounts().expect("valid compiled identity") {
            assert!(db.lookup(account.username()).is_none());
        }

        let record = db
            .authenticate(USERS_FIXTURE_USERNAME, USERS_FIXTURE_PASSWORD.as_bytes())
            .expect("the planted account authenticates");
        assert_eq!(record.username(), USERS_FIXTURE_USERNAME);
        assert_eq!(record.uid(), Uid(tairix_users::FIRST_USER_UID));
        // The planted grant round-trips as exactly the shared administrator
        // ceiling — the same set the debug image seeds — so the end-to-end
        // session vertical exercises the real CU3 grant.
        assert_eq!(record.capabilities(), tairix_users::administrator_ceiling());

        db.authenticate(USERS_FIXTURE_USERNAME, b"wrong password")
            .expect_err("a wrong password is refused");

        // The account's recorded home directory exists on the volume,
        // owned by the account and owner-only.
        let users = fs.lookup(fs.root(), b"Users").expect("/Users present");
        let home = fs.lookup(users, b"root").expect("/Users/root present");
        let sec = fs.security(home).expect("home security present");
        assert_eq!(sec.mode, 0o700);
        assert_eq!(sec.uid, tairix_users::FIRST_USER_UID);
        assert_eq!(sec.gid, tairix_users::FIRST_USER_GID);

        // The account's own files hold their fixed folders and, beside them,
        // both planted documents, owner-readable and reading back their known
        // contents: the text one the trusted picker delegates into the viewer,
        // and the picture an installed application claims so the file
        // manager's own activation has somewhere to hand it.
        let files = fs
            .lookup(home, HOME_FILES_DIR.as_bytes())
            .expect("/Users/root/Home present");
        for folder in tairix_abi::home::HOME_FILES_SUBDIRS {
            fs.lookup(files, folder.as_bytes())
                .unwrap_or_else(|_| panic!("/Users/root/Home/{folder} present"));
        }
        for (name, content) in [
            (HOME_DOC_NAME, HOME_DOC_CONTENT),
            (HOME_PICTURE_NAME, HOME_PICTURE_CONTENT),
        ] {
            let spelling = core::str::from_utf8(name).expect("a planted name is UTF-8");
            let node = fs
                .lookup(files, name)
                .unwrap_or_else(|_| panic!("/Users/root/Home/{spelling} present"));
            let sec = fs.security(node).expect("document security present");
            assert_eq!(sec.mode, 0o644, "{spelling}");
            assert_eq!(sec.uid, tairix_users::FIRST_USER_UID, "{spelling}");
            assert_eq!(sec.gid, tairix_users::FIRST_USER_GID, "{spelling}");
            let mut buf = [0u8; 128];
            let n = fs.read_at(node, 0, &mut buf).expect("read home document");
            assert_eq!(&buf[..n], content, "{spelling}");
        }
    }

    #[test]
    fn plant_nested_file_lays_a_bundle_and_creates_intermediate_directories() {
        // The shared store-planting helper (`plant_nested_file`): a
        // driver bundle laid at the design-B `/System` volume's
        // `Drivers/input/virtio_kbd/Run` is created with every intermediate
        // directory and reads back byte-for-byte off the mounted volume — the
        // on-disk shape the autoload store scan walks. Driver bundles live on the `/System` volume under design B,
        // so the path is relative to that volume's root (no `System` prefix).
        let bundle: &[u8] = b"a-signed-rxe-bundle-stand-in";
        let path: &[&[u8]] = &[b"Drivers", b"input", b"virtio_kbd", b"Run"];

        let dev = VecBlock::new(TOTAL_SECTORS);
        let mut entropy = FixtureEntropy { next: 1 };
        let mut fs = ARXFS::format(dev, INODE_COUNT, FIXTURE_KEYING, &mut entropy)
            .expect("a fresh volume formats");
        let root = fs.root();
        plant_nested_file(&mut fs, root, path, bundle).expect("the bundle plants");

        let mut node = fs.root();
        for dir in [b"Drivers".as_slice(), b"input", b"virtio_kbd"] {
            node = fs.lookup(node, dir).expect("store directory present");
        }
        let run = fs.lookup(node, b"Run").expect("the bundle leaf file");
        let mut buf = [0u8; 64];
        let n = fs.read_at(run, 0, &mut buf).expect("read the bundle bytes");
        assert_eq!(&buf[..n], bundle);
    }

    #[test]
    fn the_planted_file_survives_a_second_file_being_written() {
        let mut fs = mount();
        let root = fs.root();
        let file = fs
            .create(root, NEW_FILE_NAME, NodeKind::RegularFile, &SYSTEM_FILE)
            .expect("create new file");
        fs.write_at(file, 0, NEW_FILE_CONTENT)
            .expect("write new file");

        let node = fs.lookup(root, PLANTED_FILE_NAME).expect("planted present");
        let mut buf = [0u8; 128];
        let n = fs.read_at(node, 0, &mut buf).expect("read planted file");
        assert_eq!(&buf[..n], PLANTED_FILE_CONTENT);
    }
}
