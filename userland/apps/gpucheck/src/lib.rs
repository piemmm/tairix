//! `gpucheck`: the same work run on a GPU and on the software device, and
//! where their answers differ (`plans/GPU.md` GP11.2, GP5.4, GP5.5, GP5.6),
//! and a kernel a program supplies run the same way (`shader`, GP8).
//!
//! The work is, first, the transfers every device runs: words updated into
//! host-visible memory, a fill beside them, both copied through memory the
//! device keeps for itself and back, and a command each device must refuse,
//! all run on every queue family stating transfers, a copy engine among them.
//! Then every pass of the pinned set run on its sample; then, on a device
//! stating the uses it makes of its formats, the image work (`images`) and,
//! on one that draws, the render work (`render`); and last an indirect
//! dispatch whose counts are past the device's own, which must lose its
//! context; and last the differential corpus (`corpus`), every instruction
//! the validator admits run and held to the software device's answer. Every
//! other step runs on the first queue family running each command it
//! records: one that draws for transfers reaching depth or stencil, one that
//! computes or draws for a colour clear. What each
//! device leaves in memory, and how it refused, is the answer: two devices
//! that agree leave the same bytes and words, refuse the same command for
//! the same reason, and lose the same context.

#![no_std]
#![deny(missing_docs)]

extern crate alloc;

#[cfg(test)]
extern crate std;

// The unit tests refuse the host's heap to see each refusal answered.
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: tairix_fuzzseed::meter::Metered = tairix_fuzzseed::meter::Metered;

mod corpus;
mod images;
mod render;
mod scene;
mod shader;

pub use corpus::{Held, Output, Ran};
pub use scene::{memory_for, rendered, Scene};
pub use shader::{
    compare_kernel, lost, marked_at, run_kernel, Answered, Kernel, KernelDifference, KernelError,
    Outcome, Source, DEFAULT_WORDS, MAX_FILE_BYTES, MAX_WORDS,
};

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::time::MonotonicClock;
use tairix_abi::Duration64;
use tairix_gpu::{
    Allocation, AllocationDesc, BindSetEntry, BindSetLayout, Buffer, BufferDesc, BufferUsage,
    CommandBuffer, ComputePipeline, ComputePipelineDesc, Context, ContextDesc, CopyRegion,
    DescriptorType, Device, Error, Format, FormatFeatures, LayoutBinding, Limits, Loss,
    MemoryProperties, NoThreads, PushConstants, QueueCapabilities, QueueFamily, Resource,
    SoftwareDesc, Submission, SubmitError, DEFAULT_SUBGROUP_SIZE,
};
use tairix_shaders::compose::fill;
use tairix_shaders::{Pinned, Refused, SET};
use tairix_spirv::{Admitted, Module, Stage, Stages};
use tairix_util::getopt::{Arg, Opt, Order, Scan, Takes};
use tairix_util::quote::quote;

/// The usage line the short help shows.
pub const USAGE: &str = "usage: gpucheck [--present | --shader FILE [--words N]] [-h | -?]";

/// What a `gpucheck` command line asks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command<'a> {
    /// Check every served GPU against the software device.
    Check,
    /// Show the present scene full screen through a swapchain, and check
    /// every frame presented was shown.
    Present,
    /// Run the kernel `file` holds over a buffer of `words` on every served
    /// GPU and on the software device.
    Shader {
        /// The file holding the kernel: WGSL source or a SPIR-V module.
        file: &'a str,
        /// The words of the buffer it runs over.
        words: u32,
    },
    /// Show the command's own short help.
    Help,
}

/// Why a `gpucheck` command line was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandError {
    /// An option the shared `getopt` refuses, worded as it words it.
    Option(tairix_util::getopt::Refusal),
    /// An operand; the command takes none.
    ExtraOperand(String),
    /// A word count that is not a whole number from one to [`MAX_WORDS`].
    Words(String),
    /// A word count without a kernel to run over them.
    WordsWithoutShader,
    /// Both `--present` and `--shader`.
    PresentAndShader,
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::ExtraOperand(operand) => {
                write!(f, "extra operand {operand}", operand = quote(operand))
            }
            Self::Words(count) => write!(
                f,
                "invalid word count {count}: a whole number from 1 to {MAX_WORDS}",
                count = quote(count)
            ),
            Self::WordsWithoutShader => f.write_str("--words is given only with --shader"),
            Self::PresentAndShader => f.write_str("--present and --shader exclude each other"),
        }
    }
}

/// What an option of the command line asks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Asked {
    Help,
    Present,
    Shader,
    Words,
}

/// The options `gpucheck` takes: what each asks.
const OPTIONS: &[Opt<Asked>] = &[
    Opt::flag('h', &["help"], Asked::Help),
    Opt::letter('?', Takes::Nothing, Asked::Help),
    Opt::long(&["present"], Takes::Nothing, Asked::Present),
    Opt::long(&["shader"], Takes::Value, Asked::Shader),
    Opt::long(&["words"], Takes::Value, Asked::Words),
];

/// Read `args`, the command line after the program's name: a help switch
/// anywhere asks for help, and a repeated option's last value stands.
///
/// # Errors
///
/// [`CommandError::Option`] for an option `getopt` refuses,
/// [`CommandError::ExtraOperand`] for an operand, [`CommandError::Words`]
/// for a word count out of range, and [`CommandError::WordsWithoutShader`]
/// or [`CommandError::PresentAndShader`] for options that do not go
/// together.
pub fn command<'a>(args: &'a [&'a str]) -> Result<Command<'a>, CommandError> {
    let (mut present, mut file, mut words, mut operand) = (false, None, None, None);
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(CommandError::Option)? {
            Arg::Opt(Asked::Help, _) => return Ok(Command::Help),
            Arg::Opt(Asked::Present, _) => present = true,
            Arg::Opt(Asked::Shader, value) => file = value,
            Arg::Opt(Asked::Words, value) => words = value,
            Arg::Operand(found) => {
                operand.get_or_insert(found);
            }
        }
    }
    if let Some(operand) = operand {
        return Err(CommandError::ExtraOperand(String::from(operand)));
    }
    match (present, file, words) {
        (true, Some(_), _) => Err(CommandError::PresentAndShader),
        (_, None, Some(_)) => Err(CommandError::WordsWithoutShader),
        (true, None, None) => Ok(Command::Present),
        (false, None, None) => Ok(Command::Check),
        (false, Some(file), words) => {
            let words = match words {
                None => DEFAULT_WORDS,
                Some(count) => count
                    .parse::<u32>()
                    .ok()
                    .filter(|&count| (1..=MAX_WORDS).contains(&count))
                    .ok_or_else(|| CommandError::Words(String::from(count)))?,
            };
            Ok(Command::Shader { file, words })
        }
    }
}

/// The bytes of each memory the transfers run through.
const BYTES: u64 = 64 << 10;

/// The longest the work takes on any device.
pub const DEADLINE: Duration64 = Duration64::from_secs(DEADLINE_SECS);

/// How long the CPU waits on the work: as long again past its deadline,
/// which runs only once the device starts the work, for it to start and for
/// its end to be told.
const WAIT: Duration64 = Duration64::from_secs(2 * DEADLINE_SECS);

const DEADLINE_SECS: i64 = 10;

/// The memory the reference is made for: the half its heap offers holds the
/// work's memory and its pipelines with room to spare.
const REFERENCE_MEMORY: u64 = 16 << 20;

/// What the transfers left on one queue family.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transferred {
    /// The family they ran on.
    pub family: u32,
    /// Their host-visible memory, read back once they ran.
    pub bytes: Vec<u8>,
    /// The refusal of a fill past its buffer.
    pub refused: Option<SubmitError>,
}

/// What a device answered the work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Answer {
    /// What the transfers left on each family stating them, in order.
    pub transfers: Vec<Transferred>,
    /// What each pass of the pinned set left in each of its bindings, in the
    /// set's order.
    pub passes: Vec<Vec<Vec<u32>>>,
    /// What the image work left; `None` where the device states not every
    /// use it makes of its formats.
    pub images: Option<Vec<u8>>,
    /// What the render work left; `None` where the device draws nothing or
    /// states not every use it makes of its formats.
    pub render: Option<Rendered>,
    /// How a dispatch past the device's counts was answered; `None` where no
    /// count is past them.
    pub past_counts: Option<Result<(), Error>>,
    /// What each kernel of the differential corpus left, in its order.
    pub corpus: Vec<Ran>,
}

/// What the render work left.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rendered {
    /// Its first three passes' colour target, tags, stencil and float
    /// target, in that order.
    pub passes: Vec<u8>,
    /// What its four-sample pass resolved to; `None` where the device
    /// states no four samples of the colour format.
    pub resolved: Option<Vec<u8>>,
    /// What a draw from a record naming its first instance left, or what
    /// its submission was answered with.
    pub first_instance: Result<Vec<u8>, Error>,
    /// Whether the device states first instances read from records: one
    /// that does not must lose that draw's context.
    pub states_first_instance: bool,
}

/// How a device stating no first instances read from records must answer a
/// draw from a record naming one.
const LOST: Result<Vec<u8>, Error> = Err(Error::Lost(Loss::Fault));

/// A step of the work a device refused, and why.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Refusal {
    /// The transfers, a pass of the pinned set by name, the image work, the
    /// render work, or the dispatch past the device's counts.
    pub step: &'static str,
    /// The queue family the step ran on, where it runs on each.
    pub family: Option<u32>,
    /// What the device answered.
    pub why: Error,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.step)?;
        if let Some(family) = self.family {
            write!(f, " on queue family {family}")?;
        }
        write!(f, ": {}", self.why)
    }
}

/// The steps a refusal names, besides the pinned passes.
const TRANSFERS: &str = "the transfers";
const IMAGES: &str = "the image work";
const RENDER: &str = "the render work";
const PAST_COUNTS: &str = "a dispatch past the device's counts";

/// Where a device's answer differs from the reference's.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Difference {
    /// The device refused a step of the work, where the reference ran it.
    Refused(Refusal),
    /// A byte of the transfers' memory differs: the family they ran on, the
    /// byte's offset, the device's and the reference's.
    Byte {
        /// The family the transfers ran on.
        family: u32,
        /// Its offset into the host-visible memory.
        at: usize,
        /// What the device left there.
        device: u8,
        /// What the reference left there.
        reference: u8,
    },
    /// The command both must refuse was refused differently, or not at all.
    Refusal {
        /// The family it was submitted to.
        family: u32,
        /// The device's refusal.
        device: Option<SubmitError>,
        /// The reference's.
        reference: Option<SubmitError>,
    },
    /// A word a pinned pass left differs.
    Pass {
        /// The pass.
        name: &'static str,
        /// The binding holding the word.
        binding: usize,
        /// Its place among the binding's words.
        at: usize,
        /// What the device left there.
        device: u32,
        /// What the reference left there.
        reference: u32,
    },
    /// A byte the image work left differs: its place among what the work
    /// read back, the device's and the reference's.
    Image {
        /// Its place among the bytes the image work read back.
        at: usize,
        /// What the device left there.
        device: u8,
        /// What the reference left there.
        reference: u8,
    },
    /// A byte the render work's first three passes left differs: its place
    /// among what they read back, the device's and the reference's.
    Render {
        /// Its place among the bytes the passes read back.
        at: usize,
        /// What the device left there.
        device: u8,
        /// What the reference left there.
        reference: u8,
    },
    /// A byte the render work's four-sample pass resolved to differs.
    Resolved {
        /// Its place among the bytes the pass resolved to.
        at: usize,
        /// What the device left there.
        device: u8,
        /// What the reference left there.
        reference: u8,
    },
    /// A byte a draw from a record naming its first instance left differs.
    FirstInstance {
        /// Its place among the bytes the draw left.
        at: usize,
        /// What the device left there.
        device: u8,
        /// What the reference left there.
        reference: u8,
    },
    /// A draw from a record naming its first instance was answered otherwise
    /// than the device's statement demands: drawn where it states first
    /// instances read from records, its context lost where it does not.
    FirstInstanceAnswer {
        /// How the device answered it.
        device: Result<(), Error>,
        /// How it must answer it.
        expected: Result<(), Error>,
    },
    /// A dispatch past the device's counts was answered otherwise.
    PastCounts {
        /// The device's answer.
        device: Option<Result<(), Error>>,
        /// The reference's.
        reference: Option<Result<(), Error>>,
    },
    /// A word a kernel of the corpus left is past what it is held to.
    Corpus {
        /// The kernel's family.
        family: &'static str,
        /// The instruction the word answers.
        case: &'static str,
        /// What the device left.
        device: u32,
        /// What the reference left.
        reference: u32,
    },
}

impl fmt::Display for Difference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(refusal) => write!(f, "refused {refusal}"),
            Self::Byte {
                family,
                at,
                device,
                reference,
            } => write!(
                f,
                "on queue family {family}, byte {at} reads {device:#04x} where the software device left {reference:#04x}"
            ),
            Self::Refusal {
                family,
                device,
                reference,
            } => {
                write!(f, "on queue family {family}, a fill past its buffer was ")?;
                write_answer(f, device.as_ref())?;
                f.write_str(" where the software device's was ")?;
                write_answer(f, reference.as_ref())
            }
            Self::Pass {
                name,
                binding,
                at,
                device,
                reference,
            } => write!(
                f,
                "the {name} pass left {device:#010x} at word {at} of binding {binding} where the software device left {reference:#010x}"
            ),
            Self::Image {
                at,
                device,
                reference,
            } => write!(
                f,
                "byte {at} of the image work reads {device:#04x} where the software device left {reference:#04x}"
            ),
            Self::Render {
                at,
                device,
                reference,
            } => write!(
                f,
                "byte {at} of the render work reads {device:#04x} where the software device left {reference:#04x}"
            ),
            Self::Resolved {
                at,
                device,
                reference,
            } => write!(
                f,
                "byte {at} of the resolved samples reads {device:#04x} where the software device left {reference:#04x}"
            ),
            Self::FirstInstance {
                at,
                device,
                reference,
            } => write!(
                f,
                "byte {at} of the draw naming its first instance reads {device:#04x} where the software device left {reference:#04x}"
            ),
            Self::FirstInstanceAnswer { device, expected } => {
                f.write_str("a draw naming its first instance was ")?;
                write_ran(f, device)?;
                f.write_str(" where it must be ")?;
                write_ran(f, expected)
            }
            Self::PastCounts { device, reference } => {
                f.write_str("a dispatch past the device's counts was ")?;
                write_outcome(f, device.as_ref())?;
                f.write_str(" where the software device's was ")?;
                write_outcome(f, reference.as_ref())
            }
            Self::Corpus {
                family,
                case,
                device,
                reference,
            } => write!(
                f,
                "{family}: {case} left {device:#010x} where the software device left {reference:#010x}"
            ),
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

/// How a dispatch past the device's counts was answered.
fn write_outcome(f: &mut fmt::Formatter<'_>, outcome: Option<&Result<(), Error>>) -> fmt::Result {
    match outcome {
        None => f.write_str("not made, no count being past the device's"),
        Some(ran) => write_ran(f, ran),
    }
}

/// How a submitted piece of work was answered: run, or answered an error.
fn write_ran(f: &mut fmt::Formatter<'_>, ran: &Result<(), Error>) -> fmt::Result {
    match ran {
        Ok(()) => f.write_str("run"),
        Err(why) => write!(f, "answered ({why})"),
    }
}

/// The software device the work is held to, reading its deadlines from
/// `clock`.
///
/// # Errors
///
/// What the software device refused.
pub fn reference(clock: Arc<dyn MonotonicClock + Send + Sync>) -> Result<Device, Error> {
    software(REFERENCE_MEMORY, clock)
}

/// A software device of a machine of `memory_bytes`, reading its deadlines
/// from `clock` and running its work on the thread that readies it.
///
/// # Errors
///
/// What the software device refused.
pub fn software(
    memory_bytes: u64,
    clock: Arc<dyn MonotonicClock + Send + Sync>,
) -> Result<Device, Error> {
    Device::software(&SoftwareDesc {
        memory_bytes,
        clock,
        runner: Arc::new(tairix_parallel::Serial),
        threads: &NoThreads,
        subgroup_size: DEFAULT_SUBGROUP_SIZE,
    })
}

/// The first host-visible, coherent memory type of `device`: what all the
/// memory the CPU reads and writes is made of.
///
/// # Errors
///
/// [`Error::NoSuchMemoryType`] where the device has none.
fn near(device: &Device) -> Result<u32, Error> {
    let visible = MemoryProperties::HOST_VISIBLE.with(MemoryProperties::HOST_COHERENT);
    device
        .info()
        .memory_types
        .iter()
        .position(|kind| kind.properties.contains(visible))
        .and_then(|index| u32::try_from(index).ok())
        .ok_or(Error::NoSuchMemoryType)
}

/// Whether `device` states each use `uses` names of its format.
fn states_uses(device: &Device, uses: &[(Format, FormatFeatures)]) -> bool {
    let formats = &device.info().formats;
    uses.iter()
        .all(|&(format, wanted)| formats.get(format).contains(wanted))
}

/// Each of `resources` bound in turn, from binding 0.
fn in_binding_order<'a, const N: usize>(resources: &'a [Resource<'a>; N]) -> [BindSetEntry<'a>; N] {
    let mut next = 0;
    resources.each_ref().map(|resource| {
        let binding = next;
        next += 1;
        BindSetEntry {
            binding,
            resources: core::slice::from_ref(resource),
        }
    })
}

/// A buffer for `usage` over memory of its own: `size` bytes of type
/// `memory_type`.
fn buffer(
    context: &Context,
    (memory_type, size): (u32, u64),
    usage: BufferUsage,
) -> Result<(Allocation, Buffer), Error> {
    let memory = context.allocate(&AllocationDesc { size, memory_type })?;
    let buffer = context.create_buffer(
        &memory,
        &BufferDesc {
            offset: 0,
            size,
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

fn bytes_of(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

/// The first queue family of `device` stating `work`.
///
/// # Errors
///
/// [`Error::Unsupported`] where none states it.
pub fn family(device: &Device, work: QueueCapabilities) -> Result<u32, Error> {
    family_where(device, |family| family.capabilities.contains(work))
}

/// The first queue family of `device` that clears colour: one that computes
/// or draws.
///
/// # Errors
///
/// [`Error::Unsupported`] where none does.
pub fn clearing(device: &Device) -> Result<u32, Error> {
    family_where(device, QueueFamily::clears_colour)
}

fn family_where(device: &Device, runs: impl Fn(&QueueFamily) -> bool) -> Result<u32, Error> {
    device
        .info()
        .queue_families
        .iter()
        .position(runs)
        .and_then(|index| u32::try_from(index).ok())
        .ok_or(Error::Unsupported)
}

/// Submit `commands` to `context`'s first queue of family `family`: refused,
/// or answered.
fn submit(context: &Context, family: u32, commands: &CommandBuffer) -> Result<(), Error> {
    context
        .queue(family, 0)?
        .submit(&Submission {
            command_buffers: &[commands],
            waits: &[],
            signals: &[],
            deadline: DEADLINE,
        })
        .map_err(|refused| refused.error)?
        .wait(WAIT)
}

/// A refusal, named for the work's step `step`.
fn at(step: &'static str) -> impl Fn(Error) -> Refusal {
    move |why| Refusal {
        step,
        family: None,
        why,
    }
}

/// Run the work on `device`.
///
/// # Errors
///
/// The step the device refused that every device must run, and why.
pub fn work(device: &Device) -> Result<Answer, Refusal> {
    let near = near(device).map_err(at(TRANSFERS))?;
    let mut transferred = Vec::new();
    for (family, stated) in (0u32..).zip(&device.info().queue_families) {
        if stated.capabilities.contains(QueueCapabilities::TRANSFER) {
            let refused = |why| Refusal {
                step: TRANSFERS,
                family: Some(family),
                why,
            };
            transferred.push(transfers(device, (near, family)).map_err(refused)?);
        }
    }
    if transferred.is_empty() {
        return Err(at(TRANSFERS)(Error::Unsupported));
    }
    let context = device
        .create_context(&ContextDesc::default())
        .map_err(at(TRANSFERS))?;
    let limits = &device.info().limits;
    let compute = family(device, QueueCapabilities::COMPUTE);
    let mut passes = Vec::new();
    for pinned in &SET {
        let left = compute.and_then(|compute| pass(&context, (near, compute, limits), pinned));
        passes.push(left.map_err(at(pinned.name))?);
    }
    let images = images::work(device, near).map_err(at(IMAGES))?;
    let render = render::work(device, near).map_err(at(RENDER))?;
    let past_counts = past_counts(device, near).map_err(at(PAST_COUNTS))?;
    let corpus = corpus::run(device)?;
    Ok(Answer {
        transfers: transferred,
        passes,
        images,
        render,
        past_counts,
        corpus,
    })
}

/// The transfers on a queue of `family`, through host-visible memory of type
/// `near`: the bytes they leave, and how the fill past its buffer was
/// refused.
fn transfers(device: &Device, (near, family): (u32, u32)) -> Result<Transferred, Error> {
    // Memory the device keeps for itself where it has some, else its own.
    let far = device
        .info()
        .memory_types
        .iter()
        .position(|kind| !kind.properties.contains(MemoryProperties::HOST_VISIBLE))
        .and_then(|index| u32::try_from(index).ok())
        .unwrap_or(near);
    let context = device.create_context(&ContextDesc::default())?;
    let copies = BufferUsage::TRANSFER_SRC.with(BufferUsage::TRANSFER_DST);
    let (memory, staging) = buffer(&context, (near, BYTES), copies)?;
    let (_kept, deep) = buffer(&context, (far, BYTES), copies)?;
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
    submit(&context, family, &commands)?;
    let mut past = context.command_buffer();
    past.fill_buffer(&staging, 0, BYTES + 4, 0)?;
    let refused = context
        .queue(family, 0)?
        .submit(&Submission {
            command_buffers: &[&past],
            waits: &[],
            signals: &[],
            deadline: DEADLINE,
        })
        .err();
    let mut bytes = vec![0; usize::try_from(BYTES).map_err(|_| Error::TooLarge)?];
    memory.read(0, &mut bytes)?;
    Ok(Transferred {
        family,
        bytes,
        refused,
    })
}

/// The compute pipeline of `spirv`'s entry point `entry`, laid out as the
/// module states with `push` bytes of push constants, its set's layout, and
/// that set's bindings.
fn pipeline(
    context: &Context,
    (spirv, entry_point): (&[u8], &str),
    push: u32,
) -> Result<(ComputePipeline, BindSetLayout, Vec<LayoutBinding>), Error> {
    let module = Module::decode(spirv).map_err(|_| Error::Malformed)?;
    let admitted = Admitted::new(module).map_err(|_| Error::Malformed)?;
    let entry = admitted
        .entry_point(entry_point, Stage::Compute)
        .ok_or(Error::EntryPoint)?;
    let bindings = entry
        .bindings()
        .iter()
        .map(|binding| LayoutBinding::unsampled(binding, Stages::COMPUTE))
        .collect::<Option<Vec<LayoutBinding>>>()
        .ok_or(Error::Unsupported)?;
    let layout = context.create_bind_set_layout(&bindings)?;
    let stages = if push == 0 {
        Stages::NONE
    } else {
        Stages::COMPUTE
    };
    let laid_out = context.create_pipeline_layout(
        &[&layout],
        PushConstants {
            bytes: push,
            stages,
        },
    )?;
    let shader = context.create_shader_module(admitted)?;
    let pipeline = context.create_compute_pipeline(&ComputePipelineDesc {
        module: &shader,
        entry_point,
        layout: &laid_out,
        specialization: &[],
    })?;
    Ok((pipeline, layout, bindings))
}

/// What the pinned pass `pinned` leaves of its sample in each binding, run
/// in `context` on a queue of family `compute` through host-visible memory of
/// type `near`; a binding's range is whole granules of the robust accesses
/// `limits` state for its kind, the words past the sample's left unread.
fn pass(
    context: &Context,
    (near, compute, limits): (u32, u32, &Limits),
    pinned: &Pinned,
) -> Result<Vec<Vec<u32>>, Error> {
    let sample = (pinned.sample)().map_err(|refused| match refused {
        Refused::OutOfMemory => Error::OutOfMemory,
        _ => Error::Malformed,
    })?;
    let push = u32::try_from(sample.push.len() * 4).map_err(|_| Error::TooLarge)?;
    let (pipeline, layout, bindings) = pipeline(context, (pinned.spirv, "main"), push)?;
    let mut held = Vec::new();
    for binding in &bindings {
        let words = sample
            .buffers
            .get(binding.binding as usize)
            .ok_or(Error::Bindings)?;
        let (usage, granule) = match binding.kind {
            DescriptorType::UniformBuffer => {
                (BufferUsage::UNIFORM, limits.uniform_buffer_size_alignment)
            }
            DescriptorType::StorageBuffer => {
                (BufferUsage::STORAGE, limits.storage_buffer_size_alignment)
            }
            _ => return Err(Error::Unsupported),
        };
        let size = u64::try_from(words.len().max(1) * 4)
            .map_err(|_| Error::TooLarge)?
            .next_multiple_of(granule.max(4));
        let (memory, buffer) = buffer(context, (near, size), usage)?;
        memory.write(0, &bytes_of(words))?;
        held.push((memory, buffer, size, words.len()));
    }
    let resources: Vec<Resource<'_>> = held
        .iter()
        .map(|(_, buffer, size, _)| Resource::Buffer {
            buffer,
            offset: 0,
            size: *size,
        })
        .collect();
    let entries: Vec<BindSetEntry<'_>> = bindings
        .iter()
        .zip(&resources)
        .map(|(binding, resource)| BindSetEntry {
            binding: binding.binding,
            resources: core::slice::from_ref(resource),
        })
        .collect();
    let set = context.create_bind_set(&layout, &entries)?;
    let mut commands = context.command_buffer();
    commands.dispatch(&pipeline, &[&set], &bytes_of(&sample.push), sample.groups)?;
    submit(context, compute, &commands)?;
    let mut left = Vec::new();
    for (memory, _, _, len) in &held {
        let mut bytes = vec![0; len * 4];
        memory.read(0, &mut bytes)?;
        let (words, _) = bytes.as_chunks::<4>();
        left.push(words.iter().map(|word| u32::from_le_bytes(*word)).collect());
    }
    Ok(left)
}

/// How a fill dispatched indirectly with counts one past `device`'s along
/// its narrowest axis is answered, in a context of its own since it must be
/// lost; `None` where every axis takes any count.
fn past_counts(device: &Device, near: u32) -> Result<Option<Result<(), Error>>, Error> {
    let most = device.info().limits.max_workgroup_count;
    let Some((axis, &narrowest)) = most.iter().enumerate().min_by_key(|&(_, count)| *count) else {
        return Ok(None);
    };
    let Some(past) = narrowest.checked_add(1) else {
        return Ok(None);
    };
    let mut counts = [1; 3];
    counts[axis] = past;
    let compute = family(device, QueueCapabilities::COMPUTE)?;
    let context = device.create_context(&ContextDesc::default())?;
    let push = u32::try_from(fill::WORDS * 4).map_err(|_| Error::TooLarge)?;
    let (pipeline, layout, _) = pipeline(&context, (fill::SPIRV, "main"), push)?;
    let surface_bytes = 64 * 4;
    let (_memory, surface) = buffer(&context, (near, surface_bytes), BufferUsage::STORAGE)?;
    let (arguments, read) = buffer(&context, (near, 16), BufferUsage::INDIRECT)?;
    arguments.write(0, &bytes_of(&counts))?;
    let set = context.create_bind_set(
        &layout,
        &[BindSetEntry {
            binding: fill::TARGET,
            resources: &[Resource::Buffer {
                buffer: &surface,
                offset: 0,
                size: surface_bytes,
            }],
        }],
    )?;
    let words = fill::Push {
        stride: 8,
        left: 0,
        top: 0,
        width: 8,
        height: 8,
        pixel: 1,
    }
    .words();
    let mut commands = context.command_buffer();
    commands.dispatch_indirect(&pipeline, &[&set], &bytes_of(&words), (&read, 0))?;
    Ok(Some(submit(&context, compute, &commands)))
}

/// Where the transfers on any of a device's families, `answered`, differ
/// from the reference's on its one family, the first of `expected`.
fn compare_transfers(answered: &[Transferred], expected: &[Transferred]) -> Result<(), Difference> {
    let Some(held) = expected.first() else {
        return Ok(());
    };
    for left in answered {
        if let Some((at, device, reference)) = first_difference(&left.bytes, &held.bytes) {
            return Err(Difference::Byte {
                family: left.family,
                at,
                device,
                reference,
            });
        }
        if left.refused != held.refused {
            return Err(Difference::Refusal {
                family: left.family,
                device: left.refused,
                reference: held.refused,
            });
        }
    }
    Ok(())
}

/// Where `answered`, a device's answer to the work, differs from
/// `expected`, the reference's: the first difference, in the order the work
/// ran.
///
/// # Errors
///
/// [`Difference`] where they differ.
pub fn compare(answered: &Answer, expected: &Answer) -> Result<(), Difference> {
    compare_transfers(&answered.transfers, &expected.transfers)?;
    for ((pinned, left), held) in SET.iter().zip(&answered.passes).zip(&expected.passes) {
        let mut words = left
            .iter()
            .zip(held)
            .enumerate()
            .flat_map(|(binding, (left, held))| {
                left.iter()
                    .zip(held)
                    .enumerate()
                    .map(move |(at, (&device, &reference))| (binding, at, device, reference))
            });
        if let Some((binding, at, device, reference)) =
            words.find(|&(_, _, device, reference)| device != reference)
        {
            return Err(Difference::Pass {
                name: pinned.name,
                binding,
                at,
                device,
                reference,
            });
        }
    }
    // A device stating less than a step uses is not held to it.
    if let (Some(left), Some(held)) = (&answered.images, &expected.images) {
        if let Some((at, device, reference)) = first_difference(left, held) {
            return Err(Difference::Image {
                at,
                device,
                reference,
            });
        }
    }
    if let (Some(left), Some(held)) = (&answered.render, &expected.render) {
        if let Some((at, device, reference)) = first_difference(&left.passes, &held.passes) {
            return Err(Difference::Render {
                at,
                device,
                reference,
            });
        }
        if let (Some(left), Some(held)) = (&left.resolved, &held.resolved) {
            if let Some((at, device, reference)) = first_difference(left, held) {
                return Err(Difference::Resolved {
                    at,
                    device,
                    reference,
                });
            }
        }
        let wanted = if left.states_first_instance {
            &held.first_instance
        } else {
            &LOST
        };
        match (&left.first_instance, wanted) {
            (Ok(drew), Ok(held)) => {
                if let Some((at, device, reference)) = first_difference(drew, held) {
                    return Err(Difference::FirstInstance {
                        at,
                        device,
                        reference,
                    });
                }
            }
            (answered, wanted) => {
                let ran = |outcome: &Result<Vec<u8>, Error>| {
                    outcome.as_ref().map(|_| ()).map_err(|why| *why)
                };
                if ran(answered) != ran(wanted) {
                    return Err(Difference::FirstInstanceAnswer {
                        device: ran(answered),
                        expected: ran(wanted),
                    });
                }
            }
        }
    }
    // A device whose counts no count is past cannot be held to this.
    if answered.past_counts.is_some() && answered.past_counts != expected.past_counts {
        return Err(Difference::PastCounts {
            device: answered.past_counts,
            reference: expected.past_counts,
        });
    }
    corpus::compare(&answered.corpus, &expected.corpus)
}

/// The first place `left` and `held` differ, and what each holds there.
fn first_difference(left: &[u8], held: &[u8]) -> Option<(usize, u8, u8)> {
    left.iter()
        .zip(held)
        .enumerate()
        .find(|(_, (device, reference))| device != reference)
        .map(|(at, (&device, &reference))| (at, device, reference))
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
