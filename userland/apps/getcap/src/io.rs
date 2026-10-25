//! The seams through which `getcap` touches the outside world beyond the
//! shared tree seam (`tairix_fscopy::tree`): a held node's gate, and the
//! terminal.
//!
//! Keeping both behind object-safe traits is what lets the reporting logic
//! in [`crate::client`] run against in-memory fixtures with no kernel.

use tairix_abi::{CapabilityId, Errno};
use tairix_fscopy::tree::Node;

/// What `getcap` needs beyond the shared tree seam: the capability gate of a
/// node it holds open, read through that node.
pub trait Gates {
    /// The gate `node` carries: [`Some`] capability, or [`None`] when it has
    /// none.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] reading the gate raises.
    fn capability(&self, node: &dyn Node) -> Result<Option<CapabilityId>, Errno>;
}

pub use tairix_util::output::Output;
