//! A connection's worker: one server's [`Client`], run as a capability-empty
//! sandbox session ([`tairix_sandbox::session`]).
//!
//! Everything the server sends is parsed here — SMB, NTLM, SPNEGO, DCE/RPC,
//! and every decryption — by a process that holds no authority: it reaches
//! nothing but the pipe to the front, so a server that compromises it gains a
//! process that can only lie about that one connection. The worker is a pure
//! reactor: it acts on an [`Order`] and answers with [`Report`]s, and keeps
//! no clock of its own — the front's deadlines bound every request.
//!
//! A server grants credits as it pleases, and one granting a single credit
//! per response serialises everything on the connection. A request with no
//! credit to go out on waits here, in the order it was asked, and goes as
//! soon as a response frees one.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_sandbox::proto::ProtoError;
use tairix_sandbox::session::{FrameOut, SessionService, SessionStep};
use tairix_smb::client::{Client, Event, Mechanism, Request};
use tairix_smb::connection::Ticket;
use tairix_smb::header::Flags;
use tairix_smb::negotiate::{Capabilities, Dialect, Offer, SALT_LEN};
use tairix_smb::ntlm::{NtlmCredential, NtlmInitiator, NtlmInputs};
use tairix_smb::rpc::ShareListing;
use tairix_smb::wire::Encoder;
use tairix_smb::SmbError;

use crate::files::{Files, Halt, Tell};
use crate::wire::{Credential, Leasing, Order, Report, CHUNK_MAX};

/// The share every listing is made over.
const IPC_SHARE: &str = "IPC$";

/// The service a file server's NTLM target names, ahead of its name.
const SERVICE: &str = "cifs/";

/// The `IPC$` tree, which listings share.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Ipc {
    /// No listing has needed it.
    Absent,
    /// A listing needs it; its connect waits for a credit.
    Wanted,
    /// Its connect is out.
    Connecting(Ticket),
    /// Connected.
    Connected(u32),
}

/// One listing, and the request on `IPC$` whose answer it awaits.
struct Listing {
    request: u32,
    exchange: ShareListing,
    awaiting: Option<Ticket>,
}

/// The worker stops: its pipe failed, or the front broke the protocol.
struct Stop;

impl From<ProtoError> for Stop {
    fn from(_: ProtoError) -> Self {
        Self
    }
}

impl From<Halt> for Stop {
    fn from(_: Halt) -> Self {
        Self
    }
}

/// How the worker reports: each frame encoded in one buffer and written.
struct Teller<'a> {
    frame: &'a mut Vec<u8>,
    out: &'a mut dyn FrameOut,
}

impl Tell for Teller<'_> {
    fn tell(&mut self, report: &Report<'_>) -> Result<(), Halt> {
        // A report the worker cannot encode is one it should never make.
        report.encode(self.frame).map_err(|_| Halt)?;
        self.out.frame(self.frame).map_err(|_| Halt)
    }
}

/// One connection's client and the requests in flight on it.
pub struct Worker {
    client: Option<Client>,
    /// The server's name as dialled.
    server: String,
    /// `cifs/<server>`: the service NTLM names.
    target: String,
    ipc: Ipc,
    listings: Vec<Listing>,
    /// Tree connects waiting for a credit, in the order asked.
    connects: VecDeque<(u32, String)>,
    /// Tree connects out, by the ticket each awaits.
    connecting: Vec<(Ticket, u32)>,
    /// The file operations on the connection's trees.
    files: Files,
    /// Whether the connection's end has been reported.
    failed: bool,
    frame: Vec<u8>,
}

impl Default for Worker {
    fn default() -> Self {
        Self::new()
    }
}

impl Worker {
    /// A worker with no connection yet: the front's first order opens one.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            client: None,
            server: String::new(),
            target: String::new(),
            ipc: Ipc::Absent,
            listings: Vec::new(),
            connects: VecDeque::new(),
            connecting: Vec::new(),
            files: Files::new(),
            failed: false,
            frame: Vec::new(),
        }
    }

    fn report(&mut self, report: &Report<'_>, out: &mut dyn FrameOut) -> Result<(), Stop> {
        // A report the worker cannot encode is one it should never make.
        report.encode(&mut self.frame).map_err(|_| Stop)?;
        out.frame(&self.frame)?;
        Ok(())
    }

    fn obey(&mut self, order: Order<'_>, out: &mut dyn FrameOut) -> Result<(), Stop> {
        match order {
            Order::Open {
                floor,
                client_guid,
                salt,
                server,
            } => self.open(floor, client_guid, salt, server, out),
            Order::Received(bytes) => self.act(out, |client, _| client.receive(bytes)),
            Order::SignIn {
                credential,
                nonce_seed,
                previous,
            } => self.act(out, |client, target| {
                client.authenticate(mechanism(&credential, target)?, nonce_seed, previous)
            }),
            Order::Renew(credential) => self.act(out, |client, target| {
                client.reauthenticate(mechanism(&credential, target)?)
            }),
            Order::List { request, special } => {
                self.held()?;
                if self.failed {
                    return Ok(());
                }
                let listed = ShareListing::new(&self.server, special).and_then(|listing| {
                    self.listings
                        .try_reserve(1)
                        .map_err(|_| SmbError::OutOfMemory)?;
                    self.listings.push(Listing {
                        request,
                        exchange: listing,
                        awaiting: None,
                    });
                    Ok(())
                });
                match listed {
                    Ok(()) => {
                        if self.ipc == Ipc::Absent {
                            self.ipc = Ipc::Wanted;
                        }
                        self.pump(out)
                    }
                    Err(error) => self.refuse(request, error, out),
                }
            }
            Order::Connect { request, share } => {
                self.held()?;
                if self.failed {
                    return Ok(());
                }
                let held = owned(share).and_then(|share| {
                    self.connects
                        .try_reserve(1)
                        .map_err(|_| SmbError::OutOfMemory)?;
                    self.connects.push_back((request, share));
                    Ok(())
                });
                match held {
                    Ok(()) => self.pump(out),
                    Err(error) => self.refuse(request, error, out),
                }
            }
            // No credit means requests are out, whose answers say as much.
            Order::Echo => self.act(out, |client, _| match client.echo() {
                Err(SmbError::NoCredit) => Ok(()),
                echoed => echoed,
            }),
            Order::Disconnect { tree } => {
                self.files.forget_tree(tree);
                self.act(out, |client, _| {
                    // A tree the server already dropped needs no disconnect.
                    let _ = client.disconnect_tree(tree);
                    Ok(())
                })
            }
            Order::Stage { request, chunk } => {
                self.held()?;
                if self.failed {
                    return Ok(());
                }
                Ok(self.files.stage(request, chunk)?)
            }
            Order::File { request, tree, op } => {
                self.held()?;
                if self.failed {
                    return Ok(());
                }
                match self.files.order(request, tree, &op) {
                    Ok(()) => self.pump(out),
                    Err(error) => self.refuse(request, error, out),
                }
            }
        }
    }

    /// Refuse an order before the connection is opened: the front sends
    /// none.
    fn held(&self) -> Result<(), Stop> {
        if self.client.is_none() && !self.failed {
            return Err(Stop);
        }
        Ok(())
    }

    /// Carry `action` out on the client, then act on what it caused.
    fn act(
        &mut self,
        out: &mut dyn FrameOut,
        action: impl FnOnce(&mut Client, &str) -> Result<(), SmbError>,
    ) -> Result<(), Stop> {
        self.held()?;
        if self.failed {
            return Ok(());
        }
        let Some(client) = self.client.as_mut() else {
            return Err(Stop);
        };
        match action(client, &self.target) {
            Ok(()) => self.pump(out),
            Err(error) => self.fail(error, out),
        }
    }

    fn open(
        &mut self,
        floor: Dialect,
        client_guid: [u8; 16],
        salt: [u8; SALT_LEN],
        server: &str,
        out: &mut dyn FrameOut,
    ) -> Result<(), Stop> {
        if self.client.is_some() || self.failed {
            return Err(Stop);
        }
        let opened = owned(server).and_then(|server_name| {
            let mut target = String::new();
            target
                .try_reserve_exact(SERVICE.len() + server_name.len())
                .map_err(|_| SmbError::OutOfMemory)?;
            target.push_str(SERVICE);
            target.push_str(&server_name);
            let mut kept = String::new();
            kept.try_reserve_exact(server_name.len())
                .map_err(|_| SmbError::OutOfMemory)?;
            kept.push_str(&server_name);
            let client = Client::new(Offer {
                floor,
                client_guid,
                salt,
                server_name,
            })?;
            Ok((client, kept, target))
        });
        match opened {
            Ok((client, server, target)) => {
                self.client = Some(client);
                self.server = server;
                self.target = target;
                self.pump(out)
            }
            Err(error) => {
                self.failed = true;
                self.report(&Report::Failed(error), out)
            }
        }
    }

    fn refuse(
        &mut self,
        request: u32,
        error: SmbError,
        out: &mut dyn FrameOut,
    ) -> Result<(), Stop> {
        self.report(&Report::Refused { request, error }, out)
    }

    /// Report the connection's end once; nothing more is done on it.
    fn fail(&mut self, error: SmbError, out: &mut dyn FrameOut) -> Result<(), Stop> {
        if self.failed {
            return Ok(());
        }
        self.failed = true;
        self.report(&Report::Failed(error), out)
    }

    /// Act on everything the client has to say, send what waits for a
    /// credit, and hand the socket's bytes over, until nothing is left.
    fn pump(&mut self, out: &mut dyn FrameOut) -> Result<(), Stop> {
        loop {
            let mut acted = false;
            while let Some(event) = self.client.as_mut().and_then(Client::next_event) {
                acted = true;
                if let Err(error) = self.on_event(event, out)? {
                    return self.fail(error, out);
                }
            }
            match self.send_waiting(out)? {
                Ok(sent) => acted |= sent,
                Err(error) => return self.fail(error, out),
            }
            acted |= self.flush(out)?;
            if !acted {
                return Ok(());
            }
        }
    }

    /// Hand the client's output to the socket, in chunks; whether there was
    /// any.
    fn flush(&mut self, out: &mut dyn FrameOut) -> Result<bool, Stop> {
        let Some(client) = self.client.as_mut() else {
            return Ok(false);
        };
        let output = client.take_output();
        for chunk in output.chunks(CHUNK_MAX) {
            self.report(&Report::Send(chunk), out)?;
        }
        Ok(!output.is_empty())
    }

    /// Send what waits for a credit, in order, stopping at the first that
    /// finds none; whether anything went, or the error that ends the
    /// connection.
    fn send_waiting(&mut self, out: &mut dyn FrameOut) -> Result<Result<bool, SmbError>, Stop> {
        let mut sent = false;
        if self.ipc == Ipc::Wanted {
            let Some(client) = self.client.as_mut() else {
                return Ok(Ok(false));
            };
            match client.connect_tree(IPC_SHARE) {
                Ok(ticket) => {
                    self.ipc = Ipc::Connecting(ticket);
                    sent = true;
                }
                Err(SmbError::NoCredit) => return Ok(Ok(false)),
                Err(error) => {
                    self.ipc = Ipc::Absent;
                    self.refuse_listings(error, out)?;
                }
            }
        }
        if let Ipc::Connected(tree) = self.ipc {
            let mut at = 0;
            while at < self.listings.len() {
                let Self {
                    client, listings, ..
                } = self;
                let Some(client) = client.as_mut() else {
                    return Ok(Ok(sent));
                };
                let listing = &mut listings[at];
                if listing.awaiting.is_some() {
                    at += 1;
                    continue;
                }
                let Some(command) = listing.exchange.command() else {
                    let done = self.listings.swap_remove(at);
                    self.finish(done, out)?;
                    continue;
                };
                let step = &listing.exchange;
                let body = |enc: &mut Encoder| step.encode(enc);
                let request = Request {
                    command,
                    body: &body,
                    credit_charge: 1,
                    flags: Flags::NONE,
                };
                match client.send(tree, &[request]) {
                    Ok(tickets) => {
                        listing.awaiting = tickets.first().copied();
                        sent = true;
                        at += 1;
                    }
                    Err(SmbError::NoCredit) => return Ok(Ok(sent)),
                    Err(error) => return Ok(Err(error)),
                }
            }
        }
        if let Some(client) = self.client.as_mut() {
            let mut teller = Teller {
                frame: &mut self.frame,
                out,
            };
            match self.files.send(client, &mut teller)? {
                Ok(went) => sent |= went,
                Err(error) => return Ok(Err(error)),
            }
        }
        while let Some((request, share)) = self.connects.front() {
            let request = *request;
            let Some(client) = self.client.as_mut() else {
                return Ok(Ok(sent));
            };
            match client.connect_tree(share) {
                Ok(ticket) => {
                    self.connects.pop_front();
                    if self.connecting.try_reserve(1).is_err() {
                        return Ok(Err(SmbError::OutOfMemory));
                    }
                    self.connecting.push((ticket, request));
                    sent = true;
                }
                Err(SmbError::NoCredit) => return Ok(Ok(sent)),
                Err(error) => {
                    self.connects.pop_front();
                    self.refuse(request, error, out)?;
                }
            }
        }
        Ok(Ok(sent))
    }

    /// Report what a finished listing found, or why it found nothing.
    fn finish(&mut self, done: Listing, out: &mut dyn FrameOut) -> Result<(), Stop> {
        let request = done.request;
        match done.exchange.finish() {
            Ok(list) => {
                for share in &list.shares {
                    self.report(
                        &Report::Share {
                            request,
                            name: &share.name,
                            special: share.special,
                            remark: &share.remark,
                        },
                        out,
                    )?;
                }
                self.report(
                    &Report::Listed {
                        request,
                        hidden: list.hidden,
                        unnamed: list.unnamed,
                        unlisted: list.unlisted,
                    },
                    out,
                )
            }
            Err(error) => self.refuse(request, error, out),
        }
    }

    fn refuse_listings(&mut self, error: SmbError, out: &mut dyn FrameOut) -> Result<(), Stop> {
        for listing in core::mem::take(&mut self.listings) {
            self.refuse(listing.request, error, out)?;
        }
        Ok(())
    }

    /// Act on one event; the error that ends the connection, if it does.
    fn on_event(
        &mut self,
        event: Event,
        out: &mut dyn FrameOut,
    ) -> Result<Result<(), SmbError>, Stop> {
        match event {
            Event::Negotiated => {
                let Some(negotiated) = self.client.as_ref().and_then(Client::negotiated) else {
                    return Ok(Err(SmbError::Protocol));
                };
                let capabilities = negotiated.capabilities;
                let leasing = if capabilities.contains(Capabilities::DIRECTORY_LEASING) {
                    Leasing::Directories
                } else if capabilities.contains(Capabilities::LEASING) {
                    Leasing::Files
                } else {
                    Leasing::None
                };
                let report = Report::Negotiated {
                    server_guid: negotiated.server_guid,
                    dialect: negotiated.dialect,
                    leasing,
                };
                self.report(&report, out)?;
            }
            Event::Established { session_id } => {
                self.report(
                    &Report::Established {
                        session: session_id,
                    },
                    out,
                )?;
            }
            Event::Renewed => self.report(&Report::Renewed, out)?,
            Event::Expired => self.report(&Report::Expired, out)?,
            Event::Rejected(status) => self.report(&Report::Rejected(status), out)?,
            Event::TreeConnected { ticket, tree } => {
                if self.ipc == Ipc::Connecting(ticket) {
                    self.ipc = Ipc::Connected(tree.id);
                } else if let Some(request) = self.take_connecting(ticket) {
                    self.report(
                        &Report::Connected {
                            request,
                            tree: tree.id,
                            kind: tree.share.share_type,
                        },
                        out,
                    )?;
                } else {
                    return Ok(Err(SmbError::Protocol));
                }
            }
            Event::TreeRefused { ticket, status } => {
                if self.ipc == Ipc::Connecting(ticket) {
                    self.ipc = Ipc::Absent;
                    self.refuse_listings(SmbError::Status(status), out)?;
                } else if let Some(request) = self.take_connecting(ticket) {
                    self.refuse(request, SmbError::Status(status), out)?;
                } else {
                    return Ok(Err(SmbError::Protocol));
                }
            }
            Event::TreeDisconnected { tree_id } => {
                if self.ipc == Ipc::Connected(tree_id) {
                    self.ipc = Ipc::Absent;
                    self.refuse_listings(SmbError::Protocol, out)?;
                }
            }
            Event::Response { ticket, message } if self.files.owns(ticket) => {
                let mut teller = Teller {
                    frame: &mut self.frame,
                    out,
                };
                self.files.answer(ticket, message, &mut teller)?;
            }
            Event::Response { ticket, message } => {
                let Some(at) = self
                    .listings
                    .iter()
                    .position(|listing| listing.awaiting == Some(ticket))
                else {
                    return Ok(Err(SmbError::Protocol));
                };
                let listing = &mut self.listings[at];
                listing.exchange.answer(&message);
                listing.awaiting = None;
                if listing.exchange.command().is_none() {
                    let done = self.listings.swap_remove(at);
                    self.finish(done, out)?;
                }
            }
            // This connection asks for no lease, so a break names nothing
            // it holds; the server times it out.
            Event::Break { message } => {
                let mut teller = Teller {
                    frame: &mut self.frame,
                    out,
                };
                return Ok(Files::broke(&message, &mut teller)?);
            }
            Event::LoggedOff => {}
        }
        Ok(Ok(()))
    }

    fn take_connecting(&mut self, ticket: Ticket) -> Option<u32> {
        let at = self
            .connecting
            .iter()
            .position(|(waiting, _)| *waiting == ticket)?;
        Some(self.connecting.swap_remove(at).1)
    }
}

impl SessionService for Worker {
    fn handle(&mut self, request: &[u8], out: &mut dyn FrameOut) -> SessionStep {
        let Ok(order) = Order::decode(request) else {
            return SessionStep::Finished;
        };
        match self.obey(order, out) {
            Ok(()) => SessionStep::Continue,
            Err(Stop) => SessionStep::Finished,
        }
    }
}

/// The mechanism `credential` signs in with, to the service `target` names.
fn mechanism(credential: &Credential<'_>, target: &str) -> Result<Mechanism, SmbError> {
    let held = NtlmCredential::keyed(credential.user, credential.domain, credential.key)?;
    let inputs = NtlmInputs {
        client_challenge: credential.client_challenge,
        now: credential.now,
    };
    Ok(Mechanism::Ntlm(NtlmInitiator::new(held, target, inputs)?))
}

fn owned(text: &str) -> Result<String, SmbError> {
    let mut out = String::new();
    out.try_reserve_exact(text.len())
        .map_err(|_| SmbError::OutOfMemory)?;
    out.push_str(text);
    Ok(out)
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
