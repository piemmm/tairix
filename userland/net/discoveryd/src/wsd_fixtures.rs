//! WS-Discovery messages shaped as Windows sends them, for the unit tests.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_net::http::MAX_HOST_NAME;

/// A host name at its bound: labels at their longest, cut to the name's.
pub(crate) fn longest_host_name() -> String {
    let label = "n".repeat(63);
    let mut name = [label.as_str(); 4].join(".");
    name.truncate(MAX_HOST_NAME);
    name
}

/// A `Hello` from endpoint `urn:uuid:0001`, a computer whose metadata is at
/// `http://192.168.1.9:5357/meta`, at metadata `version`, message `number`.
pub(crate) fn hello(version: u32, number: u32) -> Vec<u8> {
    format!(
        r#"<soap:Envelope xmlns:soap="http://www.w3.org/2003/05/soap-envelope" xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:wsd="http://schemas.xmlsoap.org/ws/2005/04/discovery" xmlns:wsdp="http://schemas.xmlsoap.org/ws/2006/02/devprof" xmlns:pub="http://schemas.microsoft.com/windows/pub/2005/07"><soap:Header><wsa:To>urn:schemas-xmlsoap-org:ws:2005:04:discovery</wsa:To><wsa:Action>http://schemas.xmlsoap.org/ws/2005/04/discovery/Hello</wsa:Action><wsa:MessageID>urn:uuid:5a1f0b9e-0000-4000-8000-{number:012}</wsa:MessageID><wsd:AppSequence InstanceId="7" MessageNumber="{number}"/></soap:Header><soap:Body><wsd:Hello><wsa:EndpointReference><wsa:Address>urn:uuid:0001</wsa:Address></wsa:EndpointReference><wsd:Types>wsdp:Device pub:Computer</wsd:Types><wsd:XAddrs>http://192.168.1.9:5357/meta</wsd:XAddrs><wsd:MetadataVersion>{version}</wsd:MetadataVersion></wsd:Hello></soap:Body></soap:Envelope>"#
    )
    .into_bytes()
}

/// The response to a metadata request naming the computer `value`, as
/// `NAME/Workgroup:GROUP` or `NAME/Domain:DOMAIN`.
pub(crate) fn metadata(value: &str) -> Vec<u8> {
    let body = format!(
        r#"<soap:Envelope xmlns:soap="http://www.w3.org/2003/05/soap-envelope" xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:wsx="http://schemas.xmlsoap.org/ws/2004/09/mex" xmlns:wsdp="http://schemas.xmlsoap.org/ws/2006/02/devprof" xmlns:pub="http://schemas.microsoft.com/windows/pub/2005/07"><soap:Header><wsa:Action>http://schemas.xmlsoap.org/ws/2004/09/transfer/GetResponse</wsa:Action></soap:Header><soap:Body><wsx:Metadata><wsx:MetadataSection Dialect="http://schemas.xmlsoap.org/ws/2006/02/devprof/Relationship"><wsdp:Relationship Type="http://schemas.xmlsoap.org/ws/2006/02/devprof/host"><wsdp:Host><wsdp:Types>pub:Computer</wsdp:Types><pub:Computer>{value}</pub:Computer></wsdp:Host></wsdp:Relationship></wsx:MetadataSection></wsx:Metadata></soap:Body></soap:Envelope>"#
    );
    let mut response =
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
    response.extend_from_slice(body.as_bytes());
    response
}
