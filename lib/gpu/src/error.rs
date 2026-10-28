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
}

/// Why a request was refused. A refused request changes nothing.
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
    /// The memory could not be had.
    OutOfMemory,
    /// The allocation would take the context past its memory limit.
    OverLimit,
    /// The allocation would take its heap past what the heap holds.
    HeapFull,
    /// The device has no memory type of that index.
    NoSuchMemoryType,
    /// The device has no queue of that family and index.
    NoSuchQueue,
    /// Larger than the device's largest allocation.
    TooLarge,
    /// A size of zero, or a usage of none.
    Empty,
    /// An offset or size off the alignment the operation requires.
    Misaligned,
    /// A range running past the end of what it names.
    OutOfRange,
    /// The buffer was not made for that use, or the usage is not one a buffer
    /// may declare.
    Usage,
    /// The memory is not one the CPU can reach.
    NotHostVisible,
    /// A copy whose destination ranges overlap each other or its sources.
    Overlap,
    /// A timeline signal that would not advance it past every earlier one.
    TimelineOrder,
    /// A deadline of zero, or past the device's longest.
    Deadline,
    /// More than a submission or a command may carry.
    TooMany,
    /// The wait's timeout elapsed first.
    TimedOut,
    /// A device description no context could be served by.
    Malformed,
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
            Self::Destroyed => "the context no longer exists",
            Self::Foreign => "the object belongs to another context or device",
            Self::Stale => "the object no longer exists",
            Self::OutOfMemory => "the memory could not be had",
            Self::OverLimit => "the context's memory limit would be passed",
            Self::HeapFull => "the heap would be overfilled",
            Self::NoSuchMemoryType => "the device has no such memory type",
            Self::NoSuchQueue => "the device has no such queue",
            Self::TooLarge => "larger than the device's largest allocation",
            Self::Empty => "nothing was asked for",
            Self::Misaligned => "an offset or size is misaligned",
            Self::OutOfRange => "a range runs past the end of what it names",
            Self::Usage => "the buffer was not made for that use",
            Self::NotHostVisible => "the memory is not reachable by the CPU",
            Self::Overlap => "a copy's destination overlaps",
            Self::TimelineOrder => "the signal would not advance the timeline",
            Self::Deadline => "the deadline is zero or past the device's longest",
            Self::TooMany => "more than a submission or command may carry",
            Self::TimedOut => "the wait timed out",
            Self::Malformed => "the device description is malformed",
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
