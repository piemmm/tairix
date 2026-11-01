//! Loom models of the frame allocator, run by `cargo xtask loom`.
//!
//! They drive the public allocator API only, and loom explores every
//! interleaving of the allocator's internal `SpinLock`.

#![cfg(loom)]

use loom::sync::Arc;
use loom::thread;

use tairix_kernel_mem::{
    BootMemoryMap, FrameAllocator, MemoryClass, MemoryRegion, PhysAddr, RegionKind, PAGE_SIZE,
};

/// Above the frame at physical zero, which the allocator never enrols.
const RAM_BASE: u64 = 0x10_0000;

fn small_map(pages: usize) -> BootMemoryMap {
    let mut m = BootMemoryMap::new();
    m.push(MemoryRegion {
        start: PhysAddr::new(RAM_BASE),
        length: (pages * PAGE_SIZE) as u64,
        kind: RegionKind::Usable,
    });
    m
}

#[test]
fn concurrent_alloc_does_not_double_hand_out() {
    loom::model(|| {
        let a = Arc::new(FrameAllocator::new(&small_map(4)).unwrap());
        let a1 = a.clone();

        let t1 = thread::spawn(move || a1.alloc(MemoryClass::Kernel).ok());
        let f0 = a.alloc(MemoryClass::Kernel).ok();
        let f1 = t1.join().unwrap();

        let (Some(x), Some(y)) = (f0, f1) else {
            panic!("four free frames refused one of two requests: {f0:?} {f1:?}");
        };
        assert_ne!(x, y, "two threads got the same frame");
    });
}

#[test]
fn alloc_then_free_round_trip_concurrent() {
    loom::model(|| {
        let a = Arc::new(FrameAllocator::new(&small_map(2)).unwrap());
        let before = a.free_frames();
        let a1 = a.clone();
        let t1 = thread::spawn(move || {
            let f = a1
                .alloc(MemoryClass::Kernel)
                .expect("two frames for two requests");
            a1.free(f).unwrap();
        });
        let f = a
            .alloc(MemoryClass::Kernel)
            .expect("two frames for two requests");
        a.free(f).unwrap();
        t1.join().unwrap();
        assert_eq!(before, 2, "both frames enrolled");
        assert_eq!(a.free_frames(), before, "every frame drawn came back");
    });
}
