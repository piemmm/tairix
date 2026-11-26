//! The sandboxed WGSL compile service (`plans/SHADER.md` SH4).
//!
//! WGSL source is untrusted input, so `tairix-wgsl` compiles it in a sandbox
//! worker, never in the program that asked. The worker answers the module's
//! bytes and the diagnostics its source's filters leave; the caller-side
//! [`compile`] believes none of it until it has checked it here — the bytes
//! decoded and admitted by `lib/spirv`'s validator, and a diagnostic believed
//! only where its span lies on the source's character boundaries, its code is
//! one the front end has, and its message is printable text. Its line and
//! column are worked out here from this side's own source. A reply that
//! fails any of it is a worker broken or subverted, retired as a crash is, so
//! the trust boundary is the validator, not the process.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_spirv::{Admitted, Module};
use tairix_wgsl::{
    Code, Diagnostic, Failure, Options, OverrideKey, Severity, Span, MAX_DIAGNOSTICS,
    MAX_MESSAGE_BYTES, MAX_OUTPUT_BYTES, MAX_SOURCE_BYTES,
};

use crate::host::{Launcher, ParserSandbox, SandboxError, Unbelieved};
use crate::memory::WorkerMemory;
use crate::wire::{Reader, Writer};
use crate::worker::Service;

/// The service's name, as a program serving more than one one-shot service
/// tells its worker which (`rt::serves`).
pub const SERVICE: &[u8] = b"wgsl";

/// What a compile worker is given to compile in: the front end's own bounds
/// hold the largest source's compile within two frames.
pub const WORKER_MEMORY: WorkerMemory = WorkerMemory::Messages;

/// The most override values one compile is given.
pub const MAX_OVERRIDES: usize = 4096;

/// The longest name an override is given its value by.
pub const MAX_OVERRIDE_NAME_BYTES: usize = 1024;

const OP_COMPILE: u8 = 1;

const KEY_NAME: u8 = 0;
const KEY_ID: u8 = 1;

const REPLY_MODULE: u8 = 1;
const REPLY_SOURCE: u8 = 2;
const REPLY_FAULT: u8 = 3;

const SEVERITY_INFO: u8 = 0;
const SEVERITY_WARNING: u8 = 1;
const SEVERITY_ERROR: u8 = 2;

/// Why the worker answered no module and no source error.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Fault {
    /// The request broke the request grammar.
    MalformedRequest,
    /// The front end could not have the memory the compile needed.
    OutOfMemory,
    /// One of the front end's own invariants failed: a defect in it.
    Internal,
}

impl Fault {
    const fn to_wire(self) -> u8 {
        match self {
            Self::MalformedRequest => 1,
            Self::OutOfMemory => 2,
            Self::Internal => 3,
        }
    }

    const fn from_wire(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::MalformedRequest),
            2 => Some(Self::OutOfMemory),
            3 => Some(Self::Internal),
            _ => None,
        }
    }
}

/// A module a sandboxed compile answered, admitted here, and the warnings
/// and notes its source's diagnostic filters leave, in source order.
#[derive(Debug)]
pub struct CompiledModule {
    /// The module, admitted by the validator in this process.
    pub module: Admitted,
    /// At most [`MAX_DIAGNOSTICS`] warnings and notes, each placed in this
    /// side's source.
    pub diagnostics: Vec<Diagnostic>,
}

/// Why [`compile`] answered no module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompileFailure {
    /// The source is refused: its first shader-creation error.
    Source(Diagnostic),
    /// The source is longer than [`MAX_SOURCE_BYTES`], or the overrides past
    /// [`MAX_OVERRIDES`] or [`MAX_OVERRIDE_NAME_BYTES`]: refused here, unsent.
    TooLarge,
    /// The sandbox failed: the worker crashed, could not be started, or did
    /// not answer in time.
    Sandbox(SandboxError),
    /// The worker reported the compile failed for want of memory or by a
    /// defect of the front end's own.
    Fault(Fault),
    /// The worker's reply broke the reply grammar, or answered a module the
    /// validator refuses or a diagnostic this side cannot believe.
    ReplyMalformed,
    /// There was no memory here for the request or the answer.
    NoMemory,
}

impl Unbelieved for CompileFailure {
    fn unbelieved(&self) -> bool {
        // This side wrote the request, so a worker calling it malformed is
        // one that cannot be believed either.
        matches!(
            self,
            Self::ReplyMalformed | Self::Fault(Fault::MalformedRequest)
        )
    }
}

/// The service the sandboxed worker runs: decode the request, compile, and
/// answer the module or the error. Total: every failure is a typed reply.
#[derive(Debug, Default)]
pub struct WgslService;

impl Service for WgslService {
    fn handle(&mut self, request: &[u8]) -> Vec<u8> {
        let mut overrides = Vec::new();
        let Some(source) = decode_request(request, &mut overrides) else {
            return fault_reply(Fault::MalformedRequest);
        };
        let options = Options {
            overrides: &overrides,
        };
        match tairix_wgsl::compile(source, &options) {
            Ok(compiled) => {
                let mut w = Writer::refusable();
                w.u8(REPLY_MODULE);
                w.bytes(&compiled.module);
                // At most MAX_DIAGNOSTICS, which a u16 holds.
                w.u16(u16::try_from(compiled.diagnostics.len()).unwrap_or(u16::MAX));
                for diagnostic in &compiled.diagnostics {
                    write_diagnostic(&mut w, diagnostic);
                }
                w.finish()
                    .unwrap_or_else(|| fault_reply(Fault::OutOfMemory))
            }
            Err(Failure::Error(diagnostic)) => {
                let mut w = Writer::refusable();
                w.u8(REPLY_SOURCE);
                write_diagnostic(&mut w, &diagnostic);
                w.finish()
                    .unwrap_or_else(|| fault_reply(Fault::OutOfMemory))
            }
            Err(Failure::OutOfMemory) => fault_reply(Fault::OutOfMemory),
            Err(Failure::Internal) => fault_reply(Fault::Internal),
        }
    }
}

/// The reply naming `fault`.
fn fault_reply(fault: Fault) -> Vec<u8> {
    alloc::vec![REPLY_FAULT, fault.to_wire()]
}

/// The source a request carries, its overrides gathered into `overrides`;
/// `None` for one that breaks the grammar or its bounds.
fn decode_request<'r>(
    request: &'r [u8],
    overrides: &mut Vec<(OverrideKey<'r>, f64)>,
) -> Option<&'r str> {
    let mut r = Reader::new(request);
    if r.u8().ok()? != OP_COMPILE {
        return None;
    }
    let count = usize::from(r.u16().ok()?);
    if count > MAX_OVERRIDES {
        return None;
    }
    overrides.try_reserve_exact(count).ok()?;
    for _ in 0..count {
        let key = match r.u8().ok()? {
            KEY_NAME => OverrideKey::Name(r.str(MAX_OVERRIDE_NAME_BYTES).ok()?),
            KEY_ID => OverrideKey::Id(r.u16().ok()?),
            _ => return None,
        };
        overrides.push((key, f64::from_bits(r.u64().ok()?)));
    }
    let source = r.str(MAX_SOURCE_BYTES).ok()?;
    r.is_exhausted().then_some(source)
}

fn write_diagnostic<R: crate::wire::Room>(w: &mut Writer<R>, diagnostic: &Diagnostic) {
    w.u8(match diagnostic.severity {
        Severity::Info => SEVERITY_INFO,
        Severity::Warning => SEVERITY_WARNING,
        Severity::Error => SEVERITY_ERROR,
    });
    w.u16(diagnostic.code.number());
    w.u32(diagnostic.span.start);
    w.u32(diagnostic.span.end);
    w.str(&diagnostic.message);
}

/// A diagnostic of the reply, believed only where it is one the front end
/// could have made of `source`: of `severity`'s kind, its code one the front
/// end has, its span on `source`'s character boundaries, and its message
/// printable. Its line and column are worked out here.
fn read_diagnostic(
    r: &mut Reader<'_>,
    source: &str,
    errors: bool,
) -> Result<Diagnostic, CompileFailure> {
    let malformed = |_| CompileFailure::ReplyMalformed;
    let severity = match r.u8().map_err(malformed)? {
        SEVERITY_INFO => Severity::Info,
        SEVERITY_WARNING => Severity::Warning,
        SEVERITY_ERROR => Severity::Error,
        _ => return Err(CompileFailure::ReplyMalformed),
    };
    if (severity == Severity::Error) != errors {
        return Err(CompileFailure::ReplyMalformed);
    }
    let code =
        Code::from_number(r.u16().map_err(malformed)?).ok_or(CompileFailure::ReplyMalformed)?;
    let (start, end) = (r.u32().map_err(malformed)?, r.u32().map_err(malformed)?);
    let within = |at: u32| usize::try_from(at).is_ok_and(|at| source.is_char_boundary(at));
    if start > end || !within(start) || !within(end) {
        return Err(CompileFailure::ReplyMalformed);
    }
    let text = r.str(MAX_MESSAGE_BYTES).map_err(malformed)?;
    if text.chars().any(char::is_control) {
        return Err(CompileFailure::ReplyMalformed);
    }
    let mut message = String::new();
    message
        .try_reserve_exact(text.len())
        .map_err(|_| CompileFailure::NoMemory)?;
    message.push_str(text);
    let (line, column) = tairix_wgsl::position(source, start);
    Ok(Diagnostic {
        severity,
        code,
        span: Span::new(start, end),
        line,
        column,
        message,
    })
}

/// Decode and believe the worker's reply to a compile of `source`.
fn decode_reply(reply: &[u8], source: &str) -> Result<CompiledModule, CompileFailure> {
    let mut r = Reader::new(reply);
    let malformed = |_| CompileFailure::ReplyMalformed;
    let answer = match r.u8().map_err(malformed)? {
        REPLY_MODULE => {
            let bytes = r.bytes(MAX_OUTPUT_BYTES).map_err(malformed)?;
            let count = usize::from(r.u16().map_err(malformed)?);
            if count > MAX_DIAGNOSTICS {
                return Err(CompileFailure::ReplyMalformed);
            }
            let mut diagnostics = Vec::new();
            diagnostics
                .try_reserve_exact(count)
                .map_err(|_| CompileFailure::NoMemory)?;
            for _ in 0..count {
                diagnostics.push(read_diagnostic(&mut r, source, false)?);
            }
            // The validator is the trust boundary: a module it refuses is one
            // no front end of ours answered.
            let module = Module::decode(bytes).map_err(|_| CompileFailure::ReplyMalformed)?;
            let module = Admitted::new(module).map_err(|_| CompileFailure::ReplyMalformed)?;
            Ok(CompiledModule {
                module,
                diagnostics,
            })
        }
        REPLY_SOURCE => Err(CompileFailure::Source(read_diagnostic(
            &mut r, source, true,
        )?)),
        REPLY_FAULT => Err(CompileFailure::Fault(
            Fault::from_wire(r.u8().map_err(malformed)?).ok_or(CompileFailure::ReplyMalformed)?,
        )),
        _ => return Err(CompileFailure::ReplyMalformed),
    };
    if !r.is_exhausted() {
        return Err(CompileFailure::ReplyMalformed);
    }
    answer
}

/// Compile `source` with `overrides` in the sandboxed worker, answering the
/// module only once it is admitted here.
///
/// # Errors
///
/// [`CompileFailure`]: the source refused with its error, the request past
/// its bounds, the sandbox failed, the worker's compile failed, or the reply
/// could not be believed — which retires the worker.
pub fn compile<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    source: &str,
    overrides: &[(OverrideKey<'_>, f64)],
) -> Result<CompiledModule, CompileFailure> {
    let named_within = |key: &OverrideKey<'_>| match key {
        OverrideKey::Name(name) => name.len() <= MAX_OVERRIDE_NAME_BYTES,
        OverrideKey::Id(_) => true,
    };
    if source.len() > MAX_SOURCE_BYTES
        || overrides.len() > MAX_OVERRIDES
        || !overrides.iter().all(|(key, _)| named_within(key))
    {
        return Err(CompileFailure::TooLarge);
    }
    let mut w = Writer::refusable();
    w.u8(OP_COMPILE);
    w.u16(u16::try_from(overrides.len()).map_err(|_| CompileFailure::TooLarge)?);
    for &(key, value) in overrides {
        match key {
            OverrideKey::Name(name) => {
                w.u8(KEY_NAME);
                w.str(name);
            }
            OverrideKey::Id(id) => {
                w.u8(KEY_ID);
                w.u16(id);
            }
        }
        w.u64(value.to_bits());
    }
    w.str(source);
    let request = w.finish().ok_or(CompileFailure::NoMemory)?;
    sandbox.ask(|sandbox| {
        let reply = sandbox.request(&request).map_err(CompileFailure::Sandbox)?;
        decode_reply(&reply, source)
    })
}

#[cfg(test)]
#[path = "wgsl_tests.rs"]
mod tests;
