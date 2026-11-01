//! Why a request was refused, and why a context was lost.

use core::fmt;

/// Why a context was lost: it then refuses all work and only releases what
/// it holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Loss {
    /// A submission of its own ran past its deadline.
    Overran,
    /// A submission of its own had begun on an engine reset for another
    /// context's overrun, so what it had done cannot be vouched for.
    Reset,
    /// The device found a submission of its own malformed while running it.
    Fault,
    /// The host refused memory a submission of its own needed as it ran,
    /// past what its submission had reserved for it.
    OutOfMemory,
}

/// Why a request was refused. A refused request changes nothing but the room
/// one of its context's tables or queues, or for a refused context the
/// device's own, may have made ahead of it, which stays, charged, for the
/// next.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// The context was lost.
    Lost(Loss),
    /// The context no longer exists.
    Destroyed,
    /// The object belongs to another context, or another device.
    Foreign,
    /// The object no longer exists.
    Stale,
    /// The host refused memory the request needs.
    OutOfMemory,
    /// The request would take the context past its memory limit.
    OverLimit,
    /// The request would take a heap past what the heap holds.
    HeapFull,
    /// The device has no memory type of that index.
    NoSuchMemoryType,
    /// The device has no queue of that family and index.
    NoSuchQueue,
    /// Past what the device takes: an allocation past its largest, an image,
    /// view, texel buffer, workgroup, working set or program past its limits,
    /// or a module whose constants or lowering pass their fixed bounds.
    TooLarge,
    /// A size of zero, or a usage of none.
    Empty,
    /// An offset or size off the alignment the operation requires.
    Misaligned,
    /// A range running past the end of what it names, or a number or setting
    /// past what the device takes.
    OutOfRange,
    /// The buffer or image was not made for that use, or the usage is not
    /// one it may declare.
    Usage,
    /// The memory is not one the CPU can reach.
    NotHostVisible,
    /// A copy whose destination ranges overlap each other or its sources.
    Overlap,
    /// A timeline signal that would not advance it past every earlier one.
    TimelineOrder,
    /// A deadline of zero, or past the device's longest.
    Deadline,
    /// More than a submission, a command or a layout may carry.
    TooMany,
    /// The wait's timeout elapsed first.
    TimedOut,
    /// What cannot be: a device description no context could be served by,
    /// or a module whose admitted guarantees the device found broken.
    Malformed,
    /// A shape the object's kind does not take: an image's extent for its
    /// dimension, a view's dimension or layers for its image, a clear's
    /// value for its format.
    Shape,
    /// A sample count the format, image or operation does not take.
    Samples,
    /// An aspect the format does not hold, or more than one where one is
    /// asked for.
    Aspect,
    /// A use the format or the device does not offer.
    Unsupported,
    /// A bind set layout's bindings out of ascending order or repeated, a
    /// bind set's entries not one for each binding of its layout, in order,
    /// each of the binding's count, or a dispatch's bind sets not those its
    /// pipeline's layout names.
    Bindings,
    /// The module has no entry point of that name and stage.
    EntryPoint,
    /// The layout does not provide what the entry point uses.
    Disagreement(tairix_spirv::Disagreement),
    /// The module refused the specialization values: one no constant takes,
    /// one given twice or wider than its constant, or a workgroup size they
    /// make zero.
    Specialization(tairix_spirv::SpecError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Lost(Loss::Overran) => "the context was lost: a submission ran past its deadline",
            Self::Lost(Loss::Reset) => {
                "the context was lost: a device reset took work it had begun"
            }
            Self::Lost(Loss::Fault) => {
                "the context was lost: the device found a submission malformed"
            }
            Self::Lost(Loss::OutOfMemory) => {
                "the context was lost: the host refused memory a submission needed as it ran"
            }
            Self::Destroyed => "the context no longer exists",
            Self::Foreign => "the object belongs to another context or device",
            Self::Stale => "the object no longer exists",
            Self::OutOfMemory => "the host refused the memory needed",
            Self::OverLimit => "the context's memory limit would be passed",
            Self::HeapFull => "the heap would be overfilled",
            Self::NoSuchMemoryType => "the device has no such memory type",
            Self::NoSuchQueue => "the device has no such queue",
            Self::TooLarge => "past what the device takes",
            Self::Empty => "nothing was asked for",
            Self::Misaligned => "an offset or size is misaligned",
            Self::OutOfRange => "a range or a value past what it may be",
            Self::Usage => "the buffer or image was not made for that use",
            Self::NotHostVisible => "the memory is not reachable by the CPU",
            Self::Overlap => "a copy's destination overlaps",
            Self::TimelineOrder => "the signal would not advance the timeline",
            Self::Deadline => "the deadline is zero or past the device's longest",
            Self::TooMany => "more than a submission, command or layout may carry",
            Self::TimedOut => "the wait timed out",
            Self::Malformed => "a device description or module that cannot be",
            Self::Shape => "a shape the object's kind does not take",
            Self::Samples => "a sample count that is not offered",
            Self::Aspect => "an aspect the format does not hold",
            Self::Unsupported => "a use the format or device does not offer",
            Self::Bindings => "the bindings are not one apiece, in order, as the layout says",
            Self::EntryPoint => "the module has no such entry point",
            Self::Disagreement(_) => "the layout does not provide what the entry point uses",
            Self::Specialization(_) => "the module refused the specialization values",
        };
        f.write_str(text)
    }
}

/// Why a submission was refused, and which of its commands, counted across
/// its command buffers in order, was refused if one was.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SubmitError {
    /// The refused command.
    pub command: Option<usize>,
    /// Why.
    pub error: Error,
}

impl From<Error> for SubmitError {
    fn from(error: Error) -> Self {
        Self {
            command: None,
            error,
        }
    }
}

impl fmt::Display for SubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.command {
            Some(command) => write!(f, "command {command}: {}", self.error),
            None => self.error.fmt(f),
        }
    }
}
