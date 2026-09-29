//! The desktop clipboard, from an application's side: a payload goes out
//! through a region this program creates and grants the session, and comes
//! back the same way. The session honours either only for the window that
//! holds the keyboard.

use alloc::vec::Vec;

use tairix_abi::window_ipc::{ClipboardHeld, ClipboardKind, CLIPBOARD_MAX_BYTES};
use tairix_abi::Errno;

use crate::client::{WindowClient, WindowTransport};
use crate::frames::granted_region;

/// The region first offered for a paste: most payloads fit it, and a longer
/// one is asked for again at the length the session stated.
const FIRST_OFFER: usize = 64 * 1024;

/// Put `bytes` on the clipboard as `kind`, from this program's focused
/// window `window_id`.
///
/// # Errors
///
/// [`Errno::LengthOutOfRange`] past [`CLIPBOARD_MAX_BYTES`];
/// [`Errno::OutOfMemory`] when the region cannot be made; otherwise the
/// session's refusal.
pub fn put<T: WindowTransport>(
    client: &mut WindowClient<T>,
    window_id: u64,
    kind: ClipboardKind,
    bytes: &[u8],
) -> Result<(), Errno> {
    if bytes.len() > CLIPBOARD_MAX_BYTES {
        return Err(Errno::LengthOutOfRange);
    }
    let (mut region, grant) = granted_region(bytes.len().max(1)).ok_or(Errno::OutOfMemory)?;
    region.bytes_mut()[..bytes.len()].copy_from_slice(bytes);
    let len = u64::try_from(bytes.len()).map_err(|_| Errno::LengthOutOfRange)?;
    client.set_clipboard(window_id, grant, len, kind)
}

/// What the clipboard holds, for this program's focused window
/// `window_id`: `None` when it is empty.
///
/// # Errors
///
/// [`Errno::OutOfMemory`] when a region or the copy cannot be made;
/// [`Errno::Busy`] when the payload changed size between the two asks a long
/// one takes; otherwise the session's refusal.
pub fn take<T: WindowTransport>(
    client: &mut WindowClient<T>,
    window_id: u64,
) -> Result<Option<(ClipboardKind, Vec<u8>)>, Errno> {
    take_through(
        |offer| {
            let (region, grant) = granted_region(offer).ok_or(Errno::OutOfMemory)?;
            Ok((region, client.get_clipboard(window_id, grant)?))
        },
        |region| region.bytes_mut(),
    )
}

/// [`take`]'s policy over `ask`, which offers the session a region of the
/// length given and answers the region and the reply; `bytes` is what the
/// region shows.
fn take_through<R>(
    mut ask: impl FnMut(usize) -> Result<(R, ClipboardHeld), Errno>,
    bytes: impl Fn(&mut R) -> &[u8],
) -> Result<Option<(ClipboardKind, Vec<u8>)>, Errno> {
    let mut offer = FIRST_OFFER;
    for _ in 0..2 {
        let (mut region, held) = ask(offer)?;
        let Some(kind) = held.kind else {
            return Ok(None);
        };
        let len = usize::try_from(held.len).map_err(|_| Errno::LengthOutOfRange)?;
        // A payload that grew between asks into the mapping's page slack was
        // copied past what this region shows, so it is asked for again.
        if let Some(payload) = bytes(&mut region).get(..len).filter(|_| held.copied) {
            let mut out = Vec::new();
            out.try_reserve_exact(len).map_err(|_| Errno::OutOfMemory)?;
            out.extend_from_slice(payload);
            return Ok(Some((kind, out)));
        }
        offer = len.max(1);
    }
    Err(Errno::Busy)
}

#[cfg(test)]
mod tests {
    use super::{take_through, FIRST_OFFER};
    use alloc::vec;
    use alloc::vec::Vec;
    use tairix_abi::window_ipc::{ClipboardHeld, ClipboardKind};
    use tairix_abi::Errno;

    /// A session answering each ask in turn with the payload length it holds
    /// and whether it filled the region offered, recording every offer.
    struct Session {
        answers: Vec<(u64, bool)>,
        offers: Vec<usize>,
    }

    impl Session {
        fn answering(answers: &[(u64, bool)]) -> Self {
            Self {
                answers: answers.to_vec(),
                offers: Vec::new(),
            }
        }

        fn ask(&mut self, offer: usize) -> (Vec<u8>, ClipboardHeld) {
            let (len, copied) = self.answers[self.offers.len()];
            self.offers.push(offer);
            let held = ClipboardHeld {
                kind: Some(ClipboardKind::Text),
                len,
                copied,
            };
            (vec![7u8; offer], held)
        }

        fn take(&mut self) -> Result<Option<(ClipboardKind, Vec<u8>)>, Errno> {
            take_through(
                |offer| Ok(self.ask(offer)),
                |region: &mut Vec<u8>| region.as_slice(),
            )
        }
    }

    #[test]
    fn a_payload_that_fits_the_first_offer_is_taken_at_once() {
        let mut session = Session::answering(&[(5, true)]);
        assert_eq!(session.take(), Ok(Some((ClipboardKind::Text, vec![7; 5]))));
        assert_eq!(session.offers, [FIRST_OFFER]);
    }

    #[test]
    fn a_longer_payload_is_asked_for_again_at_the_length_the_session_stated() {
        let long = FIRST_OFFER as u64 * 3;
        let mut session = Session::answering(&[(long, false), (long, true)]);
        assert_eq!(
            session
                .take()
                .map(|held| held.map(|(_, bytes)| bytes.len())),
            Ok(Some(FIRST_OFFER * 3))
        );
        assert_eq!(session.offers, [FIRST_OFFER, FIRST_OFFER * 3]);
    }

    #[test]
    fn a_payload_that_keeps_growing_is_refused_as_busy_after_the_second_ask() {
        let long = FIRST_OFFER as u64 * 2;
        let mut session = Session::answering(&[(long, false), (long * 2, false)]);
        assert_eq!(session.take(), Err(Errno::Busy));
        assert_eq!(session.offers.len(), 2);
    }

    #[test]
    fn an_empty_clipboard_answers_nothing() {
        let taken = take_through(
            |_| {
                Ok((
                    Vec::new(),
                    ClipboardHeld {
                        kind: None,
                        len: 0,
                        copied: false,
                    },
                ))
            },
            |region: &mut Vec<u8>| region.as_slice(),
        );
        assert_eq!(taken, Ok(None));
    }
}
