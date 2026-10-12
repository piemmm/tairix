//! Reading the network's state from the System Information API.
//!
//! One [`Snapshot`] is what a refresh learns: each source read whole or
//! refused, never partly. A refused source is an answer, not a failure — the
//! map is drawn from what the caller may read, and each refusal is stated in
//! the window so the gap is never silent.

use alloc::collections::btree_map::{BTreeMap, Entry};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::net_ipc::{
    NetInterfaceFactsRecord, NetInterfaceStateRecord, NetNeighborRecord, NetRouteRecord,
};
use tairix_abi::sysinfo::SysinfoQueryId;
use tairix_abi::Errno;
use tairix_devids::DevIds;
use tairix_net::dns::DisplayLabel;
use tairix_procinfo::{
    for_each_net_interface, for_each_net_neighbor, for_each_net_route, walk_records, CallError,
    ListError, Transport, WalkStep, NET_INTERFACE_PAGE,
};

use crate::ingest::{assigned_oui, Inventory};
use crate::model::MAX_NODES;

/// One read of a source: its records, or why it could not be read.
pub type Read<R> = Result<Vec<R>, ListError>;

/// What one refresh learned.
#[derive(Debug)]
pub struct Snapshot {
    /// The interfaces' static facts.
    pub facts: Read<NetInterfaceFactsRecord>,
    /// The interfaces' live state.
    pub state: Read<NetInterfaceStateRecord>,
    /// The neighbour caches.
    pub neighbors: Read<NetNeighborRecord>,
    /// The routing tables.
    pub routes: Read<NetRouteRecord>,
    /// The organisation each neighbour's assigned hardware address names,
    /// display-safe, or why makers cannot be named.
    pub vendors: Result<BTreeMap<u32, String>, String>,
}

impl Snapshot {
    /// Read every source over `transport`, each held to [`MAX_NODES`] records:
    /// a segment of attacker-chosen size cannot grow the read without bound,
    /// and the map could hold no more.
    pub fn read(transport: &dyn Transport) -> Self {
        Self {
            facts: collect(|sink| for_each_net_interface(transport, sink)),
            state: collect(|sink| {
                walk_records(
                    transport,
                    SysinfoQueryId::NET_INTERFACE_STATE,
                    NetInterfaceStateRecord::WIRE_LEN,
                    NET_INTERFACE_PAGE,
                    NetInterfaceStateRecord::from_bytes,
                    sink,
                )
            }),
            neighbors: collect(|sink| for_each_net_neighbor(transport, sink)),
            routes: collect(|sink| for_each_net_route(transport, sink)),
            vendors: Ok(BTreeMap::new()),
        }
    }

    /// Name the organisation behind each neighbour's assigned hardware
    /// address from the OUI `table`, escaped as text from elsewhere is.
    pub fn name_vendors<B: AsRef<[u8]>>(&mut self, table: &DevIds<B>) {
        let mut vendors = BTreeMap::new();
        if let Ok(neighbors) = &self.neighbors {
            for oui in neighbors
                .iter()
                .filter_map(|neighbor| neighbor.mac.and_then(assigned_oui))
            {
                if let Entry::Vacant(slot) = vendors.entry(oui) {
                    if let Some(name) = table.organisation(oui) {
                        slot.insert(DisplayLabel(name.as_bytes()).to_string());
                    }
                }
            }
        }
        self.vendors = Ok(vendors);
    }

    /// Fold what was read into `inventory`, replacing what the last refresh
    /// held, and answer the notices that say what could not be read.
    pub fn apply(self, inventory: &mut Inventory) -> Vec<String> {
        let mut notices = Vec::new();
        inventory.clear_interfaces();
        match self.facts {
            Ok(records) => records
                .into_iter()
                .for_each(|record| inventory.observe_interface_facts(record)),
            Err(err) => notices.push(notice("Interface details", "CAP_SYSINFO_HW", err)),
        }
        match self.state {
            Ok(records) => records
                .into_iter()
                .for_each(|record| inventory.observe_interface_state(record)),
            Err(err) => notices.push(notice("Interface addresses", "CAP_SYSINFO_GLOBAL", err)),
        }
        match self.neighbors {
            Ok(records) => inventory.replace_neighbors(records),
            Err(err) => {
                inventory.replace_neighbors(Vec::new());
                notices.push(notice("Neighbouring hosts", "CAP_SYSINFO_HW", err));
            }
        }
        match self.routes {
            Ok(records) => inventory.replace_routes(records),
            Err(err) => {
                inventory.replace_routes(Vec::new());
                notices.push(notice("Routers and routes", "CAP_SYSINFO_GLOBAL", err));
            }
        }
        match self.vendors {
            Ok(vendors) => inventory.replace_vendors(vendors),
            Err(why) => {
                inventory.replace_vendors(BTreeMap::new());
                notices.push(alloc::format!("Hardware makers are not shown: {why}."));
            }
        }
        notices
    }
}

/// Run one walk, keeping at most [`MAX_NODES`] records.
fn collect<R: Clone>(
    walk: impl FnOnce(&mut dyn FnMut(&R) -> Result<WalkStep, Errno>) -> Result<(), ListError>,
) -> Read<R> {
    let mut records = Vec::new();
    walk(&mut |record: &R| {
        records.push(record.clone());
        Ok(if records.len() >= MAX_NODES {
            WalkStep::Stop
        } else {
            WalkStep::Continue
        })
    })?;
    Ok(records)
}

/// The line that says why `what` is missing from the map.
fn notice(what: &str, capability: &str, err: ListError) -> String {
    match err {
        ListError::Call(CallError::PermissionDenied) => {
            alloc::format!("{what} are not shown: reading them needs {capability}.")
        }
        ListError::Call(CallError::Service(errno)) | ListError::Sink(errno) => {
            alloc::format!("{what} could not be read ({errno}).")
        }
    }
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
