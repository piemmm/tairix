//! [`CodecServer`]: one codec's endpoint, over its driver's [`Codec`].

use tairix_abi::driver::codec::{
    encode_describe_reply, encode_done_reply, encode_error_reply, encode_gain_reply,
    refusal_reason as refusal, Codec, CodecOp, CodecRequest, DaiLink, CODEC_MAX_REPLY,
};
use tairix_abi::hwtree::HwResource;
use tairix_abi::{Errno, ProcId};
use tairix_drvrt::SupplierHost;

/// A decision the server records.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Record {
    /// An interface's driver took the codec.
    Held {
        /// Its instance.
        holder: ProcId,
    },
    /// The holder ended, and the codec was stopped.
    Abandoned {
        /// The instance that ended.
        holder: ProcId,
    },
    /// A request was refused.
    Refused {
        /// The operation, when the frame named one.
        op: Option<CodecOp>,
        /// The caller, when the kernel could say.
        caller: Option<ProcId>,
        /// Why.
        reason: Errno,
    },
}

/// Where the server's decisions go.
pub trait Recorder {
    /// Record a decision.
    fn record(&mut self, record: Record);
}

enum Answer {
    Facts,
    Done,
    Gain(i32),
}

/// One codec's endpoint.
pub struct CodecServer<C: Codec, S: SupplierHost, L: Recorder> {
    codec: C,
    supplier: S,
    log: L,
    endpoint: u64,
    holder: Option<ProcId>,
}

impl<C: Codec, S: SupplierHost, L: Recorder> CodecServer<C, S, L> {
    /// The endpoint `endpoint` over `codec`, asking about its callers
    /// through `supplier` and recording to `log`.
    pub const fn new(codec: C, supplier: S, log: L, endpoint: u64) -> Self {
        Self {
            codec,
            supplier,
            log,
            endpoint,
            holder: None,
        }
    }

    /// Serve the call `ticket` carrying `frame`, answering it.
    pub fn serve(&mut self, ticket: u64, frame: &[u8]) {
        let caller = match self.supplier.caller(ticket) {
            Ok(caller) => caller,
            Err(reason) => return self.refuse(ticket, None, None, reason),
        };
        let request = match CodecRequest::decode(frame) {
            Ok(request) => request,
            Err(reason) => return self.refuse(ticket, None, Some(caller), reason),
        };
        match self.answer(ticket, caller, &request) {
            Ok(answer) => self.send(ticket, &answer),
            Err(reason) => self.refuse(ticket, Some(request.op()), Some(caller), reason),
        }
    }

    /// Release the codec if `peer`, which has ended, held it.
    pub fn peer_exited(&mut self, peer: ProcId) {
        if self.holder != Some(peer) {
            return;
        }
        self.holder = None;
        // Nothing can drive the codec now; a refusal leaves it as it is.
        let _ = self.codec.stop();
        self.log.record(Record::Abandoned { holder: peer });
        self.supplier.unwatch(peer);
    }

    fn answer(
        &mut self,
        ticket: u64,
        caller: ProcId,
        request: &CodecRequest,
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
        if let CodecRequest::Describe(_) = request {
            return Ok(Answer::Facts);
        }
        self.hold(caller)?;
        match *request {
            CodecRequest::Configure { rate, width, .. } => {
                let dai = DaiLink::from_cells(link.selector())?;
                self.codec.configure(&dai, rate, width).map_err(refusal)?;
                Ok(Answer::Done)
            }
            CodecRequest::Gain { millibel, mute, .. } => self
                .codec
                .set_gain(millibel, mute)
                .map(Answer::Gain)
                .map_err(refusal),
            CodecRequest::Start(_) => self.codec.start().map(|()| Answer::Done).map_err(refusal),
            CodecRequest::Stop(_) => self.codec.stop().map(|()| Answer::Done).map_err(refusal),
            CodecRequest::Describe(_) => Ok(Answer::Facts),
        }
    }

    /// Make `caller` the codec's holder, unless another instance that still
    /// lives holds it: the kernel's refusal to watch the holder says it does
    /// not.
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

    fn send(&mut self, ticket: u64, answer: &Answer) {
        let mut out = [0u8; CODEC_MAX_REPLY];
        let encoded = match *answer {
            Answer::Facts => encode_describe_reply(&mut out, &self.codec.facts()),
            Answer::Done => encode_done_reply(&mut out),
            Answer::Gain(millibel) => encode_gain_reply(&mut out, millibel),
        };
        // A caller that ends before its answer arrives has its exit do the
        // cleaning up; there is nobody left to tell.
        if let Ok(len) = encoded {
            let _ = self.supplier.reply(ticket, &out[..len]);
        }
    }

    fn refuse(&mut self, ticket: u64, op: Option<CodecOp>, caller: Option<ProcId>, reason: Errno) {
        self.log.record(Record::Refused { op, caller, reason });
        let mut out = [0u8; CODEC_MAX_REPLY];
        if let Ok(len) = encode_error_reply(&mut out, reason) {
            let _ = self.supplier.reply(ticket, &out[..len]);
        }
    }
}
