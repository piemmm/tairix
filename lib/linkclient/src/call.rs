//! [`LinkCall`]: how a consumer reaches its supplier's endpoint, and
//! [`RtLinkCall`], the production one over the runtime's call traps.

use tairix_abi::Errno;

/// How a client reaches its supplier's endpoint.
pub trait LinkCall {
    /// Send `request` and block for the reply, answering its length.
    ///
    /// # Errors
    ///
    /// The transport's refusal.
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno>;

    /// Send `request` without blocking, to be answered within `deadline_ns`,
    /// answering the ticket its reply is collected by.
    ///
    /// # Errors
    ///
    /// The transport's refusal.
    fn post(&mut self, request: &[u8], deadline_ns: u64) -> Result<u64, Errno>;

    /// Collect the reply to `ticket`: its length, or `None` while it is
    /// pending.
    ///
    /// # Errors
    ///
    /// The transport's refusal, [`Errno::TimedOut`] among them; the ticket is
    /// spent either way.
    fn reap(&mut self, ticket: u64, reply: &mut [u8]) -> Result<Option<usize>, Errno>;
}

/// Send the request `encode` frames, in at most `REQUEST` bytes, and decode
/// the reply of at most `REPLY` bytes with `decode`.
pub(crate) fn ask<C: LinkCall, T, const REQUEST: usize, const REPLY: usize>(
    call: &mut C,
    encode: impl FnOnce(&mut [u8]) -> Result<usize, Errno>,
    decode: impl FnOnce(&[u8]) -> Result<T, Errno>,
) -> Result<T, Errno> {
    let mut frame = [0u8; REQUEST];
    let len = encode(&mut frame)?;
    let mut reply = [0u8; REPLY];
    let got = call.call(frame.get(..len).ok_or(Errno::LengthOutOfRange)?, &mut reply)?;
    decode(reply.get(..got).ok_or(Errno::LengthOutOfRange)?)
}

/// The supplier endpoint `endpoint`, reached through the kernel.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RtLinkCall {
    endpoint: u64,
}

impl RtLinkCall {
    /// The supplier endpoint a link names.
    #[must_use]
    pub const fn new(endpoint: u64) -> Self {
        Self { endpoint }
    }
}

impl LinkCall for RtLinkCall {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        tairix_rt::ipc_call(self.endpoint, request, reply).map_err(Errno::from_syscall)
    }

    fn post(&mut self, request: &[u8], deadline_ns: u64) -> Result<u64, Errno> {
        tairix_rt::call_post(self.endpoint, request, deadline_ns).map_err(Errno::from_syscall)
    }

    fn reap(&mut self, ticket: u64, reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        match tairix_rt::call_reap(self.endpoint, ticket, reply) {
            Ok(got) => Ok(Some(got)),
            Err(ret) => match Errno::from_syscall(ret) {
                Errno::WouldBlock => Ok(None),
                reason => Err(reason),
            },
        }
    }
}
