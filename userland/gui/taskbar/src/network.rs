//! The network signal: what the notification area states about the machine's
//! interfaces, from the report the Switchboard publishes.
//!
//! The bar reads nothing itself. With no report, or one its publisher could
//! not take, there is no signal at all rather than a fabricated "offline".
//! Hovering the signal opens a readout of each interface, its condition and
//! its addresses; a secondary press opens the network menu.

use alloc::format;
use alloc::vec::Vec;
use core::net::{Ipv4Addr, Ipv6Addr};

use tairix_abi::net_ipc::{
    if_name_str, NetAddrFamily, NetAddrState, NetIfAddr, NetInterfaceStateRecord,
};
use tairix_abi::switchboard_ipc::NetworkReport;
use tairix_controls::{Fact, FactList};

use crate::notifications::{IconId, StatusKind, StatusSignal};

/// The notification-area id the network signal goes by.
pub const NETWORK_SIGNAL: IconId = IconId(0x6e65_7477_6f01);

/// What one interface is doing, as the readout states it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Condition {
    /// The link is up with a usable address: one past duplicate detection and
    /// reachable beyond the link and the machine itself.
    Connected,
    /// The link is up and nothing usable is assigned yet.
    NoAddress,
    /// The link is down.
    NoLink,
}

impl Condition {
    /// What `state` says its interface is doing.
    #[must_use]
    pub fn of(state: &NetInterfaceStateRecord) -> Self {
        if !state.link_up {
            return Self::NoLink;
        }
        let usable = state
            .addrs
            .iter()
            .take(usize::from(state.addr_count))
            .any(usable);
        if usable {
            Self::Connected
        } else {
            Self::NoAddress
        }
    }

    /// The readout's word for it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Connected => "Connected",
            Self::NoAddress => "No address",
            Self::NoLink => "No link",
        }
    }
}

/// Whether `addr` is one a connection can use: preferred, and confined to
/// neither the link nor the machine.
fn usable(addr: &NetIfAddr) -> bool {
    addr.state == NetAddrState::Preferred
        && match addr.family {
            NetAddrFamily::V4 => {
                let [a, b, c, d, ..] = addr.addr;
                let addr = Ipv4Addr::new(a, b, c, d);
                !addr.is_link_local() && !addr.is_loopback()
            }
            NetAddrFamily::V6 => {
                let addr = Ipv6Addr::from(addr.addr);
                !addr.is_unicast_link_local() && !addr.is_loopback()
            }
        }
}

/// The machine's network as the bar last heard it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkStatus {
    report: Option<NetworkReport>,
    /// Built once per report, so neither a paint nor a pointer sample
    /// formats an address.
    facts: FactList,
}

impl Default for NetworkStatus {
    fn default() -> Self {
        Self {
            report: None,
            facts: FactList::new(Vec::new()),
        }
    }
}

impl NetworkStatus {
    /// Adopt `report` (`None` once its publisher is gone), answering whether
    /// anything the bar states moved.
    pub fn adopt(&mut self, report: Option<NetworkReport>) -> bool {
        if self.report == report {
            return false;
        }
        self.facts = FactList::new(report.as_ref().map_or_else(Vec::new, facts_of));
        self.report = report;
        true
    }

    /// The report's interfaces, when it was measured and names any.
    fn interfaces(&self) -> Option<impl Iterator<Item = &NetInterfaceStateRecord>> {
        let report = self.report.as_ref().filter(|report| report.is_measured())?;
        let mut interfaces = report.interfaces().peekable();
        interfaces.peek()?;
        Some(interfaces)
    }

    /// The signal the notification area draws: connected while any interface
    /// is, offline otherwise, and none without a measured interface to speak
    /// of.
    #[must_use]
    pub fn signal(&self) -> Option<StatusSignal> {
        let mut interfaces = self.interfaces()?;
        let kind = if interfaces.any(|interface| Condition::of(interface) == Condition::Connected) {
            StatusKind::Network
        } else {
            StatusKind::NetworkOffline
        };
        Some(StatusSignal::new(NETWORK_SIGNAL, kind))
    }

    /// The readout: each interface with its condition, then its addresses,
    /// then how many the report could not name.
    #[must_use]
    pub const fn facts(&self) -> &FactList {
        &self.facts
    }
}

fn facts_of(report: &NetworkReport) -> Vec<Fact> {
    let mut facts = Vec::new();
    for state in report.interfaces() {
        facts.push(Fact::new(
            if_name_str(&state.name),
            Condition::of(state).label(),
        ));
        for addr in state.addrs.iter().take(usize::from(state.addr_count)) {
            let family = match addr.family {
                NetAddrFamily::V4 => "IPv4",
                NetAddrFamily::V6 => "IPv6",
            };
            facts.push(Fact::new(family, tairix_procinfo::render_if_addr(addr)));
        }
    }
    match report.omitted() {
        0 => {}
        1 => facts.push(Fact::new("Not shown", "1 more interface")),
        more => facts.push(Fact::new("Not shown", format!("{more} more interfaces"))),
    }
    facts
}

#[cfg(test)]
#[path = "network_tests.rs"]
pub(crate) mod tests;
