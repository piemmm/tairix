//! # `tairix-wgsl` — the WGSL front end
//!
//! WebGPU Shading Language source, hostile by assumption, compiled to a
//! SPIR-V module `lib/spirv`'s validator admits (`plans/SHADER.md` SH5). The
//! source is read by a bounded lexer and parser, its meaning resolved as the
//! W3C specification states it, and the module written through
//! `lib/spirv`'s builder; the front end is not trusted to have written a safe
//! module — its caller admits what it answers again — so its bugs are a
//! correctness problem rather than a security one.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

mod check;
mod diag;
mod emit;
mod eval;
mod lex;
mod literal;
mod parse;
mod predeclared;
mod scope;
mod source;
mod syntax;
mod types;

use alloc::vec::Vec;

pub use diag::{Code, Diagnostic, Severity, Span, MAX_MESSAGE_BYTES};

/// The longest source compiled, in bytes: with [`MAX_SYNTAX_NODES`], what
/// keeps a compile within the message-sized sandbox worker beside its request
/// and its answer.
pub const MAX_SOURCE_BYTES: usize = 256 << 10;

/// The most expressions and statements one module holds: what keeps a
/// compile within the worker however densely its source is spelled, as
/// [`MAX_SOURCE_BYTES`] alone does not.
pub const MAX_SYNTAX_NODES: usize = 1 << 17;

/// The largest module written, in bytes: what the worker answers in one
/// frame, and validates within its share.
pub const MAX_OUTPUT_BYTES: usize = 1 << 20;

/// The deepest expressions nest: parentheses, operands, arguments, indexes
/// and template lists within one another.
pub const MAX_EXPRESSION_NESTING: u32 = 255;

/// The deepest brace-enclosed statements nest within a function: the
/// specification's minimum limit.
pub const MAX_BRACE_NESTING: u32 = 127;

/// The most parameters a function takes: the specification's minimum limit.
pub const MAX_PARAMETERS: usize = 255;

/// The most members a structure holds: the specification's minimum limit.
pub const MAX_STRUCT_MEMBERS: usize = 1023;

/// The most case selectors one switch holds, its default among them: the
/// specification's minimum limit.
pub const MAX_CASE_SELECTORS: usize = 1023;

/// The most warnings and notes one compile answers.
pub const MAX_DIAGNOSTICS: usize = 64;

/// How a module's override is named where a compile gives its value.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum OverrideKey<'a> {
    /// By its declared name.
    Name(&'a str),
    /// By its `@id`.
    Id(u16),
}

/// What a compile is given beyond the source.
#[derive(Copy, Clone, Debug, Default)]
pub struct Options<'a> {
    /// The values the module's overrides take, as the WebGPU API gives them:
    /// a number each, converted to its override's type.
    pub overrides: &'a [(OverrideKey<'a>, f64)],
}

/// A module compiled: its SPIR-V, which `lib/spirv`'s validator admitted,
/// and the warnings and notes the source's diagnostic filters leave.
#[derive(Debug)]
pub struct Compiled {
    /// The module's bytes, little-endian words.
    pub module: Vec<u8>,
    /// At most [`MAX_DIAGNOSTICS`] warnings and notes, in source order.
    pub diagnostics: Vec<Diagnostic>,
}

/// Why a compile wrote no module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Failure {
    /// The first shader-creation error.
    Error(Diagnostic),
    /// The allocator refused memory the compile needed.
    OutOfMemory,
    /// One of the front end's own invariants failed: a defect in it, never
    /// an answer about the source.
    Internal,
}

/// The line and column, each from 1, that byte `at` of `source` begins: a
/// line ends at each line break WGSL names, and a column counts code points.
#[must_use]
pub fn position(source: &str, at: u32) -> (u32, u32) {
    source::place(source, at)
}

/// Compile `source` with `options`: deterministic, so one source and one
/// set of override values write one sequence of bytes.
///
/// # Errors
///
/// The first shader-creation error, with the line and column it begins at;
/// [`Failure::OutOfMemory`] where memory was refused.
pub fn compile(source: &str, options: &Options<'_>) -> Result<Compiled, Failure> {
    let failed = |stop: diag::Stop| match stop {
        diag::Stop::Error(code, span, message) => {
            let (line, column) = position(source, span.start);
            Failure::Error(Diagnostic {
                severity: Severity::Error,
                code,
                span,
                line,
                column,
                message,
            })
        }
        diag::Stop::OutOfMemory => Failure::OutOfMemory,
        diag::Stop::Internal => Failure::Internal,
    };
    if source.len() > MAX_SOURCE_BYTES {
        return Err(failed(diag::error(
            Code::ModuleTooLarge,
            Span::default(),
            |m| {
                use core::fmt::Write as _;
                write!(m, "the source is longer than {MAX_SOURCE_BYTES} bytes")
            },
        )));
    }
    let tokens = lex::tokens(source).map_err(failed)?;
    let tree = parse::parse(source, &tokens).map_err(failed)?;
    let symbols = scope::Symbols::intern(source, &tokens).map_err(failed)?;
    // The tokens hold nothing the checker reads past the names they intern.
    drop(tokens);
    let (builder, notes) = {
        let mut checker = check::check(source, &tree, symbols, options).map_err(failed)?;
        let builder = emit::write(&mut checker).map_err(failed)?;
        (builder, core::mem::take(&mut checker.notes))
    };
    // What the module was written from is let go of before it is validated.
    drop(tree);
    let module = emit::finish(builder).map_err(failed)?;
    let mut diagnostics = Vec::new();
    diagnostics
        .try_reserve(notes.len())
        .map_err(|_| Failure::OutOfMemory)?;
    for note in notes {
        let (line, column) = position(source, note.span.start);
        diagnostics.push(Diagnostic {
            severity: note.severity,
            code: note.code,
            span: note.span,
            line,
            column,
            message: note.message,
        });
    }
    Ok(Compiled {
        module,
        diagnostics,
    })
}
