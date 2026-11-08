//! Framing a `sysinfo-v1` request and issuing it through the [`Transport`]
//! seam.
//!
//! Both the `sysinfo` and `ps` tools build the same
//! [`SysinfoRequestHeader`](tairix_abi::sysinfo::SysinfoRequestHeader)
//! envelope and map a capability denial onto the same distinguished error,
//! so that logic lives here rather than being copied.

use alloc::vec::Vec;

use tairix_abi::sysinfo::{encode_request, SysinfoQueryId, SYSINFO_MAX_REQUEST};
use tairix_abi::Errno;

use crate::transport::Transport;

/// Why a single System Information API call did not return a reply.
///
/// [`PermissionDenied`](CallError::PermissionDenied) is distinguished from
/// [`Service`](CallError::Service) so a consuming tool can print the precise
/// "this query requires a capability you do not hold" diagnostic; everything
/// else leans on the frozen [`Errno`] for the wire-level cause and invents no
/// parallel error set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallError {
    /// The service refused the query because the caller lacks the capability
    /// the query declares.
    PermissionDenied,
    /// The transport failed for any other reason. Carries the underlying
    /// [`Errno`].
    Service(Errno),
}

/// Issue `query` with `payload` through `transport`, mapping the transport
/// [`Errno`] onto the [`CallError`] vocabulary so a capability denial renders
/// precisely.
///
/// The request is framed in place at the endpoint's own bound, so asking
/// costs no allocation and a payload the endpoint could never take is refused
/// here.
///
/// # Errors
///
/// * [`CallError::PermissionDenied`] — the service refused the query for want
///   of its declared capability.
/// * [`CallError::Service`] — any other transport or IPC failure, or the
///   framer's refusal of a payload past the endpoint's bound.
pub fn call(
    transport: &dyn Transport,
    query: SysinfoQueryId,
    payload: &[u8],
) -> Result<Vec<u8>, CallError> {
    let mut request = [0u8; SYSINFO_MAX_REQUEST];
    let len = encode_request(query, payload, &mut request).map_err(CallError::Service)?;
    transport
        .query(&request[..len])
        .map_err(|errno| match errno {
            Errno::PermissionDenied => CallError::PermissionDenied,
            other => CallError::Service(other),
        })
}

#[cfg(test)]
mod tests {
    use super::{call, CallError};
    use crate::transport::Transport;
    use alloc::vec::Vec;
    use tairix_abi::sysinfo::{SysinfoQueryId, SysinfoRequestHeader, SYSINFO_MAX_REQUEST};
    use tairix_abi::Errno;

    struct Echo {
        reply: Result<Vec<u8>, Errno>,
        seen: core::cell::RefCell<Vec<u8>>,
    }

    impl Transport for Echo {
        fn query(&self, request: &[u8]) -> Result<Vec<u8>, Errno> {
            self.seen.borrow_mut().extend_from_slice(request);
            self.reply.clone()
        }
    }

    fn echo() -> Echo {
        Echo {
            reply: Ok(Vec::new()),
            seen: core::cell::RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn call_frames_the_header_and_payload_exactly() {
        let payload = [1u8, 2, 3, 4];
        let echo = echo();
        call(&echo, SysinfoQueryId::SELF_PROCESS_LIST, &payload).expect("ok");
        let bytes = echo.seen.borrow();
        assert_eq!(bytes.len(), SysinfoRequestHeader::WIRE_LEN + payload.len());
        let header = SysinfoRequestHeader::from_bytes(&bytes).expect("decode");
        assert_eq!(header.query, SysinfoQueryId::SELF_PROCESS_LIST);
        assert_eq!(header.payload_len as usize, payload.len());
        assert_eq!(&bytes[SysinfoRequestHeader::WIRE_LEN..], &payload);
    }

    #[test]
    fn call_frames_an_empty_payload() {
        let echo = echo();
        call(&echo, SysinfoQueryId::UPTIME, &[]).expect("ok");
        let bytes = echo.seen.borrow();
        let header = SysinfoRequestHeader::from_bytes(&bytes).expect("decode");
        assert_eq!(header.payload_len, 0);
        assert_eq!(bytes.len(), SysinfoRequestHeader::WIRE_LEN);
    }

    #[test]
    fn a_payload_past_the_endpoint_bound_is_refused_before_it_is_sent() {
        let echo = echo();
        let payload = [0u8; SYSINFO_MAX_REQUEST];
        assert_eq!(
            call(&echo, SysinfoQueryId::UPTIME, &payload),
            Err(CallError::Service(Errno::BufferTooSmall))
        );
        assert!(echo.seen.borrow().is_empty());
    }

    #[test]
    fn call_forwards_the_encoded_request_and_reply() {
        let echo = Echo {
            reply: Ok(alloc::vec![9, 9, 9]),
            seen: core::cell::RefCell::new(Vec::new()),
        };
        let reply = call(&echo, SysinfoQueryId::UPTIME, &[]).expect("ok");
        assert_eq!(reply, alloc::vec![9, 9, 9]);
        let header = SysinfoRequestHeader::from_bytes(&echo.seen.borrow()).expect("decode");
        assert_eq!(header.query, SysinfoQueryId::UPTIME);
    }

    #[test]
    fn call_maps_permission_denied() {
        let echo = Echo {
            reply: Err(Errno::PermissionDenied),
            seen: core::cell::RefCell::new(Vec::new()),
        };
        assert_eq!(
            call(&echo, SysinfoQueryId::GLOBAL_PROCESS_LIST, &[]),
            Err(CallError::PermissionDenied)
        );
    }

    #[test]
    fn call_maps_other_errno_to_service() {
        let echo = Echo {
            reply: Err(Errno::NotFound),
            seen: core::cell::RefCell::new(Vec::new()),
        };
        assert_eq!(
            call(&echo, SysinfoQueryId::UPTIME, &[]),
            Err(CallError::Service(Errno::NotFound))
        );
    }
}
