//! The driver served from user space (`plans/FSSERVE.md` FSS5): the node
//! it binds and how it opens the volume that node's grant names.

use tairix_abi::driver::block::Block;
use tairix_abi::driver::DriverBindKey;
use tairix_abi::volume::{FormatName, VolumeFsType};
use tairix_abi::DriverError;
use tairix_fsserve::{Opened, ServedFormat, WritebackBudget};

use crate::Fat32;

/// The node this driver binds: a volume the kernel attached as FAT32.
pub const BIND_KEYS: &[DriverBindKey] = &[tairix_fsserve::bind_key(VolumeFsType::Fat32)];

/// FAT32, served.
pub struct Served;

impl ServedFormat for Served {
    type Fs<B: Block> = Fat32<B>;

    fn open<B: Block>(
        volume: B,
        _read_only: bool,
        _budget: WritebackBudget,
    ) -> Result<Opened<Fat32<B>>, DriverError> {
        Ok(Opened {
            fs: Fat32::open(volume)?,
            format: FormatName::Fat32,
            write_refusal: None,
        })
    }
}
