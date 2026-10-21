//! A picture file's structure report, read in the sandbox.
//!
//! The report (`tairix_image::inspect`) walks a hostile file record by
//! record, so it runs in the image worker
//! ([`crate::imagerender::ImageRenderService`]) on a document handed over the
//! one upload path ([`crate::imagerender::send_document`],
//! [`crate::imagerender::upload_document`]), and only text reaches the
//! caller, checked as every report reply is (`crate::report`).

use alloc::vec::Vec;

use tairix_image::inspect::inspect;
use tairix_image::sniff;
use tairix_util::report::ReportLine;

use crate::host::{Launcher, ParserSandbox, SandboxError, Unbelieved};
use crate::imagerender::{
    encode_error, is_document_op, Document, DocumentFailure, ReplyFailure, ViewFormat,
};
use crate::memory::WorkerMemory;
use crate::report::{read_lines, write_lines, LinesFault};
use crate::wire::{Reader, Writer};
use crate::worker::Service;

/// The memory a report worker is held to: the one document it reads.
pub const WORKER_MEMORY: WorkerMemory = WorkerMemory::HeldDocument;

/// The worker a structure report is read in, for a caller that wants
/// nothing else of a picture: a document handed over the one upload path,
/// and its report. The image service serves the same op beside its others.
#[derive(Default)]
pub struct InspectService {
    document: Option<Document>,
}

impl Service for InspectService {
    fn handle(&mut self, request: &[u8]) -> Vec<u8> {
        match request.first().copied() {
            Some(op) if is_document_op(op) => Document::serve(request, &mut self.document)
                .unwrap_or_else(|refusal| encode_error(refusal.to_wire())),
            Some(OP_INSPECT) => dispatch(request, &mut self.document),
            _ => encode_error(InspectRefusal::MalformedRequest.to_wire()),
        }
    }
}

/// The inspect opcode and its reply tag, after every other op this worker
/// serves.
const OP_INSPECT: u8 = 20;
const REPLY_INSPECTED: u8 = 20;

/// Whether `op` is this module's.
pub(crate) const fn is_inspect_op(op: u8) -> bool {
    op == OP_INSPECT
}

/// Why the worker refused to report on a document.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum InspectRefusal {
    /// The request broke its grammar, or named a format with no structure
    /// report.
    MalformedRequest,
    /// No complete document had been handed over.
    NoDocument,
    /// The document is no picture this service recognises.
    NotAPicture,
}

impl InspectRefusal {
    const fn to_wire(self) -> u8 {
        match self {
            Self::MalformedRequest => 1,
            Self::NoDocument => 2,
            Self::NotAPicture => 3,
        }
    }

    const fn from_wire(raw: u8) -> Option<Self> {
        Some(match raw {
            1 => Self::MalformedRequest,
            2 => Self::NoDocument,
            3 => Self::NotAPicture,
            _ => return None,
        })
    }
}

impl core::fmt::Display for InspectRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::MalformedRequest => "malformed inspect request",
            Self::NoDocument => "no document was handed over",
            Self::NotAPicture => "not a picture format that can be read",
        })
    }
}

/// Why no report came back.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum InspectFailure {
    /// The sandbox itself failed.
    Sandbox(SandboxError),
    /// Handing the document over failed.
    Document(DocumentFailure),
    /// The worker refused, for the carried reason.
    Refused(InspectRefusal),
    /// The reply broke its grammar or its bounds: it cannot be believed.
    ReplyMalformed,
    /// There was no memory for the report.
    NoMemory,
}

impl InspectFailure {
    /// Whether it failed for want of memory, on either side: worth asking
    /// again, since it says nothing of the document.
    #[must_use]
    pub const fn passing(&self) -> bool {
        match self {
            Self::Sandbox(failure) => {
                failure.out_of_memory() || matches!(failure, SandboxError::WorkerUnavailable(_))
            }
            Self::Document(failure) => failure.out_of_memory(),
            Self::NoMemory => true,
            Self::Refused(_) | Self::ReplyMalformed => false,
        }
    }
}

impl Unbelieved for InspectFailure {
    fn unbelieved(&self) -> bool {
        match self {
            Self::ReplyMalformed => true,
            Self::Document(failure) => failure.unbelieved(),
            Self::Sandbox(_) | Self::Refused(_) | Self::NoMemory => false,
        }
    }
}

impl From<DocumentFailure> for InspectFailure {
    fn from(failure: DocumentFailure) -> Self {
        Self::Document(failure)
    }
}

impl ReplyFailure for InspectFailure {
    const MALFORMED: Self = Self::ReplyMalformed;

    fn refused(code: u8) -> Option<Self> {
        InspectRefusal::from_wire(code).map(Self::Refused)
    }
}

impl core::fmt::Display for InspectFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Sandbox(inner) => write!(f, "the reader failed: {inner}"),
            Self::Document(inner) => write!(f, "the document could not be handed over ({inner})"),
            Self::Refused(refusal) => write!(f, "{refusal}"),
            Self::ReplyMalformed => f.write_str("the reader's answer could not be believed"),
            Self::NoMemory => f.write_str("there is not enough memory to hold the report"),
        }
    }
}

/// Serve `request` over the document handed over last, which it consumes.
pub(crate) fn dispatch(request: &[u8], document: &mut Option<Document>) -> Vec<u8> {
    serve(request, document).unwrap_or_else(|refusal| encode_error(refusal.to_wire()))
}

/// `OP_INSPECT`: report on the document as the format named, or as its
/// signature says when none is.
fn serve(request: &[u8], document: &mut Option<Document>) -> Result<Vec<u8>, InspectRefusal> {
    let mut r = Reader::new(request);
    let (Ok(OP_INSPECT), Ok(named)) = (r.u8(), r.u8()) else {
        return Err(InspectRefusal::MalformedRequest);
    };
    if !r.is_exhausted() {
        return Err(InspectRefusal::MalformedRequest);
    }
    let held = document.take().ok_or(InspectRefusal::NoDocument)?;
    if !held.is_complete() {
        return Err(InspectRefusal::NoDocument);
    }
    let bytes = held.into_bytes();
    let format = match named {
        0 => sniff(&bytes).ok_or(InspectRefusal::NotAPicture)?,
        raw => ViewFormat::from_wire(raw)
            .and_then(ViewFormat::raster)
            .ok_or(InspectRefusal::MalformedRequest)?,
    };
    let report = inspect(&bytes, format);
    let mut w = Writer::new();
    w.u8(REPLY_INSPECTED);
    write_lines(&mut w, report.lines());
    Ok(w.finish())
}

/// The structure report of the document handed over last, read as `format`,
/// or as its own signature says when `None`.
///
/// # Errors
///
/// [`InspectFailure`]: the sandbox failed, the worker refused, or its reply
/// could not be believed.
pub fn inspect_document<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    format: Option<ViewFormat>,
) -> Result<Vec<ReportLine>, InspectFailure> {
    let mut w = Writer::new();
    w.u8(OP_INSPECT);
    w.u8(format.map_or(0, ViewFormat::to_wire));
    let request = w.finish();
    sandbox.ask(|sandbox| {
        let reply = sandbox.request(&request).map_err(InspectFailure::Sandbox)?;
        read_report(&reply)
    })
}

/// The report a reply carries, refused whole when any of it breaks its
/// bounds.
fn read_report(reply: &[u8]) -> Result<Vec<ReportLine>, InspectFailure> {
    let mut r = Reader::new(reply);
    InspectFailure::expect_tag(&mut r, REPLY_INSPECTED)?;
    let lines = read_lines(&mut r).map_err(|fault| match fault {
        LinesFault::Malformed => InspectFailure::ReplyMalformed,
        LinesFault::NoMemory => InspectFailure::NoMemory,
    })?;
    if !r.is_exhausted() {
        return Err(InspectFailure::ReplyMalformed);
    }
    Ok(lines)
}

#[cfg(test)]
#[path = "imageinspect_tests.rs"]
mod tests;
