//! The desktop clipboard, from an application's side: a payload goes out
//! through a region this program creates and grants the session, and comes
//! back the same way. The session honours either only for the window that
//! holds the keyboard.

use alloc::vec::Vec;

use tairix_abi::window_ipc::{ClipboardKind, CLIPBOARD_MAX_BYTES};
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
    let mut offer = FIRST_OFFER;
    for _ in 0..2 {
        let (mut region, grant) = granted_region(offer).ok_or(Errno::OutOfMemory)?;
        let held = client.get_clipboard(window_id, grant)?;
        let Some(kind) = held.kind else {
            return Ok(None);
        };
        let len = usize::try_from(held.len).map_err(|_| Errno::LengthOutOfRange)?;
        // A payload that grew between asks into the mapping's page slack was
        // copied past what this region shows, so it is asked for again.
        if let Some(payload) = region.bytes_mut().get(..len).filter(|_| held.copied) {
            let mut out = Vec::new();
            out.try_reserve_exact(len).map_err(|_| Errno::OutOfMemory)?;
            out.extend_from_slice(payload);
            return Ok(Some((kind, out)));
        }
        offer = len.max(1);
    }
    Err(Errno::Busy)
}
