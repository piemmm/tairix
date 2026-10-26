//! WS-Discovery, as Windows computers and Samba's `wsdd` announce themselves:
//! the April 2005 draft over SOAP-over-UDP with WS-Addressing 2004/08, DPWS
//! 2006/02 and WS-Transfer, and Microsoft's `pub:Computer` metadata naming a
//! computer and its workgroup or domain (`plans/ZEROCONF.md` Z10).
//!
//! The codec reads `Hello`, `Bye`, `ProbeMatches` and `ResolveMatches`
//! envelopes and the WS-Transfer `GetResponse` over a strict HTTP/1.1
//! exchange, and writes the `Probe`, `Resolve` and metadata `Get` this host
//! sends; the engine is the per-link state machine that drives them. Nothing
//! here answers a probe: this host is never a target. Pure and bounded — no
//! socket, no clock, no randomness of its own.

use core::fmt;

use tairix_inline::ArrayString;

use crate::addr::{IpAddr, Ipv4Addr, Ipv6Addr};
use crate::http::{Host, HostName};

#[path = "wsd_codec.rs"]
mod codec;

#[path = "wsd_engine.rs"]
mod engine;

#[cfg(any(test, feature = "test-support"))]
#[path = "wsd_vectors.rs"]
pub mod vectors;

pub use codec::{
    decode, decode_metadata, write_metadata_request, write_probe, write_resolve, Message, Target,
    MAX_MATCHES,
};
pub use engine::{
    Change, Destination, Emit, Fetch, Output, WsdEngine, MAX_FETCHES, MAX_TARGETS,
    MAX_TARGETS_PER_SOURCE,
};

/// The UDP port every WS-Discovery message is sent from and to.
pub const PORT: u16 = 3702;

/// The IPv4 multicast group WS-Discovery runs on.
pub const GROUP_V4: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);

/// The IPv6 link-local multicast group WS-Discovery runs on.
pub const GROUP_V6: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 0x000c);

/// The most a metadata response may hold, its HTTP head included.
///
/// A fixed bound: a computer's metadata is a few hundred bytes of names.
pub const MAX_METADATA: usize = 16 * 1024;

/// The longest endpoint address read or written.
pub const MAX_ENDPOINT: usize = 255;

/// The longest metadata path read from an `XAddrs` URL.
pub const MAX_PATH: usize = 255;

/// The longest computer name a `pub:Computer` value may carry: the longest a
/// discovery answer can.
pub const MAX_COMPUTER_NAME: usize = tairix_abi::discovery_ipc::COMPUTER_NAME_MAX;

/// The longest workgroup or domain name a `pub:Computer` value may carry: the
/// longest a discovery answer can.
pub const MAX_MEMBERSHIP_NAME: usize = tairix_abi::discovery_ipc::MEMBERSHIP_NAME_MAX;

/// Why a message or response was not read.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WsdError {
    /// Not a SOAP envelope, or one breaking WS-Discovery's grammar.
    Malformed,
    /// A message this host does not act on.
    Unsupported,
    /// A metadata response whose HTTP exchange failed.
    Http(crate::http::HttpError),
    /// A message would not fit the buffer it was to be written into.
    TooLarge,
}

/// A message id this host minted: a random UUID, spelled `urn:uuid:…`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Uuid([u8; 16]);

impl Uuid {
    /// A version 4 UUID from `random`.
    #[must_use]
    pub const fn from_random(mut random: [u8; 16]) -> Self {
        random[6] = (random[6] & 0x0f) | 0x40;
        random[8] = (random[8] & 0x3f) | 0x80;
        Self(random)
    }

    /// A version 4 UUID drawn from `rng`.
    pub fn draw(rng: &mut dyn FnMut() -> u32) -> Self {
        let mut random = [0u8; 16];
        for word in random.as_chunks_mut::<4>().0 {
            *word = rng().to_le_bytes();
        }
        Self::from_random(random)
    }

    /// The UUID `urn` spells, any case, or `None` for anything else.
    #[must_use]
    pub fn parse_urn(urn: &str) -> Option<Self> {
        let hex = urn.strip_prefix("urn:uuid:")?.as_bytes();
        if hex.len() != 36 || [8, 13, 18, 23].iter().any(|&at| hex[at] != b'-') {
            return None;
        }
        let mut bytes = [0u8; 16];
        let mut digits = hex.iter().filter(|&&byte| byte != b'-');
        for byte in &mut bytes {
            let high = nibble(*digits.next()?)?;
            let low = nibble(*digits.next()?)?;
            *byte = (high << 4) | low;
        }
        Some(Self(bytes))
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("urn:uuid:")?;
        for (at, byte) in self.0.iter().enumerate() {
            if matches!(at, 4 | 6 | 8 | 10) {
                f.write_str("-")?;
            }
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

const fn nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

/// The address a target names itself by (`wsa:EndpointReference/wsa:Address`).
///
/// Restricted to the characters of a URI that need no escaping in XML, so it
/// can be written back into a message verbatim.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Endpoint(ArrayString<MAX_ENDPOINT>);

impl Endpoint {
    /// `text` as an endpoint address, or `None` when it is empty, longer
    /// than [`MAX_ENDPOINT`], or holds a character outside the grammar.
    #[must_use]
    pub fn new(text: &str) -> Option<Self> {
        if text.is_empty() || !text.bytes().all(is_endpoint_byte) {
            return None;
        }
        ArrayString::try_from(text).ok().map(Self)
    }

    /// The address as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Whether `byte` may appear in an [`Endpoint`]: RFC 3986's unreserved and
/// reserved characters and `%`, less the `&` and `'` XML would need escaped.
const fn is_endpoint_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b':'
                | b'/'
                | b'?'
                | b'#'
                | b'['
                | b']'
                | b'@'
                | b'!'
                | b'$'
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b'%'
        )
}

/// The path a metadata request is sent to: `/`, then visible ASCII with no
/// fragment, at most [`MAX_PATH`] bytes, so it can neither end a line of the
/// request nor name another resource than its URL did.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct MetadataPath(ArrayString<MAX_PATH>);

impl MetadataPath {
    /// `text` as a metadata path, or `None` outside the grammar.
    #[must_use]
    pub fn new(text: &str) -> Option<Self> {
        let well_formed = text.starts_with('/')
            && !text.contains('#')
            && text.bytes().all(|byte| byte.is_ascii_graphic());
        if !well_formed {
            return None;
        }
        ArrayString::try_from(text).ok().map(Self)
    }

    /// The path as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// One `XAddrs` URL a target may be reached at: `http`, a host, a port, and
/// a path. A name is never resolved for a peer: a URL naming one is fetched
/// at the address the message carrying it came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XAddr {
    /// What the URL names its host by.
    pub host: Host,
    /// The port, 80 when the URL names none.
    pub port: u16,
    /// The path the metadata request is sent to.
    pub path: MetadataPath,
}

impl XAddr {
    /// `url` read, or `None` when it is not `http`, or carries a host, port
    /// or path outside the grammar.
    #[must_use]
    pub fn parse(url: &str) -> Option<Self> {
        let scheme_end = url.find("://")?;
        if !url[..scheme_end].eq_ignore_ascii_case("http") {
            return None;
        }
        let rest = &url[scheme_end + 3..];
        let path_at = rest.find('/')?;
        let (authority, path) = rest.split_at(path_at);
        let (host, port) = authority_parts(authority)?;
        Some(Self {
            host,
            port,
            path: MetadataPath::new(path)?,
        })
    }
}

/// An `XAddrs` authority's host and port.
fn authority_parts(authority: &str) -> Option<(Host, u16)> {
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let close = bracketed.find(']')?;
        let literal = &bracketed[..close];
        // A zone names the sender's link, which the fetch is pinned to anyway.
        let literal = literal.split_once("%25").map_or(literal, |(addr, _)| addr);
        let addr = literal.parse::<Ipv6Addr>().ok()?;
        (Host::Addr(IpAddr::V6(addr)), &bracketed[close + 1..])
    } else {
        let colon = authority.find(':').unwrap_or(authority.len());
        let named = &authority[..colon];
        let host = match dotted_quad(named) {
            Some(addr) => Host::Addr(IpAddr::V4(addr)),
            None => Host::Name(HostName::new(named)?),
        };
        (host, &authority[colon..])
    };
    let port = match port.strip_prefix(':') {
        None if port.is_empty() => 80,
        Some(digits)
            if !digits.is_empty()
                && digits.len() <= 5
                && digits.bytes().all(|b| b.is_ascii_digit()) =>
        {
            digits.parse::<u16>().ok().filter(|&port| port != 0)?
        }
        _ => return None,
    };
    Some((host, port))
}

/// An IPv4 address in RFC 3986 dotted-decimal: four octets, no leading zero.
fn dotted_quad(text: &str) -> Option<Ipv4Addr> {
    let mut octets = [0u8; 4];
    let mut parts = text.split('.');
    for octet in &mut octets {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 3 || (part.len() > 1 && part.starts_with('0')) {
            return None;
        }
        if !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        *octet = part.parse().ok()?;
    }
    parts.next().is_none().then(|| Ipv4Addr::from(octets))
}

/// Where a target's message sequence stands (`wsd:AppSequence`), so a stale
/// or replayed message is told from a fresh one.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AppSequence {
    /// Incremented each time the target restarts.
    pub instance: u32,
    /// A keyed hash of the sequence a message belongs to, `0` for none.
    pub sequence: u64,
    /// The message's place in its sequence.
    pub number: u32,
}

impl AppSequence {
    /// Whether a message at `self` is newer than one at `last`: a later
    /// instance, or the same instance and sequence with a later number.
    #[must_use]
    pub const fn follows(self, last: Self) -> bool {
        self.instance > last.instance
            || (self.instance == last.instance
                && (self.sequence != last.sequence || self.number > last.number))
    }
}

/// What a computer's metadata says it is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Computer {
    /// The computer's name.
    pub name: ArrayString<MAX_COMPUTER_NAME>,
    /// The workgroup or domain it belongs to.
    pub membership: Membership,
}

/// What a computer belongs to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Membership {
    /// A workgroup, by name.
    Workgroup(ArrayString<MAX_MEMBERSHIP_NAME>),
    /// A domain, by name.
    Domain(ArrayString<MAX_MEMBERSHIP_NAME>),
    /// Neither, or a value this host does not read.
    Unstated,
}

impl Computer {
    /// A `pub:Computer` value — `NAME/Workgroup:GROUP`, `NAME/Domain:DOMAIN`,
    /// or `NAME` alone — or `None` for an empty or over-long name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let (name, belongs) = value.split_once('/').unwrap_or((value, ""));
        if name.is_empty() {
            return None;
        }
        let named = |text: &str| {
            (!text.is_empty())
                .then(|| ArrayString::try_from(text).ok())
                .flatten()
        };
        let membership = if let Some(group) = belongs.strip_prefix("Workgroup:") {
            Membership::Workgroup(named(group)?)
        } else if let Some(domain) = belongs.strip_prefix("Domain:") {
            Membership::Domain(named(domain)?)
        } else {
            Membership::Unstated
        };
        Some(Self {
            name: ArrayString::try_from(name).ok()?,
            membership,
        })
    }
}

#[cfg(test)]
#[path = "wsd_tests.rs"]
mod tests;
