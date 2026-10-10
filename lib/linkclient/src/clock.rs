//! [`ClockClient`]: one clock, reached through the link that names it.

use tairix_abi::driver::clock::{
    decode_describe_reply, decode_release_reply, decode_run_reply, ClockRequest, ClockState,
    CLOCK_MAX_REPLY, CLOCK_MAX_REQUEST,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::Errno;

use crate::call::ask;
use crate::LinkCall;

/// The clock a consumer's node names, as its controller serves it.
pub struct ClockClient<C: LinkCall> {
    call: C,
    link: LinkRequest,
}

impl<C: LinkCall> ClockClient<C> {
    /// The clock `link` names, the clock link the caller's node holds.
    #[must_use]
    pub const fn new(call: C, link: LinkRequest) -> Self {
        Self { call, link }
    }

    /// The clock's rate, zero while stopped, and whether another process
    /// holds it.
    ///
    /// # Errors
    ///
    /// The controller's or the transport's refusal.
    pub fn describe(&mut self) -> Result<ClockState, Errno> {
        self.ask(&ClockRequest::Describe(self.link), decode_describe_reply)
    }

    /// Run the clock as near `hz` as the controller can make it, answering
    /// the rate it runs at.
    ///
    /// # Errors
    ///
    /// [`Errno::Busy`] for a clock another process holds at another rate, or
    /// another refusal.
    pub fn run(&mut self, hz: u64) -> Result<u64, Errno> {
        let request = ClockRequest::Run {
            link: self.link,
            hz,
        };
        self.ask(&request, decode_run_reply)
    }

    /// Give the clock up; the last holder's release stops it.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for a clock the caller does not hold, or another
    /// refusal.
    pub fn release(&mut self) -> Result<(), Errno> {
        self.ask(&ClockRequest::Release(self.link), decode_release_reply)
    }

    fn ask<T>(
        &mut self,
        request: &ClockRequest,
        decode: impl FnOnce(&[u8]) -> Result<T, Errno>,
    ) -> Result<T, Errno> {
        ask::<_, _, CLOCK_MAX_REQUEST, CLOCK_MAX_REPLY>(
            &mut self.call,
            |frame| request.encode(frame),
            decode,
        )
    }
}
