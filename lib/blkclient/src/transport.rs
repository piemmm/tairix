//! [`RtBlkCall`]: the production blkio transport.
//!
//! The bounded, capability-checked async submit/reap seam on a granted
//! block-service endpoint (`plans/FIX-IO.md` IO1), shared by every
//! [`crate::RemoteBlock`] consumer so the volume-manager probe and the RAID
//! array composer issue the identical wire discipline. Each request is made
//! with [`tairix_rt::call_within`], the one deadline call loop, under the
//! caller's per-request deadline, so a wedged device fails this transfer
//! closed at its deadline instead of parking the caller forever. The deadline
//! is the caller's, derived from the device's own declared class, so this
//! transport carries no deadline policy of its own. The serving driver fills
//! the shared window during the call, so the window parameter is untouched
//! here.
//!
//! # Each transport waits on its own set
//!
//! A transport parks on a wait-set of its own, holding only its endpoint's
//! `CallReply` member and closed when the transport drops. The kernel keeps a
//! member whose endpoint has ended ready, as the sign that it ended, so a set
//! shared with other endpoints would leave every other transfer's wait
//! spinning on a removed device's member; and a set closed with its transport
//! costs nothing past it, however often a long-lived consumer reconnects.

use tairix_abi::waitset::WaitSourceKind;
use tairix_abi::Errno;
use tairix_rt::WaitSet;

use crate::client::BlkCall;

/// The production blkio transport: one call endpoint, and the wait-set its
/// reply completions are observed on.
pub struct RtBlkCall {
    endpoint: u64,
    /// The endpoint's reply set, made with its one member on first use, so
    /// building a transport cannot fail and the first transfer surfaces a
    /// refusal.
    replies: Option<WaitSet>,
}

impl RtBlkCall {
    /// A fresh transport for `endpoint`.
    #[must_use]
    pub const fn new(endpoint: u64) -> Self {
        Self {
            endpoint,
            replies: None,
        }
    }

    /// The handle of the set this endpoint's replies are observed on.
    fn replies(&mut self) -> Result<u64, Errno> {
        if let Some(set) = &self.replies {
            return Ok(set.handle());
        }
        let set = WaitSet::create()?;
        set.add(WaitSourceKind::CallReply, self.endpoint, 0)?;
        Ok(self.replies.insert(set).handle())
    }
}

impl BlkCall for RtBlkCall {
    fn call(
        &mut self,
        request: &[u8],
        reply: &mut [u8],
        _window: &mut [u8],
        deadline_ns: u64,
    ) -> Result<usize, Errno> {
        let set = self.replies()?;
        tairix_rt::call_within(set, self.endpoint, request, reply, deadline_ns)
    }

    fn pause(&mut self, ns: u64) {
        tairix_rt::park_ns(ns);
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};
    use tairix_abi::SyscallNumber;
    use tairix_abi_trap::seam;

    use super::RtBlkCall;
    use crate::client::BlkCall;

    const fn number(syscall: SyscallNumber) -> u64 {
        syscall.as_u16() as u64
    }

    /// A transport observes its replies on a set of its own, holding only its
    /// endpoint's member and made once, and closes it when it drops: a member
    /// whose endpoint ended stays ready, so a set shared with other endpoints
    /// would wake every other transfer's wait.
    #[test]
    fn a_transport_waits_on_a_set_of_its_own_and_closes_it() {
        seam::arm(0);
        let mut transport = RtBlkCall::new(9);
        let mut reply = [0u8; 8];
        for _ in 0..2 {
            assert_eq!(transport.call(b"read", &mut reply, &mut [], 250), Ok(0));
        }
        drop(transport);
        let calls = seam::calls();
        let numbers: Vec<u64> = calls.iter().map(|(number, _)| *number).collect();
        assert_eq!(
            numbers,
            [
                number(SyscallNumber::WAITSET_CREATE),
                number(SyscallNumber::WAITSET_CTL),
                number(SyscallNumber::CALL_POST),
                number(SyscallNumber::CALL_REAP),
                number(SyscallNumber::CALL_POST),
                number(SyscallNumber::CALL_REAP),
                number(SyscallNumber::WAITSET_CLOSE),
            ]
        );
        let (_, member) = calls[1];
        assert_eq!(
            member[..4],
            [
                0,
                u64::from(WaitSetOp::Add.as_u32()),
                u64::from(WaitSourceKind::CallReply.as_u32()),
                9
            ]
        );
    }
}
