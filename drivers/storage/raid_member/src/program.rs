//! The freestanding body of the RAID member-agent driver's `Run` binary
//! (`main.rs`): the member agent's grant resolution, delegation, and
//! membership loop (`plans/FIX-IO.md` `IO6c`).
//!
//! # Delegate, offer, hold
//!
//! The agent's whole job is to put *one* device's transport into the hands of
//! the process that composes arrays, and to keep doing so for as long as the
//! device is here. Each turn it delegates its two granted resources to the
//! composer's reserved rendezvous and posts an offer naming them; the composer
//! answers only when the membership ends, so the agent then parks on that one
//! reply. It never touches the device: it holds the grants and forwards them,
//! and the composer does the reading.
//!
//! Every wait is an event: the reply wakes the agent, the composer's endpoint
//! being torn down cancels the call and wakes it too, and the only timed wait
//! is the paced re-offer when no composer is listening yet. The rendezvous's
//! replies are observed only while a membership is outstanding, so the paced
//! wait is the timer's alone. Nothing here polls the rendezvous and nothing
//! polls the disk.

use tairix_abi::hwtree::HwResourceKind;
use tairix_abi::raid_ipc::{MemberOffer, MembershipEnd, RAID_REGISTRY_ENDPOINT};
use tairix_abi::reply::STATUS_REPLY_LEN;
use tairix_abi::waitset::WaitSourceKind;
use tairix_abi::Errno;
use tairix_drv_storage_raid_member::{AgentStep, MemberAgent, Transport};
use tairix_drvrt::exit::{end, DriverExit};
use tairix_drvrt::{RtDriverHost, RtGrantSyscalls};
use tairix_log::{log, Event, EventId, Field, FieldValue, Level};
use tairix_rt::{LogSink, WaitSet};
use tairix_util::fmt::format_hex_u64;

/// Diagnostic event id: the device was offered to the composer and the
/// membership is being held open.
const RAID_MEMBER_OFFERED: EventId = EventId(28_186);

/// Diagnostic event id: no composer is listening on the rendezvous yet, so
/// the offer is paced and made again.
const RAID_NO_COMPOSER: EventId = EventId(28_187);

/// Diagnostic event id: the composer released this member — the array was
/// torn down, or the device was removed from it.
const RAID_MEMBER_RELEASED: EventId = EventId(28_188);

/// Diagnostic event id: the composer refused this device; the agent stops.
const RAID_MEMBER_REFUSED: EventId = EventId(28_189);

/// Diagnostic event id: the composer could not take this device on yet (a
/// previous membership for it has still to end, or it had no room); the agent
/// offers again on its paced cadence.
const RAID_MEMBER_DEFERRED: EventId = EventId(28_114);

/// Emit one diagnostic record carrying a single hex-rendered field.
fn log_hex_event(id: EventId, level: Level, message: &'static str, key: &'static str, value: u64) {
    let mut value_buf = [0u8; 16];
    log(
        &LogSink,
        &Event {
            level,
            id,
            message,
            fields: &[Field {
                key,
                value: FieldValue::Str(format_hex_u64(value, &mut value_buf)),
            }],
        },
    );
}

/// Resolve the member's transport from the grants its matched node carried, or
/// [`None`] when the node did not carry both.
fn resolve_transport<S: tairix_drvrt::GrantSyscalls>(host: &RtDriverHost<S>) -> Option<Transport> {
    let endpoint = host.endpoint_grant()?;
    let window = host
        .resources()
        .find(|resource| resource.kind() == Some(HwResourceKind::Shared))?
        .base();
    Some(Transport { endpoint, window })
}

/// Delegate the device's transport to the rendezvous and post one offer,
/// returning the membership ticket.
///
/// Both delegations are repeated on every offer: a composer that restarted is
/// a new task holding none of the previous one's grants, and re-granting a
/// resource a recipient already holds hands back the handle it has rather than
/// minting a second, so repeating costs nothing and re-arms everything.
fn offer(transport: &Transport, node: u32) -> Result<u64, Errno> {
    let granted = tairix_rt::call_grant(transport.endpoint, RAID_REGISTRY_ENDPOINT);
    if granted < 0 {
        return Err(Errno::from_syscall(granted));
    }
    let request = transport.offer(
        tairix_rt::shm_grant(transport.window, RAID_REGISTRY_ENDPOINT),
        node,
    )?;
    let mut frame = [0u8; MemberOffer::WIRE_LEN];
    let len = request.encode(&mut frame)?;
    // No deadline: the membership lasts as long as the array holds the device,
    // and the composer going away cancels the call and wakes the agent, so
    // there is no wedge for a deadline to break.
    tairix_rt::call_post(RAID_REGISTRY_ENDPOINT, &frame[..len], u64::MAX)
        .map_err(Errno::from_syscall)
}

/// Claim the membership's outcome, parking until it is known on a set that
/// observes the rendezvous's replies for this membership alone: kept past it,
/// a member whose endpoint ended would stay ready and wake every later park.
fn await_end(ticket: u64) -> MembershipEnd {
    let watched = WaitSet::create().and_then(|set| {
        set.add(WaitSourceKind::CallReply, RAID_REGISTRY_ENDPOINT, 0)?;
        Ok(set)
    });
    let Ok(set) = watched else {
        // A membership whose end could never be heard is withdrawn.
        let _ = tairix_rt::call_cancel(RAID_REGISTRY_ENDPOINT, ticket);
        return MembershipEnd::ComposerGone;
    };
    let mut reply = [0u8; STATUS_REPLY_LEN];
    match tairix_rt::call_await(set.handle(), RAID_REGISTRY_ENDPOINT, ticket, &mut reply) {
        Ok(len) => MembershipEnd::from_reply(Some(&reply[..len])),
        // Anything else retires the ticket: the composer went away, or the
        // reply could not be claimed, and the agent re-offers.
        Err(_) => MembershipEnd::ComposerGone,
    }
}

fn main() -> i32 {
    let caps = tairix_drv_storage_raid_member::REQUIRED_CAPABILITIES
        .iter()
        .copied()
        .collect();
    let Ok(host) = RtDriverHost::from_grants_query(caps, RtGrantSyscalls, None) else {
        return end(DriverExit::NoHost, &[]);
    };
    let Some(transport) = resolve_transport(&host) else {
        return end(DriverExit::NoResources, &[]);
    };
    // The agent's own node, so the composer can name this member in its audit
    // trail. Kernel-resolved from the calling task, never claimed; a node the
    // kernel will not name is simply unnamed in the record.
    let node = tairix_rt::hw_self_node().map_or(0, |record| record.node());

    let mut agent = MemberAgent::new();
    // The outstanding membership's ticket, held only between the offer that
    // minted it and the claim that consumes it.
    let mut ticket: Option<u64> = None;
    loop {
        let now = tairix_rt::clock_get();
        match agent.next_step(now) {
            AgentStep::Offer => match offer(&transport, node) {
                Ok(minted) => {
                    ticket = Some(minted);
                    agent.note_offered(true, now);
                    log_hex_event(
                        RAID_MEMBER_OFFERED,
                        Level::Info,
                        "raid: member offered to the array composer",
                        "endpoint_hex",
                        transport.endpoint,
                    );
                }
                Err(errno) => {
                    agent.note_offered(false, now);
                    log_hex_event(
                        RAID_NO_COMPOSER,
                        Level::Info,
                        "raid: no array composer listening yet; re-offering",
                        "errno_hex",
                        errno as u64,
                    );
                }
            },
            AgentStep::AwaitReply => {
                // A membership with no ticket cannot be claimed, so it is over
                // as far as this agent can tell; re-offering is the only way
                // back and is what `ComposerGone` asks for.
                let end = match ticket.take() {
                    Some(held) => await_end(held),
                    None => MembershipEnd::ComposerGone,
                };
                agent.note_end(end, tairix_rt::clock_get());
                log_membership_end(end, transport.endpoint);
            }
            AgentStep::Retry { deadline_ns } => tairix_rt::park_ns(deadline_ns.saturating_sub(now)),
            // The composer read this device and will not compose it. Exiting
            // is the honest end: the verdict came from the device's own
            // metadata, and `devmgr` will start a fresh agent if the device —
            // and so its node — ever comes back.
            AgentStep::Stop => return 0,
        }
    }
}

/// Record how one membership ended, so a member disk's history is readable
/// from the log whether the array released it, refused it, or lost its
/// composer.
fn log_membership_end(end: MembershipEnd, endpoint: u64) {
    match end {
        MembershipEnd::Released => log_hex_event(
            RAID_MEMBER_RELEASED,
            Level::Info,
            "raid: member released by the array composer",
            "endpoint_hex",
            endpoint,
        ),
        MembershipEnd::Deferred(errno) => log_hex_event(
            RAID_MEMBER_DEFERRED,
            Level::Info,
            "raid: array composer cannot take this device yet; re-offering",
            "errno_hex",
            errno as u64,
        ),
        MembershipEnd::Refused(errno) => log_hex_event(
            RAID_MEMBER_REFUSED,
            Level::Warn,
            "raid: array composer refused this device",
            "errno_hex",
            errno as u64,
        ),
        MembershipEnd::ComposerGone => log_hex_event(
            RAID_NO_COMPOSER,
            Level::Warn,
            "raid: array composer went away; re-offering",
            "endpoint_hex",
            endpoint,
        ),
    }
}

tairix_rt::entry!(main);
