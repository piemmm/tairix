//! Network attachments (`plans/FSSERVE.md` §6): a volume a network
//! filesystem server offers one principal, and that principal's attach of it.
//!
//! The server offers ([`NetworkOffer`], `fsserve_offer`): the kernel mints the
//! volume's endpoint and window as it does for a local volume a driver serves,
//! so the kernel is the endpoint's only caller, and names the offer
//! ([`OfferId`]). The principal the offer names attaches it
//! ([`NetworkAttach`], `fsserve_attach`), choosing its name, its
//! [`Presentation`] and where it goes, so every authority decision is made
//! against the real caller. The owner retires it by detaching, the server by
//! withdrawing it ([`FsserveCtl::Withdraw`]).

use core::num::NonZeroU64;

use crate::fs::FS_PERMISSION_BITS;
use crate::fsserve::{Lanes, ServeFrame, ServeGrant, ServeOffer, FSSERVE_VERSION, SERVE_FRAME_LEN};
use crate::le::{exact_len, put_u16, put_u32, put_u64, read_u16, read_u32, read_u64};
use crate::sysinfo::{MOUNT_SOURCE_MAX, MOUNT_TARGET_MAX};
use crate::volume::{is_binding_path, validate_volume_name, VOLUME_ID_LEN, VOLUME_NAME_MAX};
use crate::Errno;

/// The kernel's name for a network volume a server has offered to one
/// principal, which that principal attaches. Never zero, and never reused in
/// a boot.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct OfferId(NonZeroU64);

impl OfferId {
    /// The offer `raw` names, or `None` for zero, which names none.
    #[must_use]
    pub const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    /// Its wire form.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// The length of the key a server names a network volume by, from which the
/// kernel derives the volume's identity.
pub const OFFER_KEY_LEN: usize = 16;

/// The part of an encoded [`NetworkOffer`] ahead of its source.
const OFFER_FIXED_LEN: usize = SERVE_FRAME_LEN + 12 + OFFER_KEY_LEN;

/// The largest [`NetworkOffer`]: a source at [`MOUNT_SOURCE_MAX`].
pub const NETWORK_OFFER_MAX: usize = OFFER_FIXED_LEN + MOUNT_SOURCE_MAX;

/// What a network filesystem server offers: the volume, for one principal.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NetworkOffer<'a> {
    /// What it serves, over a network protocol's [`FormatName`].
    ///
    /// [`FormatName`]: crate::volume::FormatName
    pub serve: ServeOffer,
    /// The only principal that may attach it.
    pub for_uid: u32,
    /// The server's name for the volume: the kernel derives its identity from
    /// it, the principal and the server's attested application identity.
    pub key: [u8; OFFER_KEY_LEN],
    /// How its calls are spread.
    pub lanes: Lanes,
    /// What the mount table shows as its source, `//host/share`.
    pub source: &'a str,
}

impl<'a> NetworkOffer<'a> {
    /// Encode the offer into `out`, returning its length: the [`ServeFrame`],
    /// the uid (4), the lane count (1) and a pad (1), the source's length (1)
    /// and a pad (1), the lane span (4), the key, then the source.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a local format or a source out of shape;
    /// [`Errno::BufferTooSmall`] when `out` cannot hold it.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        check_offer(&self.serve, self.source)?;
        let narrow = |_| Errno::OutOfRange;
        let count = u8::try_from(self.lanes.count()).map_err(narrow)?;
        let span = u32::try_from(self.lanes.span()).map_err(narrow)?;
        let source_len = u8::try_from(self.source.len()).map_err(narrow)?;
        let len = OFFER_FIXED_LEN + self.source.len();
        let out = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
        out.fill(0);
        out[..SERVE_FRAME_LEN].copy_from_slice(&ServeFrame::Serve(self.serve).encode());
        let at = SERVE_FRAME_LEN;
        put_u32(out, at, self.for_uid);
        out[at + 4] = count;
        out[at + 6] = source_len;
        put_u32(out, at + 8, span);
        out[at + 12..OFFER_FIXED_LEN].copy_from_slice(&self.key);
        out[OFFER_FIXED_LEN..].copy_from_slice(self.source.as_bytes());
        Ok(len)
    }

    /// Decode an offer, failing closed on anything [`encode`](Self::encode)
    /// could not have written.
    ///
    /// # Errors
    ///
    /// The [`ServeFrame`]'s refusals, a refusal frame being
    /// [`Errno::OutOfRange`]; [`Errno::OutOfRange`] for a local format,
    /// lanes out of bounds, a nonzero pad, or a source out of shape;
    /// [`Errno::LengthOutOfRange`] for a frame that does not end where its
    /// source does.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Errno> {
        if bytes.len() < OFFER_FIXED_LEN || bytes.len() > NETWORK_OFFER_MAX {
            return Err(Errno::LengthOutOfRange);
        }
        let ServeFrame::Serve(serve) = ServeFrame::decode(&bytes[..SERVE_FRAME_LEN])? else {
            return Err(Errno::OutOfRange);
        };
        let at = SERVE_FRAME_LEN;
        if bytes[at + 5] != 0 || bytes[at + 7] != 0 {
            return Err(Errno::OutOfRange);
        }
        let lanes = Lanes::new(usize::from(bytes[at + 4]), read_u32(bytes, at + 8) as usize)?;
        let source_len = usize::from(bytes[at + 6]);
        if bytes.len() != OFFER_FIXED_LEN + source_len {
            return Err(Errno::LengthOutOfRange);
        }
        let source =
            core::str::from_utf8(&bytes[OFFER_FIXED_LEN..]).map_err(|_| Errno::OutOfRange)?;
        check_offer(&serve, source)?;
        let mut key = [0; OFFER_KEY_LEN];
        key.copy_from_slice(&bytes[at + 12..OFFER_FIXED_LEN]);
        Ok(Self {
            serve,
            for_uid: read_u32(bytes, at),
            key,
            lanes,
            source,
        })
    }
}

/// Whether `serve` is over a network protocol and `source` is text the mount
/// table can show.
fn check_offer(serve: &ServeOffer, source: &str) -> Result<(), Errno> {
    if serve.format.family().is_some()
        || source.is_empty()
        || source.len() > MOUNT_SOURCE_MAX
        || source.chars().any(char::is_control)
    {
        return Err(Errno::OutOfRange);
    }
    Ok(())
}

/// Wire length of an [`OfferGrant`]: the [`ServeGrant`] and the offer.
pub const OFFER_GRANT_LEN: usize = crate::fsserve::SERVE_GRANT_LEN + 8;

/// What the kernel answers a server whose offer it accepted: where the
/// volume's calls arrive and its data moves, and the offer's name.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct OfferGrant {
    /// The endpoint and window, as a local volume's serve is granted.
    pub grant: ServeGrant,
    /// The offer.
    pub offer: OfferId,
}

impl OfferGrant {
    /// Encode the grant: the [`ServeGrant`], then the offer (8).
    #[must_use]
    pub fn encode(&self) -> [u8; OFFER_GRANT_LEN] {
        let mut out = [0; OFFER_GRANT_LEN];
        let serve = crate::fsserve::SERVE_GRANT_LEN;
        out[..serve].copy_from_slice(&self.grant.encode());
        put_u64(&mut out, serve, self.offer.get());
        out
    }

    /// Decode a grant for an offer of `lanes`.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for any length but [`OFFER_GRANT_LEN`] or
    /// a window other than `lanes` divide; [`Errno::OutOfRange`] for a
    /// nonzero pad or offer zero.
    pub fn decode(bytes: &[u8], lanes: Lanes) -> Result<Self, Errno> {
        exact_len(bytes, OFFER_GRANT_LEN).map_err(|_| Errno::LengthOutOfRange)?;
        let serve = crate::fsserve::SERVE_GRANT_LEN;
        Ok(Self {
            grant: ServeGrant::decode(&bytes[..serve], lanes)?,
            offer: OfferId::new(read_u64(bytes, serve)).ok_or(Errno::OutOfRange)?,
        })
    }
}

/// What a server asks of an offer it made.
#[repr(u32)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum FsserveCtl {
    /// It is working on the calls it holds: the kernel keeps waiting on them.
    Alive = 1,
    /// Retire the offer, and its attachment if it has one, once the server
    /// has written back everything it held.
    Withdraw = 2,
}

impl FsserveCtl {
    /// The operation `raw` codes.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a code naming none.
    pub const fn from_u32(raw: u32) -> Result<Self, Errno> {
        match raw {
            1 => Ok(Self::Alive),
            2 => Ok(Self::Withdraw),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// How an attached network volume's nodes are presented: who owns them and
/// with what mode. A node the server marks read-only loses its write bits in
/// every one.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Presentation {
    /// Owned by the attaching principal and its primary group, directories
    /// `rwx------` and files `rw-------`: reachable by no one else.
    Private,
    /// Owned by the system and the storage group, directories `rwxrwx---`
    /// and files `rw-rw----`. Needs `CAP_FS_MOUNT`.
    Shared,
    /// As given. Needs `CAP_FS_MOUNT`.
    Explicit {
        /// The owner.
        uid: u32,
        /// The owning group.
        gid: u32,
        /// A directory's mode bits.
        dir_mode: u16,
        /// A file's mode bits.
        file_mode: u16,
    },
}

/// The fixed part of a [`NetworkAttach`]: the version (2), the presentation
/// (1), the flags (1), the explicit owner (4), group (4), directory mode (2)
/// and file mode (2), the name's length (1), a pad (1), and the target's
/// length (2).
const ATTACH_FIXED_LEN: usize = 20;

/// The largest [`NetworkAttach`]: a name at [`VOLUME_NAME_MAX`] and a target
/// at [`MOUNT_TARGET_MAX`].
pub const NETWORK_ATTACH_MAX: usize = ATTACH_FIXED_LEN + VOLUME_NAME_MAX + MOUNT_TARGET_MAX;

const PRESENT_PRIVATE: u8 = 1;
const PRESENT_SHARED: u8 = 2;
const PRESENT_EXPLICIT: u8 = 3;
const ATTACH_READ_ONLY: u8 = 0x01;

/// How a principal attaches a volume offered to it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NetworkAttach<'a> {
    /// The name it is attached under in `/Storage`, which the kernel suffixes
    /// until one is free.
    pub name: &'a str,
    /// How its nodes are presented.
    pub presentation: Presentation,
    /// Whether to attach it read-only, whatever the server allows.
    pub read_only: bool,
    /// The directory it is also bound at, under `/Users` or `/Storage`.
    pub target: Option<&'a str>,
}

impl<'a> NetworkAttach<'a> {
    /// Encode the attach into `out`, returning its length.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a name the catalog refuses, an explicit mode
    /// past the permission bits, or a target that is not an absolute path
    /// within [`MOUNT_TARGET_MAX`]; [`Errno::BufferTooSmall`] when `out`
    /// cannot hold it.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        check_attach(self)?;
        let target = self.target.unwrap_or("");
        let len = ATTACH_FIXED_LEN + self.name.len() + target.len();
        let out = out.get_mut(..len).ok_or(Errno::BufferTooSmall)?;
        out.fill(0);
        put_u16(out, 0, FSSERVE_VERSION);
        let (kind, uid, gid, dir_mode, file_mode) = match self.presentation {
            Presentation::Private => (PRESENT_PRIVATE, 0, 0, 0, 0),
            Presentation::Shared => (PRESENT_SHARED, 0, 0, 0, 0),
            Presentation::Explicit {
                uid,
                gid,
                dir_mode,
                file_mode,
            } => (PRESENT_EXPLICIT, uid, gid, dir_mode, file_mode),
        };
        out[2] = kind;
        out[3] = if self.read_only { ATTACH_READ_ONLY } else { 0 };
        put_u32(out, 4, uid);
        put_u32(out, 8, gid);
        put_u16(out, 12, dir_mode);
        put_u16(out, 14, file_mode);
        out[16] = u8::try_from(self.name.len()).map_err(|_| Errno::OutOfRange)?;
        put_u16(
            out,
            18,
            u16::try_from(target.len()).map_err(|_| Errno::OutOfRange)?,
        );
        let names = &mut out[ATTACH_FIXED_LEN..];
        names[..self.name.len()].copy_from_slice(self.name.as_bytes());
        names[self.name.len()..].copy_from_slice(target.as_bytes());
        Ok(len)
    }

    /// Decode an attach, failing closed on anything
    /// [`encode`](Self::encode) could not have written.
    ///
    /// # Errors
    ///
    /// [`Errno::AbiVersionUnsupported`] for another version;
    /// [`Errno::OutOfRange`] for an unknown presentation or flag, an owner or
    /// mode carried by a presentation that names none, a nonzero pad, or a
    /// name, mode or target out of shape; [`Errno::LengthOutOfRange`] for a
    /// frame that does not end where its target does.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Errno> {
        if bytes.len() < ATTACH_FIXED_LEN || bytes.len() > NETWORK_ATTACH_MAX {
            return Err(Errno::LengthOutOfRange);
        }
        if read_u16(bytes, 0) != FSSERVE_VERSION {
            return Err(Errno::AbiVersionUnsupported);
        }
        let (uid, gid) = (read_u32(bytes, 4), read_u32(bytes, 8));
        let (dir_mode, file_mode) = (read_u16(bytes, 12), read_u16(bytes, 14));
        let presentation = match bytes[2] {
            PRESENT_PRIVATE | PRESENT_SHARED if (uid, gid, dir_mode, file_mode) != (0, 0, 0, 0) => {
                return Err(Errno::OutOfRange)
            }
            PRESENT_PRIVATE => Presentation::Private,
            PRESENT_SHARED => Presentation::Shared,
            PRESENT_EXPLICIT => Presentation::Explicit {
                uid,
                gid,
                dir_mode,
                file_mode,
            },
            _ => return Err(Errno::OutOfRange),
        };
        if bytes[3] & !ATTACH_READ_ONLY != 0 || bytes[17] != 0 {
            return Err(Errno::OutOfRange);
        }
        let name_len = usize::from(bytes[16]);
        let target_len = usize::from(read_u16(bytes, 18));
        if bytes.len() != ATTACH_FIXED_LEN + name_len + target_len {
            return Err(Errno::LengthOutOfRange);
        }
        let names = &bytes[ATTACH_FIXED_LEN..];
        let text = |bytes| core::str::from_utf8(bytes).map_err(|_| Errno::OutOfRange);
        let attach = Self {
            name: text(&names[..name_len])?,
            presentation,
            read_only: bytes[3] & ATTACH_READ_ONLY != 0,
            target: match target_len {
                0 => None,
                _ => Some(text(&names[name_len..])?),
            },
        };
        check_attach(&attach)?;
        Ok(attach)
    }
}

/// Whether `attach`'s name, modes and target are each in shape.
fn check_attach(attach: &NetworkAttach<'_>) -> Result<(), Errno> {
    validate_volume_name(attach.name.as_bytes())?;
    if let Presentation::Explicit {
        dir_mode,
        file_mode,
        ..
    } = attach.presentation
    {
        if u32::from(dir_mode) & !FS_PERMISSION_BITS != 0
            || u32::from(file_mode) & !FS_PERMISSION_BITS != 0
        {
            return Err(Errno::OutOfRange);
        }
    }
    if attach.target.is_some_and(|target| !is_binding_path(target)) {
        return Err(Errno::OutOfRange);
    }
    Ok(())
}

/// Wire length of an [`AttachReply`]: the volume's identity, the name's
/// length (1), a pad (7), and the name at its bound.
pub const ATTACH_REPLY_LEN: usize = VOLUME_ID_LEN + 8 + VOLUME_NAME_MAX;

/// What the kernel answers an attach: the volume's identity, which a detach
/// names, and the name it took in `/Storage`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AttachReply {
    /// The volume's identity.
    pub volume_id: [u8; VOLUME_ID_LEN],
    name: [u8; VOLUME_NAME_MAX],
    name_len: u8,
}

impl AttachReply {
    /// The reply for the volume `volume_id`, attached as `name`.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a name the catalog refuses.
    pub fn new(volume_id: [u8; VOLUME_ID_LEN], name: &str) -> Result<Self, Errno> {
        validate_volume_name(name.as_bytes())?;
        let mut bytes = [0; VOLUME_NAME_MAX];
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        Ok(Self {
            volume_id,
            name: bytes,
            name_len: u8::try_from(name.len()).map_err(|_| Errno::OutOfRange)?,
        })
    }

    /// The name it took.
    #[must_use]
    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..usize::from(self.name_len)]).unwrap_or("")
    }

    /// Encode the reply.
    #[must_use]
    pub fn encode(&self) -> [u8; ATTACH_REPLY_LEN] {
        let mut out = [0; ATTACH_REPLY_LEN];
        out[..VOLUME_ID_LEN].copy_from_slice(&self.volume_id);
        out[VOLUME_ID_LEN] = self.name_len;
        out[VOLUME_ID_LEN + 8..].copy_from_slice(&self.name);
        out
    }

    /// Decode a reply.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for any length but [`ATTACH_REPLY_LEN`];
    /// [`Errno::OutOfRange`] for a nonzero pad, bytes past the name, or a
    /// name the catalog refuses.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        exact_len(bytes, ATTACH_REPLY_LEN).map_err(|_| Errno::LengthOutOfRange)?;
        let name_len = usize::from(bytes[VOLUME_ID_LEN]);
        let name = &bytes[VOLUME_ID_LEN + 8..];
        if bytes[VOLUME_ID_LEN + 1..VOLUME_ID_LEN + 8]
            .iter()
            .any(|&b| b != 0)
            || name_len > VOLUME_NAME_MAX
            || name[name_len..].iter().any(|&b| b != 0)
        {
            return Err(Errno::OutOfRange);
        }
        let mut volume_id = [0; VOLUME_ID_LEN];
        volume_id.copy_from_slice(&bytes[..VOLUME_ID_LEN]);
        let text = core::str::from_utf8(&name[..name_len]).map_err(|_| Errno::OutOfRange)?;
        Self::new(volume_id, text)
    }
}

#[cfg(test)]
#[path = "netattach_tests.rs"]
mod tests;
