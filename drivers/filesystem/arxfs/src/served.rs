//! The driver served from user space (`plans/FSSERVE.md` FSS5): the node
//! it binds and how it opens the volume that node's grant names.

use alloc::sync::Arc;

use tairix_abi::driver::block::Block;
use tairix_abi::driver::DriverBindKey;
use tairix_abi::volume::{FormatName, VolumeFsType};
use tairix_abi::DriverError;
use tairix_fsserve::{Opened, ServedFormat, WritebackBudget};
use tairix_reclaim::PinnedAccounting;

use crate::{Keying, ARXFS};

/// The node this driver binds: a volume the kernel attached as ARXFS.
pub const BIND_KEYS: &[DriverBindKey] = &[tairix_fsserve::bind_key(VolumeFsType::ARXFS)];

/// ARXFS, served.
pub struct Served;

impl ServedFormat for Served {
    type Fs<B: Block> = ARXFS<B>;

    fn open<B: Block>(
        volume: B,
        read_only: bool,
        budget: WritebackBudget,
    ) -> Result<Opened<ARXFS<B>>, DriverError> {
        // A removable volume is served plaintext: an encrypted one refuses
        // the open with a typed error, and the kernel never guesses a key.
        let fs = if read_only {
            ARXFS::open_read_only(volume, Keying::Plaintext)?
        } else {
            ARXFS::open(volume, Keying::Plaintext)?.with_writeback_bound(
                budget.backing,
                budget.gauge,
                Arc::new(PinnedAccounting::new()),
            )?
        };
        Ok(Opened {
            fs,
            format: FormatName::Arxfs,
            write_refusal: None,
        })
    }
}
