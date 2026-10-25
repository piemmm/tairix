//! The seams through which `setcap` touches the outside world beyond the
//! shared tree seam (`tairix_fscopy::tree`): a held node's gate, and the
//! terminal.
//!
//! Keeping both behind object-safe traits is what lets the gate-setting logic
//! in [`crate::client`] run against in-memory fixtures with no kernel.

use tairix_abi::{CapabilityId, Errno};
use tairix_fscopy::tree::Node;

/// What `setcap` needs beyond the shared tree seam: installing or clearing
/// the capability gate of a node it holds open, through that node.
pub trait Gates {
    /// Install `cap` as `node`'s gate, or clear its gate for [`None`].
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the change raises — e.g. [`Errno::PermissionDenied`]
    /// when the caller lacks the authority to set a gate.
    fn set_capability(&self, node: &dyn Node, cap: Option<CapabilityId>) -> Result<(), Errno>;
}

pub use tairix_util::output::Output;
