//! SPNEGO (RFC 4178, with MS-SPNG's `NegTokenInit2` hints): the negotiation
//! SMB 3 carries in its security buffers, over `lib/der`.
//!
//! This client offers exactly one mechanism per session setup, so there is no
//! choice for an attacker to steer: a mechanism that fails is never traded for
//! another. The mechListMIC is still sent, and checked whenever the server
//! sends one.

use alloc::vec::Vec;

use tairix_der::{DerError, Reader, Tag, Writer};

use crate::SmbError;

/// SPNEGO's OID, 1.3.6.1.5.5.2, as DER contents.
pub const OID: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x02];

/// The most mechanisms a server's hints may list.
const MAX_MECHANISMS: usize = 16;

/// How the acceptor's side of the negotiation stands: `negState`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NegState {
    /// The context is established.
    AcceptCompleted,
    /// More tokens follow.
    AcceptIncomplete,
    /// The acceptor refused.
    Reject,
    /// The acceptor asks for the mechListMIC.
    RequestMic,
}

impl NegState {
    pub(crate) const fn code(self) -> i64 {
        match self {
            Self::AcceptCompleted => 0,
            Self::AcceptIncomplete => 1,
            Self::Reject => 2,
            Self::RequestMic => 3,
        }
    }

    const fn from_code(code: i64) -> Option<Self> {
        match code {
            0 => Some(Self::AcceptCompleted),
            1 => Some(Self::AcceptIncomplete),
            2 => Some(Self::Reject),
            3 => Some(Self::RequestMic),
            _ => None,
        }
    }
}

/// The `MechTypeList` offering `mechanism` alone: what the initial token
/// carries, and what the mechListMIC is made over.
///
/// # Errors
///
/// [`SmbError::Malformed`] for an OID that is not one.
pub fn mech_list(mechanism: &[u8]) -> Result<Vec<u8>, SmbError> {
    let mut w = Writer::new();
    w.sequence(|w| w.oid(mechanism));
    Ok(w.finish()?)
}

/// The initial token: GSS-API's framing around a `negTokenInit` offering
/// `mechanism` with its first `token`.
///
/// # Errors
///
/// As [`mech_list`].
pub fn initial(mechanism: &[u8], token: &[u8]) -> Result<Vec<u8>, SmbError> {
    let mut w = Writer::new();
    w.constructed(Tag::application(0), |w| {
        w.oid(OID);
        w.explicit(0, |w| {
            w.sequence(|w| {
                w.explicit(0, |w| w.sequence(|w| w.oid(mechanism)));
                w.explicit(2, |w| w.octet_string(token));
            });
        });
    });
    Ok(w.finish()?)
}

/// A later initiator token: a `negTokenResp` saying accept-incomplete, with
/// the mechanism's `token` and the `mic` over the mechanism list.
///
/// # Errors
///
/// [`SmbError::TooLarge`] past what DER lengths carry.
pub fn response(token: &[u8], mic: &[u8]) -> Result<Vec<u8>, SmbError> {
    let mut w = Writer::new();
    w.explicit(1, |w| {
        w.sequence(|w| {
            w.explicit(0, |w| w.enumerated(NegState::AcceptIncomplete.code()));
            w.explicit(2, |w| w.octet_string(token));
            w.explicit(3, |w| w.octet_string(mic));
        });
    });
    Ok(w.finish()?)
}

/// An acceptor's `negTokenResp`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reply<'a> {
    /// Where the negotiation stands, when the acceptor said.
    pub state: Option<NegState>,
    /// The mechanism the acceptor chose, in its first reply.
    pub mechanism: Option<&'a [u8]>,
    /// The mechanism's token.
    pub token: Option<&'a [u8]>,
    /// The acceptor's mechListMIC.
    pub mic: Option<&'a [u8]>,
}

impl<'a> Reply<'a> {
    /// The `negTokenResp` that is the whole of `token`.
    ///
    /// # Errors
    ///
    /// [`SmbError::Malformed`] for anything else.
    pub fn decode(token: &'a [u8]) -> Result<Self, SmbError> {
        let mut r = Reader::new(token);
        let (state, mechanism, token, mic) = r.explicit(1, |r| {
            r.sequence(|r| {
                Ok((
                    r.explicit_optional(0, Reader::enumerated)?,
                    r.explicit_optional(1, Reader::oid)?,
                    r.explicit_optional(2, Reader::octet_string)?,
                    r.explicit_optional(3, Reader::octet_string)?,
                ))
            })
        })?;
        r.finish()?;
        let state = match state {
            Some(code) => Some(NegState::from_code(code).ok_or(SmbError::Malformed)?),
            None => None,
        };
        Ok(Self {
            state,
            mechanism,
            token,
            mic,
        })
    }
}

/// The mechanisms a server's `NEGOTIATE` response offers: the hints in its
/// security buffer, a `negTokenInit2` inside GSS-API's framing.
///
/// # Errors
///
/// [`SmbError::Malformed`] for anything else, [`SmbError::TooLarge`] for
/// more mechanisms than the bound.
pub fn offered(hints: &[u8]) -> Result<Vec<&[u8]>, SmbError> {
    let mut r = Reader::new(hints);
    let mechanisms = r.nested(Tag::application(0), |r| {
        if r.oid()? != OID {
            return Err(DerError::UnexpectedTag);
        }
        r.explicit(0, |r| {
            r.sequence(|r| {
                let mechanisms = r.explicit_optional(0, |r| r.sequence(mechanism_list))?;
                // reqFlags, mechToken, negHints and mechListMIC, unread.
                for field in 1..=4 {
                    r.explicit_optional(field, |r| {
                        while !r.is_empty() {
                            r.tlv()?;
                        }
                        Ok(())
                    })?;
                }
                Ok(mechanisms.unwrap_or_default())
            })
        })
    })?;
    r.finish()?;
    Ok(mechanisms)
}

fn mechanism_list<'a>(r: &mut Reader<'a>) -> Result<Vec<&'a [u8]>, DerError> {
    let mut list = Vec::new();
    while !r.is_empty() {
        if list.len() == MAX_MECHANISMS {
            return Err(DerError::TooLong);
        }
        list.try_reserve(1).map_err(|_| DerError::OutOfMemory)?;
        list.push(r.oid()?);
    }
    Ok(list)
}

#[cfg(test)]
#[path = "spnego_tests.rs"]
mod tests;
