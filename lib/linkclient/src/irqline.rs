//! [`IrqLineClient`]: one interrupt line through a second-level controller.

use tairix_abi::driver::irqline::{
    decode_reply, IrqLineRequest, IRQLINE_MAX_REPLY, IRQLINE_MAX_REQUEST,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::Errno;

use crate::LinkCall;

/// The line a link names, waited on through its controller.
///
/// A wait unmasks the line and is answered when it fires; the line stays
/// masked from then until the next wait, so the consumer services its device
/// before the line can fire again.
pub struct IrqLineClient<C: LinkCall> {
    call: C,
    line: LinkRequest,
    /// The posted wait's ticket, while one is outstanding.
    waiting: Option<u64>,
}

impl<C: LinkCall> IrqLineClient<C> {
    /// The client for `line`, the interrupt line the caller's node holds,
    /// reached through `call`.
    pub const fn new(call: C, line: LinkRequest) -> Self {
        Self {
            call,
            line,
            waiting: None,
        }
    }

    /// The link the client waits on.
    #[must_use]
    pub const fn line(&self) -> &LinkRequest {
        &self.line
    }

    /// Wait, blocked, until the line fires.
    ///
    /// # Errors
    ///
    /// [`Errno::Busy`] while a wait is posted, or the controller's or the
    /// transport's refusal — [`Errno::NotFound`] once the controller is gone.
    pub fn wait(&mut self) -> Result<(), Errno> {
        if self.waiting.is_some() {
            return Err(Errno::Busy);
        }
        crate::call::ask::<_, _, IRQLINE_MAX_REQUEST, IRQLINE_MAX_REPLY>(
            &mut self.call,
            |frame| IrqLineRequest::Wait(self.line).encode(frame),
            decode_reply,
        )
    }

    /// Post a wait for the line, to be answered within `deadline_ns`.
    ///
    /// # Errors
    ///
    /// [`Errno::Busy`] while a wait is outstanding, or the transport's
    /// refusal.
    pub fn post_wait(&mut self, deadline_ns: u64) -> Result<(), Errno> {
        if self.waiting.is_some() {
            return Err(Errno::Busy);
        }
        let mut frame = [0u8; IRQLINE_MAX_REQUEST];
        let len = IrqLineRequest::Wait(self.line).encode(&mut frame)?;
        let frame = frame.get(..len).ok_or(Errno::LengthOutOfRange)?;
        self.waiting = Some(self.call.post(frame, deadline_ns)?);
        Ok(())
    }

    /// Collect the posted wait: `Ok(true)` once the line fired, `Ok(false)`
    /// while it is pending or when none is posted.
    ///
    /// # Errors
    ///
    /// The controller's refusal of the wait, or the transport's —
    /// [`Errno::TimedOut`] for a deadline that passed. The wait is spent
    /// either way.
    pub fn reap_wait(&mut self) -> Result<bool, Errno> {
        let Some(ticket) = self.waiting else {
            return Ok(false);
        };
        let mut reply = [0u8; IRQLINE_MAX_REPLY];
        let outcome = self.call.reap(ticket, &mut reply);
        if matches!(outcome, Ok(None)) {
            return Ok(false);
        }
        self.waiting = None;
        let got = outcome?.ok_or(Errno::LengthOutOfRange)?;
        decode_reply(reply.get(..got).ok_or(Errno::LengthOutOfRange)?).map(|()| true)
    }

    /// Whether a wait is outstanding.
    #[must_use]
    pub const fn is_waiting(&self) -> bool {
        self.waiting.is_some()
    }
}
