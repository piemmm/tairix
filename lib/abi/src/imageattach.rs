//! Image attachments (`plans/DISK-IMAGES.md`): the volume a disk-image file
//! holds, attached for the principal that hands the kernel the file.
//!
//! The caller opens the image and passes the descriptor to `image_attach`
//! with an [`ImageAttach`]: the catalog name it wants, how the volume is
//! presented, how writable it is to be, a directory it is also bound at, and
//! the filesystem it expects. The kernel recognises the file itself
//! and answers an [`ImageAttachReply`]: the volume it attached, the one the
//! principal had attached from that file already, or the [`ImageRefusal`]
//! saying why the file holds nothing it can attach.

use crate::le::exact_len;
use crate::netattach::{
    check_attach_fields, AttachFrame, AttachReply, Presentation, ATTACH_FLAG_READ_ONLY, ATTACH_MAX,
    ATTACH_REPLY_LEN,
};
use crate::volume::VolumeFsTypes;
use crate::Errno;

/// The version every [`ImageAttach`] frame opens with.
pub const IMAGE_ATTACH_VERSION: u16 = 1;

/// The largest [`ImageAttach`]: the attach frame every attach shares, whose
/// own word is here the filesystems expected.
pub const IMAGE_ATTACH_MAX: usize = ATTACH_MAX;

/// The [`ImageAttach`] flag saying the frame's own word is the
/// [`VolumeFsTypes`] the file must hold one of.
pub const IMAGE_ATTACH_FLAG_EXPECTS: u8 = 0x02;
/// The [`ImageAttach`] flag asking for [`ImageAccess::Writable`].
pub const IMAGE_ATTACH_FLAG_WRITABLE: u8 = 0x04;
/// The [`ImageAttach`] flag asking to take up a volume attached already
/// ([`ImageAttach::adopt`]).
pub const IMAGE_ATTACH_FLAG_ADOPT: u8 = 0x08;

const _: () = assert!(
    (ATTACH_FLAG_READ_ONLY
        | IMAGE_ATTACH_FLAG_EXPECTS
        | IMAGE_ATTACH_FLAG_WRITABLE
        | IMAGE_ATTACH_FLAG_ADOPT)
        .count_ones()
        == 4
);

/// How writable an image volume is to be.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ImageAccess {
    /// Read-only, whatever the file and its format allow.
    ReadOnly,
    /// Writable where the descriptor and the format allow it, else
    /// read-only.
    WritableWherePossible,
    /// Writable, or refused [`Errno::ReadOnly`] where the descriptor or the
    /// format is not.
    Writable,
}

impl ImageAccess {
    /// Whether a volume that is, or is not, `read_only` is as writable as
    /// this access asks.
    #[must_use]
    pub const fn accepts(self, read_only: bool) -> bool {
        match self {
            Self::ReadOnly => read_only,
            Self::WritableWherePossible => true,
            Self::Writable => !read_only,
        }
    }
}

/// How a principal asks for the volume an image file holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ImageAttach<'a> {
    /// The catalog name it must take in `/Storage`, or `None` for the first
    /// free one the naming policy gives the volume's label, else the file's
    /// name without its extension, else `virtual_<fingerprint>`; the reply
    /// names the one it took.
    pub name: Option<&'a str>,
    /// How its nodes are presented.
    pub presentation: Presentation,
    /// How writable it is to be.
    pub access: ImageAccess,
    /// The directory it is also bound at, under `/Users` or `/Storage`.
    pub target: Option<&'a str>,
    /// The filesystems the caller expects the file to hold one of: a claim
    /// the kernel holds to its own recognition, never one that chooses a
    /// driver.
    pub expects: Option<VolumeFsTypes>,
    /// Take up a volume of this file the caller has attached already, one as
    /// writable as `access` accepts, for the caller's login session too, so
    /// it lives until that session ends as well. Without it, or for a volume
    /// `access` does not accept, such an attach is answered and changes
    /// nothing.
    pub adopt: bool,
}

impl<'a> ImageAttach<'a> {
    /// Encode the attach into `out`, returning its length.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a name the catalog refuses, an explicit mode
    /// past the permission bits, or a target that is not an absolute path
    /// within [`MOUNT_TARGET_MAX`](crate::sysinfo::MOUNT_TARGET_MAX);
    /// [`Errno::BufferTooSmall`] when `out` cannot hold it.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        self.check()?;
        let mut flags = match self.access {
            ImageAccess::ReadOnly => ATTACH_FLAG_READ_ONLY,
            ImageAccess::WritableWherePossible => 0,
            ImageAccess::Writable => IMAGE_ATTACH_FLAG_WRITABLE,
        };
        if self.expects.is_some() {
            flags |= IMAGE_ATTACH_FLAG_EXPECTS;
        }
        if self.adopt {
            flags |= IMAGE_ATTACH_FLAG_ADOPT;
        }
        AttachFrame {
            version: IMAGE_ATTACH_VERSION,
            presentation: self.presentation,
            flags,
            own: self.expects.map_or(0, VolumeFsTypes::bits),
            name: self.name.unwrap_or(""),
            target: self.target,
        }
        .encode(out)
    }

    /// Decode an attach, failing closed on anything [`encode`](Self::encode)
    /// could not have written.
    ///
    /// # Errors
    ///
    /// [`Errno::AbiVersionUnsupported`] for another version;
    /// [`Errno::OutOfRange`] for an unknown presentation, flag or filesystem,
    /// read-only and writable both asked, a claim naming no filesystem,
    /// an owner or mode carried by a presentation that names none, a
    /// filesystem word with no claim, or a name, mode or target out of shape;
    /// [`Errno::LengthOutOfRange`] for a frame that does not end where its
    /// target does.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Errno> {
        let frame = AttachFrame::decode(
            bytes,
            IMAGE_ATTACH_VERSION,
            ATTACH_FLAG_READ_ONLY
                | IMAGE_ATTACH_FLAG_EXPECTS
                | IMAGE_ATTACH_FLAG_WRITABLE
                | IMAGE_ATTACH_FLAG_ADOPT,
        )?;
        let expects = match (frame.flags & IMAGE_ATTACH_FLAG_EXPECTS != 0, frame.own) {
            (true, bits) => Some(VolumeFsTypes::from_bits(bits)?),
            (false, 0) => None,
            (false, _) => return Err(Errno::OutOfRange),
        };
        let access = match frame.flags & (ATTACH_FLAG_READ_ONLY | IMAGE_ATTACH_FLAG_WRITABLE) {
            0 => ImageAccess::WritableWherePossible,
            ATTACH_FLAG_READ_ONLY => ImageAccess::ReadOnly,
            IMAGE_ATTACH_FLAG_WRITABLE => ImageAccess::Writable,
            _ => return Err(Errno::OutOfRange),
        };
        let attach = Self {
            name: (!frame.name.is_empty()).then_some(frame.name),
            presentation: frame.presentation,
            access,
            target: frame.target,
            expects,
            adopt: frame.flags & IMAGE_ATTACH_FLAG_ADOPT != 0,
        };
        attach.check()?;
        Ok(attach)
    }

    /// Whether its name, modes and target are each in shape.
    fn check(&self) -> Result<(), Errno> {
        check_attach_fields(self.name, self.presentation, self.target)
    }
}

/// Why an image file holds nothing the kernel will attach. A closed set, so
/// every caller can state the reason in its own words.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ImageRefusal {
    /// The file holds no filesystem the kernel recognises.
    Unrecognised = 1,
    /// It holds a filesystem other than the one the caller expected.
    NotExpected = 2,
    /// It holds a member of a RAID array, which is assembled, never attached
    /// on its own.
    RaidMember = 3,
    /// It holds a partitioned disk, an Amiga Rigid Disk Block's, rather than
    /// one volume.
    Partitioned = 4,
    /// Its filesystem is one whose recorded owners every volume of it is
    /// served with (ext2/3/4, ARXFS), which presenting it as one principal's
    /// would contradict.
    RecordsOwners = 5,
    /// Its filesystem's driver found the volume damaged.
    Damaged = 6,
    /// Its filesystem is a variant its driver does not read.
    UnsupportedVariant = 7,
    /// The file could not be read.
    Unreadable = 8,
}

impl ImageRefusal {
    /// Every refusal, in wire order.
    pub const ALL: [Self; 8] = [
        Self::Unrecognised,
        Self::NotExpected,
        Self::RaidMember,
        Self::Partitioned,
        Self::RecordsOwners,
        Self::Damaged,
        Self::UnsupportedVariant,
        Self::Unreadable,
    ];

    /// The wire code.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// The refusal `raw` codes.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a code naming none.
    pub const fn from_u8(raw: u8) -> Result<Self, Errno> {
        Ok(match raw {
            1 => Self::Unrecognised,
            2 => Self::NotExpected,
            3 => Self::RaidMember,
            4 => Self::Partitioned,
            5 => Self::RecordsOwners,
            6 => Self::Damaged,
            7 => Self::UnsupportedVariant,
            8 => Self::Unreadable,
            _ => return Err(Errno::OutOfRange),
        })
    }

    /// Why, in the clause a person is told it in: lower-case, unpunctuated,
    /// its subject the image file.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Unrecognised => "it holds no disk the system can read",
            Self::NotExpected => "it holds another kind of disk than the one asked for",
            Self::RaidMember => "it holds one disk of a RAID array, which is only assembled whole",
            Self::Partitioned => "it holds a partitioned disk, whose partitions are not mounted",
            Self::RecordsOwners => {
                "its disk's recorded owners always apply, so it is not mounted for one user"
            }
            Self::Damaged => "its disk is damaged",
            Self::UnsupportedVariant => "its disk is a kind the system does not read",
            Self::Unreadable => "it could not be read",
        }
    }

    /// The stable spelling the audit log records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unrecognised => "unrecognised",
            Self::NotExpected => "not_expected",
            Self::RaidMember => "raid_member",
            Self::Partitioned => "partitioned",
            Self::RecordsOwners => "records_owners",
            Self::Damaged => "damaged",
            Self::UnsupportedVariant => "unsupported_variant",
            Self::Unreadable => "unreadable",
        }
    }
}

/// Wire length of an [`ImageAttachReply`]: the outcome (1), the refusal (1), a
/// pad (2), the errno a target or name was refused with (4), and the
/// [`AttachReply`], all zero for a refusal.
pub const IMAGE_ATTACH_REPLY_LEN: usize = 8 + ATTACH_REPLY_LEN;

/// Where the errno of [`ImageAttachReply::TargetRefused`] and
/// [`ImageAttachReply::NameRefused`] is on the wire.
const REFUSED_ERRNO: core::ops::Range<usize> = 4..8;

/// The wire outcome of [`ImageAttachReply::Attached`].
pub const IMAGE_ATTACH_OUTCOME_ATTACHED: u8 = 1;
/// The wire outcome of [`ImageAttachReply::AlreadyAttached`].
pub const IMAGE_ATTACH_OUTCOME_ALREADY_ATTACHED: u8 = 2;
/// The wire outcome of [`ImageAttachReply::Refused`].
pub const IMAGE_ATTACH_OUTCOME_REFUSED: u8 = 3;
/// The wire outcome of [`ImageAttachReply::TargetRefused`].
pub const IMAGE_ATTACH_OUTCOME_TARGET_REFUSED: u8 = 4;
/// The wire outcome of [`ImageAttachReply::NameRefused`].
pub const IMAGE_ATTACH_OUTCOME_NAME_REFUSED: u8 = 5;

/// What the kernel answers an image attach.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ImageAttachReply {
    /// The volume was attached.
    Attached(AttachReply),
    /// The principal had the volume this file holds attached already: the
    /// attach made nothing, and names that volume.
    AlreadyAttached(AttachReply),
    /// The file holds nothing the kernel will attach.
    Refused(ImageRefusal),
    /// The directory the volume was also to be shown at would not take it,
    /// for the reason the errno states. The volume was taken out before this
    /// answer, unless the caller was being killed, something held it, or its
    /// detach was refused: the system then takes it out, and a retry may be
    /// refused [`Errno::Busy`] until it has.
    TargetRefused(Errno),
    /// The catalog name asked for the volume would not be given it —
    /// [`Errno::OutOfRange`] for one the naming policy keeps,
    /// [`Errno::AlreadyExists`] for one in use — so nothing was attached.
    NameRefused(Errno),
}

impl ImageAttachReply {
    /// Encode the reply.
    #[must_use]
    pub fn encode(&self) -> [u8; IMAGE_ATTACH_REPLY_LEN] {
        let mut out = [0; IMAGE_ATTACH_REPLY_LEN];
        let (outcome, reply) = match self {
            Self::Attached(reply) => (IMAGE_ATTACH_OUTCOME_ATTACHED, reply),
            Self::AlreadyAttached(reply) => (IMAGE_ATTACH_OUTCOME_ALREADY_ATTACHED, reply),
            Self::Refused(why) => {
                out[0] = IMAGE_ATTACH_OUTCOME_REFUSED;
                out[1] = why.as_u8();
                return out;
            }
            Self::TargetRefused(errno) => {
                return Self::refused(IMAGE_ATTACH_OUTCOME_TARGET_REFUSED, *errno);
            }
            Self::NameRefused(errno) => {
                return Self::refused(IMAGE_ATTACH_OUTCOME_NAME_REFUSED, *errno);
            }
        };
        out[0] = outcome;
        out[8..].copy_from_slice(&reply.encode());
        out
    }

    /// A reply refusing for the errno named, under `outcome`.
    fn refused(outcome: u8, errno: Errno) -> [u8; IMAGE_ATTACH_REPLY_LEN] {
        let mut out = [0; IMAGE_ATTACH_REPLY_LEN];
        out[0] = outcome;
        out[REFUSED_ERRNO].copy_from_slice(&errno.as_i32().to_le_bytes());
        out
    }

    /// Decode a reply.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for any length but
    /// [`IMAGE_ATTACH_REPLY_LEN`]; [`Errno::OutOfRange`] for an unknown outcome,
    /// refusal or errno, a nonzero pad, a refusal carrying a volume or a volume
    /// carrying a refusal, and the [`AttachReply`]'s own refusals.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        exact_len(bytes, IMAGE_ATTACH_REPLY_LEN).map_err(|_| Errno::LengthOutOfRange)?;
        if bytes[2..REFUSED_ERRNO.start].iter().any(|&b| b != 0) {
            return Err(Errno::OutOfRange);
        }
        let mut errno = [0; 4];
        errno.copy_from_slice(&bytes[REFUSED_ERRNO]);
        let errno = i32::from_le_bytes(errno);
        let refused: Option<fn(Errno) -> Self> = match bytes[0] {
            IMAGE_ATTACH_OUTCOME_TARGET_REFUSED => Some(Self::TargetRefused),
            IMAGE_ATTACH_OUTCOME_NAME_REFUSED => Some(Self::NameRefused),
            _ => None,
        };
        if let Some(refused) = refused {
            return match Errno::from_i32(errno) {
                Some(errno) if bytes[1] == 0 && bytes[8..].iter().all(|&b| b == 0) => {
                    Ok(refused(errno))
                }
                _ => Err(Errno::OutOfRange),
            };
        }
        if errno != 0 {
            return Err(Errno::OutOfRange);
        }
        match bytes[0] {
            IMAGE_ATTACH_OUTCOME_ATTACHED | IMAGE_ATTACH_OUTCOME_ALREADY_ATTACHED
                if bytes[1] == 0 =>
            {
                let reply = AttachReply::decode(&bytes[8..])?;
                Ok(if bytes[0] == IMAGE_ATTACH_OUTCOME_ATTACHED {
                    Self::Attached(reply)
                } else {
                    Self::AlreadyAttached(reply)
                })
            }
            IMAGE_ATTACH_OUTCOME_REFUSED if bytes[8..].iter().all(|&b| b == 0) => {
                Ok(Self::Refused(ImageRefusal::from_u8(bytes[1])?))
            }
            _ => Err(Errno::OutOfRange),
        }
    }
}

#[cfg(test)]
#[path = "imageattach_tests.rs"]
mod tests;
