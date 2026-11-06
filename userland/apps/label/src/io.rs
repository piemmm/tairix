//! The seams through which `label` touches the outside world.
//!
//! The mount and view listings arrive through the shared
//! [`Transport`](tairix_procinfo::Transport) seam, and who names a volume and
//! how a path they spell resolves through the shared [`Caller`] seam the
//! retitle extends; only the retitle and the output stream need seams of their
//! own, so the engine runs against in-memory fixtures with no kernel.

use tairix_abi::driver::filesystem::VolumeTitle;
use tairix_abi::volume::VOLUME_ID_LEN;
use tairix_abi::Errno;
use tairix_procinfo::Caller;

/// Titles a volume, for the [`Caller`] that names it.
///
/// The implementation performs no authorisation of its own: the kernel's
/// `volume_retitle` path decides who may retitle the volume, re-validates its
/// key against the attached volumes, and audits every decision. A refusal
/// surfaces as the exact [`Errno`] the kernel chose.
pub trait Retitler: Caller {
    /// Title the volume published under `volume_id` `title`; an empty one
    /// clears it.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the kernel raises — e.g. [`Errno::PermissionDenied`] for
    /// a volume the caller may not retitle, [`Errno::NotSupported`] for one
    /// whose format takes no title, [`Errno::ReadOnly`] for one that takes no
    /// writes, [`Errno::Busy`] while another operation holds it, or
    /// [`Errno::LengthOutOfRange`] or [`Errno::OutOfRange`] for a title its
    /// format cannot store.
    fn retitle(&self, volume_id: [u8; VOLUME_ID_LEN], title: &VolumeTitle) -> Result<(), Errno>;
}

pub use tairix_util::output::Output;
