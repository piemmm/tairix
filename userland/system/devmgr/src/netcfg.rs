//! Delivering the stack-wide `net.*` policy to the network stack.
//!
//! `netstack` is the network-parsing sandbox and holds no filesystem
//! capability, so it cannot read `/System/Settings/Configuration/system.conf`
//! itself. The device manager already holds `CAP_NET_ADMIN` and drives the
//! network stack's admin endpoint (see [`crate::netbind`]), so it is the
//! component that reads the stack-wide `net.*` settings from the
//! configuration store post-unlock and delivers them to `netstack` over the
//! capability-gated `ApplyNetworkSettings` admin op (`plans/NETWORK.md`
//! N9b-2).
//!
//! This module is the pure, host-testable policy for that delivery: read the
//! settings through the [`NetworkConfigSource`] seam and, until they have
//! been delivered, push them through the [`crate::netbind::NetstackBind`]
//! seam. Delivery is fail-soft — the store may not be mounted yet (before the
//! root unlock) and the stack may not be up yet, so a failed attempt is
//! logged and retried when the hardware tree next moves or a volume is
//! mounted, exactly like an unavailable driver store. Until the real policy lands, `netstack`'s own
//! safe defaults (both families enabled, SYN cookies `auto`) hold.

use alloc::collections::BTreeSet;

use tairix_abi::net_ipc::{NetBondConfigMsg, NetInterfaceConfigMsg, NetworkSettings, IF_NAME_LEN};
use tairix_log::{log as log_event, Event, Field, FieldValue, Level, Sink};
use tairix_netconfig::InterfaceConfigPlan;

use crate::events;
use crate::netbind::{NetBindState, NetstackBind};

/// The device manager's read of the stack-wide `net.*` policy from the
/// system-configuration store.
///
/// The production implementation reads `system.conf` — the administrator's
/// document when the root is mounted, the shipped default beneath
/// it otherwise — and maps it through the one shared
/// [`SystemConfig::network_settings`](tairix_sysconfig::SystemConfig::network_settings);
/// it is a seam so the delivery policy is host-testable against a scripted
/// double.
pub trait NetworkConfigSource {
    /// Load the current stack-wide network settings.
    ///
    /// Returns [`Some`] when the store was read and parsed (the real policy,
    /// ready to deliver), and [`None`] when it could not be read — the store
    /// service is not reachable yet, the file is absent, or the read failed.
    /// A [`None`] is not an error: the caller keeps the network stack on its
    /// safe defaults and retries on the next generation bump.
    fn load(&mut self) -> Option<NetworkSettings>;
}

/// The device manager's memory of the stack-wide `net.*` policy it last
/// delivered to the network stack.
///
/// Not a "delivered once" flag: the shipped default on the read-only
/// `/System` volume is only the *lower* layer of the store, and the
/// authoritative document on the writable root becomes readable when the
/// root is mounted. So the policy is re-read on each generation
/// bump and re-delivered whenever it differs from what the stack was last
/// given, which is what makes an administrator's edit take effect on the
/// next boot rather than being silently ignored.
#[derive(Default)]
pub struct NetConfigState {
    delivered: Option<NetworkSettings>,
    deferred: bool,
}

impl NetConfigState {
    /// A fresh state with nothing delivered.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a policy has already been delivered and accepted.
    #[must_use]
    pub fn is_delivered(&self) -> bool {
        self.delivered.is_some()
    }

    /// Whether the last pass left a readable policy undelivered.
    ///
    /// Neither the store becoming readable nor the stack coming up bumps a
    /// generation, so a caller that parks for one would never retry.
    #[must_use]
    pub fn has_deferred_work(&self) -> bool {
        self.deferred
    }
}

/// Deliver the stack-wide `net.*` policy to the network stack whenever it
/// differs from the policy last accepted.
///
/// Reads the policy through `source`; if the store is not yet readable
/// ([`None`]) it leaves the stack on its safe defaults and returns (retried
/// at the next reaction). A policy identical to the last delivered one is not
/// re-pushed. Otherwise it is pushed through `netstack`: success is recorded,
/// and a refusal is logged fail-soft and retried at the next reaction — the stack may
/// not have bound its admin endpoint yet.
pub fn deliver_network_settings(
    source: &mut dyn NetworkConfigSource,
    state: &mut NetConfigState,
    netstack: &mut dyn NetstackBind,
    sink: &dyn Sink,
) {
    // Deferred means concrete work in hand that could not be completed, never
    // "nothing to do yet": an absent store is the steady state on a machine
    // that has no policy, and treating it as outstanding would wake the loop
    // for the life of that machine.
    state.deferred = false;
    let Some(settings) = source.load() else {
        // The store is not readable yet (the store service not reachable yet,
        // or an absent/failed read): the stack keeps its safe defaults and
        // this is retried on the next generation bump. Not logged — an absent
        // store early in boot is the expected state, not an anomaly.
        return;
    };
    if state.delivered == Some(settings) {
        return;
    }
    if netstack.apply_settings(settings).is_ok() {
        state.delivered = Some(settings);
        log_event(
            sink,
            &Event {
                level: Level::Info,
                id: events::NETWORK_SETTINGS_DELIVERED,
                message: "network settings delivered to the network stack",
                fields: &[],
            },
        );
    } else {
        state.deferred = true;
        log_event(
            sink,
            &Event {
                level: Level::Warn,
                id: events::NETWORK_SETTINGS_DELIVERY_FAILED,
                message: "network settings delivery to the network stack failed; will retry",
                fields: &[],
            },
        );
    }
}

/// The device manager's read of the per-interface `network.conf`
/// configuration (`plans/NETWORK.md` §6.1).
///
/// The production implementation reads
/// `/System/Settings/Network/network.conf` and maps it through the one
/// shared `lib/netconfig` projection ([`InterfaceConfigPlan::of`]); it is a
/// seam so the delivery policy is host-testable against a scripted double.
pub trait NetworkInterfaceConfigSource {
    /// Load the current per-interface configuration plan.
    ///
    /// Returns [`Some`] when the store was read and parsed, and [`None`]
    /// when it could not be read — the store service is not reachable yet,
    /// the file is absent, the read failed, or the document did not parse. A
    /// [`None`] is not an error: the caller retries on the next generation
    /// bump (fail closed — never a half-applied guess).
    fn load(&mut self) -> Option<InterfaceConfigPlan>;
}

/// The device manager's memory of the per-interface plan it last read, which
/// of it the network stack has accepted, and which it refused.
///
/// Unlike the stack-wide settings (one message for the whole policy), each
/// interface's configuration is delivered when *its* interface binds —
/// asynchronously, as the driver comes up — and a delivered interface is
/// skipped thereafter (idempotent). A refusal is the stack's answer, not a
/// failure to reach it: the configuration is offered again only once its
/// document entry changes or another NIC binds.
#[derive(Default)]
pub struct NetIfConfigState {
    plan: Option<InterfaceConfigPlan>,
    delivered: BTreeSet<[u8; IF_NAME_LEN]>,
    delivered_bonds: BTreeSet<[u8; IF_NAME_LEN]>,
    refused: BTreeSet<[u8; IF_NAME_LEN]>,
    refused_bonds: BTreeSet<[u8; IF_NAME_LEN]>,
    nics_seen: usize,
    rejected_logged: bool,
}

impl NetIfConfigState {
    /// A fresh state with nothing loaded or delivered.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adopt what the source answered with this bump.
    ///
    /// [`None`] is the store being momentarily unreadable rather than an
    /// empty document, so the plan already held stands and delivery of it
    /// carries on. A plan that differs forgets the delivery of every
    /// interface whose message changed — that is what makes an
    /// administrator's live edit reach the running stack instead of being
    /// masked by the fact that the *old* configuration landed — while an
    /// interface the edit did not touch keeps its mark and is not re-pushed
    /// for nothing.
    fn adopt(&mut self, loaded: Option<InterfaceConfigPlan>) {
        let Some(plan) = loaded else {
            return;
        };
        if let Some(held) = self.plan.as_ref() {
            if *held == plan {
                return;
            }
            let message_kept =
                |alias: &[u8; IF_NAME_LEN]| held.message_for(alias) == plan.message_for(alias);
            let bond_kept =
                |alias: &[u8; IF_NAME_LEN]| held.bond_for(alias) == plan.bond_for(alias);
            self.delivered.retain(message_kept);
            self.refused.retain(message_kept);
            self.delivered_bonds.retain(bond_kept);
            self.refused_bonds.retain(bond_kept);
            if held.rejected != plan.rejected {
                self.rejected_logged = false;
            }
        }
        self.plan = Some(plan);
    }
}

/// Deliver each managed interface's `network.conf` configuration to the
/// network stack once the stack has what it names.
///
/// The plan is re-read through `source` at every reaction, exactly as the
/// stack-wide policy is: the document on the writable root only becomes
/// readable at the root mount, and an administrator may edit it at any time
/// afterwards. Until the store is readable this does nothing. Any config-error
/// rejects (a managed non-bond interface with no `match.mac`/`match.node`) are
/// surfaced loud, once per distinct rejected set.
///
/// A configuration is offered only once the stack holds what it names: an
/// interface's once a NIC in `bound` is the one its selector names, a bond's
/// once every member's configuration has landed, and a bond's own addressing
/// once the bond is composed. So no interface the stack lacks is ever offered,
/// and every refusal recorded is a real one.
pub fn deliver_interface_configs(
    source: &mut dyn NetworkInterfaceConfigSource,
    state: &mut NetIfConfigState,
    bound: &NetBindState,
    netstack: &mut dyn NetstackBind,
    sink: &dyn Sink,
) {
    // An unreadable store early in boot is the expected state, not an
    // anomaly, so it is not logged.
    state.adopt(source.load());
    let Some(plan) = state.plan.take() else {
        return;
    };
    if !state.rejected_logged {
        for name in &plan.rejected {
            audit_iface(
                sink,
                events::NETWORK_IFCONFIG_REJECTED,
                Level::Warn,
                "network.conf interface has no match.mac/match.node identity; skipped",
                name,
            );
        }
        state.rejected_logged = true;
    }
    let nics = bound.nics().count();
    if nics != state.nics_seen {
        state.nics_seen = nics;
        state.refused.clear();
        state.refused_bonds.clear();
    }

    for msg in plan.messages.iter().filter(|msg| !msg.names_by_alias()) {
        if state.offerable(msg)
            && bound
                .nics()
                .any(|nic| msg.selects(&nic.mac, nic.node_location))
        {
            state.offer_interface(msg, netstack, sink);
        }
    }
    for bond in &plan.bonds {
        let members_landed = bond
            .members()
            .iter()
            .all(|member| state.delivered.contains(member));
        if members_landed
            && !state.delivered_bonds.contains(&bond.alias)
            && !state.refused_bonds.contains(&bond.alias)
        {
            state.offer_bond(bond, netstack, sink);
        }
    }
    for msg in plan.messages.iter().filter(|msg| msg.names_by_alias()) {
        if state.offerable(msg) && state.delivered_bonds.contains(&msg.alias) {
            state.offer_interface(msg, netstack, sink);
        }
    }
    state.plan = Some(plan);
}

impl NetIfConfigState {
    /// Whether `msg` is neither delivered nor refused as it stands.
    fn offerable(&self, msg: &NetInterfaceConfigMsg) -> bool {
        !self.delivered.contains(&msg.alias) && !self.refused.contains(&msg.alias)
    }

    fn offer_interface(
        &mut self,
        msg: &NetInterfaceConfigMsg,
        netstack: &mut dyn NetstackBind,
        sink: &dyn Sink,
    ) {
        let (id, level, message) = if netstack.apply_interface_config(msg).is_ok() {
            self.delivered.insert(msg.alias);
            (
                events::NETWORK_IFCONFIG_DELIVERED,
                Level::Info,
                "per-interface network configuration delivered",
            )
        } else {
            self.refused.insert(msg.alias);
            (
                events::NETWORK_IFCONFIG_DELIVERY_FAILED,
                Level::Warn,
                "per-interface network configuration refused",
            )
        };
        audit_iface(sink, id, level, message, &msg.alias);
    }

    fn offer_bond(
        &mut self,
        bond: &NetBondConfigMsg,
        netstack: &mut dyn NetstackBind,
        sink: &dyn Sink,
    ) {
        let (id, level, message) = if netstack.apply_bond_config(bond).is_ok() {
            self.delivered_bonds.insert(bond.alias);
            (
                events::NETWORK_IFCONFIG_DELIVERED,
                Level::Info,
                "bond interface composed",
            )
        } else {
            self.refused_bonds.insert(bond.alias);
            (
                events::NETWORK_IFCONFIG_DELIVERY_FAILED,
                Level::Warn,
                "bond interface composition refused",
            )
        };
        audit_iface(sink, id, level, message, &bond.alias);
    }
}

/// Emit one audit record carrying the interface alias.
fn audit_iface(
    sink: &dyn Sink,
    id: tairix_log::EventId,
    level: Level,
    message: &'static str,
    iface: &[u8; IF_NAME_LEN],
) {
    let name = tairix_abi::net_ipc::if_name_str(iface);
    log_event(
        sink,
        &Event {
            level,
            id,
            message,
            fields: &[Field {
                key: "iface",
                value: FieldValue::Str(name),
            }],
        },
    );
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    use super::*;

    use crate::testsink::RecordingSink;
    use tairix_abi::net_ipc::IF_NAME_LEN;
    use tairix_abi::Errno;

    /// A scripted config source: hands out a queued `load` result per call.
    struct ScriptedSource {
        results: RefCell<Vec<Option<NetworkSettings>>>,
    }

    impl ScriptedSource {
        fn new(results: Vec<Option<NetworkSettings>>) -> Self {
            Self {
                results: RefCell::new(results),
            }
        }
    }

    impl NetworkConfigSource for ScriptedSource {
        fn load(&mut self) -> Option<NetworkSettings> {
            self.results.borrow_mut().pop().flatten()
        }
    }

    /// A recording netstack seam: captures each delivered policy and answers
    /// each `apply_settings` with a scripted result.
    struct RecordingNetstack {
        applied: RefCell<Vec<NetworkSettings>>,
        results: RefCell<Vec<Result<(), Errno>>>,
        ifconfigs: RefCell<Vec<NetInterfaceConfigMsg>>,
        ifconfig_results: RefCell<Vec<Result<(), Errno>>>,
        bonds: RefCell<Vec<NetBondConfigMsg>>,
        macs: RefCell<Vec<[u8; 6]>>,
    }

    impl RecordingNetstack {
        fn new(results: Vec<Result<(), Errno>>) -> Self {
            Self {
                applied: RefCell::new(Vec::new()),
                results: RefCell::new(results),
                ifconfigs: RefCell::new(Vec::new()),
                ifconfig_results: RefCell::new(Vec::new()),
                bonds: RefCell::new(Vec::new()),
                macs: RefCell::new(Vec::new()),
            }
        }

        /// A recorder scripted with per-`apply_interface_config` results
        /// (consumed front-to-back).
        fn with_ifconfig_results(results: Vec<Result<(), Errno>>) -> Self {
            let mut me = Self::new(Vec::new());
            // Reverse so `pop` returns them in call order.
            let mut reversed = results;
            reversed.reverse();
            me.ifconfig_results = RefCell::new(reversed);
            me
        }
    }

    impl NetstackBind for RecordingNetstack {
        fn bind_driver(
            &mut self,
            _e: u64,
            _i: &[u8; IF_NAME_LEN],
            _node_location: u64,
        ) -> Result<[u8; 6], Errno> {
            self.macs.borrow_mut().pop().ok_or(Errno::NotFound)
        }

        fn apply_settings(&mut self, settings: NetworkSettings) -> Result<(), Errno> {
            self.applied.borrow_mut().push(settings);
            self.results.borrow_mut().pop().unwrap_or(Ok(()))
        }

        fn apply_interface_config(&mut self, config: &NetInterfaceConfigMsg) -> Result<(), Errno> {
            self.ifconfigs.borrow_mut().push(*config);
            self.ifconfig_results.borrow_mut().pop().unwrap_or(Ok(()))
        }

        fn apply_bond_config(&mut self, config: &NetBondConfigMsg) -> Result<(), Errno> {
            self.bonds.borrow_mut().push(*config);
            Ok(())
        }
    }

    // A flat test builder mirroring the six independent wire flags of
    // `NetworkSettings`; an enum would only obscure the mapping the test
    // is asserting.
    #[allow(clippy::fn_params_excessive_bools)]
    fn settings(
        v4: bool,
        v6: bool,
        cookies: bool,
        privacy: bool,
        keepalive: bool,
        ecn: bool,
    ) -> NetworkSettings {
        NetworkSettings {
            ipv4_enabled: v4,
            ipv6_enabled: v6,
            syncookies_always: cookies,
            ipv6_privacy: privacy,
            tcp_keepalive: keepalive,
            tcp_ecn: ecn,
            // The budget a 1 GiB machine derives, so a delivered policy
            // is a fixed figure rather than the host's own RAM.
            socket_budget_bytes: 128 * 1024 * 1024,
        }
    }

    /// A one-gibibyte machine, the size the derived capacities here
    /// are stated against.
    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn settings_map_from_the_config_registry() {
        let mut config = tairix_sysconfig::SystemConfig::default();
        assert_eq!(
            config.network_settings(GIB),
            settings(true, true, false, false, false, false),
            "the registry defaults map to families-on, cookies-auto, privacy-off, keepalive-off, ecn-off"
        );
        config.net_ipv6_enabled = tairix_sysconfig::NetToggle::Disabled;
        config.net_tcp_syncookies = tairix_sysconfig::SynCookies::Always;
        config.net_ipv6_privacy = tairix_sysconfig::NetToggle::Enabled;
        config.net_tcp_keepalive = tairix_sysconfig::NetToggle::Enabled;
        config.net_tcp_ecn = tairix_sysconfig::NetToggle::Enabled;
        assert_eq!(
            config.network_settings(GIB),
            settings(true, false, true, true, true, true)
        );
        // A capacity follows the machine the deliverer read, so the same
        // document yields a larger budget on a larger one.
        assert_eq!(
            config.network_settings(512 * GIB).socket_budget_bytes,
            64 * GIB
        );
    }

    #[test]
    fn absent_store_keeps_defaults_and_retries() {
        let mut source = ScriptedSource::new(alloc::vec![None]);
        let mut state = NetConfigState::new();
        let mut netstack = RecordingNetstack::new(Vec::new());
        let sink = RecordingSink::new();
        deliver_network_settings(&mut source, &mut state, &mut netstack, &sink);
        assert!(!state.is_delivered(), "an unreadable store defers delivery");
        assert!(netstack.applied.borrow().is_empty(), "nothing pushed");
        assert!(sink.ids().is_empty(), "the expected early state is quiet");
    }

    #[test]
    fn an_unchanged_policy_is_not_re_delivered() {
        let policy = settings(true, false, true, true, true, true);
        let mut source = ScriptedSource::new(alloc::vec![Some(policy), Some(policy)]);
        let mut state = NetConfigState::new();
        let mut netstack = RecordingNetstack::new(alloc::vec![Ok(()), Ok(())]);
        let sink = RecordingSink::new();
        deliver_network_settings(&mut source, &mut state, &mut netstack, &sink);
        assert!(state.is_delivered());
        assert_eq!(*netstack.applied.borrow(), alloc::vec![policy]);
        assert_eq!(
            sink.ids().as_slice(),
            &[events::NETWORK_SETTINGS_DELIVERED.0]
        );
        // Re-reading the same document costs the stack nothing.
        deliver_network_settings(&mut source, &mut state, &mut netstack, &sink);
        assert_eq!(netstack.applied.borrow().len(), 1, "delivered once");
    }

    #[test]
    fn a_changed_policy_is_re_delivered() {
        // The shipped default on the read-only volume is only the lower
        // layer: the administrator's document becomes readable when the
        // root is mounted, so a policy that changes between reads
        // must reach the stack rather than being ignored as "already
        // delivered".
        let shipped = settings(true, true, false, false, false, false);
        let edited = settings(true, false, true, true, true, true);
        let mut source = ScriptedSource::new(alloc::vec![Some(edited), Some(shipped)]);
        let mut state = NetConfigState::new();
        let mut netstack = RecordingNetstack::new(alloc::vec![Ok(()), Ok(())]);
        let sink = RecordingSink::new();
        deliver_network_settings(&mut source, &mut state, &mut netstack, &sink);
        deliver_network_settings(&mut source, &mut state, &mut netstack, &sink);
        assert_eq!(*netstack.applied.borrow(), alloc::vec![shipped, edited]);
        assert_eq!(
            sink.ids().as_slice(),
            &[
                events::NETWORK_SETTINGS_DELIVERED.0,
                events::NETWORK_SETTINGS_DELIVERED.0
            ]
        );
    }

    #[test]
    fn a_refused_delivery_is_retried() {
        let policy = settings(false, true, false, false, false, false);
        let mut source = ScriptedSource::new(alloc::vec![Some(policy), Some(policy)]);
        let mut state = NetConfigState::new();
        // First apply refused (stack not up yet), second accepted.
        let mut netstack = RecordingNetstack::new(alloc::vec![Ok(()), Err(Errno::NotConnected)]);
        let sink = RecordingSink::new();
        deliver_network_settings(&mut source, &mut state, &mut netstack, &sink);
        assert!(!state.is_delivered(), "a refused delivery is not recorded");
        deliver_network_settings(&mut source, &mut state, &mut netstack, &sink);
        assert!(state.is_delivered(), "retried and delivered");
        assert_eq!(*netstack.applied.borrow(), alloc::vec![policy, policy]);
        assert_eq!(
            sink.ids().as_slice(),
            &[
                events::NETWORK_SETTINGS_DELIVERY_FAILED.0,
                events::NETWORK_SETTINGS_DELIVERED.0
            ]
        );
    }

    // --- Per-interface configuration delivery (N9b-3-1) -----------------

    /// A scripted per-interface config source: hands out a queued `load`
    /// result per call.
    struct ScriptedIfSource {
        results: RefCell<Vec<Option<InterfaceConfigPlan>>>,
    }

    impl ScriptedIfSource {
        fn new(results: Vec<Option<InterfaceConfigPlan>>) -> Self {
            Self {
                results: RefCell::new(results),
            }
        }
    }

    impl NetworkInterfaceConfigSource for ScriptedIfSource {
        fn load(&mut self) -> Option<InterfaceConfigPlan> {
            self.results.borrow_mut().pop().flatten()
        }
    }

    fn iface_name(text: &str) -> [u8; IF_NAME_LEN] {
        let mut out = [0u8; IF_NAME_LEN];
        out[..text.len()].copy_from_slice(text.as_bytes());
        out
    }

    /// NICs with these MACs and hardware locations, each bound as the
    /// device manager binds a channel its driver publishes beneath its node.
    fn bound(nics: &[([u8; 6], u64)]) -> NetBindState {
        use tairix_abi::hwtree::{HwDeviceClass, HwMatchKey, HwNode, HwResource, HW_NODE_ROOT};
        let mut nodes = Vec::new();
        for (at, &(_, location)) in (0u32..).zip(nics) {
            let mut device = HwNode::new(2 * at + 1, HW_NODE_ROOT, HwDeviceClass::Network);
            device
                .push_resource(HwResource::mmio(location, 0x1000))
                .expect("room");
            let mut channel = HwNode::new(2 * at + 2, device.id(), HwDeviceClass::Network);
            channel
                .push_match_key(
                    HwMatchKey::compatible(
                        tairix_abi::driver::net_channel::NETCHAN_NODE_COMPATIBLE,
                    )
                    .expect("key"),
                )
                .expect("room");
            channel
                .push_resource(HwResource::endpoint(0x100 + u64::from(at)))
                .expect("room");
            nodes.push(device);
            nodes.push(channel);
        }
        let mut stack = RecordingNetstack::new(Vec::new());
        *stack.macs.borrow_mut() = nics.iter().rev().map(|&(mac, _)| mac).collect();
        let mut state = NetBindState::new();
        crate::netbind::bind_new_channels(&nodes, &mut state, &mut stack, &RecordingSink::new());
        assert_eq!(state.nics().count(), nics.len(), "every NIC binds");
        state
    }

    const WAN_MAC: [u8; 6] = [1, 2, 3, 4, 5, 6];
    const LAN_MAC: [u8; 6] = [1, 2, 3, 4, 5, 7];

    fn a_config(alias: &str, mac: [u8; 6]) -> NetInterfaceConfigMsg {
        NetInterfaceConfigMsg {
            alias: iface_name(alias),
            match_mac: Some(mac),
            match_node: None,
            ipv4: tairix_abi::net_ipc::NetIpv4Config::Disabled,
            ipv6: tairix_abi::net_ipc::NetIpv6Config::Slaac,
            mtu: 0,
            dns: tairix_abi::net_ipc::NetDnsServers::EMPTY,
        }
    }

    #[test]
    fn an_absent_interface_config_store_is_quiet_and_retries() {
        let mut source = ScriptedIfSource::new(alloc::vec![None]);
        let mut state = NetIfConfigState::new();
        let mut netstack = RecordingNetstack::new(Vec::new());
        let sink = RecordingSink::new();
        let nics = bound(&[(WAN_MAC, 0)]);
        deliver_interface_configs(&mut source, &mut state, &nics, &mut netstack, &sink);
        assert!(netstack.ifconfigs.borrow().is_empty(), "nothing pushed");
        assert!(sink.ids().is_empty(), "the early state is quiet");
    }

    /// A configuration is offered once its NIC binds, however many tree
    /// changes come first, and is accepted at once.
    #[test]
    fn a_configuration_whose_nic_binds_late_is_offered_once_and_accepted() {
        let plan = InterfaceConfigPlan {
            messages: alloc::vec![a_config("wan", WAN_MAC)],
            bonds: Vec::new(),
            rejected: Vec::new(),
        };
        let mut source = ScriptedIfSource::new(alloc::vec![Some(plan)]);
        let mut state = NetIfConfigState::new();
        let mut netstack = RecordingNetstack::new(Vec::new());
        let sink = RecordingSink::new();
        let unbound = NetBindState::new();
        for _ in 0..3 {
            deliver_interface_configs(&mut source, &mut state, &unbound, &mut netstack, &sink);
        }
        assert!(
            netstack.ifconfigs.borrow().is_empty(),
            "nothing to offer to"
        );
        assert!(sink.ids().is_empty());

        let nics = bound(&[(WAN_MAC, 0)]);
        deliver_interface_configs(&mut source, &mut state, &nics, &mut netstack, &sink);
        deliver_interface_configs(&mut source, &mut state, &nics, &mut netstack, &sink);
        assert_eq!(netstack.ifconfigs.borrow().len(), 1, "offered once");
        assert_eq!(
            sink.ids().as_slice(),
            &[events::NETWORK_IFCONFIG_DELIVERED.0]
        );
    }

    /// The Pi 4 image configures a QEMU NIC by MAC beside the Pi's own by
    /// node: the one never present is never offered, so nothing is refused.
    #[test]
    fn a_configuration_whose_nic_never_binds_is_never_offered() {
        let mut wan = a_config("wan", WAN_MAC);
        wan.match_mac = None;
        wan.match_node = Some(0xfd58_0000);
        let plan = InterfaceConfigPlan {
            messages: alloc::vec![wan, a_config("vwan", LAN_MAC)],
            bonds: Vec::new(),
            rejected: Vec::new(),
        };
        let mut source = ScriptedIfSource::new(alloc::vec![Some(plan)]);
        let mut state = NetIfConfigState::new();
        let mut netstack = RecordingNetstack::new(Vec::new());
        let sink = RecordingSink::new();
        let nics = bound(&[(WAN_MAC, 0xfd58_0000)]);
        for _ in 0..4 {
            deliver_interface_configs(&mut source, &mut state, &nics, &mut netstack, &sink);
        }
        assert_eq!(
            *netstack.ifconfigs.borrow(),
            alloc::vec![wan],
            "by its node"
        );
        assert_eq!(
            sink.ids().as_slice(),
            &[events::NETWORK_IFCONFIG_DELIVERED.0]
        );
    }

    /// A refusal is the stack's answer: it is recorded once and offered again
    /// only when something changes, never on a timer.
    #[test]
    fn a_refused_configuration_waits_for_its_entry_or_another_nic() {
        let plan = InterfaceConfigPlan {
            messages: alloc::vec![a_config("wan", WAN_MAC)],
            bonds: Vec::new(),
            rejected: Vec::new(),
        };
        let mut source = ScriptedIfSource::new(alloc::vec![Some(plan)]);
        let mut state = NetIfConfigState::new();
        let mut netstack = RecordingNetstack::with_ifconfig_results(alloc::vec![
            Err(Errno::AlreadyExists),
            Ok(())
        ]);
        let sink = RecordingSink::new();
        let one = bound(&[(WAN_MAC, 0)]);
        for _ in 0..3 {
            deliver_interface_configs(&mut source, &mut state, &one, &mut netstack, &sink);
        }
        assert_eq!(
            netstack.ifconfigs.borrow().len(),
            1,
            "refused once, not retried"
        );
        assert_eq!(
            sink.ids().as_slice(),
            &[events::NETWORK_IFCONFIG_DELIVERY_FAILED.0]
        );

        let two = bound(&[(WAN_MAC, 0), (LAN_MAC, 0)]);
        deliver_interface_configs(&mut source, &mut state, &two, &mut netstack, &sink);
        assert_eq!(netstack.ifconfigs.borrow().len(), 2, "another NIC bound");
        assert_eq!(
            sink.ids().last(),
            Some(&events::NETWORK_IFCONFIG_DELIVERED.0)
        );
    }

    #[test]
    fn a_managed_interface_without_match_mac_is_rejected_once() {
        let plan = InterfaceConfigPlan {
            messages: Vec::new(),
            bonds: Vec::new(),
            rejected: alloc::vec![iface_name("wan")],
        };
        let mut source = ScriptedIfSource::new(alloc::vec![Some(plan)]);
        let mut state = NetIfConfigState::new();
        let mut netstack = RecordingNetstack::new(Vec::new());
        let sink = RecordingSink::new();
        let nics = NetBindState::new();
        deliver_interface_configs(&mut source, &mut state, &nics, &mut netstack, &sink);
        deliver_interface_configs(&mut source, &mut state, &nics, &mut netstack, &sink);
        assert_eq!(
            sink.ids().as_slice(),
            &[events::NETWORK_IFCONFIG_REJECTED.0],
            "the config error is surfaced loud exactly once"
        );
        assert!(netstack.ifconfigs.borrow().is_empty());
    }

    #[test]
    fn an_edited_document_is_re_delivered_and_an_untouched_interface_is_not() {
        let wan = a_config("wan", WAN_MAC);
        let lan = a_config("lan", LAN_MAC);
        let mut edited = wan;
        edited.mtu = 9000;
        // Read in reverse: the source pops from the back.
        let mut source = ScriptedIfSource::new(alloc::vec![
            Some(InterfaceConfigPlan {
                messages: alloc::vec![edited, lan],
                bonds: Vec::new(),
                rejected: Vec::new(),
            }),
            Some(InterfaceConfigPlan {
                messages: alloc::vec![wan, lan],
                bonds: Vec::new(),
                rejected: Vec::new(),
            }),
        ]);
        let mut state = NetIfConfigState::new();
        let mut netstack = RecordingNetstack::new(Vec::new());
        let sink = RecordingSink::new();
        let nics = bound(&[(WAN_MAC, 0), (LAN_MAC, 0)]);

        deliver_interface_configs(&mut source, &mut state, &nics, &mut netstack, &sink);
        assert_eq!(netstack.ifconfigs.borrow().len(), 2, "both delivered");

        // The administrator edits `wan`'s MTU while the machine runs. The
        // plan is re-read, so the change reaches the stack — and `lan`, which
        // the edit did not touch, is not pushed a second time.
        deliver_interface_configs(&mut source, &mut state, &nics, &mut netstack, &sink);
        let pushed = netstack.ifconfigs.borrow().clone();
        assert_eq!(pushed.len(), 3, "only the edited interface is re-pushed");
        assert_eq!(pushed[2], edited);
    }

    #[test]
    fn a_newly_rejected_interface_is_surfaced_when_the_document_changes() {
        let clean = InterfaceConfigPlan::default();
        let broken = InterfaceConfigPlan {
            messages: Vec::new(),
            bonds: Vec::new(),
            rejected: alloc::vec![iface_name("wan")],
        };
        let mut source = ScriptedIfSource::new(alloc::vec![Some(broken), Some(clean)]);
        let mut state = NetIfConfigState::new();
        let mut netstack = RecordingNetstack::new(Vec::new());
        let sink = RecordingSink::new();
        let nics = NetBindState::new();

        deliver_interface_configs(&mut source, &mut state, &nics, &mut netstack, &sink);
        assert!(sink.ids().is_empty(), "nothing to reject yet");

        deliver_interface_configs(&mut source, &mut state, &nics, &mut netstack, &sink);
        assert_eq!(
            sink.ids().as_slice(),
            &[events::NETWORK_IFCONFIG_REJECTED.0],
            "an edit that breaks an interface is surfaced, not swallowed"
        );
    }

    #[test]
    fn a_bond_is_delivered_after_its_members() {
        let text = "\
bond0.kind bond
bond0.bond.members eth0,eth1
eth0.match.mac 02:00:00:00:00:02
eth1.match.mac 02:00:00:00:00:03
";
        let config = tairix_netconfig::NetworkConfig::parse(text).expect("parses");
        let plan = InterfaceConfigPlan::of(&config);
        let mut source = ScriptedIfSource::new(alloc::vec![Some(plan)]);
        let mut state = NetIfConfigState::new();
        let mut netstack = RecordingNetstack::new(Vec::new());
        let sink = RecordingSink::new();
        let members = bound(&[([2, 0, 0, 0, 0, 2], 0), ([2, 0, 0, 0, 0, 3], 0)]);
        deliver_interface_configs(&mut source, &mut state, &members, &mut netstack, &sink);
        // The bond's own addressing and both member renames were delivered.
        assert_eq!(
            netstack.ifconfigs.borrow().len(),
            3,
            "bond addressing + two member renames"
        );
        assert_eq!(netstack.bonds.borrow().len(), 1, "the bond composed");
        assert_eq!(netstack.bonds.borrow()[0].alias, iface_name("bond0"));
    }
}
