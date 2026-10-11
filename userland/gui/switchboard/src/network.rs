//! The network report the session's notification area states: the machine's
//! interfaces as this service last read them, from the interface-state
//! reading the network notice refreshes.

use tairix_abi::net_ipc::NetInterfaceStateRecord;
use tairix_abi::switchboard_ipc::NetworkReport;

/// The report `state` makes: each interface it names, in the stack's order.
/// A missing reading — refused under this service's authority, or not yet
/// taken — is an unmeasured report, never an empty one. It needs nothing but
/// the state query, so a user who may see the interfaces' links and addresses
/// sees the network status without authority over their hardware facts.
#[must_use]
pub fn report(state: Option<&[NetInterfaceStateRecord]>) -> NetworkReport {
    state.map_or(NetworkReport::UNMEASURED, NetworkReport::measured)
}

#[cfg(test)]
#[path = "network_tests.rs"]
mod tests;
