//! The desktop clipboard's paste policy: what a region offered for a paste
//! brings back. The session honours a copy or a paste only for the window
//! that holds the keyboard; [`WindowClient::set_clipboard`] and
//! [`WindowClient::ask_clipboard`] carry them.
//!
//! [`WindowClient::set_clipboard`]: crate::WindowClient::set_clipboard
//! [`WindowClient::ask_clipboard`]: crate::WindowClient::ask_clipboard

use alloc::vec::Vec;

use tairix_abi::window_ipc::{ClipboardHeld, ClipboardKind, CLIPBOARD_MAX_BYTES};
use tairix_abi::Errno;

/// The region first offered for a paste: most payloads fit it, and a longer
/// one is asked for again at the length the session stated.
pub(crate) const FIRST_OFFER: usize = 64 * 1024;

/// What a paste's answer comes to.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Landing {
    /// The clipboard's contents, `None` when it is empty.
    Taken(Option<(ClipboardKind, Vec<u8>)>),
    /// The payload did not fit: ask again through a region this long.
    Again(usize),
}

/// What `held`, the session's answer to a paste offered `region`, comes to:
/// the payload copied out, or the length to offer again unless `second`
/// already was the second offer.
///
/// # Errors
///
/// [`Errno::LengthOutOfRange`] for a payload past [`CLIPBOARD_MAX_BYTES`],
/// which no copy can have put there; [`Errno::Busy`] when the payload
/// changed size between the two offers a long one takes;
/// [`Errno::OutOfMemory`] when the copy cannot be made.
pub(crate) fn landed(held: ClipboardHeld, region: &[u8], second: bool) -> Result<Landing, Errno> {
    let Some(kind) = held.kind else {
        return Ok(Landing::Taken(None));
    };
    let len = usize::try_from(held.len)
        .ok()
        .filter(|&len| len <= CLIPBOARD_MAX_BYTES)
        .ok_or(Errno::LengthOutOfRange)?;
    // A payload that grew between offers into the mapping's page slack was
    // copied past what this region shows, so it is asked for again.
    if let Some(payload) = region.get(..len).filter(|_| held.copied) {
        let mut out = Vec::new();
        out.try_reserve_exact(len).map_err(|_| Errno::OutOfMemory)?;
        out.extend_from_slice(payload);
        return Ok(Landing::Taken(Some((kind, out))));
    }
    if second {
        return Err(Errno::Busy);
    }
    Ok(Landing::Again(len.max(1)))
}

#[cfg(test)]
mod tests {
    use super::{landed, Landing, FIRST_OFFER};
    use alloc::vec;
    use tairix_abi::window_ipc::{ClipboardHeld, ClipboardKind, CLIPBOARD_MAX_BYTES};
    use tairix_abi::Errno;

    /// The session's answer: a text payload of `len` bytes, copied or not.
    const fn text(len: u64, copied: bool) -> ClipboardHeld {
        ClipboardHeld {
            kind: Some(ClipboardKind::Text),
            len,
            copied,
        }
    }

    #[test]
    fn a_payload_that_fits_the_first_offer_is_taken_at_once() {
        let region = vec![7u8; FIRST_OFFER];
        assert_eq!(
            landed(text(5, true), &region, false),
            Ok(Landing::Taken(Some((ClipboardKind::Text, vec![7; 5]))))
        );
    }

    #[test]
    fn a_longer_payload_is_asked_for_again_at_the_length_the_session_stated() {
        let region = vec![7u8; FIRST_OFFER];
        let long = FIRST_OFFER * 3;
        assert_eq!(
            landed(text(long as u64, false), &region, false),
            Ok(Landing::Again(long))
        );
        let region = vec![7u8; long];
        assert_eq!(
            landed(text(long as u64, true), &region, true)
                .map(|landing| matches!(landing, Landing::Taken(Some((_, ref bytes))) if bytes.len() == long)),
            Ok(true)
        );
    }

    #[test]
    fn a_payload_that_keeps_growing_is_refused_as_busy_after_the_second_offer() {
        let region = vec![7u8; FIRST_OFFER * 2];
        assert_eq!(
            landed(text(FIRST_OFFER as u64 * 4, false), &region, true),
            Err(Errno::Busy)
        );
    }

    #[test]
    fn a_payload_copied_past_what_the_region_shows_is_asked_for_again() {
        let region = vec![7u8; 16];
        assert_eq!(
            landed(text(32, true), &region, false),
            Ok(Landing::Again(32))
        );
    }

    /// A session stating a length no copy can have put on the clipboard is
    /// not offered a region of it.
    #[test]
    fn a_length_past_the_clipboard_bound_is_refused_rather_than_offered() {
        let region = vec![0u8; FIRST_OFFER];
        let past = CLIPBOARD_MAX_BYTES as u64 + 1;
        assert_eq!(
            landed(text(past, false), &region, false),
            Err(Errno::LengthOutOfRange)
        );
    }

    #[test]
    fn an_empty_clipboard_answers_nothing() {
        let empty = ClipboardHeld {
            kind: None,
            len: 0,
            copied: false,
        };
        assert_eq!(landed(empty, &[], false), Ok(Landing::Taken(None)));
    }
}
