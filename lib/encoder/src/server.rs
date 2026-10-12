//! [`EncoderServer`]: one encoder's endpoint, over its driver's [`Encoder`].

use tairix_abi::driver::encoder::{
    encode_done_reply, encode_error_reply, encode_state_reply, refusal_reason, Encoder, EncoderOp,
    EncoderRequest, EncoderState, Phase, ENCODER_MAX_REPLY,
};
use tairix_abi::hwtree::HwResource;
use tairix_abi::{DriverError, Errno, ProcId};
use tairix_drvrt::{Recorder, Supplier, SupplierHost};

/// A decision the server records.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Record {
    /// A display engine took the encoder.
    Held {
        /// Its instance.
        holder: ProcId,
    },
    /// The holder ended; the encoder's phase stands for the next to find.
    Abandoned {
        /// The instance that ended.
        holder: ProcId,
    },
    /// The connector moved.
    Moved {
        /// The count of moves now.
        moves: u64,
        /// Whether a sink is connected.
        connected: bool,
    },
    /// A request was refused.
    Refused {
        /// The operation, when the frame named one.
        op: Option<EncoderOp>,
        /// The caller, when the kernel could say.
        caller: Option<ProcId>,
        /// Why.
        reason: Errno,
    },
}

enum Answer {
    State,
    Done,
    /// A wait whose answer comes when the connector moves.
    Posted,
}

/// One encoder's endpoint.
pub struct EncoderServer<E: Encoder, S: SupplierHost, L: Recorder<Record>> {
    encoder: E,
    supplier: S,
    log: L,
    endpoint: u64,
    holder: Option<ProcId>,
    phase: Phase,
    moves: u64,
    /// The holder's posted wait.
    waiting: Option<u64>,
}

impl<E: Encoder, S: SupplierHost, L: Recorder<Record>> EncoderServer<E, S, L> {
    /// The endpoint `endpoint` over `encoder`, from the phase its hardware is
    /// in, asking about its callers through `supplier` and recording to `log`.
    pub fn new(encoder: E, supplier: S, log: L, endpoint: u64) -> Self {
        let phase = encoder.phase();
        Self {
            encoder,
            supplier,
            log,
            endpoint,
            holder: None,
            phase,
            moves: 0,
            waiting: None,
        }
    }

    fn state(&self) -> EncoderState {
        EncoderState {
            moves: self.moves,
            connector: self.encoder.connector(),
            phase: self.phase,
        }
    }

    fn answer(
        &mut self,
        ticket: u64,
        caller: ProcId,
        request: &EncoderRequest,
    ) -> Result<Answer, Errno> {
        let link = request.link();
        if link.endpoint() != self.endpoint {
            return Err(Errno::OutOfRange);
        }
        if !self
            .supplier
            .caller_holds(ticket, &HwResource::request(link))?
        {
            return Err(Errno::PermissionDenied);
        }
        if let EncoderRequest::Describe(_) = request {
            return Ok(Answer::State);
        }
        self.hold(caller)?;
        let op = request.op();
        match *request {
            EncoderRequest::Wait { seen, .. } => {
                if seen != self.moves {
                    return Ok(Answer::State);
                }
                if self.waiting.is_some() {
                    return Err(Errno::Busy);
                }
                self.waiting = Some(ticket);
                Ok(Answer::Posted)
            }
            EncoderRequest::Configure { timing, .. } => {
                let next = self.phase.after(op).ok_or(Errno::OutOfRange)?;
                if self.encoder.connector().mode != Some(timing) {
                    return Err(Errno::Stale);
                }
                self.encoder.configure(&timing).map_err(refusal_reason)?;
                self.phase = next;
                Ok(Answer::Done)
            }
            EncoderRequest::Limit { limits, .. } => {
                if self.encoder.limit(&limits).map_err(refusal_reason)? {
                    self.moved();
                }
                Ok(Answer::Done)
            }
            EncoderRequest::PowerDown(_) if self.phase == Phase::Off => Ok(Answer::Done),
            EncoderRequest::Prepare(_)
            | EncoderRequest::Enable(_)
            | EncoderRequest::Disable(_)
            | EncoderRequest::PowerDown(_) => {
                let next = self.phase.after(op).ok_or(Errno::OutOfRange)?;
                let stepped = match op {
                    EncoderOp::Prepare => self.encoder.prepare(),
                    EncoderOp::Enable => self.encoder.enable(),
                    EncoderOp::Disable => self.encoder.disable(),
                    _ => self.encoder.power_down(),
                };
                stepped.map_err(refusal_reason)?;
                self.phase = next;
                Ok(Answer::Done)
            }
            EncoderRequest::Describe(_) => Ok(Answer::State),
        }
    }

    /// Make `caller` the encoder's holder, unless another instance that
    /// still lives holds it: the kernel's refusal to watch the holder says it
    /// does not.
    fn hold(&mut self, caller: ProcId) -> Result<(), Errno> {
        match self.holder {
            Some(holder) if holder == caller => Ok(()),
            Some(holder) => match self.supplier.watch(holder) {
                Ok(()) => Err(Errno::Busy),
                Err(Errno::NotFound) => {
                    self.peer_exited(holder);
                    self.take(caller)
                }
                Err(reason) => Err(reason),
            },
            None => self.take(caller),
        }
    }

    fn take(&mut self, caller: ProcId) -> Result<(), Errno> {
        self.supplier.watch(caller)?;
        self.holder = Some(caller);
        self.log.record(Record::Held { holder: caller });
        Ok(())
    }

    /// Count a move of the connector and answer the holder's posted wait.
    fn moved(&mut self) {
        self.moves = self.moves.wrapping_add(1);
        self.log.record(Record::Moved {
            moves: self.moves,
            connected: self.encoder.connector().connected,
        });
        if let Some(ticket) = self.waiting.take() {
            self.send(ticket, &Answer::State);
        }
    }

    fn send(&mut self, ticket: u64, answer: &Answer) {
        let mut out = [0u8; ENCODER_MAX_REPLY];
        let encoded = match answer {
            Answer::State => encode_state_reply(&mut out, &self.state()),
            Answer::Done => encode_done_reply(&mut out),
            Answer::Posted => return,
        };
        // A caller that ends before its answer arrives has its exit do the
        // cleaning up; there is nobody left to tell.
        if let Ok(len) = encoded {
            let _ = self.supplier.reply(ticket, &out[..len]);
        }
    }

    fn refuse(
        &mut self,
        ticket: u64,
        op: Option<EncoderOp>,
        caller: Option<ProcId>,
        reason: Errno,
    ) {
        self.log.record(Record::Refused { op, caller, reason });
        let mut out = [0u8; ENCODER_MAX_REPLY];
        if let Ok(len) = encode_error_reply(&mut out, reason) {
            let _ = self.supplier.reply(ticket, &out[..len]);
        }
    }
}

impl<E: Encoder, S: SupplierHost, L: Recorder<Record>> Supplier for EncoderServer<E, S, L> {
    fn serve(&mut self, ticket: u64, frame: &[u8]) {
        let caller = match self.supplier.caller(ticket) {
            Ok(caller) => caller,
            Err(reason) => return self.refuse(ticket, None, None, reason),
        };
        let request = match EncoderRequest::decode(frame) {
            Ok(request) => request,
            Err(reason) => return self.refuse(ticket, None, Some(caller), reason),
        };
        match self.answer(ticket, caller, &request) {
            Ok(answer) => self.send(ticket, &answer),
            Err(reason) => self.refuse(ticket, Some(request.op()), Some(caller), reason),
        }
    }

    /// Release the encoder if `peer` held it, its posted wait with it. The
    /// phase stands: the next engine finds it and brings the output down in
    /// order.
    fn peer_exited(&mut self, peer: ProcId) {
        if self.holder != Some(peer) {
            return;
        }
        self.holder = None;
        self.waiting = None;
        self.log.record(Record::Abandoned { holder: peer });
        self.supplier.unwatch(peer);
    }

    /// Collect what woke the encoder's event source at `index`; a connector
    /// that moved answers the holder's posted wait.
    fn woke(&mut self, index: usize) -> Result<(), DriverError> {
        if self.encoder.service_event(index)? {
            self.moved();
        }
        Ok(())
    }

    fn deadline_us(&self) -> Option<u64> {
        self.encoder.deadline_us()
    }

    /// Run the encoder's check; a connector that moved answers the holder's
    /// posted wait.
    fn expired(&mut self) -> Result<(), DriverError> {
        if self.encoder.expired()? {
            self.moved();
        }
        Ok(())
    }
}
