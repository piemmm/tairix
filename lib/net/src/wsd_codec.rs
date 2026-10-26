//! The WS-Discovery wire codec: SOAP envelopes read over `lib/xml` with every
//! name resolved by namespace, and the few messages this host sends written
//! from fixed text around values the grammar has already vetted.

use alloc::vec::Vec;
use core::fmt::{self, Write as _};

use tairix_hash::{siphash::SipHash13, HashSeed};
use tairix_inline::ArrayVec;
use tairix_xml::{Element, Scope};

use super::{AppSequence, Computer, Endpoint, MetadataPath, Uuid, WsdError, XAddr, MAX_METADATA};
use crate::http::{self, Authority, Cursor};

const SOAP: &str = "http://www.w3.org/2003/05/soap-envelope";
const WSA: &str = "http://schemas.xmlsoap.org/ws/2004/08/addressing";
const WSD: &str = "http://schemas.xmlsoap.org/ws/2005/04/discovery";
const WSDP: &str = "http://schemas.xmlsoap.org/ws/2006/02/devprof";
const WSX: &str = "http://schemas.xmlsoap.org/ws/2004/09/mex";
const PUB: &str = "http://schemas.microsoft.com/windows/pub/2005/07";

const ACTION_HELLO: &str = "http://schemas.xmlsoap.org/ws/2005/04/discovery/Hello";
const ACTION_BYE: &str = "http://schemas.xmlsoap.org/ws/2005/04/discovery/Bye";
const ACTION_PROBE: &str = "http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe";
const ACTION_PROBE_MATCHES: &str = "http://schemas.xmlsoap.org/ws/2005/04/discovery/ProbeMatches";
const ACTION_RESOLVE: &str = "http://schemas.xmlsoap.org/ws/2005/04/discovery/Resolve";
const ACTION_RESOLVE_MATCHES: &str =
    "http://schemas.xmlsoap.org/ws/2005/04/discovery/ResolveMatches";
const ACTION_GET: &str = "http://schemas.xmlsoap.org/ws/2004/09/transfer/Get";
const ACTION_GET_RESPONSE: &str = "http://schemas.xmlsoap.org/ws/2004/09/transfer/GetResponse";

/// Where every discovery message is addressed.
const DISCOVERY_TO: &str = "urn:schemas-xmlsoap-org:ws:2005:04:discovery";

/// The dialect of the metadata section naming a device's host.
const RELATIONSHIP: &str = "http://schemas.xmlsoap.org/ws/2006/02/devprof/Relationship";

/// The relationship type of a device's host.
const HOST: &str = "http://schemas.xmlsoap.org/ws/2006/02/devprof/host";

/// The most matches one `ProbeMatches` is read for.
pub const MAX_MATCHES: usize = 8;

/// The most `XAddrs` URLs read from one target.
pub const MAX_XADDRS: usize = 4;

/// The most qualified names read from one `Types` list.
const MAX_TYPES: usize = 16;

/// What one discovery message says.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    /// A target joining the network.
    Hello(Target),
    /// A target leaving it.
    Bye {
        /// Who is leaving.
        endpoint: Endpoint,
    },
    /// Targets answering a probe, at most [`MAX_MATCHES`].
    ProbeMatches(Vec<Target>),
    /// A target answering a resolve.
    ResolveMatch(Target),
    /// A probe or resolve another client sent, which nothing here answers.
    Query,
}

/// One target a message describes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target {
    /// The address it names itself by.
    pub endpoint: Endpoint,
    /// Whether its types include `pub:Computer`.
    pub computer: bool,
    /// The URLs it may be reached at, as many as were read.
    pub xaddrs: ArrayVec<XAddr, MAX_XADDRS>,
    /// Its metadata's version, which changes whenever the metadata does.
    pub metadata_version: u32,
}

/// One decoded envelope: its header's identity and ordering, and what its
/// body says.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Envelope {
    /// A keyed hash of the message's id, for telling a repeat from a fresh
    /// message.
    pub message_id: u64,
    /// The id of the message it answers, where it answers one this host
    /// could have sent.
    pub relates_to: Option<Uuid>,
    /// Where its sender's message sequence stands.
    pub app_sequence: Option<AppSequence>,
    /// What it says.
    pub message: Message,
}

/// Read one discovery datagram, keying its ids with `seed`.
///
/// # Errors
///
/// [`WsdError::Malformed`] for anything but a well-formed envelope carrying
/// what its action requires, and [`WsdError::Unsupported`] for an action this
/// host does not act on.
pub fn decode(datagram: &[u8], seed: HashSeed) -> Result<Envelope, WsdError> {
    let text = core::str::from_utf8(datagram).map_err(|_| WsdError::Malformed)?;
    let root = tairix_xml::parse(text, "").map_err(|_| WsdError::Malformed)?;
    let envelope = Scope::root(&root);
    let (header, body) = parts(&root)?;
    let header = header.ok_or(WsdError::Malformed)?;
    let action = only(header, WSA, "Action")?
        .ok_or(WsdError::Malformed)?
        .text();
    let message_id = only(header, WSA, "MessageID")?
        .ok_or(WsdError::Malformed)?
        .text();
    let relates_to = only(header, WSA, "RelatesTo")?.and_then(|e| Uuid::parse_urn(e.text().trim()));
    let app_sequence = only(header, WSD, "AppSequence")?
        .map(|element| app_sequence(element, seed))
        .transpose()?;
    let body_scope = envelope.enter(body);
    let message = match action.trim() {
        ACTION_HELLO => Message::Hello(target(&body_scope, required(body, WSD, "Hello")?)?),
        ACTION_BYE => Message::Bye {
            endpoint: endpoint(required(body, WSD, "Bye")?)?,
        },
        ACTION_PROBE_MATCHES => {
            let matches = required(body, WSD, "ProbeMatches")?;
            let scope = body_scope.enter(matches);
            let mut targets = Vec::new();
            for element in matches
                .children()
                .filter(|e| e.is(WSD, "ProbeMatch"))
                .take(MAX_MATCHES)
            {
                let target = target(&scope, element)?;
                if targets.try_reserve(1).is_err() {
                    break;
                }
                targets.push(target);
            }
            Message::ProbeMatches(targets)
        }
        ACTION_RESOLVE_MATCHES => {
            let matches = required(body, WSD, "ResolveMatches")?;
            let scope = body_scope.enter(matches);
            Message::ResolveMatch(target(&scope, required(matches, WSD, "ResolveMatch")?)?)
        }
        ACTION_PROBE | ACTION_RESOLVE => Message::Query,
        _ => return Err(WsdError::Unsupported),
    };
    Ok(Envelope {
        message_id: SipHash13::hash_bytes(seed, message_id.trim().as_bytes()),
        relates_to,
        app_sequence,
        message,
    })
}

/// A SOAP envelope's header, where it has one, and its body.
fn parts<'e, 'a>(
    root: &'e Element<'a>,
) -> Result<(Option<&'e Element<'a>>, &'e Element<'a>), WsdError> {
    if !root.is(SOAP, "Envelope") {
        return Err(WsdError::Malformed);
    }
    Ok((only(root, SOAP, "Header")?, required(root, SOAP, "Body")?))
}

/// `element`'s one child `local` in `namespace`, `None` when it has none,
/// refusing two: a message saying one thing twice says nothing reliably.
fn only<'e, 'a>(
    element: &'e Element<'a>,
    namespace: &str,
    local: &str,
) -> Result<Option<&'e Element<'a>>, WsdError> {
    let mut found = element
        .children()
        .filter(|child| child.is(namespace, local));
    let first = found.next();
    if found.next().is_some() {
        return Err(WsdError::Malformed);
    }
    Ok(first)
}

fn required<'e, 'a>(
    element: &'e Element<'a>,
    namespace: &str,
    local: &str,
) -> Result<&'e Element<'a>, WsdError> {
    only(element, namespace, local)?.ok_or(WsdError::Malformed)
}

fn endpoint(holder: &Element<'_>) -> Result<Endpoint, WsdError> {
    let reference = required(holder, WSA, "EndpointReference")?;
    Endpoint::new(required(reference, WSA, "Address")?.text().trim()).ok_or(WsdError::Malformed)
}

fn target(outer: &Scope<'_, '_>, element: &Element<'_>) -> Result<Target, WsdError> {
    let scope = outer.enter(element);
    let computer = only(element, WSD, "Types")?
        .is_some_and(|types| names_computer(&scope.enter(types), &types.text()));
    let mut xaddrs = ArrayVec::new();
    if let Some(listed) = only(element, WSD, "XAddrs")? {
        for url in listed.text().split_ascii_whitespace() {
            if let Some(xaddr) = XAddr::parse(url) {
                if xaddrs.try_push(xaddr).is_err() {
                    break;
                }
            }
        }
    }
    let version = required(element, WSD, "MetadataVersion")?.text();
    Ok(Target {
        endpoint: endpoint(element)?,
        computer,
        xaddrs,
        metadata_version: decimal(version.trim())?,
    })
}

/// Whether a `Types` list, its names resolved where it stands, names
/// `pub:Computer`.
fn names_computer(scope: &Scope<'_, '_>, list: &str) -> bool {
    list.split_ascii_whitespace()
        .take(MAX_TYPES)
        .any(|qname| scope.resolve(qname) == Some((Some(PUB), "Computer")))
}

fn app_sequence(element: &Element<'_>, seed: HashSeed) -> Result<AppSequence, WsdError> {
    let number = |name| {
        element
            .attr(name)
            .ok_or(WsdError::Malformed)
            .and_then(decimal)
    };
    Ok(AppSequence {
        instance: number("InstanceId")?,
        sequence: element.attr("SequenceId").map_or(0, |sequence| {
            SipHash13::hash_bytes(seed, sequence.as_bytes())
        }),
        number: number("MessageNumber")?,
    })
}

/// An `xs:unsignedInt` in its canonical decimal form.
fn decimal(text: &str) -> Result<u32, WsdError> {
    if text.is_empty() || text.len() > 10 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(WsdError::Malformed);
    }
    text.parse().map_err(|_| WsdError::Malformed)
}

/// Read a metadata response — the whole of what a target sent before it
/// closed the connection — for the computer its host relationship names.
///
/// # Errors
///
/// [`WsdError::Http`] when the exchange failed, and [`WsdError::Malformed`]
/// when the body is not a `GetResponse` naming a computer.
pub fn decode_metadata(response: &[u8]) -> Result<Computer, WsdError> {
    let body = http::read_response(response, MAX_METADATA).map_err(WsdError::Http)?;
    let text = core::str::from_utf8(&body).map_err(|_| WsdError::Malformed)?;
    let root = tairix_xml::parse(text, "").map_err(|_| WsdError::Malformed)?;
    let (header, body) = parts(&root)?;
    if let Some(header) = header {
        let action = required(header, WSA, "Action")?.text();
        if action.trim() != ACTION_GET_RESPONSE {
            return Err(WsdError::Malformed);
        }
    }
    let envelope = Scope::root(&root);
    let body_scope = envelope.enter(body);
    let metadata = required(body, WSX, "Metadata")?;
    let metadata_scope = body_scope.enter(metadata);
    for section in metadata.children().filter(|e| e.is(WSX, "MetadataSection")) {
        if section.attr("Dialect") != Some(RELATIONSHIP) {
            continue;
        }
        let section_scope = metadata_scope.enter(section);
        for relationship in section.children().filter(|e| e.is(WSDP, "Relationship")) {
            if relationship.attr("Type") != Some(HOST) {
                continue;
            }
            let relationship_scope = section_scope.enter(relationship);
            let host = required(relationship, WSDP, "Host")?;
            let host_scope = relationship_scope.enter(host);
            let is_computer = only(host, WSDP, "Types")?
                .is_some_and(|types| names_computer(&host_scope.enter(types), &types.text()));
            let named = only(host, PUB, "Computer")?;
            if let (true, Some(named)) = (is_computer, named) {
                return Computer::parse(named.text().trim()).ok_or(WsdError::Malformed);
            }
        }
    }
    Err(WsdError::Malformed)
}

/// The fixed text before a message's header fields.
const ENVELOPE_START: &str = concat!(
    r#"<?xml version="1.0" encoding="utf-8"?>"#,
    r#"<soap:Envelope xmlns:soap="http://www.w3.org/2003/05/soap-envelope""#,
    r#" xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing""#,
    r#" xmlns:wsd="http://schemas.xmlsoap.org/ws/2005/04/discovery""#,
    r#" xmlns:wsdp="http://schemas.xmlsoap.org/ws/2006/02/devprof">"#,
    "<soap:Header>",
);

/// Write a `Probe` for every DPWS device, identified by `id`, into `out` and
/// return its length.
///
/// The type is spelled `wsdp:Device`, since Samba's `wsdd` answers a probe
/// for no other spelling.
///
/// # Errors
///
/// [`WsdError::TooLarge`] when it does not fit `out`.
pub fn write_probe(out: &mut [u8], id: Uuid) -> Result<usize, WsdError> {
    write_discovery(out, ACTION_PROBE, id, |cursor| {
        cursor.write_str("<wsd:Probe><wsd:Types>wsdp:Device</wsd:Types></wsd:Probe>")
    })
}

/// Write a `Resolve` for `endpoint`, identified by `id`, into `out` and
/// return its length.
///
/// # Errors
///
/// [`WsdError::TooLarge`] when it does not fit `out`.
pub fn write_resolve(out: &mut [u8], id: Uuid, endpoint: &Endpoint) -> Result<usize, WsdError> {
    write_discovery(out, ACTION_RESOLVE, id, |cursor| {
        write!(
            cursor,
            "<wsd:Resolve><wsa:EndpointReference><wsa:Address>{}</wsa:Address>\
             </wsa:EndpointReference></wsd:Resolve>",
            endpoint.as_str()
        )
    })
}

fn write_discovery(
    out: &mut [u8],
    action: &str,
    id: Uuid,
    body: impl FnOnce(&mut Cursor<'_>) -> fmt::Result,
) -> Result<usize, WsdError> {
    let mut cursor = Cursor::new(out);
    write!(
        cursor,
        "{ENVELOPE_START}<wsa:To>{DISCOVERY_TO}</wsa:To><wsa:Action>{action}</wsa:Action>\
         <wsa:MessageID>{id}</wsa:MessageID></soap:Header><soap:Body>"
    )
    .and_then(|()| body(&mut cursor))
    .and_then(|()| cursor.write_str("</soap:Body></soap:Envelope>"))
    .map_err(|_| WsdError::TooLarge)?;
    Ok(cursor.len())
}

/// Write the HTTP request asking `endpoint`, by the URL with authority `at`
/// and `path`, for its metadata (a WS-Transfer `Get` identified by `id`) into
/// `out`, and return its length.
///
/// # Errors
///
/// [`WsdError::TooLarge`] when it does not fit `out`, and
/// [`WsdError::Malformed`] for a path outside the HTTP grammar.
pub fn write_metadata_request(
    out: &mut [u8],
    at: Authority<'_>,
    path: &MetadataPath,
    endpoint: &Endpoint,
    id: Uuid,
) -> Result<usize, WsdError> {
    let mut body = [0u8; 1024];
    let mut cursor = Cursor::new(&mut body);
    write!(
        cursor,
        "{ENVELOPE_START}<wsa:To>{}</wsa:To><wsa:Action>{ACTION_GET}</wsa:Action>\
         <wsa:MessageID>{id}</wsa:MessageID></soap:Header><soap:Body/></soap:Envelope>",
        endpoint.as_str()
    )
    .map_err(|_| WsdError::TooLarge)?;
    let len = cursor.len();
    http::write_post(out, at, path.as_str(), "application/soap+xml", &body[..len]).map_err(|err| {
        match err {
            http::HttpError::TooLarge => WsdError::TooLarge,
            _ => WsdError::Malformed,
        }
    })
}
