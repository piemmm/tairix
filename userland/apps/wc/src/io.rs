//! The seams through which `wc` touches the outside world.
//!
//! Keeping the filesystem, standard input, and the two output streams
//! behind object-safe traits is what lets the counting logic in
//! [`crate::client`] run against in-memory fixtures with no kernel,
//! mirroring the seam design of the other userland tools (`cat`'s and
//! `head`'s `FileSource`/`Input`/`Output`).

use tairix_abi::Errno;

/// What a size probe of a named path learned, for the GNU output-width
/// rule: regular files contribute their byte size to the column width,
/// an existing non-regular node forces the wide (7-column) minimum, and
/// an unprobeable path contributes nothing at all — exactly the three
/// ways a `stat` outcome feeds GNU `wc`'s `compute_number_width`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SizeProbe {
    /// A regular file of this many bytes.
    Regular(u64),
    /// The path exists but is not a regular file (e.g. a directory).
    NotRegular,
    /// The path could not be probed (missing or unreadable).
    Unavailable,
}

/// Reads a byte range — and the regular-file size — of a named file.
///
/// The client streams a file by repeatedly calling [`read`](FileSource::read)
/// with an advancing `offset` until a call returns `0`, which marks
/// end-of-file. An implementation must return at most `buf.len()` bytes and
/// must report a short read (fewer than requested) only at end-of-file or by
/// returning an [`Errno`].
pub trait FileSource {
    /// Read up to `buf.len()` bytes of `path` starting at `offset`, returning
    /// the number of bytes written into `buf` (`0` at end-of-file).
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the filesystem raises — e.g. [`Errno::NotFound`] for a
    /// missing path or [`Errno::PermissionDenied`] when the caller may not
    /// read it.
    fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, Errno>;

    /// Probe `path` for the GNU output-width rule (see [`SizeProbe`]).
    ///
    /// This backs the column padding only — the count columns are sized
    /// from the summed sizes of the regular-file operands — so an honest
    /// [`SizeProbe::Unavailable`] merely changes the padding; it never
    /// changes a count.
    fn size(&self, path: &str) -> SizeProbe;
}

/// Reads the next bytes of standard input.
///
/// The client streams standard input by repeatedly calling
/// [`read`](Input::read) until a call returns `0` (end-of-input).
pub trait Input {
    /// Read up to `buf.len()` bytes of standard input, returning the number
    /// of bytes written into `buf` (`0` at end-of-input).
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the console raises (e.g. a closed terminal).
    fn read(&self, buf: &mut [u8]) -> Result<usize, Errno>;
}

pub use tairix_util::output::Output;
