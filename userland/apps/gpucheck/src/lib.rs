//! `gpucheck`: the same work run on a GPU and on the software device, and
//! where their answers differ (`plans/GPU.md` GP11.2).
//!
//! The work is the transfers every device runs: words updated into
//! host-visible memory, a fill beside them, both copied through memory the
//! device keeps for itself and back, and a command each device must refuse.
//! What each device leaves in memory, and how it refused, is the answer: two
//! devices that agree leave the same bytes and refuse the same command for
//! the same reason.

#![no_std]
#![deny(missing_docs)]

extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::time::MonotonicClock;
use tairix_abi::Duration64;
use tairix_gpu::{
    AllocationDesc, Buffer, BufferDesc, BufferUsage, Context, ContextDesc, CopyRegion, Device,
    Error, MemoryProperties, NoThreads, SoftwareDesc, Submission, SubmitError,
    DEFAULT_SUBGROUP_SIZE,
};

/// The usage line the short help shows.
pub const USAGE: &str = "usage: gpucheck [-h | -?]";

/// The bytes of each memory the work runs through.
const BYTES: u64 = 64 << 10;

/// The longest the work takes on any device.
const DEADLINE: Duration64 = Duration64::from_secs(10);

/// The memory the reference is made for: the half its heap offers holds the
/// work's memory with room to spare.
const REFERENCE_MEMORY: u64 = 4 << 20;

/// What a device answered the work: the bytes it left, and how it refused
/// the command it must refuse.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Answer {
    /// The host-visible memory, read back once the work ran.
    pub bytes: Vec<u8>,
    /// The refusal of a fill past its buffer.
    pub refused: Option<SubmitError>,
}

/// Where a device's answer differs from the reference's.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Difference {
    /// The device refused the work, where the reference ran it.
    Refused(Error),
    /// A byte of memory differs: its offset, the device's and the reference's.
    Byte {
        /// Its offset into the host-visible memory.
        at: usize,
        /// What the device left there.
        device: u8,
        /// What the reference left there.
        reference: u8,
    },
    /// The command both must refuse was refused differently, or not at all.
    Refusal {
        /// The device's refusal.
        device: Option<SubmitError>,
        /// The reference's.
        reference: Option<SubmitError>,
    },
}

impl fmt::Display for Difference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(why) => write!(f, "refused the work: {why}"),
            Self::Byte {
                at,
                device,
                reference,
            } => write!(
                f,
                "byte {at} reads {device:#04x} where the software device left {reference:#04x}"
            ),
            Self::Refusal { device, reference } => {
                f.write_str("a fill past its buffer was ")?;
                write_answer(f, device.as_ref())?;
                f.write_str(" where the software device's was ")?;
                write_answer(f, reference.as_ref())
            }
        }
    }
}

/// How a submission was answered: run, or refused and why.
fn write_answer(f: &mut fmt::Formatter<'_>, refused: Option<&SubmitError>) -> fmt::Result {
    match refused {
        None => f.write_str("run"),
        Some(refused) => write!(f, "refused ({refused})"),
    }
}

/// The software device the work is held to, reading its deadlines from
/// `clock`.
///
/// # Errors
///
/// What the software device refused.
pub fn reference(clock: Arc<dyn MonotonicClock + Send + Sync>) -> Result<Device, Error> {
    Device::software(&SoftwareDesc {
        memory_bytes: REFERENCE_MEMORY,
        clock,
        runner: Arc::new(tairix_parallel::Serial),
        threads: &NoThreads,
        subgroup_size: DEFAULT_SUBGROUP_SIZE,
    })
}

/// The first memory type of `device` holding all of `wanted`.
fn memory_type(device: &Device, wanted: MemoryProperties) -> Option<u32> {
    device
        .info()
        .memory_types
        .iter()
        .position(|kind| kind.properties.contains(wanted))
        .and_then(|index| u32::try_from(index).ok())
}

/// A buffer over memory of its own of type `memory_type`.
fn buffer(context: &Context, memory_type: u32) -> Result<(tairix_gpu::Allocation, Buffer), Error> {
    let memory = context.allocate(&AllocationDesc {
        size: BYTES,
        memory_type,
    })?;
    let usage = BufferUsage::TRANSFER_SRC.with(BufferUsage::TRANSFER_DST);
    let buffer = context.create_buffer(
        &memory,
        &BufferDesc {
            offset: 0,
            size: BYTES,
            usage,
        },
    )?;
    Ok((memory, buffer))
}

/// The words the work updates into memory: a pattern no two runs of zeros
/// or ones could pass for.
fn pattern(len: usize) -> Vec<u8> {
    (0..len)
        .map(|at| u8::try_from((at * 151 + 7) % 251).unwrap_or(0))
        .collect()
}

/// Run the work on `device`.
///
/// # Errors
///
/// What the device refused that every device must run.
pub fn work(device: &Device) -> Result<Answer, Error> {
    let visible = MemoryProperties::HOST_VISIBLE.with(MemoryProperties::HOST_COHERENT);
    let near = memory_type(device, visible).ok_or(Error::NoSuchMemoryType)?;
    // Memory the device keeps for itself where it has some, else its own.
    let far = device
        .info()
        .memory_types
        .iter()
        .position(|kind| !kind.properties.contains(MemoryProperties::HOST_VISIBLE))
        .and_then(|index| u32::try_from(index).ok())
        .unwrap_or(near);
    let context = device.create_context(&ContextDesc::default())?;
    let (memory, staging) = buffer(&context, near)?;
    let (_kept, deep) = buffer(&context, far)?;
    let queue = context.queue(0, 0)?;
    let run = |commands: &tairix_gpu::CommandBuffer| {
        queue.submit(&Submission {
            command_buffers: &[commands],
            waits: &[],
            signals: &[],
            deadline: DEADLINE,
        })
    };
    let mut commands = context.command_buffer();
    commands.update_buffer(&staging, 0, &pattern(4096))?;
    commands.fill_buffer(&staging, 4096, 4096, 0x5a5a_a5a5)?;
    commands.copy_buffer(
        &staging,
        &deep,
        &[CopyRegion {
            src_offset: 0,
            dst_offset: 400,
            size: 8192,
        }],
    )?;
    commands.copy_buffer(
        &deep,
        &staging,
        &[CopyRegion {
            src_offset: 0,
            dst_offset: 16 << 10,
            size: 16 << 10,
        }],
    )?;
    run(&commands)
        .map_err(|refused| refused.error)?
        .wait(DEADLINE)?;
    let mut past = context.command_buffer();
    past.fill_buffer(&staging, 0, BYTES + 4, 0)?;
    let refused = run(&past).err();
    let mut bytes = vec![0; usize::try_from(BYTES).map_err(|_| Error::TooLarge)?];
    memory.read(0, &mut bytes)?;
    Ok(Answer { bytes, refused })
}

/// Where `device`'s answer to the work differs from `expected`, the
/// reference's, the first difference found.
///
/// # Errors
///
/// [`Difference`] where they differ.
pub fn compare(device: &Device, expected: &Answer) -> Result<(), Difference> {
    let answered = work(device).map_err(Difference::Refused)?;
    if let Some((at, (&device, &reference))) = answered
        .bytes
        .iter()
        .zip(&expected.bytes)
        .enumerate()
        .find(|(_, (device, reference))| device != reference)
    {
        return Err(Difference::Byte {
            at,
            device,
            reference,
        });
    }
    if answered.refused != expected.refused {
        return Err(Difference::Refusal {
            device: answered.refused,
            reference: expected.refused,
        });
    }
    Ok(())
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
