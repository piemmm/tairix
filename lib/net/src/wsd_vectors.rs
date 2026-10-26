//! Peers' WS-Discovery traffic for tests, and the probe id an answer to this
//! host relates to. A QNAP NAS (TS-869 Pro, QTS 4.3.4) answering this host's
//! probe and metadata request, captured on a LAN with its identifiers
//! replaced: a `ProbeMatches` from an ephemeral port naming its `XAddrs` host
//! by name and numbering every answer as a fresh instance, and a chunked
//! `GetResponse` kept byte for byte.

use alloc::string::String;

use tairix_hash::HashSeed;

use super::{decode, Message, Uuid};

/// The id of the probe `datagram` is, which an answer to it relates to, or
/// `None` for any other datagram.
#[must_use]
pub fn probe_id(datagram: &[u8]) -> Option<Uuid> {
    let text = core::str::from_utf8(datagram).ok()?;
    let is_probe = decode(datagram, HashSeed::UNKEYED).is_ok_and(|e| e.message == Message::Query)
        && text.contains("wsd:Probe>");
    if !is_probe {
        return None;
    }
    let start = text.find("<wsa:MessageID>")? + "<wsa:MessageID>".len();
    let end = text[start..].find('<')? + start;
    Uuid::parse_urn(&text[start..end])
}

/// Its endpoint, spelled as a bare UUID.
pub const QNAP_ENDPOINT: &str = "5d0c4a2e-7f31-4b8a-9c6d-2e1f0a3b4c5d";

/// Its name: its `XAddrs` URL's host, and the computer its metadata names.
pub const QNAP_NAME: &str = "OFFICE";

/// The workgroup its metadata names.
pub const QNAP_WORKGROUP: &str = "WORKGROUP";

/// The port its `XAddrs` URL names.
pub const QNAP_PORT: u16 = 8080;

/// The path its `XAddrs` URL names.
pub const QNAP_PATH: &str = "/wsd/5d0c4a2e-7f31-4b8a-9c6d-2e1f0a3b4c5d";

/// The port its `ProbeMatches` came from.
pub const QNAP_SOURCE_PORT: u16 = 36010;

/// Its `ProbeMatches`, as message `message` answering the probe
/// `relates_to`, under the fresh `instance` it gives each answer.
#[must_use]
pub fn qnap_probe_matches(message: Uuid, relates_to: Uuid, instance: u32) -> String {
    alloc::format!(
        concat!(
            "<?xml version=\"1.0\" ?>\n",
            "<soap:Envelope",
            " xmlns:pub=\"http://schemas.microsoft.com/windows/pub/2005/07\"",
            " xmlns:soap=\"http://www.w3.org/2003/05/soap-envelope\"",
            " xmlns:wsa=\"http://schemas.xmlsoap.org/ws/2004/08/addressing\"",
            " xmlns:wsd=\"http://schemas.xmlsoap.org/ws/2005/04/discovery\"",
            " xmlns:wsdp=\"http://schemas.xmlsoap.org/ws/2006/02/devprof\">\n",
            "\t<soap:Header>\n",
            "\t\t<wsa:Action>http://schemas.xmlsoap.org/ws/2005/04/discovery/ProbeMatches",
            "</wsa:Action>\n",
            "\t\t<wsa:MessageID>{message}</wsa:MessageID>\n",
            "\t\t<wsa:RelatesTo>{relates_to}</wsa:RelatesTo>\n",
            "\t\t<wsa:To>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous",
            "</wsa:To>\n",
            "\t\t<wsd:AppSequence InstanceId=\"{instance}\" MessageNumber=\"1\"/>\n",
            "\t</soap:Header>\n",
            "\t<soap:Body>\n",
            "\t\t<wsd:ProbeMatches>\n",
            "\t\t\t<wsd:ProbeMatch>\n",
            "\t\t\t\t<wsa:EndpointReference>\n",
            "\t\t\t\t\t<wsa:Address>{endpoint}\n",
            "</wsa:Address>\n",
            "\t\t\t\t</wsa:EndpointReference>\n",
            "\t\t\t\t<wsd:Types>wsdp:Device pub:Computer</wsd:Types>\n",
            "\t\t\t\t<wsd:XAddrs>http://{name}:{port}{path}\n",
            "</wsd:XAddrs>\n",
            "\t\t\t\t<wsd:MetadataVersion>1</wsd:MetadataVersion>\n",
            "\t\t\t</wsd:ProbeMatch>\n",
            "\t\t</wsd:ProbeMatches>\n",
            "\t</soap:Body>\n",
            "</soap:Envelope>\n",
        ),
        message = message,
        relates_to = relates_to,
        instance = instance,
        endpoint = QNAP_ENDPOINT,
        name = QNAP_NAME,
        port = QNAP_PORT,
        path = QNAP_PATH,
    )
}

/// The whole of its response to a metadata request, up to its close.
pub const QNAP_METADATA: &[u8] = concat!(
    "HTTP/1.1 200 OK\r\n",
    "Date: Sun, 25 Oct 2026 12:36:39 GMT\r\n",
    "Server: Apache\r\n",
    "X-Frame-Options: SAMEORIGIN\r\n",
    "Content-type: application/soap+xml\r\n",
    "Connection: close\r\n",
    "Transfer-Encoding: chunked\r\n",
    "\r\n",
    "819\r\n",
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
    "<soap:Envelope xmlns:soap=\"http://www.w3.org/2003/05/soap-envelope\"",
    " xmlns:pub=\"http://schemas.microsoft.com/windows/pub/2005/07\"",
    " xmlns:un0=\"http://schemas.microsoft.com/windows/pnpx/2005/10\"",
    " xmlns:wsa=\"http://schemas.xmlsoap.org/ws/2004/08/addressing\"",
    " xmlns:wsd=\"http://schemas.xmlsoap.org/ws/2005/04/discovery\"",
    " xmlns:wsdp=\"http://schemas.xmlsoap.org/ws/2006/02/devprof\"",
    " xmlns:wsx=\"http://schemas.xmlsoap.org/ws/2004/09/mex\">",
    "<soap:Header><wsa:To>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous",
    "</wsa:To><wsa:Action>http://schemas.xmlsoap.org/ws/2004/09/transfer/GetResponse",
    "</wsa:Action><wsa:MessageID>urn:uuid:0b0b0b0b-0b0b-4b0b-8b0b-0b0b0b0b0b0b",
    "</wsa:MessageID><wsa:RelatesTo>urn:uuid:0a0a0a0a-0a0a-4a0a-8a0a-0a0a0a0a0a0a",
    "</wsa:RelatesTo></soap:Header><soap:Body><wsx:Metadata><wsx:MetadataSection",
    " Dialect=\"http://schemas.xmlsoap.org/ws/2006/02/devprof/ThisDevice\">",
    "<wsdp:ThisDevice><wsdp:FriendlyName>QNAP",
    "</wsdp:FriendlyName></wsdp:ThisDevice>",
    "</wsx:MetadataSection><wsx:MetadataSection",
    " Dialect=\"http://schemas.xmlsoap.org/ws/2006/02/devprof/ThisModel\">",
    "<wsdp:ThisModel><wsdp:Manufacturer>QNAP",
    "</wsdp:Manufacturer><wsdp:ManufacturerUrl>http://www.qnap.com",
    "</wsdp:ManufacturerUrl><wsdp:ModelName>QNAP",
    "</wsdp:ModelName><wsdp:ModelNumber>NAS",
    "</wsdp:ModelNumber><wsdp:ModelUrl>http://www.qnap.com",
    "</wsdp:ModelUrl><wsdp:PresentationUrl>http://www.qnap.com",
    "</wsdp:PresentationUrl><un0:DeviceCategory>Computers",
    "</un0:DeviceCategory></wsdp:ThisModel>",
    "</wsx:MetadataSection><wsx:MetadataSection",
    " Dialect=\"http://schemas.xmlsoap.org/ws/2006/02/devprof/Relationship\">",
    "<wsdp:Relationship Type=\"http://schemas.xmlsoap.org/ws/2006/02/devprof/host\">",
    "<wsdp:Host><wsa:EndpointReference>",
    "<wsa:Address>urn:uuid:5d0c4a2e-7f31-4b8a-9c6d-2e1f0a3b4c5d",
    "</wsa:Address></wsa:EndpointReference><wsdp:Types>pub:Computer</wsdp:Types>",
    "<wsdp:ServiceId>urn:uuid:5d0c4a2e-7f31-4b8a-9c6d-2e1f0a3b4c5d",
    "</wsdp:ServiceId><pub:Computer>OFFICE/Workgroup:WORKGROUP",
    "</pub:Computer></wsdp:Host></wsdp:Relationship>",
    "</wsx:MetadataSection></wsx:Metadata></soap:Body></soap:Envelope>\n",
    "\r\n0\r\n\r\n",
)
.as_bytes();
