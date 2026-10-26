//! The driver served from user space (`plans/FSSERVE.md` FSS5): the node
//! it binds and how it opens the volume that node's grant names.

use alloc::sync::Arc;

use tairix_abi::driver::block::Block;
use tairix_abi::driver::DriverBindKey;
use tairix_abi::volume::VolumeFsType;
use tairix_abi::DriverError;
use tairix_fsserve::{Opened, ServedFormat, WritebackBudget};
use tairix_reclaim::PinnedAccounting;

use crate::Ext4;

/// The node this driver binds: a volume the kernel attached as the ext family (ext2, ext3 and ext4).
pub const BIND_KEYS: &[DriverBindKey] = &[tairix_fsserve::bind_key(VolumeFsType::Ext4)];

/// the ext family (ext2, ext3 and ext4), served.
pub struct Served;

impl ServedFormat for Served {
    type Fs<B: Block> = Ext4<B>;

    fn open<B: Block>(
        volume: B,
        read_only: bool,
        budget: WritebackBudget,
    ) -> Result<Opened<Ext4<B>>, DriverError> {
        let fs = if read_only {
            Ext4::open_read_only(volume)
        } else {
            Ext4::open(volume)
        }?;
        let format = fs.generation().format();
        let write_refusal = if read_only { None } else { fs.write_refusal() };
        // A handle that will not write never stages a block, so it needs no
        // bound.
        let fs = if read_only || write_refusal.is_some() {
            fs
        } else {
            fs.with_writeback_bound(
                budget.backing,
                budget.gauge,
                Arc::new(PinnedAccounting::new()),
            )?
        };
        Ok(Opened {
            fs,
            format,
            write_refusal,
        })
    }
}
