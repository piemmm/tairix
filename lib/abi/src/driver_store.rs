//! The read-only `/System` driver-store IPC protocol (Design D D2b-2c —
//! `plans/PI.md`).
//!
//! Under Design D the one bootstrap-floor disk is owned for the life of the
//! system by the never-returning kernel driver-store service, which keeps
//! the read-only signed-bundle `/System` volume mounted. The reactive user-space device manager (`userland/system/devmgr`)
//! reaches that service through a single capability-gated synchronous IPC
//! call endpoint — the [`SyscallNumber::IPC_CALL`](crate::SyscallNumber::IPC_CALL)
//! surface served by the kernel over the well-known [`DRIVER_STORE_ENDPOINT`].
//!
//! The device manager owns *policy* (which driver binds which node); the
//! kernel keeps the *mechanism* (bundle bytes, signature verification, spawn)
//! in its trusted base. The two operations that contract
//! needs are:
//!
//! * [`StoreRequest::Catalogue`] — the kernel returns one entry per installed
//!   bundle: an **opaque** `bundle_id` plus the bind table the kernel already
//!   decoded fail-closed from that bundle's signed manifest. No bytes and no `/System` path ever cross to user space — the
//!   device manager matches the decoded bind keys against the hardware tree
//!   with `lib/devmatch` and never re-parses an
//!   image, keeping the layering intact.
//! * [`StoreRequest::Load`] — naming a `bundle_id` it matched and the
//!   hardware-tree `node_id` that matched it, the device manager asks the
//!   kernel to load that bundle. The kernel re-reads the bundle, re-runs the
//!   full signed gate, and spawns it with **only** the resources the named
//!   node requested — the device manager supplies no bytes and no grants
//!   (no ambient authority).
//!
//! This module is the wire contract for that endpoint: the request encoder
//! both sides share and the reply framing, all operating on borrowed buffers
//! (no allocation, matching the crate's `no_std` contract).
//! Every reply is length-framed and carries a leading status word so a
//! fail-closed refusal is delivered in-band rather than as a truncated
//! payload.

use crate::driver::DriverBindKey;
use crate::le::{put_i32, put_u16, put_u32, put_u64, read_i32, read_u16, read_u32, read_u64};
use crate::{Errno, DRIVER_MANIFEST_MAX_BIND_KEYS};

/// The signed-driver store's place in the namespace: every installed driver
/// bundle lives beneath it, at `<class>[_<subtype>]/<leaf>/Run`.
pub const DRIVER_STORE_PATH: &str = "/System/Drivers";

/// Well-known kernel-owned call-endpoint id of the read-only `/System`
/// driver-store service (Design D D2b).
///
/// The disk-owning kernel service creates one [`crate::ipc`]-style call
/// endpoint under this reserved id; the device manager names it as the
/// `endpoint` argument to [`SyscallNumber::IPC_CALL`](crate::SyscallNumber::IPC_CALL).
/// A reserved well-known id (rather than a delegated handle) keeps the
/// bootstrap client/server rendezvous from needing a prior name-exchange
/// step; the endpoint's required send capability still gates every call.
pub const DRIVER_STORE_ENDPOINT: u64 = 0xD012_5701;

/// Request opcode: list the installed store as opaque-id + bind-key entries.
const OP_CATALOGUE: u8 = 1;
/// Request opcode: load the bundle `bundle_id` for the matched `node_id`.
const OP_LOAD: u8 = 2;
/// Request opcode: unload the driver instance named by `handle`.
const OP_UNLOAD: u8 = 3;
/// Request opcode: read the image's default of one settings document (a
/// [`SystemConfigFile`]) off the read-only `/System` volume.
const OP_READ_CONFIG: u8 = 4;

/// Encoded length of a [`StoreRequest::ReadConfig`]: opcode + `which` (u8).
pub const READ_CONFIG_REQUEST_LEN: usize = 1 + 1;

/// Where the read-only `/System` volume's own root is seen.
const SYSTEM_VIEW: &str = "/System";

/// One of the machine's settings documents.
///
/// Each has two layers. The image's own default ships read-only on the
/// `/System` volume ([`Self::default_path`]); the administrator's document,
/// written only by the settings service ([`crate::settings_ipc`]), lives on
/// the writable root ([`Self::path`]) and replaces the default whole once it
/// exists ([`Self::layers`]). The set is closed: before the root is mounted
/// the store service reads exactly these defaults for a bootstrap client
/// ([`StoreRequest::ReadConfig`]), so its endpoint never becomes a general
/// `/System` read.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SystemConfigFile {
    /// `system.conf` — the machine-wide system configuration.
    System = 0,
    /// `network.conf` — the per-interface network configuration (`match.*`
    /// binding, static addressing, bonds).
    Network = 1,
}

impl SystemConfigFile {
    /// The wire discriminant.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Decode a wire discriminant, or `None` for an unknown value
    /// (fail closed — an unrecognised file is never guessed).
    #[must_use]
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::System),
            1 => Some(Self::Network),
            _ => None,
        }
    }

    /// The administrator's document, on the writable root.
    #[must_use]
    pub const fn path(self) -> &'static str {
        match self {
            Self::System => "/System/Settings/Configuration/system.conf",
            Self::Network => "/System/Settings/Network/network.conf",
        }
    }

    /// The directory holding [`Self::path`], which the settings service owns.
    #[must_use]
    pub const fn dir(self) -> &'static str {
        match self {
            Self::System => "/System/Settings/Configuration",
            Self::Network => "/System/Settings/Network",
        }
    }

    /// The image's own default, read-only on the `/System` volume.
    #[must_use]
    pub const fn default_path(self) -> &'static str {
        match self {
            Self::System => "/System/Defaults/Configuration/system.conf",
            Self::Network => "/System/Defaults/Network/network.conf",
        }
    }

    /// Where a reader takes the document from, first to last: the
    /// administrator's layer, then the image's.
    ///
    /// The first layer that exists is the document, whole. One that exists
    /// but cannot be read is the answer, never a reason to read the next: a
    /// reader that fell back would act on the image's settings while the
    /// administrator's stood.
    #[must_use]
    pub const fn layers(self) -> [&'static str; 2] {
        [self.path(), self.default_path()]
    }

    /// The document from the first of its [`Self::layers`] that exists, or
    /// `None` when neither does.
    ///
    /// `read` answers one layer's path: `Ok(None)` for a layer that does not
    /// exist, and an error for one that exists but cannot be read, which
    /// ends the search.
    ///
    /// # Errors
    ///
    /// The first error `read` answers.
    pub fn read_effective<T, E>(
        self,
        mut read: impl FnMut(&'static str) -> Result<Option<T>, E>,
    ) -> Result<Option<T>, E> {
        for path in self.layers() {
            if let Some(document) = read(path)? {
                return Ok(Some(document));
            }
        }
        Ok(None)
    }

    /// [`Self::default_path`] relative to the `/System` volume's own root:
    /// where an image builder plants the default.
    #[must_use]
    pub const fn volume_path(self) -> &'static str {
        self.default_path().split_at(SYSTEM_VIEW.len() + 1).1
    }

    /// [`Self::default_path`] as an absolute path within the `/System`
    /// volume's own tree: where a reader holding the volume itself, before
    /// the root is mounted, finds the default.
    #[must_use]
    pub const fn on_volume_path(self) -> &'static str {
        self.default_path().split_at(SYSTEM_VIEW.len()).1
    }
}

/// Encoded length of a [`StoreRequest::Load`]: opcode + `bundle_id` (u32) +
/// `node_id` (u32).
pub const LOAD_REQUEST_LEN: usize = 1 + 4 + 4;

/// Encoded length of a [`StoreRequest::Unload`]: opcode + `handle` (u64).
pub const UNLOAD_REQUEST_LEN: usize = 1 + 8;

/// The endpoint's maximum request size: the longest of every request
/// encoding ([`StoreRequest::Catalogue`] is one opcode byte; a
/// [`StoreRequest::Load`] is [`LOAD_REQUEST_LEN`]; a [`StoreRequest::Unload`]
/// is [`UNLOAD_REQUEST_LEN`]). Derived from the protocol bounds so the
/// server's request cap can never drift from what a valid request encodes.
pub const MAX_REQUEST_LEN: usize = {
    let a = if LOAD_REQUEST_LEN > UNLOAD_REQUEST_LEN {
        LOAD_REQUEST_LEN
    } else {
        UNLOAD_REQUEST_LEN
    };
    if a > READ_CONFIG_REQUEST_LEN {
        a
    } else {
        READ_CONFIG_REQUEST_LEN
    }
};

// The endpoint sizes its request cap from `MAX_REQUEST_LEN`; statically prove
// it admits the longest valid request so the cap can never drift below a
// request a client legitimately encodes.
const _: () = assert!(MAX_REQUEST_LEN >= LOAD_REQUEST_LEN);
const _: () = assert!(MAX_REQUEST_LEN >= UNLOAD_REQUEST_LEN);
const _: () = assert!(MAX_REQUEST_LEN >= READ_CONFIG_REQUEST_LEN);

/// A request posted to the driver-store endpoint.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StoreRequest {
    /// List every installed bundle as an opaque `bundle_id` plus its decoded
    /// bind table.
    Catalogue,
    /// Load the bundle the device manager matched, granting the loaded
    /// driver only the resources the matched hardware-tree node requested.
    Load {
        /// Opaque bundle id from a prior [`StoreRequest::Catalogue`] entry.
        bundle_id: u32,
        /// The matched hardware-tree node ([`crate::HwNode::id`]) whose
        /// resource grants the kernel mints for the loaded driver.
        node_id: u32,
    },
    /// Unload a driver instance the device manager previously loaded, named
    /// by the [`crate::DriverHandle`] a [`StoreRequest::Load`] returned.
    ///
    /// The symmetric partner of [`StoreRequest::Load`]: the kernel tears the
    /// driver process down — reclaiming its grants, served endpoints, IRQ
    /// bindings, and address space, and deregistering it — so a device whose
    /// hardware-tree node has vanished leaves no running driver behind.
    /// Teardown is idempotent and fails closed: unloading an
    /// already-gone handle is a benign [`crate::Errno::NotFound`], never a
    /// panic. Capability-gated exactly as [`StoreRequest::Load`] is
    /// (`CAP_DRV_LOAD`); the device manager owns *which* handle to unload,
    /// the kernel owns the teardown *mechanism*.
    Unload {
        /// The driver instance to tear down — the value a prior
        /// [`StoreRequest::Load`] reply carried ([`decode_load_reply`]).
        handle: u64,
    },
    /// Read the image's default of one settings document off the read-only
    /// `/System` volume the store service owns.
    ///
    /// The device manager needs the network configuration before the root
    /// is mounted, so it can bring interfaces up, but the general VFS is not
    /// mounted until unlock. The store service already holds the `/System`
    /// volume, so it reads the default and returns its bytes. The reply is
    /// capability-gated exactly as every other request (`CAP_DRV_LOAD`); the
    /// file set is closed ([`SystemConfigFile`]), so this never becomes a
    /// general file-read primitive.
    ReadConfig {
        /// Which whitelisted configuration file to read.
        which: SystemConfigFile,
    },
}

impl StoreRequest {
    /// Encode `self` into `buf`, returning the number of bytes written.
    ///
    /// # Errors
    ///
    /// [`Errno::BufferTooSmall`] if `buf` cannot hold the encoding.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, Errno> {
        match *self {
            StoreRequest::Catalogue => {
                if buf.is_empty() {
                    return Err(Errno::BufferTooSmall);
                }
                buf[0] = OP_CATALOGUE;
                Ok(1)
            }
            StoreRequest::Load { bundle_id, node_id } => {
                if buf.len() < LOAD_REQUEST_LEN {
                    return Err(Errno::BufferTooSmall);
                }
                buf[0] = OP_LOAD;
                put_u32(buf, 1, bundle_id);
                put_u32(buf, 5, node_id);
                Ok(LOAD_REQUEST_LEN)
            }
            StoreRequest::Unload { handle } => {
                if buf.len() < UNLOAD_REQUEST_LEN {
                    return Err(Errno::BufferTooSmall);
                }
                buf[0] = OP_UNLOAD;
                put_u64(buf, 1, handle);
                Ok(UNLOAD_REQUEST_LEN)
            }
            StoreRequest::ReadConfig { which } => {
                if buf.len() < READ_CONFIG_REQUEST_LEN {
                    return Err(Errno::BufferTooSmall);
                }
                buf[0] = OP_READ_CONFIG;
                buf[1] = which.as_u8();
                Ok(READ_CONFIG_REQUEST_LEN)
            }
        }
    }

    /// Decode a request from `bytes`.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] if `bytes` is empty or a `Load` is
    ///   truncated.
    /// * [`Errno::OutOfRange`] if the opcode is unknown.
    pub fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        let Some((&op, _)) = bytes.split_first() else {
            return Err(Errno::LengthOutOfRange);
        };
        match op {
            OP_CATALOGUE => Ok(StoreRequest::Catalogue),
            OP_LOAD => {
                if bytes.len() < LOAD_REQUEST_LEN {
                    return Err(Errno::LengthOutOfRange);
                }
                Ok(StoreRequest::Load {
                    bundle_id: read_u32(bytes, 1),
                    node_id: read_u32(bytes, 5),
                })
            }
            OP_UNLOAD => {
                if bytes.len() < UNLOAD_REQUEST_LEN {
                    return Err(Errno::LengthOutOfRange);
                }
                Ok(StoreRequest::Unload {
                    handle: read_u64(bytes, 1),
                })
            }
            OP_READ_CONFIG => {
                if bytes.len() < READ_CONFIG_REQUEST_LEN {
                    return Err(Errno::LengthOutOfRange);
                }
                let which = SystemConfigFile::from_u8(bytes[1]).ok_or(Errno::OutOfRange)?;
                Ok(StoreRequest::ReadConfig { which })
            }
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// Fixed prefix of every reply frame: a status word (`0` on success, else
/// the negated [`Errno`] discriminant).
const REPLY_STATUS_LEN: usize = 4;

/// Encode a fail-closed error reply (status only) into `buf`.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `buf` is shorter than the status word.
pub fn encode_error_reply(buf: &mut [u8], err: Errno) -> Result<usize, Errno> {
    if buf.len() < REPLY_STATUS_LEN {
        return Err(Errno::BufferTooSmall);
    }
    // A negative status carries `-errno`; `Errno` discriminants are positive.
    put_i32(buf, 0, -err.as_i32());
    Ok(REPLY_STATUS_LEN)
}

/// Map a reply's status word to the [`Errno`] it encodes, or `Ok(())` when
/// the status is success (`0`).
///
/// # Errors
///
/// The decoded [`Errno`] when the status is negative, or
/// [`Errno::BadMagic`] if the status is neither `0` nor a known negated
/// discriminant (wire corruption — fail closed), or [`Errno::BufferTooSmall`]
/// if `reply` is shorter than the status word.
pub fn reply_status(reply: &[u8]) -> Result<(), Errno> {
    if reply.len() < REPLY_STATUS_LEN {
        return Err(Errno::BufferTooSmall);
    }
    match read_i32(reply, 0) {
        0 => Ok(()),
        negative => Errno::try_from_status(negative).map_or(Err(Errno::BadMagic), Err),
    }
}

/// Reply prefix once the status word is `0`: the status word plus a `u32`
/// entry count.
const REPLY_OK_HEADER_LEN: usize = REPLY_STATUS_LEN + 4;
/// Fixed prefix of one catalogue entry: `bundle_id` (u32) + `key_count`
/// (u16), before the entry's [`DriverBindKey`] records.
const CATALOGUE_ENTRY_HEADER_LEN: usize = 4 + 2;

/// Encode the body of a [`StoreRequest::Catalogue`] reply into `buf`,
/// returning the number of bytes written.
///
/// Each `entries` item is `(bundle_id, bind_keys)`: the opaque kernel bundle
/// id and the bind table the kernel decoded from that bundle's signed
/// manifest. The frame is
/// `status(0) || count || (bundle_id || key_count || DriverBindKey*)*`.
///
/// # Errors
///
/// * [`Errno::BufferTooSmall`] if `buf` cannot hold every entry — the
///   catalogue is never truncated; the caller grows its
///   buffer.
/// * [`Errno::LengthOutOfRange`] if an entry holds more than
///   [`DRIVER_MANIFEST_MAX_BIND_KEYS`] keys or the entry count exceeds `u32`.
pub fn encode_catalogue_reply(
    buf: &mut [u8],
    entries: &[(u32, &[DriverBindKey])],
) -> Result<usize, Errno> {
    if buf.len() < REPLY_OK_HEADER_LEN {
        return Err(Errno::BufferTooSmall);
    }
    put_i32(buf, 0, 0);
    put_u32(
        buf,
        REPLY_STATUS_LEN,
        u32::try_from(entries.len()).map_err(|_| Errno::LengthOutOfRange)?,
    );
    let mut pos = REPLY_OK_HEADER_LEN;
    for (bundle_id, keys) in entries {
        if keys.len() > DRIVER_MANIFEST_MAX_BIND_KEYS as usize {
            return Err(Errno::LengthOutOfRange);
        }
        let entry_len = CATALOGUE_ENTRY_HEADER_LEN + keys.len() * DriverBindKey::WIRE_LEN;
        if buf.len() < pos + entry_len {
            return Err(Errno::BufferTooSmall);
        }
        put_u32(buf, pos, *bundle_id);
        put_u16(
            buf,
            pos + 4,
            u16::try_from(keys.len()).expect("bounded above"),
        );
        let mut kp = pos + CATALOGUE_ENTRY_HEADER_LEN;
        for key in *keys {
            buf[kp..kp + DriverBindKey::WIRE_LEN].copy_from_slice(&key.to_le_bytes());
            kp += DriverBindKey::WIRE_LEN;
        }
        pos += entry_len;
    }
    Ok(pos)
}

/// One entry of a decoded [`StoreRequest::Catalogue`] reply: the opaque
/// `bundle_id` plus the still-encoded bind-key records, decoded on demand
/// by [`CatalogueEntry::decode_keys`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CatalogueEntry<'a> {
    /// The opaque kernel bundle id to name in a subsequent
    /// [`StoreRequest::Load`].
    pub bundle_id: u32,
    /// Number of [`DriverBindKey`] records the entry carries.
    key_count: u16,
    /// The entry's bind-key records, `key_count * DriverBindKey::WIRE_LEN`
    /// bytes.
    keys_bytes: &'a [u8],
}

impl CatalogueEntry<'_> {
    /// The number of bind keys this entry carries.
    #[must_use]
    pub fn key_count(&self) -> usize {
        usize::from(self.key_count)
    }

    /// Decode this entry's bind table into `out`, returning the key count.
    ///
    /// # Errors
    ///
    /// * [`Errno::BufferTooSmall`] if `out` is shorter than
    ///   [`Self::key_count`].
    /// * [`Errno::BadMagic`] if any record fails to decode (wire
    ///   corruption — fail closed).
    pub fn decode_keys(&self, out: &mut [DriverBindKey]) -> Result<usize, Errno> {
        let count = usize::from(self.key_count);
        if out.len() < count {
            return Err(Errno::BufferTooSmall);
        }
        for (i, slot) in out.iter_mut().enumerate().take(count) {
            let off = i * DriverBindKey::WIRE_LEN;
            *slot =
                DriverBindKey::from_bytes(&self.keys_bytes[off..]).map_err(|_| Errno::BadMagic)?;
        }
        Ok(count)
    }
}

/// An iterator over the entries a successful [`StoreRequest::Catalogue`]
/// reply carries.
///
/// Construct with [`decode_catalogue_reply`]; each [`Iterator::next`] yields
/// one `Result<CatalogueEntry, Errno>`, failing closed on a truncated entry.
pub struct CatalogueReplyIter<'a> {
    body: &'a [u8],
    remaining: u32,
}

impl<'a> Iterator for CatalogueReplyIter<'a> {
    type Item = Result<CatalogueEntry<'a>, Errno>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        if self.body.len() < CATALOGUE_ENTRY_HEADER_LEN {
            return Some(Err(Errno::BadMagic));
        }
        let bundle_id = read_u32(self.body, 0);
        let key_count = read_u16(self.body, 4);
        let span = usize::from(key_count) * DriverBindKey::WIRE_LEN;
        let entry_end = CATALOGUE_ENTRY_HEADER_LEN + span;
        if self.body.len() < entry_end {
            return Some(Err(Errno::BadMagic));
        }
        let keys_bytes = &self.body[CATALOGUE_ENTRY_HEADER_LEN..entry_end];
        self.body = &self.body[entry_end..];
        Some(Ok(CatalogueEntry {
            bundle_id,
            key_count,
            keys_bytes,
        }))
    }
}

/// Decode a successful [`StoreRequest::Catalogue`] reply into an iterator
/// over its entries.
///
/// # Errors
///
/// The carried [`Errno`] for an error frame, or [`Errno::BadMagic`] if the
/// frame header is truncated (fail closed).
pub fn decode_catalogue_reply(reply: &[u8]) -> Result<CatalogueReplyIter<'_>, Errno> {
    reply_status(reply)?;
    if reply.len() < REPLY_OK_HEADER_LEN {
        return Err(Errno::BadMagic);
    }
    let count = read_u32(reply, REPLY_STATUS_LEN);
    Ok(CatalogueReplyIter {
        body: &reply[REPLY_OK_HEADER_LEN..],
        remaining: count,
    })
}

/// Encoded length of a successful [`StoreRequest::Load`] reply: the status
/// word plus the spawned driver's [`crate::DriverHandle`] (u64).
const LOAD_REPLY_LEN: usize = REPLY_STATUS_LEN + 8;

/// Encode the body of a successful [`StoreRequest::Load`] reply carrying the
/// loaded driver's `handle`.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `buf` cannot hold the framed reply.
pub fn encode_load_reply(buf: &mut [u8], handle: u64) -> Result<usize, Errno> {
    if buf.len() < LOAD_REPLY_LEN {
        return Err(Errno::BufferTooSmall);
    }
    put_i32(buf, 0, 0);
    put_u64(buf, REPLY_STATUS_LEN, handle);
    Ok(LOAD_REPLY_LEN)
}

/// Recover the driver handle a successful [`StoreRequest::Load`] reply
/// carries.
///
/// # Errors
///
/// The carried [`Errno`] for an error frame (e.g. the load gate's refusal),
/// or [`Errno::BadMagic`] if a success frame is truncated (fail closed).
pub fn decode_load_reply(reply: &[u8]) -> Result<u64, Errno> {
    reply_status(reply)?;
    if reply.len() < LOAD_REPLY_LEN {
        return Err(Errno::BadMagic);
    }
    Ok(read_u64(reply, REPLY_STATUS_LEN))
}

/// Encode a successful [`StoreRequest::Unload`] reply.
///
/// Unload carries no payload — the teardown either succeeds (status `0`) or
/// fails closed with an in-band error reply ([`encode_error_reply`]), so a
/// success frame is the status word alone.
///
/// # Errors
///
/// [`Errno::BufferTooSmall`] if `buf` is shorter than the status word.
pub fn encode_unload_reply(buf: &mut [u8]) -> Result<usize, Errno> {
    if buf.len() < REPLY_STATUS_LEN {
        return Err(Errno::BufferTooSmall);
    }
    put_i32(buf, 0, 0);
    Ok(REPLY_STATUS_LEN)
}

/// Confirm a [`StoreRequest::Unload`] reply: `Ok(())` when the teardown
/// succeeded, else the carried [`Errno`].
///
/// A thin alias over [`reply_status`] (an unload reply is status-only), kept
/// so the client reads as the symmetric partner of [`decode_load_reply`].
///
/// # Errors
///
/// The carried [`Errno`] for an error frame (e.g. [`Errno::NotFound`] for an
/// already-gone handle), or [`Errno::BufferTooSmall`] if `reply` is shorter
/// than the status word.
pub fn decode_unload_reply(reply: &[u8]) -> Result<(), Errno> {
    reply_status(reply)
}

/// Reply prefix of a successful [`StoreRequest::ReadConfig`]: the status
/// word plus a `u32` byte count, before the config file's raw bytes.
const CONFIG_REPLY_HEADER_LEN: usize = REPLY_STATUS_LEN + 4;

/// Encode a successful [`StoreRequest::ReadConfig`] reply carrying the
/// config file's `bytes`, returning the number of bytes written. The frame
/// is `status(0) || len || bytes`.
///
/// A read that found no such file is *not* framed here — the server sends an
/// [`encode_error_reply`] with [`Errno::NotFound`] instead, so an absent
/// config reads as a benign "no configuration" (fail closed), never an empty
/// success the caller might mistake for a valid empty store.
///
/// # Errors
///
/// * [`Errno::BufferTooSmall`] if `buf` cannot hold the framed reply — the
///   config is never truncated; the caller grows its buffer.
/// * [`Errno::LengthOutOfRange`] if `bytes` is longer than `u32`.
pub fn encode_config_reply(buf: &mut [u8], bytes: &[u8]) -> Result<usize, Errno> {
    let total = CONFIG_REPLY_HEADER_LEN
        .checked_add(bytes.len())
        .ok_or(Errno::LengthOutOfRange)?;
    if buf.len() < total {
        return Err(Errno::BufferTooSmall);
    }
    put_i32(buf, 0, 0);
    put_u32(
        buf,
        REPLY_STATUS_LEN,
        u32::try_from(bytes.len()).map_err(|_| Errno::LengthOutOfRange)?,
    );
    buf[CONFIG_REPLY_HEADER_LEN..total].copy_from_slice(bytes);
    Ok(total)
}

/// Recover the config bytes a successful [`StoreRequest::ReadConfig`] reply
/// carries, borrowing them from `reply`.
///
/// # Errors
///
/// The carried [`Errno`] for an error frame (notably [`Errno::NotFound`]
/// when the file is absent), or [`Errno::BadMagic`] if a success frame is
/// truncated or its length runs past the frame (fail closed).
pub fn decode_config_reply(reply: &[u8]) -> Result<&[u8], Errno> {
    reply_status(reply)?;
    if reply.len() < CONFIG_REPLY_HEADER_LEN {
        return Err(Errno::BadMagic);
    }
    let len = usize::try_from(read_u32(reply, REPLY_STATUS_LEN)).map_err(|_| Errno::BadMagic)?;
    let end = CONFIG_REPLY_HEADER_LEN
        .checked_add(len)
        .ok_or(Errno::BadMagic)?;
    if reply.len() < end {
        return Err(Errno::BadMagic);
    }
    Ok(&reply[CONFIG_REPLY_HEADER_LEN..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HwMatchKey;

    fn key(priority: u16, virtio: u32) -> DriverBindKey {
        DriverBindKey::new(priority, HwMatchKey::virtio(virtio))
    }

    #[test]
    fn catalogue_request_round_trips() {
        let mut buf = [0u8; 8];
        let n = StoreRequest::Catalogue.encode(&mut buf).expect("encodes");
        assert_eq!(n, 1);
        assert_eq!(StoreRequest::decode(&buf[..n]), Ok(StoreRequest::Catalogue));
    }

    #[test]
    fn load_request_round_trips() {
        let req = StoreRequest::Load {
            bundle_id: 0x0102_0304,
            node_id: 0x0001_3002,
        };
        let mut buf = [0u8; LOAD_REQUEST_LEN];
        let n = req.encode(&mut buf).expect("encodes");
        assert_eq!(n, LOAD_REQUEST_LEN);
        assert_eq!(StoreRequest::decode(&buf[..n]), Ok(req));
    }

    #[test]
    fn request_decode_rejects_empty_unknown_and_truncated() {
        assert_eq!(StoreRequest::decode(&[]), Err(Errno::LengthOutOfRange));
        assert_eq!(StoreRequest::decode(&[0xFF]), Err(Errno::OutOfRange));
        // A `Load` opcode with a truncated body is rejected, never read past
        // its bytes.
        assert_eq!(
            StoreRequest::decode(&[OP_LOAD, 1, 2, 3]),
            Err(Errno::LengthOutOfRange)
        );
    }

    #[test]
    fn request_encode_rejects_small_buffer() {
        let mut empty: [u8; 0] = [];
        assert_eq!(
            StoreRequest::Catalogue.encode(&mut empty),
            Err(Errno::BufferTooSmall)
        );
        let mut buf = [0u8; LOAD_REQUEST_LEN - 1];
        assert_eq!(
            StoreRequest::Load {
                bundle_id: 1,
                node_id: 2
            }
            .encode(&mut buf),
            Err(Errno::BufferTooSmall)
        );
    }

    #[test]
    fn catalogue_reply_round_trips_entries_and_their_bind_keys() {
        let a = [key(5, 2)];
        let b = [key(4, 16), key(7, 1)];
        let entries: [(u32, &[DriverBindKey]); 2] = [(10, &a), (20, &b)];
        let mut buf = [0u8; 1024];
        let n = encode_catalogue_reply(&mut buf, &entries).expect("encodes");

        let mut it = decode_catalogue_reply(&buf[..n]).expect("ok frame");
        let first = it.next().expect("first").expect("entry");
        assert_eq!(first.bundle_id, 10);
        assert_eq!(first.key_count(), 1);
        let mut kbuf = [key(0, 0); DRIVER_MANIFEST_MAX_BIND_KEYS as usize];
        assert_eq!(first.decode_keys(&mut kbuf), Ok(1));
        assert_eq!(&kbuf[..1], &a);

        let second = it.next().expect("second").expect("entry");
        assert_eq!(second.bundle_id, 20);
        assert_eq!(second.decode_keys(&mut kbuf), Ok(2));
        assert_eq!(&kbuf[..2], &b);
        assert!(it.next().is_none());
    }

    #[test]
    fn empty_catalogue_reply_round_trips() {
        let mut buf = [0u8; 16];
        let entries: [(u32, &[DriverBindKey]); 0] = [];
        let n = encode_catalogue_reply(&mut buf, &entries).expect("encodes");
        let mut it = decode_catalogue_reply(&buf[..n]).expect("ok frame");
        assert!(it.next().is_none());
    }

    #[test]
    fn catalogue_reply_fails_closed_on_small_buffer_never_truncates() {
        let a = [key(5, 2), key(6, 3)];
        let entries: [(u32, &[DriverBindKey]); 1] = [(1, &a)];
        let mut buf = [0u8; REPLY_OK_HEADER_LEN + 4];
        assert_eq!(
            encode_catalogue_reply(&mut buf, &entries),
            Err(Errno::BufferTooSmall)
        );
    }

    #[test]
    fn catalogue_reply_rejects_over_max_bind_keys() {
        let many = [key(1, 1); DRIVER_MANIFEST_MAX_BIND_KEYS as usize + 1];
        let entries: [(u32, &[DriverBindKey]); 1] = [(1, &many)];
        let mut buf = [0u8; 4096];
        assert_eq!(
            encode_catalogue_reply(&mut buf, &entries),
            Err(Errno::LengthOutOfRange)
        );
    }

    #[test]
    fn truncated_catalogue_entry_fails_closed() {
        // Status ok + declared count 1, but no entry body.
        let mut buf = [0u8; REPLY_OK_HEADER_LEN];
        put_i32(&mut buf, 0, 0);
        put_u32(&mut buf, REPLY_STATUS_LEN, 1);
        let mut it = decode_catalogue_reply(&buf).expect("ok frame");
        assert_eq!(it.next(), Some(Err(Errno::BadMagic)));
    }

    #[test]
    fn load_reply_round_trips_the_handle() {
        let mut buf = [0u8; LOAD_REPLY_LEN];
        let n = encode_load_reply(&mut buf, 0xDEAD_BEEF_0000_0001).expect("encodes");
        assert_eq!(decode_load_reply(&buf[..n]), Ok(0xDEAD_BEEF_0000_0001));
    }

    #[test]
    fn an_error_reply_surfaces_its_errno_for_both_decoders() {
        let mut buf = [0u8; 16];
        let n = encode_error_reply(&mut buf, Errno::PermissionDenied).expect("encodes");
        assert_eq!(reply_status(&buf[..n]), Err(Errno::PermissionDenied));
        assert_eq!(decode_load_reply(&buf[..n]), Err(Errno::PermissionDenied));
        assert!(decode_catalogue_reply(&buf[..n]).is_err());
    }

    #[test]
    fn truncated_load_reply_fails_closed() {
        // Status ok but no handle body.
        let mut buf = [0u8; REPLY_STATUS_LEN];
        put_i32(&mut buf, 0, 0);
        assert_eq!(decode_load_reply(&buf), Err(Errno::BadMagic));
    }

    #[test]
    fn unload_request_round_trips() {
        let req = StoreRequest::Unload {
            handle: 0xDEAD_BEEF_0000_0001,
        };
        let mut buf = [0u8; UNLOAD_REQUEST_LEN];
        let n = req.encode(&mut buf).expect("encodes");
        assert_eq!(n, UNLOAD_REQUEST_LEN);
        assert_eq!(StoreRequest::decode(&buf[..n]), Ok(req));
    }

    #[test]
    fn unload_request_rejects_truncated_body_and_small_buffer() {
        // An `Unload` opcode with a truncated body is rejected, never read
        // past its bytes.
        assert_eq!(
            StoreRequest::decode(&[OP_UNLOAD, 1, 2, 3]),
            Err(Errno::LengthOutOfRange)
        );
        let mut buf = [0u8; UNLOAD_REQUEST_LEN - 1];
        assert_eq!(
            StoreRequest::Unload { handle: 1 }.encode(&mut buf),
            Err(Errno::BufferTooSmall)
        );
    }

    #[test]
    fn unload_reply_round_trips_success() {
        let mut buf = [0u8; REPLY_STATUS_LEN];
        let n = encode_unload_reply(&mut buf).expect("encodes");
        assert_eq!(decode_unload_reply(&buf[..n]), Ok(()));
    }

    #[test]
    fn unload_reply_surfaces_an_in_band_error_fail_closed() {
        // An already-gone handle is reported in band as `NotFound`.
        let mut buf = [0u8; 16];
        let n = encode_error_reply(&mut buf, Errno::NotFound).expect("encodes");
        assert_eq!(decode_unload_reply(&buf[..n]), Err(Errno::NotFound));
    }

    #[test]
    fn read_config_request_round_trips_each_file() {
        for which in [SystemConfigFile::System, SystemConfigFile::Network] {
            let req = StoreRequest::ReadConfig { which };
            let mut buf = [0u8; READ_CONFIG_REQUEST_LEN];
            let n = req.encode(&mut buf).expect("encodes");
            assert_eq!(n, READ_CONFIG_REQUEST_LEN);
            assert_eq!(StoreRequest::decode(&buf[..n]), Ok(req));
        }
    }

    #[test]
    fn read_config_request_rejects_unknown_file_and_truncation() {
        // An unknown `which` discriminant is refused, never guessed.
        assert_eq!(
            StoreRequest::decode(&[OP_READ_CONFIG, 0xFF]),
            Err(Errno::OutOfRange)
        );
        // A `ReadConfig` opcode with no `which` byte is rejected.
        assert_eq!(
            StoreRequest::decode(&[OP_READ_CONFIG]),
            Err(Errno::LengthOutOfRange)
        );
        let mut empty: [u8; 0] = [];
        assert_eq!(
            StoreRequest::ReadConfig {
                which: SystemConfigFile::Network
            }
            .encode(&mut empty),
            Err(Errno::BufferTooSmall)
        );
    }

    #[test]
    fn system_config_file_discriminants_and_paths_are_stable() {
        assert_eq!(SystemConfigFile::System.as_u8(), 0);
        assert_eq!(SystemConfigFile::Network.as_u8(), 1);
        assert_eq!(SystemConfigFile::from_u8(0), Some(SystemConfigFile::System));
        assert_eq!(
            SystemConfigFile::from_u8(1),
            Some(SystemConfigFile::Network)
        );
        assert_eq!(SystemConfigFile::from_u8(2), None);
        assert_eq!(
            SystemConfigFile::System.path(),
            "/System/Settings/Configuration/system.conf"
        );
        assert_eq!(
            SystemConfigFile::Network.path(),
            "/System/Settings/Network/network.conf"
        );
        assert_eq!(
            SystemConfigFile::System.default_path(),
            "/System/Defaults/Configuration/system.conf"
        );
        assert_eq!(
            SystemConfigFile::Network.default_path(),
            "/System/Defaults/Network/network.conf"
        );
    }

    #[test]
    fn each_document_is_directly_inside_its_directory() {
        for which in [SystemConfigFile::System, SystemConfigFile::Network] {
            let name = which
                .path()
                .strip_prefix(which.dir())
                .and_then(|rest| rest.strip_prefix('/'));
            assert!(
                name.is_some_and(|name| !name.is_empty() && !name.contains('/')),
                "{which:?}"
            );
        }
    }

    #[test]
    fn every_volume_path_is_its_default_without_the_system_prefix() {
        // The pre-unlock reader resolves `volume_path` on the `/System`
        // volume while a VFS reader resolves `default_path`; if the two ever
        // named different files, an image would plant its default where one
        // of them never looks.
        for which in [SystemConfigFile::System, SystemConfigFile::Network] {
            assert_eq!(
                which.default_path().strip_prefix("/System/"),
                Some(which.volume_path()),
                "{which:?}"
            );
        }
    }

    #[test]
    fn the_effective_document_is_the_first_layer_that_exists() {
        let mut asked = [""; 2];
        let mut count = 0;
        let found = SystemConfigFile::Network.read_effective(|path| {
            asked[count] = path;
            count += 1;
            Ok::<_, Errno>((path == SystemConfigFile::Network.default_path()).then_some(path))
        });
        assert_eq!(found, Ok(Some(SystemConfigFile::Network.default_path())));
        assert_eq!(asked, SystemConfigFile::Network.layers());

        let administrator =
            SystemConfigFile::System.read_effective(|path| Ok::<_, Errno>(Some(path)));
        assert_eq!(administrator, Ok(Some(SystemConfigFile::System.path())));
        let neither = SystemConfigFile::System.read_effective(|_| Ok::<Option<()>, Errno>(None));
        assert_eq!(neither, Ok(None));
    }

    #[test]
    fn a_layer_that_cannot_be_read_ends_the_search() {
        let mut asked = 0;
        let refused = SystemConfigFile::System.read_effective(|_| {
            asked += 1;
            Err::<Option<()>, _>(Errno::PermissionDenied)
        });
        assert_eq!(refused, Err(Errno::PermissionDenied));
        assert_eq!(
            asked, 1,
            "the image's layer stands in only for an absent one"
        );
    }

    #[test]
    fn each_volume_form_is_the_default_seen_from_the_volume_root() {
        for which in [SystemConfigFile::System, SystemConfigFile::Network] {
            assert_eq!(
                which.on_volume_path().strip_prefix('/'),
                Some(which.volume_path()),
                "{which:?}"
            );
            assert_eq!(
                which.default_path().strip_prefix(SYSTEM_VIEW),
                Some(which.on_volume_path()),
                "{which:?}"
            );
        }
        assert_eq!(
            SystemConfigFile::Network.volume_path(),
            "Defaults/Network/network.conf"
        );
    }

    #[test]
    fn a_default_is_never_beneath_a_writable_mount() {
        // `/System/Logs` and `/System/Settings` are the root volume grafted
        // over the read-only one, so a default planted beneath either would
        // be hidden from every reader once the root is mounted.
        for which in [SystemConfigFile::System, SystemConfigFile::Network] {
            let default = which.default_path();
            assert!(
                !default.starts_with("/System/Settings/") && !default.starts_with("/System/Logs/"),
                "{which:?}"
            );
            assert_eq!(which.layers(), [which.path(), default], "{which:?}");
        }
    }

    #[test]
    fn config_reply_round_trips_bytes() {
        let payload = b"wan.kind ethernet\nwan.ipv6.method static\n";
        let mut buf = [0u8; 128];
        let n = encode_config_reply(&mut buf, payload).expect("encodes");
        assert_eq!(decode_config_reply(&buf[..n]), Ok(&payload[..]));
    }

    #[test]
    fn empty_config_reply_round_trips() {
        // A zero-length (but present, empty) config file is a valid success
        // frame distinct from the `NotFound` error frame.
        let mut buf = [0u8; 16];
        let n = encode_config_reply(&mut buf, &[]).expect("encodes");
        assert_eq!(decode_config_reply(&buf[..n]), Ok(&[][..]));
    }

    #[test]
    fn config_reply_surfaces_not_found_fail_closed() {
        // An absent config is an in-band `NotFound`, never an empty success.
        let mut buf = [0u8; 16];
        let n = encode_error_reply(&mut buf, Errno::NotFound).expect("encodes");
        assert_eq!(decode_config_reply(&buf[..n]), Err(Errno::NotFound));
    }

    #[test]
    fn config_reply_fails_closed_on_small_buffer_and_truncation() {
        // The config is never truncated into a too-small buffer.
        let mut small = [0u8; CONFIG_REPLY_HEADER_LEN + 2];
        assert_eq!(
            encode_config_reply(&mut small, b"four"),
            Err(Errno::BufferTooSmall)
        );
        // A success frame whose declared length runs past the frame is
        // rejected rather than read out of bounds.
        let mut buf = [0u8; CONFIG_REPLY_HEADER_LEN + 2];
        put_i32(&mut buf, 0, 0);
        put_u32(&mut buf, REPLY_STATUS_LEN, 100);
        assert_eq!(decode_config_reply(&buf), Err(Errno::BadMagic));
    }
    #[test]
    fn the_most_negative_status_word_fails_closed_instead_of_aborting() {
        let mut reply = [0u8; REPLY_STATUS_LEN];
        reply.copy_from_slice(&i32::MIN.to_le_bytes());
        assert_eq!(reply_status(&reply), Err(Errno::BadMagic));
    }
}
