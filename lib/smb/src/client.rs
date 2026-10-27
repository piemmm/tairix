//! The client (MS-SMB2 §3.2): one principal's connection to one server —
//! negotiation, the session's authentication over SPNEGO, and its trees.
//!
//! A connection carries one session, since connections are shared per
//! principal and endpoint: no worker ever holds two principals' keys. Bytes
//! the socket read go in, bytes to write come out, and what happened is
//! reported as [`Event`]s.
//!
//! - The 3.1.1 pre-auth integrity hash chains the negotiation and every setup
//!   leg but the final response, so the keys bind every byte exchanged.
//! - A session's keys are derived once and installed only once the final
//!   response's signature verifies under them. A re-authentication never
//!   derives them again, so no nonce sequence restarts under the same key.
//! - On 3.0 and 3.0.2 no tree is handed out until validate-negotiate has
//!   passed once; a validation that fails or is refused ends the connection.
//! - A guest or anonymous session, a mechanism the client did not offer, and
//!   a server mechListMIC that does not verify are refused.
//!
//! A server refusing an authentication — a wrong password, a disabled
//! account — is [`Event::Rejected`], and another attempt may follow. Anything
//! the client finds wrong ends the connection: every later call answers that
//! first failure.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::netshare_ipc::{is_share_name, MOUNTS_MAX};
use tairix_inline::ArrayVec;

use crate::connection::{Body, Connection, Message, Outgoing, Received, Sent, Ticket};
use crate::crypt::{KeyInputs, PreauthHash, SessionKeys, NONCE_SEED_LEN};
use crate::file::NO_FILE;
use crate::frame::MAX_COMPOUND;
use crate::header::{Address, Command, Flags, Header};
use crate::ioctl::{IoctlRequest, IoctlResponse};
use crate::negotiate::{
    check_validate_negotiate, validate_negotiate_request, Capabilities, Dialect, NegotiateRequest,
    NegotiateResponse, Negotiated, Offer, SecurityMode, CLIENT_CAPABILITIES,
    FSCTL_VALIDATE_NEGOTIATE_INFO,
};
use crate::ntlm::{self, NtlmInitiator, SIGNATURE_LEN};
use crate::session::{SessionFlags, SessionSetupRequest, SessionSetupResponse};
use crate::simple::encode_empty;
use crate::spnego::{self, NegState, Reply};
use crate::status::NtStatus;
use crate::tree::{TreeConnectRequest, TreeConnectResponse};
use crate::{Refusal, Role, SmbError};

/// The most trees a session holds, connected or connecting: past the mounts a
/// principal may hold, with room for a share listing.
pub const MAX_TREES: usize = 64;

const _: () = assert!(MAX_TREES > MOUNTS_MAX);

/// The tree id a compound's related requests carry, which no tree may take.
const RELATED_TREE: u32 = 0xffff_ffff;

/// What a `SESSION_SETUP` announces: whether the client can follow DFS paths,
/// as its `NEGOTIATE` did.
const SETUP_CAPABILITIES: Capabilities =
    Capabilities::from_bits(CLIENT_CAPABILITIES.bits() & Capabilities::DFS.bits());

/// How a session authenticates: the one mechanism its SPNEGO offer names.
#[derive(Debug)]
pub enum Mechanism {
    /// NTLMv2.
    Ntlm(NtlmInitiator),
}

impl Mechanism {
    const fn oid(&self) -> &'static [u8] {
        match self {
            Self::Ntlm(_) => ntlm::OID,
        }
    }

    fn first_token(&mut self) -> Result<Vec<u8>, SmbError> {
        match self {
            Self::Ntlm(ntlm) => ntlm.negotiate(),
        }
    }

    fn answer(&mut self, token: &[u8]) -> Result<Vec<u8>, SmbError> {
        match self {
            Self::Ntlm(ntlm) => ntlm.authenticate(token),
        }
    }

    fn session_key(&self) -> Option<&[u8]> {
        match self {
            Self::Ntlm(ntlm) => ntlm.session_key().map(|key| &key[..]),
        }
    }

    fn get_mic(&mut self, data: &[u8]) -> Result<[u8; SIGNATURE_LEN], SmbError> {
        match self {
            Self::Ntlm(ntlm) => ntlm.sign(data),
        }
    }

    fn verify_mic(&mut self, data: &[u8], mic: &[u8]) -> Result<(), SmbError> {
        match self {
            Self::Ntlm(ntlm) => ntlm.verify(data, mic),
        }
    }
}

/// A request on a tree, as [`Client::send`] takes it.
#[derive(Copy, Clone)]
pub struct Request<'a> {
    /// The command.
    pub command: Command,
    /// The body that follows the header.
    pub body: Body<'a>,
    /// How many credits the request consumes: one per 64 KiB of payload.
    pub credit_charge: u16,
    /// Flags beyond those the connection decides.
    pub flags: Flags,
}

impl core::fmt::Debug for Request<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Request")
            .field("command", &self.command)
            .field("credit_charge", &self.credit_charge)
            .field("flags", &self.flags)
            .finish_non_exhaustive()
    }
}

/// A connected tree.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Tree {
    /// The id the server gave it.
    pub id: u32,
    /// What the server said of the share.
    pub share: TreeConnectResponse,
}

/// What happened on the connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    /// The negotiation settled: [`Client::negotiated`] says on what.
    Negotiated,
    /// The session is authenticated and keyed: everything on it is sealed.
    Established {
        /// The session's id.
        session_id: u64,
    },
    /// A re-authentication completed; the session keeps its keys.
    Renewed,
    /// The server reports the session's authentication expired; requests
    /// fail until [`Client::reauthenticate`] renews it.
    Expired,
    /// The server refused an authentication with this status.
    Rejected(NtStatus),
    /// A tree is connected and may be used.
    TreeConnected {
        /// The [`Client::connect_tree`] it answers.
        ticket: Ticket,
        /// The tree.
        tree: Tree,
    },
    /// The server refused a tree connect.
    TreeRefused {
        /// The [`Client::connect_tree`] it answers.
        ticket: Ticket,
        /// The server's status.
        status: NtStatus,
    },
    /// A tree was disconnected.
    TreeDisconnected {
        /// The tree's id.
        tree_id: u32,
    },
    /// The session logged off; the connection serves no other.
    LoggedOff,
    /// The final response to a request [`Client::send`] made.
    Response {
        /// The request it answers.
        ticket: Ticket,
        /// The response.
        message: Message,
    },
    /// An unsolicited oplock or lease break.
    Break {
        /// The break.
        message: Message,
    },
}

/// A request the client made for itself, whose response it consumes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Control {
    Negotiate,
    Setup,
    TreeConnect,
    Validate,
    TreeDisconnect(u32),
    Logoff,
}

/// One SPNEGO exchange: the mechanism, what its MIC is made over, and the
/// leg owed to the server once a credit frees. Deliberately not `Debug`: an
/// owed leg carries an NTLMv2 response, which is crackable offline.
struct Exchange {
    mechanism: Mechanism,
    mech_list: Vec<u8>,
    replies: u32,
    answered: bool,
    owed: Option<SessionSetupRequest>,
}

impl Exchange {
    /// The exchange, and the initial token offering its one mechanism.
    fn start(mut mechanism: Mechanism) -> Result<(Self, Vec<u8>), SmbError> {
        let oid = mechanism.oid();
        let token = spnego::initial(oid, &mechanism.first_token()?)?;
        let exchange = Self {
            mech_list: spnego::mech_list(oid)?,
            mechanism,
            replies: 0,
            answered: false,
            owed: None,
        };
        Ok((exchange, token))
    }

    /// The next token, answering a reply that asks for more.
    fn next_token(&mut self, buffer: &[u8]) -> Result<Vec<u8>, SmbError> {
        let reply = self.reply(buffer)?;
        if self.answered
            || !matches!(
                reply.state,
                None | Some(NegState::AcceptIncomplete | NegState::RequestMic)
            )
        {
            return Err(SmbError::Protocol);
        }
        let token = reply.token.ok_or(SmbError::Protocol)?;
        let answer = self.mechanism.answer(token)?;
        self.answered = true;
        let mic = self.mechanism.get_mic(&self.mech_list)?;
        spnego::response(&answer, &mic)
    }

    /// Accept the server's last token: the mechanism complete, no token
    /// beyond it, and the server's mechListMIC verified when it sent one.
    fn finish(&mut self, buffer: &[u8]) -> Result<(), SmbError> {
        if !self.answered || self.mechanism.session_key().is_none() {
            return Err(SmbError::Protocol);
        }
        if buffer.is_empty() {
            return Ok(());
        }
        let reply = self.reply(buffer)?;
        if !matches!(reply.state, None | Some(NegState::AcceptCompleted)) || reply.token.is_some() {
            return Err(SmbError::Protocol);
        }
        match reply.mic {
            Some(mic) => self.mechanism.verify_mic(&self.mech_list, mic),
            None => Ok(()),
        }
    }

    /// A reply checked against the offer: only the first may name the
    /// mechanism, and only as the one offered, and the first must say where
    /// the negotiation stands.
    fn reply<'a>(&mut self, buffer: &'a [u8]) -> Result<Reply<'a>, SmbError> {
        let reply = Reply::decode(buffer)?;
        self.replies = self.replies.saturating_add(1);
        match reply.mechanism {
            None => {}
            Some(oid) if self.replies == 1 && oid == self.mechanism.oid() => {}
            Some(_) if self.replies == 1 => return Err(SmbError::Refused(Refusal::Mechanism)),
            Some(_) => return Err(SmbError::Protocol),
        }
        if self.replies == 1 && reply.state.is_none() {
            return Err(SmbError::Protocol);
        }
        Ok(reply)
    }
}

/// A first authentication in flight.
struct Setup {
    exchange: Exchange,
    /// Zero until the server's first answer names the session.
    session_id: u64,
    /// The session this one replaces, or zero.
    previous_session_id: u64,
    /// The session's pre-auth hash, on 3.1.1.
    preauth: Option<PreauthHash>,
    nonce_seed: [u8; NONCE_SEED_LEN],
}

struct Established {
    id: u64,
    /// The server said the session expired, and no renewal has completed
    /// since.
    expired: bool,
    renewal: Option<Exchange>,
}

enum Session {
    /// None yet, or the last attempt was refused.
    None,
    SettingUp(Setup),
    Established(Established),
    LoggingOff(u64),
    LoggedOff,
}

/// Where validate-negotiate stands.
#[derive(Debug)]
enum Validation {
    /// 3.1.1, whose pre-auth hash already binds the negotiation.
    Unneeded,
    /// Owed on the first tree to connect, or on `tree` once a credit frees.
    Owed {
        request: IoctlRequest,
        tree: Option<u32>,
    },
    InFlight,
    Passed,
}

impl Validation {
    const fn released(&self) -> bool {
        matches!(self, Self::Unneeded | Self::Passed)
    }
}

#[derive(Debug)]
struct Settled {
    negotiated: Negotiated,
    /// The connection's pre-auth hash after the negotiation, on 3.1.1.
    preauth: PreauthHash,
    ntlm_offered: bool,
    validation: Validation,
}

#[derive(Debug)]
enum Phase {
    Negotiating {
        request: NegotiateRequest,
        preauth: PreauthHash,
    },
    Settled(Settled),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum TreeState {
    /// Connected, but held until the negotiation is validated.
    Held,
    Ready,
    Disconnecting,
}

#[derive(Debug)]
struct TreeEntry {
    tree: Tree,
    ticket: Ticket,
    state: TreeState,
}

/// One principal's connection to one server.
pub struct Client {
    conn: Connection,
    offer: Offer,
    phase: Phase,
    session: Session,
    trees: Vec<TreeEntry>,
    control: Vec<(Ticket, Control)>,
    events: VecDeque<Event>,
    failed: Option<SmbError>,
}

impl core::fmt::Debug for Client {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Client")
            .field("conn", &self.conn)
            .field("trees", &self.trees.len())
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// A client opening a connection with `offer`: the `NEGOTIATE` is
    /// queued for the socket.
    ///
    /// # Errors
    ///
    /// [`SmbError::OutOfMemory`], or [`SmbError::TooLarge`] for a server name
    /// past what a negotiate context carries.
    pub fn new(offer: Offer) -> Result<Self, SmbError> {
        let request = offer.request();
        let mut conn = Connection::new();
        let sent = conn.send(&[Outgoing {
            command: Command::Negotiate,
            session_id: 0,
            tree_id: 0,
            body: &|enc| request.encode(enc),
            credit_charge: 0,
            flags: Flags::NONE,
        }])?;
        let mut preauth = PreauthHash::new();
        preauth.update(&sent.clear);
        let mut control = Vec::new();
        track(&mut control, &sent, Control::Negotiate)?;
        Ok(Self {
            conn,
            offer,
            phase: Phase::Negotiating { request, preauth },
            session: Session::None,
            trees: Vec::new(),
            control,
            events: VecDeque::new(),
            failed: None,
        })
    }

    /// What the negotiation settled on, once it has.
    #[must_use]
    pub fn negotiated(&self) -> Option<&Negotiated> {
        match &self.phase {
            Phase::Settled(settled) => Some(&settled.negotiated),
            Phase::Negotiating { .. } => None,
        }
    }

    /// The failure that ended the connection, if one has.
    #[must_use]
    pub const fn failure(&self) -> Option<SmbError> {
        self.failed
    }

    /// The bytes waiting to be written to the socket, taken.
    pub fn take_output(&mut self) -> Vec<u8> {
        self.conn.take_output()
    }

    /// The next thing that happened, if any.
    pub fn next_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// Take `bytes` the socket read, act on every whole frame in them, and
    /// send what they make owed.
    ///
    /// # Errors
    ///
    /// The failure that ends the connection.
    pub fn receive(&mut self, bytes: &[u8]) -> Result<(), SmbError> {
        self.guard()?;
        if let Err(err) = self.conn.receive(bytes) {
            return Err(self.fail(err));
        }
        while let Some(received) = self.conn.next_received() {
            if let Err(err) = self.dispatch(received) {
                return Err(self.fail(err));
            }
        }
        self.flush().map_err(|err| self.fail(err))
    }

    /// Authenticate the session with `mechanism`, its keys to seal under
    /// nonces seeded from `nonce_seed`, which must be fresh CSPRNG output.
    /// After a dropped connection, `previous` names the session this one
    /// replaces, so the server lets the old one go and keeps its durable opens
    /// for this one to reclaim; the offer must then carry the same client
    /// GUID.
    ///
    /// # Errors
    ///
    /// [`SmbError::Refused`] with [`Refusal::Mechanism`] when the server
    /// offers no such mechanism; [`SmbError::Protocol`] before the
    /// negotiation settles, while a session exists, or for a previous session
    /// of id zero; [`SmbError::NoCredit`] when no credit is free; or the
    /// failure that ended the connection.
    pub fn authenticate(
        &mut self,
        mechanism: Mechanism,
        nonce_seed: [u8; NONCE_SEED_LEN],
        previous: Option<u64>,
    ) -> Result<(), SmbError> {
        self.guard()?;
        let settled = self.settled()?;
        if !matches!(self.session, Session::None) || previous == Some(0) {
            return Err(SmbError::Protocol);
        }
        let previous_session_id = previous.unwrap_or(0);
        match mechanism {
            Mechanism::Ntlm(_) if !settled.ntlm_offered => {
                return Err(SmbError::Refused(Refusal::Mechanism));
            }
            Mechanism::Ntlm(_) => {}
        }
        let mut preauth =
            (settled.negotiated.dialect == Dialect::Smb311).then(|| settled.preauth.clone());
        let (exchange, token) = Exchange::start(mechanism)?;
        let request = setup_request(token, previous_session_id);
        let sent = self
            .conn
            .send(&[setup_leg(0, &|enc| request.encode(enc))])?;
        if let Some(preauth) = &mut preauth {
            preauth.update(&sent.clear);
        }
        track(&mut self.control, &sent, Control::Setup)?;
        self.session = Session::SettingUp(Setup {
            exchange,
            session_id: 0,
            previous_session_id,
            preauth,
            nonce_seed,
        });
        Ok(())
    }

    /// Re-authenticate the established session with `mechanism`, as an
    /// expired one must be. Its keys stay as they are.
    ///
    /// # Errors
    ///
    /// [`SmbError::Protocol`] with no established session or while another
    /// renewal is in flight; [`SmbError::NoCredit`]; or the failure that
    /// ended the connection.
    pub fn reauthenticate(&mut self, mechanism: Mechanism) -> Result<(), SmbError> {
        self.guard()?;
        let Session::Established(established) = &self.session else {
            return Err(SmbError::Protocol);
        };
        if established.renewal.is_some() {
            return Err(SmbError::Protocol);
        }
        let id = established.id;
        let (exchange, token) = Exchange::start(mechanism)?;
        let request = setup_request(token, 0);
        let sent = self
            .conn
            .send(&[setup_leg(id, &|enc| request.encode(enc))])?;
        track(&mut self.control, &sent, Control::Setup)?;
        if let Session::Established(established) = &mut self.session {
            established.renewal = Some(exchange);
        }
        Ok(())
    }

    /// Connect the tree `share` on the server dialled. The answer is an
    /// [`Event::TreeConnected`] or [`Event::TreeRefused`] naming the ticket
    /// returned.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] for a name [`is_share_name`] refuses;
    /// [`SmbError::TooLarge`] past [`MAX_TREES`]; [`SmbError::Protocol`] with
    /// no established session; [`SmbError::NoCredit`]; or the failure that
    /// ended the connection.
    pub fn connect_tree(&mut self, share: &str) -> Result<Ticket, SmbError> {
        self.guard()?;
        let session_id = self.session_id()?;
        if !is_share_name(share) {
            return Err(SmbError::Malformed);
        }
        let connecting = self
            .control
            .iter()
            .filter(|(_, control)| *control == Control::TreeConnect)
            .count();
        if self.trees.len() + connecting >= MAX_TREES {
            return Err(SmbError::TooLarge);
        }
        let server = &self.offer.server_name;
        let mut path = String::new();
        path.try_reserve(2 + server.len() + 1 + share.len())
            .map_err(|_| SmbError::OutOfMemory)?;
        path.push_str(r"\\");
        path.push_str(server);
        path.push('\\');
        path.push_str(share);
        let request = TreeConnectRequest { path };
        let sent = self.conn.send(&[Outgoing {
            command: Command::TreeConnect,
            session_id,
            tree_id: 0,
            body: &|enc| request.encode(enc),
            credit_charge: 1,
            flags: Flags::NONE,
        }])?;
        track(&mut self.control, &sent, Control::TreeConnect)
    }

    /// Disconnect the tree `tree_id`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Protocol`] for a tree not connected and handed out;
    /// [`SmbError::NoCredit`]; or the failure that ended the connection.
    pub fn disconnect_tree(&mut self, tree_id: u32) -> Result<(), SmbError> {
        self.guard()?;
        let session_id = self.session_id()?;
        let index = self.ready_tree(tree_id)?;
        let sent = self.conn.send(&[Outgoing {
            command: Command::TreeDisconnect,
            session_id,
            tree_id,
            body: &encode_empty,
            credit_charge: 1,
            flags: Flags::NONE,
        }])?;
        track(&mut self.control, &sent, Control::TreeDisconnect(tree_id))?;
        self.trees[index].state = TreeState::Disconnecting;
        Ok(())
    }

    /// Log the session off, once nothing is outstanding on the connection.
    ///
    /// # Errors
    ///
    /// [`SmbError::Protocol`] with no established session, a renewal in
    /// flight, or requests outstanding; [`SmbError::NoCredit`]; or the
    /// failure that ended the connection.
    pub fn logoff(&mut self) -> Result<(), SmbError> {
        self.guard()?;
        let Session::Established(established) = &self.session else {
            return Err(SmbError::Protocol);
        };
        if established.renewal.is_some() || self.conn.outstanding() > 0 {
            return Err(SmbError::Protocol);
        }
        let id = established.id;
        let sent = self.conn.send(&[Outgoing {
            command: Command::Logoff,
            session_id: id,
            tree_id: 0,
            body: &encode_empty,
            credit_charge: 1,
            flags: Flags::NONE,
        }])?;
        track(&mut self.control, &sent, Control::Logoff)?;
        self.session = Session::LoggingOff(id);
        Ok(())
    }

    /// Send `requests` on the tree `tree_id`, in one frame: a compound when
    /// there are several. Each answer is an [`Event::Response`] naming its
    /// ticket.
    ///
    /// # Errors
    ///
    /// [`SmbError::Protocol`] for a tree not handed out, a command the client
    /// issues itself, or no request; [`SmbError::TooLarge`] past a
    /// compound's bound; [`SmbError::NoCredit`]; or the failure that ended
    /// the connection.
    pub fn send(
        &mut self,
        tree_id: u32,
        requests: &[Request<'_>],
    ) -> Result<Vec<Ticket>, SmbError> {
        self.guard()?;
        let session_id = self.session_id()?;
        self.ready_tree(tree_id)?;
        let mut outgoing: ArrayVec<Outgoing<'_>, MAX_COMPOUND> = ArrayVec::new();
        for request in requests {
            if is_control(request.command) {
                return Err(SmbError::Protocol);
            }
            outgoing
                .try_push(Outgoing {
                    command: request.command,
                    session_id,
                    tree_id,
                    body: request.body,
                    credit_charge: request.credit_charge,
                    flags: request.flags,
                })
                .map_err(|_| SmbError::TooLarge)?;
        }
        Ok(self.conn.send(&outgoing)?.tickets)
    }

    /// Ask the server to cancel the request `ticket` names, which still
    /// completes, normally with `STATUS_CANCELLED`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Protocol`] for a ticket that is not an outstanding
    /// [`Client::send`]'s, or the failure that ended the connection.
    pub fn cancel(&mut self, ticket: Ticket) -> Result<(), SmbError> {
        self.guard()?;
        if self.control.iter().any(|(owned, _)| *owned == ticket) {
            return Err(SmbError::Protocol);
        }
        self.conn.cancel(ticket)
    }

    fn guard(&self) -> Result<(), SmbError> {
        self.failed.map_or(Ok(()), Err)
    }

    /// Latch `err`, and drop every secret the connection still held.
    fn fail(&mut self, err: SmbError) -> SmbError {
        if self.failed.is_none() {
            self.failed = Some(err);
        }
        let session = core::mem::replace(&mut self.session, Session::None);
        if let Session::Established(Established { id, .. }) | Session::LoggingOff(id) = session {
            self.conn.remove_session(id);
        }
        err
    }

    fn settled(&self) -> Result<&Settled, SmbError> {
        match &self.phase {
            Phase::Settled(settled) => Ok(settled),
            Phase::Negotiating { .. } => Err(SmbError::Protocol),
        }
    }

    fn session_id(&self) -> Result<u64, SmbError> {
        match &self.session {
            Session::Established(established) => Ok(established.id),
            _ => Err(SmbError::Protocol),
        }
    }

    /// The index of the tree `tree_id`, connected and handed out.
    fn ready_tree(&self, tree_id: u32) -> Result<usize, SmbError> {
        self.trees
            .iter()
            .position(|entry| entry.tree.id == tree_id && entry.state == TreeState::Ready)
            .ok_or(SmbError::Protocol)
    }

    fn push(&mut self, event: Event) -> Result<(), SmbError> {
        self.events
            .try_reserve(1)
            .map_err(|_| SmbError::OutOfMemory)?;
        self.events.push_back(event);
        Ok(())
    }

    fn dispatch(&mut self, received: Received) -> Result<(), SmbError> {
        match received {
            Received::Break { message } => self.push(Event::Break { message }),
            Received::Response {
                ticket,
                header,
                message,
                sealed,
            } => {
                let Some(at) = self.control.iter().position(|(owned, _)| *owned == ticket) else {
                    self.note(header.status)?;
                    return self.push(Event::Response { ticket, message });
                };
                let (_, control) = self.control.swap_remove(at);
                match control {
                    Control::Negotiate => self.on_negotiate(&header, &message),
                    Control::Setup => self.on_setup(&header, &message, sealed),
                    Control::TreeConnect => self.on_tree_connect(ticket, &header, &message),
                    Control::Validate => self.on_validate(&header, &message),
                    Control::TreeDisconnect(tree_id) => self.on_tree_disconnect(tree_id, &header),
                    Control::Logoff => self.on_logoff(),
                }
            }
        }
    }

    /// Report an expired session once per expiry.
    fn note(&mut self, status: NtStatus) -> Result<(), SmbError> {
        if status != NtStatus::NETWORK_SESSION_EXPIRED {
            return Ok(());
        }
        match &mut self.session {
            Session::Established(established) if !established.expired => {
                established.expired = true;
                self.push(Event::Expired)
            }
            _ => Ok(()),
        }
    }

    fn on_negotiate(&mut self, header: &Header, message: &[u8]) -> Result<(), SmbError> {
        let Phase::Negotiating { request, preauth } = &mut self.phase else {
            return Err(SmbError::Protocol);
        };
        match header.status {
            NtStatus::SUCCESS => {}
            // What a server answers when it speaks none of the dialects offered.
            NtStatus::NOT_SUPPORTED => return Err(SmbError::Refused(Refusal::Dialect)),
            status => return Err(SmbError::Status(status)),
        }
        let response = NegotiateResponse::decode(message)?;
        let negotiated = self.offer.accept(&response)?;
        let ntlm_offered = offers(&response.security_buffer, ntlm::OID)?;
        let mut preauth = core::mem::take(preauth);
        let validation = match negotiated.dialect {
            Dialect::Smb311 => {
                preauth.update(message);
                Validation::Unneeded
            }
            Dialect::Smb300 | Dialect::Smb302 => Validation::Owed {
                request: validate_negotiate_request(request)?,
                tree: None,
            },
        };
        self.phase = Phase::Settled(Settled {
            negotiated,
            preauth,
            ntlm_offered,
            validation,
        });
        self.push(Event::Negotiated)
    }

    fn on_setup(&mut self, header: &Header, message: &[u8], sealed: bool) -> Result<(), SmbError> {
        match core::mem::replace(&mut self.session, Session::None) {
            Session::SettingUp(setup) => self.on_first_setup(setup, header, message),
            Session::Established(mut established) => {
                let outcome = match established.renewal.take() {
                    // A renewal is sent under the session's keys, and answered
                    // under them.
                    Some(_) if !sealed || header.session_id != established.id => {
                        Err(SmbError::Integrity)
                    }
                    Some(renewal) => self.on_renewal(&mut established, renewal, header, message),
                    None => Err(SmbError::Protocol),
                };
                self.session = Session::Established(established);
                outcome
            }
            other => {
                self.session = other;
                Err(SmbError::Protocol)
            }
        }
    }

    fn on_first_setup(
        &mut self,
        mut setup: Setup,
        header: &Header,
        message: &[u8],
    ) -> Result<(), SmbError> {
        match header.status {
            NtStatus::MORE_PROCESSING_REQUIRED => {
                if header.session_id == 0
                    || (setup.session_id != 0 && header.session_id != setup.session_id)
                {
                    return Err(SmbError::Protocol);
                }
                setup.session_id = header.session_id;
                if let Some(preauth) = &mut setup.preauth {
                    preauth.update(message);
                }
                let response = SessionSetupResponse::decode(message)?;
                let token = setup.exchange.next_token(&response.security_buffer)?;
                setup.exchange.owed = Some(setup_request(token, setup.previous_session_id));
                self.session = Session::SettingUp(setup);
                Ok(())
            }
            NtStatus::SUCCESS => {
                let response = SessionSetupResponse::decode(message)?;
                refuse_unprotected(response.session_flags)?;
                if setup.session_id == 0 || header.session_id != setup.session_id {
                    return Err(SmbError::Protocol);
                }
                let session_key = setup
                    .exchange
                    .mechanism
                    .session_key()
                    .ok_or(SmbError::Protocol)?;
                let negotiated = &self.settled()?.negotiated;
                let preauth = setup.preauth.take().unwrap_or_default();
                let keys = SessionKeys::derive(
                    &KeyInputs {
                        dialect: negotiated.dialect,
                        cipher: negotiated.cipher,
                        signing: negotiated.signing,
                        session_key,
                        preauth: &preauth,
                    },
                    Role::Client,
                    &setup.nonce_seed,
                )?;
                keys.verify(message)?;
                setup.exchange.finish(&response.security_buffer)?;
                self.conn.install_keys(setup.session_id, keys)?;
                self.session = Session::Established(Established {
                    id: setup.session_id,
                    expired: false,
                    renewal: None,
                });
                self.push(Event::Established {
                    session_id: setup.session_id,
                })
            }
            status if status.is_error() => self.push(Event::Rejected(status)),
            _ => Err(SmbError::Protocol),
        }
    }

    fn on_renewal(
        &mut self,
        established: &mut Established,
        mut renewal: Exchange,
        header: &Header,
        message: &[u8],
    ) -> Result<(), SmbError> {
        match header.status {
            NtStatus::MORE_PROCESSING_REQUIRED => {
                let response = SessionSetupResponse::decode(message)?;
                let token = renewal.next_token(&response.security_buffer)?;
                renewal.owed = Some(setup_request(token, 0));
                established.renewal = Some(renewal);
                Ok(())
            }
            NtStatus::SUCCESS => {
                let response = SessionSetupResponse::decode(message)?;
                refuse_unprotected(response.session_flags)?;
                renewal.finish(&response.security_buffer)?;
                established.expired = false;
                self.push(Event::Renewed)
            }
            status if status.is_error() => self.push(Event::Rejected(status)),
            _ => Err(SmbError::Protocol),
        }
    }

    fn on_tree_connect(
        &mut self,
        ticket: Ticket,
        header: &Header,
        message: &[u8],
    ) -> Result<(), SmbError> {
        match header.status {
            NtStatus::SUCCESS => {}
            status if status.is_error() => {
                self.note(status)?;
                return self.push(Event::TreeRefused { ticket, status });
            }
            _ => return Err(SmbError::Protocol),
        }
        let Address::Sync { tree_id } = header.address else {
            return Err(SmbError::Protocol);
        };
        if tree_id == 0
            || tree_id == RELATED_TREE
            || self.trees.iter().any(|entry| entry.tree.id == tree_id)
        {
            return Err(SmbError::Protocol);
        }
        let tree = Tree {
            id: tree_id,
            share: TreeConnectResponse::decode(message)?,
        };
        let released = self.settled()?.validation.released();
        self.trees
            .try_reserve(1)
            .map_err(|_| SmbError::OutOfMemory)?;
        self.trees.push(TreeEntry {
            tree,
            ticket,
            state: if released {
                TreeState::Ready
            } else {
                TreeState::Held
            },
        });
        if released {
            return self.push(Event::TreeConnected { ticket, tree });
        }
        if let Phase::Settled(Settled {
            validation: Validation::Owed {
                tree: owed @ None, ..
            },
            ..
        }) = &mut self.phase
        {
            *owed = Some(tree_id);
        }
        Ok(())
    }

    fn on_validate(&mut self, header: &Header, message: &[u8]) -> Result<(), SmbError> {
        if header.status != NtStatus::SUCCESS {
            return Err(SmbError::Refused(Refusal::Validation));
        }
        let response = IoctlResponse::decode(message)?;
        if response.ctl_code != FSCTL_VALIDATE_NEGOTIATE_INFO || response.file_id != NO_FILE {
            return Err(SmbError::Protocol);
        }
        let Phase::Settled(settled) = &mut self.phase else {
            return Err(SmbError::Protocol);
        };
        if !matches!(settled.validation, Validation::InFlight) {
            return Err(SmbError::Protocol);
        }
        check_validate_negotiate(&response.output, &settled.negotiated)?;
        settled.validation = Validation::Passed;
        for at in 0..self.trees.len() {
            let entry = &mut self.trees[at];
            if entry.state == TreeState::Held {
                entry.state = TreeState::Ready;
                let (ticket, tree) = (entry.ticket, entry.tree);
                self.push(Event::TreeConnected { ticket, tree })?;
            }
        }
        Ok(())
    }

    fn on_tree_disconnect(&mut self, tree_id: u32, header: &Header) -> Result<(), SmbError> {
        self.note(header.status)?;
        self.trees.retain(|entry| entry.tree.id != tree_id);
        self.push(Event::TreeDisconnected { tree_id })
    }

    fn on_logoff(&mut self) -> Result<(), SmbError> {
        let Session::LoggingOff(id) = self.session else {
            return Err(SmbError::Protocol);
        };
        self.conn.remove_session(id);
        self.trees.clear();
        self.session = Session::LoggedOff;
        self.push(Event::LoggedOff)
    }

    /// Send what is owed and a credit now covers: a setup leg, then the
    /// validation.
    fn flush(&mut self) -> Result<(), SmbError> {
        let (session_id, exchange, preauth) = match &mut self.session {
            Session::SettingUp(setup) => (
                setup.session_id,
                Some(&mut setup.exchange),
                setup.preauth.as_mut(),
            ),
            Session::Established(established) => {
                (established.id, established.renewal.as_mut(), None)
            }
            _ => (0, None, None),
        };
        if let Some(exchange) = exchange {
            if let Some(request) = exchange.owed.take() {
                match self
                    .conn
                    .send(&[setup_leg(session_id, &|enc| request.encode(enc))])
                {
                    Ok(sent) => {
                        if let Some(preauth) = preauth {
                            preauth.update(&sent.clear);
                        }
                        track(&mut self.control, &sent, Control::Setup)?;
                    }
                    Err(SmbError::NoCredit) => exchange.owed = Some(request),
                    Err(err) => return Err(err),
                }
            }
        }
        let Session::Established(established) = &self.session else {
            return Ok(());
        };
        let Phase::Settled(settled) = &mut self.phase else {
            return Ok(());
        };
        let Validation::Owed {
            request,
            tree: Some(tree_id),
        } = &settled.validation
        else {
            return Ok(());
        };
        let sent = match self.conn.send(&[Outgoing {
            command: Command::Ioctl,
            session_id: established.id,
            tree_id: *tree_id,
            body: &|enc| request.encode(enc),
            credit_charge: 1,
            flags: Flags::NONE,
        }]) {
            Ok(sent) => sent,
            Err(SmbError::NoCredit) => return Ok(()),
            Err(err) => return Err(err),
        };
        settled.validation = Validation::InFlight;
        track(&mut self.control, &sent, Control::Validate).map(|_| ())
    }
}

/// Record that `sent` — one request — is the client's own, of `kind`.
fn track(
    control: &mut Vec<(Ticket, Control)>,
    sent: &Sent,
    kind: Control,
) -> Result<Ticket, SmbError> {
    let [ticket] = sent.tickets[..] else {
        return Err(SmbError::Protocol);
    };
    control.try_reserve(1).map_err(|_| SmbError::OutOfMemory)?;
    control.push((ticket, kind));
    Ok(ticket)
}

/// One setup leg carrying `security_buffer`. Every leg of a first
/// authentication names the session it replaces: a server lets the old one go
/// on the leg that completes.
fn setup_request(security_buffer: Vec<u8>, previous_session_id: u64) -> SessionSetupRequest {
    SessionSetupRequest {
        binding: false,
        security_mode: SecurityMode::SIGNING_ENABLED.with(SecurityMode::SIGNING_REQUIRED),
        capabilities: SETUP_CAPABILITIES,
        previous_session_id,
        security_buffer,
    }
}

const fn setup_leg(session_id: u64, body: Body<'_>) -> Outgoing<'_> {
    Outgoing {
        command: Command::SessionSetup,
        session_id,
        tree_id: 0,
        body,
        credit_charge: 1,
        flags: Flags::NONE,
    }
}

/// Whether the server's `NEGOTIATE` hints offer `oid`. A server sending none
/// leaves the choice to the client.
fn offers(hints: &[u8], oid: &[u8]) -> Result<bool, SmbError> {
    if hints.is_empty() {
        return Ok(true);
    }
    Ok(spnego::offered(hints)?.contains(&oid))
}

/// Refuse a session nobody authenticated: a guest's or an anonymous one,
/// whose keys no secret protects.
const fn refuse_unprotected(flags: SessionFlags) -> Result<(), SmbError> {
    if flags.contains(SessionFlags::IS_GUEST) || flags.contains(SessionFlags::IS_NULL) {
        Err(SmbError::Refused(Refusal::Guest))
    } else {
        Ok(())
    }
}

/// Whether `command` is one the client issues and answers for itself.
const fn is_control(command: Command) -> bool {
    matches!(
        command,
        Command::Negotiate
            | Command::SessionSetup
            | Command::Logoff
            | Command::TreeConnect
            | Command::TreeDisconnect
            | Command::Cancel
    )
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "client_file_tests.rs"]
mod file_tests;
