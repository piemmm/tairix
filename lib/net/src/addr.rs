//! The dual-stack address vocabulary.
//!
//! IPv4 and IPv6 are peers throughout `lib/net`, expressed in one
//! vocabulary: the [`core::net`] address types ([`IpAddr`], [`Ipv4Addr`],
//! [`Ipv6Addr`]), re-exported here so every layer of the stack — and every
//! consumer of the engine — names the same types the language already
//! defines, rather than a parallel first-party copy that would drift.
//!
//! What `core::net` deliberately does not carry, this module adds:
//!
//! - [`Ipv6Scope`] — RFC 4007 scope classification, needed to decide
//!   whether an address is meaningful beyond one link.
//! - [`zone_of`] — the zone, an interface, an address of narrower than
//!   global scope is qualified by: such an address names a host only within
//!   its zone, so on a host with several interfaces it is ambiguous without
//!   one.

use tairix_abi::net_ipc::IF_NAME_LEN;

pub use core::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The two-bit ECN codepoint (RFC 3168 §5) carried in the low two bits of
/// the IPv4 TOS byte and the IPv6 Traffic Class field.
///
/// It is one shared vocabulary for both IP versions (the charter's
/// one-definition rule): the IPv4 header carries it as its own field and
/// the IPv6 header derives it from its Traffic Class, but both parse and
/// emit the same four codepoints, and the TCP engine reasons about ECN in
/// these terms without knowing which family carried the packet.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Ecn {
    /// `00` — Not ECN-Capable Transport: the packet opts out of ECN and a
    /// congested router drops it rather than marking it.
    #[default]
    NotEct = 0b00,
    /// `01` — ECN-Capable Transport (1). Equivalent to [`Ecn::Ect0`] for
    /// transport purposes; RFC 3168 §5 defines both as ECT.
    Ect1 = 0b01,
    /// `10` — ECN-Capable Transport (0), the codepoint an ECN-capable
    /// sender stamps on its data packets so a router may mark instead of
    /// drop them.
    Ect0 = 0b10,
    /// `11` — Congestion Experienced: a router re-marked an ECT packet to
    /// signal congestion to the receiver (RFC 3168 §5).
    Ce = 0b11,
}

impl Ecn {
    /// The two-bit wire value.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self as u8
    }

    /// The codepoint encoded by the low two bits of `bits` (any higher
    /// bits, e.g. the DSCP, are ignored). Total: every input maps to one
    /// of the four codepoints, so a hostile header can never be rejected
    /// for its ECN field alone.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        match bits & 0b11 {
            0b00 => Self::NotEct,
            0b01 => Self::Ect1,
            0b10 => Self::Ect0,
            _ => Self::Ce,
        }
    }

    /// Whether this codepoint is ECN-Capable Transport (either ECT value).
    #[must_use]
    pub const fn is_ect(self) -> bool {
        matches!(self, Self::Ect0 | Self::Ect1)
    }

    /// Whether this codepoint signals Congestion Experienced.
    #[must_use]
    pub const fn is_ce(self) -> bool {
        matches!(self, Self::Ce)
    }
}

/// An IPv6 scope per RFC 4007 §5 / RFC 4291 §2.7.
///
/// Discriminants are the multicast `scop` field values, so the derived
/// ordering is the RFC 4007 "covers" ordering: a smaller scope is
/// contained in every larger one.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Ipv6Scope {
    /// Spans a single interface only (`scop` 1); loopback traffic.
    InterfaceLocal = 0x1,
    /// Spans a single link (`scop` 2); `fe80::/10` unicast and the
    /// loopback address (RFC 4007 §4 treats the loopback interface as
    /// its own link).
    LinkLocal = 0x2,
    /// The smallest administratively configured scope (`scop` 4).
    AdminLocal = 0x4,
    /// Spans a single site (`scop` 5).
    SiteLocal = 0x5,
    /// Spans several sites of one organisation (`scop` 8).
    OrganizationLocal = 0x8,
    /// Unbounded (`scop` 0xE); all ordinary unicast addresses.
    Global = 0xE,
}

impl Ipv6Scope {
    /// Classify `addr`.
    ///
    /// Returns `None` for the addresses that have no scope to act on —
    /// the unspecified address and multicast addresses whose `scop`
    /// field is reserved or unassigned — so a caller fails closed
    /// (drops) instead of guessing.
    ///
    /// Unicast rules: `fe80::/10` and the loopback address are
    /// link-local (RFC 4007 §4); everything else — including ULA
    /// `fc00::/7`, whose *scope* is global per RFC 4193 — is
    /// [`Ipv6Scope::Global`]. The deprecated site-local `fec0::/10`
    /// prefix (RFC 3879) still classifies as [`Ipv6Scope::SiteLocal`],
    /// because that is what its bits say on the wire.
    #[must_use]
    pub fn of(addr: &Ipv6Addr) -> Option<Self> {
        if addr.is_unspecified() {
            return None;
        }
        if addr.is_multicast() {
            return match addr.octets()[1] & 0x0F {
                0x1 => Some(Self::InterfaceLocal),
                0x2 => Some(Self::LinkLocal),
                0x4 => Some(Self::AdminLocal),
                0x5 => Some(Self::SiteLocal),
                0x8 => Some(Self::OrganizationLocal),
                0xE => Some(Self::Global),
                _ => None,
            };
        }
        if addr.is_loopback() || is_unicast_link_local(addr) {
            return Some(Self::LinkLocal);
        }
        if addr.segments()[0] & 0xFFC0 == 0xFEC0 {
            return Some(Self::SiteLocal);
        }
        Some(Self::Global)
    }
}

/// True for a unicast link-local address (`fe80::/10`, RFC 4291 §2.5.6).
#[must_use]
pub fn is_unicast_link_local(addr: &Ipv6Addr) -> bool {
    addr.segments()[0] & 0xFFC0 == 0xFE80
}

/// Whether `addr` names a host only within a zone (RFC 4007 §6): an IPv6
/// address of narrower than global scope.
#[must_use]
pub fn needs_zone(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V6(v6) => Ipv6Scope::of(&v6).is_some_and(|scope| scope < Ipv6Scope::Global),
        IpAddr::V4(_) => false,
    }
}

/// The zone `addr` is qualified by when it was met on `interface`: that
/// interface when the address [needs one](needs_zone), and none when it
/// names a host alone.
#[must_use]
pub fn zone_of(addr: IpAddr, interface: [u8; IF_NAME_LEN]) -> Option<[u8; IF_NAME_LEN]> {
    needs_zone(addr).then_some(interface)
}

/// The solicited-node multicast group of `addr`
/// (`ff02::1:ffXX:XXXX`, RFC 4291 §2.7.1): the last 24 bits of the
/// unicast address appended to the fixed prefix. Neighbour and
/// duplicate-address solicitations are sent to this group, so a host
/// listens on it for every address it has, tentative included.
#[must_use]
pub fn solicited_node_multicast(addr: &Ipv6Addr) -> Ipv6Addr {
    let unicast = addr.octets();
    let mut octets = [0u8; 16];
    octets[0] = 0xFF;
    octets[1] = 0x02;
    octets[11] = 0x01;
    octets[12] = 0xFF;
    octets[13] = unicast[13];
    octets[14] = unicast[14];
    octets[15] = unicast[15];
    Ipv6Addr::from(octets)
}

/// The all-nodes link-local multicast group (`ff02::1`, RFC 4291).
pub const ALL_NODES: Ipv6Addr = Ipv6Addr::new(0xFF02, 0, 0, 0, 0, 0, 0, 1);

/// The all-routers link-local multicast group (`ff02::2`, RFC 4291).
pub const ALL_ROUTERS: Ipv6Addr = Ipv6Addr::new(0xFF02, 0, 0, 0, 0, 0, 0, 2);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecn_codepoint_round_trips_and_classifies() {
        for cp in [Ecn::NotEct, Ecn::Ect1, Ecn::Ect0, Ecn::Ce] {
            assert_eq!(Ecn::from_bits(cp.bits()), cp);
        }
        // Only the low two bits are read; DSCP (high bits) is ignored.
        assert_eq!(Ecn::from_bits(0b1011_0110), Ecn::Ect0);
        assert_eq!(Ecn::from_bits(0b1111_1111), Ecn::Ce);
        assert!(Ecn::Ect0.is_ect() && Ecn::Ect1.is_ect());
        assert!(!Ecn::NotEct.is_ect() && !Ecn::Ce.is_ect());
        assert!(Ecn::Ce.is_ce() && !Ecn::Ect0.is_ce());
        assert_eq!(Ecn::default(), Ecn::NotEct);
    }

    #[test]
    fn unicast_scope_classification() {
        let link_local = Ipv6Addr::new(0xFE80, 0, 0, 0, 0, 0, 0, 1);
        assert_eq!(Ipv6Scope::of(&link_local), Some(Ipv6Scope::LinkLocal));
        assert!(is_unicast_link_local(&link_local));

        let global = Ipv6Addr::new(0x2001, 0xDB8, 0, 0, 0, 0, 0, 1);
        assert_eq!(Ipv6Scope::of(&global), Some(Ipv6Scope::Global));
        assert!(!is_unicast_link_local(&global));

        // ULA has global *scope* (RFC 4193 §3.3).
        let ula = Ipv6Addr::new(0xFD00, 0, 0, 0, 0, 0, 0, 1);
        assert_eq!(Ipv6Scope::of(&ula), Some(Ipv6Scope::Global));

        // Deprecated site-local bits still classify as what they say.
        let site = Ipv6Addr::new(0xFEC0, 0, 0, 0, 0, 0, 0, 1);
        assert_eq!(Ipv6Scope::of(&site), Some(Ipv6Scope::SiteLocal));

        assert_eq!(
            Ipv6Scope::of(&Ipv6Addr::LOCALHOST),
            Some(Ipv6Scope::LinkLocal)
        );
        assert_eq!(Ipv6Scope::of(&Ipv6Addr::UNSPECIFIED), None);
    }

    #[test]
    fn multicast_scope_follows_scop_field() {
        let cases = [
            (0x1, Some(Ipv6Scope::InterfaceLocal)),
            (0x2, Some(Ipv6Scope::LinkLocal)),
            (0x4, Some(Ipv6Scope::AdminLocal)),
            (0x5, Some(Ipv6Scope::SiteLocal)),
            (0x8, Some(Ipv6Scope::OrganizationLocal)),
            (0xE, Some(Ipv6Scope::Global)),
            (0x0, None),
            (0x3, None),
            (0xF, None),
        ];
        for (scop, expected) in cases {
            let addr = Ipv6Addr::new(0xFF00 | scop, 0, 0, 0, 0, 0, 0, 1);
            assert_eq!(Ipv6Scope::of(&addr), expected, "scop {scop:#x}");
        }
    }

    #[test]
    fn scope_ordering_is_rfc4007_covering_order() {
        assert!(Ipv6Scope::InterfaceLocal < Ipv6Scope::LinkLocal);
        assert!(Ipv6Scope::LinkLocal < Ipv6Scope::AdminLocal);
        assert!(Ipv6Scope::AdminLocal < Ipv6Scope::SiteLocal);
        assert!(Ipv6Scope::SiteLocal < Ipv6Scope::OrganizationLocal);
        assert!(Ipv6Scope::OrganizationLocal < Ipv6Scope::Global);
    }

    #[test]
    fn an_address_narrower_than_global_is_qualified_by_its_zone() {
        let eth0 = *b"eth0\0\0\0\0\0\0\0\0\0\0\0\0";
        for zoned in [
            Ipv6Addr::new(0xFE80, 0, 0, 0, 0, 0, 0, 1),
            Ipv6Addr::LOCALHOST,
            Ipv6Addr::new(0xFF02, 0, 0, 0, 0, 0, 0, 1),
            Ipv6Addr::new(0xFEC0, 0, 0, 0, 0, 0, 0, 1),
        ] {
            assert_eq!(zone_of(IpAddr::V6(zoned), eth0), Some(eth0), "{zoned}");
        }
        for alone in [
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xDB8, 0, 0, 0, 0, 0, 1)),
            IpAddr::V6(Ipv6Addr::new(0xFD00, 0, 0, 0, 0, 0, 0, 1)),
            IpAddr::V6(Ipv6Addr::new(0xFF0E, 0, 0, 0, 0, 0, 0, 1)),
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1)),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        ] {
            assert_eq!(zone_of(alone, eth0), None, "{alone}");
        }
    }
}
