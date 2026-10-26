//! The driver served from user space (`plans/FSSERVE.md` FSS5): the node
//! it binds and how it opens the volume that node's grant names.

use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::WriteRefusal;
use tairix_abi::driver::DriverBindKey;
use tairix_abi::volume::{FormatName, VolumeFsType};
use tairix_abi::DriverError;
use tairix_fsserve::{Opened, ServedFormat, WritebackBudget};

use crate::Iso9660;

/// The node this driver binds: a volume the kernel attached as ISO 9660.
pub const BIND_KEYS: &[DriverBindKey] = &[tairix_fsserve::bind_key(VolumeFsType::Iso9660)];

/// ISO 9660, served.
pub struct Served;

impl ServedFormat for Served {
    type Fs<B: Block> = Iso9660<B>;

    fn open<B: Block>(
        volume: B,
        read_only: bool,
        _budget: WritebackBudget,
    ) -> Result<Opened<Iso9660<B>>, DriverError> {
        Ok(Opened {
            fs: Iso9660::open(volume)?,
            format: FormatName::Iso9660,
            write_refusal: (!read_only).then_some(WriteRefusal::FormatReadOnly),
        })
    }
}
