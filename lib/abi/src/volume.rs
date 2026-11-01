//! Runtime volume attach/detach request frames (`plans/DEVICES.md` D3b).
//!
//! A volume manager that has probed a hot-pluggable block source — a
//! [`crate::blkio`] block-service endpoint plus its shared data window,
//! both inherited as grants from the storage node the block driver emitted
//! — asks the kernel to **attach** a filesystem driver to it with a
//! [`VolumeAttachRequest`] (the `volume_attach` syscall) and to take it
//! back out of service with a [`VolumeDetachRequest`] (`volume_detach`).
//! An attach requires `CAP_FS_MOUNT`; a detach requires it too, unless the
//! caller owns the network or disk-image volume it names. Both are audited;
//! every field is validated fail-closed here *and* re-checked kernel-side
//! against live state (the endpoint, the window, the device geometry) before
//! anything is mounted.
//!
//! The frames are fixed-shape and bounded: the attach request carries the
//! partition extent the caller probed (`lib/partition`), the filesystem
//! type its signature probe matched, and the catalog name the volume's
//! root is projected under (`/Storage/<name>` — the `Storage:` catalog
//! view location). Naming *policy* (label sanitisation, collision
//! fingerprints) is the volume manager's job; this layer enforces only
//! the structural spelling bounds so a malformed name can never reach the
//! mount table.
//!
//! The volume a user-space filesystem driver serves (`plans/FSSERVE.md` §4)
//! is reached through `volume_read`, `volume_write` and `volume_ctl`, whose
//! control frame ([`VolumeCtl`]) and description ([`VolumeInfo`]) live here
//! too.

use crate::blkio::{BlkMedium, BLK_MEDIUM_LEN};
use crate::driver::block::{DiscLayout, DiscardCapability, MAX_BLOCK_SIZE, MIN_BLOCK_SIZE};
use crate::driver::filesystem::MountFlags;
use crate::le::{exact_len, put_u16, put_u32, put_u64, read_u16, read_u32, read_u64};
use crate::sysinfo::MOUNT_TARGET_MAX;
use crate::Errno;

/// The filesystem drivers a volume can be attached with. Closed set: an
/// unknown discriminant is refused at decode (fail closed), and a new
/// filesystem extends this enum together with the kernel service that
/// opens it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum VolumeFsType {
    /// The native filesystem (`drivers/filesystem/arxfs`).
    ARXFS = 0,
    /// ext2/ext3/ext4 (`drivers/filesystem/ext4`).
    Ext4 = 1,
    /// FAT32 (`drivers/filesystem/fat32`).
    Fat32 = 2,
    /// ISO 9660 and its High Sierra predecessor, with the Joliet, Rock Ridge,
    /// RISC OS, Apple, Amiga, XA and zisofs extensions
    /// (`drivers/filesystem/iso9660`).
    Iso9660 = 3,
    /// UDF 1.02–2.60 (`drivers/filesystem/udf`).
    Udf = 4,
    /// Acorn ADFS / RISC OS `FileCore` (`drivers/filesystem/adfs`).
    Adfs = 5,
    /// The AmigaDOS filesystems, OFS and FFS (`drivers/filesystem/affs`).
    Affs = 6,
}

impl VolumeFsType {
    /// Every filesystem type, in wire order.
    pub const ALL: [Self; 7] = [
        Self::ARXFS,
        Self::Ext4,
        Self::Fat32,
        Self::Iso9660,
        Self::Udf,
        Self::Adfs,
        Self::Affs,
    ];

    /// The wire byte for this filesystem type.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Recover a filesystem type from its wire byte.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] if `value` names no known filesystem type.
    pub const fn from_u8(value: u8) -> Result<Self, Errno> {
        match value {
            0 => Ok(Self::ARXFS),
            1 => Ok(Self::Ext4),
            2 => Ok(Self::Fat32),
            3 => Ok(Self::Iso9660),
            4 => Ok(Self::Udf),
            5 => Ok(Self::Adfs),
            6 => Ok(Self::Affs),
            _ => Err(Errno::OutOfRange),
        }
    }

    /// The `compatible` key of the volume node the kernel publishes for a
    /// volume of this format, which that format's driver binds by.
    #[must_use]
    pub const fn served_compatible(self) -> &'static [u8] {
        match self {
            Self::ARXFS => b"tairix,volume-arxfs",
            Self::Ext4 => b"tairix,volume-ext4",
            Self::Fat32 => b"tairix,volume-fat32",
            Self::Iso9660 => b"tairix,volume-iso9660",
            Self::Udf => b"tairix,volume-udf",
            Self::Adfs => b"tairix,volume-adfs",
            Self::Affs => b"tairix,volume-affs",
        }
    }
}

/// A set of filesystem types, one bit a [`VolumeFsType`]'s wire value: the
/// filesystems a caller accepts a volume holding. Never empty.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct VolumeFsTypes(u32);

impl VolumeFsTypes {
    /// The set of `types`, or `None` where there are none.
    #[must_use]
    pub fn of(types: impl IntoIterator<Item = VolumeFsType>) -> Option<Self> {
        let bits = types
            .into_iter()
            .fold(0, |bits, fstype| bits | 1 << fstype.as_u8());
        (bits != 0).then_some(Self(bits))
    }

    /// Whether the set holds `fstype`.
    #[must_use]
    pub const fn contains(self, fstype: VolumeFsType) -> bool {
        self.0 & 1 << fstype.as_u8() != 0
    }

    /// The set's wire bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// The set `bits` spells.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a bit naming no filesystem type, or none
    /// set.
    pub const fn from_bits(bits: u32) -> Result<Self, Errno> {
        let known = (1 << VolumeFsType::ALL.len()) - 1;
        if bits == 0 || bits & !known != 0 {
            return Err(Errno::OutOfRange);
        }
        Ok(Self(bits))
    }
}

/// What the mount table calls a volume: its format, and for a format with
/// generations, the one this volume is; or, for a network volume, the protocol
/// it is served over.
///
/// A closed set, so the name a user-space driver gives the kernel is a code
/// the kernel holds to the format the volume was attached as, and a server
/// offering a network volume can never name a local format.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FormatName {
    /// The native filesystem.
    Arxfs = 1,
    /// ext2: no journal, and no feature past ext3's.
    Ext2 = 2,
    /// ext3: a journal, and no feature past ext3's.
    Ext3 = 3,
    /// ext4: a feature past ext3's.
    Ext4 = 4,
    /// FAT32.
    Fat32 = 5,
    /// ISO 9660.
    Iso9660 = 6,
    /// UDF.
    Udf = 7,
    /// Acorn ADFS.
    Adfs = 8,
    /// A share served over SMB 3.
    Smb3 = 9,
    /// An AmigaDOS volume, OFS or FFS.
    Affs = 10,
}

impl FormatName {
    /// Every name, in wire order.
    pub const ALL: [Self; 10] = [
        Self::Arxfs,
        Self::Ext2,
        Self::Ext3,
        Self::Ext4,
        Self::Fat32,
        Self::Iso9660,
        Self::Udf,
        Self::Adfs,
        Self::Smb3,
        Self::Affs,
    ];

    /// The format a user and `mount` call `name`, as [`as_str`](Self::as_str)
    /// spells it, exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|format| format.as_str() == name)
    }

    /// The name as a user and `mount` know it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Arxfs => "arxfs",
            Self::Ext2 => "ext2",
            Self::Ext3 => "ext3",
            Self::Ext4 => "ext4",
            Self::Fat32 => "fat32",
            Self::Iso9660 => "iso9660",
            Self::Udf => "udf",
            Self::Adfs => "adfs",
            Self::Smb3 => "smb3",
            Self::Affs => "affs",
        }
    }

    /// The format family a local volume of this name is probed as, or `None`
    /// for a network protocol, which no local volume carries.
    #[must_use]
    pub const fn family(self) -> Option<VolumeFsType> {
        Some(match self {
            Self::Arxfs => VolumeFsType::ARXFS,
            Self::Ext2 | Self::Ext3 | Self::Ext4 => VolumeFsType::Ext4,
            Self::Fat32 => VolumeFsType::Fat32,
            Self::Iso9660 => VolumeFsType::Iso9660,
            Self::Udf => VolumeFsType::Udf,
            Self::Adfs => VolumeFsType::Adfs,
            Self::Affs => VolumeFsType::Affs,
            Self::Smb3 => return None,
        })
    }

    /// The wire code.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// The name `raw` codes.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a code naming none.
    pub const fn from_u8(raw: u8) -> Result<Self, Errno> {
        Ok(match raw {
            1 => Self::Arxfs,
            2 => Self::Ext2,
            3 => Self::Ext3,
            4 => Self::Ext4,
            5 => Self::Fat32,
            6 => Self::Iso9660,
            7 => Self::Udf,
            8 => Self::Adfs,
            9 => Self::Smb3,
            10 => Self::Affs,
            _ => return Err(Errno::OutOfRange),
        })
    }
}

/// Maximum byte length of a volume's catalog name. A fixed security bound
/// on untrusted input, not a growable capacity: catalog names are short
/// human labels, and the mount-table spelling below `/Storage/` must stay
/// bounded.
pub const VOLUME_NAME_MAX: usize = 32;

/// Byte length of the 16-byte volume identity every detach names — the
/// same stable identity the volume forest publishes for `id::` paths.
pub const VOLUME_ID_LEN: usize = 16;

/// Fixed prefix of a [`VolumeAttachRequest`] frame:
/// `endpoint(8) || window(8) || first_lba(8) || blocks(8) || fstype(1) ||
/// name_len(1)`, followed by exactly `name_len` name bytes.
const ATTACH_FIXED_LEN: usize = 8 + 8 + 8 + 8 + 1 + 1;

/// Maximum encoded length of a [`VolumeAttachRequest`].
pub const VOLUME_ATTACH_MAX_LEN: usize = ATTACH_FIXED_LEN + VOLUME_NAME_MAX;

/// Encoded length of a [`VolumeDetachRequest`]: the volume identity plus
/// the [`DetachMode`] byte.
pub const VOLUME_DETACH_LEN: usize = VOLUME_ID_LEN + 1;

/// The most one `volume_read` or `volume_write` moves: one served window,
/// so a driver reads the data a call asks for in one transfer.
pub const VOLUME_IO_MAX: usize = crate::fsserve::FSSERVE_WINDOW_LEN;

/// What a `volume_ctl` call asks of a served volume.
#[repr(u32)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum VolumeCtlOp {
    /// Describe the volume: a [`VolumeInfo`].
    Info = 1,
    /// Make every write the volume accepted durable on its medium.
    Flush = 2,
    /// Tell the device `blocks` blocks from `lba` no longer hold data.
    Discard = 3,
}

/// A `volume_ctl` request: the operation and, for a discard, its range.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct VolumeCtl {
    /// The operation.
    pub op: VolumeCtlOp,
    /// The first block of a discard; zero otherwise.
    pub lba: u64,
    /// How many blocks a discard covers; zero otherwise.
    pub blocks: u64,
}

/// Encoded length of a [`VolumeCtl`]: `op(4) || pad(4) || lba(8) ||
/// blocks(8)`.
pub const VOLUME_CTL_LEN: usize = 24;

impl VolumeCtl {
    /// Encode the request.
    #[must_use]
    pub fn encode(&self) -> [u8; VOLUME_CTL_LEN] {
        let mut out = [0u8; VOLUME_CTL_LEN];
        put_u32(&mut out, 0, self.op as u32);
        put_u64(&mut out, 8, self.lba);
        put_u64(&mut out, 16, self.blocks);
        out
    }

    /// Decode a request.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for any length but [`VOLUME_CTL_LEN`];
    /// [`Errno::OutOfRange`] for an unknown operation, a nonzero pad, a
    /// range on an operation that takes none, or an empty discard.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        exact_len(bytes, VOLUME_CTL_LEN).map_err(|_| Errno::LengthOutOfRange)?;
        let op = match read_u32(bytes, 0) {
            1 => VolumeCtlOp::Info,
            2 => VolumeCtlOp::Flush,
            3 => VolumeCtlOp::Discard,
            _ => return Err(Errno::OutOfRange),
        };
        if read_u32(bytes, 4) != 0 {
            return Err(Errno::OutOfRange);
        }
        let ctl = Self {
            op,
            lba: read_u64(bytes, 8),
            blocks: read_u64(bytes, 16),
        };
        let ranged = op == VolumeCtlOp::Discard;
        if ranged == (ctl.blocks == 0) || (!ranged && ctl.lba != 0) {
            return Err(Errno::OutOfRange);
        }
        Ok(ctl)
    }
}

/// What a served volume is: the `volume_ctl` description a driver builds its
/// block device from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct VolumeInfo {
    /// The block size: a power of two the block layer accepts.
    pub block_size: u32,
    /// How many blocks the volume spans.
    pub block_count: u64,
    /// The volume may not be written.
    pub read_only: bool,
    /// What the device can discard.
    pub discard: DiscardCapability,
    /// Where the disc keeps its newest structures, for an optical medium.
    pub disc_layout: Option<DiscLayout>,
    /// What the device said its medium is.
    pub medium: Option<BlkMedium>,
}

/// Encoded length of a [`VolumeInfo`]: `block_size(4) || flags(4) ||
/// block_count(8) || discard granularity(8) || discard maximum(8) ||
/// last_session_start(8) || last_recorded(8) || medium(8)`.
pub const VOLUME_INFO_LEN: usize = 56;

const INFO_READ_ONLY: u32 = 1 << 0;
const INFO_DISCARD: u32 = 1 << 1;
const INFO_LAYOUT: u32 = 1 << 2;
const INFO_RECORDED: u32 = 1 << 3;

impl VolumeInfo {
    /// Encode the description.
    #[must_use]
    pub fn encode(&self) -> [u8; VOLUME_INFO_LEN] {
        let mut out = [0u8; VOLUME_INFO_LEN];
        let mut flags = 0;
        if self.read_only {
            flags |= INFO_READ_ONLY;
        }
        put_u32(&mut out, 0, self.block_size);
        put_u64(&mut out, 8, self.block_count);
        if self.discard.supported {
            flags |= INFO_DISCARD;
            put_u64(&mut out, 16, self.discard.granularity_blocks);
            put_u64(&mut out, 24, self.discard.max_blocks_per_request);
        }
        if let Some(layout) = self.disc_layout {
            flags |= INFO_LAYOUT;
            put_u64(&mut out, 32, layout.last_session_start);
            if let Some(recorded) = layout.last_recorded {
                flags |= INFO_RECORDED;
                put_u64(&mut out, 40, recorded);
            }
        }
        out[48..].copy_from_slice(&BlkMedium::encode(self.medium));
        put_u32(&mut out, 4, flags);
        out
    }

    /// Decode a description.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for any length but [`VOLUME_INFO_LEN`];
    /// [`Errno::OutOfRange`] for a block size outside the block layer's
    /// range, an empty volume, an unknown flag, or a field its flag says is
    /// absent but which is not zero.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        exact_len(bytes, VOLUME_INFO_LEN).map_err(|_| Errno::LengthOutOfRange)?;
        let block_size = read_u32(bytes, 0);
        let flags = read_u32(bytes, 4);
        let block_count = read_u64(bytes, 8);
        if !block_size.is_power_of_two()
            || !(MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&block_size)
            || block_count == 0
            || flags & !(INFO_READ_ONLY | INFO_DISCARD | INFO_LAYOUT | INFO_RECORDED) != 0
            || (flags & INFO_RECORDED != 0 && flags & INFO_LAYOUT == 0)
        {
            return Err(Errno::OutOfRange);
        }
        let absent = |set: u32, at: usize| flags & set != 0 || read_u64(bytes, at) == 0;
        if !absent(INFO_DISCARD, 16)
            || !absent(INFO_DISCARD, 24)
            || !absent(INFO_LAYOUT, 32)
            || !absent(INFO_RECORDED, 40)
        {
            return Err(Errno::OutOfRange);
        }
        let discard = if flags & INFO_DISCARD == 0 {
            DiscardCapability::unsupported()
        } else {
            DiscardCapability {
                supported: true,
                granularity_blocks: read_u64(bytes, 16),
                max_blocks_per_request: read_u64(bytes, 24),
            }
        };
        let disc_layout = (flags & INFO_LAYOUT != 0).then(|| DiscLayout {
            last_session_start: read_u64(bytes, 32),
            last_recorded: (flags & INFO_RECORDED != 0).then(|| read_u64(bytes, 40)),
        });
        let mut medium = [0u8; BLK_MEDIUM_LEN];
        medium.copy_from_slice(&bytes[48..]);
        Ok(Self {
            block_size,
            block_count,
            read_only: flags & INFO_READ_ONLY != 0,
            discard,
            disc_layout,
            medium: BlkMedium::decode(&medium),
        })
    }
}

/// `true` if `byte` may begin or end a volume catalog name: a lowercase
/// ASCII letter or a digit, as the alias grammar asks of an alias.
const fn name_edge_ok(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

/// `true` if `byte` may appear inside a volume catalog name: the alias
/// character set, lowercase ASCII letters, digits, `-` and `_`. Anything else
/// — separators, dots, control bytes, non-ASCII — is refused, so a name can
/// never spell a path component like `..`, an empty segment, or a terminal
/// escape.
const fn name_byte_ok(byte: u8) -> bool {
    name_edge_ok(byte) || byte == b'-' || byte == b'_'
}

/// Validate a volume catalog name's spelling: an alias, as the alias grammar
/// spells one (`plans/ALIAS.md` §5.2).
///
/// # Errors
///
/// [`Errno::OutOfRange`] when the name is empty, longer than
/// [`VOLUME_NAME_MAX`], contains a byte outside the alias character set, or
/// begins or ends with anything but a lowercase letter or a digit.
pub fn validate_volume_name(name: &[u8]) -> Result<(), Errno> {
    let (Some(&first), Some(&last)) = (name.first(), name.last()) else {
        return Err(Errno::OutOfRange);
    };
    if name.len() > VOLUME_NAME_MAX || !name_edge_ok(first) || !name_edge_ok(last) {
        return Err(Errno::OutOfRange);
    }
    let mut i = 0;
    while i < name.len() {
        if !name_byte_ok(name[i]) {
            return Err(Errno::OutOfRange);
        }
        i += 1;
    }
    Ok(())
}

/// One runtime volume-attach request: attach `fstype` to the block extent
/// `[first_lba, first_lba + blocks)` served by the block-service
/// `endpoint` + shared data `window`, and publish its root under the
/// catalog name `name`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct VolumeAttachRequest<'a> {
    /// The block-service call endpoint id (the storage node's endpoint
    /// grant).
    pub endpoint: u64,
    /// The shared data window's region id (the storage node's
    /// shared-memory grant).
    pub window: u64,
    /// First logical block of the partition extent the filesystem lives
    /// in. Zero for a whole-device filesystem.
    pub first_lba: u64,
    /// Length of the extent in logical blocks. Must be non-zero; the
    /// kernel re-checks the extent against the device's live geometry.
    pub blocks: u64,
    /// The filesystem driver to attach.
    pub fstype: VolumeFsType,
    /// The catalog name the volume's root is projected under
    /// (`/Storage/<name>`), already policy-derived by the caller and
    /// structurally validated here.
    pub name: &'a [u8],
}

impl<'a> VolumeAttachRequest<'a> {
    /// Encode `self` into `buf`, returning the number of bytes written.
    ///
    /// # Errors
    ///
    /// * [`Errno::OutOfRange`] for a structurally invalid name or a
    ///   zero-length extent — a frame that cannot mean what it says is
    ///   never produced.
    /// * [`Errno::BufferTooSmall`] if `buf` cannot hold the encoding.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, Errno> {
        validate_volume_name(self.name)?;
        if self.blocks == 0 {
            return Err(Errno::OutOfRange);
        }
        let len = ATTACH_FIXED_LEN + self.name.len();
        if buf.len() < len {
            return Err(Errno::BufferTooSmall);
        }
        put_u64(buf, 0, self.endpoint);
        put_u64(buf, 8, self.window);
        put_u64(buf, 16, self.first_lba);
        put_u64(buf, 24, self.blocks);
        buf[32] = self.fstype.as_u8();
        // Bounded by `VOLUME_NAME_MAX` (≤ 255), so the cast is exact.
        #[allow(clippy::cast_possible_truncation)]
        {
            buf[33] = self.name.len() as u8;
        }
        buf[ATTACH_FIXED_LEN..len].copy_from_slice(self.name);
        Ok(len)
    }

    /// Decode an attach request from `bytes`, validating every field.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] when `bytes` is shorter than the
    ///   fixed prefix, longer than [`VOLUME_ATTACH_MAX_LEN`], or does not
    ///   carry exactly `name_len` name bytes — a frame is exact, never
    ///   padded or truncated.
    /// * [`Errno::OutOfRange`] for an unknown filesystem type, a
    ///   zero-length extent, an extent whose end overflows, or a
    ///   structurally invalid name.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Errno> {
        if bytes.len() < ATTACH_FIXED_LEN || bytes.len() > VOLUME_ATTACH_MAX_LEN {
            return Err(Errno::LengthOutOfRange);
        }
        let endpoint = read_u64(bytes, 0);
        let window = read_u64(bytes, 8);
        let first_lba = read_u64(bytes, 16);
        let blocks = read_u64(bytes, 24);
        let fstype = VolumeFsType::from_u8(bytes[32])?;
        let name_len = usize::from(bytes[33]);
        if bytes.len() != ATTACH_FIXED_LEN + name_len {
            return Err(Errno::LengthOutOfRange);
        }
        if blocks == 0 || first_lba.checked_add(blocks).is_none() {
            return Err(Errno::OutOfRange);
        }
        let name = &bytes[ATTACH_FIXED_LEN..];
        validate_volume_name(name)?;
        Ok(Self {
            endpoint,
            window,
            first_lba,
            blocks,
            fstype,
            name,
        })
    }
}

/// How a [`VolumeDetachRequest`] takes its volume down.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum DetachMode {
    /// Flush first and fail closed.
    Plain = 0,
    /// The audited force-unmount.
    Force = 1,
    /// A force for a volume recorded lost, a plain detach for any other: the
    /// kernel decides from the volume's state as it stands, so a stale view
    /// can never force a healthy volume that took a lost one's identity.
    Lost = 2,
}

impl DetachMode {
    /// The wire byte for this mode.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Recover a mode from its wire byte.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] if `value` names no mode.
    pub const fn from_u8(value: u8) -> Result<Self, Errno> {
        match value {
            0 => Ok(Self::Plain),
            1 => Ok(Self::Force),
            2 => Ok(Self::Lost),
            _ => Err(Errno::OutOfRange),
        }
    }

    /// Whether a detach in this mode forces a volume that is, or is not,
    /// `lost`.
    #[must_use]
    pub const fn forces(self, lost: bool) -> bool {
        match self {
            Self::Plain => false,
            Self::Force => true,
            Self::Lost => lost,
        }
    }
}

/// One runtime volume-detach request: take the volume published under
/// `volume_id` out of service.
///
/// A plain detach flushes first and fails closed, each refusal its own errno:
/// [`Errno::PermissionDenied`] without authority over the volume;
/// [`Errno::NotFound`] for an identity not attached; [`Errno::Busy`] while a
/// descriptor, a working directory, an image on it or another operation
/// holds it; [`Errno::NotEmpty`] while a binding stands within it; a refused
/// flush's own errno; and, from its writes, the refusal
/// [`MountAvailability::plain_detach_refusal`](crate::sysinfo::MountAvailability::plain_detach_refusal)
/// names for a vanished or conflicted volume. A **forced** detach is the
/// deliberate, separately-audited force-unmount: it detaches the volumes
/// shown beneath first, revokes the descriptors still holding its nodes, and
/// retracts the volume even when nothing can be committed, discarding any
/// retained uncommitted writes; what it gave up is logged with its own audit
/// event. [`DetachMode`] says which a request is. None widens the authority
/// a detach needs.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct VolumeDetachRequest {
    /// The volume's stable 16-byte identity, as the forest published it.
    pub volume_id: [u8; VOLUME_ID_LEN],
    /// How the volume is taken down.
    pub mode: DetachMode,
}

impl VolumeDetachRequest {
    /// Encode `self` into `buf`, returning the number of bytes written.
    ///
    /// # Errors
    ///
    /// * [`Errno::OutOfRange`] for the reserved all-zero identity, which
    ///   is never published and so can never be detached.
    /// * [`Errno::BufferTooSmall`] if `buf` cannot hold
    ///   [`VOLUME_DETACH_LEN`] bytes.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, Errno> {
        if self.volume_id == [0u8; VOLUME_ID_LEN] {
            return Err(Errno::OutOfRange);
        }
        if buf.len() < VOLUME_DETACH_LEN {
            return Err(Errno::BufferTooSmall);
        }
        buf[..VOLUME_ID_LEN].copy_from_slice(&self.volume_id);
        buf[VOLUME_ID_LEN] = self.mode.as_u8();
        Ok(VOLUME_DETACH_LEN)
    }

    /// Decode a detach request from `bytes`.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] if `bytes` is not exactly
    ///   [`VOLUME_DETACH_LEN`] bytes.
    /// * [`Errno::OutOfRange`] for the reserved all-zero identity (fail
    ///   closed; it can never name a published volume), or a mode byte
    ///   naming no [`DetachMode`] (wire corruption is refused, never
    ///   guessed at).
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() != VOLUME_DETACH_LEN {
            return Err(Errno::LengthOutOfRange);
        }
        let mut volume_id = [0u8; VOLUME_ID_LEN];
        volume_id.copy_from_slice(&bytes[..VOLUME_ID_LEN]);
        if volume_id == [0u8; VOLUME_ID_LEN] {
            return Err(Errno::OutOfRange);
        }
        let mode = DetachMode::from_u8(bytes[VOLUME_ID_LEN])?;
        Ok(Self { volume_id, mode })
    }
}

/// The fixed part of a [`ViewBindRequest`]: the volume's identity, the
/// narrowing (4), and the subtree's and target's lengths (2 each).
const VIEW_BIND_FIXED_LEN: usize = VOLUME_ID_LEN + 8;

/// The largest [`ViewBindRequest`]: a subtree and a target each at
/// [`MOUNT_TARGET_MAX`], the bound a binding is listed within.
pub const VIEW_BIND_MAX: usize = VIEW_BIND_FIXED_LEN + 2 * MOUNT_TARGET_MAX;

/// A projection of an attached volume's subtree at a directory
/// (`view_bind`, `plans/NEW-NAMESPACE.md` NS-4a).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ViewBindRequest<'a> {
    /// The volume's stable identity, as the forest published it.
    pub volume_id: [u8; VOLUME_ID_LEN],
    /// The directory within the volume projected, `/` for its root.
    pub subtree: &'a str,
    /// Where it is projected: an absolute path under `/Users` or `/Storage`.
    pub target: &'a str,
    /// Flags the projection adds to its volume's; it can never remove one.
    pub narrowing: MountFlags,
}

impl<'a> ViewBindRequest<'a> {
    /// Encode the request into `out`, returning its length.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for the all-zero identity or a path that is not
    /// absolute, holds a control character, or passes [`MOUNT_TARGET_MAX`];
    /// [`Errno::BufferTooSmall`] when `out` cannot hold it.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        check_view_bind(self)?;
        let len = VIEW_BIND_FIXED_LEN + self.subtree.len() + self.target.len();
        let out = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
        out[..VOLUME_ID_LEN].copy_from_slice(&self.volume_id);
        put_u32(out, VOLUME_ID_LEN, self.narrowing.bits());
        let path_len = |path: &str| u16::try_from(path.len()).map_err(|_| Errno::OutOfRange);
        put_u16(out, VOLUME_ID_LEN + 4, path_len(self.subtree)?);
        put_u16(out, VOLUME_ID_LEN + 6, path_len(self.target)?);
        let paths = &mut out[VIEW_BIND_FIXED_LEN..];
        paths[..self.subtree.len()].copy_from_slice(self.subtree.as_bytes());
        paths[self.subtree.len()..].copy_from_slice(self.target.as_bytes());
        Ok(len)
    }

    /// Decode a request, failing closed on anything
    /// [`encode`](Self::encode) could not have written.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for an unknown flag or anything `encode`
    /// refuses; [`Errno::LengthOutOfRange`] for a frame that does not end
    /// where its target does.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Errno> {
        if bytes.len() < VIEW_BIND_FIXED_LEN || bytes.len() > VIEW_BIND_MAX {
            return Err(Errno::LengthOutOfRange);
        }
        let subtree_len = usize::from(read_u16(bytes, VOLUME_ID_LEN + 4));
        let target_len = usize::from(read_u16(bytes, VOLUME_ID_LEN + 6));
        if bytes.len() != VIEW_BIND_FIXED_LEN + subtree_len + target_len {
            return Err(Errno::LengthOutOfRange);
        }
        let mut volume_id = [0; VOLUME_ID_LEN];
        volume_id.copy_from_slice(&bytes[..VOLUME_ID_LEN]);
        let narrowing =
            MountFlags::from_bits(read_u32(bytes, VOLUME_ID_LEN)).map_err(|_| Errno::OutOfRange)?;
        let paths = &bytes[VIEW_BIND_FIXED_LEN..];
        let text = |bytes| core::str::from_utf8(bytes).map_err(|_| Errno::OutOfRange);
        let request = Self {
            volume_id,
            subtree: text(&paths[..subtree_len])?,
            target: text(&paths[subtree_len..])?,
            narrowing,
        };
        check_view_bind(&request)?;
        Ok(request)
    }
}

/// Whether `path` is an absolute path a binding can be listed with.
#[must_use]
pub fn is_binding_path(path: &str) -> bool {
    path.starts_with('/') && path.len() <= MOUNT_TARGET_MAX && !path.chars().any(char::is_control)
}

fn check_view_bind(request: &ViewBindRequest<'_>) -> Result<(), Errno> {
    if request.volume_id == [0; VOLUME_ID_LEN]
        || !is_binding_path(request.subtree)
        || !is_binding_path(request.target)
    {
        return Err(Errno::OutOfRange);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;
    use crate::blkio::BlkTransport;

    fn bind<'a>(subtree: &'a str, target: &'a str) -> ViewBindRequest<'a> {
        ViewBindRequest {
            volume_id: *b"0123456789abcdef",
            subtree,
            target,
            narrowing: MountFlags::READ_ONLY,
        }
    }

    #[test]
    fn a_view_binding_round_trips_and_the_largest_fits_its_bound() {
        let mut out = [0u8; VIEW_BIND_MAX];
        let request = bind("/", "/Users/ann/Home/nas");
        let len = request.encode(&mut out).unwrap();
        assert_eq!(ViewBindRequest::decode(&out[..len]), Ok(request));
        let path = alloc::string::String::from("/") + &"p".repeat(MOUNT_TARGET_MAX - 1);
        let largest = bind(&path, &path);
        assert_eq!(largest.encode(&mut out), Ok(VIEW_BIND_MAX));
        assert_eq!(ViewBindRequest::decode(&out), Ok(largest));
    }

    #[test]
    fn a_view_binding_out_of_shape_is_refused_both_ways() {
        let mut out = [0u8; VIEW_BIND_MAX];
        let long = alloc::string::String::from("/") + &"p".repeat(MOUNT_TARGET_MAX);
        for bad in [
            ViewBindRequest {
                volume_id: [0; VOLUME_ID_LEN],
                ..bind("/", "/Storage/x")
            },
            bind("docs", "/Storage/x"),
            bind("/", "Storage/x"),
            bind("/", &long),
            bind("/", "/Storage/\u{7}x"),
        ] {
            assert_eq!(bad.encode(&mut out), Err(Errno::OutOfRange), "{bad:?}");
        }
        assert_eq!(
            bind("/", "/Storage/x").encode(&mut [0; 8]),
            Err(Errno::BufferTooSmall)
        );
        let len = bind("/", "/Storage/x").encode(&mut out).unwrap();
        let good = &out[..len];
        let mut flag = good.to_vec();
        flag[VOLUME_ID_LEN] = 0x80;
        assert_eq!(ViewBindRequest::decode(&flag), Err(Errno::OutOfRange));
        let mut zero = good.to_vec();
        zero[..VOLUME_ID_LEN].fill(0);
        assert_eq!(ViewBindRequest::decode(&zero), Err(Errno::OutOfRange));
        let mut trailing = good.to_vec();
        trailing.push(b'x');
        assert_eq!(
            ViewBindRequest::decode(&trailing),
            Err(Errno::LengthOutOfRange)
        );
        assert_eq!(
            ViewBindRequest::decode(&good[..len - 1]),
            Err(Errno::LengthOutOfRange)
        );
    }

    #[test]
    fn a_control_request_round_trips_and_carries_a_range_only_to_a_discard() {
        for ctl in [
            VolumeCtl {
                op: VolumeCtlOp::Info,
                lba: 0,
                blocks: 0,
            },
            VolumeCtl {
                op: VolumeCtlOp::Flush,
                lba: 0,
                blocks: 0,
            },
            VolumeCtl {
                op: VolumeCtlOp::Discard,
                lba: 9,
                blocks: 7,
            },
        ] {
            assert_eq!(VolumeCtl::decode(&ctl.encode()), Ok(ctl));
        }
        let mut stray = VolumeCtl {
            op: VolumeCtlOp::Flush,
            lba: 0,
            blocks: 0,
        }
        .encode();
        stray[8] = 1;
        assert_eq!(VolumeCtl::decode(&stray), Err(Errno::OutOfRange));
        let empty = VolumeCtl {
            op: VolumeCtlOp::Discard,
            lba: 1,
            blocks: 0,
        }
        .encode();
        assert_eq!(VolumeCtl::decode(&empty), Err(Errno::OutOfRange));
        let mut unknown = empty;
        unknown[0] = 4;
        assert_eq!(VolumeCtl::decode(&unknown), Err(Errno::OutOfRange));
        assert_eq!(VolumeCtl::decode(&[0; 23]), Err(Errno::LengthOutOfRange));
    }

    #[test]
    fn a_volume_description_round_trips_every_optional_part() {
        let plain = VolumeInfo {
            block_size: 512,
            block_count: 1 << 30,
            read_only: false,
            discard: DiscardCapability::unsupported(),
            disc_layout: None,
            medium: None,
        };
        let full = VolumeInfo {
            block_size: 2048,
            block_count: 300_000,
            read_only: true,
            discard: DiscardCapability {
                supported: true,
                granularity_blocks: 8,
                max_blocks_per_request: 1 << 16,
            },
            disc_layout: Some(DiscLayout {
                last_session_start: 40_000,
                last_recorded: Some(299_999),
            }),
            medium: Some(BlkMedium::of_transport(BlkTransport::Usb)),
        };
        for info in [plain, full] {
            assert_eq!(VolumeInfo::decode(&info.encode()), Ok(info));
        }
    }

    #[test]
    fn a_volume_description_out_of_shape_is_refused() {
        type Corruption = (&'static str, fn(&mut [u8; VOLUME_INFO_LEN]));
        let good = VolumeInfo {
            block_size: 512,
            block_count: 8,
            read_only: false,
            discard: DiscardCapability::unsupported(),
            disc_layout: None,
            medium: None,
        }
        .encode();
        let cases: [Corruption; 5] = [
            ("block size", |b| {
                b[0..4].copy_from_slice(&3u32.to_le_bytes());
            }),
            ("empty volume", |b| b[8..16].fill(0)),
            ("unknown flag", |b| b[4] = 0x10),
            ("discard without its flag", |b| b[16] = 1),
            ("recorded without a layout", |b| b[4] = 0x08),
        ];
        for (what, corrupt) in cases {
            let mut bad = good;
            corrupt(&mut bad);
            assert_eq!(VolumeInfo::decode(&bad), Err(Errno::OutOfRange), "{what}");
        }
    }

    #[test]
    fn every_format_name_round_trips_into_its_family() {
        let coded: alloc::vec::Vec<FormatName> = (0..=u8::MAX)
            .filter_map(|raw| FormatName::from_u8(raw).ok())
            .collect();
        assert_eq!(
            coded,
            FormatName::ALL,
            "every code is listed, in wire order"
        );
        for name in FormatName::ALL {
            assert_eq!(FormatName::from_u8(name.as_u8()), Ok(name));
            assert_eq!(FormatName::from_name(name.as_str()), Some(name));
            assert_eq!(name.family().is_none(), name == FormatName::Smb3);
        }
        for unknown in ["", "ISO9660", "vfat", "iso9660 "] {
            assert_eq!(FormatName::from_name(unknown), None, "{unknown:?}");
        }
        assert_eq!(FormatName::Ext2.family(), Some(VolumeFsType::Ext4));
        assert_eq!(FormatName::Affs.family(), Some(VolumeFsType::Affs));
        assert_eq!(FormatName::from_u8(0), Err(Errno::OutOfRange));
        assert_eq!(FormatName::from_u8(11), Err(Errno::OutOfRange));
    }

    #[test]
    fn every_format_names_its_own_volume_node() {
        let keys = VolumeFsType::ALL.map(VolumeFsType::served_compatible);
        for (at, key) in keys.iter().enumerate() {
            assert!(key.starts_with(b"tairix,volume-"));
            assert!(!keys[..at].contains(key), "{key:?} named twice");
        }
    }

    fn attach() -> VolumeAttachRequest<'static> {
        VolumeAttachRequest {
            endpoint: 0x004D_5344_0000_0001,
            window: 7,
            first_lba: 2048,
            blocks: 1 << 21,
            fstype: VolumeFsType::Fat32,
            name: b"usb1",
        }
    }

    #[test]
    fn fstype_round_trips_and_rejects_unknown() {
        for (at, fstype) in VolumeFsType::ALL.into_iter().enumerate() {
            assert_eq!(usize::from(fstype.as_u8()), at);
            assert_eq!(VolumeFsType::from_u8(fstype.as_u8()), Ok(fstype));
        }
        assert_eq!(VolumeFsType::from_u8(7), Err(Errno::OutOfRange));
        assert_eq!(VolumeFsType::from_u8(u8::MAX), Err(Errno::OutOfRange));
    }

    #[test]
    fn attach_round_trips() {
        let req = attach();
        let mut buf = [0u8; VOLUME_ATTACH_MAX_LEN];
        let n = req.encode(&mut buf).expect("encodes");
        assert_eq!(n, ATTACH_FIXED_LEN + 4);
        assert_eq!(VolumeAttachRequest::decode(&buf[..n]), Ok(req));
    }

    #[test]
    fn attach_name_bounds_are_enforced_both_ways() {
        let mut buf = [0u8; VOLUME_ATTACH_MAX_LEN + 8];
        for bad in [
            &b""[..],
            &[b'a'; VOLUME_NAME_MAX + 1][..],
            b"a/b",
            b"a b",
            b"..",
            b"caf\xC3\xA9",
            b"Backup",
            b"-backup",
            b"_backup",
            b"backup-",
            b"backup_",
        ] {
            let req = VolumeAttachRequest {
                name: bad,
                ..attach()
            };
            assert_eq!(req.encode(&mut buf), Err(Errno::OutOfRange), "{bad:?}");
        }
        for good in [&b"my-backup_2"[..], b"7", b"usb_25cm"] {
            assert_eq!(validate_volume_name(good), Ok(()), "{good:?}");
        }
        // A maximum-length name is fine.
        let req = VolumeAttachRequest {
            name: &[b'a'; VOLUME_NAME_MAX],
            ..attach()
        };
        let n = req.encode(&mut buf).expect("encodes");
        assert_eq!(n, VOLUME_ATTACH_MAX_LEN);
        assert_eq!(VolumeAttachRequest::decode(&buf[..n]), Ok(req));
    }

    #[test]
    fn attach_decode_rejects_wrong_lengths() {
        let req = attach();
        let mut buf = [0u8; VOLUME_ATTACH_MAX_LEN];
        let n = req.encode(&mut buf).expect("encodes");
        // Truncated fixed prefix, truncated name, and trailing garbage all
        // fail closed.
        assert_eq!(
            VolumeAttachRequest::decode(&buf[..ATTACH_FIXED_LEN - 1]),
            Err(Errno::LengthOutOfRange)
        );
        assert_eq!(
            VolumeAttachRequest::decode(&buf[..n - 1]),
            Err(Errno::LengthOutOfRange)
        );
        assert_eq!(
            VolumeAttachRequest::decode(&buf[..=n]),
            Err(Errno::LengthOutOfRange)
        );
    }

    #[test]
    fn attach_decode_rejects_bad_fields() {
        let mut buf = [0u8; VOLUME_ATTACH_MAX_LEN];
        let n = attach().encode(&mut buf).expect("encodes");
        // Unknown filesystem type.
        let mut bad = buf;
        bad[32] = 9;
        assert_eq!(
            VolumeAttachRequest::decode(&bad[..n]),
            Err(Errno::OutOfRange)
        );
        // Zero-length extent.
        let mut bad = buf;
        put_u64(&mut bad, 24, 0);
        assert_eq!(
            VolumeAttachRequest::decode(&bad[..n]),
            Err(Errno::OutOfRange)
        );
        // Extent end overflows the block-address space.
        let mut bad = buf;
        put_u64(&mut bad, 16, u64::MAX);
        assert_eq!(
            VolumeAttachRequest::decode(&bad[..n]),
            Err(Errno::OutOfRange)
        );
        // A name byte outside the permitted set.
        let mut bad = buf;
        bad[ATTACH_FIXED_LEN] = b'/';
        assert_eq!(
            VolumeAttachRequest::decode(&bad[..n]),
            Err(Errno::OutOfRange)
        );
    }

    #[test]
    fn detach_round_trips_and_rejects_nil_and_wrong_lengths() {
        for mode in [DetachMode::Plain, DetachMode::Force, DetachMode::Lost] {
            let req = VolumeDetachRequest {
                volume_id: [7; 16],
                mode,
            };
            let mut buf = [0u8; VOLUME_DETACH_LEN];
            let n = req.encode(&mut buf).expect("encodes");
            assert_eq!(n, VOLUME_DETACH_LEN);
            assert_eq!(VolumeDetachRequest::decode(&buf[..n]), Ok(req));
        }

        let mut buf = [0u8; VOLUME_DETACH_LEN];
        assert_eq!(
            VolumeDetachRequest {
                volume_id: [0; 16],
                mode: DetachMode::Plain,
            }
            .encode(&mut buf),
            Err(Errno::OutOfRange)
        );
        assert_eq!(
            VolumeDetachRequest::decode(&[0u8; VOLUME_DETACH_LEN]),
            Err(Errno::OutOfRange)
        );
        let n = VolumeDetachRequest {
            volume_id: [7; 16],
            mode: DetachMode::Force,
        }
        .encode(&mut buf)
        .expect("encodes");
        assert_eq!(
            VolumeDetachRequest::decode(&buf[..n - 1]),
            Err(Errno::LengthOutOfRange)
        );
        // A mode byte naming no mode is wire corruption, refused whole.
        for byte in [3, 255] {
            let mut bad = buf;
            bad[VOLUME_ID_LEN] = byte;
            assert_eq!(
                VolumeDetachRequest::decode(&bad[..n]),
                Err(Errno::OutOfRange)
            );
        }
    }

    #[test]
    fn a_detach_taking_the_lost_forces_only_a_lost_volume() {
        for lost in [false, true] {
            assert!(!DetachMode::Plain.forces(lost));
            assert!(DetachMode::Force.forces(lost));
        }
        assert!(DetachMode::Lost.forces(true));
        assert!(!DetachMode::Lost.forces(false));
    }
}
