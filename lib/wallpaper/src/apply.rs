//! Asking the desktop session to adopt a settings document.
//!
//! The one client of the pinboard rendezvous's applies, shared by every
//! surface that edits a settings registry the session keeps — its own and
//! the file manager's. A second copy of this round trip would be two places
//! for "what did the session say" to drift apart.
//!
//! Nothing here holds authority. An application publishes only its own
//! app-data scope, so no surface can write the desktop's document at all: it
//! renders the keys it edits and *asks*, and the session decides, applies,
//! persists, and answers with what the store did.

use alloc::string::String;

#[cfg(feature = "rt")]
use tairix_abi::pinboard_ipc::{
    PinboardDocument, PinboardRequest, PINBOARD_ENDPOINT, PINBOARD_MAX_REQUEST,
};
#[cfg(feature = "rt")]
use tairix_abi::reply::{decode_status_reply, STATUS_REPLY_LEN};

/// The outcome of asking the desktop session to adopt a rendered settings
/// document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApplyOutcome {
    /// The session adopted and persisted the change.
    Applied,
    /// The session refused the request, with the reason it gave.
    Refused(String),
    /// No desktop session answered the pinboard rendezvous at all (the
    /// call itself failed) — distinct from an authenticated refusal.
    NoDesktop,
}

/// Which of the registries the session keeps an applied document is for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ApplyTo {
    /// The desktop's own settings (`crate::DesktopSettings`).
    Desktop,
    /// The file manager's (`tairix_browse::FilerPreferences`).
    Files,
}

/// Ask the desktop session to adopt `document` as `to`'s settings and report
/// what it said.
///
/// Nothing is reported as applied that the session did not accept: a
/// rendezvous that does not answer and a typed refusal are distinct
/// outcomes, and the reply waits for the *store*, so an `Applied` means the
/// document was actually written.
#[cfg(feature = "rt")]
#[must_use]
pub fn apply(to: ApplyTo, document: PinboardDocument) -> ApplyOutcome {
    let request = match to {
        ApplyTo::Desktop => PinboardRequest::Apply { document },
        ApplyTo::Files => PinboardRequest::ApplyFiles { document },
    };
    let mut frame = [0u8; PINBOARD_MAX_REQUEST];
    let Ok(sent) = request.encode(&mut frame) else {
        return ApplyOutcome::Refused(alloc::format!("{:?}", tairix_abi::Errno::LengthOutOfRange));
    };
    let mut reply = [0u8; STATUS_REPLY_LEN];
    let Ok(len) = tairix_rt::ipc_call(PINBOARD_ENDPOINT, &frame[..sent], &mut reply) else {
        return ApplyOutcome::NoDesktop;
    };
    match decode_status_reply(&reply[..len.min(reply.len())]) {
        Ok(()) => ApplyOutcome::Applied,
        Err(err) => ApplyOutcome::Refused(alloc::format!("{err:?}")),
    }
}
