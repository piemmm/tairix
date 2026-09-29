//! The desktop's clipboard: one payload the session holds, taken only from
//! and handed only to the window the user is working in, and wiped when
//! another replaces it.

use alloc::vec::Vec;

use tairix_abi::window_ipc::{ClipboardHeld, ClipboardKind, CLIPBOARD_MAX_BYTES};
use tairix_abi::Errno;
use tairix_util::secret::wipe;
use tairix_window::ClientRegion;

/// The session's reach into a shared-memory region a client granted it.
pub trait PayloadRegion {
    /// Append the first `len` bytes of `region` to `into`.
    ///
    /// # Errors
    ///
    /// The region cannot be mapped as its client's, or is shorter than `len`.
    fn read(&mut self, region: ClientRegion, len: usize, into: &mut Vec<u8>) -> Result<(), Errno>;

    /// Copy `from` to the start of `region`, answering whether it was long
    /// enough to take it.
    ///
    /// # Errors
    ///
    /// The region cannot be mapped as its client's.
    fn write(&mut self, region: ClientRegion, from: &[u8]) -> Result<bool, Errno>;
}

/// What a window host asks of the clipboard, once it has checked the asking
/// window is the one the user is working in.
pub trait ClipboardService {
    /// Take the first `len` bytes of `region` as the payload.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] past [`CLIPBOARD_MAX_BYTES`],
    /// [`Errno::OutOfMemory`], [`Errno::OutOfRange`] for text that is not
    /// UTF-8, or the region's own refusal.
    fn put(&mut self, region: ClientRegion, len: u64, kind: ClipboardKind) -> Result<(), Errno>;

    /// Copy the payload into `region`.
    ///
    /// # Errors
    ///
    /// The region's own refusal.
    fn get(&mut self, region: ClientRegion) -> Result<ClipboardHeld, Errno>;
}

/// The one clipboard of a session.
pub struct SessionClipboard<R> {
    regions: R,
    held: Option<(ClipboardKind, Vec<u8>)>,
}

impl<R> SessionClipboard<R> {
    /// An empty clipboard reaching regions through `regions`.
    pub const fn new(regions: R) -> Self {
        Self {
            regions,
            held: None,
        }
    }

    fn replace(&mut self, next: Option<(ClipboardKind, Vec<u8>)>) {
        if let Some((_, old)) = &mut self.held {
            wipe(old);
        }
        self.held = next;
    }
}

impl<R: PayloadRegion> ClipboardService for SessionClipboard<R> {
    fn put(&mut self, region: ClientRegion, len: u64, kind: ClipboardKind) -> Result<(), Errno> {
        let len = usize::try_from(len)
            .ok()
            .filter(|&len| len <= CLIPBOARD_MAX_BYTES)
            .ok_or(Errno::LengthOutOfRange)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(len)
            .map_err(|_| Errno::OutOfMemory)?;
        // Checked on the session's own copy: the region is the client's and
        // may change under a check made in place.
        let refused = match self.regions.read(region, len, &mut bytes) {
            Err(err) => Some(err),
            Ok(()) if kind == ClipboardKind::Text && core::str::from_utf8(&bytes).is_err() => {
                Some(Errno::OutOfRange)
            }
            Ok(()) => None,
        };
        if let Some(err) = refused {
            wipe(&mut bytes);
            return Err(err);
        }
        self.replace(Some((kind, bytes)));
        Ok(())
    }

    fn get(&mut self, region: ClientRegion) -> Result<ClipboardHeld, Errno> {
        let Some((kind, bytes)) = &self.held else {
            return Ok(ClipboardHeld {
                kind: None,
                len: 0,
                copied: false,
            });
        };
        let copied = self.regions.write(region, bytes)?;
        Ok(ClipboardHeld {
            kind: Some(*kind),
            len: u64::try_from(bytes.len()).map_err(|_| Errno::LengthOutOfRange)?,
            copied,
        })
    }
}

impl<R> Drop for SessionClipboard<R> {
    fn drop(&mut self) {
        self.replace(None);
    }
}

/// The clipboard of a bridge that serves no client request, such as one
/// only tearing a departed client's windows down: every ask is refused.
pub struct NoClipboard;

impl ClipboardService for NoClipboard {
    fn put(&mut self, _: ClientRegion, _: u64, _: ClipboardKind) -> Result<(), Errno> {
        Err(Errno::NotSupported)
    }

    fn get(&mut self, _: ClientRegion) -> Result<ClipboardHeld, Errno> {
        Err(Errno::NotSupported)
    }
}

#[cfg(test)]
#[path = "clipboard_tests.rs"]
mod tests;
