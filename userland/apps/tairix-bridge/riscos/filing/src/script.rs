//! Scripts of command lines (`plans/RISCOS-EMULATOR.md` §5): an Obey file,
//! `*Repeat`'s walk and the boot sequence, each run a line at a time through
//! `OS_CLI`.
//!
//! While a script runs, FileSwitch's resume routine is the exit handler, so
//! a program one of its lines starts hands the script back its turn when it
//! exits, as RISC OS 5's Obey and Repeat take theirs back. A script's state
//! is held by its context, not the Rust stack the program's start unwound,
//! and scripts nest, each its depth's. A program that ends in an error ends
//! every script it was started from, as on RISC OS 5.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::any::Any;

use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::{self as swi, environment};
use tairix_riscos_kernel::swi::X;

/// The routine FileSwitch holds for resuming a script.
pub(crate) const RESUME: usize = 0;

/// Where a script's lines come from.
pub(crate) enum Source {
    /// An Obey file, read as the Obey module reads one.
    Obey(crate::obey::File),
    /// The boot sequence's lines.
    Boot(crate::boot::Sequence),
    /// Lines made beforehand.
    Made(VecDeque<Vec<u8>>),
}

impl Source {
    /// The next line, none after the last.
    fn next(&mut self, os: &mut Os<'_>) -> Result<Option<Vec<u8>>, Fail> {
        match self {
            Self::Obey(file) => file.next(os),
            Self::Boot(sequence) => sequence.next(os),
            Self::Made(lines) => Ok(lines.pop_front()),
        }
    }

    /// What it holds let go: an Obey file's open file closed.
    fn close(self, os: &mut Os<'_>) -> Result<(), Fail> {
        match self {
            Self::Obey(file) => file.close(os),
            Self::Boot(_) | Self::Made(_) => Ok(()),
        }
    }
}

/// What a script does with a line's error.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OnError {
    /// It ends, with the error.
    Stop,
    /// It keeps the error as a failure and goes on.
    Report,
}

/// A line a reporting script could not run, and the error it gave.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Failure {
    /// The line.
    pub line: Vec<u8>,
    /// The error's number.
    pub number: u32,
    /// The error's text.
    pub text: Vec<u8>,
}

/// The most failures a report keeps: a containment bound on what a boot of
/// a hostile disc may hold, the rest counted.
const FAILURES_MOST: usize = 64;

/// What reporting scripts met: each line that failed, in the order met, as
/// many as a report keeps, and how many more there were.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Report {
    /// The failures kept.
    pub failures: Vec<Failure>,
    /// How many more there were.
    pub omitted: u32,
}

impl Report {
    /// `failure` kept, or counted past the bound: whether there was room to
    /// keep it.
    fn keep(&mut self, failure: Failure) -> bool {
        if self.failures.len() >= FAILURES_MOST {
            self.omitted = self.omitted.saturating_add(1);
            return true;
        }
        if self.failures.try_reserve(1).is_err() {
            return false;
        }
        self.failures.push(failure);
        true
    }

    /// `later`'s failures after these, within the bound.
    fn absorb(&mut self, later: Self) {
        for failure in later.failures {
            if !self.keep(failure) {
                self.omitted = self.omitted.saturating_add(1);
            }
        }
        self.omitted = self.omitted.saturating_add(later.omitted);
    }
}

/// A script under way.
struct Script {
    source: Source,
    on_error: OnError,
    report: Report,
    /// The exit handler it stands in front of: its code, workspace and
    /// buffer.
    replaced: [u32; 3],
}

/// The scripts under way, the innermost last: a script's depth is the
/// workspace its exit handler is given.
#[derive(Default)]
pub(crate) struct Scripts {
    stack: Vec<Script>,
    /// What reporting scripts that ended after a program they started met,
    /// for their owner to take.
    reported: Report,
}

/// `source` run as a script, its errors handled as `on_error` says: what a
/// reporting script met.
///
/// # Errors
///
/// A stopping script's error, or the unwind a program one of its lines
/// starts leaves by, after which the script goes on when the program exits.
pub(crate) fn run(os: &mut Os<'_>, source: Source, on_error: OnError) -> Result<Report, Fail> {
    let routine = routine(os)?;
    let replaced = exit_handler(os, [0; 3])?;
    let depth = push(os, routine, replaced, source, on_error)?;
    exit_handler(os, [routine, depth, 0])?;
    let ran = run_lines(os, depth);
    if let Err(Fail::Unwind(unwind)) = ran {
        return Err(Fail::Unwind(unwind));
    }
    let report = finish(os, depth);
    ran.and(report)
}

/// FileSwitch's resume routine, the exit handler while a script runs: the
/// script R12 names goes on from its next line, then ends as the program it
/// now is ends — through the exit handler it stood in front of, or its error.
pub(crate) fn resumed(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let depth = regs.r[12];
    let at = usize::try_from(depth).unwrap_or(usize::MAX);
    let (owned, ended) = with(os, |scripts| {
        let owned = (1..=scripts.stack.len()).contains(&at);
        let kept = if owned { at } else { 0 };
        (owned, scripts.stack.split_off(kept))
    })?;
    close_all(os, ended);
    if !owned {
        return end_unowned(os);
    }
    let ran = run_lines(os, depth);
    if let Err(Fail::Unwind(unwind)) = ran {
        return Err(Fail::Unwind(unwind));
    }
    let report = finish(os, depth);
    match ran.and(report) {
        Ok(report) => {
            with(os, |scripts| scripts.reported.absorb(report))?;
            exit(os)
        }
        Err(Fail::Error(block)) => raise(os, block),
        Err(unwind) => Err(unwind),
    }
}

/// What reporting scripts that ended after a program they started met,
/// taken.
pub(crate) fn take_reported(os: &mut Os<'_>) -> Report {
    os.extension::<Scripts>()
        .map(|scripts| core::mem::take(&mut scripts.reported))
        .unwrap_or_default()
}

/// The routine scripts are resumed through.
fn routine(os: &mut Os<'_>) -> Result<u32, Fail> {
    match os.routine_of(&crate::fileswitch::FILE_SWITCH, RESUME) {
        Some(at) => Ok(at),
        None => Err(os.error(tairix_riscos_kernel::filing::NOT_PRESENT)),
    }
}

/// The exit handler set to `handler` — its code, workspace and buffer — or
/// read where its code is zero: the one it replaced.
fn exit_handler(os: &mut Os<'_>, handler: [u32; 3]) -> Result<[u32; 3], Fail> {
    let mut regs = Regs::default();
    regs.r[0] = environment::EXIT;
    regs.r[1..4].copy_from_slice(&handler);
    os.perform(X | swi::CHANGE_ENVIRONMENT, &mut regs)?;
    Ok([regs.r[1], regs.r[2], regs.r[3]])
}

/// `work` on the current context's scripts, held from its first one on:
/// they go on through its own exit handler.
fn with<T>(os: &mut Os<'_>, work: impl FnOnce(&mut Scripts) -> T) -> Result<T, Fail> {
    if os.extension::<Scripts>().is_none() {
        os.hold_context_extension(Scripts::default(), ended)?;
    }
    match os.extension::<Scripts>() {
        Some(scripts) => Ok(work(scripts)),
        None => Err(os.no_memory()),
    }
}

/// The script pushed, standing in front of `replaced`: its depth. Scripts
/// the exit handler no longer leads to — ended with a program that failed —
/// are let go first.
fn push(
    os: &mut Os<'_>,
    routine: u32,
    replaced: [u32; 3],
    source: Source,
    on_error: OnError,
) -> Result<u32, Fail> {
    let live = if replaced[0] == routine {
        usize::try_from(replaced[1]).unwrap_or(usize::MAX)
    } else {
        0
    };
    let stale = with(os, |scripts| {
        let kept = live.min(scripts.stack.len());
        scripts.stack.split_off(kept)
    })?;
    close_all(os, stale);
    let pushed = with(os, |scripts| {
        if scripts.stack.try_reserve(1).is_err() {
            return Err(source);
        }
        scripts.stack.push(Script {
            source,
            on_error,
            report: Report::default(),
            replaced,
        });
        Ok(scripts.stack.len())
    })?;
    match pushed {
        Ok(depth) => u32::try_from(depth).map_err(|_| os.no_memory()),
        Err(source) => {
            // The refusal to hold it is the one to report.
            let _ = source.close(os);
            Err(os.no_memory())
        }
    }
}

/// Each line of the script at `depth` run in turn, its errors as the script
/// says.
fn run_lines(os: &mut Os<'_>, depth: u32) -> Result<(), Fail> {
    loop {
        let Some(line) = next_line(os, depth)? else {
            return Ok(());
        };
        match os.perform_line(&line) {
            Ok(()) => {}
            Err(Fail::Error(block)) => {
                let at = usize::try_from(depth).unwrap_or(usize::MAX).wrapping_sub(1);
                let reporting = with(os, |scripts| {
                    scripts
                        .stack
                        .get(at)
                        .is_some_and(|script| script.on_error == OnError::Report)
                })?;
                if !reporting {
                    return Err(Fail::Error(block));
                }
                let (number, text) = os.read_error(block)?;
                let failure = Failure { line, number, text };
                let kept = with(os, |scripts| {
                    scripts
                        .stack
                        .get_mut(at)
                        .is_some_and(|script| script.report.keep(failure))
                })?;
                if !kept {
                    return Err(Fail::Error(block));
                }
            }
            Err(unwind) => return Err(unwind),
        }
    }
}

/// The script at `depth`'s next line, read with the script held aside so
/// its source can use the session while the script stays where it is.
fn next_line(os: &mut Os<'_>, depth: u32) -> Result<Option<Vec<u8>>, Fail> {
    let at = usize::try_from(depth).unwrap_or(usize::MAX);
    let taken = with(os, |scripts| {
        (scripts.stack.len() == at)
            .then(|| scripts.stack.pop())
            .flatten()
    })?;
    let Some(mut script) = taken else {
        return Ok(None);
    };
    let line = script.source.next(os);
    with(os, |scripts| scripts.stack.push(script))?;
    line
}

/// The script at `depth` ended: let go, the exit handler it stood in front
/// of back, what it met.
fn finish(os: &mut Os<'_>, depth: u32) -> Result<Report, Fail> {
    let at = usize::try_from(depth).unwrap_or(usize::MAX);
    let ended = with(os, |scripts| {
        (scripts.stack.len() == at)
            .then(|| scripts.stack.pop())
            .flatten()
    })?;
    let Some(script) = ended else {
        return Ok(Report::default());
    };
    let restored = exit_handler(os, script.replaced);
    let closed = script.source.close(os);
    restored.and(closed).map(|()| script.report)
}

/// Scripts that will not go on, let go.
fn close_all(os: &mut Os<'_>, scripts: Vec<Script>) {
    for script in scripts.into_iter().rev() {
        // A script that will not go on has no one to tell of its file's close.
        let _ = script.source.close(os);
    }
}

/// A context's scripts at its end: each one's file closed.
fn ended(os: &mut Os<'_>, value: Box<dyn Any>) {
    if let Ok(scripts) = value.downcast::<Scripts>() {
        close_all(os, scripts.stack);
    }
}

/// The program over, as it ends through the exit handler.
pub(crate) fn exit(os: &mut Os<'_>) -> Result<(), Fail> {
    let mut regs = Regs::default();
    os.perform(swi::EXIT, &mut regs)
}

/// The error at `block` the program's, raised to its error handler.
pub(crate) fn raise(os: &mut Os<'_>, block: u32) -> Result<(), Fail> {
    let mut regs = Regs::default();
    regs.r[0] = block;
    os.perform(swi::GENERATE_ERROR, &mut regs)
}

/// A resume no script owns: the exit handler's default back, and the
/// program over through it.
fn end_unowned(os: &mut Os<'_>) -> Result<(), Fail> {
    let mut regs = Regs::default();
    regs.r[0] = environment::EXIT;
    os.perform(X | swi::READ_DEFAULT_HANDLER, &mut regs)?;
    exit_handler(os, [regs.r[1], regs.r[2], regs.r[3]])?;
    exit(os)
}
