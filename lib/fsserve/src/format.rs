//! What a format's driver crate supplies to be served: how it opens the
//! volume it is given, and the node it binds.

use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::WriteRefusal;
use tairix_abi::fsserve::ServeOffer;
use tairix_abi::volume::{FormatName, VolumeFsType};
use tairix_abi::{CapabilityId, DriverBindKey, DriverError, HwMatchKey};
use tairix_reclaim::PressureGauge;

use crate::ServedFilesystem;

/// What every served filesystem driver runs with: the authority to serve the
/// volume its grant names, and its diagnostics. It reaches nothing else.
pub const REQUIRED_CAPABILITIES: &[CapabilityId] =
    &[CapabilityId::FS_SERVE, CapabilityId::LOG_EMIT];

/// The priority a served format's bind key carries: an exact match on the
/// node the kernel published for one format.
const BIND_PRIORITY: u16 = 10;

/// The key a driver of `fstype` binds the volume nodes the kernel publishes
/// for that format by.
#[must_use]
pub const fn bind_key(fstype: VolumeFsType) -> DriverBindKey {
    DriverBindKey::new(
        BIND_PRIORITY,
        match HwMatchKey::compatible(fstype.served_compatible()) {
            Ok(key) => key,
            // Unreachable: every served compatible is a short literal, and a
            // long one would fail const evaluation here, never at run time.
            Err(_) => panic!("a served compatible fits HW_COMPATIBLE_MAX"),
        },
    )
}

/// What a driver bounds the writes it holds unpublished against.
#[derive(Copy, Clone)]
pub struct WritebackBudget {
    /// The memory the bound is a share of: the machine's.
    pub backing: usize,
    /// The pressure that lowers it.
    pub gauge: &'static dyn PressureGauge,
}

/// A volume its driver opened.
pub struct Opened<F> {
    /// The driver, over the volume.
    pub fs: F,
    /// What the volume is, down to its generation.
    pub format: FormatName,
    /// Why the driver will not write a volume it was given writable.
    pub write_refusal: Option<WriteRefusal>,
}

impl<F: ServedFilesystem> Opened<F> {
    /// The offer the volume is served under.
    pub fn offer(&mut self) -> ServeOffer {
        ServeOffer {
            root: self.fs.root(),
            name_matching: self.fs.name_matching(),
            node_identity: self.fs.node_identity(),
            attrs: self.fs.attrs_fs().is_some(),
            write_refusal: self.write_refusal,
            format: self.format,
        }
    }
}

/// A format a driver serves.
pub trait ServedFormat {
    /// The driver, over the volume it serves.
    type Fs<B: Block>: ServedFilesystem;

    /// Open `volume`, only for reading when `read_only`, bounding what it
    /// holds unpublished by `budget`.
    ///
    /// # Errors
    ///
    /// The format's refusal, which the driver gives the kernel as the reason
    /// the volume is not served.
    fn open<B: Block>(
        volume: B,
        read_only: bool,
        budget: WritebackBudget,
    ) -> Result<Opened<Self::Fs<B>>, DriverError>;
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
