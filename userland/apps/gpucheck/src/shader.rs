//! `gpucheck --shader`: a kernel a program supplies at run time, run on each
//! served GPU and on the software device, and where their answers differ
//! (`plans/GPU.md` GP8).
//!
//! The kernel's one compute entry point runs over one storage buffer at set
//! 0, binding 0, whose word `i` holds `i`, one invocation for each word in
//! workgroups of the kernel's own size. Beside it, in a context of its own on
//! the same device, a neighbour's memory holds a marker the kernel's buffer
//! must never come to hold. A device answers the words the kernel left, the
//! loss its context met, or its refusal; one that lost the context must then
//! still run a fill in another.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_gpu::{
    BindSetEntry, Buffer, BufferUsage, Context, ContextDesc, Device, Error, Loss,
    QueueCapabilities, Resource,
};
use tairix_spirv::{
    Admitted, BindingKind, DecodeError, Module, Refusal as Refused, SpecError, Stage, MAGIC,
};

use crate::{at, buffer, bytes_of, family, near, pipeline, submit, Refusal};

/// The words of the buffer a kernel runs over unless asked otherwise.
pub const DEFAULT_WORDS: u32 = 256;

/// The most words a kernel's buffer may be asked to hold: 256 KiB, which with
/// its neighbour's leaves the reference room for the largest pipeline.
pub const MAX_WORDS: u32 = 1 << 16;

/// The longest file read as a kernel: a module's largest, which is past a
/// WGSL source's.
pub const MAX_FILE_BYTES: usize = tairix_spirv::MAX_MODULE_BYTES;

const _: () = assert!(tairix_wgsl::MAX_SOURCE_BYTES <= MAX_FILE_BYTES);

/// The steps a refusal names.
const NEIGHBOUR: &str = "the neighbour's memory";
const KERNEL_BUFFER: &str = "the kernel's buffer";

/// The word a fill after a loss writes, which a device that served on reads
/// back.
const SERVED: u32 = 0x5e4e_ed00;

/// What a kernel file holds, told by its first word.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source<'a> {
    /// A SPIR-V module, beginning with the magic word in either byte order.
    Spirv(&'a [u8]),
    /// WGSL source: any other text.
    Wgsl(&'a str),
}

impl<'a> Source<'a> {
    /// What `bytes` holds; `None` for neither a module nor text.
    #[must_use]
    pub fn of(bytes: &'a [u8]) -> Option<Self> {
        let first = bytes
            .first_chunk::<4>()
            .map(|word| u32::from_le_bytes(*word));
        if first.is_some_and(|word| word == MAGIC || word == MAGIC.swap_bytes()) {
            return Some(Self::Spirv(bytes));
        }
        core::str::from_utf8(bytes).ok().map(Self::Wgsl)
    }
}

/// Why a module is no kernel `gpucheck` runs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KernelError {
    /// The decoder refused the module.
    Decoded(DecodeError),
    /// The validator refused it.
    Refused(Refused),
    /// It has no compute entry point, or more than one: how many.
    EntryPoints(usize),
    /// Its compute entry point binds other than one storage buffer at set
    /// 0, binding 0, or takes push constants.
    Interface,
    /// Its specialization constants, which its workgroup size is read from,
    /// cannot be evaluated at their defaults.
    Workgroup(SpecError),
    /// There was no memory for its entry point's name.
    OutOfMemory,
}

impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decoded(refused) => write!(f, "not a SPIR-V module the decoder reads: {refused}"),
            Self::Refused(refused) => write!(f, "the validator refuses the module at {refused}"),
            Self::EntryPoints(0) => f.write_str("the module has no compute entry point"),
            Self::EntryPoints(count) => {
                write!(f, "the module has {count} compute entry points, not one")
            }
            Self::Interface => {
                f.write_str("the kernel binds other than one storage buffer at set 0, binding 0")
            }
            Self::Workgroup(_) => {
                f.write_str("the kernel's specialization constants cannot be evaluated")
            }
            Self::OutOfMemory => f.write_str("there was no memory for the kernel"),
        }
    }
}

/// A kernel admitted to run: its module, its compute entry point, and how
/// many invocations a workgroup of it holds.
#[derive(Debug)]
pub struct Kernel {
    module: Admitted,
    entry: String,
    invocations: u32,
}

impl Kernel {
    /// The SPIR-V module `bytes`, decoded and admitted here.
    ///
    /// # Errors
    ///
    /// As [`Self::new`], and [`KernelError::Decoded`] or
    /// [`KernelError::Refused`] for a module the decoder or the validator
    /// refuses.
    pub fn decode(bytes: &[u8]) -> Result<Self, KernelError> {
        let module = Module::decode(bytes).map_err(KernelError::Decoded)?;
        Self::new(Admitted::new(module).map_err(KernelError::Refused)?)
    }

    /// `module`, held to the interface a kernel runs through.
    ///
    /// # Errors
    ///
    /// [`KernelError::EntryPoints`] for other than one compute entry point,
    /// [`KernelError::Interface`] for one binding other than one storage
    /// buffer at set 0, binding 0, or taking push constants, and
    /// [`KernelError::Workgroup`] for a workgroup size that cannot be read.
    pub fn new(module: Admitted) -> Result<Self, KernelError> {
        let is_compute = |entry: &&tairix_spirv::EntryPoint| entry.stage() == Stage::Compute;
        let computes = module.entry_points().iter().filter(is_compute).count();
        let Some((index, entry)) = module
            .entry_points()
            .iter()
            .enumerate()
            .find(|(_, entry)| is_compute(entry))
            .filter(|_| computes == 1)
        else {
            return Err(KernelError::EntryPoints(computes));
        };
        let one_buffer = matches!(
            entry.bindings(),
            [binding] if binding.set == 0
                && binding.binding == 0
                && binding.count == 1
                && binding.kind == BindingKind::StorageBuffer
        );
        if !one_buffer || entry.push_constants().is_some() {
            return Err(KernelError::Interface);
        }
        let mut name = String::new();
        name.try_reserve_exact(entry.name().len())
            .map_err(|_| KernelError::OutOfMemory)?;
        name.push_str(entry.name());
        let sizes = module.specialize(&[]).map_err(KernelError::Workgroup)?;
        let invocations = sizes
            .workgroup_size(index)
            .map_or(1, |[x, y, z]| x.saturating_mul(y).saturating_mul(z))
            .max(1);
        Ok(Self {
            module,
            entry: name,
            invocations,
        })
    }

    /// Its module.
    #[must_use]
    pub const fn module(&self) -> &Admitted {
        &self.module
    }

    /// The workgroups a buffer of `words` takes: one invocation a word.
    #[must_use]
    pub const fn groups(&self, words: u32) -> u32 {
        let groups = words.div_ceil(self.invocations);
        if groups == 0 {
            1
        } else {
            groups
        }
    }
}

/// What a device answered a kernel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// It ran: the words it left.
    Ran(Vec<u32>),
    /// Its context was lost.
    Lost {
        /// How.
        loss: Loss,
        /// Whether the device then ran a fill in a context of its own.
        serves: bool,
    },
    /// The device refused the kernel's pipeline or its dispatch.
    Refused(Error),
    /// The device answered neither its end nor its context's loss in the
    /// time the CPU waits on it.
    Unanswered,
}

impl Outcome {
    /// What it was, its words aside.
    #[must_use]
    pub const fn answered(&self) -> Answered {
        match self {
            Self::Ran(_) => Answered::Ran,
            Self::Lost { loss, .. } => Answered::Lost(*loss),
            Self::Refused(why) => Answered::Refused(*why),
            Self::Unanswered => Answered::Unanswered,
        }
    }
}

/// What an outcome was, its words aside.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Answered {
    /// The kernel ran.
    Ran,
    /// Its context was lost.
    Lost(Loss),
    /// It was refused.
    Refused(Error),
    /// It was never answered.
    Unanswered,
}

impl fmt::Display for Answered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ran => f.write_str("ran"),
            Self::Lost(loss) => write!(f, "lost: {}", lost(*loss)),
            Self::Refused(why) => write!(f, "refused: {why}"),
            Self::Unanswered => f.write_str("was never answered"),
        }
    }
}

/// How a kernel's context was lost.
#[must_use]
pub const fn lost(loss: Loss) -> &'static str {
    match loss {
        Loss::Overran => "it ran past its deadline",
        Loss::Reset => "a reset took its work",
        Loss::Fault => "the device found it malformed",
        Loss::OutOfMemory => "the host refused memory it needed",
    }
}

/// Run `kernel` over a buffer of `words` on `device`, word `i` starting as
/// `i`, the memory of a neighbouring context filled with `marker` beside it
/// throughout.
///
/// # Errors
///
/// The step the device refused before the kernel could be made: its
/// buffer's memory or its neighbour's.
pub fn run_kernel(
    device: &Device,
    kernel: &Kernel,
    (words, marker): (u32, u32),
) -> Result<Outcome, Refusal> {
    let near = near(device).map_err(at(KERNEL_BUFFER))?;
    let neighbour = device
        .create_context(&ContextDesc::default())
        .map_err(at(NEIGHBOUR))?;
    let (filled, _filled_buffer) = buffer(
        &neighbour,
        (near, u64::from(words) * 4),
        BufferUsage::STORAGE,
    )
    .map_err(at(NEIGHBOUR))?;
    filled
        .write(0, &words_of(words, |_| marker).map_err(at(NEIGHBOUR))?)
        .map_err(at(NEIGHBOUR))?;
    let start = words_of(words, |at| at).map_err(at(KERNEL_BUFFER))?;
    run_over(device, kernel, (&start, kernel.groups(words)))
}

/// Run `groups` workgroups of `kernel` on `device` over a buffer starting as
/// `start`, whose words are its bytes.
///
/// # Errors
///
/// The step the device refused before the kernel could be made: its
/// buffer's memory.
pub(crate) fn run_over(
    device: &Device,
    kernel: &Kernel,
    (start, groups): (&[u8], u32),
) -> Result<Outcome, Refusal> {
    let near = near(device).map_err(at(KERNEL_BUFFER))?;
    let words = u32::try_from(start.len() / 4)
        .map_err(|_| Error::TooLarge)
        .map_err(at(KERNEL_BUFFER))?;
    let size = u64::from(words) * 4;
    let context = device
        .create_context(&ContextDesc::default())
        .map_err(at(KERNEL_BUFFER))?;
    let (memory, held) =
        buffer(&context, (near, size), BufferUsage::STORAGE).map_err(at(KERNEL_BUFFER))?;
    memory.write(0, start).map_err(at(KERNEL_BUFFER))?;
    Ok(
        match dispatch(device, &context, kernel, (&held, words, groups)) {
            Ok(()) => Outcome::Ran(read_words(&memory, words).map_err(at(KERNEL_BUFFER))?),
            Err(Error::Lost(loss)) => Outcome::Lost {
                loss,
                serves: serves_on(device),
            },
            Err(Error::TimedOut) => Outcome::Unanswered,
            Err(why) => Outcome::Refused(why),
        },
    )
}

/// The `count` words `memory` holds from its start.
pub(crate) fn read_words(memory: &tairix_gpu::Allocation, count: u32) -> Result<Vec<u32>, Error> {
    let mut bytes = words_of(count, |_| 0)?;
    memory.read(0, &mut bytes)?;
    let (words, _) = bytes.as_chunks::<4>();
    let mut left = Vec::new();
    left.try_reserve_exact(words.len())
        .map_err(|_| Error::OutOfMemory)?;
    left.extend(words.iter().map(|word| u32::from_le_bytes(*word)));
    Ok(left)
}

/// The bytes of `count` words, word `at` holding `word(at)`.
fn words_of(count: u32, word: impl Fn(u32) -> u32) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(usize::try_from(count).map_err(|_| Error::TooLarge)? * 4)
        .map_err(|_| Error::OutOfMemory)?;
    bytes.extend((0..count).flat_map(|at| word(at).to_le_bytes()));
    Ok(bytes)
}

/// `groups` workgroups of `kernel` run in `context` over `buffer`, `words`
/// long, on the first queue stating compute work.
fn dispatch(
    device: &Device,
    context: &Context,
    kernel: &Kernel,
    (buffer, words, groups): (&Buffer, u32, u32),
) -> Result<(), Error> {
    let compute = family(device, QueueCapabilities::COMPUTE)?;
    let (pipeline, layout, _) =
        pipeline(context, (kernel.module.module().bytes(), &kernel.entry), 0)?;
    let set = context.create_bind_set(
        &layout,
        &[BindSetEntry {
            binding: 0,
            resources: &[Resource::Buffer {
                buffer,
                offset: 0,
                size: u64::from(words) * 4,
            }],
        }],
    )?;
    let mut commands = context.command_buffer();
    commands.dispatch(&pipeline, &[&set], &[], [groups, 1, 1])?;
    submit(context, compute, &commands)
}

/// Whether `device` runs a fill, and its reading, in a context of its own.
fn serves_on(device: &Device) -> bool {
    let fill = || -> Result<bool, Error> {
        let near = near(device)?;
        let transfer = family(device, QueueCapabilities::TRANSFER)?;
        let context = device.create_context(&ContextDesc::default())?;
        let (memory, filled) = buffer(&context, (near, 16), BufferUsage::TRANSFER_DST)?;
        let mut commands = context.command_buffer();
        commands.fill_buffer(&filled, 0, 16, SERVED)?;
        submit(&context, transfer, &commands)?;
        let mut bytes = [0; 16];
        memory.read(0, &mut bytes)?;
        Ok(bytes == *bytes_of(&[SERVED; 4]).as_slice())
    };
    fill().unwrap_or(false)
}

/// Where a device's answer to a kernel differs from the software device's.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KernelDifference {
    /// The device lost the kernel's context and then ran no fill in another.
    Stopped,
    /// A word holds the marker its neighbour's memory was filled with, where
    /// the software device's does not.
    Neighbour {
        /// Its place among the buffer's words.
        at: usize,
    },
    /// A word differs.
    Word {
        /// Its place among the buffer's words.
        at: usize,
        /// What the device left there.
        device: u32,
        /// What the software device left there.
        reference: u32,
    },
    /// The device answered otherwise than the software device.
    Answered {
        /// The device's answer.
        device: Answered,
        /// The software device's.
        reference: Answered,
    },
}

impl fmt::Display for KernelDifference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => f.write_str("lost the kernel's context and then served nothing"),
            Self::Neighbour { at } => write!(
                f,
                "word {at} holds the marker its neighbour's memory was filled with"
            ),
            Self::Word {
                at,
                device,
                reference,
            } => write!(
                f,
                "word {at} reads {device:#010x} where the software device left {reference:#010x}"
            ),
            Self::Answered { device, reference } => write!(
                f,
                "the kernel {device} where on the software device it {reference}"
            ),
        }
    }
}

/// Where `device`, a device's answer to a kernel, differs from `reference`,
/// the software device's, their neighbours' memory having held `marker`.
///
/// # Errors
///
/// [`KernelDifference`] where they differ.
pub fn compare_kernel(
    device: &Outcome,
    reference: &Outcome,
    marker: u32,
) -> Result<(), KernelDifference> {
    match (device, reference) {
        (Outcome::Lost { serves: false, .. }, _) => Err(KernelDifference::Stopped),
        (Outcome::Ran(left), Outcome::Ran(held)) => {
            let mut words = left.iter().zip(held).enumerate();
            if let Some((at, _)) = words
                .clone()
                .find(|&(_, (&left, &held))| left == marker && held != marker)
            {
                return Err(KernelDifference::Neighbour { at });
            }
            match words.find(|(_, (left, held))| left != held) {
                Some((at, (&device, &reference))) => Err(KernelDifference::Word {
                    at,
                    device,
                    reference,
                }),
                None => Ok(()),
            }
        }
        (Outcome::Lost { loss, .. }, Outcome::Lost { loss: held, .. }) if loss == held => Ok(()),
        _ => Err(KernelDifference::Answered {
            device: device.answered(),
            reference: reference.answered(),
        }),
    }
}

/// The first word of `words` holding `marker`.
#[must_use]
pub fn marked_at(words: &[u32], marker: u32) -> Option<usize> {
    words.iter().position(|&word| word == marker)
}

#[cfg(test)]
#[path = "shader_tests.rs"]
mod tests;
