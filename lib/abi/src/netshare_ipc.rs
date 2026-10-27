//! `netshare-v1`: the protocol programs reach the network-share service,
//! `smbfs`, through (`plans/SMB.md` SMB8).
//!
//! A program asks an endpoint which shares it offers
//! ([`NetshareRequest::Shares`]) and has one served
//! ([`NetshareRequest::Mount`]): the service connects, authenticates and
//! offers the share's volume to the caller, which attaches the offer itself
//! (`plans/FSSERVE.md` FSS6), so where the volume goes and how it is
//! presented are judged by the kernel against the real caller. A program
//! withdraws a mount it made ([`NetshareRequest::Unmount`]) and lists its own
//! ([`NetshareRequest::Mounts`]).
//!
//! Every request is one call and one reply. Listing and mounting are network
//! work the service answers once it completes, so an interactive surface
//! makes the call from a worker, never from its own loop. The caller's
//! identity and login session are its kernel-attested origin, never a field.
//!
//! Only a program that may reach the network itself may call: the endpoint's
//! send capability is `CAP_NET`, enforced by the kernel before the service
//! reads a byte. Everything the service does for a caller is network work,
//! and a request's names cross the wire, in a lookup and in the sign-in, so a
//! program without that authority could otherwise probe hosts and ports, or
//! carry data out, through the service.
//!
//! Credentials cross in the clear over the kernel-copied IPC buffer, the
//! trust boundary a password typed at a prompt already crosses. So the
//! endpoint is reserved, the password is a [`Secret`], and both ends erase
//! their copies once the exchange resolves. A remembered credential is
//! offered only with the [`Pin`] it was remembered under: the service checks
//! what the connection proves against it before any credential leaves, and a
//! host whose pin differs is [`NetshareError::PinMismatch`] — new and
//! unknown — and never authenticated to.
//!
//! # Wire shape
//!
//! A request is a fixed header and the fields its operation carries:
//!
//! ```text
//! 0   u32   magic        NETSHARE_REQUEST_MAGIC
//! 4   u16   version      NETSHARE_VERSION
//! 6   u8    op
//! 7   u8    flags        Shares: special; Mount: read-only; else zero
//! 8   ...   Shares, Mount: host, address, interface, port, pin, floor,
//!                          user, domain, password; Mount: then the share
//!           Unmount: the offer
//! ```
//!
//! A reply is a fixed header, then the observed pin for a refused pin, or
//! what the operation answers; [`Op::max_reply`] bounds each operation's:
//!
//! ```text
//! 0   u32   magic        NETSHARE_REPLY_MAGIC
//! 4   u16   version      NETSHARE_VERSION
//! 6   u8    op           the request's, or 0 for one the service could not read
//! 7   u8    outcome      0 answered, else the refusal
//! 8   u8    detail       a rejection's or a shortfall's reason, else zero
//! ```
//!
//! A string is a `u16` length and that many bytes of UTF-8; an address is a
//! family byte (0 for none) and sixteen bytes. A field an operation does not
//! use is zero, and a frame ends exactly where its last field does.

use core::net::IpAddr;

use crate::account::MAX_PASSWORD_LEN;
use crate::fsserve::OfferId;
use crate::le::{Reader, Writer};
use crate::net_ipc::{
    address_parts, ip_from_parts, is_unicast, validate_if_name, NetAddrFamily, IF_NAME_LEN,
};
use crate::secret::Secret;
use crate::Errno;

/// Reserved well-known call-endpoint id of the network-share service
/// (`"NSH1"` little-endian).
///
/// Reserved ([`crate::ipc::is_reserved_endpoint`]), so binding it requires
/// `CAP_IPC_BIND_PRIVILEGED`: a squatter claiming it first would be handed
/// every password a program offers a share. Calling it requires `CAP_NET`.
pub const NETSHARE_ENDPOINT: u64 = u64::from_le_bytes(*b"NSH1\0\0\0\0");

/// Magic opening every request.
pub const NETSHARE_REQUEST_MAGIC: u32 = u32::from_le_bytes(*b"NSHQ");

/// Magic opening every reply.
pub const NETSHARE_REPLY_MAGIC: u32 = u32::from_le_bytes(*b"NSHA");

/// The protocol version every frame carries; any other is refused.
pub const NETSHARE_VERSION: u16 = 1;

/// The port SMB is served on.
pub const SMB_PORT: u16 = 445;

/// The longest host name, in bytes.
pub const HOST_MAX: usize = 255;

/// The longest user or domain name, in bytes: within the 256 UTF-16 units
/// NTLM carries a name in.
pub const ACCOUNT_NAME_MAX: usize = 256;

/// The longest share name, in characters (MS-SRVS §2.2.2.10's
/// `NAMETYPE_SHARE`).
pub const SHARE_NAME_MAX: usize = 80;

/// The longest share name, in bytes of UTF-8.
const SHARE_NAME_BYTES: usize = SHARE_NAME_MAX * 4;

/// The longest share remark, in bytes: past the 256 UTF-16 units of
/// Windows' `MAXCOMMENTSZ`. A longer one is cut at a character.
pub const REMARK_MAX: usize = 1024;

/// The most shares one listing carries.
pub const SHARES_MAX: usize = 256;

/// The most mounts one principal holds.
pub const MOUNTS_MAX: usize = 32;

const HEADER_LEN: usize = 8;
const REPLY_HEADER_LEN: usize = 9;
const ADDRESS_LEN: usize = 1 + 16;
const PIN_LEN: usize = 16 + ADDRESS_LEN;

/// The largest request: a mount with every field at its bound.
pub const NETSHARE_MAX_REQUEST: usize = HEADER_LEN
    + (2 + HOST_MAX)
    + ADDRESS_LEN
    + IF_NAME_LEN
    + 2
    + 1
    + PIN_LEN
    + 1
    + 2 * (2 + ACCOUNT_NAME_MAX)
    + (2 + MAX_PASSWORD_LEN)
    + (2 + SHARE_NAME_BYTES);

const SHARE_ENTRY_MAX: usize = (2 + SHARE_NAME_BYTES) + 1 + (2 + REMARK_MAX);
const MOUNT_ENTRY_MAX: usize = 8 + (2 + HOST_MAX) + (2 + SHARE_NAME_BYTES) + 1 + 1;
const REFUSED_MAX: usize = REPLY_HEADER_LEN + PIN_LEN;
const SHARES_REPLY_MAX: usize =
    REPLY_HEADER_LEN + PIN_LEN + 4 + 4 + 2 + SHARES_MAX * SHARE_ENTRY_MAX;
const MOUNTED_LEN: usize = REPLY_HEADER_LEN + PIN_LEN + 8;
const MOUNTS_REPLY_MAX: usize = REPLY_HEADER_LEN + 2 + MOUNTS_MAX * MOUNT_ENTRY_MAX;

/// The largest reply: a listing of [`SHARES_MAX`] shares at their bounds.
pub const NETSHARE_MAX_REPLY: usize = Op::Shares.max_reply();

const _: () = assert!(
    Op::Mount.max_reply() <= NETSHARE_MAX_REPLY
        && Op::Unmount.max_reply() <= NETSHARE_MAX_REPLY
        && Op::Mounts.max_reply() <= NETSHARE_MAX_REPLY
);

const FLAG_SPECIAL: u8 = 0x01;
const FLAG_READ_ONLY: u8 = 0x01;

/// Whether `name` is a share a tree connect can name: one to
/// [`SHARE_NAME_MAX`] characters, none a path separator or a control
/// character.
#[must_use]
pub fn is_share_name(name: &str) -> bool {
    let mut count = 0;
    for c in name.chars() {
        if matches!(c, '\\' | '/') || c.is_control() {
            return false;
        }
        count += 1;
    }
    (1..=SHARE_NAME_MAX).contains(&count)
}

/// Whether `host` is one a request may name: one to [`HOST_MAX`] bytes, none
/// of them a control character, a space or a path separator — what a tree
/// path and a service name are built from.
#[must_use]
pub fn is_host_name(host: &str) -> bool {
    (1..=HOST_MAX).contains(&host.len())
        && !host
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '\\' | '/'))
}

fn is_account_name(name: &str) -> bool {
    name.len() <= ACCOUNT_NAME_MAX && !name.chars().any(char::is_control)
}

/// What a request does.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Op {
    /// [`NetshareRequest::Shares`].
    Shares = 1,
    /// [`NetshareRequest::Mount`].
    Mount = 2,
    /// [`NetshareRequest::Unmount`].
    Unmount = 3,
    /// [`NetshareRequest::Mounts`].
    Mounts = 4,
}

impl Op {
    const fn from_u8(byte: u8) -> Result<Self, Errno> {
        match byte {
            1 => Ok(Self::Shares),
            2 => Ok(Self::Mount),
            3 => Ok(Self::Unmount),
            4 => Ok(Self::Mounts),
            _ => Err(Errno::OutOfRange),
        }
    }

    /// The largest reply to a request of this kind, refusals included: the
    /// buffer a caller receives it in.
    #[must_use]
    pub const fn max_reply(self) -> usize {
        let answer = match self {
            Self::Shares => SHARES_REPLY_MAX,
            Self::Mount => MOUNTED_LEN,
            Self::Unmount => REPLY_HEADER_LEN,
            Self::Mounts => MOUNTS_REPLY_MAX,
        };
        if answer > REFUSED_MAX {
            answer
        } else {
            REFUSED_MAX
        }
    }
}

/// The oldest SMB dialect a connection may settle on: 3.0 unless a caller
/// raises it. Nothing lowers it.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum DialectFloor {
    /// SMB 3.0.
    #[default]
    Smb300 = 0,
    /// SMB 3.0.2.
    Smb302 = 1,
    /// SMB 3.1.1.
    Smb311 = 2,
}

impl DialectFloor {
    const fn from_u8(byte: u8) -> Result<Self, Errno> {
        match byte {
            0 => Ok(Self::Smb300),
            1 => Ok(Self::Smb302),
            2 => Ok(Self::Smb311),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// The endpoint a request is for, as the user chose it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Endpoint<'a> {
    /// The host's name: what the service name `cifs/<host>` and the tree
    /// path `\\<host>\<share>` are built from.
    pub host: &'a str,
    /// The unicast address to reach it at, as shown at the moment of
    /// choice, or `None` for the service to resolve the name.
    pub address: Option<IpAddr>,
    /// The link the host was found on: required for an IPv6 link-local
    /// address, and what a link-local name is resolved on.
    pub interface: Option<[u8; IF_NAME_LEN]>,
    /// The port, normally [`SMB_PORT`].
    pub port: u16,
}

/// What a connection proved about its endpoint: the server GUID its
/// `NEGOTIATE` answered with, and the address it was reached at.
///
/// With the host as chosen, this is what a remembered credential is bound
/// to.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Pin {
    /// The server's GUID.
    pub server_guid: [u8; 16],
    /// The address it was reached at.
    pub address: IpAddr,
}

/// An NTLMv2 account and its password.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Credentials<'a> {
    /// The account's name; never empty, since an empty one is an anonymous
    /// logon.
    pub user: &'a str,
    /// Its domain, or empty for an account of the server's own.
    pub domain: &'a str,
    /// The password.
    pub password: Secret<'a>,
}

/// How a request reaches its endpoint and signs in to it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Connect<'a> {
    /// The endpoint.
    pub endpoint: Endpoint<'a>,
    /// The pin the credentials were remembered under, which the connection
    /// must prove before they are offered; `None` for credentials the user
    /// has just entered.
    pub pin: Option<Pin>,
    /// The oldest dialect accepted.
    pub floor: DialectFloor,
    /// The credentials.
    pub credentials: Credentials<'a>,
}

/// One request to the network-share service.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NetshareRequest<'a> {
    /// The disk shares the endpoint offers: the special ones (`C$`,
    /// `ADMIN$`) only when `special` is set.
    Shares {
        /// The endpoint and how to sign in to it.
        connect: Connect<'a>,
        /// Whether to list the special shares.
        special: bool,
    },
    /// Serve `share` and offer its volume to the caller, read-only when
    /// `read_only` is set.
    Mount {
        /// The endpoint and how to sign in to it.
        connect: Connect<'a>,
        /// The share.
        share: &'a str,
        /// Whether to serve it read-only.
        read_only: bool,
    },
    /// Withdraw a mount the caller made, flushing it first.
    Unmount {
        /// The mount's offer.
        offer: OfferId,
    },
    /// The caller's own mounts.
    Mounts,
}

impl NetshareRequest<'_> {
    /// What the request does.
    #[must_use]
    pub const fn op(&self) -> Op {
        match self {
            Self::Shares { .. } => Op::Shares,
            Self::Mount { .. } => Op::Mount,
            Self::Unmount { .. } => Op::Unmount,
            Self::Mounts => Op::Mounts,
        }
    }
}

fn check_connect(connect: &Connect<'_>) -> Result<(), Errno> {
    let endpoint = &connect.endpoint;
    let link_local = matches!(endpoint.address, Some(IpAddr::V6(v6)) if v6.is_unicast_link_local());
    let credentials = &connect.credentials;
    if !is_host_name(endpoint.host)
        || endpoint.address.is_some_and(|address| !is_unicast(address))
        || endpoint.port == 0
        || (link_local && endpoint.interface.is_none())
        || credentials.user.is_empty()
        || !is_account_name(credentials.user)
        || !is_account_name(credentials.domain)
        || credentials.password.len() > MAX_PASSWORD_LEN
    {
        return Err(Errno::OutOfRange);
    }
    if let Some(interface) = &endpoint.interface {
        validate_if_name(interface)?;
    }
    Ok(())
}

fn put_address(w: &mut Writer<'_>, address: Option<IpAddr>) -> Result<(), Errno> {
    match address {
        None => {
            w.u8(0)?;
            w.bytes(&[0; 16])
        }
        Some(address) => {
            let (family, bytes) = address_parts(address);
            w.u8(family.as_u8())?;
            w.bytes(&bytes)
        }
    }
}

fn read_address(r: &mut Reader<'_>) -> Result<Option<IpAddr>, Errno> {
    let family = r.u8()?;
    let mut bytes = [0; 16];
    bytes.copy_from_slice(r.take(16)?);
    match family {
        0 if bytes == [0; 16] => Ok(None),
        4 if bytes[4..] == [0; 12] => Ok(Some(ip_from_parts(NetAddrFamily::V4, bytes))),
        6 => Ok(Some(ip_from_parts(NetAddrFamily::V6, bytes))),
        _ => Err(Errno::OutOfRange),
    }
}

fn put_pin(w: &mut Writer<'_>, pin: &Pin) -> Result<(), Errno> {
    w.bytes(&pin.server_guid)?;
    put_address(w, Some(pin.address))
}

fn read_pin(r: &mut Reader<'_>) -> Result<Pin, Errno> {
    let mut server_guid = [0; 16];
    server_guid.copy_from_slice(r.take(16)?);
    let address = read_address(r)?.ok_or(Errno::OutOfRange)?;
    Ok(Pin {
        server_guid,
        address,
    })
}

fn put_connect(w: &mut Writer<'_>, connect: &Connect<'_>) -> Result<(), Errno> {
    let endpoint = &connect.endpoint;
    w.str(endpoint.host)?;
    put_address(w, endpoint.address)?;
    w.bytes(&endpoint.interface.unwrap_or([0; IF_NAME_LEN]))?;
    w.u16(endpoint.port)?;
    match &connect.pin {
        None => {
            w.u8(0)?;
            w.bytes(&[0; PIN_LEN])?;
        }
        Some(pin) => {
            w.u8(1)?;
            put_pin(w, pin)?;
        }
    }
    w.u8(connect.floor as u8)?;
    w.str(connect.credentials.user)?;
    w.str(connect.credentials.domain)?;
    w.str(connect.credentials.password.expose())
}

fn read_connect<'a>(r: &mut Reader<'a>) -> Result<Connect<'a>, Errno> {
    let host = r.str()?;
    let address = read_address(r)?;
    let mut interface = [0; IF_NAME_LEN];
    interface.copy_from_slice(r.take(IF_NAME_LEN)?);
    let port = r.u16()?;
    let pin = match r.u8()? {
        0 => {
            if r.take(PIN_LEN)? != [0; PIN_LEN] {
                return Err(Errno::OutOfRange);
            }
            None
        }
        1 => Some(read_pin(r)?),
        _ => return Err(Errno::OutOfRange),
    };
    let connect = Connect {
        endpoint: Endpoint {
            host,
            address,
            interface: (interface != [0; IF_NAME_LEN]).then_some(interface),
            port,
        },
        pin,
        floor: DialectFloor::from_u8(r.u8()?)?,
        credentials: Credentials {
            user: r.str()?,
            domain: r.str()?,
            password: Secret::new(r.str()?),
        },
    };
    check_connect(&connect)?;
    Ok(connect)
}

impl<'a> NetshareRequest<'a> {
    /// Encode the request into `out`, returning its length.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a field the protocol refuses — a host,
    /// account or share name out of shape, an address that is not unicast,
    /// a link-local one with no interface, port zero;
    /// [`Errno::BufferTooSmall`] when `out` cannot hold it.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        let mut w = Writer::new(out);
        w.u32(NETSHARE_REQUEST_MAGIC)?;
        w.u16(NETSHARE_VERSION)?;
        w.u8(self.op() as u8)?;
        match self {
            Self::Shares { connect, special } => {
                check_connect(connect)?;
                w.u8(if *special { FLAG_SPECIAL } else { 0 })?;
                put_connect(&mut w, connect)?;
            }
            Self::Mount {
                connect,
                share,
                read_only,
            } => {
                check_connect(connect)?;
                if !is_share_name(share) {
                    return Err(Errno::OutOfRange);
                }
                w.u8(if *read_only { FLAG_READ_ONLY } else { 0 })?;
                put_connect(&mut w, connect)?;
                w.str(share)?;
            }
            Self::Unmount { offer } => {
                w.u8(0)?;
                w.u64(offer.get())?;
            }
            Self::Mounts => w.u8(0)?,
        }
        Ok(w.written())
    }

    /// Decode the request in `bytes`, failing closed on anything another
    /// encoder could not have written.
    ///
    /// # Errors
    ///
    /// [`Errno::BadMagic`] for a frame of another protocol;
    /// [`Errno::OutOfRange`] for another version, an unknown operation or
    /// value, a field out of shape, or a flag the operation does not carry;
    /// [`Errno::LengthOutOfRange`] for a frame past
    /// [`NETSHARE_MAX_REQUEST`], short, or with bytes past its end.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Errno> {
        if bytes.len() > NETSHARE_MAX_REQUEST {
            return Err(Errno::LengthOutOfRange);
        }
        let mut r = Reader::new(bytes);
        if r.u32()? != NETSHARE_REQUEST_MAGIC {
            return Err(Errno::BadMagic);
        }
        if r.u16()? != NETSHARE_VERSION {
            return Err(Errno::OutOfRange);
        }
        let op = Op::from_u8(r.u8()?)?;
        let flags = r.u8()?;
        let request = match op {
            Op::Shares if flags & !FLAG_SPECIAL == 0 => Self::Shares {
                connect: read_connect(&mut r)?,
                special: flags & FLAG_SPECIAL != 0,
            },
            Op::Mount if flags & !FLAG_READ_ONLY == 0 => {
                let connect = read_connect(&mut r)?;
                let share = r.str()?;
                if !is_share_name(share) {
                    return Err(Errno::OutOfRange);
                }
                Self::Mount {
                    connect,
                    share,
                    read_only: flags & FLAG_READ_ONLY != 0,
                }
            }
            Op::Unmount if flags == 0 => Self::Unmount {
                offer: OfferId::new(r.u64()?).ok_or(Errno::OutOfRange)?,
            },
            Op::Mounts if flags == 0 => Self::Mounts,
            _ => return Err(Errno::OutOfRange),
        };
        if !r.exhausted() {
            return Err(Errno::LengthOutOfRange);
        }
        Ok(request)
    }
}

/// Why a server refused the credentials.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Rejection {
    /// The user, or its password, is wrong: servers do not say which.
    Credentials = 1,
    /// The account is disabled.
    Disabled = 2,
    /// The account is locked out.
    Locked = 3,
    /// The password has expired, or must be changed.
    Expired = 4,
    /// The account may not sign in now, or from here.
    Restricted = 5,
}

impl Rejection {
    const fn from_u8(byte: u8) -> Result<Self, Errno> {
        match byte {
            1 => Ok(Self::Credentials),
            2 => Ok(Self::Disabled),
            3 => Ok(Self::Locked),
            4 => Ok(Self::Expired),
            5 => Ok(Self::Restricted),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// What a server offers too little of to be reached at all.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Shortfall {
    /// No dialect at or above the floor.
    Dialect = 1,
    /// No encryption.
    Encryption = 2,
    /// No signing algorithm this system accepts.
    Signing = 3,
    /// No sign-in this system can use.
    Authentication = 4,
    /// Only a guest or anonymous session, which nothing protects.
    Guest = 5,
}

impl Shortfall {
    const fn from_u8(byte: u8) -> Result<Self, Errno> {
        match byte {
            1 => Ok(Self::Dialect),
            2 => Ok(Self::Encryption),
            3 => Ok(Self::Signing),
            4 => Ok(Self::Authentication),
            5 => Ok(Self::Guest),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// Why the service refused a request.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NetshareError {
    /// The host's name resolves to no address.
    UnknownHost,
    /// No connection to the endpoint could be made.
    Unreachable,
    /// The host is not the one the pin names: it is shown as new and
    /// unknown. Carries what the connection proved instead.
    PinMismatch(Pin),
    /// The server falls short of what this system requires.
    Unsupported(Shortfall),
    /// The connection failed its integrity checks: something altered or
    /// downgraded it on the way.
    Tampered,
    /// The server broke the protocol.
    Protocol,
    /// The server answered with more than this system accepts: more than
    /// [`SHARES_MAX`] shares, or an answer past a fixed bound.
    TooLarge,
    /// The server refused the credentials.
    Rejected(Rejection),
    /// The server offers no disk share of that name.
    NoSuchShare,
    /// The server refused the account the share, or the listing.
    Denied,
    /// The caller has no login session for a mount to end with.
    NoSession,
    /// The caller made no such mount.
    NoSuchMount,
    /// The caller holds as many mounts or connections as it may.
    Limit,
    /// The service failed.
    Failed,
}

impl NetshareError {
    const fn codes(&self) -> (u8, u8) {
        match self {
            Self::UnknownHost => (1, 0),
            Self::Unreachable => (2, 0),
            Self::PinMismatch(_) => (3, 0),
            Self::Unsupported(shortfall) => (4, *shortfall as u8),
            Self::Tampered => (5, 0),
            Self::Protocol => (6, 0),
            Self::TooLarge => (7, 0),
            Self::Rejected(rejection) => (8, *rejection as u8),
            Self::NoSuchShare => (9, 0),
            Self::Denied => (10, 0),
            Self::NoSession => (11, 0),
            Self::NoSuchMount => (12, 0),
            Self::Limit => (13, 0),
            Self::Failed => (14, 0),
        }
    }

    fn read(outcome: u8, detail: u8, r: &mut Reader<'_>) -> Result<Self, Errno> {
        let error = match (outcome, detail) {
            (1, 0) => Self::UnknownHost,
            (2, 0) => Self::Unreachable,
            (3, 0) => Self::PinMismatch(read_pin(r)?),
            (4, detail) => Self::Unsupported(Shortfall::from_u8(detail)?),
            (5, 0) => Self::Tampered,
            (6, 0) => Self::Protocol,
            (7, 0) => Self::TooLarge,
            (8, detail) => Self::Rejected(Rejection::from_u8(detail)?),
            (9, 0) => Self::NoSuchShare,
            (10, 0) => Self::Denied,
            (11, 0) => Self::NoSession,
            (12, 0) => Self::NoSuchMount,
            (13, 0) => Self::Limit,
            (14, 0) => Self::Failed,
            _ => return Err(Errno::OutOfRange),
        };
        Ok(error)
    }
}

/// A listing's records: those an encoder is handed ([`Records::new`]), or
/// those a decoder checked, read again as they are walked.
#[derive(Copy, Clone, Debug)]
pub struct Records<'a, T>(Source<'a, T>);

#[derive(Copy, Clone, Debug)]
enum Source<'a, T> {
    Listed(&'a [T]),
    /// Only a decoder makes one, having checked every record.
    Read {
        bytes: &'a [u8],
        count: usize,
    },
}

impl<'a, T: Copy> Records<'a, T> {
    /// `records`, to encode.
    #[must_use]
    pub const fn new(records: &'a [T]) -> Self {
        Self(Source::Listed(records))
    }

    /// How many records there are.
    #[must_use]
    pub const fn len(&self) -> usize {
        match self.0 {
            Source::Listed(records) => records.len(),
            Source::Read { count, .. } => count,
        }
    }

    /// Whether there are none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn walk(&self, read: fn(&mut Reader<'a>) -> Result<T, Errno>) -> impl Iterator<Item = T> + 'a {
        let (listed, bytes, count): (&'a [T], &'a [u8], usize) = match self.0 {
            Source::Listed(records) => (records, &[], 0),
            Source::Read { bytes, count } => (&[], bytes, count),
        };
        let mut r = Reader::new(bytes);
        // Read records were all checked, so reading them again cannot fail.
        listed
            .iter()
            .copied()
            .chain((0..count).map_while(move |_| read(&mut r).ok()))
    }

    fn write(
        &self,
        w: &mut Writer<'_>,
        max: usize,
        read: fn(&mut Reader<'a>) -> Result<T, Errno>,
        write: fn(&T, &mut Writer<'_>) -> Result<(), Errno>,
    ) -> Result<(), Errno> {
        if self.len() > max {
            return Err(Errno::OutOfRange);
        }
        w.u16(u16::try_from(self.len()).map_err(|_| Errno::OutOfRange)?)?;
        for record in self.walk(read) {
            write(&record, w)?;
        }
        Ok(())
    }

    fn read(
        r: &mut Reader<'a>,
        max: usize,
        read: fn(&mut Reader<'a>) -> Result<T, Errno>,
    ) -> Result<Self, Errno> {
        let count = usize::from(r.u16()?);
        if count > max {
            return Err(Errno::OutOfRange);
        }
        let start = r.position();
        for _ in 0..count {
            read(r)?;
        }
        Ok(Self(Source::Read {
            bytes: r.since(start)?,
            count,
        }))
    }
}

/// The walk and the equality of a listing of `$entry`s, read by `$read`.
macro_rules! listing {
    ($entry:ident, $read:path) => {
        impl<'a> Records<'a, $entry<'a>> {
            /// The records, in order.
            pub fn iter(&self) -> impl Iterator<Item = $entry<'a>> + 'a {
                self.walk($read)
            }
        }

        impl PartialEq for Records<'_, $entry<'_>> {
            fn eq(&self, other: &Self) -> bool {
                self.len() == other.len() && self.iter().eq(other.iter())
            }
        }

        impl Eq for Records<'_, $entry<'_>> {}
    };
}

listing!(ShareEntry, ShareEntry::read);
listing!(MountEntry, MountEntry::read);

/// One share an endpoint offers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ShareEntry<'a> {
    /// Its name, one a tree connect can take.
    pub name: &'a str,
    /// Whether it is a special share: `C$`, `ADMIN$`.
    pub special: bool,
    /// The server's description of it: a stranger's text, never display-safe.
    pub remark: &'a str,
}

impl<'a> ShareEntry<'a> {
    fn write(&self, w: &mut Writer<'_>) -> Result<(), Errno> {
        if !is_share_name(self.name) || self.remark.len() > REMARK_MAX {
            return Err(Errno::OutOfRange);
        }
        w.str(self.name)?;
        w.u8(u8::from(self.special))?;
        w.str(self.remark)
    }

    fn read(r: &mut Reader<'a>) -> Result<Self, Errno> {
        let name = r.str()?;
        let special = match r.u8()? {
            0 => false,
            1 => true,
            _ => return Err(Errno::OutOfRange),
        };
        let remark = r.str()?;
        if !is_share_name(name) || remark.len() > REMARK_MAX {
            return Err(Errno::OutOfRange);
        }
        Ok(Self {
            name,
            special,
            remark,
        })
    }
}

/// Where a mount's connection stands.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum MountState {
    /// Connected and serving.
    Serving = 1,
    /// The connection dropped and is being re-established.
    Reconnecting = 2,
    /// The connection could not be re-established in time: every operation
    /// fails until the mount is withdrawn.
    Lost = 3,
}

impl MountState {
    const fn from_u8(byte: u8) -> Result<Self, Errno> {
        match byte {
            1 => Ok(Self::Serving),
            2 => Ok(Self::Reconnecting),
            3 => Ok(Self::Lost),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// One mount the caller made.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MountEntry<'a> {
    /// Its offer.
    pub offer: OfferId,
    /// The host it is served from.
    pub host: &'a str,
    /// The share.
    pub share: &'a str,
    /// Whether it is served read-only.
    pub read_only: bool,
    /// Where its connection stands.
    pub state: MountState,
}

impl<'a> MountEntry<'a> {
    fn write(&self, w: &mut Writer<'_>) -> Result<(), Errno> {
        if !is_host_name(self.host) || !is_share_name(self.share) {
            return Err(Errno::OutOfRange);
        }
        w.u64(self.offer.get())?;
        w.str(self.host)?;
        w.str(self.share)?;
        w.u8(u8::from(self.read_only))?;
        w.u8(self.state as u8)
    }

    fn read(r: &mut Reader<'a>) -> Result<Self, Errno> {
        let offer = OfferId::new(r.u64()?).ok_or(Errno::OutOfRange)?;
        let host = r.str()?;
        let share = r.str()?;
        let read_only = match r.u8()? {
            0 => false,
            1 => true,
            _ => return Err(Errno::OutOfRange),
        };
        let state = MountState::from_u8(r.u8()?)?;
        if !is_host_name(host) || !is_share_name(share) {
            return Err(Errno::OutOfRange);
        }
        Ok(Self {
            offer,
            host,
            share,
            read_only,
            state,
        })
    }
}

/// An endpoint's shares.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Shares<'a> {
    /// What the connection proved about the endpoint.
    pub pin: Pin,
    /// How many special shares were left out, since they were not asked for.
    pub hidden: u32,
    /// How many shares the server named in a way no tree connect could take.
    pub unnamed: u32,
    /// The shares, in the server's order.
    pub shares: Records<'a, ShareEntry<'a>>,
}

/// The service's answer to one request.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NetshareReply<'a> {
    /// The answer to [`NetshareRequest::Shares`].
    Shares(Shares<'a>),
    /// The answer to [`NetshareRequest::Mount`]: the share is served and
    /// `offer` waits for the caller to attach it.
    Mounted {
        /// What the connection proved about the endpoint.
        pin: Pin,
        /// The offer.
        offer: OfferId,
    },
    /// The answer to [`NetshareRequest::Unmount`].
    Unmounted,
    /// The answer to [`NetshareRequest::Mounts`].
    Mounts(Records<'a, MountEntry<'a>>),
    /// The request `op` was refused.
    Refused {
        /// What the request did.
        op: Op,
        /// Why it was refused.
        error: NetshareError,
    },
    /// The service could not read the request: its sender speaks another
    /// version of the protocol, or speaks it wrongly.
    Unreadable,
}

impl<'a> NetshareReply<'a> {
    const fn op_byte(&self) -> u8 {
        match self {
            Self::Shares(_) => Op::Shares as u8,
            Self::Mounted { .. } => Op::Mount as u8,
            Self::Unmounted => Op::Unmount as u8,
            Self::Mounts(_) => Op::Mounts as u8,
            Self::Refused { op, .. } => *op as u8,
            Self::Unreadable => 0,
        }
    }

    /// Encode the reply into `out`, returning its length.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a record out of shape or a listing past its
    /// bound; [`Errno::BufferTooSmall`] when `out` cannot hold it.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Errno> {
        let mut w = Writer::new(out);
        w.u32(NETSHARE_REPLY_MAGIC)?;
        w.u16(NETSHARE_VERSION)?;
        w.u8(self.op_byte())?;
        match self {
            Self::Refused { error, .. } => {
                let (outcome, detail) = error.codes();
                w.u8(outcome)?;
                w.u8(detail)?;
                if let NetshareError::PinMismatch(pin) = error {
                    put_pin(&mut w, pin)?;
                }
            }
            answer => {
                w.u8(0)?;
                w.u8(0)?;
                match answer {
                    Self::Shares(shares) => {
                        put_pin(&mut w, &shares.pin)?;
                        w.u32(shares.hidden)?;
                        w.u32(shares.unnamed)?;
                        shares.shares.write(
                            &mut w,
                            SHARES_MAX,
                            ShareEntry::read,
                            ShareEntry::write,
                        )?;
                    }
                    Self::Mounted { pin, offer } => {
                        put_pin(&mut w, pin)?;
                        w.u64(offer.get())?;
                    }
                    Self::Mounts(mounts) => {
                        mounts.write(&mut w, MOUNTS_MAX, MountEntry::read, MountEntry::write)?;
                    }
                    Self::Unmounted | Self::Refused { .. } | Self::Unreadable => {}
                }
            }
        }
        Ok(w.written())
    }

    /// Decode the reply in `bytes`, failing closed on anything another
    /// encoder could not have written.
    ///
    /// # Errors
    ///
    /// [`Errno::BadMagic`] for a frame of another protocol;
    /// [`Errno::OutOfRange`] for another version, an unknown operation,
    /// outcome or value, a record out of shape, or an unreadable request's
    /// answer that carries anything;
    /// [`Errno::LengthOutOfRange`] for a frame past [`NETSHARE_MAX_REPLY`],
    /// short, or with bytes past its end.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Errno> {
        if bytes.len() > NETSHARE_MAX_REPLY {
            return Err(Errno::LengthOutOfRange);
        }
        let mut r = Reader::new(bytes);
        if r.u32()? != NETSHARE_REPLY_MAGIC {
            return Err(Errno::BadMagic);
        }
        if r.u16()? != NETSHARE_VERSION {
            return Err(Errno::OutOfRange);
        }
        let op = r.u8()?;
        let outcome = r.u8()?;
        let detail = r.u8()?;
        let reply = if op == 0 {
            if (outcome, detail) != (0, 0) {
                return Err(Errno::OutOfRange);
            }
            Self::Unreadable
        } else {
            let op = Op::from_u8(op)?;
            match (outcome, detail) {
                (0, 0) => match op {
                    Op::Shares => Self::Shares(Shares {
                        pin: read_pin(&mut r)?,
                        hidden: r.u32()?,
                        unnamed: r.u32()?,
                        shares: Records::read(&mut r, SHARES_MAX, ShareEntry::read)?,
                    }),
                    Op::Mount => Self::Mounted {
                        pin: read_pin(&mut r)?,
                        offer: OfferId::new(r.u64()?).ok_or(Errno::OutOfRange)?,
                    },
                    Op::Unmount => Self::Unmounted,
                    Op::Mounts => {
                        Self::Mounts(Records::read(&mut r, MOUNTS_MAX, MountEntry::read)?)
                    }
                },
                (0, _) => return Err(Errno::OutOfRange),
                (outcome, detail) => Self::Refused {
                    op,
                    error: NetshareError::read(outcome, detail, &mut r)?,
                },
            }
        };
        if !r.exhausted() {
            return Err(Errno::LengthOutOfRange);
        }
        Ok(reply)
    }
}

#[cfg(test)]
#[path = "netshare_ipc_tests.rs"]
mod tests;
