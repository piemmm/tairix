//! The driver served from user space (`plans/FSSERVE.md` FSS5): the node
//! it binds and how it opens the volume that node's grant names.

use tairix_abi::driver::block::Block;
use tairix_abi::driver::DriverBindKey;
use tairix_abi::volume::{FormatName, VolumeFsType};
use tairix_abi::DriverError;
use tairix_fsserve::{Opened, ServedFormat, WritebackBudget};

use crate::Udf;

/// The node this driver binds: a volume the kernel attached as UDF.
pub const BIND_KEYS: &[DriverBindKey] = &[tairix_fsserve::bind_key(VolumeFsType::Udf)];

/// UDF, served.
pub struct Served;

impl ServedFormat for Served {
    type Fs<B: Block> = Udf<B>;

    fn open<B: Block>(
        volume: B,
        read_only: bool,
        _budget: WritebackBudget,
    ) -> Result<Opened<Udf<B>>, DriverError> {
        let fs = Udf::open(volume)?;
        let write_refusal = if read_only { None } else { fs.write_refusal() };
        Ok(Opened {
            fs,
            format: FormatName::Udf,
            write_refusal,
        })
    }
}
