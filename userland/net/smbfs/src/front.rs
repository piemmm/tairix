//! The front: the `netshare-v1` calls, the connections they share, and the
//! mounts they made.
//!
//! The front never waits. A call that needs the network is held by its ticket
//! on the connection it needs and answered when that work completes, fails,
//! or outlives its deadline. Connections are shared per principal, endpoint
//! and account: listing a server's shares and then mounting two is one
//! connection and one session. A connection is made for the account a call
//! names and serves only calls naming the same account with the same
//! password, so no call rides a session it did not prove it may open.
//!
//! What a connection reports of its server — the GUID it negotiated with and
//! the address it was reached at — is the pin a remembered credential is held
//! to, checked before any credential leaves: a call whose pin differs is
//! answered [`NetshareError::PinMismatch`], and a connection no remaining call
//! needs is closed unauthenticated.
//!
//! Each connection's deadlines are one entry in an indexed heap, so the next
//! instant to wake at is read in constant time and only what is due is
//! visited.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write as _;
use core::net::IpAddr;

use tairix_abi::driver::filesystem::{NameMatching, NodeId, NodeIdentity, WriteRefusal};
use tairix_abi::fsserve::{Lanes, ServeOffer, FSSERVE_WINDOW_LEN};
use tairix_abi::net::{SocketAddr, SocketId, SocketStreamEvent};
use tairix_abi::net_ipc::{address_parts, is_unicast, IF_NAME_LEN};
use tairix_abi::netattach::{NetworkOffer, OfferId};
use tairix_abi::netshare_ipc::{
    Connect, DialectFloor, Endpoint, MountState, NetshareError, NetshareReply, NetshareRequest, Op,
    Pin, Records, ShareEntry, Shares, HOST_MAX, MOUNTS_MAX, SHARES_MAX, SHARE_NAME_BYTES,
};
use tairix_abi::sysinfo::MOUNT_SOURCE_MAX;
use tairix_abi::time::NANOS_PER_SEC;
use tairix_abi::volume::FormatName;
use tairix_abi::{Errno, FieldValue, ProcId};
use tairix_collections::{HashMap, IndexedHeap, SlotMap, TryReserveError};
use tairix_hash::{BuildFastHash, BuildSipHash13};
use tairix_inline::ArrayString;
use tairix_log::{Event, EventId, Field, Level, Sink};
use tairix_reclaim::{CacheBudget, PinnedAccounting, PinnedShare, PressureGauge};
use tairix_sandbox::session::SessionTransport;
use tairix_smb::crypt::NONCE_SEED_LEN;
use tairix_smb::negotiate::{Dialect, SALT_LEN};
use tairix_smb::ntlm::NtlmCredential;
use tairix_smb::rpc::Share;
use tairix_smb::status::NtStatus;
use tairix_smb::tree::ShareType;
use tairix_util::secret::wipe;
use tairix_writeback::WritebackBound;

use crate::events::{
    AUTHENTICATION_REJECTED, CONNECTION_ENDED, CONNECTION_LOST, MOUNTED, MOUNTS_LOST, PIN_MISMATCH,
    RECONNECTED, REQUEST_REFUSED, REQUEST_UNREADABLE, UNMOUNTED,
};
use crate::host::{Arming, ConnId, Host, Resolution, Unresolved};
use crate::judge;
use crate::link::{Link, LinkError};
use crate::mounts::{volume_key, Mount, Mounts};
use crate::nodes::Nodes;
use crate::wire::{Credential, FileOp, Leasing, Order, Report, REPORT_MAX};
use held::Holding;
use served::Retiring;

mod held;
mod meta;
mod served;
mod written;

use served::Volume;

/// A worker reported what no honest worker does.
pub(crate) struct Lie;

/// How long a call may wait for its answer: connecting, signing in, and the
/// server's reply included.
pub const CALL_DEADLINE_NS: u64 = 30 * NANOS_PER_SEC as u64;

/// How long a connection may take to be resolved, connected, negotiated and
/// signed in.
pub const SETUP_DEADLINE_NS: u64 = 30 * NANOS_PER_SEC as u64;

/// How long a connection nothing needs is kept, so the mount that follows a
/// listing finds it open.
pub const LINGER_NS: u64 = 30 * NANOS_PER_SEC as u64;

/// How long a connection its mounts rest on may go unheard before the server
/// is asked whether it still serves it, and the ask unanswered before the
/// connection is taken for dropped: a third of a call's deadline, so a dead
/// connection is replaced while the calls it holds still have time.
pub const ECHO_AFTER_NS: u64 = CALL_DEADLINE_NS / 3;

/// How long a dropped connection's mounts wait for it when the server made
/// none of their files durable: MS-SMB2's default durable timeout.
pub const DEFAULT_GRACE_NS: u64 = 60 * NANOS_PER_SEC as u64;

/// The wait before a dropped connection is tried again after an attempt
/// fails, doubling with each failure up to a call's deadline.
pub const RETRY_FIRST_NS: u64 = NANOS_PER_SEC as u64;

/// The longest wait between attempts.
pub const RETRY_MAX_NS: u64 = CALL_DEADLINE_NS;

/// What the front's caches are bounded by and answer to.
pub struct Caching {
    /// The machine's memory, of which each cache takes a share; none keeps
    /// nothing cached and holds nothing written.
    pub memory: usize,
    /// The process's memory-pressure gauge.
    pub pressure: &'static dyn PressureGauge,
    /// Where a cache's own audit records go.
    pub sink: &'static (dyn Sink + Sync),
    /// The machine-wide share every volume's unwritten bytes draw on.
    pub pinned: &'static PinnedShare,
}

impl Caching {
    /// A writable volume's bound on bytes written and not yet sent, or none
    /// where the machine's share cannot hold one write.
    fn bound(&self) -> Option<WritebackBound> {
        let pinned = Arc::try_new(PinnedAccounting::within(self.pinned)).ok()?;
        WritebackBound::new(
            CacheBudget::from_backing(self.memory),
            self.pressure,
            pinned,
        )
        .ok()
    }
}

/// The most connections one principal holds.
pub const CONNECTIONS_MAX: usize = 16;

/// The most calls one principal has waiting on the service at once.
pub const CALLS_MAX: usize = 16;

/// How a mounted share's calls are spread: four lanes of a quarter of the
/// window each, so four calls are worked on at once.
pub const LANES: Lanes = match Lanes::new(4, FSSERVE_WINDOW_LEN / 4) {
    Ok(lanes) => lanes,
    Err(_) => Lanes::ONE,
};

/// The node a mounted share's root is reported under.
pub const ROOT_NODE: NodeId = NodeId::from_raw(1);

/// A caller, as the kernel attested it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Caller {
    /// Its user.
    pub uid: u32,
    /// The login session it lies within.
    pub session: Option<ProcId>,
}

/// What a call asks of a connection.
enum Work {
    Shares {
        special: bool,
        shares: Vec<Share>,
    },
    Mount {
        share: String,
        read_only: bool,
        session: ProcId,
    },
}

impl Work {
    const fn op(&self) -> Op {
        match self {
            Self::Shares { .. } => Op::Shares,
            Self::Mount { .. } => Op::Mount,
        }
    }
}

/// Which sign-in a connection makes.
#[derive(Copy, Clone)]
enum SignIn {
    /// The session's first, its nonces seeded from `nonce_seed`.
    First { nonce_seed: [u8; NONCE_SEED_LEN] },
    /// A renewal of the established session's authentication.
    Renewal,
}

/// A call waiting on a connection.
struct Call {
    ticket: u64,
    /// The worker's name for it, once ordered.
    request: Option<u32>,
    work: Work,
    /// The pin its credentials were remembered under.
    pin: Option<Pin>,
    deadline: u64,
}

/// Where a connection stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Phase {
    /// Dropped, waiting to be tried again.
    Waiting,
    Resolving,
    Connecting,
    Negotiating,
    SigningIn,
    /// Made again, its mounted shares' trees being connected again.
    Rejoining,
    Ready,
}

/// A dropped connection's outage: how long its mounts wait for it, and when
/// it is next tried.
#[derive(Copy, Clone, Debug)]
struct Outage {
    /// When the grace window ends.
    ends: u64,
    /// Whether it has, and every call fails at once.
    past: bool,
    /// Attempts that failed, which the next wait doubles with.
    failed: u32,
    /// When the next attempt starts, while none is under way.
    retry_at: Option<u64>,
}

/// The wait after a failure that `earlier` failures in a row went before.
fn backoff(earlier: u32) -> u64 {
    RETRY_FIRST_NS
        .saturating_mul(1 << earlier.min(u64::BITS - 1))
        .min(RETRY_MAX_NS)
}

/// One connection to one server, for one principal and account.
struct Connection<T: SessionTransport, S: Sink> {
    uid: u32,
    /// The host as the calls name it.
    host: ArrayString<HOST_MAX>,
    /// The address the calls named, or none for one this service resolved.
    asked: Option<IpAddr>,
    interface: Option<[u8; IF_NAME_LEN]>,
    port: u16,
    floor: Dialect,
    credential: NtlmCredential,
    /// The GUID this client is known by, kept for the connection replacing
    /// this one after a drop.
    client_guid: [u8; 16],
    phase: Phase,
    address: Option<IpAddr>,
    socket: Option<SocketId>,
    link: Option<Link<T, S>>,
    pin: Option<Pin>,
    /// The leases the server grants, once negotiated.
    leasing: Leasing,
    /// The session the server keyed, which a session replacing it after a
    /// drop names.
    session: Option<u64>,
    /// When bytes last arrived from the server.
    heard_at: u64,
    /// Since when a call, a held file or a directory lease of its mounts has
    /// rested on it, while one does: only then is a silence asked after, so
    /// an idle mount costs no wakeups.
    watched_since: Option<u64>,
    /// When the server was last asked whether it serves, while unanswered.
    echo_at: Option<u64>,
    outage: Option<Outage>,
    /// When it was last made again after a drop.
    recovered_at: Option<u64>,
    /// Drops in a row, each within a retry's longest wait of the recovery
    /// before it: a connection that comes back only to drop again waits
    /// longer each time, as one whose attempts fail does.
    flaps: u32,
    /// The trees connected again after a drop, each for its volume.
    rejoins: Vec<(u32, OfferId)>,
    calls: Vec<Call>,
    mounts: usize,
    /// The offers of the volumes it serves.
    offers: Vec<OfferId>,
    setup_by: u64,
    linger_until: Option<u64>,
}

impl<T: SessionTransport, S: Sink> Connection<T, S> {
    /// Whether a call naming `endpoint`, `floor` and `credential` may share
    /// this connection.
    fn serves(&self, endpoint: &Endpoint<'_>, floor: Dialect, credential: &NtlmCredential) -> bool {
        self.host.eq_ignore_ascii_case(endpoint.host)
            && self.port == endpoint.port
            && self.interface == endpoint.interface
            && self.asked == endpoint.address
            && self.floor >= floor
            && self.credential.matches(credential)
    }

    /// How its mounts reach their shares.
    const fn state(&self) -> MountState {
        match self.outage {
            None => MountState::Serving,
            Some(Outage { past: false, .. }) => MountState::Reconnecting,
            Some(_) => MountState::Lost,
        }
    }

    /// The earliest instant something on this connection is due.
    fn next_deadline(&self) -> Option<u64> {
        let setup = (!matches!(self.phase, Phase::Ready | Phase::Waiting)).then_some(self.setup_by);
        let calls = self.calls.iter().map(|call| call.deadline).min();
        let retry = self.outage.and_then(|outage| outage.retry_at);
        let window = self
            .outage
            .filter(|outage| !outage.past)
            .map(|outage| outage.ends);
        [
            setup,
            calls,
            self.linger_until,
            retry,
            window,
            self.probe_at(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// When, while watched, it is next asked whether the server still serves
    /// it, or taken for dropped once asked.
    fn probe_at(&self) -> Option<u64> {
        let since = self.watched_since?;
        let quiet = self.echo_at.unwrap_or(self.heard_at.max(since));
        Some(quiet.saturating_add(ECHO_AFTER_NS))
    }

    /// Take the call `at` off the connection, answering its ticket and op.
    fn drop_call(&mut self, at: usize) -> (u64, Op) {
        let call = self.calls.swap_remove(at);
        (call.ticket, call.work.op())
    }
}

/// The principal, host and pin of `id`, for answering a call taken off it.
fn facts<T: SessionTransport, S: Sink>(
    conns: &SlotMap<ConnId, Connection<T, S>>,
    id: ConnId,
) -> (u32, ArrayString<HOST_MAX>, Option<Pin>) {
    conns.get(id).map_or((0, ArrayString::new(), None), |conn| {
        (conn.uid, conn.host, conn.pin)
    })
}

/// What one principal holds of the service.
#[derive(Default)]
struct Holdings {
    conns: Vec<ConnId>,
    calls: usize,
    mounts: usize,
}

/// The front's state, over a [`Host`] and an audit [`Sink`].
pub struct Front<H: Host, S: Sink + Clone> {
    host: H,
    sink: S,
    conns: SlotMap<ConnId, Connection<H::Worker, S>>,
    /// Each connection's next deadline under its slot, with the connection
    /// it is for, so a due entry names it.
    deadlines: IndexedHeap<(u64, ConnId)>,
    principals: HashMap<u32, Holdings, BuildFastHash>,
    mounts: Mounts,
    volumes: HashMap<u64, Volume, BuildFastHash>,
    /// The volume each file operation in flight is for, by its request.
    routes: HashMap<u32, u64, BuildFastHash>,
    hasher: BuildSipHash13,
    next_request: u32,
    /// The last name given an open the front keeps.
    next_open: u32,
    /// The clean pages of the files the kernel holds open, where the
    /// machine leaves room to keep any.
    pages: Option<held::Pages>,
    /// What the server last said of each node and each directory's listing.
    attrs: Option<meta::Attrs>,
    snapshots: Option<meta::Snapshots>,
    caching: Caching,
    reply: Vec<u8>,
    report: Vec<u8>,
    served_request: Vec<u8>,
    served_reply: Vec<u8>,
}

/// The dialect a request's floor names.
const fn floor_dialect(floor: DialectFloor) -> Dialect {
    match floor {
        DialectFloor::Smb300 => Dialect::Smb300,
        DialectFloor::Smb302 => Dialect::Smb302,
        DialectFloor::Smb311 => Dialect::Smb311,
    }
}

/// The operation a reply answers, and the largest a reply to it can be.
const fn reply_op(reply: &NetshareReply<'_>) -> Op {
    match reply {
        NetshareReply::Shares(_) => Op::Shares,
        NetshareReply::Mounted { .. } => Op::Mount,
        NetshareReply::Mounts(_) => Op::Mounts,
        NetshareReply::Refused { op, .. } => *op,
        NetshareReply::Unmounted | NetshareReply::Unreadable => Op::Unmount,
    }
}

/// Answer `ticket` with `reply`, encoded in `buffer`; a reply that cannot be
/// had is answered as the service's failure, which always can be.
fn answer<H: Host>(host: &mut H, buffer: &mut Vec<u8>, ticket: u64, reply: &NetshareReply<'_>) {
    let op = reply_op(reply);
    let need = op.max_reply();
    let room = buffer.len() >= need
        || buffer
            .try_reserve_exact(need - buffer.len())
            .map(|()| buffer.resize(need, 0))
            .is_ok();
    if room {
        if let Ok(len) = reply.encode(buffer) {
            host.reply(ticket, &buffer[..len]);
            return;
        }
    }
    let mut small = [0u8; 64];
    let failed = NetshareReply::Refused {
        op,
        error: NetshareError::Failed,
    };
    if let Ok(len) = failed.encode(&mut small) {
        host.reply(ticket, &small[..len]);
    }
}

/// The reason a refusal's audit record gives.
const fn reason(error: NetshareError) -> &'static str {
    match error {
        NetshareError::UnknownHost => "unknown host",
        NetshareError::Unreachable => "unreachable",
        NetshareError::PinMismatch(_) => "pin mismatch",
        NetshareError::Unsupported(_) => "server falls short",
        NetshareError::Tampered => "tampered",
        NetshareError::Protocol => "protocol",
        NetshareError::TooLarge => "too large",
        NetshareError::Rejected(_) => "credentials rejected",
        NetshareError::NoSuchShare => "no such share",
        NetshareError::Denied => "denied",
        NetshareError::NoSession => "no login session",
        NetshareError::NoSuchMount => "no such mount",
        NetshareError::Limit => "limit",
        NetshareError::Failed => "failed",
        NetshareError::AlreadyMounted => "already mounted",
    }
}

fn record<S: Sink>(
    sink: &S,
    id: EventId,
    level: Level,
    message: &'static str,
    fields: &[Field<'_>],
) {
    tairix_log::log(
        sink,
        &Event {
            level,
            id,
            message,
            fields,
        },
    );
}

fn address_field(address: Option<IpAddr>) -> FieldValue<'static> {
    address.map_or(FieldValue::Null, |address| FieldValue::Ip(address.into()))
}

/// What a mounted share is offered as.
const fn serve_offer(read_only: bool) -> ServeOffer {
    ServeOffer {
        root: ROOT_NODE,
        name_matching: NameMatching::AsciiCaseInsensitive,
        node_identity: NodeIdentity::Stable,
        attrs: false,
        write_refusal: if read_only {
            Some(WriteRefusal::OpenedReadOnly)
        } else {
            None
        },
        format: FormatName::Smb3,
    }
}

/// `//host/share`, cut to what the mount table shows.
fn mount_source(host: &str, share: &str) -> Result<String, NetshareError> {
    let mut source = String::new();
    source
        .try_reserve_exact(2 + host.len() + 1 + share.len())
        .map_err(|_| NetshareError::Failed)?;
    let _ = write!(source, "//{host}/{share}");
    tairix_util::utf8::cut(&mut source, MOUNT_SOURCE_MAX);
    Ok(source)
}

impl<H: Host, S: Sink + Clone> Front<H, S> {
    /// A front with nothing open, over `host`, recording to `sink`, its
    /// volumes' nodes and pages indexed under `hasher`, caching within
    /// `caching`.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when its buffers cannot be had.
    pub fn new(host: H, sink: S, hasher: BuildSipHash13, caching: Caching) -> Result<Self, Errno> {
        let filled = |len: usize| tairix_util::fallible::filled(len, 0u8).ok_or(Errno::OutOfMemory);
        let mut report = Vec::new();
        report
            .try_reserve_exact(REPORT_MAX)
            .map_err(|_| Errno::OutOfMemory)?;
        let (attrs, snapshots) = if caching.memory > 0 {
            let (attrs, snapshots) = meta::caches(
                CacheBudget::from_backing(caching.memory),
                caching.pressure,
                caching.sink,
                hasher,
            );
            (Some(attrs), Some(snapshots))
        } else {
            (None, None)
        };
        Ok(Self {
            host,
            sink,
            conns: SlotMap::new(),
            deadlines: IndexedHeap::new(),
            principals: HashMap::with_hasher(BuildFastHash::new()),
            mounts: Mounts::new(),
            volumes: HashMap::with_hasher(BuildFastHash::new()),
            routes: HashMap::with_hasher(BuildFastHash::new()),
            pages: (caching.memory > 0).then(|| {
                held::page_cache(
                    CacheBudget::from_backing(caching.memory),
                    caching.pressure,
                    caching.sink,
                    hasher,
                )
            }),
            attrs,
            snapshots,
            hasher,
            next_request: 0,
            next_open: 0,
            caching,
            reply: Vec::new(),
            report,
            served_request: filled(tairix_abi::fsserve::FSSERVE_REQUEST_MAX)?,
            served_reply: filled(tairix_abi::fsserve::FSSERVE_REPLY_MAX)?,
        })
    }

    /// A fresh name for one request to a worker.
    fn next_request_id(&mut self) -> u32 {
        self.next_request = self.next_request.wrapping_add(1);
        self.next_request
    }

    /// The machine's memory pressure moved: the caches shrink to what the
    /// new band lets them keep.
    pub fn pressure_changed(&mut self) {
        if let Some(pages) = self.pages.as_mut() {
            pages.enforce_pressure();
        }
        if let Some(attrs) = self.attrs.as_mut() {
            attrs.enforce_pressure();
        }
        if let Some(snapshots) = self.snapshots.as_mut() {
            snapshots.enforce_pressure();
        }
    }

    /// The host, for the loop that drives it.
    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    /// The earliest instant the front must act by, if any.
    #[must_use]
    pub fn next_deadline(&self) -> Option<u64> {
        self.deadlines.peek().map(|(_, (at, _))| at)
    }

    /// Serve the call `ticket` names from `caller`, its request in
    /// `request`, which is erased before this returns.
    pub fn call(&mut self, now: u64, ticket: u64, caller: Caller, request: &mut [u8]) {
        if let Ok(decoded) = NetshareRequest::decode(request) {
            self.take(now, ticket, caller, &decoded);
        } else {
            record(
                &self.sink,
                REQUEST_UNREADABLE,
                Level::Warn,
                "smbfs: a request could not be read",
                &[Field {
                    key: "uid",
                    value: FieldValue::UnsignedInt(u64::from(caller.uid)),
                }],
            );
            answer(
                &mut self.host,
                &mut self.reply,
                ticket,
                &NetshareReply::Unreadable,
            );
        }
        wipe(request);
    }

    fn refuse(&mut self, ticket: u64, uid: u32, host: &str, op: Op, error: NetshareError) {
        record(
            &self.sink,
            REQUEST_REFUSED,
            Level::Info,
            "smbfs: a request was refused",
            &[
                Field {
                    key: "uid",
                    value: FieldValue::UnsignedInt(u64::from(uid)),
                },
                Field {
                    key: "host",
                    value: FieldValue::Str(host),
                },
                Field {
                    key: "reason",
                    value: FieldValue::Str(reason(error)),
                },
            ],
        );
        answer(
            &mut self.host,
            &mut self.reply,
            ticket,
            &NetshareReply::Refused { op, error },
        );
    }

    /// Refuse the call `ticket` names on `id`, which is `uid`'s.
    fn refuse_on(&mut self, id: ConnId, uid: u32, ticket: u64, op: Op, error: NetshareError) {
        if let Some(held) = self.principals.get_mut(&uid) {
            held.calls = held.calls.saturating_sub(1);
        }
        let host = self
            .conns
            .get(id)
            .map_or_else(ArrayString::new, |conn| conn.host);
        self.refuse(ticket, uid, &host, op, error);
    }

    fn holdings_mut(&mut self, uid: u32) -> Result<&mut Holdings, NetshareError> {
        if !self.principals.contains_key(&uid) {
            self.principals
                .try_insert(uid, Holdings::default())
                .map_err(|_| NetshareError::Failed)?;
        }
        self.principals.get_mut(&uid).ok_or(NetshareError::Failed)
    }

    /// Forget a principal that holds nothing.
    fn settle(&mut self, uid: u32) {
        if self
            .principals
            .get(&uid)
            .is_some_and(|held| held.conns.is_empty() && held.calls == 0 && held.mounts == 0)
        {
            self.principals.remove(&uid);
        }
    }

    fn take(&mut self, now: u64, ticket: u64, caller: Caller, request: &NetshareRequest<'_>) {
        match *request {
            NetshareRequest::Mounts => match self.mounts.entries(caller.uid, |mount| {
                self.conns
                    .get(mount.conn)
                    .map_or(MountState::Lost, Connection::state)
            }) {
                Ok(entries) => answer(
                    &mut self.host,
                    &mut self.reply,
                    ticket,
                    &NetshareReply::Mounts(Records::new(&entries)),
                ),
                Err(_) => self.refuse(ticket, caller.uid, "", Op::Mounts, NetshareError::Failed),
            },
            NetshareRequest::Unmount { offer } => {
                if self.mounts.owned(caller.uid, offer).is_none() {
                    self.refuse(
                        ticket,
                        caller.uid,
                        "",
                        Op::Unmount,
                        NetshareError::NoSuchMount,
                    );
                    return;
                }
                self.retire(now, offer, "unmounted", Some(ticket));
            }
            NetshareRequest::Shares { connect, special } => self.connect(
                now,
                ticket,
                caller.uid,
                &connect,
                Work::Shares {
                    special,
                    shares: Vec::new(),
                },
            ),
            NetshareRequest::Mount {
                connect,
                share,
                read_only,
            } => {
                let host = connect.endpoint.host;
                let Some(session) = caller.session else {
                    self.refuse(
                        ticket,
                        caller.uid,
                        host,
                        Op::Mount,
                        NetshareError::NoSession,
                    );
                    return;
                };
                if self
                    .principals
                    .get(&caller.uid)
                    .is_some_and(|held| held.mounts >= MOUNTS_MAX)
                {
                    self.refuse(ticket, caller.uid, host, Op::Mount, NetshareError::Limit);
                    return;
                }
                let mut held = String::new();
                if held.try_reserve_exact(share.len()).is_err() {
                    self.refuse(ticket, caller.uid, host, Op::Mount, NetshareError::Failed);
                    return;
                }
                held.push_str(share);
                self.connect(
                    now,
                    ticket,
                    caller.uid,
                    &connect,
                    Work::Mount {
                        share: held,
                        read_only,
                        session,
                    },
                );
            }
        }
    }

    /// Hold `work` for `ticket` on a connection that serves it, opening one
    /// when none does.
    fn connect(&mut self, now: u64, ticket: u64, uid: u32, connect: &Connect<'_>, work: Work) {
        let op = work.op();
        if let Err(error) = self.admit(now, ticket, uid, connect, work) {
            self.refuse(ticket, uid, connect.endpoint.host, op, error);
            self.settle(uid);
        }
    }

    fn admit(
        &mut self,
        now: u64,
        ticket: u64,
        uid: u32,
        connect: &Connect<'_>,
        work: Work,
    ) -> Result<(), NetshareError> {
        if self.holdings_mut(uid)?.calls >= CALLS_MAX {
            return Err(NetshareError::Limit);
        }
        let credentials = &connect.credentials;
        let credential = NtlmCredential::new(
            credentials.user,
            credentials.domain,
            credentials.password.expose(),
        )
        .map_err(|_| NetshareError::Failed)?;
        let floor = floor_dialect(connect.floor);
        let held = self.principals.get(&uid).ok_or(NetshareError::Failed)?;
        let shared = held.conns.iter().copied().find(|&id| {
            self.conns
                .get(id)
                .is_some_and(|conn| conn.serves(&connect.endpoint, floor, &credential))
        });
        let id = match shared {
            Some(id) => id,
            None if held.conns.len() >= CONNECTIONS_MAX => return Err(NetshareError::Limit),
            None => self.open(now, uid, connect, floor, credential)?,
        };
        let conn = self.conns.get_mut(id).ok_or(NetshareError::Failed)?;
        conn.calls
            .try_reserve(1)
            .map_err(|_| NetshareError::Failed)?;
        conn.calls.push(Call {
            ticket,
            request: None,
            work,
            pin: connect.pin,
            deadline: now.saturating_add(CALL_DEADLINE_NS),
        });
        let ready = conn.phase == Phase::Ready;
        if let Some(held) = self.principals.get_mut(&uid) {
            held.calls += 1;
        }
        if ready {
            self.dispatch(now, id);
        }
        self.settled(now, id);
        Ok(())
    }

    /// Open a connection for `uid` to `connect`'s endpoint, signed in as
    /// `credential`.
    fn open(
        &mut self,
        now: u64,
        uid: u32,
        connect: &Connect<'_>,
        floor: Dialect,
        credential: NtlmCredential,
    ) -> Result<ConnId, NetshareError> {
        let endpoint = &connect.endpoint;
        let host = ArrayString::try_from(endpoint.host).map_err(|_| NetshareError::Failed)?;
        let mut client_guid = [0; 16];
        self.host
            .fill_random(&mut client_guid)
            .map_err(|_| NetshareError::Failed)?;
        self.holdings_mut(uid)?
            .conns
            .try_reserve(1)
            .map_err(|_| NetshareError::Failed)?;
        let opened = self.conns.try_insert(Connection {
            uid,
            host,
            asked: endpoint.address,
            interface: endpoint.interface,
            port: endpoint.port,
            floor,
            credential,
            client_guid,
            phase: Phase::Resolving,
            address: None,
            socket: None,
            link: None,
            pin: None,
            leasing: Leasing::None,
            session: None,
            heard_at: now,
            watched_since: None,
            echo_at: None,
            outage: None,
            recovered_at: None,
            flaps: 0,
            rejoins: Vec::new(),
            calls: Vec::new(),
            mounts: 0,
            offers: Vec::new(),
            setup_by: now.saturating_add(SETUP_DEADLINE_NS),
            linger_until: None,
        });
        let id = opened.map_err(|error| match error {
            TryReserveError::CapacityOverflow => NetshareError::Limit,
            TryReserveError::AllocFailed => NetshareError::Failed,
        })?;
        if self.deadlines.try_reserve_slot(id.index()).is_err() {
            self.conns.remove(id);
            return Err(NetshareError::Failed);
        }
        if let Some(held) = self.principals.get_mut(&uid) {
            held.conns.push(id);
        }
        let started = match endpoint.address {
            Some(address) => self.dial(id, address),
            None => self
                .host
                .resolve(id, endpoint.host, endpoint.interface)
                .map_err(|_| NetshareError::Unreachable),
        };
        if let Err(error) = started {
            self.end(now, id, Some(error));
            return Err(error);
        }
        Ok(id)
    }

    /// Begin connecting `id` to `address`.
    fn dial(&mut self, id: ConnId, address: IpAddr) -> Result<(), NetshareError> {
        let conn = self.conns.get(id).ok_or(NetshareError::Failed)?;
        let (interface, port) = (conn.interface, conn.port);
        let (family, addr) = address_parts(address);
        let socket = self
            .host
            .open(id, family)
            .map_err(|_| NetshareError::Unreachable)?;
        if let Some(conn) = self.conns.get_mut(id) {
            conn.socket = Some(socket);
            conn.address = Some(address);
            conn.phase = Phase::Connecting;
        }
        self.host
            .connect(socket, SocketAddr { family, addr, port }, interface)
            .map_err(|_| NetshareError::Unreachable)
    }

    /// A lookup the front asked for `id` finished.
    pub fn resolved(&mut self, now: u64, id: ConnId, resolution: Resolution) {
        if !self
            .conns
            .get(id)
            .is_some_and(|conn| conn.phase == Phase::Resolving)
        {
            return;
        }
        let outcome = match resolution {
            Ok(address) if is_unicast(address) => self.dial(id, address),
            Ok(_) | Err(Unresolved::NoSuchHost) => Err(NetshareError::UnknownHost),
            Err(Unresolved::Unanswered) => Err(NetshareError::Unreachable),
        };
        match outcome {
            Ok(()) => self.settled(now, id),
            Err(error) => self.fail(now, id, error),
        }
    }

    /// An event arrived on `id`'s socket.
    pub fn socket(&mut self, now: u64, id: ConnId, event: &SocketStreamEvent<'_>) {
        let Some(socket) = self.conns.get(id).and_then(|conn| conn.socket) else {
            return;
        };
        let outcome = match *event {
            SocketStreamEvent::Connected { socket: from } if from == socket => self.connected(id),
            SocketStreamEvent::Data {
                socket: from,
                payload,
            } if from == socket && !payload.is_empty() => {
                if let Some(conn) = self.conns.get_mut(id) {
                    conn.heard_at = now;
                    conn.echo_at = None;
                }
                self.order(id, &Order::Received(payload))
            }
            SocketStreamEvent::Room { socket: from } if from == socket => {
                let Self { conns, host, .. } = self;
                match conns.get_mut(id).and_then(|conn| conn.link.as_mut()) {
                    Some(link) => link.room(host, socket).map_err(link_refusal),
                    None => Ok(()),
                }
            }
            SocketStreamEvent::Closed { socket: from, .. } if from == socket => {
                Err(NetshareError::Unreachable)
            }
            _ => Ok(()),
        };
        match outcome {
            Ok(()) => {
                self.drain(now, id);
                self.settled(now, id);
            }
            Err(error) => self.fail(now, id, error),
        }
    }

    /// `id`'s socket connected: start its worker and open the connection.
    fn connected(&mut self, id: ConnId) -> Result<(), NetshareError> {
        if !self
            .conns
            .get(id)
            .is_some_and(|conn| conn.phase == Phase::Connecting)
        {
            return Err(NetshareError::Protocol);
        }
        let worker = self.host.launch().map_err(|_| NetshareError::Failed)?;
        let mut link = Link::new(worker, self.sink.clone()).map_err(|_| NetshareError::Failed)?;
        let mut salt = [0; SALT_LEN];
        self.host
            .fill_random(&mut salt)
            .map_err(|_| NetshareError::Failed)?;
        let conn = self.conns.get_mut(id).ok_or(NetshareError::Failed)?;
        link.order(&Order::Open {
            floor: conn.floor,
            client_guid: conn.client_guid,
            salt,
            server: &conn.host,
        })
        .map_err(|_| NetshareError::Failed)?;
        conn.link = Some(link);
        conn.phase = Phase::Negotiating;
        Ok(())
    }

    /// Hand `order` to `id`'s worker.
    fn order(&mut self, id: ConnId, order: &Order<'_>) -> Result<(), NetshareError> {
        self.conns
            .get_mut(id)
            .and_then(|conn| conn.link.as_mut())
            .ok_or(NetshareError::Failed)?
            .order(order)
            .map_err(link_refusal)
    }

    /// `id`'s worker has written.
    pub fn worker_readable(&mut self, now: u64, id: ConnId) {
        let read = self
            .conns
            .get_mut(id)
            .and_then(|conn| conn.link.as_mut())
            .map(Link::readable);
        self.after_worker(now, id, read);
    }

    /// `id`'s worker has room for more.
    pub fn worker_writable(&mut self, now: u64, id: ConnId) {
        let written = self
            .conns
            .get_mut(id)
            .and_then(|conn| conn.link.as_mut())
            .map(Link::writable);
        self.after_worker(now, id, written);
    }

    fn after_worker(&mut self, now: u64, id: ConnId, outcome: Option<Result<(), LinkError>>) {
        match outcome {
            Some(Ok(())) => {
                self.drain(now, id);
                self.settled(now, id);
            }
            Some(Err(error)) => self.fail(now, id, link_refusal(error)),
            None => {}
        }
    }

    /// Act on every report `id`'s worker has sent.
    fn drain(&mut self, now: u64, id: ConnId) {
        let mut report = core::mem::take(&mut self.report);
        while let Some(link) = self.conns.get_mut(id).and_then(|conn| conn.link.as_mut()) {
            let next = link.next_report(&mut report);
            let outcome = match next {
                Ok(false) => break,
                Ok(true) => match Report::decode(&report) {
                    Ok(decoded) => self.on_report(now, id, &decoded),
                    Err(_) => Err(NetshareError::Protocol),
                },
                Err(error) => Err(link_refusal(error)),
            };
            if let Err(error) = outcome {
                self.fail(now, id, error);
                break;
            }
        }
        self.report = report;
    }

    /// Act on `report` when it is a file operation's, or `None` when it is
    /// not.
    fn file_report(
        &mut self,
        now: u64,
        id: ConnId,
        report: &Report<'_>,
    ) -> Option<Result<(), NetshareError>> {
        let request = report.file_request()?;
        if let Some(offer) = self.route(request) {
            // A volume served on another connection is no report of this
            // one's to make.
            if self.volumes.get(&offer.get()).map(|volume| volume.conn) != Some(id) {
                return Some(Err(NetshareError::Protocol));
            }
            return Some(
                self.on_file_report(now, offer, report)
                    .map_err(|Lie| NetshareError::Protocol),
            );
        }
        // A file operation already answered, or let go unanswered; a refusal
        // may be a call's.
        (!matches!(report, Report::Refused { .. })).then_some(Ok(()))
    }

    fn on_report(
        &mut self,
        now: u64,
        id: ConnId,
        report: &Report<'_>,
    ) -> Result<(), NetshareError> {
        if let Some(outcome) = self.file_report(now, id, report) {
            return outcome;
        }
        match *report {
            Report::Send(bytes) => {
                let Self { conns, host, .. } = self;
                let conn = conns.get_mut(id).ok_or(NetshareError::Failed)?;
                let socket = conn.socket.ok_or(NetshareError::Failed)?;
                conn.link
                    .as_mut()
                    .ok_or(NetshareError::Failed)?
                    .transmit(host, socket, bytes)
                    .map_err(link_refusal)
            }
            Report::Negotiated {
                server_guid,
                leasing,
                ..
            } => self.negotiated(now, id, server_guid, leasing),
            Report::Established { session } => {
                let conn = self.conns.get_mut(id).ok_or(NetshareError::Failed)?;
                if conn.phase != Phase::SigningIn {
                    return Err(NetshareError::Protocol);
                }
                conn.session = Some(session);
                if conn.outage.is_some() {
                    return self.rejoin(now, id);
                }
                conn.phase = Phase::Ready;
                self.dispatch(now, id);
                Ok(())
            }
            Report::Rejected(status) => self.rejected(id, status),
            Report::Expired => self.sign_in(id, SignIn::Renewal),
            Report::Renewed
            | Report::Stat { .. }
            | Report::Data { .. }
            | Report::Entry { .. }
            | Report::Page { .. }
            | Report::Done { .. }
            | Report::Space { .. }
            | Report::Held { .. } => Ok(()),
            Report::Break { key, new, ack } => {
                self.on_break(now, id, key, new, ack);
                Ok(())
            }
            Report::Share {
                request,
                name,
                special,
                remark,
            } => self.listed_share(id, request, name, special, remark),
            Report::Listed {
                request,
                hidden,
                unnamed,
                unlisted,
            } => self.listed(id, request, hidden, unnamed, unlisted),
            Report::Connected {
                request,
                tree,
                kind,
            } if self.rejoining(id, request).is_some() => {
                let offer = self.rejoining(id, request).ok_or(NetshareError::Failed)?;
                self.rejoined(now, id, request, offer, Ok((tree, kind)))
            }
            Report::Refused { request, error } if self.rejoining(id, request).is_some() => {
                let offer = self.rejoining(id, request).ok_or(NetshareError::Failed)?;
                self.rejoined(now, id, request, offer, Err(error))
            }
            Report::Connected {
                request,
                tree,
                kind,
            } => match self.take_call(id, request) {
                Some(call) => self.mount(now, id, &call, tree, kind),
                // A mount whose call ran out of time leaves no tree behind.
                None => self.order(id, &Order::Disconnect { tree }),
            },
            Report::Refused { request, error } => {
                if let Some(call) = self.take_call(id, request) {
                    let op = call.work.op();
                    let refusal = match op {
                        Op::Shares => judge::listing(error),
                        _ => judge::tree(error),
                    };
                    let (uid, host, _) = facts(&self.conns, id);
                    self.refuse(call.ticket, uid, &host, op, refusal);
                }
                Ok(())
            }
            Report::Failed(error) => Err(judge::connection(error)),
        }
    }

    /// The server refused `id`'s credentials with `status`.
    fn rejected(&self, id: ConnId, status: NtStatus) -> Result<(), NetshareError> {
        let conn = self.conns.get(id).ok_or(NetshareError::Failed)?;
        if conn.phase != Phase::SigningIn {
            return Err(NetshareError::Protocol);
        }
        record(
            &self.sink,
            AUTHENTICATION_REJECTED,
            Level::Warn,
            "smbfs: a server refused the credentials",
            &[
                Field {
                    key: "uid",
                    value: FieldValue::UnsignedInt(u64::from(conn.uid)),
                },
                Field {
                    key: "host",
                    value: FieldValue::Str(&conn.host),
                },
                Field {
                    key: "address",
                    value: address_field(conn.address),
                },
            ],
        );
        Err(NetshareError::Rejected(judge::rejection(status)))
    }

    /// The server is breaking the lease `key` on `id`: the volume holding it
    /// acts on it, and a lease none holds — let go while the break was on
    /// its way — is acknowledged at once, as the server waits on it.
    fn on_break(&mut self, now: u64, id: ConnId, key: [u8; 16], new: u8, ack: bool) {
        let offers = self.conns.get(id).map_or(0, |conn| conn.offers.len());
        for at in 0..offers {
            let offer = self
                .conns
                .get(id)
                .and_then(|conn| conn.offers.get(at).copied());
            if offer.is_some_and(|offer| {
                self.held_break(now, offer, key, new, ack) || self.dir_break(offer, key, new, ack)
            }) {
                return;
            }
        }
        let tree = self
            .conns
            .get(id)
            .and_then(|conn| conn.offers.first())
            .and_then(|offer| self.volumes.get(&offer.get()))
            .map(|volume| volume.tree);
        if let (true, Some(tree)) = (ack, tree) {
            let request = self.next_request_id();
            // An acknowledgment the connection cannot take is the server's to
            // time out.
            let _ = self.order(
                id,
                &Order::File {
                    request,
                    tree,
                    op: FileOp::Ack { key, state: new },
                },
            );
        }
    }

    /// Keep one share a listing found for the call the worker knows as
    /// `request`.
    fn listed_share(
        &mut self,
        id: ConnId,
        request: u32,
        name: &str,
        special: bool,
        remark: &str,
    ) -> Result<(), NetshareError> {
        let call = self.conns.get_mut(id).and_then(|conn| {
            conn.calls
                .iter_mut()
                .find(|call| call.request == Some(request))
        });
        let shares = match call {
            Some(Call {
                work: Work::Shares { shares, .. },
                ..
            }) => shares,
            // A share for a mount is no report an honest worker makes.
            Some(_) => return Err(NetshareError::Protocol),
            // A listing whose call ran out of time.
            None => return Ok(()),
        };
        if shares.len() >= SHARES_MAX {
            return Err(NetshareError::Protocol);
        }
        let name = ArrayString::try_from(name).map_err(|_| NetshareError::Protocol)?;
        let mut held = String::new();
        held.try_reserve_exact(remark.len())
            .map_err(|_| NetshareError::Failed)?;
        held.push_str(remark);
        shares.try_reserve(1).map_err(|_| NetshareError::Failed)?;
        shares.push(Share {
            name,
            special,
            remark: held,
        });
        Ok(())
    }

    /// Answer the listing the worker knows as `request` with the shares it
    /// found.
    ///
    /// # Errors
    ///
    /// [`NetshareError::Protocol`] for a listing reported for a mount, which
    /// is answered so first.
    fn listed(
        &mut self,
        id: ConnId,
        request: u32,
        hidden: u32,
        unnamed: u32,
        unlisted: u32,
    ) -> Result<(), NetshareError> {
        let Some(call) = self.take_call(id, request) else {
            return Ok(());
        };
        let (uid, host, pin) = facts(&self.conns, id);
        let (Some(pin), Work::Shares { shares, .. }) = (pin, &call.work) else {
            self.refuse(
                call.ticket,
                uid,
                &host,
                call.work.op(),
                NetshareError::Protocol,
            );
            return Err(NetshareError::Protocol);
        };
        let mut entries = Vec::new();
        if entries.try_reserve_exact(shares.len()).is_err() {
            self.refuse(call.ticket, uid, &host, Op::Shares, NetshareError::Failed);
            return Ok(());
        }
        entries.extend(shares.iter().map(|share| ShareEntry {
            name: &share.name,
            special: share.special,
            remark: &share.remark,
        }));
        answer(
            &mut self.host,
            &mut self.reply,
            call.ticket,
            &NetshareReply::Shares(Shares {
                pin,
                hidden,
                unnamed,
                unlisted,
                shares: Records::new(&entries),
            }),
        );
        Ok(())
    }

    /// Take the call the worker knows as `request` off `id`.
    fn take_call(&mut self, id: ConnId, request: u32) -> Option<Call> {
        let conn = self.conns.get_mut(id)?;
        let at = conn
            .calls
            .iter()
            .position(|call| call.request == Some(request))?;
        let call = conn.calls.swap_remove(at);
        let uid = conn.uid;
        if let Some(held) = self.principals.get_mut(&uid) {
            held.calls = held.calls.saturating_sub(1);
        }
        Some(call)
    }

    /// Order a sign-in or a renewal on `id`, with its account and fresh
    /// randomness.
    fn sign_in(&mut self, id: ConnId, kind: SignIn) -> Result<(), NetshareError> {
        let mut client_challenge = [0; 8];
        self.host
            .fill_random(&mut client_challenge)
            .map_err(|_| NetshareError::Failed)?;
        let now = self.host.wall_time();
        let conn = self.conns.get_mut(id).ok_or(NetshareError::Failed)?;
        let credential = Credential {
            user: conn.credential.user(),
            domain: conn.credential.domain(),
            key: conn.credential.response_key(),
            client_challenge,
            now,
        };
        let order = match kind {
            SignIn::First { nonce_seed } => Order::SignIn {
                credential,
                nonce_seed,
                previous: conn.session,
            },
            SignIn::Renewal => Order::Renew(credential),
        };
        conn.link
            .as_mut()
            .ok_or(NetshareError::Failed)?
            .order(&order)
            .map_err(link_refusal)
    }

    /// `id` negotiated with the server whose GUID is `server_guid`: hold every
    /// call to the pin it remembered, and sign in for those that remain.
    fn negotiated(
        &mut self,
        now: u64,
        id: ConnId,
        server_guid: [u8; 16],
        leasing: Leasing,
    ) -> Result<(), NetshareError> {
        let conn = self.conns.get_mut(id).ok_or(NetshareError::Failed)?;
        conn.leasing = leasing;
        if conn.phase != Phase::Negotiating {
            return Err(NetshareError::Protocol);
        }
        let address = conn.address.ok_or(NetshareError::Failed)?;
        let pin = Pin {
            server_guid,
            address,
        };
        // Made again, it must be the server its mounts are on, where they
        // were reached.
        if conn.outage.is_some() && conn.pin.is_some_and(|known| known != pin) {
            return Err(NetshareError::PinMismatch(pin));
        }
        conn.pin = Some(pin);
        let (uid, host) = (conn.uid, conn.host);
        let mut at = 0;
        while let Some(conn) = self.conns.get_mut(id) {
            let Some(call) = conn.calls.get(at) else {
                break;
            };
            if call.pin.is_none_or(|wanted| wanted == pin) {
                at += 1;
                continue;
            }
            let (ticket, op) = conn.drop_call(at);
            record(
                &self.sink,
                PIN_MISMATCH,
                Level::Warn,
                "smbfs: a host is not the one a remembered sign-in is for",
                &[
                    Field {
                        key: "uid",
                        value: FieldValue::UnsignedInt(u64::from(uid)),
                    },
                    Field {
                        key: "host",
                        value: FieldValue::Str(&host),
                    },
                    Field {
                        key: "address",
                        value: FieldValue::Ip(address.into()),
                    },
                ],
            );
            self.refuse_on(id, uid, ticket, op, NetshareError::PinMismatch(pin));
        }
        if self
            .conns
            .get(id)
            .is_some_and(|conn| conn.calls.is_empty() && conn.mounts == 0)
        {
            // Nothing that remains needs the server, so no credential leaves.
            self.end(now, id, None);
            return Ok(());
        }
        let mut nonce_seed = [0; NONCE_SEED_LEN];
        self.host
            .fill_random(&mut nonce_seed)
            .map_err(|_| NetshareError::Failed)?;
        if let Some(conn) = self.conns.get_mut(id) {
            conn.phase = Phase::SigningIn;
        }
        self.sign_in(id, SignIn::First { nonce_seed })
    }

    /// Order every call waiting on the ready connection `id`, refusing those
    /// whose pin it does not match.
    fn dispatch(&mut self, now: u64, id: ConnId) {
        let mut at = 0;
        loop {
            let Some(conn) = self.conns.get_mut(id) else {
                return;
            };
            let (Some(pin), Some(call)) = (conn.pin, conn.calls.get(at)) else {
                return;
            };
            if call.request.is_some() {
                at += 1;
                continue;
            }
            if call.pin.is_some_and(|wanted| wanted != pin) {
                let uid = conn.uid;
                let (ticket, op) = conn.drop_call(at);
                self.refuse_on(id, uid, ticket, op, NetshareError::PinMismatch(pin));
                continue;
            }
            let request = self.next_request_id();
            let Some(conn) = self.conns.get_mut(id) else {
                return;
            };
            let Some(call) = conn.calls.get_mut(at) else {
                return;
            };
            call.request = Some(request);
            let order = match &call.work {
                Work::Shares { special, .. } => Order::List {
                    request,
                    special: *special,
                },
                Work::Mount { share, .. } => Order::Connect {
                    request,
                    share: share.as_str(),
                },
            };
            let ordered = conn
                .link
                .as_mut()
                .is_some_and(|link| link.order(&order).is_ok());
            if !ordered {
                self.fail(now, id, NetshareError::Failed);
                return;
            }
            at += 1;
        }
    }

    /// Mount the share a call asked for, now its tree is connected.
    ///
    /// # Errors
    ///
    /// [`NetshareError::Protocol`] for a tree reported for a listing, which
    /// is answered so first.
    fn mount(
        &mut self,
        now: u64,
        id: ConnId,
        call: &Call,
        tree: u32,
        kind: ShareType,
    ) -> Result<(), NetshareError> {
        let (uid, host, pin) = facts(&self.conns, id);
        let address = self.conns.get(id).and_then(|conn| conn.address);
        let (
            Some(pin),
            Work::Mount {
                share,
                read_only,
                session,
            },
        ) = (pin, &call.work)
        else {
            self.refuse(
                call.ticket,
                uid,
                &host,
                call.work.op(),
                NetshareError::Protocol,
            );
            return Err(NetshareError::Protocol);
        };
        let mounted = if kind == ShareType::Disk {
            self.offer(now, id, &pin, share, *read_only, *session, tree)
        } else {
            Err(NetshareError::NoSuchShare)
        };
        match mounted {
            Ok(offer) => {
                record(
                    &self.sink,
                    MOUNTED,
                    Level::Info,
                    "smbfs: a share was mounted",
                    &[
                        Field {
                            key: "uid",
                            value: FieldValue::UnsignedInt(u64::from(uid)),
                        },
                        Field {
                            key: "host",
                            value: FieldValue::Str(&host),
                        },
                        Field {
                            key: "share",
                            value: FieldValue::Str(share),
                        },
                        Field {
                            key: "address",
                            value: address_field(address),
                        },
                        Field {
                            key: "offer",
                            value: FieldValue::UnsignedInt(offer.get()),
                        },
                    ],
                );
                answer(
                    &mut self.host,
                    &mut self.reply,
                    call.ticket,
                    &NetshareReply::Mounted { pin, offer },
                );
                Ok(())
            }
            Err(error) => {
                self.refuse(call.ticket, uid, &host, Op::Mount, error);
                self.order(id, &Order::Disconnect { tree })
            }
        }
    }

    /// Offer `share`'s volume, connected as `tree` on `id`, and record its
    /// mount.
    #[allow(clippy::too_many_arguments)] // One mount's parts, each its own.
    fn offer(
        &mut self,
        now: u64,
        id: ConnId,
        pin: &Pin,
        share: &str,
        read_only: bool,
        session: ProcId,
        tree: u32,
    ) -> Result<OfferId, NetshareError> {
        let conn = self.conns.get(id).ok_or(NetshareError::Failed)?;
        let uid = conn.uid;
        let key = volume_key(&pin.server_guid, share);
        if self.mounts.holds(uid, &key) {
            return Err(NetshareError::AlreadyMounted);
        }
        if self
            .principals
            .get(&uid)
            .is_some_and(|held| held.mounts >= MOUNTS_MAX)
        {
            return Err(NetshareError::Limit);
        }
        let mut host = String::new();
        host.try_reserve_exact(conn.host.len())
            .map_err(|_| NetshareError::Failed)?;
        host.push_str(&conn.host);
        let source = mount_source(&host, share)?;
        let offer = self
            .host
            .offer(&NetworkOffer {
                serve: serve_offer(read_only),
                for_uid: uid,
                key,
                lanes: LANES,
                source: &source,
            })
            .map_err(|errno| match errno {
                Errno::AlreadyExists => NetshareError::AlreadyMounted,
                _ => NetshareError::Failed,
            })?;
        let Ok(share) = ArrayString::<SHARE_NAME_BYTES>::try_from(share) else {
            self.host.withdraw(offer);
            return Err(NetshareError::Failed);
        };
        let first = self.mounts.add(Mount {
            offer,
            uid,
            session,
            conn: id,
            key,
            host,
            share,
            read_only,
        });
        let Ok(first) = first else {
            self.host.withdraw(offer);
            return Err(NetshareError::Failed);
        };
        let directories = self
            .conns
            .get(id)
            .is_some_and(|conn| conn.leasing == Leasing::Directories);
        let mut volume = Volume::new(
            id,
            tree,
            read_only,
            Nodes::new(&self.hasher),
            Holding::new((!read_only).then(|| self.caching.bound()).flatten()),
        );
        volume.dir_leasing = directories;
        let held = self
            .conns
            .get_mut(id)
            .is_some_and(|conn| conn.offers.try_reserve(1).is_ok());
        if !held || self.volumes.try_insert(offer.get(), volume).is_err() {
            // A session's first mount is watched only once it is made.
            let _ = self.mounts.remove(offer);
            self.host.withdraw(offer);
            return Err(NetshareError::Failed);
        }
        if let Some(conn) = self.conns.get_mut(id) {
            conn.mounts += 1;
            conn.offers.push(offer);
        }
        if let Some(held) = self.principals.get_mut(&uid) {
            held.mounts += 1;
        }
        if first && self.host.watch(session).is_err() {
            // The session ended before its mount was made.
            self.withdraw(now, offer, "session ended");
            return Err(NetshareError::NoSession);
        }
        Ok(offer)
    }

    /// Withdraw the mount `offer` names, for `cause`.
    fn withdraw(&mut self, now: u64, offer: OfferId, cause: &'static str) {
        let Some((mount, last)) = self.mounts.remove(offer) else {
            return;
        };
        let tree = self.volumes.get(&offer.get()).map(|volume| volume.tree);
        self.abandon_volume(offer);
        self.host.withdraw(offer);
        if last {
            self.host.unwatch(mount.session);
        }
        if let Some(held) = self.principals.get_mut(&mount.uid) {
            held.mounts = held.mounts.saturating_sub(1);
        }
        if let Some(conn) = self.conns.get_mut(mount.conn) {
            conn.mounts = conn.mounts.saturating_sub(1);
            conn.offers.retain(|held| *held != offer);
            if let (Some(link), Some(tree)) = (conn.link.as_mut(), tree) {
                // A connection that cannot take the order is ending anyway.
                let _ = link.order(&Order::Disconnect { tree });
            }
        }
        record(
            &self.sink,
            UNMOUNTED,
            Level::Info,
            "smbfs: a share was unmounted",
            &[
                Field {
                    key: "uid",
                    value: FieldValue::UnsignedInt(u64::from(mount.uid)),
                },
                Field {
                    key: "host",
                    value: FieldValue::Str(&mount.host),
                },
                Field {
                    key: "share",
                    value: FieldValue::Str(&mount.share),
                },
                Field {
                    key: "offer",
                    value: FieldValue::UnsignedInt(offer.get()),
                },
                Field {
                    key: "cause",
                    value: FieldValue::Str(cause),
                },
            ],
        );
        self.settled(now, mount.conn);
        self.settle(mount.uid);
    }

    /// The process anchoring a login session ended: every mount of the
    /// session ends with it.
    pub fn peer_exited(&mut self, now: u64, anchor: ProcId) {
        loop {
            let volumes = &self.volumes;
            let next = self.mounts.first(|mount| {
                mount.session == anchor
                    && volumes
                        .get(&mount.offer.get())
                        .is_none_or(|volume| volume.retiring.is_none())
            });
            let Some(offer) = next else {
                return;
            };
            self.retire(now, offer, "session ended", None);
        }
    }

    /// Retire `offer`'s volume for `cause`: what its held files hold is sent
    /// and their opens let go first — within a call's deadline — and the
    /// unmount `ticket` asking is answered once the volume is withdrawn.
    fn retire(&mut self, now: u64, offer: OfferId, cause: &'static str, ticket: Option<u64>) {
        let Some(volume) = self.volumes.get_mut(&offer.get()) else {
            self.withdraw(now, offer, cause);
            if let Some(ticket) = ticket {
                answer(
                    &mut self.host,
                    &mut self.reply,
                    ticket,
                    &NetshareReply::Unmounted,
                );
            }
            return;
        };
        let retiring = volume.retiring.get_or_insert(Retiring {
            cause,
            by: now.saturating_add(CALL_DEADLINE_NS),
            unmounts: Vec::new(),
        });
        if let Some(ticket) = ticket {
            if retiring.unmounts.try_reserve(1).is_ok() {
                retiring.unmounts.push(ticket);
            } else {
                answer(
                    &mut self.host,
                    &mut self.reply,
                    ticket,
                    &NetshareReply::Unmounted,
                );
            }
        }
        let mut held = Vec::new();
        if held.try_reserve(volume.held.files.len()).is_err() {
            self.retired(now, offer);
            return;
        }
        held.extend(volume.held.files.keys().copied());
        if held.is_empty() {
            self.retired(now, offer);
            return;
        }
        for node in held {
            self.unheld(now, offer, NodeId::from_raw(node));
        }
    }

    /// `offer`'s retiring volume has let its held files go, or run out of
    /// time: it is withdrawn, and the unmounts waiting on it answered.
    fn retired(&mut self, now: u64, offer: OfferId) {
        let Some(retiring) = self
            .volumes
            .get_mut(&offer.get())
            .and_then(|volume| volume.retiring.take())
        else {
            return;
        };
        self.withdraw(now, offer, retiring.cause);
        for ticket in retiring.unmounts {
            answer(
                &mut self.host,
                &mut self.reply,
                ticket,
                &NetshareReply::Unmounted,
            );
        }
    }

    /// Act on everything due by `now`.
    pub fn expire(&mut self, now: u64) {
        while let Some((slot, (at, id))) = self.deadlines.peek() {
            if at > now {
                break;
            }
            if self.conns.contains_key(id) {
                self.expire_conn(now, id);
            } else {
                self.deadlines.remove(slot);
            }
        }
    }

    fn expire_conn(&mut self, now: u64, id: ConnId) {
        let offers = self.conns.get(id).map_or(0, |conn| conn.offers.len());
        for at in 0..offers {
            let offer = self
                .conns
                .get(id)
                .and_then(|conn| conn.offers.get(at).copied());
            if let Some(offer) = offer {
                self.expire_volume(now, offer);
            }
        }
        let Some(conn) = self.conns.get_mut(id) else {
            return;
        };
        let (window_over, retry_due) = conn.outage.map_or((false, false), |outage| {
            (
                !outage.past && outage.ends <= now,
                outage.retry_at.is_some_and(|at| at <= now),
            )
        });
        if window_over {
            self.window_ended(id);
        }
        if retry_due {
            self.attempt(now, id);
        }
        let Some(conn) = self.conns.get_mut(id) else {
            return;
        };
        if !matches!(conn.phase, Phase::Ready | Phase::Waiting) && conn.setup_by <= now {
            self.fail(now, id, NetshareError::Unreachable);
            return;
        }
        self.probe(now, id);
        let Some(conn) = self.conns.get_mut(id) else {
            return;
        };
        if conn.calls.is_empty() && conn.linger_until.is_some_and(|until| until <= now) {
            self.end(now, id, None);
            return;
        }
        let uid = conn.uid;
        let mut at = 0;
        while let Some(conn) = self.conns.get_mut(id) {
            let Some(call) = conn.calls.get(at) else {
                break;
            };
            if call.deadline > now {
                at += 1;
                continue;
            }
            let (ticket, op) = conn.drop_call(at);
            self.refuse_on(id, uid, ticket, op, NetshareError::Unreachable);
        }
        self.settled(now, id);
    }

    /// Bring `id`'s linger, deadline and wait-set membership up to date.
    fn settled(&mut self, now: u64, id: ConnId) {
        let watched = self.watched(id);
        let Some(conn) = self.conns.get_mut(id) else {
            return;
        };
        conn.watched_since = watched.then(|| conn.watched_since.unwrap_or(now));
        if conn.phase == Phase::Ready && conn.calls.is_empty() && conn.mounts == 0 {
            conn.linger_until
                .get_or_insert(now.saturating_add(LINGER_NS));
        } else {
            conn.linger_until = None;
        }
        let socket = conn.socket.is_some();
        let arming = conn.link.as_ref().map_or(
            Arming {
                socket,
                ..Arming::default()
            },
            |link| link.arming(socket),
        );
        let own = conn.next_deadline();
        let volumes = conn
            .offers
            .iter()
            .filter_map(|offer| self.volumes.get(&offer.get()))
            .filter_map(Volume::next_deadline)
            .min();
        let deadline = [own, volumes].into_iter().flatten().min();
        if self.host.arm(id, arming).is_err() {
            self.end(now, id, Some(NetshareError::Failed));
            return;
        }
        let slot = id.index();
        match deadline {
            // Room for every slot's entry was made with the slot, so only a
            // broken heap refuses; a connection that could never time out
            // is not kept.
            Some(at) => {
                if self.deadlines.try_insert(slot, (at, id)).is_err() {
                    self.end(now, id, Some(NetshareError::Failed));
                }
            }
            None => {
                self.deadlines.remove(slot);
            }
        }
    }

    /// `id` failed with `error`: a connection mounted shares depend on is
    /// made again, unless the server refused the account or is no longer
    /// the one they are on; any other ends.
    fn fail(&mut self, now: u64, id: ConnId, error: NetshareError) {
        let Some(conn) = self.conns.get(id) else {
            return;
        };
        let refused = matches!(
            error,
            NetshareError::Rejected(_) | NetshareError::PinMismatch(_)
        );
        if conn.mounts > 0 && !refused {
            self.lose(now, id, error);
        } else {
            self.end(now, id, Some(error));
        }
    }

    /// `id`'s connection dropped, or an attempt to make it again failed: its
    /// socket and worker go, and the next attempt waits its turn. On the
    /// first, the mounts start waiting out the grace window — the longest
    /// the server keeps any of their files' opens.
    fn lose(&mut self, now: u64, id: ConnId, error: NetshareError) {
        let Some(conn) = self.conns.get(id) else {
            return;
        };
        if conn.outage.is_none() {
            let grace = conn
                .offers
                .iter()
                .filter_map(|offer| self.volumes.get(&offer.get()))
                .filter_map(|volume| volume.held.longest_durable_ms())
                .max()
                .map_or(DEFAULT_GRACE_NS, |ms| u64::from(ms) * 1_000_000);
            record(
                &self.sink,
                CONNECTION_LOST,
                Level::Warn,
                "smbfs: a connection mounted shares need dropped",
                &[
                    Field {
                        key: "host",
                        value: FieldValue::Str(&conn.host),
                    },
                    Field {
                        key: "address",
                        value: address_field(conn.address),
                    },
                    Field {
                        key: "reason",
                        value: FieldValue::Str(reason(error)),
                    },
                    Field {
                        key: "grace_ms",
                        value: FieldValue::UnsignedInt(grace / 1_000_000),
                    },
                ],
            );
            if let Some(conn) = self.conns.get_mut(id) {
                let flapping = conn
                    .recovered_at
                    .is_some_and(|at| now.saturating_sub(at) < RETRY_MAX_NS);
                conn.flaps = if flapping {
                    conn.flaps.saturating_add(1)
                } else {
                    0
                };
                let wait = match conn.flaps {
                    0 => 0,
                    flaps => backoff(flaps - 1),
                };
                conn.outage = Some(Outage {
                    ends: now.saturating_add(grace),
                    past: false,
                    failed: conn.flaps,
                    retry_at: Some(now.saturating_add(wait)),
                });
            }
            let mut at = 0;
            while let Some(offer) = self
                .conns
                .get(id)
                .and_then(|conn| conn.offers.get(at).copied())
            {
                self.suspend_volume(offer);
                at += 1;
            }
        } else if let Some(outage) = self.conns.get_mut(id).and_then(|conn| conn.outage.as_mut()) {
            outage.retry_at = Some(now.saturating_add(backoff(outage.failed)));
            outage.failed = outage.failed.saturating_add(1);
        }
        let Some(conn) = self.conns.get_mut(id) else {
            return;
        };
        conn.phase = Phase::Waiting;
        conn.link = None;
        conn.echo_at = None;
        conn.rejoins.clear();
        // What a call asked of the connection itself is asked again.
        for call in &mut conn.calls {
            call.request = None;
        }
        if let Some(socket) = conn.socket.take() {
            self.host.close(id, socket);
        }
        self.settled(now, id);
    }

    /// Make the dropped connection `id` again: its host resolved afresh,
    /// unless its calls named an address, and dialled.
    fn attempt(&mut self, now: u64, id: ConnId) {
        let Some(conn) = self.conns.get_mut(id) else {
            return;
        };
        if conn.mounts == 0 && conn.calls.is_empty() {
            // Every mount it was made again for went meanwhile.
            self.end(now, id, None);
            return;
        }
        if let Some(outage) = conn.outage.as_mut() {
            outage.retry_at = None;
        }
        conn.setup_by = now.saturating_add(SETUP_DEADLINE_NS);
        let (asked, host, interface) = (conn.asked, conn.host, conn.interface);
        let started = if let Some(address) = asked {
            self.dial(id, address)
        } else {
            conn.phase = Phase::Resolving;
            self.host
                .resolve(id, &host, interface)
                .map_err(|_| NetshareError::Unreachable)
        };
        if let Err(error) = started {
            self.lose(now, id, error);
        }
    }

    /// The session `id` dropped is made again: every mounted share's tree is
    /// connected again before its volume serves.
    fn rejoin(&mut self, now: u64, id: ConnId) -> Result<(), NetshareError> {
        let conn = self.conns.get_mut(id).ok_or(NetshareError::Failed)?;
        conn.phase = Phase::Rejoining;
        conn.rejoins.clear();
        conn.rejoins
            .try_reserve(conn.offers.len())
            .map_err(|_| NetshareError::Failed)?;
        let mut at = 0;
        while let Some(offer) = self
            .conns
            .get(id)
            .and_then(|conn| conn.offers.get(at).copied())
        {
            at += 1;
            let Some(share) = self.mounts.get(offer).map(|mount| mount.share) else {
                continue;
            };
            let request = self.next_request_id();
            self.order(
                id,
                &Order::Connect {
                    request,
                    share: &share,
                },
            )?;
            if let Some(conn) = self.conns.get_mut(id) {
                // Room was reserved above.
                conn.rejoins.push((request, offer));
            }
        }
        if self
            .conns
            .get(id)
            .is_some_and(|conn| conn.rejoins.is_empty())
        {
            self.recovered(now, id);
        }
        Ok(())
    }

    /// The volume the tree connect `request` is for, while `id` rejoins.
    fn rejoining(&self, id: ConnId, request: u32) -> Option<OfferId> {
        self.conns
            .get(id)?
            .rejoins
            .iter()
            .find(|(asked, _)| *asked == request)
            .map(|(_, offer)| *offer)
    }

    /// The tree connect `request` for `offer`'s share answered: the volume
    /// serves on the tree, or its mount is withdrawn where the share is no
    /// longer one it can serve. Once every tree is back, the connection is.
    fn rejoined(
        &mut self,
        now: u64,
        id: ConnId,
        request: u32,
        offer: OfferId,
        outcome: Result<(u32, ShareType), tairix_smb::SmbError>,
    ) -> Result<(), NetshareError> {
        if let Some(conn) = self.conns.get_mut(id) {
            conn.rejoins.retain(|(asked, _)| *asked != request);
        }
        match outcome {
            Ok((tree, ShareType::Disk)) => {
                if let Some(volume) = self.volumes.get_mut(&offer.get()) {
                    volume.tree = tree;
                }
            }
            Ok((tree, _)) => {
                self.order(id, &Order::Disconnect { tree })?;
                self.withdraw(now, offer, "share no longer a disk share");
            }
            Err(_) => self.withdraw(now, offer, "share refused on reconnect"),
        }
        if self
            .conns
            .get(id)
            .is_some_and(|conn| conn.rejoins.is_empty())
        {
            self.recovered(now, id);
        }
        Ok(())
    }

    /// `id` is whole again: its mounts serve, and its calls go on.
    fn recovered(&mut self, now: u64, id: ConnId) {
        let Some(conn) = self.conns.get_mut(id) else {
            return;
        };
        let failed = conn.outage.take().map_or(0, |outage| outage.failed);
        conn.phase = Phase::Ready;
        conn.heard_at = now;
        conn.echo_at = None;
        conn.recovered_at = Some(now);
        record(
            &self.sink,
            RECONNECTED,
            Level::Info,
            "smbfs: a dropped connection was made again",
            &[
                Field {
                    key: "host",
                    value: FieldValue::Str(&conn.host),
                },
                Field {
                    key: "address",
                    value: address_field(conn.address),
                },
                Field {
                    key: "failed",
                    value: FieldValue::UnsignedInt(u64::from(failed)),
                },
            ],
        );
        let mut at = 0;
        while let Some(offer) = self
            .conns
            .get(id)
            .and_then(|conn| conn.offers.get(at).copied())
        {
            self.resume_volume(now, offer);
            at += 1;
        }
        self.dispatch(now, id);
    }

    /// `id` was not made again within its grace window: its mounts are lost —
    /// every call failing at once — while the attempts go on.
    fn window_ended(&mut self, id: ConnId) {
        let Some(conn) = self.conns.get_mut(id) else {
            return;
        };
        if let Some(outage) = conn.outage.as_mut() {
            outage.past = true;
        }
        record(
            &self.sink,
            MOUNTS_LOST,
            Level::Warn,
            "smbfs: a dropped connection was not made again in time",
            &[
                Field {
                    key: "host",
                    value: FieldValue::Str(&conn.host),
                },
                Field {
                    key: "mounts",
                    value: FieldValue::UnsignedInt(conn.mounts as u64),
                },
            ],
        );
        let mut at = 0;
        while let Some(offer) = self
            .conns
            .get(id)
            .and_then(|conn| conn.offers.get(at).copied())
        {
            self.lose_volume(offer);
            at += 1;
        }
    }

    /// Whether a drop of `id` must be found before what its mounts rest on
    /// it is lost: one of them has a call waiting on its server, a file held
    /// open or a directory leased.
    fn watched(&self, id: ConnId) -> bool {
        self.conns.get(id).is_some_and(|conn| {
            conn.phase == Phase::Ready
                && conn.mounts > 0
                && (!conn.calls.is_empty()
                    || conn
                        .offers
                        .iter()
                        .filter_map(|offer| self.volumes.get(&offer.get()))
                        .any(Volume::rests_on_server))
        })
    }

    /// Ask a watched connection's server whether it still serves it once it
    /// has gone unheard, and take it for dropped once the ask goes
    /// unanswered.
    fn probe(&mut self, now: u64, id: ConnId) {
        let Some(conn) = self.conns.get_mut(id) else {
            return;
        };
        if conn.phase != Phase::Ready || conn.probe_at().is_none_or(|at| at > now) {
            return;
        }
        if conn.echo_at.is_some() {
            self.fail(now, id, NetshareError::Unreachable);
            return;
        }
        conn.echo_at = Some(now);
        if let Err(error) = self.order(id, &Order::Echo) {
            self.fail(now, id, error);
        }
    }

    /// End `id`, answering every call it held with `error` — or as
    /// unreachable, for a connection ended for no error of its own — and
    /// withdrawing its mounts.
    fn end(&mut self, now: u64, id: ConnId, error: Option<NetshareError>) {
        let Some(conn) = self.release(id) else {
            return;
        };
        if let Some(error) = error {
            record(
                &self.sink,
                CONNECTION_ENDED,
                Level::Info,
                "smbfs: a connection ended",
                &[
                    Field {
                        key: "host",
                        value: FieldValue::Str(&conn.host),
                    },
                    Field {
                        key: "address",
                        value: address_field(conn.address),
                    },
                    Field {
                        key: "reason",
                        value: FieldValue::Str(reason(error)),
                    },
                ],
            );
        }
        let refusal = error.unwrap_or(NetshareError::Unreachable);
        for call in &conn.calls {
            self.refuse(call.ticket, conn.uid, &conn.host, call.work.op(), refusal);
        }
        if let Some(held) = self.principals.get_mut(&conn.uid) {
            held.calls = held.calls.saturating_sub(conn.calls.len());
            held.conns.retain(|&held| held != id);
        }
        if conn.mounts > 0 {
            while let Some(offer) = self.mounts.first(|mount| mount.conn == id) {
                self.withdraw(now, offer, "connection lost");
            }
        }
        self.settle(conn.uid);
    }

    /// Take `id` out of its slot, closing its socket and reaping its worker.
    fn release(&mut self, id: ConnId) -> Option<Connection<H::Worker, S>> {
        let conn = self.conns.remove(id)?;
        self.deadlines.remove(id.index());
        // Withdrawing every member cannot strand the connection: it is gone.
        let _ = self.host.arm(id, Arming::default());
        if let Some(socket) = conn.socket {
            self.host.close(id, socket);
        }
        Some(conn)
    }
}

/// The refusal a link's failure answers.
const fn link_refusal(error: LinkError) -> NetshareError {
    match error {
        LinkError::Socket(_) => NetshareError::Unreachable,
        LinkError::Worker => NetshareError::Protocol,
        LinkError::OutOfMemory => NetshareError::Failed,
    }
}

#[cfg(test)]
#[path = "front_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "front/reconnect_tests.rs"]
mod reconnect_tests;
