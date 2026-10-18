//! Asking the session authority for the civil-time store, which the screen
//! reads through a delegation because it may open no file itself.

use core::num::NonZeroU64;

use tairix_abi::session_ipc::{
    decode_zone_store_reply, SessionRequest, SESSION_MAX_REQUEST, SESSION_ZONE_STORE_LEN,
};

use crate::accounts::SessionTransport;

/// Why no delegation of the store came back; the screen then reads UTC.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ZoneStoreRefusal {
    /// The authority could not be reached.
    Unreachable,
    /// The authority answered, but not with a zone-store reply.
    Malformed,
    /// The authority delegated nothing: it refused, or holds no store.
    Withheld,
}

impl ZoneStoreRefusal {
    /// A phrase for the record.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Unreachable => "the session authority could not be reached",
            Self::Malformed => "the session authority's answer was malformed",
            Self::Withheld => "the session authority delegated no zone store",
        }
    }
}

/// The handle of a read-only delegation of the store, minted to this
/// process by the authority.
///
/// # Errors
///
/// [`ZoneStoreRefusal`] for every way no delegation came back.
pub fn zone_store_grant<T: SessionTransport>(
    transport: &mut T,
) -> Result<NonZeroU64, ZoneStoreRefusal> {
    let mut request = [0u8; SESSION_MAX_REQUEST];
    let len = SessionRequest::ZoneStore
        .encode(&mut request)
        .map_err(|_| ZoneStoreRefusal::Malformed)?;
    let mut reply = [0u8; SESSION_ZONE_STORE_LEN];
    let read = transport
        .call(&request[..len], &mut reply)
        .map_err(|_| ZoneStoreRefusal::Unreachable)?;
    let frame = reply.get(..read).ok_or(ZoneStoreRefusal::Malformed)?;
    decode_zone_store_reply(frame)
        .map_err(|_| ZoneStoreRefusal::Malformed)?
        .ok_or(ZoneStoreRefusal::Withheld)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_abi::session_ipc::encode_zone_store_reply;
    use tairix_abi::Errno;

    /// Answers every zone-store request with `handle`, refusing anything else.
    struct Delegating(Option<NonZeroU64>);

    impl SessionTransport for Delegating {
        fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
            if SessionRequest::decode(request)? != SessionRequest::ZoneStore {
                return Err(Errno::OutOfRange);
            }
            encode_zone_store_reply(self.0, reply)
        }
    }

    struct Answering(&'static [u8]);

    impl SessionTransport for Answering {
        fn call(&mut self, _request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
            reply[..self.0.len()].copy_from_slice(self.0);
            Ok(self.0.len())
        }
    }

    struct Down;

    impl SessionTransport for Down {
        fn call(&mut self, _request: &[u8], _reply: &mut [u8]) -> Result<usize, Errno> {
            Err(Errno::NotFound)
        }
    }

    #[test]
    fn a_delegation_comes_back_as_its_handle() {
        let handle = NonZeroU64::new(12);
        assert_eq!(
            zone_store_grant(&mut Delegating(handle)),
            handle.ok_or(ZoneStoreRefusal::Withheld)
        );
    }

    #[test]
    fn every_way_no_delegation_comes_back_is_named() {
        assert_eq!(
            zone_store_grant(&mut Delegating(None)),
            Err(ZoneStoreRefusal::Withheld)
        );
        assert_eq!(
            zone_store_grant(&mut Down),
            Err(ZoneStoreRefusal::Unreachable)
        );
        assert_eq!(
            zone_store_grant(&mut Answering(b"not a reply")),
            Err(ZoneStoreRefusal::Malformed)
        );
    }
}
