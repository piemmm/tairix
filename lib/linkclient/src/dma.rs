//! [`DmaClient`]: one channel on one DMA request line.

use tairix_abi::driver::dmaengine::{
    decode_done_reply, decode_open_reply, decode_position_reply, decode_prepare_reply,
    decode_wait_reply, CyclicParams, DmaBufferGrant, DmaEngineRequest, WaitReport,
    DMA_ENGINE_MAX_REPLY, DMA_ENGINE_MAX_REQUEST,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::Errno;

use crate::LinkCall;

fn ask<C: LinkCall, T>(
    call: &mut C,
    request: &DmaEngineRequest,
    decode: impl FnOnce(&[u8]) -> Result<T, Errno>,
) -> Result<T, Errno> {
    crate::call::ask::<_, _, DMA_ENGINE_MAX_REQUEST, DMA_ENGINE_MAX_REPLY>(
        call,
        |frame| request.encode(frame),
        decode,
    )
}

/// A DMA channel opened for one request line.
///
/// The channel is the opening instance's until [`close`](Self::close) or its
/// end, when the controller stops it and frees its buffer.
pub struct DmaClient<C: LinkCall> {
    call: C,
    channel: u8,
    /// The posted wait's ticket, while one is outstanding.
    waiting: Option<u64>,
}

impl<C: LinkCall> DmaClient<C> {
    /// Open a channel on `line`, the DMA request line the caller's node
    /// holds.
    ///
    /// # Errors
    ///
    /// The controller's or the transport's refusal.
    pub fn open(mut call: C, line: LinkRequest) -> Result<Self, Errno> {
        let channel = ask(&mut call, &DmaEngineRequest::Open(line), decode_open_reply)?;
        Ok(Self {
            call,
            channel,
            waiting: None,
        })
    }

    /// The channel the controller opened.
    #[must_use]
    pub const fn channel(&self) -> u8 {
        self.channel
    }

    /// Have the controller carve the channel's buffer and build its chain for
    /// `params`, answering the grant the caller maps it through.
    ///
    /// # Errors
    ///
    /// The controller's or the transport's refusal.
    pub fn prepare(&mut self, params: &CyclicParams) -> Result<DmaBufferGrant, Errno> {
        let request = DmaEngineRequest::Prepare {
            channel: self.channel,
            params: *params,
        };
        ask(&mut self.call, &request, decode_prepare_reply)
    }

    /// Start the chain from its first period.
    ///
    /// # Errors
    ///
    /// The controller's or the transport's refusal.
    pub fn start(&mut self) -> Result<(), Errno> {
        self.done(DmaEngineRequest::Start {
            channel: self.channel,
        })
    }

    /// Stop the chain; the posted wait is answered as stopped.
    ///
    /// # Errors
    ///
    /// [`Errno::DeviceFault`] for a channel that would not reset, whose buffer
    /// may still be written; or another refusal.
    pub fn stop(&mut self) -> Result<(), Errno> {
        self.done(DmaEngineRequest::Stop {
            channel: self.channel,
        })
    }

    /// The channel's live memory-side offset within its buffer.
    ///
    /// # Errors
    ///
    /// The controller's or the transport's refusal.
    pub fn position(&mut self) -> Result<u64, Errno> {
        let request = DmaEngineRequest::Position {
            channel: self.channel,
        };
        ask(&mut self.call, &request, decode_position_reply)
    }

    /// Block until the first period boundary past byte position `after`.
    ///
    /// For a caller with nothing else to wait on; a serve loop posts the wait
    /// instead.
    ///
    /// # Errors
    ///
    /// [`Errno::Busy`] while a posted wait is outstanding, or the controller's
    /// or the transport's refusal.
    pub fn wait(&mut self, after: u64) -> Result<WaitReport, Errno> {
        if self.waiting.is_some() {
            return Err(Errno::Busy);
        }
        let request = DmaEngineRequest::Wait {
            channel: self.channel,
            after,
        };
        ask(&mut self.call, &request, decode_wait_reply)
    }

    /// Post a wait for the first period boundary past byte position `after`,
    /// to be answered within `deadline_ns`.
    ///
    /// # Errors
    ///
    /// [`Errno::Busy`] while a wait is outstanding, or the transport's
    /// refusal.
    pub fn post_wait(&mut self, after: u64, deadline_ns: u64) -> Result<(), Errno> {
        if self.waiting.is_some() {
            return Err(Errno::Busy);
        }
        let request = DmaEngineRequest::Wait {
            channel: self.channel,
            after,
        };
        let mut frame = [0u8; DMA_ENGINE_MAX_REQUEST];
        let len = request.encode(&mut frame)?;
        let frame = frame.get(..len).ok_or(Errno::LengthOutOfRange)?;
        self.waiting = Some(self.call.post(frame, deadline_ns)?);
        Ok(())
    }

    /// Collect the posted wait's answer: `None` while it is pending or when
    /// none is posted.
    ///
    /// # Errors
    ///
    /// The controller's refusal of the wait, or the transport's —
    /// [`Errno::TimedOut`] for a controller that let the deadline pass. The
    /// wait is spent either way.
    pub fn reap_wait(&mut self) -> Result<Option<WaitReport>, Errno> {
        let Some(ticket) = self.waiting else {
            return Ok(None);
        };
        let mut reply = [0u8; DMA_ENGINE_MAX_REPLY];
        let got = match self.call.reap(ticket, &mut reply) {
            Ok(None) => return Ok(None),
            Ok(Some(got)) => got,
            Err(reason) => {
                self.waiting = None;
                return Err(reason);
            }
        };
        self.waiting = None;
        decode_wait_reply(reply.get(..got).ok_or(Errno::LengthOutOfRange)?).map(Some)
    }

    /// Whether a wait is outstanding.
    #[must_use]
    pub const fn is_waiting(&self) -> bool {
        self.waiting.is_some()
    }

    /// Stop the channel, release its buffer and free it.
    ///
    /// # Errors
    ///
    /// The controller's or the transport's refusal.
    pub fn close(mut self) -> Result<(), Errno> {
        let closed = self.done(DmaEngineRequest::Close {
            channel: self.channel,
        });
        // The controller answers the posted wait before it answers the close,
        // so collecting it now leaves no reply behind.
        let _ = self.reap_wait();
        closed
    }

    /// Send `request`, which a bare acknowledgement answers.
    fn done(&mut self, request: DmaEngineRequest) -> Result<(), Errno> {
        let op = request.op();
        ask(&mut self.call, &request, |reply| {
            decode_done_reply(reply, op)
        })
    }
}
