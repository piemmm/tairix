//! The software device: every queue family's work run on the CPU, on the
//! thread whose act made it ready.

use alloc::sync::Arc;
use core::ops::Range;

use tairix_abi::time::MonotonicClock;
use tairix_abi::Duration64;
use tairix_inline::ArrayVec;

use crate::command::Op;
use crate::info::{
    DeviceInfo, Limits, MemoryHeap, MemoryProperties, MemoryType, QueueCapabilities, QueueFamily,
};
use crate::memory::Memory;
use crate::model::Model;
use crate::{Device, Error};

/// What a buffer's offset must be a multiple of: the widest vector the CPU's
/// copies move.
const BUFFER_ALIGNMENT: u64 = 16;

/// The longest a submission runs once started: past any frame or compute
/// pass a CPU runs, and the bound on how long a runaway holds the engine.
const MAX_JOB_SECS: i64 = 60;

/// How many bytes work moves between two readings of the clock for its
/// deadline: a reading costs a call into the kernel, and a mebibyte's copy
/// costs a hundred times that.
const BYTES_PER_DEADLINE_CHECK: u64 = 1 << 20;

/// How many bytes the software device's one heap offers: half the machine's
/// memory, leaving the rest to the system and to what its clients keep
/// outside the device.
pub(crate) const fn heap_bytes(total_ram_bytes: u64) -> u64 {
    total_ram_bytes / 2
}

impl Device {
    /// The software device, its heap sized from the machine's discovered
    /// memory, its deadlines read from `clock`.
    ///
    /// One memory type, the CPU's own memory, is every kind at once; one
    /// queue family runs transfers, one job at a time.
    ///
    /// # Errors
    ///
    /// [`Error::Malformed`] when `total_ram_bytes` leaves no heap.
    pub fn software(
        total_ram_bytes: u64,
        clock: Arc<dyn MonotonicClock + Send + Sync>,
    ) -> Result<Self, Error> {
        let heap = heap_bytes(total_ram_bytes);
        let mut info = DeviceInfo {
            heaps: ArrayVec::new(),
            memory_types: ArrayVec::new(),
            queue_families: ArrayVec::new(),
            limits: Limits {
                max_allocation_size: heap,
                buffer_alignment: BUFFER_ALIGNMENT,
                max_job_time: Duration64::from_secs(MAX_JOB_SECS),
            },
        };
        let filed = info
            .heaps
            .try_push(MemoryHeap {
                size: heap,
                device_local: true,
            })
            .is_ok()
            && info
                .memory_types
                .try_push(MemoryType {
                    heap: 0,
                    properties: MemoryProperties::DEVICE_LOCAL
                        .with(MemoryProperties::HOST_VISIBLE)
                        .with(MemoryProperties::HOST_COHERENT)
                        .with(MemoryProperties::HOST_CACHED),
                })
                .is_ok()
            && info
                .queue_families
                .try_push(QueueFamily {
                    capabilities: QueueCapabilities::TRANSFER,
                    queues: 1,
                })
                .is_ok();
        if !filed {
            return Err(Error::Malformed);
        }
        Ok(Self::from_model(Model::new(info, &[1])?, clock))
    }
}

/// How a run of a job ended.
pub(crate) enum Outcome {
    Done,
    /// Stopped between two commands, past its deadline.
    Overran,
    /// A command reached past its memory, which its checks proved it cannot.
    Fault,
}

/// Run `ops` in order, stopping between two of them once `deadline` has
/// passed: the clock is read between commands once a mebibyte has moved since
/// it was last read.
pub(crate) fn run(ops: &[Op], deadline: u64, clock: &dyn MonotonicClock) -> Outcome {
    let mut unchecked = 0u64;
    for op in ops {
        if unchecked >= BYTES_PER_DEADLINE_CHECK {
            if clock.now_ns() > deadline {
                return Outcome::Overran;
            }
            unchecked = 0;
        }
        if apply(op).is_none() {
            return Outcome::Fault;
        }
        unchecked = unchecked.saturating_add(op.bytes());
    }
    Outcome::Done
}

fn span(offset: u64, len: u64) -> Option<Range<usize>> {
    let start = usize::try_from(offset).ok()?;
    let end = start.checked_add(usize::try_from(len).ok()?)?;
    Some(start..end)
}

fn apply(op: &Op) -> Option<()> {
    match op {
        Op::Copy { src, dst, spans } if Arc::ptr_eq(src, dst) => {
            let mut bytes = src.lock();
            for copy in spans {
                let from = span(copy.src, copy.len)?;
                let to = span(copy.dst, copy.len)?;
                if from.end > bytes.len() || to.end > bytes.len() {
                    return None;
                }
                bytes.copy_within(from, to.start);
            }
        }
        Op::Copy { src, dst, spans } => {
            let (from, mut to) = Memory::lock_pair(src, dst);
            for copy in spans {
                let source = from.get(span(copy.src, copy.len)?)?;
                to.get_mut(span(copy.dst, copy.len)?)?
                    .copy_from_slice(source);
            }
        }
        Op::Fill {
            dst,
            offset,
            len,
            word,
        } => {
            let mut bytes = dst.lock();
            let (words, rest) = bytes.get_mut(span(*offset, *len)?)?.as_chunks_mut::<4>();
            if !rest.is_empty() {
                return None;
            }
            words.fill(word.to_le_bytes());
        }
        Op::Update { dst, offset, data } => {
            let mut bytes = dst.lock();
            bytes
                .get_mut(span(*offset, data.len() as u64)?)?
                .copy_from_slice(data.as_slice());
        }
    }
    Some(())
}
