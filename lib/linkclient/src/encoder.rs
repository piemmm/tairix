//! [`EncoderClient`]: the encoder a display engine's output runs through.

use tairix_abi::driver::display::Timing;
use tairix_abi::driver::encoder::{
    decode_done_reply, decode_state_reply, refusal, EncoderRequest, EncoderState, PipeLimits,
    ENCODER_MAX_REPLY, ENCODER_MAX_REQUEST,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::{DriverError, Errno, CALL_DEADLINE_NONE};

use crate::call::ask;
use crate::LinkCall;

/// The encoder an engine's node names, as its driver serves it.
///
/// Every step of a mode set blocks for the encoder's answer; the connector's
/// moves arrive as the answer to a posted wait the engine's serve loop wakes
/// on. Refusals are decoded by `encoder-v1`'s [`refusal`], so a connector
/// that moved ([`DriverError::Stale`]) is told from a failing encoder.
pub struct EncoderClient<C: LinkCall> {
    call: C,
    link: LinkRequest,
    /// The posted wait's ticket, while one is outstanding.
    waiting: Option<u64>,
}

impl<C: LinkCall> EncoderClient<C> {
    /// The client for `link`, the encoder link the caller's node holds,
    /// reached through `call`.
    pub const fn new(call: C, link: LinkRequest) -> Self {
        Self {
            call,
            link,
            waiting: None,
        }
    }

    /// The link the client drives.
    #[must_use]
    pub const fn link(&self) -> &LinkRequest {
        &self.link
    }

    /// The connector, the phase, and how often the connector has moved.
    ///
    /// # Errors
    ///
    /// The encoder's or the transport's refusal.
    pub fn describe(&mut self) -> Result<EncoderState, DriverError> {
        self.ask(&EncoderRequest::Describe(self.link), decode_state_reply)
    }

    /// Take the encoder and bound the connector's mode by what the caller's
    /// pipe carries; a mode that no longer fits moves the connector.
    ///
    /// # Errors
    ///
    /// [`DriverError::Busy`] for an encoder another live engine holds, or
    /// the encoder's or the transport's refusal.
    pub fn limit(&mut self, limits: &PipeLimits) -> Result<(), DriverError> {
        let request = EncoderRequest::Limit {
            link: self.link,
            limits: *limits,
        };
        self.ask(&request, decode_done_reply)
    }

    /// Take the encoder and set it up for `timing`, the connector's mode.
    ///
    /// # Errors
    ///
    /// [`DriverError::Stale`] once the connector runs another mode or none,
    /// [`DriverError::Busy`] for an encoder another live engine holds,
    /// [`DriverError::OutOfRange`] out of the mode set's order, or the
    /// device's refusal.
    pub fn configure(&mut self, timing: &Timing) -> Result<(), DriverError> {
        let request = EncoderRequest::Configure {
            link: self.link,
            timing: *timing,
        };
        self.ask(&request, decode_done_reply)
    }

    /// Ready the output for the timing generator's video.
    ///
    /// # Errors
    ///
    /// As [`Self::configure`], bar the connector's.
    pub fn prepare(&mut self) -> Result<(), DriverError> {
        self.ask(&EncoderRequest::Prepare(self.link), decode_done_reply)
    }

    /// Start the output, once the timing generator's video has started.
    ///
    /// # Errors
    ///
    /// As [`Self::prepare`].
    pub fn enable(&mut self) -> Result<(), DriverError> {
        self.ask(&EncoderRequest::Enable(self.link), decode_done_reply)
    }

    /// Stop the output, before the timing generator's video stops.
    ///
    /// # Errors
    ///
    /// As [`Self::prepare`].
    pub fn disable(&mut self) -> Result<(), DriverError> {
        self.ask(&EncoderRequest::Disable(self.link), decode_done_reply)
    }

    /// Power the output down, from any phase but sending.
    ///
    /// # Errors
    ///
    /// As [`Self::prepare`].
    pub fn power_down(&mut self) -> Result<(), DriverError> {
        self.ask(&EncoderRequest::PowerDown(self.link), decode_done_reply)
    }

    /// Post a wait answered once the connector has moved more than `seen`
    /// times, with no deadline: a sink may not arrive for days, and the wait
    /// ends without one only with the encoder.
    ///
    /// # Errors
    ///
    /// [`DriverError::Busy`] while a wait is outstanding, or the transport's
    /// refusal.
    pub fn post_wait(&mut self, seen: u64) -> Result<(), DriverError> {
        if self.waiting.is_some() {
            return Err(DriverError::Busy);
        }
        let mut frame = [0u8; ENCODER_MAX_REQUEST];
        let len = EncoderRequest::Wait {
            link: self.link,
            seen,
        }
        .encode(&mut frame)
        .map_err(refusal)?;
        let frame = frame.get(..len).ok_or(DriverError::OutOfRange)?;
        self.waiting = Some(self.call.post(frame, CALL_DEADLINE_NONE).map_err(refusal)?);
        Ok(())
    }

    /// Collect the posted wait: the encoder's state once it is answered,
    /// [`None`] while it is pending or when none is posted.
    ///
    /// # Errors
    ///
    /// The encoder's refusal of the wait, or the transport's —
    /// [`DriverError::NotFound`] once the encoder is gone. The wait is spent
    /// either way.
    pub fn reap_wait(&mut self) -> Result<Option<EncoderState>, DriverError> {
        let Some(ticket) = self.waiting else {
            return Ok(None);
        };
        let mut reply = [0u8; ENCODER_MAX_REPLY];
        let outcome = self.call.reap(ticket, &mut reply);
        if matches!(outcome, Ok(None)) {
            return Ok(None);
        }
        self.waiting = None;
        let got = outcome.map_err(refusal)?.ok_or(DriverError::OutOfRange)?;
        let reply = reply.get(..got).ok_or(DriverError::OutOfRange)?;
        decode_state_reply(reply).map(Some).map_err(refusal)
    }

    /// Whether a wait is outstanding.
    #[must_use]
    pub const fn is_waiting(&self) -> bool {
        self.waiting.is_some()
    }

    fn ask<T>(
        &mut self,
        request: &EncoderRequest,
        decode: impl FnOnce(&[u8]) -> Result<T, Errno>,
    ) -> Result<T, DriverError> {
        ask::<_, _, ENCODER_MAX_REQUEST, ENCODER_MAX_REPLY>(
            &mut self.call,
            |frame| request.encode(frame),
            decode,
        )
        .map_err(refusal)
    }
}
