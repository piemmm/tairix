//! Deterministic fuzz harness for the submission model, run on the software
//! device against a plain byte-array reference.
//!
//! A submission's commands carry offsets and sizes a client chooses, so each
//! round records copies, fills and updates over buffers that share
//! allocations, with ranges drawn to land just inside, across and well past
//! what they name, and usages both granted and not. The invariants are:
//!
//! 1. Nothing panics, whatever is asked.
//! 2. A submission the reference refuses is refused, naming the same command
//!    and the same reason, and changes no byte anywhere.
//! 3. A submission the reference admits is admitted, completes, and leaves
//!    every allocation byte for byte as the reference does.
//! 4. Every round admits a fill within its buffer and refuses one past it, so
//!    neither verdict goes unexercised however few submissions a sweep runs.
//!
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from the
//! same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use std::sync::Arc;
use std::vec::Vec;

use tairix_abi::time::MonotonicClock;
use tairix_abi::Duration64;
use tairix_fuzzseed::Prng;
use tairix_gpu::{
    AllocationDesc, BufferDesc, BufferUsage, CommandBuffer, ContextDesc, CopyRegion, Device, Error,
    Submission, SubmitError, MAX_UPDATE_BYTES,
};

/// Rounds per sweep. Miri interprets every operation, so it drives a handful.
const ROUNDS_PER_SWEEP: u32 = if cfg!(miri) { 2 } else { 300 };

/// Submissions per round.
const SUBMISSIONS_PER_ROUND: u32 = if cfg!(miri) { 4 } else { 40 };

const ALIGNMENT: u64 = 16;

struct Still;

impl MonotonicClock for Still {
    fn now_ns(&self) -> u64 {
        0
    }
}

/// An offset the reference has proved within one of its memories.
fn index_of(value: u64) -> usize {
    usize::try_from(value).expect("within a small memory")
}

/// What the reference knows of a buffer.
#[derive(Copy, Clone)]
struct Shape {
    memory: usize,
    offset: u64,
    size: u64,
    usage: BufferUsage,
}

enum Recorded {
    Copy {
        src: usize,
        dst: usize,
        regions: Vec<CopyRegion>,
    },
    Fill {
        dst: usize,
        offset: u64,
        size: u64,
        word: u32,
    },
    Update {
        dst: usize,
        offset: u64,
        data: Vec<u8>,
    },
}

/// An offset or size drawn to land inside, at the edge of, or past `limit`.
fn draw_extent(rng: &mut Prng, limit: u64) -> u64 {
    match rng.below(8) {
        0 => 0,
        1 => limit,
        2 => limit + 4,
        3 => u64::MAX - rng.below(8) as u64,
        _ => rng.below(usize::try_from(limit).unwrap_or(0) + 1) as u64,
    }
}

fn draw_word_extent(rng: &mut Prng, limit: u64) -> u64 {
    let raw = draw_extent(rng, limit);
    if rng.below(4) == 0 {
        raw
    } else {
        raw & !3
    }
}

/// Where `len` bytes at `offset` into `shape` lie in its memory: the device's
/// own rule, written out again.
fn within(shape: &Shape, offset: u64, len: u64) -> Result<u64, Error> {
    if len == 0 {
        return Err(Error::Empty);
    }
    match offset.checked_add(len) {
        Some(end) if end <= shape.size => Ok(shape.offset + offset),
        _ => Err(Error::OutOfRange),
    }
}

fn usable(shape: &Shape, usage: BufferUsage) -> Result<(), Error> {
    if shape.usage.contains(usage) {
        Ok(())
    } else {
        Err(Error::Usage)
    }
}

/// The ranges `regions` read and write, in memory offsets, once each is
/// proved within its buffer.
fn spans(src: &Shape, dst: &Shape, regions: &[CopyRegion]) -> Result<Vec<(u64, u64, u64)>, Error> {
    usable(src, BufferUsage::TRANSFER_SRC)?;
    usable(dst, BufferUsage::TRANSFER_DST)?;
    if regions.is_empty() {
        return Err(Error::Empty);
    }
    let mut spans = Vec::new();
    for region in regions {
        let from = within(src, region.src_offset, region.size)?;
        let to = within(dst, region.dst_offset, region.size)?;
        spans.push((from, to, region.size));
    }
    let meets = |a: u64, a_len: u64, b: u64, b_len: u64| a < b + b_len && b < a + a_len;
    let one_memory = src.memory == dst.memory;
    for (i, &(_, at, len)) in spans.iter().enumerate() {
        let written_twice = spans[i + 1..]
            .iter()
            .any(|&(_, other, other_len)| meets(at, len, other, other_len));
        let reads_written = one_memory
            && spans
                .iter()
                .any(|&(read, _, read_len)| meets(at, len, read, read_len));
        if written_twice || reads_written {
            return Err(Error::Overlap);
        }
    }
    Ok(spans)
}

/// Whether the reference admits `commands`, and if it does, apply them to
/// `memories`.
fn reference(
    memories: &mut [Vec<u8>],
    shapes: &[Shape],
    commands: &[Recorded],
) -> Result<(), SubmitError> {
    let refused = |command: usize| {
        move |error: Error| SubmitError {
            command: Some(command),
            error,
        }
    };
    let mut planned: Vec<(usize, u64, Vec<u8>)> = Vec::new();
    let mut staged = memories.to_vec();
    for (index, command) in commands.iter().enumerate() {
        match command {
            Recorded::Copy { src, dst, regions } => {
                let (src, dst) = (shapes[*src], shapes[*dst]);
                for (from, to, len) in spans(&src, &dst, regions).map_err(refused(index))? {
                    let (from, len) = (index_of(from), index_of(len));
                    let bytes = staged[src.memory][from..from + len].to_vec();
                    planned.push((dst.memory, to, bytes));
                }
            }
            Recorded::Fill {
                dst,
                offset,
                size,
                word,
            } => {
                let dst = shapes[*dst];
                usable(&dst, BufferUsage::TRANSFER_DST).map_err(refused(index))?;
                let at = within(&dst, *offset, *size).map_err(refused(index))?;
                if offset % 4 != 0 || size % 4 != 0 {
                    return Err(refused(index)(Error::Misaligned));
                }
                let bytes: Vec<u8> = word
                    .to_le_bytes()
                    .iter()
                    .copied()
                    .cycle()
                    .take(index_of(*size))
                    .collect();
                planned.push((dst.memory, at, bytes));
            }
            Recorded::Update { dst, offset, data } => {
                let dst = shapes[*dst];
                usable(&dst, BufferUsage::TRANSFER_DST).map_err(refused(index))?;
                let at = within(&dst, *offset, data.len() as u64).map_err(refused(index))?;
                if offset % 4 != 0 || data.len() % 4 != 0 {
                    return Err(refused(index)(Error::Misaligned));
                }
                planned.push((dst.memory, at, data.clone()));
            }
        }
        for (memory, at, bytes) in planned.drain(..) {
            let at = index_of(at);
            staged[memory][at..at + bytes.len()].copy_from_slice(&bytes);
        }
    }
    memories.clone_from_slice(&staged);
    Ok(())
}

fn record(commands: &mut CommandBuffer, buffers: &[tairix_gpu::Buffer], recorded: &Recorded) {
    let outcome = match recorded {
        Recorded::Copy { src, dst, regions } => {
            commands.copy_buffer(&buffers[*src], &buffers[*dst], regions)
        }
        Recorded::Fill {
            dst,
            offset,
            size,
            word,
        } => commands.fill_buffer(&buffers[*dst], *offset, *size, *word),
        Recorded::Update { dst, offset, data } => {
            commands.update_buffer(&buffers[*dst], *offset, data)
        }
    };
    assert_eq!(outcome, Ok(()), "every drawn command records");
}

fn draw_command(rng: &mut Prng, shapes: &[Shape]) -> Recorded {
    let dst = rng.below(shapes.len());
    let limit = shapes[dst].size;
    match rng.below(3) {
        0 => {
            let src = rng.below(shapes.len());
            let count = 1 + rng.below(4);
            let regions = (0..count)
                .map(|_| CopyRegion {
                    src_offset: draw_extent(rng, shapes[src].size),
                    dst_offset: draw_extent(rng, limit),
                    size: draw_extent(rng, limit.min(shapes[src].size) / 2),
                })
                .collect();
            Recorded::Copy { src, dst, regions }
        }
        1 => Recorded::Fill {
            dst,
            offset: draw_word_extent(rng, limit),
            size: draw_word_extent(rng, limit),
            word: rng.next_u32(),
        },
        _ => {
            let len = usize::try_from(draw_word_extent(rng, limit))
                .unwrap_or(0)
                .min(MAX_UPDATE_BYTES);
            let mut data = vec![0u8; len];
            rng.fill(&mut data);
            Recorded::Update {
                dst,
                offset: draw_word_extent(rng, limit),
                data,
            }
        }
    }
}

/// One round's device and the reference beside it.
struct Round {
    context: tairix_gpu::Context,
    queue: tairix_gpu::Queue,
    allocations: Vec<tairix_gpu::Allocation>,
    buffers: Vec<tairix_gpu::Buffer>,
    shapes: Vec<Shape>,
    memories: Vec<Vec<u8>>,
}

impl Round {
    fn new(rng: &mut Prng) -> Self {
        let clock: Arc<dyn MonotonicClock + Send + Sync> = Arc::new(Still);
        let device = Device::software(64 << 20, clock).expect("a software device");
        let context = device
            .create_context(&ContextDesc::default())
            .expect("a context");
        let queue = context.queue(0, 0).expect("the transfer queue");
        let allocations: Vec<tairix_gpu::Allocation> = (0..=rng.below(3))
            .map(|_| {
                let size = ALIGNMENT * (1 + rng.below(16) as u64);
                context
                    .allocate(&AllocationDesc {
                        size,
                        memory_type: 0,
                    })
                    .expect("memory")
            })
            .collect();
        let memories = allocations
            .iter()
            .map(|allocation| vec![0u8; index_of(allocation.size())])
            .collect();
        let mut round = Self {
            context,
            queue,
            allocations,
            buffers: Vec::new(),
            shapes: Vec::new(),
            memories,
        };
        let whole = round.allocations[0].size();
        round.add_buffer(0, 0, whole, BufferUsage::ALL);
        let usages = [
            BufferUsage::ALL,
            BufferUsage::ALL,
            BufferUsage::TRANSFER_SRC,
            BufferUsage::TRANSFER_DST,
        ];
        for _ in 0..=rng.below(4) {
            let memory = rng.below(round.allocations.len());
            let whole = round.allocations[memory].size();
            let offset = ALIGNMENT * rng.below(index_of(whole / ALIGNMENT)) as u64;
            let size = 1 + rng.below(index_of(whole - offset)) as u64;
            round.add_buffer(memory, offset, size, *rng.pick(&usages));
        }
        round
    }

    fn add_buffer(&mut self, memory: usize, offset: u64, size: u64, usage: BufferUsage) {
        let buffer = self
            .context
            .create_buffer(
                &self.allocations[memory],
                &BufferDesc {
                    offset,
                    size,
                    usage,
                },
            )
            .expect("a buffer that fits");
        self.buffers.push(buffer);
        self.shapes.push(Shape {
            memory,
            offset,
            size,
            usage,
        });
    }

    /// Write random bytes into one memory from the CPU, on both sides.
    fn scribble(&mut self, rng: &mut Prng) {
        let memory = rng.below(self.allocations.len());
        let at = rng.below(self.memories[memory].len());
        let mut bytes = vec![0u8; rng.at_most(self.memories[memory].len() - at)];
        rng.fill(&mut bytes);
        self.allocations[memory]
            .write(at as u64, &bytes)
            .expect("an in-range write");
        self.memories[memory][at..at + bytes.len()].copy_from_slice(&bytes);
    }

    /// Submit `drawn` to the device and the reference, require them to agree on
    /// the verdict and on every byte, and answer the verdict.
    fn exchange(&mut self, drawn: &[Recorded]) -> Result<(), SubmitError> {
        let mut commands = self.context.command_buffer();
        for recorded in drawn {
            record(&mut commands, &self.buffers, recorded);
        }
        let expected = reference(&mut self.memories, &self.shapes, drawn);
        let outcome = self.queue.submit(&Submission {
            command_buffers: &[&commands],
            waits: &[],
            signals: &[],
            deadline: Duration64::from_secs(1),
        });
        match (outcome, expected) {
            (Ok(fence), Ok(())) => fence.wait(Duration64::ZERO).expect("completed"),
            (Err(got), Err(want)) => assert_eq!(got, want),
            (got, want) => {
                panic!("the device answered {got:?} where the reference answered {want:?}")
            }
        }
        for (allocation, reference) in self.allocations.iter().zip(&self.memories) {
            let mut seen = vec![0u8; reference.len()];
            allocation.read(0, &mut seen).expect("readable");
            assert_eq!(&seen, reference);
        }
        expected
    }
}

fn run_round(rng: &mut Prng) {
    let mut round = Round::new(rng);
    let whole = round.shapes[0].size;
    let within = round.exchange(&[Recorded::Fill {
        dst: 0,
        offset: 0,
        size: ALIGNMENT,
        word: rng.next_u32(),
    }]);
    assert_eq!(within, Ok(()), "a fill within its buffer is admitted");
    let past = round.exchange(&[Recorded::Fill {
        dst: 0,
        offset: 0,
        size: whole + 4,
        word: 0,
    }]);
    assert_eq!(
        past,
        Err(SubmitError {
            command: Some(0),
            error: Error::OutOfRange
        }),
        "a fill past its buffer is refused"
    );
    for _ in 0..SUBMISSIONS_PER_ROUND {
        if rng.below(3) == 0 {
            round.scribble(rng);
        }
        let drawn: Vec<Recorded> = (0..=rng.below(6))
            .map(|_| draw_command(rng, &round.shapes))
            .collect();
        let _ = round.exchange(&drawn);
    }
}

#[test]
fn submissions_agree_with_a_byte_array_reference() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "submissions_agree_with_a_byte_array_reference",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..ROUNDS_PER_SWEEP {
            run_round(&mut rng);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
