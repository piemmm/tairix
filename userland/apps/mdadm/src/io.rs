//! The seams through which `mdadm` touches the outside world.
//!
//! Keeping the reads, the mutations, and the output behind object-safe traits
//! is what lets the engine in [`crate::client`] run against in-memory fixtures
//! with no kernel, mirroring the seam discipline of the other userland tools
//! (`lspci`'s `Transport`/`Output`, `df`'s `Output`). The freestanding `Run`
//! binary binds the production forms: [`Reader`] over the shared System
//! Information client (`tairix_procinfo::raid_arrays` / `raid_members`) and
//! [`Controller`] over a single `ipc_call` to the composer's control endpoint.

use alloc::vec::Vec;

use tairix_abi::raid_admin::{RaidArrayRecord, RaidMemberRecord};
use tairix_abi::Errno;

/// Reads the composer's live array and device inventory.
///
/// Both reads carry the same authority the hardware tree is read under
/// (`CAP_SYSINFO_HW`, enforced by the composer against the caller's
/// kernel-attested origin), so a refusal surfaces as
/// [`Errno::PermissionDenied`] and the tool reports it — it never fabricates
/// an inventory.
pub trait Reader {
    /// The live arrays the composer serves.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the query raises, in particular
    /// [`Errno::PermissionDenied`] when the caller lacks `CAP_SYSINFO_HW`.
    fn arrays(&self) -> Result<Vec<RaidArrayRecord>, Errno>;

    /// Every device the composer holds: array members and the unaffiliated
    /// candidates a new array can be created over.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the query raises, as for [`Self::arrays`].
    fn members(&self) -> Result<Vec<RaidMemberRecord>, Errno>;
}

/// Carries one encoded array-control frame to the composer and returns its
/// reply bytes.
///
/// The composer checks the operation's required capability
/// (`CAP_STORAGE_ADMIN` for a mutation) against the caller's kernel-attested
/// origin before it acts; this seam conveys no authority and the engine reads
/// the outcome from the reply.
pub trait Controller {
    /// Post `request` to the control endpoint and copy the reply into
    /// `reply`, returning the number of reply bytes written.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the IPC path or the composer raises.
    fn call(&self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno>;
}

pub use tairix_util::output::Output;
