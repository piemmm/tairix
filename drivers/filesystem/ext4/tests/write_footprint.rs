//! Regression guard for the write path's footprint: storing bytes over data
//! already in place stages nothing, and laying out a fresh block stages that
//! block, never a whole run's worth.
//!
//! A correctness test cannot see this — a write that staged a run's worth of
//! zeros for every call returns the same answers — so a counting allocator
//! measures what each write allocates on the calling thread.

use std::alloc::System;
use std::cell::Cell;

use core::alloc::{GlobalAlloc, Layout};

use tairix_abi::driver::filesystem::{FilesystemRead, FilesystemWrite, NodeKind, NodeSecurity};
use tairix_drv_fs_ext4::Ext4;
use tairix_writeback::RUN_BYTES;

#[path = "support/mem_block.rs"]
mod mem_block;
use mem_block::MemBlock;

/// A pass-through allocator that counts the bytes each thread asks for, so a
/// measured call is not disturbed by tests running beside it.
struct CountingAlloc;

thread_local! {
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every call is forwarded unchanged to the system allocator, so its
// contract is exactly the caller's; the count is a thread-local cell that
// needs no allocation and no destructor, so reaching it cannot recurse.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATED.with(|count| count.set(count.get() + layout.size()));
        // SAFETY: the caller's layout, forwarded unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `alloc` above with this `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

/// The bytes `call` allocated on this thread.
fn allocated_by(call: impl FnOnce()) -> usize {
    let before = ALLOCATED.with(Cell::get);
    call();
    ALLOCATED.with(Cell::get) - before
}

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

    let over = allocated_by(|| {
        fs.write_at(f, 100, &[2; 100]).expect("over data in place");
    });
    let fresh = allocated_by(|| {
        fs.write_at(f, 4096, &[3; 100]).expect("into a fresh block");
    });
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
