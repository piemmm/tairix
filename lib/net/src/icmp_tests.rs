//! Unit tests for the shared ICMP/ICMPv6 machinery.

use super::*;

const V6_SRC: Ipv6Addr = Ipv6Addr::new(0xFE80, 0, 0, 0, 0, 0, 0, 1);
const V6_DST: Ipv6Addr = Ipv6Addr::new(0xFE80, 0, 0, 0, 0, 0, 0, 2);

fn v6() -> IcmpContext {
    IcmpContext::V6 {
        source: V6_SRC,
        destination: V6_DST,
    }
}

fn contexts() -> [IcmpContext; 2] {
    [IcmpContext::V4, v6()]
}

#[test]
fn echo_round_trips_in_both_families() {
    for context in contexts() {
        let payload = [0xAB, 0xCD, 0xEF];
        let echo = IcmpEcho {
            kind: EchoKind::Request,
            identifier: 0x1234,
            sequence: 0x0001,
            payload: &payload,
        };
        let mut out = [0u8; 64];
        let len = echo.write(context, &mut out).expect("fits");
        let parsed = IcmpEcho::parse(context, &out[..len]).expect("parses");
        assert_eq!(parsed, echo);
    }
}

#[test]
fn echo_types_are_family_specific() {
    let echo = IcmpEcho {
        kind: EchoKind::Request,
        identifier: 1,
        sequence: 2,
        payload: &[],
    };
    let mut out = [0u8; 16];
    echo.write(IcmpContext::V4, &mut out).expect("fits");
    assert_eq!(out[0], TYPE_ECHO_REQUEST);
    // Bytes checksummed for one family never verify under the other.
    assert!(IcmpEcho::parse(v6(), &out[..8]).is_none());
    let len = echo.write(v6(), &mut out).expect("fits");
    assert_eq!(out[0], TYPE_V6_ECHO_REQUEST);
    assert!(IcmpEcho::parse(IcmpContext::V4, &out[..len]).is_none());
}

#[test]
fn echo_parse_rejects_corruption_and_wrong_shapes() {
    for context in contexts() {
        let echo = IcmpEcho {
            kind: EchoKind::Request,
            identifier: 7,
            sequence: 9,
            payload: &[1, 2, 3],
        };
        let mut out = [0u8; 16];
        let len = echo.write(context, &mut out).expect("fits");
        let mut corrupt = out;
        corrupt[4] ^= 0x01;
        assert!(IcmpEcho::parse(context, &corrupt[..len]).is_none());
        // Non-zero code.
        let mut coded = out;
        coded[1] = 1;
        assert!(IcmpEcho::parse(context, &coded[..len]).is_none());
        // Truncated.
        assert!(IcmpEcho::parse(context, &out[..ICMP_HEADER_LEN - 1]).is_none());
    }
}

#[test]
fn echo_reply_preserves_identity() {
    let request = IcmpEcho {
        kind: EchoKind::Request,
        identifier: 5,
        sequence: 6,
        payload: &[9, 8, 7],
    };
    let reply = request.reply();
    assert_eq!(reply.kind, EchoKind::Reply);
    assert_eq!(reply.identifier, request.identifier);
    assert_eq!(reply.sequence, request.sequence);
    assert_eq!(reply.payload, request.payload);
}

#[test]
fn error_round_trips_in_both_families() {
    let invoking = [0x45u8; 32];
    let kinds = [
        IcmpErrorKind::DestinationUnreachable { code: 3 },
        IcmpErrorKind::TimeExceeded { code: 1 },
        IcmpErrorKind::PacketTooBig { mtu: 1400 },
        IcmpErrorKind::ParameterProblem {
            code: 0,
            pointer: 40,
        },
    ];
    for context in contexts() {
        for kind in kinds {
            let error = IcmpError {
                kind,
                invoking: &invoking,
            };
            let mut out = [0u8; 64];
            let len = error.write(context, &mut out).expect("fits");
            let parsed = IcmpError::parse(context, &out[..len]).expect("parses");
            assert_eq!(parsed.kind, kind);
            assert_eq!(parsed.invoking, &invoking);
        }
    }
}

#[test]
fn v4_packet_too_big_uses_frag_needed_wire_form() {
    let error = IcmpError {
        kind: IcmpErrorKind::PacketTooBig { mtu: 1400 },
        invoking: &[0u8; 8],
    };
    let mut out = [0u8; 32];
    error.write(IcmpContext::V4, &mut out).expect("fits");
    assert_eq!(out[0], TYPE_DEST_UNREACHABLE);
    assert_eq!(out[1], 4);
    assert_eq!(u16::from_be_bytes([out[6], out[7]]), 1400);
}

#[test]
fn error_write_rejects_unrepresentable_v4_fields() {
    let mut out = [0u8; 64];
    let wide_pointer = IcmpError {
        kind: IcmpErrorKind::ParameterProblem {
            code: 0,
            pointer: 256,
        },
        invoking: &[0u8; 8],
    };
    assert!(wide_pointer.write(IcmpContext::V4, &mut out).is_none());
    assert!(wide_pointer.write(v6(), &mut out).is_some());
    let wide_mtu = IcmpError {
        kind: IcmpErrorKind::PacketTooBig { mtu: 70_000 },
        invoking: &[0u8; 8],
    };
    assert!(wide_mtu.write(IcmpContext::V4, &mut out).is_none());
    assert!(wide_mtu.write(v6(), &mut out).is_some());
}

#[test]
fn about_truncates_to_the_family_excerpt_bound() {
    let big = [0u8; 2048];
    let v4 = IcmpError::about(IcmpErrorKind::TimeExceeded { code: 0 }, &big, false);
    assert_eq!(v4.invoking.len(), MAX_ERROR_EXCERPT_V4);
    let v6e = IcmpError::about(IcmpErrorKind::TimeExceeded { code: 0 }, &big, true);
    assert_eq!(v6e.invoking.len(), MAX_ERROR_EXCERPT_V6);
    let small = [0u8; 12];
    let whole = IcmpError::about(IcmpErrorKind::TimeExceeded { code: 0 }, &small, true);
    assert_eq!(whole.invoking.len(), 12);
}

#[test]
fn error_parse_rejects_echo_types() {
    for context in contexts() {
        let echo = IcmpEcho {
            kind: EchoKind::Request,
            identifier: 1,
            sequence: 1,
            payload: &[],
        };
        let mut out = [0u8; 16];
        let len = echo.write(context, &mut out).expect("fits");
        assert!(IcmpError::parse(context, &out[..len]).is_none());
    }
}

fn allowed_base() -> ErrorContext {
    ErrorContext {
        invoking_is_icmp_error: false,
        dest_is_multicast: false,
        source_is_ambiguous: false,
        multicast_exception: false,
    }
}

#[test]
fn error_allowed_applies_rfc_4443_rules() {
    assert!(error_allowed(allowed_base()));
    assert!(!error_allowed(ErrorContext {
        invoking_is_icmp_error: true,
        ..allowed_base()
    }));
    assert!(!error_allowed(ErrorContext {
        source_is_ambiguous: true,
        ..allowed_base()
    }));
    assert!(!error_allowed(ErrorContext {
        dest_is_multicast: true,
        ..allowed_base()
    }));
    // The multicast exceptions (Packet Too Big, Parameter Problem
    // code 2 for a report-demanding option) stay allowed.
    assert!(error_allowed(ErrorContext {
        dest_is_multicast: true,
        multicast_exception: true,
        ..allowed_base()
    }));
    // The exception never overrides the error-about-error ban.
    assert!(!error_allowed(ErrorContext {
        invoking_is_icmp_error: true,
        dest_is_multicast: true,
        multicast_exception: true,
        ..allowed_base()
    }));
}

/// An echo request as the IPv6 packet a router would quote.
fn quoted_v6_echo(next_header: u8, echo_type: u8) -> alloc::vec::Vec<u8> {
    let mut packet = alloc::vec![0u8; crate::ipv6::IPV6_HEADER_LEN + 8];
    packet[0] = 0x60;
    packet[6] = next_header;
    packet[8..24].copy_from_slice(&Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1).octets());
    packet[24..40].copy_from_slice(&Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 9).octets());
    let echo = &mut packet[crate::ipv6::IPV6_HEADER_LEN..];
    echo[0] = echo_type;
    echo[4..6].copy_from_slice(&0xBEEFu16.to_be_bytes());
    echo[6..8].copy_from_slice(&3u16.to_be_bytes());
    packet
}

fn error_about(invoking: &[u8]) -> IcmpError<'_> {
    IcmpError {
        kind: IcmpErrorKind::TimeExceeded { code: 0 },
        invoking,
    }
}

#[test]
fn an_ipv6_quote_names_its_echo() {
    let packet = quoted_v6_echo(crate::ipv6::NEXT_HEADER_ICMPV6, TYPE_V6_ECHO_REQUEST);
    assert_eq!(
        error_about(&packet).quoted_echo(),
        Some(QuotedEcho {
            source: crate::addr::IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)),
            destination: crate::addr::IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 9)),
            identifier: 0xBEEF,
            sequence: 3,
        })
    );
}

#[test]
fn a_quote_that_is_not_an_echo_requests_first_bytes_names_none() {
    // Another upper protocol, an echo reply, and a short excerpt.
    assert_eq!(
        error_about(&quoted_v6_echo(17, TYPE_V6_ECHO_REQUEST)).quoted_echo(),
        None
    );
    assert_eq!(
        error_about(&quoted_v6_echo(
            crate::ipv6::NEXT_HEADER_ICMPV6,
            TYPE_V6_ECHO_REPLY
        ))
        .quoted_echo(),
        None
    );
    let packet = quoted_v6_echo(crate::ipv6::NEXT_HEADER_ICMPV6, TYPE_V6_ECHO_REQUEST);
    assert_eq!(error_about(&packet[..packet.len() - 1]).quoted_echo(), None);
    assert_eq!(error_about(&[]).quoted_echo(), None);

    // A non-first IPv4 fragment carries no ICMP header to read.
    let mut v4 = alloc::vec![0u8; crate::ipv4::IPV4_HEADER_LEN + 8];
    v4[0] = 0x45;
    v4[6] = 0x00;
    v4[7] = 0x10;
    v4[9] = crate::ipv4::PROTOCOL_ICMP;
    v4[crate::ipv4::IPV4_HEADER_LEN] = TYPE_ECHO_REQUEST;
    assert_eq!(error_about(&v4).quoted_echo(), None);
    v4[7] = 0;
    assert!(error_about(&v4).quoted_echo().is_some());
    // An IHL below the minimum header is no header.
    v4[0] = 0x44;
    assert_eq!(error_about(&v4).quoted_echo(), None);
}
