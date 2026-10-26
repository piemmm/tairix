//! Unit tests for the HTTP/1.1 exchange: the request as written, and the
//! response reader's framings and refusals.

use alloc::vec::Vec;

use super::{
    read_response, write_post, Authority, Host, HostName, HttpError, MAX_FIELDS, MAX_HOST_NAME,
    MAX_LINE,
};
use crate::addr::{IpAddr, Ipv4Addr, Ipv6Addr};

const MAX: usize = 1024;

fn at(host: &Host, port: u16) -> Authority<'_> {
    Authority { host, port }
}

#[test]
fn a_post_is_written_whole_and_asks_the_server_to_close() {
    let mut out = [0u8; 512];
    let v4 = Host::Addr(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 7)));
    let len = write_post(
        &mut out,
        at(&v4, 5357),
        "/a/b-c",
        "application/soap+xml",
        b"<x/>",
    )
    .expect("fits");
    assert_eq!(
        &out[..len],
        b"POST /a/b-c HTTP/1.1\r\nHost: 192.168.1.7:5357\r\n\
          Content-Type: application/soap+xml\r\nContent-Length: 4\r\n\
          Connection: close\r\n\r\n<x/>"
            .as_slice()
    );
    let v6 = Host::Addr(IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)));
    let len = write_post(&mut out, at(&v6, 80), "/", "text/xml", b"").expect("fits");
    assert!(out[..len].starts_with(b"POST / HTTP/1.1\r\nHost: [fe80::1]\r\n"));
}

#[test]
fn a_host_field_names_the_url_by_its_name_as_written() {
    let mut out = [0u8; 512];
    let named = Host::Name(HostName::new("Office-NAS").expect("a name"));
    let len = write_post(&mut out, at(&named, 8080), "/wsd/x", "text/xml", b"").expect("fits");
    assert!(out[..len].starts_with(b"POST /wsd/x HTTP/1.1\r\nHost: Office-NAS:8080\r\n"));
    let len = write_post(&mut out, at(&named, 80), "/", "text/xml", b"").expect("fits");
    assert!(out[..len].starts_with(b"POST / HTTP/1.1\r\nHost: Office-NAS\r\n"));
}

#[test]
fn a_host_name_is_unreserved_labels_never_an_address() {
    for good in ["OFFICE", "nas.example", "a_b~c-d", "x1.y2", "1x.2y"] {
        assert_eq!(
            HostName::new(good).as_ref().map(HostName::as_str),
            Some(good),
            "{good}"
        );
    }
    for bad in [
        "",
        ".",
        "a..b",
        "trailing.",
        ".leading",
        "has space",
        "a:80",
        "a/b",
        "a@b",
        "a%20b",
        "[::1]",
        "a\r\nb",
        "caf\u{e9}",
        "192.168.001.20",
        "1.2.3",
        "4294967295",
        "nas.0x7f.1",
    ] {
        assert_eq!(HostName::new(bad), None, "{bad:?}");
    }
    assert_eq!(
        HostName::new("OFFICE"),
        HostName::new("office"),
        "a host compares without case"
    );
    assert_ne!(HostName::new("OFFICE"), HostName::new("OFFICES"));
    let label = "a".repeat(63);
    assert!(HostName::new(&label).is_some());
    assert!(HostName::new(&"a".repeat(64)).is_none(), "a label past 63");
    let longest = [label.as_str(); 4].join(".");
    assert_eq!(longest.len(), 255);
    assert!(HostName::new(&longest[..MAX_HOST_NAME]).is_some());
    assert!(HostName::new(&longest).is_none(), "a name past 253");
}

#[test]
fn a_post_refuses_what_could_end_a_line_or_does_not_fit() {
    let mut out = [0u8; 512];
    let localhost = Host::Addr(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let to = at(&localhost, 80);
    for target in ["relative", "/a b", "/a\r\nX: y", "", "/caf\u{e9}"] {
        assert_eq!(
            write_post(&mut out, to, target, "text/xml", b""),
            Err(HttpError::Malformed),
            "{target:?}"
        );
    }
    assert_eq!(
        write_post(&mut out, to, "/", "text/xml\r\n", b""),
        Err(HttpError::Malformed)
    );
    assert_eq!(
        write_post(&mut out[..40], to, "/", "text/xml", b""),
        Err(HttpError::TooLarge)
    );
}

#[test]
fn a_body_is_framed_by_its_length_its_chunks_or_the_close() {
    let length = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nServer: x\r\n\r\nhello";
    assert_eq!(
        read_response(length, MAX).as_deref(),
        Ok(b"hello".as_slice())
    );
    let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: Chunked\r\n\r\n\
                    3;name=v\r\nhel\r\n2\r\nlo\r\n0\r\nTrailer: t\r\n\r\n";
    assert_eq!(
        read_response(chunked, MAX).as_deref(),
        Ok(b"hello".as_slice())
    );
    let closed = b"HTTP/1.0 200 Fine\r\n\r\nhello";
    assert_eq!(
        read_response(closed, MAX).as_deref(),
        Ok(b"hello".as_slice())
    );
    let interim = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
    assert_eq!(read_response(interim, MAX).as_deref(), Ok(b"ok".as_slice()));
}

#[test]
fn a_status_other_than_ok_is_an_answer_not_a_body() {
    assert_eq!(
        read_response(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n", MAX),
        Err(HttpError::Status(404))
    );
    assert_eq!(
        read_response(b"HTTP/1.1 500 \r\n\r\n", MAX),
        Err(HttpError::Status(500))
    );
}

#[test]
fn smuggling_and_splitting_shapes_are_refused() {
    for response in [
        &b"HTTP/1.1 200 OK\nContent-Length: 2\n\nok"[..],
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nTransfer-Encoding: chunked\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\nok",
        b"HTTP/1.1 200 OK\r\nContent-Length: 2, 2\r\n\r\nok",
        b"HTTP/1.1 200 OK\r\nContent-Length : 2\r\n\r\nok",
        b"HTTP/1.1 200 OK\r\nX: a\r\n folded\r\nContent-Length: 2\r\n\r\nok",
        b"HTTP/1.1 200 OK\r\nX: a\rb\r\nContent-Length: 2\r\n\r\nok",
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nokextra",
        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nok",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\nmore",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nok\r\n0\r\n\r\n",
        b"HTTP/2 200 OK\r\n\r\n",
        b"HTTP/1.1 2000 OK\r\n\r\n",
        b"HTTP/1.1 099 Low\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: 2",
    ] {
        assert_eq!(
            read_response(response, MAX),
            Err(HttpError::Malformed),
            "{:?}",
            core::str::from_utf8(response)
        );
    }
    assert_eq!(
        read_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n", MAX),
        Err(HttpError::Unsupported)
    );
}

#[test]
fn every_bound_holds() {
    let long = b"HTTP/1.1 200 OK\r\nContent-Length: 2000\r\n\r\n";
    let mut body = Vec::from(long.as_slice());
    body.resize(long.len() + 2000, b'x');
    assert_eq!(read_response(&body, MAX), Err(HttpError::TooLarge));
    let chunks = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n401\r\n";
    let mut chunked = Vec::from(chunks.as_slice());
    chunked.resize(chunks.len() + 0x401, b'x');
    chunked.extend_from_slice(b"\r\n0\r\n\r\n");
    assert_eq!(read_response(&chunked, MAX), Err(HttpError::TooLarge));
    let mut fields = Vec::from(b"HTTP/1.1 200 OK\r\n".as_slice());
    for i in 0..=MAX_FIELDS {
        fields.extend_from_slice(alloc::format!("X-{i}: v\r\n").as_bytes());
    }
    fields.extend_from_slice(b"\r\n");
    assert_eq!(read_response(&fields, MAX), Err(HttpError::TooLarge));
    let mut line = Vec::from(b"HTTP/1.1 200 OK\r\nX: ".as_slice());
    line.resize(line.len() + MAX_LINE, b'v');
    line.extend_from_slice(b"\r\n\r\n");
    assert_eq!(read_response(&line, MAX), Err(HttpError::TooLarge));
}
