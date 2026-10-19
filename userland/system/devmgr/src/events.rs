//! Stable [`tairix_log::EventId`] constants emitted by the device
//! manager.
//!
//! Per `lib/log` convention every subsystem owns a
//! 1 000-wide reserved range. The device manager occupies
//! `13000..14000`. Once shipped the numeric values must never be
//! re-used or re-numbered — external audit-log consumers rely on them.

use tairix_log::{log, Event, EventId, Field, FieldValue, Level, Sink};
use tairix_util::fmt::format_hex_u64;

/// Range start (inclusive) reserved for `devmgr` event identifiers.
///
/// Exposed so audit consumers can filter by subsystem in O(1) instead of
/// matching on individual event identifiers.
pub const DEVMGR_RANGE_START: u32 = tairix_log::registry::DEVMGR.start();
/// Range end (exclusive) reserved for `devmgr` event identifiers.
pub const DEVMGR_RANGE_END: u32 = tairix_log::registry::DEVMGR.end();

/// A hardware-tree node was bound: its winning driver is loaded.
pub const NODE_BOUND: EventId = EventId(13_001);
/// A hardware-tree node matched no driver bind table and was left
/// unbound — never an error. Emitted at `Debug`: an
/// unmatched node is the routine, high-volume case (most nodes on a real
/// device tree have no driver), so it is filtered out by the default
/// `Info` threshold and never floods the slow diagnostic UART — lower the
/// level to trace which nodes were left unbound.
/// A *binding*, a packaging tie, or a load refusal stays visible.
pub const NODE_UNBOUND: EventId = EventId(13_002);
/// Two or more drivers matched a node at the same highest priority; the
/// unbroken tie is a packaging defect, so the node is refused a binding.
pub const NODE_TIE_REJECTED: EventId = EventId(13_003);
/// A node's winning driver failed to load through the driver-host load
/// gate; the node stays unbound (fail closed).
pub const NODE_LOAD_FAILED: EventId = EventId(13_004);
/// The read-only `/System` driver-store catalogue could not be fetched (the
/// store endpoint is unbound or the store is unreadable). The device
/// manager loads nothing but keeps observing the hardware tree — never an
/// error (fail-soft).
pub const DRIVER_STORE_UNAVAILABLE: EventId = EventId(13_005);
/// A hardware-tree snapshot was read: its generation and node count. Emitted
/// at `Debug` (verbose boot/hotplug diagnostics, filtered out by default).
pub const TREE_OBSERVED: EventId = EventId(13_006);
/// One node of the observed hardware tree: its id, parent, class, and match-
/// key count. Emitted at `Debug`, so one record per node is filtered out on a
/// default-`Info` boot and only a build running this service below `Info`
/// observes it.
pub const NODE_OBSERVED: EventId = EventId(13_007);
/// A bound driver was unloaded because its hardware-tree node vanished
/// (hot-removal). The mirror of [`NODE_BOUND`]: the device
/// manager diffed a generation bump, found a bound node gone, and asked the
/// kernel to tear its driver down. Carries the unbound `node` and the torn-
/// down driver `handle`.
pub const NODE_UNLOADED: EventId = EventId(13_008);
/// The reactive observe loop ended because a hardware-tree seam operation
/// (`hw_tree_read` / `hw_tree_wait` / snapshot decode) reported an error the
/// loop fails closed on. Carries the `errno` so the failing seam is
/// diagnosable from the log; the service then exits non-zero and PID 1
/// `init` supervises the relaunch. A silent exit would hide which seam
/// refused — every abnormal exit states its reason.
pub const TREE_SEAM_FAILED: EventId = EventId(13_009);
/// A discovered NIC device-channel node (`compatible = "tairix,netchan"`,
/// emitted by a bound NIC driver process) was handed to the network stack:
/// the device manager `ipc_call`ed `netstack` `BindDriver` with the node's
/// endpoint and a derived interface alias, and the stack accepted it.
pub const NETSTACK_BOUND: EventId = EventId(13_010);
/// A NIC device-channel node was observed but could not be handed to the
/// network stack (the stack refused the bind, or its endpoint was
/// unreachable). The channel stays unbound and the hand-off is retried on
/// the next generation bump — never an error (fail-soft, like the driver
/// store being unavailable).
pub const NETSTACK_BIND_FAILED: EventId = EventId(13_011);
/// The stack-wide `net.*` policy read from `system.conf` was delivered to
/// the network stack and accepted (the `ApplyNetworkSettings` admin call
/// succeeded). Recorded at `Info`: a one-time boot configuration step.
pub const NETWORK_SETTINGS_DELIVERED: EventId = EventId(13_012);
/// The `net.*` policy was read but the network stack refused it or was
/// unreachable (its admin endpoint not yet bound). Retried on the next
/// generation bump — never an error (fail-soft, like the driver store
/// being unavailable). Recorded at `Warn`.
pub const NETWORK_SETTINGS_DELIVERY_FAILED: EventId = EventId(13_013);
/// A per-interface `network.conf` configuration was delivered to the
/// network stack and accepted (the `NetInterfaceConfigMsg` admin call
/// succeeded). Recorded at `Info`: an interface's addressing came up.
pub const NETWORK_IFCONFIG_DELIVERED: EventId = EventId(13_014);
/// The network stack refused a per-interface configuration or a bond offered
/// to an interface it holds. Offered again once its `network.conf` entry
/// changes or another NIC binds, never on a timer. Recorded at `Warn`.
pub const NETWORK_IFCONFIG_DELIVERY_FAILED: EventId = EventId(13_015);
/// A managed ethernet interface in `network.conf` declares neither a
/// `match.mac` nor a `match.node` identity selector, so the device manager
/// cannot bind it to hardware by stable identity. The interface is skipped
/// and the operator's configuration error is surfaced loud, once. Recorded
/// at `Warn`.
pub const NETWORK_IFCONFIG_REJECTED: EventId = EventId(13_016);

/// A readable `/System/Settings/` configuration document could not be
/// parsed, so nothing was delivered from it.
///
/// A settings document the engine cannot fully understand is refused whole
/// rather than half-applied; the `path` names the layer that was read.
/// Recorded at `Warn`: staying on the safe defaults is correct, but silently
/// ignoring the document is not — without this it would simply appear to
/// have no effect. (The settings service renders canonically, so this
/// surfaces an image built wrong or a document damaged on disk.)
pub const CONFIG_DOCUMENT_MALFORMED: EventId = EventId(13_017);

/// A discovered audio device-channel node (`compatible = "tairix,audiochan"`,
/// emitted by a bound audio driver process) was handed to the audio service:
/// the device manager `ipc_call`ed `audiod` `BindDriver` with the node's
/// endpoint and the service adopted it.
pub const AUDIOD_BOUND: EventId = EventId(13_018);
/// An audio device-channel node was observed but could not be handed to the
/// audio service (it refused the bind, or its rendezvous was unreachable).
/// The channel stays unbound and the hand-off is retried on the next
/// generation bump — never an error (fail-soft, like the driver store being
/// unavailable).
pub const AUDIOD_BIND_FAILED: EventId = EventId(13_019);

/// A matched node left the hardware tree before its driver was admitted, so
/// nothing was loaded for it. A hot-removal racing the load, not a refusal:
/// recorded at `Info`, carrying the `errno`.
pub const NODE_LOAD_RACED_REMOVAL: EventId = EventId(13_020);
/// A matched node's load was refused because another live driver already
/// holds the node. The node is driven, just not by an instance this service
/// loaded: recorded at `Info`, carrying the `errno`.
pub const NODE_ALREADY_DRIVEN: EventId = EventId(13_021);
/// A matched node waits for a supplier its links name, which is in the tree
/// and matched but not yet bound, or not in the tree at all. Routine at boot,
/// so recorded at `Debug`, carrying the `supplier` node id.
pub const NODE_HELD: EventId = EventId(13_022);
/// An audio device channel the audio service had adopted left the hardware
/// tree — its driver exited or its device was removed — so the service was
/// told to retire it, and the endpoint is free to be handed over again.
pub const AUDIOD_UNBOUND: EventId = EventId(13_023);
/// The machine's audio baseline read from the configuration store was
/// delivered to the audio service.
pub const AUDIO_BASELINE_DELIVERED: EventId = EventId(13_024);
/// The audio baseline could not be delivered: the service refused it or was
/// unreachable. Retried on the next reaction.
pub const AUDIO_BASELINE_DELIVERY_FAILED: EventId = EventId(13_025);
/// The hardware present at boot has settled: matched, its drivers settled and
/// its channels handed on. The device manager announces its readiness.
pub const DEVICES_SETTLED: EventId = EventId(13_026);
/// A device was waited on for the whole settle budget without settling; the
/// boot settles without it. `node` names it, or `channels` for the hand-offs.
pub const SETTLE_LAPSED: EventId = EventId(13_027);
/// The service manager did not record the device manager's readiness.
pub const READINESS_REFUSED: EventId = EventId(13_028);
/// The driver bound to `node` ended and is not reloaded; the node is left
/// unbound and holds no consumer. Carries the `ending` the kernel recorded,
/// and the `restarts` spent when a restart budget ran out. Recorded at
/// `Info`: the driver stated why it ended.
pub const NODE_DRIVER_ENDED: EventId = EventId(13_029);
/// The driver bound to `node` ended for a failure a fresh instance may not
/// meet, so it is reloaded after `delay_ms`, restart `attempt` of the
/// node's budget. Recorded at `Warn`.
pub const NODE_DRIVER_RESTARTING: EventId = EventId(13_030);
/// The administrator's layer of a settings document exists but could not be
/// read, so nothing is delivered from either layer: falling back to the
/// image's would act on settings the administrator replaced. Carries the
/// `path` and the `errno`. Recorded at `Warn`.
pub const CONFIG_DOCUMENT_UNREADABLE: EventId = EventId(13_031);

/// A decision about one node, so every one is recorded with its own id and
/// message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NodeEvent {
    Bound,
    Unbound,
    TieRejected,
    LoadFailed,
    LoadRacedRemoval,
    AlreadyDriven,
    Unloaded,
    Held,
    DriverEnded,
    DriverRestarting,
}

impl NodeEvent {
    #[cfg(test)]
    const ALL: [Self; 10] = [
        Self::Bound,
        Self::Unbound,
        Self::TieRejected,
        Self::LoadFailed,
        Self::LoadRacedRemoval,
        Self::AlreadyDriven,
        Self::Unloaded,
        Self::Held,
        Self::DriverEnded,
        Self::DriverRestarting,
    ];

    const fn id(self) -> EventId {
        match self {
            Self::Bound => NODE_BOUND,
            Self::Unbound => NODE_UNBOUND,
            Self::TieRejected => NODE_TIE_REJECTED,
            Self::LoadFailed => NODE_LOAD_FAILED,
            Self::LoadRacedRemoval => NODE_LOAD_RACED_REMOVAL,
            Self::AlreadyDriven => NODE_ALREADY_DRIVEN,
            Self::Unloaded => NODE_UNLOADED,
            Self::Held => NODE_HELD,
            Self::DriverEnded => NODE_DRIVER_ENDED,
            Self::DriverRestarting => NODE_DRIVER_RESTARTING,
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::Bound => "node bound to driver",
            Self::Unbound => "node left unbound: no matching driver",
            Self::TieRejected => "node refused: unbroken bind-priority tie",
            Self::LoadFailed => "node load failed: the load gate refused",
            Self::LoadRacedRemoval => "node left the tree before its driver loaded",
            Self::AlreadyDriven => "node already held by a live driver",
            Self::Unloaded => "driver unloaded: its node or a supplier vanished",
            Self::Held => "node held until its supplier serves",
            Self::DriverEnded => "node left unbound: its driver ended",
            Self::DriverRestarting => "node's driver failed; reloading it",
        }
    }
}

/// The most fields a node decision carries beyond its node.
const NODE_EVENT_DETAIL: usize = 3;

/// Record `event` about `node` at `level` with up to [`NODE_EVENT_DETAIL`]
/// fields of `extra`; fields past those are dropped rather than overrun.
pub(crate) fn audit_node(
    sink: &dyn Sink,
    event: NodeEvent,
    level: Level,
    node: u32,
    extra: &[Field<'_>],
) {
    let mut nbuf = [0u8; 16];
    let mut fields = [Field {
        key: "node",
        value: FieldValue::Str(format_hex_u64(u64::from(node), &mut nbuf)),
    }; 1 + NODE_EVENT_DETAIL];
    let mut len = 1;
    for (slot, field) in fields[1..].iter_mut().zip(extra) {
        *slot = *field;
        len += 1;
    }
    log(
        sink,
        &Event {
            level,
            id: event.id(),
            message: event.message(),
            fields: &fields[..len],
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsink::RecordingSink;

    const ALL: [EventId; 31] = [
        NODE_BOUND,
        NODE_UNBOUND,
        NODE_TIE_REJECTED,
        NODE_LOAD_FAILED,
        DRIVER_STORE_UNAVAILABLE,
        TREE_OBSERVED,
        NODE_OBSERVED,
        NODE_UNLOADED,
        TREE_SEAM_FAILED,
        NETSTACK_BOUND,
        CONFIG_DOCUMENT_MALFORMED,
        NETSTACK_BIND_FAILED,
        NETWORK_SETTINGS_DELIVERED,
        NETWORK_SETTINGS_DELIVERY_FAILED,
        NETWORK_IFCONFIG_DELIVERED,
        NETWORK_IFCONFIG_DELIVERY_FAILED,
        NETWORK_IFCONFIG_REJECTED,
        AUDIOD_BOUND,
        AUDIOD_BIND_FAILED,
        NODE_LOAD_RACED_REMOVAL,
        NODE_ALREADY_DRIVEN,
        NODE_HELD,
        AUDIOD_UNBOUND,
        AUDIO_BASELINE_DELIVERED,
        AUDIO_BASELINE_DELIVERY_FAILED,
        DEVICES_SETTLED,
        SETTLE_LAPSED,
        READINESS_REFUSED,
        NODE_DRIVER_ENDED,
        NODE_DRIVER_RESTARTING,
        CONFIG_DOCUMENT_UNREADABLE,
    ];

    #[test]
    fn ids_are_inside_reserved_range() {
        for id in ALL {
            assert!(id.0 >= DEVMGR_RANGE_START && id.0 < DEVMGR_RANGE_END);
        }
    }

    #[test]
    fn ids_are_unique() {
        let mut ids = ALL.map(|id| id.0);
        ids.sort_unstable();
        for w in ids.windows(2) {
            assert_ne!(w[0], w[1], "duplicate devmgr EventId");
        }
    }

    #[test]
    fn every_node_decision_has_its_own_id_and_message() {
        for (at, event) in NodeEvent::ALL.iter().enumerate() {
            assert!(ALL.contains(&event.id()));
            for other in &NodeEvent::ALL[at + 1..] {
                assert_ne!(event.id(), other.id());
                assert_ne!(event.message(), other.message());
            }
        }
    }

    #[test]
    fn a_node_decision_names_its_node_and_drops_detail_past_the_bound() {
        let field = |key| Field {
            key,
            value: FieldValue::Str("x"),
        };
        let sink = RecordingSink::new();
        audit_node(
            &sink,
            NodeEvent::DriverEnded,
            Level::Info,
            0x2a,
            &[field("a"), field("b"), field("c"), field("d")],
        );
        assert_eq!(sink.ids(), [NODE_DRIVER_ENDED.0]);
        let id = NODE_DRIVER_ENDED.0;
        assert_eq!(
            sink.field_of(id, "node").as_deref(),
            Some("000000000000002a")
        );
        assert!(sink.field_of(id, "c").is_some());
        assert!(sink.field_of(id, "d").is_none());
    }
}
