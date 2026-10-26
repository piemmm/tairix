//! The driver served from user space (`plans/FSSERVE.md` FSS5): the node
//! it binds and how it opens the volume that node's grant names.

use tairix_abi::driver::block::Block;
use tairix_abi::driver::DriverBindKey;
use tairix_abi::volume::{FormatName, VolumeFsType};
use tairix_abi::DriverError;
use tairix_fsserve::{Opened, ServedFormat, WritebackBudget};

use crate::Adfs;

/// The node this driver binds: a volume the kernel attached as ADFS.
pub const BIND_KEYS: &[DriverBindKey] = &[tairix_fsserve::bind_key(VolumeFsType::Adfs)];

/// ADFS, served.
pub struct Served;

impl ServedFormat for Served {
    type Fs<B: Block> = Adfs<B>;

    fn open<B: Block>(
        volume: B,
        _read_only: bool,
        _budget: WritebackBudget,
    ) -> Result<Opened<Adfs<B>>, DriverError> {
        Ok(Opened {
            fs: Adfs::open(volume)?,
            format: FormatName::Adfs,
            write_refusal: None,
        })
    }
}
