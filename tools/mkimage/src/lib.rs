//! Platform image builders for TAIRiX.
//!
//! `tairix-mkimage` authors flashable images in pure Rust: the partition
//! contents are laid down by the **real** in-tree filesystem drivers — the
//! same code the booted system mounts the volumes with — so the image
//! author and its consumer can never drift. There is no
//! shelling out to `parted`/`mkfs`/`xorriso`.
//!
//! ## `images/tairix-aarch64-rpi.img` (`plans/PI.md` P9)
//!
//! [`build_rpi_image`] assembles the flashable Raspberry Pi 4 SD image:
//!
//! - **MBR** ([`tairix_syshelp::assemble_disk`]): three primary partitions,
//!   all 1 MiB-aligned and back to back.
//! - **Boot partition** ([`fatboot`], FAT32, [`BOOT_PART_SECTORS`]): the
//!   pinned third-party firmware blobs ([`firmware`]),
//!   the generated `config.txt`, and `kernel8.img` — the freestanding
//!   aarch64 `tairix-kernel` ELF flattened by [`elfflat`].
//! - **`/System` partition** ([`rootfs::build_system_partition`], read-only
//!   `ARXFS`): the signed driver and program bundles and the discovered
//!   system payload, in whole 32 MiB grains and at most one grain more than
//!   they need ([`tairix_syshelp::build_system_volume`]).
//! - **Root partition** ([`rootfs`], `ARXFS`, [`ROOT_PART_SECTORS`]): the
//!   writable volume carrying the directory skeleton, plaintext by default
//!   ([`VolumeProtection`]) so the image boots with no passphrase; whether to
//!   encrypt the system disk is the installer's question to the user. A
//!   debug image may instead be built with an encrypted root, whose volume
//!   key is **derived from [`DEBUG_PASSPHRASE`]**: the build provisions an
//!   [`UnlockDescriptor`] (a per-volume random salt + PBKDF2 iteration
//!   count), derives the key under it, provisions the root with that key,
//!   and lays the plaintext descriptor on the boot partition
//!   ([`fatboot::ROOT_UNLOCK_NAME`]) so the bootstrap can re-derive the key
//!   before mounting. The passphrase itself is never stored in the image.
//!
//! Two [`ImageProfile`]s exist, and both seed **human accounts only**: the
//! system/service identity is compiled into the kernel
//! (`tairix_users::system_accounts` / `system_groups`, `plans/USERS.md`)
//! and never written to disk. **Installer** is the shippable form: it
//! seeds an *empty* users database (the installer authors the first human
//! user on first boot), so a fresh install boots straight into the
//! installer. **Debug** is the development form: it seeds an interactive
//! `root`/`root` administrator ([`DEBUG_USERNAME`]/[`DEBUG_PASSWORD`], uid
//! [`DEBUG_UID`], salted and hashed per build), so the login prompt is
//! usable without running the installer. A debug image must never ship.
//!
//! The builder is driven by `cargo xtask image --target aarch64-rpi` (or
//! `cargo xtask build --target aarch64-rpi`) and by the `tairix-mkimage`
//! binary directly; see `docs/src/install/raspberry_pi.md`.

use std::fmt;
use std::io::Read;

use tairix_arch_aarch64::uart_init::CONSOLE_BAUD;

pub mod device;
pub mod elfflat;
pub mod fatboot;
pub mod firmware;
pub mod library;
pub mod overlay;
pub mod rootfs;

pub use tairix_drv_fs_arxfs::{
    EntropySource, Keying, UnlockDescriptor, VolumeKey, VolumeProtection,
    UNLOCK_DEFAULT_ITERATIONS, UNLOCK_DESCRIPTOR_LEN, VOLUME_KEY_LEN,
};

use firmware::FirmwareFile;
use tairix_abi::{DriverError, MACHINE_ID_LEN};
use tairix_syshelp::{DiskLayoutError, BOOT_PART_SECTORS};
use tairix_users::{
    AccountState, Gid, GroupRecord, GroupsDb, Identity, Salt, Uid, UserRecord, UsersDb, STORAGE_GID,
};
use tairix_util::secret::WipedBuf;

/// Sectors in the `ARXFS` root partition as the image is built: 64 MiB, the
/// skeleton plus headroom. The root is the image's last partition, so the
/// kernel's first writable mount on a larger card extends it to the card's end
/// and grows the volume into it: a card-sized root is a first-boot job, not an
/// image-build job.
pub const ROOT_PART_SECTORS: u64 = 131_072;

/// Everything that can go wrong while authoring an image. Every variant is
/// a refusal: mkimage never emits a best-effort image.
#[derive(Debug)]
pub enum MkimageError {
    /// The firmware pin manifest is malformed or incomplete.
    Manifest(String),
    /// A pinned firmware blob is missing or fails verification.
    Firmware(String),
    /// The kernel ELF cannot be flattened into `kernel8.img`.
    KernelElf(&'static str),
    /// The partitions could not be laid out on the disk.
    Disk(DiskLayoutError),
    /// Authoring the FAT32 boot partition failed.
    BootPartition(DriverError),
    /// Authoring the read-only `ARXFS` `/System` partition failed.
    SystemPartition(DriverError),
    /// Authoring the `ARXFS` root partition failed.
    RootPartition(DriverError),
    /// Giving a seeded account's home, or `/System/Settings`, its fixed shape
    /// failed.
    Shape(tairix_users::ShapeError),
    /// Host randomness for the volume's identity or keys is unavailable.
    Entropy(String),
    /// Provisioning or encoding the passphrase-unlock descriptor failed.
    Unlock(DriverError),
    /// Authoring the seeded user database failed.
    UsersDb(String),
    /// Authoring the seeded group registry failed.
    GroupsDb(String),
    /// Deriving the shipped program-library catalog from the planted
    /// bundles failed.
    LibraryCatalog(String),
    /// The image's `network.conf` addressing default does not parse through
    /// the `tairix_netconfig` engine `netstack` reads it with, so shipping it
    /// would give the booted system a store its own stack rejects.
    NetworkConfig,
    /// A first-party device-tree overlay could not be written.
    Overlay(&'static str),
}

impl fmt::Display for MkimageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(msg) => write!(f, "firmware manifest: {msg}"),
            Self::Firmware(msg) => write!(f, "firmware input: {msg}"),
            Self::KernelElf(msg) => write!(f, "kernel ELF: {msg}"),
            Self::Disk(err) => write!(f, "disk layout: {err:?}"),
            Self::BootPartition(err) => write!(f, "boot partition: driver error {err:?}"),
            Self::SystemPartition(err) => write!(f, "system partition: driver error {err:?}"),
            Self::RootPartition(err) => write!(f, "root partition: driver error {err:?}"),
            Self::Shape(err) => write!(f, "directory shape: {err:?}"),
            Self::Entropy(msg) => write!(f, "host entropy: {msg}"),
            Self::Unlock(err) => write!(f, "unlock descriptor: driver error {err:?}"),
            Self::NetworkConfig => {
                f.write_str("network configuration: the image's network.conf does not parse")
            }
            Self::UsersDb(msg) => write!(f, "users database: {msg}"),
            Self::GroupsDb(msg) => write!(f, "group registry: {msg}"),
            Self::LibraryCatalog(msg) => write!(f, "program-library catalog: {msg}"),
            Self::Overlay(msg) => write!(f, "device-tree overlay: {msg}"),
        }
    }
}

impl std::error::Error for MkimageError {}

/// Which kind of image to author.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ImageProfile {
    /// Development image: the default system/service account set plus the
    /// interactive [`DEBUG_USERNAME`]/[`DEBUG_PASSWORD`] administrator and
    /// its [`DEBUG_GROUP`] group, so the login prompt is usable — and the
    /// kernel's identity table builds — without the installer. Never shipped.
    Debug,
    /// Shippable image: the default no-login system/service account set
    /// only; the installer appends the first human user to
    /// `/System/Security/Users` and `/System/Security/Groups` on first boot.
    Installer,
}

impl ImageProfile {
    /// The stable name used in image filenames and the CLI.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Installer => "installer",
        }
    }

    /// The profile named `name`, if any. Exact and case-sensitive, so a
    /// profile flag has one spelling (fail closed).
    #[must_use]
    pub fn from_label(name: &str) -> Option<Self> {
        match name {
            "debug" => Some(Self::Debug),
            "installer" => Some(Self::Installer),
            _ => None,
        }
    }

    /// The number of image profiles — the length a per-profile (or
    /// per-`(arch, profile)`) memo table indexed by [`Self::index`] must
    /// have. Kept in lockstep with the variants by
    /// `index_covers_every_profile`.
    pub const COUNT: usize = 2;

    /// This profile's stable index, so a builder can memoise one composed
    /// artefact per `(arch, profile)` pair in a fixed-size array without a
    /// runtime map.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Debug => 0,
            Self::Installer => 1,
        }
    }

    /// The extra `cargo build` arguments that select the Cargo profile this
    /// image compiles its Rust artefacts (the kernel and every user-space
    /// `Run` binary) in: the non-shippable `debug` image builds in Cargo's
    /// `dev` profile (`debug_assertions` on, no extra argument), and the
    /// shippable `installer` image builds `--release` (optimised,
    /// `debug_assertions` off). This is the single source of truth for the
    /// image → Cargo-profile mapping, paired with [`Self::cargo_profile_dir`].
    #[must_use]
    pub const fn cargo_build_args(self) -> &'static [&'static str] {
        match self {
            Self::Debug => &[],
            Self::Installer => &["--release"],
        }
    }

    /// The `target/<triple>/<dir>/` subdirectory Cargo writes this profile's
    /// artefacts to — `debug` for the `dev` profile, `release` for
    /// `--release`. The read-back path counterpart of
    /// [`Self::cargo_build_args`].
    #[must_use]
    pub const fn cargo_profile_dir(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Installer => "release",
        }
    }
}

/// Username of the debug-profile test account.
pub const DEBUG_USERNAME: &str = "root";

/// Password of the debug-profile test account. Knowable by design — the
/// debug image exists for bring-up on development hardware and must never
/// ship; the installer image seeds no login-capable account at all.
pub const DEBUG_PASSWORD: &str = "root";

/// Uid of the debug-profile test account: the first interactive-user id
/// ([`tairix_users::FIRST_USER_UID`]). Uid 0 belongs to the no-login
/// `system` record — powers come from the account's capability ceiling,
/// never from its uid, so the debug administrator is an ordinary
/// user-band principal.
pub const DEBUG_UID: Uid = Uid(tairix_users::FIRST_USER_UID);

/// Primary group id of the debug-profile test account: the first
/// interactive-user gid ([`tairix_users::FIRST_USER_GID`]). Defined once so
/// the seeded user's `primary_gid` and the group registry it must resolve
/// against cannot disagree about which gid exists.
pub const DEBUG_PRIMARY_GID: Gid = Gid(tairix_users::FIRST_USER_GID);

/// Name of the debug-profile primary group (gid [`DEBUG_PRIMARY_GID`]). The
/// conventional administrative group; the seeded `root` account's powers come
/// from capabilities, not from this group.
pub const DEBUG_GROUP: &str = "wheel";

/// Passphrase a debug image built with an encrypted root
/// ([`VolumeProtection::Encrypted`]) is unlocked with.
///
/// The debug profile is a bring-up image that must never ship (see
/// [`ImageProfile::Debug`]); its root account is `root`/`root`
/// ([`DEBUG_USERNAME`] / [`DEBUG_PASSWORD`]), and the unlock passphrase is
/// the same word, typed at the `ARXFS passphrase:` prompt. Knowable by
/// design — like the seeded account, it exists only for development
/// hardware.
pub const DEBUG_PASSPHRASE: &[u8] = b"root";

/// What kind of image to author: its profile and how its root is protected.
///
/// Only a debug image may be encrypted — whether to encrypt a real
/// installation is the installer's question to the user — so an encrypted
/// installer image cannot be asked for at all.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ImageKind {
    profile: ImageProfile,
    root: VolumeProtection,
}

impl ImageKind {
    /// An image of `profile` whose root is protected as `root` says; an
    /// encrypted root is keyed by [`DEBUG_PASSPHRASE`].
    ///
    /// # Errors
    ///
    /// [`EncryptedInstallerRoot`] for an encrypted installer image.
    pub const fn new(
        profile: ImageProfile,
        root: VolumeProtection,
    ) -> Result<Self, EncryptedInstallerRoot> {
        match (profile, root) {
            (ImageProfile::Installer, VolumeProtection::Encrypted) => Err(EncryptedInstallerRoot),
            _ => Ok(Self { profile, root }),
        }
    }

    /// An image of `profile` with a plaintext root.
    #[must_use]
    pub const fn plaintext(profile: ImageProfile) -> Self {
        Self {
            profile,
            root: VolumeProtection::Plaintext,
        }
    }

    /// The image's profile.
    #[must_use]
    pub const fn profile(self) -> ImageProfile {
        self.profile
    }

    /// How the image's root is protected.
    #[must_use]
    pub const fn root(self) -> VolumeProtection {
        self.root
    }
}

/// An encrypted root asked of an installer image, which is never authored.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EncryptedInstallerRoot;

impl fmt::Display for EncryptedInstallerRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "only a debug image's root can be encrypted: whether to encrypt an \
             installation is the installer's question",
        )
    }
}

impl std::error::Error for EncryptedInstallerRoot {}

/// Build the profile's `/System/Security/Users` text.
///
/// The on-disk database holds **human** accounts only: the system/service
/// identity is compiled into the kernel (`tairix_users::system_accounts`,
/// `plans/USERS.md`) and the kernel's identity merge refuses any on-disk
/// record colliding with it, so neither profile writes those records to
/// disk. The installer profile therefore seeds an *empty* database (its
/// first human account is created by the installer's first-boot
/// provisioning); the debug profile seeds the interactive
/// [`DEBUG_USERNAME`] administrator (uid [`DEBUG_UID`], its password
/// salted from `entropy` and hashed at the default PBKDF2 cost) granted
/// the administrator capability ceiling
/// (`tairix_users::administrator_ceiling`) a bring-up session needs.
/// Powers come from capabilities, not from a uid: the account is an
/// administrator only because its ceiling says so.
fn users_db(
    profile: ImageProfile,
    entropy: &mut dyn EntropySource,
) -> Result<String, MkimageError> {
    let mut records = Vec::new();
    if profile == ImageProfile::Debug {
        let mut salt: Salt = [0u8; tairix_users::SALT_LEN];
        entropy
            .fill(&mut salt)
            .map_err(|e| MkimageError::Entropy(format!("users salt: {e:?}")))?;

        let record = UserRecord::with_password(
            Identity {
                username: DEBUG_USERNAME,
                uid: DEBUG_UID,
                primary_gid: DEBUG_PRIMARY_GID,
                supplementary_gids: &[STORAGE_GID],
                display_name: "System Administrator",
                home: Some(&tairix_users::default_home(DEBUG_USERNAME)),
                shell: Some(tairix_users::DEFAULT_SHELL),
                capabilities: tairix_users::administrator_ceiling(),
                state: AccountState::Active,
            },
            DEBUG_PASSWORD.as_bytes(),
            salt,
            tairix_users::DEFAULT_ITERATIONS,
        )
        .map_err(|e| MkimageError::UsersDb(format!("debug root record: {e}")))?;
        records.push(record);
    }
    let db = UsersDb::new(records)
        .map_err(|e| MkimageError::UsersDb(format!("seeded database: {e}")))?;
    Ok(db.serialise())
}

/// Build the profile's `/System/Security/Groups` text.
///
/// Both profiles seed the well-known removable-storage group
/// ([`tairix_users::STORAGE_GROUP`]) the unlock resolves by name to arm
/// the hotplug-volume identity map (`plans/DEVICES.md` D3d) — storage
/// membership is admin-managed data about human accounts, so it lives on
/// disk beside them. The `system` and `services` groups are compiled into
/// the kernel with the system accounts (`plans/USERS.md`) and never
/// written to disk. The debug profile appends the [`DEBUG_GROUP`] group
/// (gid [`DEBUG_PRIMARY_GID`]) the seeded administrator's primary gid
/// references — so the kernel's identity merge resolves that reference
/// against a real registry rather than failing closed on a dangling
/// group. Membership is not stored here — it lives in the user records;
/// this is only the authoritative name↔gid set.
fn groups_db(profile: ImageProfile) -> Result<String, MkimageError> {
    let mut records = vec![GroupRecord::new(tairix_users::STORAGE_GROUP, STORAGE_GID)
        .map_err(|e| MkimageError::GroupsDb(format!("storage group record: {e}")))?];
    if profile == ImageProfile::Debug {
        records.push(
            GroupRecord::new(DEBUG_GROUP, DEBUG_PRIMARY_GID)
                .map_err(|e| MkimageError::GroupsDb(format!("debug group record: {e}")))?,
        );
    }
    let db = GroupsDb::new(records)
        .map_err(|e| MkimageError::GroupsDb(format!("seeded registry: {e}")))?;
    Ok(db.serialise())
}

/// Build the debug-profile log-attestation key file image
/// (`PREREQUISITES.md` P-E): a fresh random [`tairix_log::LogAttestationKey`]
/// drawn from `entropy`, serialised to its on-disk form. Only a debug image
/// bakes a key; an installer image's per-installation key is generated at
/// first boot (baking one common key into every installer image would be a
/// shared secret, a security hole).
/// Fails closed if the host entropy source cannot supply the key bytes.
fn debug_log_attestation_key(entropy: &mut dyn EntropySource) -> Result<Vec<u8>, MkimageError> {
    let mut key = [0u8; tairix_log::LOG_ATTESTATION_KEY_LEN];
    entropy
        .fill(&mut key)
        .map_err(|e| MkimageError::Entropy(format!("log-attestation key: {e:?}")))?;
    Ok(tairix_log::LogAttestationKey::from_key(key)
        .to_file_bytes()
        .to_vec())
}

/// Build the debug-profile per-installation machine-id: [`MACHINE_ID_LEN`]
/// fresh random bytes drawn from `entropy`. The machine-id is **non-secret**
/// per-installation identity (the TAIRiX equivalent of `/etc/machine-id`) that
/// the system log binds each stream's hash-chain genesis to
/// (`plans/SYSLOG.md` §7.1); giving each debug image its own random id keeps
/// two images' logs from sharing a genesis. Only a debug image bakes one; an
/// installer image mints its machine-id at first boot, exactly as it does the
/// log-attestation key. Fails closed if the host entropy source cannot supply
/// the bytes.
fn debug_machine_id(entropy: &mut dyn EntropySource) -> Result<[u8; MACHINE_ID_LEN], MkimageError> {
    let mut id = [0u8; MACHINE_ID_LEN];
    entropy
        .fill(&mut id)
        .map_err(|e| MkimageError::Entropy(format!("machine-id: {e:?}")))?;
    Ok(id)
}

/// The assembled image plus the material the operator must keep.
pub struct RpiImage {
    /// The flashable image bytes, exactly as long as its partition table
    /// describes.
    pub image: Vec<u8>,
    /// What an encrypted root was provisioned under; `None` for a plaintext
    /// root.
    pub unlock: Option<RootUnlock>,
}

/// What an encrypted root was provisioned under.
pub struct RootUnlock {
    /// The passphrase-unlock descriptor, laid down in the clear on the boot
    /// partition ([`fatboot::ROOT_UNLOCK_NAME`]). It is not a secret — only
    /// the salt and iteration count needed to re-derive the volume key from
    /// the passphrase.
    pub descriptor: UnlockDescriptor,
    /// The root volume key, derived from [`DEBUG_PASSPHRASE`] under
    /// [`Self::descriptor`]. Mounting the root needs it; it is stored nowhere
    /// inside the image.
    pub key: VolumeKey,
}

/// Build the flashable Raspberry Pi 4 SD image.
///
/// `kernel_elf` is the freestanding aarch64 `tairix-kernel` ELF;
/// `firmware` is the verified blob set from
/// [`firmware::FirmwareManifest::load_dir`]; `entropy` draws the unlock
/// descriptor's salt, the root volume's internal key hierarchy, and, on a
/// debug build, the seeded account's password salt; `kind` selects the
/// [`ImageProfile`] and the root's protection.
///
/// A plaintext root boots with no passphrase. An encrypted one — a debug
/// image's only — is keyed by a
/// [`VolumeKey`] **derived** from [`DEBUG_PASSPHRASE`] through a freshly
/// provisioned [`UnlockDescriptor`], and the plaintext descriptor is laid on
/// the boot partition ([`fatboot::ROOT_UNLOCK_NAME`]) so the bootstrap
/// re-derives the key from the passphrase before mounting.
///
/// `drivers` is the set of signed `.rxe` driver bundles to install into the
/// read-only `/System/Drivers/` store, each as `(components, bytes)` where
/// `components` is the bundle's leaf path **relative to the `/System` volume
/// root** (for example `&[b"Drivers", b"bus_mailbox", b"vcmailbox", b"Run"]`)
/// and `bytes` is the bundle exactly as the store scan reads it back.
/// The caller cross-compiles and signs each bundle (this crate stays pure —
/// it never drives `cargo`); `build_rpi_image` only plants the bytes. The
/// kernel verifies every bundle against its embedded trust anchor at load, so
/// a tampered read-only store fails closed. An empty slice
/// produces an image with no autoloaded drivers (every node left unbound).
///
/// `apps` is the set of application-bundle files to plant into the read-only
/// system program and service stores, each as `(components, bytes)` where
/// `components` is the file's path relative to the `/System` volume root
/// (for example `&[b"Commands", b"ls.app", b"AppInfo"]`) — every program's
/// signed `AppInfo` + `Run` beside its `Help/` tree, so each bundle is a
/// complete, self-contained on-disk directory (`plans/APPS.md` deliverable
/// 8). As with `drivers`, the caller composes and signs the files; this
/// crate only plants them.
///
/// `network_conf` is the per-interface network configuration the image ships,
/// planted on the read-only `/System` volume at the volume-relative path its
/// only reader — the device manager's pre-unlock store read — resolves. The
/// caller composes it (so *which* interfaces an image manages is a property
/// of the image, not of this writer) and this crate validates it through the
/// one `tairix_netconfig` engine before planting it, so an image can never
/// ship an addressing default its own stack would reject.
///
/// # Errors
///
/// Any [`MkimageError`] from the kernel conversion, descriptor
/// provisioning, partition authoring, or assembly; the build fails closed
/// rather than emitting a partial image. [`MkimageError::NetworkConfig`] if
/// `network_conf` does not parse.
pub fn build_rpi_image(
    kernel_elf: &[u8],
    firmware: &[FirmwareFile],
    entropy: &mut dyn EntropySource,
    kind: ImageKind,
    drivers: &[(&[&[u8]], &[u8])],
    apps: &[(&[&[u8]], &[u8])],
    network_conf: &str,
) -> Result<RpiImage, MkimageError> {
    let profile = kind.profile();
    let users_db = users_db(profile, entropy)?;
    let groups_db = groups_db(profile)?;
    // The debug image's interactive administrator gets its recorded home
    // provisioned account-owned and owner-only (a recorded home is a real
    // inode, never a dangling path); the installer image seeds no
    // login-capable account, so no home either.
    let home_dirs: &[(&str, u32, u32)] = match profile {
        ImageProfile::Debug => &[(DEBUG_USERNAME, DEBUG_UID.0, DEBUG_PRIMARY_GID.0)],
        ImageProfile::Installer => &[],
    };
    // Only a debug image bakes a log-attestation key; an installer image's
    // per-installation key is generated at first boot (a common baked key
    // would be a shared secret, a security hole). When present the bytes are
    // the `tairix_log::LogAttestationKey` on-disk image.
    let log_key_file = match profile {
        ImageProfile::Debug => Some(debug_log_attestation_key(entropy)?),
        ImageProfile::Installer => None,
    };
    // Likewise the non-secret per-installation machine-id: a debug image bakes
    // a random one (so two debug images do not share a log genesis); an
    // installer image mints it at first boot.
    let machine_id = match profile {
        ImageProfile::Debug => Some(debug_machine_id(entropy)?),
        ImageProfile::Installer => None,
    };
    // The machine-wide program-library catalog ships derived from the very
    // bundles this image plants (never a hand-maintained list); an image
    // with no listed application ships the canonical empty store.
    let library_conf = library::library_catalog(apps)?;
    let kernel8 = elfflat::elf_to_flat(kernel_elf, elfflat::KERNEL_LOAD_ADDR)?;

    // An encrypted root's key is derived under a fresh per-volume descriptor,
    // laid (non-secret) beside the volume on the boot partition so the
    // bootstrap can re-derive it.
    let unlock = match kind.root() {
        VolumeProtection::Plaintext => None,
        VolumeProtection::Encrypted => {
            let descriptor = UnlockDescriptor::provision(UNLOCK_DEFAULT_ITERATIONS, entropy)
                .map_err(MkimageError::Unlock)?;
            let key = *descriptor
                .derive_volume_key(DEBUG_PASSPHRASE)
                .map_err(MkimageError::Unlock)?;
            Some(RootUnlock { descriptor, key })
        }
    };
    let mut encoded = [0u8; UNLOCK_DESCRIPTOR_LEN];
    if let Some(unlock) = &unlock {
        unlock
            .descriptor
            .encode(&mut encoded)
            .map_err(MkimageError::Unlock)?;
    }

    let boot = fatboot::build_boot_partition(
        BOOT_PART_SECTORS,
        firmware,
        &kernel8,
        unlock.as_ref().map(|_| &encoded[..]),
        CONSOLE_BAUD,
    )?;
    let system = rootfs::build_system_partition(entropy, drivers, apps, network_conf)?;
    let keying = unlock
        .as_ref()
        .map_or(Keying::Plaintext, |unlock| Keying::Key(&unlock.key));
    let root = rootfs::build_root_partition(
        ROOT_PART_SECTORS,
        keying,
        entropy,
        &rootfs::RootSeed {
            users_db: &users_db,
            groups_db: &groups_db,
            home_dirs,
            log_attestation_key: log_key_file.as_deref(),
            machine_id: machine_id.as_ref().map(<[u8; MACHINE_ID_LEN]>::as_slice),
            library_conf: &library_conf,
        },
    )?;

    let image = tairix_syshelp::assemble_disk(&boot, &system, &root).map_err(MkimageError::Disk)?;

    Ok(RpiImage { image, unlock })
}

/// Cryptographic randomness from the host operating system
/// (`/dev/urandom`), used for every volume's identity and, for an encrypted
/// root, its descriptor salt and internal key hierarchy. Fails closed when the host source is
/// unavailable — an image is never provisioned with predictable key
/// material.
pub struct HostEntropy;

impl EntropySource for HostEntropy {
    fn fill(&mut self, out: &mut [u8]) -> Result<(), DriverError> {
        let mut source =
            std::fs::File::open("/dev/urandom").map_err(|_| DriverError::DeviceFault)?;
        source.read_exact(out).map_err(|_| DriverError::DeviceFault)
    }
}

/// Replace the build output at `path` with `bytes`, whole or not at all.
///
/// The bytes are written beside `path` and renamed over it, so a build cut
/// short never leaves a truncated image under the name, and an emulator still
/// running the previous image keeps the disk it opened. Two builds writing the
/// same output take turns on an advisory lock on `<name>.lock` beside it, so
/// neither renames the other's half-written sibling over it.
///
/// # Errors
///
/// `InvalidInput` when `path` names something other than a regular file — a
/// device or a link is never replaced by a file — else the I/O error that
/// refused the write or the rename. `path` is then as it was.
pub fn replace_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    replace_with(path, bytes, std::fs::OpenOptions::new())
}

fn replace_with(
    path: &std::path::Path,
    bytes: &[u8],
    mut options: std::fs::OpenOptions,
) -> std::io::Result<()> {
    use std::io::Write as _;
    match std::fs::symlink_metadata(path) {
        Ok(found) if !found.file_type().is_file() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name"))?;
    let sibling = |suffix: &str| {
        let mut sibling = name.to_os_string();
        sibling.push(suffix);
        path.with_file_name(sibling)
    };
    let turn = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(sibling(".lock"))?;
    turn.lock()?;
    let staged = sibling(".part");
    // What a cut-short build left is cleared, and the exclusive create then
    // never writes through anything planted at the name.
    match std::fs::remove_file(&staged) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    let mut file = options.write(true).create_new(true).open(&staged)?;
    let written = file
        .write_all(bytes)
        .and_then(|()| std::fs::rename(&staged, path));
    if written.is_err() {
        // The refusal is the error to report; the sibling goes regardless.
        let _ = std::fs::remove_file(&staged);
    }
    written
}

/// Write `unlock`'s root volume key to `path` as its 64-hex-digit key-file
/// body, readable by its owner alone before any key byte reaches it: the key
/// mounts the image's root on a host. It replaces the file whole, as an image
/// is replaced ([`replace_file`]).
///
/// # Errors
///
/// As [`replace_file`].
pub fn write_root_key(path: &std::path::Path, unlock: &RootUnlock) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    replace_with(path, &volume_key_to_hex(&unlock.key), options)
}

/// Remove the root-key file an earlier encrypted build left at `path`, so it
/// cannot be taken for the key of a plaintext image written beside it.
/// Returns whether there was one.
///
/// # Errors
///
/// The I/O error that refused the removal; a file that is absent is none.
pub fn remove_stale_root_key(path: &std::path::Path) -> std::io::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

/// Render a volume key as the 64-hex-digit key-file body.
fn volume_key_to_hex(key: &VolumeKey) -> WipedBuf {
    let mut text = Vec::with_capacity(VOLUME_KEY_LEN * 2 + 1);
    text.extend(key.iter().flat_map(|&byte| tairix_util::hex::digits(byte)));
    text.push(b'\n');
    WipedBuf::new(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use device::{MemBlock, SECTOR_BYTES};
    use tairix_abi::driver::filesystem::{FilesystemRead, FilesystemWrite, NodeKind};
    use tairix_abi::CapabilityId;
    use tairix_drv_fs_arxfs::ARXFS;
    use tairix_drv_fs_fat32::Fat32;
    use tairix_partition::{mbr, PartitionType};
    use tairix_users::STORAGE_GROUP;

    const TEST_KEY: VolumeKey = [0x42; VOLUME_KEY_LEN];

    /// Every profile has a distinct index below `COUNT`, so a
    /// `(arch, profile)` memo table sized `PieArch::COUNT * COUNT` and
    /// addressed by `index` never aliases two profiles onto one slot — the
    /// defect that let the `installer` image reuse the `debug` image's
    /// composed bundles.
    #[test]
    fn index_covers_every_profile() {
        let mut seen = [false; ImageProfile::COUNT];
        for profile in [ImageProfile::Debug, ImageProfile::Installer] {
            let idx = profile.index();
            assert!(idx < ImageProfile::COUNT, "{profile:?} index in range");
            assert!(!seen[idx], "{profile:?} index must be unique");
            seen[idx] = true;
        }
        assert!(seen.iter().all(|&s| s), "every slot is claimed once");
    }

    /// The image → Cargo-profile mapping: the non-shippable `debug` image
    /// builds in Cargo's `dev` profile (no `--release`, artefacts under
    /// `debug/`) and the shippable `installer` image builds `--release`
    /// (artefacts under `release/`). The single source both the kernel build
    /// and the user-space `Run` cross-compiles read.
    #[test]
    fn cargo_profile_mapping_matches_the_image_profile() {
        assert!(ImageProfile::Debug.cargo_build_args().is_empty());
        assert_eq!(ImageProfile::Debug.cargo_profile_dir(), "debug");
        assert_eq!(ImageProfile::Installer.cargo_build_args(), &["--release"]);
        assert_eq!(ImageProfile::Installer.cargo_profile_dir(), "release");
    }

    struct TestEntropy(u8);

    impl EntropySource for TestEntropy {
        fn fill(&mut self, out: &mut [u8]) -> Result<(), DriverError> {
            for byte in out.iter_mut() {
                *byte = self.0;
                self.0 = self.0.wrapping_add(1);
            }
            Ok(())
        }
    }

    /// A minimal valid kernel ELF reused from the converter's test
    /// builder: one code segment at the Pi load address.
    fn test_kernel_elf() -> Vec<u8> {
        elfflat::tests_support::sample_kernel(&[0xde, 0xad, 0xbe, 0xef])
    }

    /// The canonical empty `network.conf` these image tests ship: they plant
    /// no NIC driver, so the image manages no interface beyond loopback.
    fn test_network_conf() -> String {
        tairix_netconfig::NetworkConfig::default().render()
    }

    fn test_firmware() -> Vec<FirmwareFile> {
        vec![
            FirmwareFile {
                name: "start4.elf".into(),
                bytes: vec![0x11; 2048],
            },
            FirmwareFile {
                name: "fixup4.dat".into(),
                bytes: vec![0x22; 64],
            },
            FirmwareFile {
                name: "bcm2711-rpi-4-b.dtb".into(),
                bytes: vec![0x33; 512],
            },
        ]
    }

    /// The byte range `ty`'s partition occupies in `image`, read from the
    /// image's own partition table.
    fn extent(image: &[u8], ty: PartitionType) -> core::ops::Range<usize> {
        let table = mbr::parse(&image[..SECTOR_BYTES]).expect("the MBR parses");
        let part = table.first_of_type(ty).expect("the partition is present");
        let at = usize::try_from(part.start_lba).expect("fits usize") * SECTOR_BYTES;
        at..at + usize::try_from(part.block_count).expect("fits usize") * SECTOR_BYTES
    }

    /// The bytes of `image`'s data-root partition.
    fn root_partition(image: &[u8]) -> Vec<u8> {
        image[extent(image, PartitionType::ARXFSRoot)].to_vec()
    }

    /// Read the encoded unlock descriptor planted on a built image's FAT
    /// boot partition.
    fn read_unlock_descriptor(image: &[u8]) -> UnlockDescriptor {
        let boot = image[extent(image, PartitionType::FatBoot)].to_vec();
        let mut fat = Fat32::open(MemBlock::from_bytes(boot).expect("whole sectors"))
            .expect("boot partition mounts");
        let root = fat.root();
        let node = fat
            .lookup(root, fatboot::ROOT_UNLOCK_NAME.as_bytes())
            .expect("root.unlock present");
        let mut bytes = [0u8; UNLOCK_DESCRIPTOR_LEN];
        let n = fat.read_at(node, 0, &mut bytes).expect("descriptor reads");
        assert_eq!(n, UNLOCK_DESCRIPTOR_LEN);
        UnlockDescriptor::decode(&bytes).expect("descriptor decodes")
    }

    #[test]
    fn assembles_a_flashable_image_both_partitions_mount() {
        let built = build_rpi_image(
            &test_kernel_elf(),
            &test_firmware(),
            &mut TestEntropy(9),
            ImageKind::plaintext(ImageProfile::Installer),
            &[],
            &[],
            &test_network_conf(),
        )
        .expect("image builds");
        assert_eq!(
            extent(&built.image, PartitionType::ARXFSRoot).end,
            built.image.len(),
            "the image ends where its last partition does"
        );

        // The MBR carries the expected three-partition table: FAT boot,
        // read-only `/System`, data root.
        assert_eq!(built.image[510], 0x55);
        assert_eq!(built.image[511], 0xaa);
        assert_eq!(built.image[446 + 4], mbr::PART_TYPE_FAT32_LBA);
        assert_eq!(built.image[446 + 16 + 4], mbr::PART_TYPE_ARXFS_SYSTEM);
        assert_eq!(built.image[446 + 32 + 4], mbr::PART_TYPE_ARXFS);

        // The boot partition mounts and carries the flat kernel.
        let boot_extent = extent(&built.image, PartitionType::FatBoot);
        assert_eq!(
            boot_extent.start,
            1024 * 1024,
            "the boot partition is 1 MiB in"
        );
        let boot = built.image[boot_extent].to_vec();
        let mut fat = Fat32::open(MemBlock::from_bytes(boot).expect("whole sectors"))
            .expect("boot partition mounts");
        let root = fat.root();
        let node = fat
            .lookup(root, fatboot::KERNEL_IMG_NAME.as_bytes())
            .expect("kernel8.img present");
        let mut kernel8 = [0u8; 16];
        let n = fat.read_at(node, 0, &mut kernel8).expect("kernel reads");
        assert_eq!(&kernel8[..n], &[0xde, 0xad, 0xbe, 0xef]);
        let info = fat.node_info(node).expect("kernel node info");
        assert_eq!(info.kind, NodeKind::RegularFile);

        // A plaintext root carries no unlock descriptor and mounts with no
        // key.
        assert!(built.unlock.is_none());
        assert_eq!(
            fat.lookup(root, fatboot::ROOT_UNLOCK_NAME.as_bytes()).err(),
            Some(DriverError::NotFound)
        );
        let mut rfs = ARXFS::open(
            MemBlock::from_bytes(root_partition(&built.image)).expect("whole sectors"),
            Keying::Plaintext,
        )
        .expect("root partition mounts");
        let arxfs_root = rfs.root();
        rfs.lookup(arxfs_root, b"System").expect("/System exists");

        // An installer image seeds the human-account security databases
        // (an empty users database — the first human user is a first-boot
        // job; the installer-profile content is pinned by
        // `an_installer_image_seeds_an_empty_users_database`).
        let system = rfs.lookup(arxfs_root, b"System").expect("/System");
        let security = rfs.lookup(system, b"Security").expect("Security");
        rfs.lookup(security, rootfs::USERS_DB_NAME.as_bytes())
            .expect("Users database exists");
        rfs.lookup(security, rootfs::GROUPS_DB_NAME.as_bytes())
            .expect("Groups registry exists");
    }

    #[test]
    fn the_system_partition_mounts_read_only_and_carries_the_skeleton() {
        use tairix_partition::{parse_partition_table, PartitionBlock, PartitionType};

        let built = build_rpi_image(
            &test_kernel_elf(),
            &test_firmware(),
            &mut TestEntropy(9),
            ImageKind::plaintext(ImageProfile::Installer),
            &[],
            &[],
            &test_network_conf(),
        )
        .expect("image builds");

        // The whole-disk table locates the read-only `/System` partition by
        // role at the documented offset, in whole grains and followed
        // directly by the root.
        let mut disk = MemBlock::from_bytes(built.image.clone()).expect("whole sectors");
        let table = parse_partition_table(&mut disk).expect("the MBR parses");
        let system = table
            .first_of_type(PartitionType::ARXFSSystem)
            .expect("a /System partition is present");
        assert_eq!(system.start_lba, tairix_syshelp::SYSTEM_PART_LBA);
        let grain = tairix_syshelp::SYSTEM_VOLUME_GRAIN_BYTES / SECTOR_BYTES as u64;
        assert!(
            system.block_count.is_multiple_of(grain),
            "/System is whole grains: {} sectors",
            system.block_count
        );
        let root = table
            .first_of_type(PartitionType::ARXFSRoot)
            .expect("a data root is present");
        assert_eq!(root.start_lba, system.start_lba + system.block_count);

        // It mounts read-only as the plaintext volume it is, and its root
        // *is* `/System`, carrying the skeleton directly.
        let window = PartitionBlock::from_partition(disk, &system).expect("the /System window");
        let mut sys = ARXFS::open_read_only(window, Keying::Plaintext)
            .expect("the plaintext /System mounts read-only");
        let sys_root = sys.root();
        sys.lookup(sys_root, b"Drivers").expect("/System/Drivers");
        let security = sys.lookup(sys_root, b"Security").expect("/System/Security");
        sys.lookup(security, b"Keys")
            .expect("/System/Security/Keys");
        // The store carries no users database (that secret stays on the
        // root volume).
        assert!(sys
            .lookup(security, rootfs::USERS_DB_NAME.as_bytes())
            .is_err());

        // A read-only mount refuses mutation fail-closed, as read-only.
        assert_eq!(
            sys.create(sys_root, b"x", NodeKind::Directory),
            Err(DriverError::ReadOnly)
        );
    }

    #[test]
    fn an_installed_driver_bundle_reads_back_from_the_readonly_system_store() {
        use tairix_partition::{parse_partition_table, PartitionBlock, PartitionType};

        // A synthetic bundle blob: this test proves the *planting* (path +
        // bytes + intermediate-directory creation), not signature validity —
        // the signed-bundle composition is exercised where it is built. The store path mirrors the real mailbox
        // service-driver install (`Drivers/<class>/<leaf>/Run`).
        const BUNDLE: &[u8] = b"a signed .rxe driver bundle's bytes (synthetic)";
        let store_path: &[&[u8]] = &[b"Drivers", b"bus_mailbox", b"vcmailbox", b"Run"];

        let built = build_rpi_image(
            &test_kernel_elf(),
            &test_firmware(),
            &mut TestEntropy(9),
            ImageKind::plaintext(ImageProfile::Installer),
            &[(store_path, BUNDLE)],
            &[],
            &test_network_conf(),
        )
        .expect("image builds");

        // Mount the read-only `/System` volume and read the planted bundle
        // back at its store path, byte-for-byte — the shape the store
        // scan reads.
        let mut disk = MemBlock::from_bytes(built.image).expect("whole sectors");
        let table = parse_partition_table(&mut disk).expect("the MBR parses");
        let system = table
            .first_of_type(PartitionType::ARXFSSystem)
            .expect("a /System partition is present");
        let window = PartitionBlock::from_partition(disk, &system).expect("the /System window");
        let mut sys = ARXFS::open_read_only(window, Keying::Plaintext)
            .expect("the plaintext /System mounts read-only");

        let mut node = sys.root();
        for component in store_path {
            node = sys.lookup(node, component).expect("store path component");
        }
        let mut buf = vec![0u8; BUNDLE.len() + 16];
        let read = sys.read_at(node, 0, &mut buf).expect("bundle reads back");
        assert_eq!(&buf[..read], BUNDLE);
    }

    #[test]
    fn the_network_configuration_reads_back_from_the_readonly_system_store() {
        use tairix_abi::driver_store::SystemConfigFile;
        use tairix_partition::{parse_partition_table, PartitionBlock, PartitionType};

        // The device manager reads this document off the read-only `/System`
        // volume through the pre-unlock store endpoint, so the whole assembly
        // has to land it there — not on the root volume, whose
        // `/System/Settings` sub-mount no bootstrap client can reach. An
        // image that plants it anywhere else boots with no managed interface
        // and never starts its DHCP client.
        let conf = "wan.kind ethernet\nwan.match.node 0xfd580000\nwan.ipv4.method dhcp\n";
        let built = build_rpi_image(
            &test_kernel_elf(),
            &test_firmware(),
            &mut TestEntropy(23),
            ImageKind::plaintext(ImageProfile::Installer),
            &[],
            &[],
            conf,
        )
        .expect("image builds");

        let mut disk = MemBlock::from_bytes(built.image).expect("whole sectors");
        let table = parse_partition_table(&mut disk).expect("the MBR parses");
        let system = table
            .first_of_type(PartitionType::ARXFSSystem)
            .expect("a /System partition is present");
        let window = PartitionBlock::from_partition(disk, &system).expect("the /System window");
        let mut sys = ARXFS::open_read_only(window, Keying::Plaintext)
            .expect("the plaintext /System mounts read-only");

        let mut node = sys.root();
        for component in SystemConfigFile::Network.volume_path().split('/') {
            node = sys
                .lookup(node, component.as_bytes())
                .expect("the ABI volume path resolves on the /System volume");
        }
        let mut buf = [0u8; tairix_netconfig::MAX_CONFIG_LEN];
        let read = sys.read_at(node, 0, &mut buf).expect("the document reads");
        let parsed = tairix_netconfig::NetworkConfig::parse(
            core::str::from_utf8(&buf[..read]).expect("utf-8"),
        )
        .expect("the planted document parses");
        let wan = parsed.interface("wan").expect("the planted interface");
        assert_eq!(wan.match_node, Some(0xfd58_0000));
        assert_eq!(wan.ipv4_method(), tairix_netconfig::Ipv4Method::Dhcp);
    }

    #[test]
    fn an_installed_app_bundle_reads_back_beside_its_help_tree() {
        use tairix_partition::{parse_partition_table, PartitionBlock, PartitionType};

        // Synthetic bundle files: this test proves the *planting* of a
        // complete self-contained bundle (`AppInfo` + `Run` beside the
        // discovered `Help/` tree), not signature validity — the signed
        // composition is exercised where it is built. The manifests must
        // still *decode* (the program-library derivation reads every
        // planted app-store manifest and fails the build closed on
        // garbage); the `Run` bytes stay synthetic. The store paths mirror
        // the real installs (`Commands/<name>.app/…`, `Services/<name>.app/…`).
        const RUN: &[u8] = b"a Run rxe image's bytes (synthetic)";
        let ls_appinfo =
            crate::library::test_manifest("ls", tairix_abi::ProgramKind::Command, None, None);
        let login_appinfo =
            crate::library::test_manifest("login", tairix_abi::ProgramKind::Service, None, None);
        let app_files: [(&[&[u8]], &[u8]); 4] = [
            (&[b"Commands", b"ls.app", b"AppInfo"], ls_appinfo.as_slice()),
            (&[b"Commands", b"ls.app", b"Run"], RUN),
            (
                &[b"Services", b"login.app", b"AppInfo"],
                login_appinfo.as_slice(),
            ),
            (&[b"Services", b"login.app", b"Run"], RUN),
        ];

        let built = build_rpi_image(
            &test_kernel_elf(),
            &test_firmware(),
            &mut TestEntropy(9),
            ImageKind::plaintext(ImageProfile::Installer),
            &[],
            &app_files,
            &test_network_conf(),
        )
        .expect("image builds");

        let mut disk = MemBlock::from_bytes(built.image).expect("whole sectors");
        let table = parse_partition_table(&mut disk).expect("the MBR parses");
        let system = table
            .first_of_type(PartitionType::ARXFSSystem)
            .expect("a /System partition is present");
        let window = PartitionBlock::from_partition(disk, &system).expect("the /System window");
        let mut sys = ARXFS::open_read_only(window, Keying::Plaintext)
            .expect("the plaintext /System mounts read-only");

        // Every planted bundle file reads back byte-for-byte at its store
        // path…
        for (components, bytes) in &app_files {
            let mut node = sys.root();
            for component in *components {
                node = sys.lookup(node, component).expect("store path component");
            }
            let mut buf = vec![0u8; bytes.len() + 16];
            let read = sys.read_at(node, 0, &mut buf).expect("file reads back");
            assert_eq!(&buf[..read], *bytes);
        }
        // …and the command app's bundle directory also carries its
        // discovered `Help/` tree, so the on-disk bundle is complete.
        let mut help = sys.root();
        for component in [b"Commands".as_slice(), b"ls.app", b"Help", b"en-US"] {
            help = sys.lookup(help, component).expect("Help path component");
        }
        sys.lookup(help, b"ls.md")
            .expect("the bundle's default help document is planted beside Run");
    }

    #[test]
    fn the_desktop_icon_artwork_reads_back_from_system_graphics() {
        use tairix_partition::{parse_partition_table, PartitionBlock, PartitionType};

        // The desktop's icon class masters ship under `/System/Graphics`,
        // discovered from disk (`tairix_syshelp::ASSET_FILES`). One
        // master of each class format is read back, so neither the vector
        // nor the raster arm can rot, and an accidentally empty walk cannot
        // pass this test silently.
        let built = build_rpi_image(
            &test_kernel_elf(),
            &test_firmware(),
            &mut TestEntropy(9),
            ImageKind::plaintext(ImageProfile::Installer),
            &[],
            &[],
            &test_network_conf(),
        )
        .expect("image builds");

        let mut disk = MemBlock::from_bytes(built.image).expect("whole sectors");
        let table = parse_partition_table(&mut disk).expect("the MBR parses");
        let system = table
            .first_of_type(PartitionType::ARXFSSystem)
            .expect("a /System partition is present");
        let window = PartitionBlock::from_partition(disk, &system).expect("the /System window");
        let mut sys = ARXFS::open_read_only(window, Keying::Plaintext)
            .expect("the plaintext /System mounts read-only");

        for file in ["folder.svg", "file.png"] {
            let master = tairix_syshelp::ASSET_FILES
                .iter()
                .find(|asset| {
                    asset.family == tairix_syshelp::AssetFamilyKind::Icon && asset.file == file
                })
                .expect("the icon master ships");
            let mut node = sys.root();
            for component in [b"Graphics".as_slice(), b"Icons", file.as_bytes()] {
                node = sys
                    .lookup(node, component)
                    .expect("Graphics/Icons path component");
            }
            let mut buf = vec![0u8; master.bytes.len() + 16];
            let read = sys.read_at(node, 0, &mut buf).expect("the icon reads back");
            assert_eq!(
                &buf[..read],
                master.bytes,
                "the planted {file} is byte-identical to the shipped master"
            );
        }
    }

    #[test]
    fn an_encrypted_debug_root_mounts_only_under_the_debug_passphrases_key() {
        let built = build_rpi_image(
            &test_kernel_elf(),
            &test_firmware(),
            &mut TestEntropy(9),
            ImageKind::new(ImageProfile::Debug, VolumeProtection::Encrypted)
                .expect("a debug image may be encrypted"),
            &[],
            &[],
            &test_network_conf(),
        )
        .expect("image builds");
        let unlock = built.unlock.as_ref().expect("an encrypted root's unlock");

        // The on-disk descriptor re-derives exactly the key the image was
        // provisioned under (the bootstrap's path).
        let descriptor = read_unlock_descriptor(&built.image);
        assert_eq!(descriptor, unlock.descriptor);
        assert_eq!(
            *descriptor
                .derive_volume_key(DEBUG_PASSPHRASE)
                .expect("derive"),
            unlock.key
        );
        let device = || MemBlock::from_bytes(root_partition(&built.image)).expect("whole sectors");
        let mut rfs = ARXFS::open(device(), Keying::Key(&unlock.key)).expect("root mounts");
        let arxfs_root = rfs.root();
        rfs.lookup(arxfs_root, b"System").expect("/System exists");

        // A wrong passphrase derives a different key, which the volume's
        // AEAD-wrapped master key rejects — no separate oracle — and an
        // encrypted root never mounts with no key.
        let wrong = *descriptor
            .derive_volume_key(b"not the passphrase")
            .expect("derive");
        assert_ne!(wrong, unlock.key);
        assert!(ARXFS::open(device(), Keying::Key(&wrong)).is_err());
        assert!(ARXFS::open(device(), Keying::Plaintext).is_err());
    }

    #[test]
    fn only_a_debug_image_can_be_asked_for_with_an_encrypted_root() {
        let encrypted = ImageKind::new(ImageProfile::Debug, VolumeProtection::Encrypted)
            .expect("a debug image may be encrypted");
        assert_eq!(encrypted.profile(), ImageProfile::Debug);
        assert_eq!(encrypted.root(), VolumeProtection::Encrypted);
        assert_eq!(
            ImageKind::new(ImageProfile::Installer, VolumeProtection::Encrypted),
            Err(EncryptedInstallerRoot)
        );
        for profile in [ImageProfile::Debug, ImageProfile::Installer] {
            assert_eq!(
                ImageKind::new(profile, VolumeProtection::Plaintext),
                Ok(ImageKind::plaintext(profile))
            );
            assert_eq!(
                ImageKind::plaintext(profile).root(),
                VolumeProtection::Plaintext
            );
            assert_eq!(ImageKind::plaintext(profile).profile(), profile);
        }
    }

    #[test]
    fn the_root_key_file_is_owner_only_even_when_it_already_existed() {
        let dir = std::env::temp_dir().join(format!("mkimage-rootkey-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("image.rootkey");
        std::fs::write(&path, b"stale").expect("a pre-existing file");
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o644))
            .expect("world-readable to begin with");
        let unlock = RootUnlock {
            descriptor: UnlockDescriptor::provision(UNLOCK_DEFAULT_ITERATIONS, &mut TestEntropy(3))
                .expect("provision"),
            key: [0xA7; VOLUME_KEY_LEN],
        };

        write_root_key(&path, &unlock).expect("the key file is written");

        let body = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(body, format!("{}\n", "a7".repeat(VOLUME_KEY_LEN)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_replaced_image_leaves_a_running_reader_on_the_disk_it_opened() {
        use std::io::Read as _;
        let dir = std::env::temp_dir().join(format!("mkimage-replace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("disk.img");
        std::fs::write(&path, b"the image a guest is running").expect("an earlier image");
        let mut running = std::fs::File::open(&path).expect("a guest's open disk");
        let staged = dir.join("disk.img.part");
        std::fs::write(&staged, b"a build cut short").expect("a stale sibling");

        replace_file(&path, b"the rebuilt image").expect("replaced");

        let mut seen = String::new();
        running
            .read_to_string(&mut seen)
            .expect("the guest reads on");
        assert_eq!(seen, "the image a guest is running");
        assert_eq!(
            std::fs::read(&path).expect("read back"),
            b"the rebuilt image"
        );
        assert!(!staged.exists(), "nothing is left beside the image");
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    /// A second writer of one output waits for the first to finish, so
    /// neither renames the other's half-written sibling over it.
    #[test]
    fn writers_of_one_output_take_turns() {
        use std::sync::mpsc;
        use std::time::Duration;
        let dir = std::env::temp_dir().join(format!("mkimage-turns-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("disk.img");
        let first = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("disk.img.lock"))
            .expect("the first writer's turn");
        first.lock().expect("held");
        let (done, finished) = mpsc::channel();
        let second = {
            let path = path.clone();
            std::thread::spawn(move || {
                replace_file(&path, b"second").expect("written in its turn");
                let _ = done.send(());
            })
        };
        assert!(
            finished.recv_timeout(Duration::from_millis(200)).is_err(),
            "it waits its turn"
        );
        assert!(!path.exists());
        drop(first);
        finished
            .recv_timeout(Duration::from_secs(30))
            .expect("its turn came");
        second.join().expect("the second writer ends");
        assert_eq!(std::fs::read(&path).expect("read back"), b"second");
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn only_a_regular_file_is_replaced() {
        let dir = std::env::temp_dir().join(format!("mkimage-irregular-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a directory")).expect("scratch dir");
        let refused = replace_file(&dir.join("a directory"), b"image").expect_err("a directory");
        assert_eq!(refused.kind(), std::io::ErrorKind::InvalidInput);
        #[cfg(unix)]
        {
            let target = dir.join("elsewhere");
            std::fs::write(&target, b"untouched").expect("a link's target");
            let link = dir.join("linked.img");
            std::os::unix::fs::symlink(&target, &link).expect("a link");
            let refused = replace_file(&link, b"image").expect_err("a link");
            assert_eq!(refused.kind(), std::io::ErrorKind::InvalidInput);
            assert_eq!(std::fs::read(&target).expect("read"), b"untouched");
            assert!(std::fs::symlink_metadata(&link)
                .expect("the link stays")
                .file_type()
                .is_symlink());
        }
        assert!(!dir.join("a directory.part").exists());
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_plaintext_rebuild_removes_the_key_file_an_encrypted_build_left() {
        let dir = std::env::temp_dir().join(format!("mkimage-stalekey-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("image.rootkey");
        std::fs::write(&path, b"a key for an image no longer there").expect("a stale key file");

        assert!(remove_stale_root_key(&path).expect("removed"));
        assert!(!path.exists());
        assert!(!remove_stale_root_key(&path).expect("absent is no error"));
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_debug_image_seeds_a_root_account_that_authenticates() {
        let built = build_rpi_image(
            &test_kernel_elf(),
            &test_firmware(),
            &mut TestEntropy(9),
            ImageKind::plaintext(ImageProfile::Debug),
            &[],
            &[],
            &test_network_conf(),
        )
        .expect("image builds");

        let root_bytes = root_partition(&built.image);
        let mut rfs = ARXFS::open(
            MemBlock::from_bytes(root_bytes).expect("whole sectors"),
            Keying::Plaintext,
        )
        .expect("root partition mounts");
        let arxfs_root = rfs.root();
        let system = rfs.lookup(arxfs_root, b"System").expect("/System");
        let security = rfs.lookup(system, b"Security").expect("Security");

        let users = rfs
            .lookup(security, rootfs::USERS_DB_NAME.as_bytes())
            .expect("Users database exists");
        let mut buf = vec![0u8; tairix_users::MAX_DB_LEN];
        let read = rfs
            .read_at(users, 0, &mut buf)
            .expect("Users database reads");
        let text = core::str::from_utf8(&buf[..read]).expect("valid UTF-8");
        let db = UsersDb::parse(text).expect("seeded database parses");

        let record = db
            .authenticate(DEBUG_USERNAME, DEBUG_PASSWORD.as_bytes())
            .expect("root/root authenticates");
        // The debug administrator is an ordinary user-band principal: uid 0
        // belongs to the compiled-in `system` identity, never to a record
        // on disk.
        assert_eq!(record.uid(), DEBUG_UID);
        assert_eq!(record.shell(), Some("/System/Commands/elsh.app/Run"));
        // The seeded grant is exactly the shared administrator ceiling
        // (session baseline + administrative set) — the B3 regression
        // (`plans/CAPABILITY_USE.md` CU3): a root account without
        // `CAP_FS_ACCESS` cannot use the filesystem even once the
        // intersection is wired.
        assert_eq!(record.capabilities(), tairix_users::administrator_ceiling());
        assert!(record.capabilities().contains(CapabilityId::FS_ACCESS));
        assert!(db.authenticate(DEBUG_USERNAME, b"wrong").is_err());

        // The on-disk database holds the one human debug account and
        // nothing else: the system/service identity is compiled into the
        // kernel and never written to disk, and the kernel's identity
        // merge would refuse any on-disk record colliding with it
        // (`plans/USERS.md`).
        assert_eq!(db.records().len(), 1);
        for account in tairix_users::system_accounts().expect("compiled accounts build") {
            assert!(
                db.lookup(account.username()).is_none(),
                "compiled account {} must never be seeded on disk",
                account.username()
            );
        }
    }

    #[test]
    fn an_installer_image_seeds_an_empty_users_database() {
        let built = build_rpi_image(
            &test_kernel_elf(),
            &test_firmware(),
            &mut TestEntropy(9),
            ImageKind::plaintext(ImageProfile::Installer),
            &[],
            &[],
            &test_network_conf(),
        )
        .expect("image builds");

        let root_bytes = root_partition(&built.image);
        let mut rfs = ARXFS::open(
            MemBlock::from_bytes(root_bytes).expect("whole sectors"),
            Keying::Plaintext,
        )
        .expect("root partition mounts");
        let arxfs_root = rfs.root();
        let system = rfs.lookup(arxfs_root, b"System").expect("/System");
        let security = rfs.lookup(system, b"Security").expect("Security");

        let users = rfs
            .lookup(security, rootfs::USERS_DB_NAME.as_bytes())
            .expect("Users database exists");
        let mut buf = vec![0u8; tairix_users::MAX_DB_LEN];
        let read = rfs
            .read_at(users, 0, &mut buf)
            .expect("Users database reads");
        let text = core::str::from_utf8(&buf[..read]).expect("valid UTF-8");
        let db = UsersDb::parse(text).expect("seeded database parses");

        // An empty database — no debug account and nothing that can start
        // a session: the installer authors the first human user on first
        // boot, and the system/service identity is compiled into the
        // kernel, never written to disk (`plans/USERS.md`).
        assert!(db.records().is_empty());
        assert!(db.lookup(DEBUG_USERNAME).is_none());

        // The matching group registry carries only the well-known
        // removable-storage group — no debug group and no compiled system
        // group.
        let groups = rfs
            .lookup(security, rootfs::GROUPS_DB_NAME.as_bytes())
            .expect("Groups registry exists");
        let mut buf = vec![0u8; tairix_users::MAX_GROUPS_DB_LEN];
        let read = rfs
            .read_at(groups, 0, &mut buf)
            .expect("Groups registry reads");
        let text = core::str::from_utf8(&buf[..read]).expect("valid UTF-8");
        let registry = tairix_users::GroupsDb::parse(text).expect("seeded registry parses");
        assert_eq!(registry.records().len(), 1);
        assert!(registry.lookup(DEBUG_GROUP).is_none());
        assert_eq!(
            registry
                .lookup(tairix_users::STORAGE_GROUP)
                .map(GroupRecord::gid),
            Some(STORAGE_GID)
        );
    }

    #[test]
    fn a_debug_image_seeds_a_group_registry_the_root_account_resolves_against() {
        use tairix_users::GroupsDb;

        let built = build_rpi_image(
            &test_kernel_elf(),
            &test_firmware(),
            &mut TestEntropy(9),
            ImageKind::plaintext(ImageProfile::Debug),
            &[],
            &[],
            &test_network_conf(),
        )
        .expect("image builds");

        let root_bytes = root_partition(&built.image);
        let mut rfs = ARXFS::open(
            MemBlock::from_bytes(root_bytes).expect("whole sectors"),
            Keying::Plaintext,
        )
        .expect("root partition mounts");
        let arxfs_root = rfs.root();
        let system = rfs.lookup(arxfs_root, b"System").expect("/System");
        let security = rfs.lookup(system, b"Security").expect("Security");

        let groups = rfs
            .lookup(security, rootfs::GROUPS_DB_NAME.as_bytes())
            .expect("Groups registry exists");
        let mut buf = vec![0u8; tairix_users::MAX_GROUPS_DB_LEN];
        let read = rfs
            .read_at(groups, 0, &mut buf)
            .expect("Groups registry reads");
        let text = core::str::from_utf8(&buf[..read]).expect("valid UTF-8");
        let db = GroupsDb::parse(text).expect("seeded registry parses");
        // The seeded root account's primary gid must resolve to a real group,
        // or the kernel's identity-table build would fail closed.
        assert!(db.lookup_gid(DEBUG_PRIMARY_GID).is_some());
        assert_eq!(
            db.lookup(DEBUG_GROUP).map(GroupRecord::gid),
            Some(DEBUG_PRIMARY_GID)
        );
        // The well-known removable-storage group ships too, so the unlock
        // resolves it by name and arms the hotplug-volume identity map.
        assert_eq!(
            db.lookup(STORAGE_GROUP).map(GroupRecord::gid),
            Some(STORAGE_GID)
        );
        // The `system` and `services` groups are compiled into the kernel
        // with the system accounts, never seeded to disk — a reserved
        // record here would be refused by the kernel's identity merge.
        assert!(db.lookup(tairix_users::SYSTEM_GROUP).is_none());
        assert!(db.lookup(tairix_users::SERVICES_GROUP).is_none());
    }

    #[test]
    fn a_bad_kernel_fails_the_whole_build() {
        assert!(build_rpi_image(
            b"not an elf",
            &test_firmware(),
            &mut TestEntropy(9),
            ImageKind::plaintext(ImageProfile::Installer),
            &[],
            &[],
            &test_network_conf(),
        )
        .is_err());
    }

    #[test]
    fn generated_config_uses_the_architecture_console_baud() {
        assert_eq!(CONSOLE_BAUD, 115_200);
        assert!(fatboot::config_txt(false, CONSOLE_BAUD).contains("init_uart_baud=115200\n"));
    }

    #[test]
    fn image_profiles_have_one_spelling_each() {
        assert_eq!(ImageProfile::from_label("debug"), Some(ImageProfile::Debug));
        assert_eq!(
            ImageProfile::from_label("installer"),
            Some(ImageProfile::Installer)
        );
        assert_eq!(ImageProfile::from_label("Debug"), None);
        assert_eq!(ImageProfile::from_label(""), None);
        assert_eq!(ImageProfile::Debug.label(), "debug");
        assert_eq!(ImageProfile::Installer.label(), "installer");
    }

    #[test]
    fn volume_key_renders_as_hex() {
        let text = volume_key_to_hex(&TEST_KEY);
        assert_eq!(text.len(), VOLUME_KEY_LEN * 2 + 1);
        assert!(text.ends_with(b"\n"));
        assert!(text[..VOLUME_KEY_LEN * 2].iter().all(u8::is_ascii_hexdigit));
        assert!(text.starts_with(b"4242"));
    }

    #[test]
    fn host_entropy_draws_nonzero_bytes() {
        let mut out = [0u8; 64];
        HostEntropy.fill(&mut out).expect("host entropy available");
        assert!(out.iter().any(|&b| b != 0));
    }
}
