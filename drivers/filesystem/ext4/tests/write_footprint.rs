//! Regression guard for the write path's footprint: storing bytes over data
//! already in place stages nothing, and laying out a fresh block stages that
//! block, never a whole run's worth.
//!
//! A correctness test cannot see this — a write that staged a run's worth of
//! zeros for every call returns the same answers — so the metered allocator
//! measures what each write allocates on the calling thread.

use tairix_abi::driver::filesystem::{FilesystemRead, FilesystemWrite, NodeKind, NodeSecurity};
use tairix_drv_fs_ext4::Ext4;
use tairix_fuzzseed::meter::{metered, Metered};
use tairix_writeback::RUN_BYTES;

#[path = "support/mem_block.rs"]
mod mem_block;
use mem_block::MemBlock;

#[global_allocator]
static GLOBAL: Metered = Metered;

#[test]
fn a_small_write_stages_no_more_than_the_blocks_it_lays_out() {
    let device = MemBlock {
        data: vec![0u8; 8 << 20],
    };
    let mut fs = Ext4::format(device, 64, [0x5A; 16]).expect("format");
    let root = fs.root();
    let f = fs
        .create(
            root,
            b"f",
            NodeKind::RegularFile,
            &NodeSecurity::new(0o644, 0, 0),
        )
        .expect("create");
    fs.write_at(f, 0, &[1; 4096]).expect("the first block");

    let ((), over) = metered(|| {
        fs.write_at(f, 100, &[2; 100]).expect("over data in place");
    });
    let ((), fresh) = metered(|| {
        fs.write_at(f, 4096, &[3; 100]).expect("into a fresh block");
    });
    let (over, fresh) = (over.requested, fresh.requested);
    assert!(
        over < RUN_BYTES / 2 && fresh < RUN_BYTES / 2,
        "a 100-byte write allocated {over} bytes over data in place and {fresh} into a fresh \
         block; neither stages a run"
    );

    let file = fs.lookup(root, b"f").expect("f");
    let mut read = [0u8; 4196];
    assert_eq!(fs.read_at(file, 0, &mut read), Ok(read.len()));
    assert!(read[..100].iter().all(|&b| b == 1));
    assert!(read[100..200].iter().all(|&b| b == 2));
    assert!(read[200..4096].iter().all(|&b| b == 1));
    assert!(read[4096..].iter().all(|&b| b == 3));
}
