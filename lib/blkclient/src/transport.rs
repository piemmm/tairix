//! [`RtBlkCall`]: the production blkio transport.
//!
//! The bounded, capability-checked async submit/reap seam on a granted
//! block-service endpoint (`plans/FIX-IO.md` IO1), shared by every
//! [`crate::RemoteBlock`] consumer so the volume-manager probe and the RAID
//! array composer issue the identical wire discipline. Each request is made
//! with [`tairix_rt::call_within`], the one deadline call, under the caller's
//! per-request deadline, so a wedged device fails this transfer closed at its
//! deadline instead of parking the caller forever. The deadline is the
//! caller's, derived from the device's own declared class, so this transport
//! carries no deadline policy of its own. The serving driver fills the shared
//! window during the call, so the window parameter is untouched here.

use tairix_abi::Errno;

use crate::client::BlkCall;

/// The production blkio transport: one call endpoint.
pub struct RtBlkCall {
    endpoint: u64,
}

impl RtBlkCall {
    /// A fresh transport for `endpoint`.
    #[must_use]
    pub const fn new(endpoint: u64) -> Self {
        Self { endpoint }
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
        tairix_rt::call_within(self.endpoint, request, reply, deadline_ns)
    }

    fn pause(&mut self, ns: u64) {
        tairix_rt::park_ns(ns);
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use tairix_abi::SyscallNumber;
    use tairix_abi_trap::seam;

    use super::RtBlkCall;
    use crate::client::BlkCall;

    const fn number(syscall: SyscallNumber) -> u64 {
        syscall.as_u16() as u64
    }

    /// Each transfer is posted under its deadline and that call alone is
    /// awaited, so a consumer with several transfers outstanding to one
    /// endpoint never wakes for another's answer.
    #[test]
    fn a_transfer_is_posted_under_its_deadline_and_awaited_alone() {
        seam::arm(0);
        let mut transport = RtBlkCall::new(9);
        let mut reply = [0u8; 8];
        for _ in 0..2 {
            assert_eq!(transport.call(b"read", &mut reply, &mut [], 250), Ok(0));
        }
        let calls = seam::calls();
        let numbers: Vec<u64> = calls.iter().map(|(number, _)| *number).collect();
        assert_eq!(
            numbers,
            [
                number(SyscallNumber::CALL_POST),
                number(SyscallNumber::CALL_AWAIT),
                number(SyscallNumber::CALL_POST),
                number(SyscallNumber::CALL_AWAIT),
            ]
        );
        let (_, posted) = calls[0];
        assert_eq!((posted[0], posted[4]), (9, 250));
        let (_, awaited) = calls[1];
        assert_eq!(awaited[0], 9);
    }
}
