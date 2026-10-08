//! Unit tests for the capability-gated DMA entry points.
//!
//! Every test pairs a synthetic [`TaskCapabilities`] with a fresh
//! [`DmaPool`] driven by `kernel/mem`'s `HostPageTable` and a small
//! `FrameAllocator`. The [`RecordingSink`] from `crate::audit` captures
//! the exact audit-event sequence so the security trail is asserted
//! alongside the functional outcome.

extern crate alloc;

use super::*;
use crate::audit::RecordingSink;
use crate::captable::{ProcessId, TaskCapabilities};
use crate::identity::UserId;
use tairix_abi::CapabilityId;
use tairix_caps::CapabilitySet;
use tairix_kernel_mem::{
    bootinfo::{BootMemoryMap, MemoryRegion, RegionKind},
    AddressSpace, DmaPool, FrameAllocator, HostPageTable, PhysAddr, SimPhysMap, VirtAddr,
    PAGE_SIZE,
};

fn caps_of(items: &[CapabilityId]) -> CapabilitySet {
    let mut s = CapabilitySet::empty();
    for c in items {
        s.insert(*c);
    }
    s
}

fn task_with(caps: &[CapabilityId], sink: &RecordingSink) -> TaskCapabilities {
    let user_grant = caps_of(caps);
    let manifest_request = user_grant;
    TaskCapabilities::derive(
        ProcessId(42),
        UserId(1000),
        user_grant,
        manifest_request,
        sink,
    )
}

fn small_map(usable_pages: usize) -> BootMemoryMap {
    let mut m = BootMemoryMap::new();
    m.push(MemoryRegion {
        kind: RegionKind::Usable,
        start: PhysAddr::new(PAGE_SIZE as u64 * 16),
        length: (PAGE_SIZE * usable_pages) as u64,
    });
    m
}

/// Simulated physical RAM covering the frame allocator's usable
/// region, so the pool can reach a buffer's frames from the CPU.
fn fresh_sim() -> SimPhysMap {
    SimPhysMap::new(PhysAddr::new(PAGE_SIZE as u64 * 16), 16 * PAGE_SIZE)
}

fn fresh_pool<'a>(frames: &'a FrameAllocator, sim: &'a SimPhysMap) -> DmaPool<'a, HostPageTable> {
    DmaPool::new(
        AddressSpace::new(HostPageTable::new()),
        VirtAddr::new(0x2000_0000),
        16,
        frames,
        sim,
        tairix_abi::DmaCoherence::Snooped,
    )
    .expect("pool constructs")
}

/// Recording sink that ignores the `TaskCapabilitiesDerived` event
/// emitted by [`task_with`] so each test asserts only DMA-relevant ids.
fn ids_after_derive(sink: &RecordingSink) -> alloc::vec::Vec<u32> {
    sink.ids()
        .into_iter()
        .filter(|&id| id != AuditEvent::TaskCapabilitiesDerived.id().0)
        .collect()
}

#[test]
fn alloc_succeeds_when_caller_holds_mem_dma() {
    let frames = FrameAllocator::new(&small_map(16)).unwrap();
    let sim = fresh_sim();
    let mut pool = fresh_pool(&frames, &sim);
    let sink = RecordingSink::new();
    let caller = task_with(&[CapabilityId::MEM_DMA], &sink);
    let buf = alloc_dma(&mut pool, &caller, PAGE_SIZE, 0, &sink).expect("granted");
    assert_eq!(buf.len(), PAGE_SIZE);
    assert_eq!(
        ids_after_derive(&sink),
        [AuditEvent::DmaAllocated.id().0],
        "exactly one DmaAllocated event must be emitted"
    );
}

#[test]
fn a_granted_carve_honours_the_devices_reach() {
    let frames = FrameAllocator::new(&small_map(16)).unwrap();
    let sim = fresh_sim();
    let mut pool = fresh_pool(&frames, &sim);
    let sink = RecordingSink::new();
    let caller = task_with(&[CapabilityId::MEM_DMA], &sink);
    let limit = PAGE_SIZE as u64 * 20;
    let buf = alloc_dma(&mut pool, &caller, PAGE_SIZE, limit, &sink).expect("granted");
    assert!(buf.phys().as_u64() + buf.len() as u64 <= limit);
    let unreachable = alloc_dma(&mut pool, &caller, PAGE_SIZE, PAGE_SIZE as u64 * 16, &sink);
    assert!(matches!(unreachable, Err(DmaGateError::Pool(_))));
}

#[test]
fn alloc_refused_without_mem_dma() {
    let frames = FrameAllocator::new(&small_map(16)).unwrap();
    let sim = fresh_sim();
    let mut pool = fresh_pool(&frames, &sim);
    let sink = RecordingSink::new();
    // The caller holds *other* capabilities but not MEM_DMA, so the
    // gate is the only thing standing between it and the buffer.
    let caller = task_with(&[CapabilityId::FS_MOUNT, CapabilityId::NET_RAW], &sink);
    let err = alloc_dma(&mut pool, &caller, PAGE_SIZE, 0, &sink).unwrap_err();
    assert_eq!(err, DmaGateError::CapabilityMissing);
    assert_eq!(err.as_driver_error(), DriverError::PermissionDenied);
    assert_eq!(
        ids_after_derive(&sink),
        [AuditEvent::DmaAllocDenied.id().0],
        "denial must produce exactly one DmaAllocDenied event"
    );
    // Refusal must not consume any frames.
    assert_eq!(pool.live(), 0);
}

#[test]
fn alloc_zero_size_propagates_pool_error_with_audit() {
    let frames = FrameAllocator::new(&small_map(16)).unwrap();
    let sim = fresh_sim();
    let mut pool = fresh_pool(&frames, &sim);
    let sink = RecordingSink::new();
    let caller = task_with(&[CapabilityId::MEM_DMA], &sink);
    let err = alloc_dma(&mut pool, &caller, 0, 0, &sink).unwrap_err();
    assert_eq!(err, DmaGateError::Pool(DmaError::ZeroSize));
    assert_eq!(err.as_driver_error(), DriverError::BufferTooSmall);
    // The capability check passed, the pool refused — no DmaAllocated
    // record (the alloc never succeeded) and no DmaAllocDenied (the
    // capability was held).
    assert!(ids_after_derive(&sink).is_empty());
}

#[test]
fn an_oversized_carve_is_too_long_not_exhausted() {
    let frames = FrameAllocator::new(&small_map(16)).unwrap();
    let sim = fresh_sim();
    let mut pool = fresh_pool(&frames, &sim);
    let sink = RecordingSink::new();
    let caller = task_with(&[CapabilityId::MEM_DMA], &sink);
    // Exceed MAX_ORDER ⇒ DmaError::SizeUnsupported.
    let too_big = (1usize << (tairix_kernel_mem::MAX_ORDER + 1)) * PAGE_SIZE;
    let err = alloc_dma(&mut pool, &caller, too_big, 0, &sink).unwrap_err();
    assert_eq!(err.as_driver_error(), DriverError::LengthOutOfRange);
}

#[test]
fn alloc_then_free_round_trip_emits_one_audit_record() {
    let frames = FrameAllocator::new(&small_map(16)).unwrap();
    let sim = fresh_sim();
    let mut pool = fresh_pool(&frames, &sim);
    let sink = RecordingSink::new();
    let caller = task_with(&[CapabilityId::MEM_DMA], &sink);
    let buf = alloc_dma(&mut pool, &caller, PAGE_SIZE, 0, &sink).expect("alloc");
    free_dma(&mut pool, &caller, buf, &sink).expect("free");
    // Free is silent on success — the audit value is in the *grant*,
    // not the matching release. One DmaAllocated event is correct.
    assert_eq!(ids_after_derive(&sink), [AuditEvent::DmaAllocated.id().0],);
    assert_eq!(pool.live(), 0);
}

#[test]
fn free_refused_without_mem_dma_and_buffer_is_retained() {
    let frames = FrameAllocator::new(&small_map(16)).unwrap();
    let sim = fresh_sim();
    let mut pool = fresh_pool(&frames, &sim);
    let granted_sink = RecordingSink::new();
    let granter = task_with(&[CapabilityId::MEM_DMA], &granted_sink);
    let buf = alloc_dma(&mut pool, &granter, PAGE_SIZE, 0, &granted_sink).expect("alloc");

    let revoked_sink = RecordingSink::new();
    let revoked = task_with(&[CapabilityId::FS_MOUNT], &revoked_sink);
    let err = free_dma(&mut pool, &revoked, buf, &revoked_sink).unwrap_err();
    assert_eq!(err, DmaGateError::CapabilityMissing);
    assert_eq!(err.as_driver_error(), DriverError::PermissionDenied);
    assert_eq!(
        ids_after_derive(&revoked_sink),
        [AuditEvent::DmaAllocDenied.id().0]
    );
    // The buffer must still be live in the pool.
    assert_eq!(pool.live(), 1);
}

/// A refused carve reaches its driver as what it was: exhausted memory is
/// never a bad length or a faulted device.
#[test]
fn a_refused_carve_reaches_the_driver_as_itself() {
    use tairix_kernel_mem::AllocError;
    for (refusal, read) in [
        (
            DmaGateError::CapabilityMissing,
            DriverError::PermissionDenied,
        ),
        (
            DmaGateError::Pool(DmaError::Alloc(AllocError::OutOfMemory)),
            DriverError::OutOfMemory,
        ),
        (
            DmaGateError::Pool(DmaError::ZeroSize),
            DriverError::BufferTooSmall,
        ),
        (
            DmaGateError::Pool(DmaError::Alloc(AllocError::ZeroSize)),
            DriverError::BufferTooSmall,
        ),
        (
            DmaGateError::Pool(DmaError::SizeUnsupported),
            DriverError::LengthOutOfRange,
        ),
        (
            DmaGateError::Pool(DmaError::Alloc(AllocError::SizeUnsupported)),
            DriverError::LengthOutOfRange,
        ),
        (
            DmaGateError::Pool(DmaError::Alloc(AllocError::OutOfRange)),
            DriverError::OutOfRange,
        ),
        (
            DmaGateError::Pool(DmaError::UnknownBuffer),
            DriverError::OutOfRange,
        ),
        (
            DmaGateError::Pool(DmaError::DirectMap),
            DriverError::OutOfRange,
        ),
        (
            DmaGateError::Pool(DmaError::InvalidPoolConfig),
            DriverError::OutOfRange,
        ),
    ] {
        assert_eq!(refusal.as_driver_error(), read, "{refusal:?}");
    }
}
