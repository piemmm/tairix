//! The production transport: the network-share service's endpoint, called
//! through the kernel.
//!
//! This module is compiled only for a freestanding userland program that
//! opts into the `program` feature.

use tairix_abi::netshare_ipc::NETSHARE_ENDPOINT;
use tairix_abi::Errno;

use crate::Transport;

/// The network-share service's endpoint, called directly.
pub struct RtTransport;

impl Transport for RtTransport {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        tairix_rt::ipc_call(NETSHARE_ENDPOINT, request, reply).map_err(Errno::from_syscall)
    }
}
