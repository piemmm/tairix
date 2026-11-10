use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use tairix_abi::time::MonotonicClock;
use tairix_abi::Duration64;

use tairix_parallel::Serial;

use crate::{
    Allocation, AllocationDesc, Buffer, BufferDesc, BufferUsage, CommandBuffer, Context,
    ContextDesc, CopyRegion, Device, Error, Fence, Loss, NoThreads, Queue, SoftwareDesc, Spawn,
    Submission, SubmitError, DEFAULT_SUBGROUP_SIZE, MAX_UPDATE_BYTES,
};

/// A clock a test sets, advancing by `step` at every reading, which it
/// counts.
#[derive(Default)]
pub(crate) struct Clock {
    pub(crate) now: AtomicU64,
    pub(crate) step: AtomicU64,
    pub(crate) readings: AtomicU64,
}

impl MonotonicClock for Clock {
    fn now_ns(&self) -> u64 {
        self.readings.fetch_add(1, Ordering::SeqCst);
        self.now
            .fetch_add(self.step.load(Ordering::SeqCst), Ordering::SeqCst)
    }
}

/// The host's own clock, for a test whose waits must time out for real.
struct HostClock(std::time::Instant);

impl MonotonicClock for HostClock {
    fn now_ns(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// The trap that asks the kernel to wake waiters, as the host's seam
/// records it.
pub(crate) const FUTEX_WAKE: u64 = tairix_abi::SyscallNumber::FUTEX_WAKE.as_u16() as u64;

/// How many of `calls` asked the kernel to wake at most `count` waiters.
pub(crate) fn wakes(calls: &[tairix_abi_trap::seam::Trap], count: u32) -> usize {
    calls
        .iter()
        .filter(|(number, args)| *number == FUTEX_WAKE && args.get(1) == Some(&u64::from(count)))
        .count()
}

/// Whether `done` holds within a minute, another thread's progress being what
/// it waits on: answered rather than asserted, so a failing test can let that
/// thread go before it fails, and leaves nothing running.
pub(crate) fn until(mut done: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !done() {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::yield_now();
    }
    true
}

pub(crate) const RAM: u64 = 4 << 20;
const MEBIBYTE: u64 = 1 << 20;
pub(crate) const SECOND: Duration64 = Duration64::from_secs(1);

/// A software device of `memory` bytes whose work runs on the thread whose
/// act made it ready, so a test sees it done when the call returns.
pub(crate) fn software(memory: u64, clock: Arc<dyn MonotonicClock + Send + Sync>) -> Device {
    software_on(memory, clock, Arc::new(Serial))
}

/// [`software`], its commands' work shared across `runner`.
pub(crate) fn software_on(
    memory: u64,
    clock: Arc<dyn MonotonicClock + Send + Sync>,
    runner: Arc<dyn tairix_parallel::JobRunner + Send>,
) -> Device {
    Device::software(&SoftwareDesc {
        memory_bytes: memory,
        clock,
        runner,
        threads: &NoThreads,
        subgroup_size: DEFAULT_SUBGROUP_SIZE,
    })
    .expect("a software device")
}

/// A spawner that keeps the worker's body and never runs it: a worker that
/// is never parked, and work that queues and is never run.
#[derive(Default)]
pub(crate) struct Unstarted(std::sync::Mutex<Vec<alloc::boxed::Box<dyn FnOnce() + Send>>>);

impl Spawn for Unstarted {
    fn spawn(&self, body: alloc::boxed::Box<dyn FnOnce() + Send>) -> bool {
        self.0.lock().expect("unpoisoned").push(body);
        true
    }
}

pub(crate) fn unstarted(threads: &Unstarted) -> Device {
    Device::software(&SoftwareDesc {
        memory_bytes: RAM,
        clock: Arc::new(Clock::default()),
        runner: Arc::new(Serial),
        threads,
        subgroup_size: DEFAULT_SUBGROUP_SIZE,
    })
    .expect("a software device")
}

pub(crate) fn device() -> (Device, Arc<Clock>) {
    let clock = Arc::new(Clock::default());
    let device = software(
        RAM,
        Arc::clone(&clock) as Arc<dyn MonotonicClock + Send + Sync>,
    );
    (device, clock)
}

pub(crate) fn context(device: &Device) -> Context {
    device
        .create_context(&ContextDesc::default())
        .expect("a context")
}

pub(crate) fn memory(context: &Context, size: u64) -> Allocation {
    context
        .allocate(&AllocationDesc {
            size,
            memory_type: 0,
        })
        .expect("an allocation")
}

pub(crate) fn over(context: &Context, allocation: &Allocation, offset: u64, size: u64) -> Buffer {
    context
        .create_buffer(
            allocation,
            &BufferDesc {
                offset,
                size,
                usage: BufferUsage::ALL,
            },
        )
        .expect("a buffer")
}

pub(crate) fn buffer(context: &Context, size: u64) -> (Allocation, Buffer) {
    let allocation = memory(context, size);
    let buffer = over(context, &allocation, 0, size);
    (allocation, buffer)
}

pub(crate) fn queue(context: &Context) -> Queue {
    context.queue(0, 0).expect("the transfer queue")
}

pub(crate) fn submit(queue: &Queue, commands: &CommandBuffer) -> Result<Fence, SubmitError> {
    queue.submit(&Submission {
        command_buffers: &[commands],
        waits: &[],
        signals: &[],
        deadline: SECOND,
    })
}

pub(crate) fn contents(allocation: &Allocation) -> Vec<u8> {
    let mut out = vec![0u8; usize::try_from(allocation.size()).expect("small")];
    allocation.read(0, &mut out).expect("readable");
    out
}

fn region(src_offset: u64, dst_offset: u64, size: u64) -> CopyRegion {
    CopyRegion {
        src_offset,
        dst_offset,
        size,
    }
}

#[test]
fn copies_fills_and_updates_land_where_they_were_asked_to() {
    let (device, _) = device();
    let context = context(&device);
    let (a, src) = buffer(&context, 32);
    let (b, dst) = buffer(&context, 32);
    let pattern: Vec<u8> = (1..=32).collect();
    a.write(0, &pattern).expect("writable");
    let mut commands = context.command_buffer();
    commands
        .copy_buffer(&src, &dst, &[region(0, 16, 8)])
        .expect("recorded");
    commands
        .fill_buffer(&dst, 0, 16, 0xAABB_CCDD)
        .expect("recorded");
    commands
        .update_buffer(&dst, 24, &[9, 8, 7, 6, 5, 4, 3, 2])
        .expect("recorded");
    let fence = submit(&queue(&context), &commands).expect("admitted");
    fence.wait(Duration64::ZERO).expect("done");
    let mut expected = Vec::new();
    for _ in 0..4 {
        expected.extend_from_slice(&0xAABB_CCDD_u32.to_le_bytes());
    }
    expected.extend_from_slice(&pattern[..8]);
    expected.extend_from_slice(&[9, 8, 7, 6, 5, 4, 3, 2]);
    assert_eq!(contents(&b), expected);
}

#[test]
fn a_handle_of_another_context_is_refused_wherever_it_is_named() {
    let (device, _) = device();
    let mine = context(&device);
    let theirs = context(&device);
    let (their_memory, their_buffer) = buffer(&theirs, 16);
    let (_, my_buffer) = buffer(&mine, 16);
    let mut commands = mine.command_buffer();
    assert_eq!(
        commands.copy_buffer(&their_buffer, &my_buffer, &[region(0, 0, 16)]),
        Err(Error::Foreign)
    );
    assert_eq!(
        commands.fill_buffer(&their_buffer, 0, 16, 0),
        Err(Error::Foreign)
    );
    assert_eq!(
        mine.create_buffer(
            &their_memory,
            &BufferDesc {
                offset: 0,
                size: 16,
                usage: BufferUsage::ALL
            }
        )
        .err(),
        Some(Error::Foreign)
    );
    let their_commands = theirs.command_buffer();
    assert_eq!(
        submit(&queue(&mine), &their_commands).err(),
        Some(SubmitError::from(Error::Foreign))
    );
    let their_timeline = theirs.create_timeline(0).expect("timeline");
    let refused = queue(&mine).submit(&Submission {
        command_buffers: &[],
        waits: &[(&their_timeline, 1)],
        signals: &[],
        deadline: SECOND,
    });
    assert_eq!(refused.err(), Some(SubmitError::from(Error::Foreign)));
}

#[test]
fn a_handle_of_another_device_is_refused_even_where_its_keys_match() {
    let (first, _) = device();
    let (second, _) = device();
    let one = context(&first);
    let other = context(&second);
    let (memory, theirs) = buffer(&other, 16);
    let (_, mine) = buffer(&one, 16);
    let mut commands = one.command_buffer();
    assert_eq!(
        commands.copy_buffer(&theirs, &mine, &[region(0, 0, 16)]),
        Err(Error::Foreign)
    );
    assert_eq!(
        one.import(memory.export().expect("exported")).err(),
        Some(Error::Foreign)
    );
}

#[test]
fn memory_is_reached_from_another_context_only_once_it_is_imported() {
    let (device, _) = device();
    let owner = context(&device);
    let reader = context(&device);
    let shared = memory(&owner, 64);
    shared.write(0, &[7; 64]).expect("writable");
    assert_eq!(
        reader
            .create_buffer(
                &shared,
                &BufferDesc {
                    offset: 0,
                    size: 64,
                    usage: BufferUsage::ALL
                }
            )
            .err(),
        Some(Error::Foreign)
    );
    let imported = reader
        .import(shared.export().expect("exported"))
        .expect("imported");
    let src = over(&reader, &imported, 0, 64);
    let (copy, dst) = buffer(&reader, 64);
    let mut commands = reader.command_buffer();
    commands
        .copy_buffer(&src, &dst, &[region(0, 0, 64)])
        .expect("recorded");
    submit(&queue(&reader), &commands).expect("admitted");
    assert_eq!(contents(&copy), [7; 64]);
    assert_eq!(owner.memory_used().allocations, 64);
    assert_eq!(reader.memory_used().allocations, 64);
    drop(shared);
    assert_eq!(
        owner.memory_used().allocations,
        64,
        "the importer still holds it"
    );
    drop((src, imported));
    assert_eq!(owner.memory_used().allocations, 0);
}

#[test]
fn a_released_context_s_budget_stays_charged_to_the_heap_while_its_memory_is_held() {
    let (device, _) = device();
    let keeper = context(&device);
    let heap = || keeper.shared.model.lock().work_heap_taken();
    let owner = context(&device);
    let shared = memory(&owner, 64);
    let imported = keeper
        .import(shared.export().expect("exported"))
        .expect("imported");
    drop((shared, owner));
    let pinned = heap();
    drop(imported);
    assert_eq!(
        pinned - heap(),
        64 + crate::memory::Memory::overhead(64)
            + crate::model::shared_bytes::<crate::memory::Budget>(),
        "the memory, and the budget it is charged to"
    );
}

#[test]
fn a_submission_with_one_bad_command_is_refused_whole_and_changes_nothing() {
    let (device, _) = device();
    let context = context(&device);
    let (dst_memory, dst) = buffer(&context, 32);
    let (_, src) = buffer(&context, 32);
    let timeline = context.create_timeline(0).expect("timeline");
    let mut commands = context.command_buffer();
    commands
        .fill_buffer(&dst, 0, 32, u32::MAX)
        .expect("recorded");
    commands
        .copy_buffer(&src, &dst, &[region(16, 0, 32)])
        .expect("recorded");
    let refused = queue(&context).submit(&Submission {
        command_buffers: &[&commands],
        waits: &[],
        signals: &[(&timeline, 1)],
        deadline: SECOND,
    });
    assert_eq!(
        refused.err(),
        Some(SubmitError {
            command: Some(1),
            error: Error::OutOfRange
        })
    );
    assert_eq!(contents(&dst_memory), [0; 32]);
    let empty = context.command_buffer();
    let admitted = queue(&context).submit(&Submission {
        command_buffers: &[&empty],
        waits: &[],
        signals: &[(&timeline, 1)],
        deadline: SECOND,
    });
    assert!(admitted.is_ok(), "the refused signal was never queued");
    assert_eq!(timeline.value(), Ok(1));
}

#[test]
fn a_range_past_what_it_names_is_refused_not_clamped() {
    let (device, _) = device();
    let context = context(&device);
    let (_, small) = buffer(&context, 16);
    let (_, large) = buffer(&context, 64);
    let refusal = |record: &dyn Fn(&mut CommandBuffer)| {
        let mut commands = context.command_buffer();
        record(&mut commands);
        submit(&queue(&context), &commands).err().map(|e| e.error)
    };
    assert_eq!(
        refusal(&|c| c
            .copy_buffer(&large, &small, &[region(0, 8, 16)])
            .expect("recorded")),
        Some(Error::OutOfRange)
    );
    assert_eq!(
        refusal(&|c| c
            .copy_buffer(&large, &small, &[region(u64::MAX, 0, 2)])
            .expect("recorded")),
        Some(Error::OutOfRange)
    );
    assert_eq!(
        refusal(&|c| c
            .copy_buffer(&large, &small, &[region(0, 0, 0)])
            .expect("recorded")),
        Some(Error::Empty)
    );
    assert_eq!(
        refusal(&|c| c.copy_buffer(&large, &small, &[]).expect("recorded")),
        Some(Error::Empty)
    );
    assert_eq!(
        refusal(&|c| c.fill_buffer(&small, 0, 20, 0).expect("recorded")),
        Some(Error::OutOfRange)
    );
    assert_eq!(
        refusal(&|c| c.fill_buffer(&small, 2, 4, 0).expect("recorded")),
        Some(Error::Misaligned)
    );
    assert_eq!(
        refusal(&|c| c.update_buffer(&small, 0, &[1, 2, 3]).expect("recorded")),
        Some(Error::Misaligned)
    );
    assert_eq!(
        refusal(&|c| c.update_buffer(&small, 12, &[0; 8]).expect("recorded")),
        Some(Error::OutOfRange)
    );
    let mut commands = context.command_buffer();
    assert_eq!(
        commands.update_buffer(&large, 0, &vec![0; MAX_UPDATE_BYTES + 4]),
        Err(Error::TooMany)
    );
}

#[test]
fn a_buffer_is_used_only_as_it_was_made_for() {
    let (device, _) = device();
    let context = context(&device);
    let allocation = memory(&context, 64);
    let make = |usage| {
        context.create_buffer(
            &allocation,
            &BufferDesc {
                offset: 0,
                size: 32,
                usage,
            },
        )
    };
    let read_only = make(BufferUsage::TRANSFER_SRC).expect("a source");
    let write_only = make(BufferUsage::TRANSFER_DST).expect("a destination");
    assert_eq!(make(BufferUsage::NONE).err(), Some(Error::Empty));
    assert_eq!(
        make(BufferUsage::from_bits(1 << 9)).err(),
        Some(Error::Usage)
    );
    let mut commands = context.command_buffer();
    commands.fill_buffer(&read_only, 0, 4, 0).expect("recorded");
    assert_eq!(
        submit(&queue(&context), &commands).err().map(|e| e.error),
        Some(Error::Usage)
    );
    let mut commands = context.command_buffer();
    commands
        .copy_buffer(&write_only, &read_only, &[region(0, 0, 4)])
        .expect("recorded");
    assert_eq!(
        submit(&queue(&context), &commands).err().map(|e| e.error),
        Some(Error::Usage)
    );
}

#[test]
fn a_buffer_must_fit_its_allocation_at_an_aligned_offset() {
    let (device, _) = device();
    let context = context(&device);
    let allocation = memory(&context, 64);
    let make = |offset, size| {
        context
            .create_buffer(
                &allocation,
                &BufferDesc {
                    offset,
                    size,
                    usage: BufferUsage::ALL,
                },
            )
            .err()
    };
    assert_eq!(make(8, 16), Some(Error::Misaligned));
    assert_eq!(make(48, 32), Some(Error::OutOfRange));
    assert_eq!(make(u64::MAX - 15, 32), Some(Error::OutOfRange));
    assert_eq!(make(0, 0), Some(Error::Empty));
    assert_eq!(make(48, 16), None);
}

#[test]
fn a_copy_of_part_of_a_word_is_refused() {
    let (device, _) = device();
    let context = context(&device);
    let (_, src) = buffer(&context, 64);
    let (_, dst) = buffer(&context, 64);
    let verdict = |regions: &[CopyRegion]| {
        let mut commands = context.command_buffer();
        commands.copy_buffer(&src, &dst, regions).expect("recorded");
        submit(&queue(&context), &commands).err().map(|e| e.error)
    };
    assert_eq!(verdict(&[region(1, 0, 4)]), Some(Error::Misaligned));
    assert_eq!(verdict(&[region(0, 2, 4)]), Some(Error::Misaligned));
    assert_eq!(verdict(&[region(0, 0, 6)]), Some(Error::Misaligned));
    assert_eq!(verdict(&[region(4, 8, 12)]), None);
}

#[test]
fn a_copy_that_writes_a_byte_twice_or_reads_what_it_writes_is_refused() {
    let (device, _) = device();
    let context = context(&device);
    let allocation = memory(&context, 128);
    let low = over(&context, &allocation, 0, 64);
    let high = over(&context, &allocation, 48, 64);
    let (_, other) = buffer(&context, 64);
    let verdict = |src: &Buffer, dst: &Buffer, regions: &[CopyRegion]| {
        let mut commands = context.command_buffer();
        commands.copy_buffer(src, dst, regions).expect("recorded");
        submit(&queue(&context), &commands).err().map(|e| e.error)
    };
    assert_eq!(
        verdict(&low, &low, &[region(0, 8, 16)]),
        Some(Error::Overlap)
    );
    assert_eq!(
        verdict(&low, &high, &[region(48, 0, 8)]),
        Some(Error::Overlap)
    );
    assert_eq!(
        verdict(&other, &low, &[region(0, 0, 16), region(32, 8, 16)]),
        Some(Error::Overlap)
    );
    assert_eq!(verdict(&low, &low, &[region(0, 16, 16)]), None);
    assert_eq!(
        verdict(&other, &low, &[region(0, 0, 16), region(0, 16, 16)]),
        None
    );
}

#[test]
fn memory_past_a_context_s_limit_or_its_heap_is_refused() {
    let (device, _) = device();
    // What a fresh context keeps, and what one allocation adds past its
    // bytes: its memory's own and its table's first room.
    let (base, extra) = {
        let probe = context(&device);
        let base = probe.memory_used().total();
        let _held = memory(&probe, 4096);
        (base, probe.memory_used().total() - base - 4096)
    };
    let small = device
        .create_context(&ContextDesc {
            memory_limit: Some(base + 4096 + extra),
        })
        .expect("a context");
    let held = memory(&small, 4096);
    let past = small.allocate(&AllocationDesc {
        size: 16,
        memory_type: 0,
    });
    assert_eq!(past.err(), Some(Error::OverLimit));
    drop(held);
    assert_eq!(small.memory_used().allocations, 0);
    let again = memory(&small, 4096);
    assert_eq!(
        small.memory_used().allocations,
        4096,
        "the table's room is kept, so the same allocation fits again"
    );
    drop(again);
    drop(small);

    let heap = crate::software::heap_bytes(RAM);
    let first = context(&device);
    let second = context(&device);
    // All of the heap but 64 bytes, past what the device and both contexts
    // already keep.
    let taken = first.shared.model.lock().work_heap_taken();
    let most = memory(&first, heap - taken - extra - 64);
    let refused = second.allocate(&AllocationDesc {
        size: 128,
        memory_type: 0,
    });
    assert_eq!(refused.err(), Some(Error::HeapFull));
    drop(most);
    let fits = memory(&second, 128);
    drop(fits);

    let context = context(&vast_device());
    assert_eq!(
        context
            .allocate(&AllocationDesc {
                size: 0,
                memory_type: 0
            })
            .err(),
        Some(Error::Empty)
    );
    assert_eq!(
        context
            .allocate(&AllocationDesc {
                size: 8,
                memory_type: 1
            })
            .err(),
        Some(Error::NoSuchMemoryType)
    );
    assert_eq!(
        context
            .allocate(&AllocationDesc {
                size: (1 << 61) + 1,
                memory_type: 0
            })
            .err(),
        Some(Error::TooLarge)
    );
}

/// All of the device's heap but a few bytes, fewer than any request takes,
/// held by an allocation of a context of its own.
pub(crate) fn fill_heap(device: &Device) -> (Context, Allocation) {
    // What a context's first allocation adds past its bytes.
    let extra = {
        let probe = context(device);
        let base = probe.memory_used().total();
        let _held = memory(&probe, 4096);
        probe.memory_used().total() - base - 4096
    };
    let hog = context(device);
    let taken = hog.shared.model.lock().work_heap_taken();
    let most = memory(
        &hog,
        (crate::software::heap_bytes(RAM) - taken - extra - 8) / 4 * 4,
    );
    (hog, most)
}

/// A context whose limit leaves one byte short of what `request` charges
/// once `setup` is made, as a fresh context's own probe of both measured.
pub(crate) fn short_of<S, R>(
    device: &Device,
    setup: impl Fn(&Context) -> S,
    request: impl Fn(&Context, &S) -> R,
) -> (Context, S) {
    let (made, asked) = {
        let probe = context(device);
        let held = setup(&probe);
        let made = probe.memory_used().total();
        let _made = request(&probe, &held);
        (made, probe.memory_used().total() - made)
    };
    let short = device
        .create_context(&ContextDesc {
            memory_limit: Some(made + asked - 1),
        })
        .expect("a context");
    let held = setup(&short);
    (short, held)
}

#[test]
fn a_request_past_both_its_context_s_limit_and_the_heap_is_refused_for_the_limit() {
    let threads = Unstarted::default();
    let device = unstarted(&threads);
    let small = device
        .create_context(&ContextDesc {
            memory_limit: Some(64 << 10),
        })
        .expect("a context");
    let (timelines, ()) = short_of(
        &device,
        |_| (),
        |context, ()| context.create_timeline(0).expect("a timeline"),
    );
    let (submits, target) = short_of(
        &device,
        |context| buffer(context, 16),
        |context, (_, target)| {
            let mut commands = context.command_buffer();
            commands.fill_buffer(target, 0, 16, 7).expect("recorded");
            // Queued, as no worker runs it, so what it holds stays charged.
            submit(&queue(context), &commands).expect("queued")
        },
    );
    let _full = fill_heap(&device);
    assert_eq!(
        small
            .allocate(&AllocationDesc {
                size: MEBIBYTE,
                memory_type: 0,
            })
            .err(),
        Some(Error::OverLimit),
        "an allocation, though its table's room is past the heap first"
    );
    assert_eq!(
        timelines.create_timeline(0).err(),
        Some(Error::OverLimit),
        "an object, though its own bytes alone are within the limit"
    );
    let mut commands = submits.command_buffer();
    commands.fill_buffer(&target.1, 0, 16, 7).expect("recorded");
    assert_eq!(
        submit(&queue(&submits), &commands)
            .err()
            .map(|refused| refused.error),
        Some(Error::OverLimit),
        "a submission, though its job's bytes alone are within the limit"
    );
    assert_eq!(
        device
            .create_context(&ContextDesc {
                memory_limit: Some(64),
            })
            .err(),
        Some(Error::OverLimit),
        "a context, though its budget is past the heap first"
    );
}

/// A device whose heap is far larger than the host can supply.
fn vast_device() -> Device {
    let clock: Arc<dyn MonotonicClock + Send + Sync> = Arc::new(Clock::default());
    software(1 << 62, clock)
}

#[test]
fn memory_the_machine_cannot_supply_is_refused_after_the_attempt() {
    let context = context(&vast_device());
    let unsuppliable = context.allocate(&AllocationDesc {
        size: 1 << 60,
        memory_type: 0,
    });
    assert_eq!(unsuppliable.err(), Some(Error::OutOfMemory));
    assert_eq!(
        context.memory_used().allocations,
        0,
        "the charge went back with the attempt"
    );
}

#[test]
fn freed_memory_stays_with_the_work_that_holds_it_until_that_work_is_done() {
    let (device, _) = device();
    let context = context(&device);
    let (a, src) = buffer(&context, 16);
    let (b, dst) = buffer(&context, 16);
    a.write(0, &[5; 16]).expect("writable");
    let gate = context.create_timeline(0).expect("timeline");
    let mut commands = context.command_buffer();
    commands
        .copy_buffer(&src, &dst, &[region(0, 0, 16)])
        .expect("recorded");
    let fence = queue(&context)
        .submit(&Submission {
            command_buffers: &[&commands],
            waits: &[(&gate, 1)],
            signals: &[],
            deadline: SECOND,
        })
        .expect("admitted");
    drop((commands, src, a));
    assert_eq!(
        context.memory_used().allocations,
        32,
        "the queued copy holds its source"
    );
    assert_eq!(fence.is_signalled(), Ok(false));
    gate.signal(1).expect("advances");
    assert_eq!(fence.is_signalled(), Ok(true));
    assert_eq!(contents(&b), [5; 16]);
    assert_eq!(context.memory_used().allocations, 16);
    drop((dst, b));
    assert_eq!(context.memory_used().allocations, 0);
}

#[test]
fn memory_whose_last_holder_is_a_buffer_goes_without_the_devices_lock() {
    let (device, _) = device();
    let context = context(&device);
    let before = crate::fair::held::drops_under();
    let (allocation, buffer) = buffer(&context, 64);
    drop(allocation);
    assert_eq!(context.memory_used().allocations, 64, "the buffer holds it");
    drop(buffer);
    assert_eq!(context.memory_used().allocations, 0);
    assert_eq!(crate::fair::held::drops_under(), before);
}

#[test]
fn memory_an_import_refused_for_its_tables_room_held_last_goes_without_the_devices_lock() {
    let (device, _) = device();
    let owner = context(&device);
    let allocation = memory(&owner, 64);
    let exported = allocation.export().expect("exported");
    drop(allocation);
    assert_eq!(owner.memory_used().allocations, 64, "the export holds it");
    // A context whose table is full and has no room to grow.
    let probe = context(&device);
    let kept: Vec<Allocation> = (0..4).map(|_| memory(&probe, 4)).collect();
    let full = probe.memory_used().total();
    drop((kept, probe));
    let tight = device
        .create_context(&ContextDesc {
            memory_limit: Some(full),
        })
        .expect("a context");
    let _held: Vec<Allocation> = (0..4).map(|_| memory(&tight, 4)).collect();
    let before = crate::fair::held::drops_under();
    assert_eq!(tight.import(exported).err(), Some(Error::OverLimit));
    assert_eq!(owner.memory_used().allocations, 0, "the refusal let it go");
    assert_eq!(crate::fair::held::drops_under(), before);
}

#[test]
fn memory_whose_last_holder_is_a_job_goes_without_the_devices_lock() {
    let (device, _) = device();
    let context = context(&device);
    let (a, src) = buffer(&context, 16);
    let (_b, dst) = buffer(&context, 16);
    let gate = context.create_timeline(0).expect("timeline");
    let mut commands = context.command_buffer();
    commands
        .copy_buffer(&src, &dst, &[region(0, 0, 16)])
        .expect("recorded");
    let fence = queue(&context)
        .submit(&Submission {
            command_buffers: &[&commands],
            waits: &[(&gate, 1)],
            signals: &[],
            deadline: SECOND,
        })
        .expect("admitted");
    drop((commands, src, a));
    let before = crate::fair::held::drops_under();
    gate.signal(1).expect("advances");
    assert_eq!(fence.is_signalled(), Ok(true));
    assert_eq!(context.memory_used().allocations, 16, "the job let it go");
    assert_eq!(crate::fair::held::drops_under(), before);
}

#[test]
fn memory_a_lost_contexts_queued_work_last_held_goes_without_the_devices_lock() {
    let (device, clock) = device();
    let context = context(&device);
    let (_slow_memory, slow) = buffer(&context, MEBIBYTE);
    let (a, src) = buffer(&context, 16);
    let (_b, dst) = buffer(&context, 16);
    let gate = context.create_timeline(0).expect("timeline");
    let mut overrunning = context.command_buffer();
    overrunning
        .fill_buffer(&slow, 0, MEBIBYTE, 1)
        .expect("recorded");
    // Work after a check's worth, so the check stops it.
    overrunning.fill_buffer(&slow, 0, 64, 1).expect("recorded");
    let _overrun = queue(&context)
        .submit(&Submission {
            command_buffers: &[&overrunning],
            waits: &[(&gate, 1)],
            signals: &[],
            deadline: SECOND,
        })
        .expect("admitted");
    let mut queued = context.command_buffer();
    queued
        .copy_buffer(&src, &dst, &[region(0, 0, 16)])
        .expect("recorded");
    let _never = submit(&queue(&context), &queued).expect("admitted, behind it");
    drop((queued, src, a));
    let before = crate::fair::held::drops_under();
    clock.step.store(2_000_000_000, Ordering::SeqCst);
    gate.signal(1).expect("advances");
    clock.step.store(0, Ordering::SeqCst);
    assert_eq!(context.status(), Err(Error::Lost(Loss::Overran)));
    assert_eq!(
        context.memory_used().allocations,
        MEBIBYTE + 16,
        "the discarded copy let its source go"
    );
    assert_eq!(crate::fair::held::drops_under(), before);
}

#[test]
fn a_queue_runs_its_work_in_the_order_submitted() {
    let (device, _) = device();
    let context = context(&device);
    let (_, src) = buffer(&context, 16);
    let (b, dst) = buffer(&context, 16);
    let gate = context.create_timeline(0).expect("timeline");
    let mut first = context.command_buffer();
    first
        .fill_buffer(&src, 0, 16, 0x0101_0101)
        .expect("recorded");
    first
        .copy_buffer(&src, &dst, &[region(0, 0, 16)])
        .expect("recorded");
    let mut second = context.command_buffer();
    second
        .fill_buffer(&dst, 0, 8, 0x0202_0202)
        .expect("recorded");
    let queue = queue(&context);
    let early = queue
        .submit(&Submission {
            command_buffers: &[&first],
            waits: &[(&gate, 1)],
            signals: &[],
            deadline: SECOND,
        })
        .expect("admitted");
    let late = submit(&queue, &second).expect("admitted");
    assert_eq!(late.is_signalled(), Ok(false), "it waits its turn");
    assert_eq!(contents(&b), [0; 16]);
    gate.signal(1).expect("advances");
    assert_eq!(early.is_signalled(), Ok(true));
    assert_eq!(late.is_signalled(), Ok(true));
    assert_eq!(contents(&b), [[2; 8], [1; 8]].concat());
}

#[test]
fn a_command_buffer_can_be_submitted_again() {
    let (device, _) = device();
    let context = context(&device);
    let (a, src) = buffer(&context, 16);
    let (b, dst) = buffer(&context, 16);
    let mut commands = context.command_buffer();
    commands
        .copy_buffer(&src, &dst, &[region(0, 0, 16)])
        .expect("recorded");
    let queue = queue(&context);
    a.write(0, &[3; 16]).expect("writable");
    submit(&queue, &commands).expect("admitted");
    assert_eq!(contents(&b), [3; 16]);
    a.write(0, &[4; 16]).expect("writable");
    submit(&queue, &commands).expect("admitted");
    assert_eq!(contents(&b), [4; 16]);
    commands.clear();
    assert!(commands.is_empty());
}

#[test]
fn a_signal_that_would_not_advance_its_timeline_is_refused() {
    let (device, _) = device();
    let context = context(&device);
    let timeline = context.create_timeline(5).expect("timeline");
    let gate = context.create_timeline(0).expect("timeline");
    assert_eq!(timeline.signal(5), Err(Error::TimelineOrder));
    assert_eq!(timeline.signal(4), Err(Error::TimelineOrder));
    let queue = queue(&context);
    let empty = context.command_buffer();
    let with = |signals: &[(&crate::Timeline, u64)]| {
        queue
            .submit(&Submission {
                command_buffers: &[&empty],
                waits: &[(&gate, 1)],
                signals,
                deadline: SECOND,
            })
            .err()
            .map(|e| e.error)
    };
    assert_eq!(with(&[(&timeline, 5)]), Some(Error::TimelineOrder));
    assert_eq!(
        with(&[(&timeline, 9), (&timeline, 10)]),
        Some(Error::TimelineOrder)
    );
    assert_eq!(with(&[(&timeline, 9)]), None);
    assert_eq!(
        with(&[(&timeline, 8)]),
        Some(Error::TimelineOrder),
        "below a queued signal"
    );
    assert_eq!(
        timeline.signal(9),
        Err(Error::TimelineOrder),
        "not short of the queued signal"
    );
    timeline.signal(7).expect("short of the queued signal");
    gate.signal(1).expect("advances");
    assert_eq!(timeline.value(), Ok(9));
}

#[test]
fn a_deadline_of_zero_or_past_the_device_s_longest_is_refused() {
    let (device, _) = device();
    let context = context(&device);
    let empty = context.command_buffer();
    let queue = queue(&context);
    for deadline in [
        Duration64::ZERO,
        Duration64::from_secs(-1),
        Duration64::from_secs(61),
    ] {
        let refused = queue.submit(&Submission {
            command_buffers: &[&empty],
            waits: &[],
            signals: &[],
            deadline,
        });
        assert_eq!(refused.err(), Some(SubmitError::from(Error::Deadline)));
    }
    assert_eq!(context.queue(0, 1).err(), Some(Error::NoSuchQueue));
    assert_eq!(context.queue(1, 0).err(), Some(Error::NoSuchQueue));
}

#[test]
fn an_overrunning_submission_loses_its_own_context_and_no_other() {
    let (device, clock) = device();
    let runaway = context(&device);
    let bystander = context(&device);
    let (runaway_memory, slow) = buffer(&runaway, MEBIBYTE);
    let (_, also) = buffer(&runaway, 64);
    let (b, steady) = buffer(&bystander, 16);
    let mut commands = runaway.command_buffer();
    commands
        .fill_buffer(&slow, 0, MEBIBYTE, 1)
        .expect("recorded");
    commands.fill_buffer(&also, 0, 64, 1).expect("recorded");
    clock.step.store(2_000_000_000, Ordering::SeqCst);
    let fence = submit(&queue(&runaway), &commands).expect("admitted");
    clock.step.store(0, Ordering::SeqCst);
    assert_eq!(runaway.status(), Err(Error::Lost(Loss::Overran)));
    assert_eq!(fence.wait(SECOND), Err(Error::Lost(Loss::Overran)));
    assert_eq!(
        submit(&queue(&runaway), &commands).err(),
        Some(SubmitError::from(Error::Lost(Loss::Overran)))
    );
    assert_eq!(
        runaway_memory.write(0, &[0]),
        Err(Error::Lost(Loss::Overran))
    );
    let mut steady_work = bystander.command_buffer();
    steady_work
        .fill_buffer(&steady, 0, 16, 0x0909_0909)
        .expect("recorded");
    submit(&queue(&bystander), &steady_work).expect("admitted");
    assert_eq!(bystander.status(), Ok(()));
    assert_eq!(contents(&b), [9; 16]);
    assert_eq!(runaway.memory_used().allocations, MEBIBYTE + 64);
    drop((slow, also, runaway_memory, commands));
    assert_eq!(
        runaway.memory_used().allocations,
        0,
        "the lost work let its memory go"
    );
    drop(fence);
}

#[test]
fn work_of_small_commands_reads_the_clock_only_to_start() {
    let (device, clock) = device();
    let context = context(&device);
    let (_, target) = buffer(&context, 4096);
    let mut commands = context.command_buffer();
    for word in 0..1000 {
        commands
            .fill_buffer(&target, (word % 1024) * 4, 4, 7)
            .expect("recorded");
    }
    clock.readings.store(0, Ordering::SeqCst);
    submit(&queue(&context), &commands).expect("admitted");
    assert_eq!(
        clock.readings.load(Ordering::SeqCst),
        1,
        "one reading to start the work, and none to find nothing more ready"
    );
}

#[test]
fn a_wait_on_another_thread_returns_once_the_work_it_waits_for_completes() {
    let clock = Arc::new(HostClock(std::time::Instant::now()));
    let device = software(RAM, clock);
    let context = context(&device);
    let (b, dst) = buffer(&context, 16);
    let gate = context.create_timeline(0).expect("timeline");
    let mut commands = context.command_buffer();
    commands
        .fill_buffer(&dst, 0, 16, 0x0606_0606)
        .expect("recorded");
    let fence = queue(&context)
        .submit(&Submission {
            command_buffers: &[&commands],
            waits: &[(&gate, 1)],
            signals: &[],
            deadline: SECOND,
        })
        .expect("admitted");
    let waiting = fence.clone();
    let waiter = std::thread::spawn(move || waiting.wait(Duration64::from_secs(30)));
    gate.signal(1).expect("advances");
    assert_eq!(waiter.join().expect("the waiter ran"), Ok(()));
    assert_eq!(contents(&b), [6; 16]);
    assert_eq!(
        context
            .create_timeline(0)
            .expect("timeline")
            .wait(1, Duration64::from_millis(5)),
        Err(Error::TimedOut)
    );
}

#[test]
fn releasing_a_context_wakes_every_thread_waiting_on_it() {
    let clock = Arc::new(HostClock(std::time::Instant::now()));
    let device = software(RAM, clock);
    let context = context(&device);
    let timeline = Arc::new(context.create_timeline(0).expect("timeline"));
    let waiting = Arc::clone(&timeline);
    let waiter = std::thread::spawn(move || waiting.wait(1, Duration64::from_secs(30)));
    drop(context);
    assert_eq!(
        waiter.join().expect("the waiter ran"),
        Err(Error::Destroyed)
    );
    assert_eq!(timeline.value(), Err(Error::Destroyed));
}

#[test]
fn the_cpu_reaches_only_what_an_allocation_holds() {
    let (device, _) = device();
    let context = context(&device);
    let allocation = memory(&context, 16);
    assert_eq!(allocation.write(12, &[0; 8]), Err(Error::OutOfRange));
    assert_eq!(allocation.write(u64::MAX, &[0; 2]), Err(Error::OutOfRange));
    let mut out = [0u8; 4];
    assert_eq!(allocation.read(14, &mut out), Err(Error::OutOfRange));
    allocation.write(12, &[1, 2, 3, 4]).expect("writable");
    allocation.read(12, &mut out).expect("readable");
    assert_eq!(out, [1, 2, 3, 4]);
}

#[test]
fn a_submission_with_no_commands_still_waits_and_signals_in_order() {
    let (device, _) = device();
    let context = context(&device);
    let gate = context.create_timeline(0).expect("timeline");
    let done = context.create_timeline(0).expect("timeline");
    let fence = queue(&context)
        .submit(&Submission {
            command_buffers: &[],
            waits: &[(&gate, 2)],
            signals: &[(&done, 1)],
            deadline: SECOND,
        })
        .expect("admitted");
    gate.signal(1).expect("advances");
    assert_eq!(done.value(), Ok(0));
    gate.signal(2).expect("advances");
    assert_eq!(done.value(), Ok(1));
    fence.wait(Duration64::ZERO).expect("done");
}

/// Host threads, kept so a test can join them.
#[derive(Default)]
struct HostThreads {
    started: std::sync::Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl Spawn for HostThreads {
    fn spawn(&self, body: alloc::boxed::Box<dyn FnOnce() + Send>) -> bool {
        let handle = std::thread::spawn(body);
        self.started.lock().expect("unpoisoned").push(handle);
        true
    }
}

fn threaded(threads: &HostThreads) -> Device {
    Device::software(&SoftwareDesc {
        memory_bytes: RAM,
        clock: Arc::new(HostClock(std::time::Instant::now())),
        runner: Arc::new(Serial),
        threads,
        subgroup_size: DEFAULT_SUBGROUP_SIZE,
    })
    .expect("a software device")
}

#[test]
fn a_device_with_a_worker_runs_its_work_there_and_completes_it() {
    let threads = HostThreads::default();
    let device = threaded(&threads);
    let context = context(&device);
    let (b, dst) = buffer(&context, 16);
    let gate = context.create_timeline(0).expect("timeline");
    let mut commands = context.command_buffer();
    commands
        .fill_buffer(&dst, 0, 16, 0x0909_0909)
        .expect("recorded");
    let fence = queue(&context)
        .submit(&Submission {
            command_buffers: &[&commands],
            waits: &[(&gate, 1)],
            signals: &[],
            deadline: SECOND,
        })
        .expect("admitted");
    gate.signal(1).expect("advances");
    fence.wait(Duration64::from_secs(30)).expect("completed");
    assert_eq!(contents(&b), [9; 16]);
    for round in 0u32..32 {
        let mut commands = context.command_buffer();
        commands.fill_buffer(&dst, 0, 16, round).expect("recorded");
        submit(&queue(&context), &commands)
            .expect("admitted")
            .wait(Duration64::from_secs(30))
            .expect("completed");
        assert_eq!(contents(&b)[..4], round.to_le_bytes());
    }
}

#[test]
fn a_devices_worker_ends_when_the_device_is_released() {
    let threads = HostThreads::default();
    let device = threaded(&threads);
    let context = context(&device);
    let (_, dst) = buffer(&context, 16);
    let mut commands = context.command_buffer();
    commands.fill_buffer(&dst, 0, 16, 1).expect("recorded");
    submit(&queue(&context), &commands)
        .expect("admitted")
        .wait(Duration64::from_secs(30))
        .expect("completed");
    drop((commands, dst, context, device));
    let started = core::mem::take(&mut *threads.started.lock().expect("unpoisoned"));
    assert_eq!(started.len(), 1, "one worker");
    for worker in started {
        worker.join().expect("the worker returned");
    }
}

#[test]
fn an_allocation_is_charged_for_what_it_keeps_besides_its_bytes() {
    let (device, _) = device();
    let context = context(&device);
    // The first makes the table's room; the second fits in it.
    let _first = memory(&context, 1);
    let before = context.memory_used();
    let second = memory(&context, 1);
    let after = context.memory_used();
    assert_eq!(after.allocations - before.allocations, 1);
    assert_eq!(
        after.objects - before.objects,
        crate::memory::Memory::overhead(1),
        "its words rounded up and the memory itself"
    );
    drop(second);
    assert_eq!(context.memory_used(), before);
}

#[test]
fn a_table_s_room_stays_charged_after_its_objects_go() {
    let (device, _) = device();
    let context = context(&device);
    let fresh = context.memory_used();
    let held: Vec<_> = (0..1000).map(|_| memory(&context, 4)).collect();
    drop(held);
    let kept = context.memory_used();
    assert_eq!(kept.allocations, 0);
    assert!(
        kept.objects >= fresh.objects + 1000 * 24,
        "a thousand slots kept, and charged: {kept:?}"
    );
}

#[test]
fn an_update_submitted_many_times_is_charged_for_its_bytes_once() {
    let (device, _) = device();
    let context = context(&device);
    let (_memory, target) = buffer(&context, 4096);
    let gate = context.create_timeline(0).expect("a gate");
    let mut commands = context.command_buffer();
    let before = context.memory_used().objects;
    commands
        .update_buffer(&target, 0, &[7; 4096])
        .expect("recorded");
    let recorded = context.memory_used().objects - before;
    assert!(
        recorded >= 4096,
        "the bytes are charged as they are recorded"
    );
    let queue = queue(&context);
    let mut fences = Vec::new();
    for _ in 0..64 {
        fences.push(
            queue
                .submit(&crate::Submission {
                    command_buffers: &[&commands],
                    waits: &[(&gate, 1)],
                    signals: &[],
                    deadline: SECOND,
                })
                .expect("admitted"),
        );
    }
    let queued = context.memory_used().objects - before;
    assert!(
        queued < recorded + 64 * 4096 / 8,
        "sixty-four submissions share the bytes: {queued} past {recorded}"
    );
    drop(commands);
    gate.signal(1).expect("opened");
    for fence in fences {
        fence.wait(SECOND).expect("ran");
    }
}

#[test]
fn a_fill_of_many_checks_work_reads_the_clock_as_it_goes() {
    let clock = Arc::new(Clock::default());
    let device = software(
        64 << 20,
        Arc::clone(&clock) as Arc<dyn MonotonicClock + Send + Sync>,
    );
    let context = context(&device);
    let bytes = 8 * crate::software::WORK_PER_CHECK;
    let (_memory, target) = buffer(&context, bytes);
    let mut commands = context.command_buffer();
    commands
        .fill_buffer(&target, 0, bytes, 7)
        .expect("recorded");
    // The clock reads 0.6 seconds a check's work in, and 1.2 seconds at the
    // next: past the deadline with six checks' work still to do.
    clock.step.store(600_000_000, Ordering::SeqCst);
    let ran = submit(&queue(&context), &commands);
    clock.step.store(0, Ordering::SeqCst);
    assert!(ran.is_ok(), "admitted");
    assert_eq!(
        context.status(),
        Err(Error::Lost(Loss::Overran)),
        "stopped within the fill, not after it"
    );
}
