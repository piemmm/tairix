//! The Code display (`plans/TEXTEDIT.md` TE14, TE18): what an executable's
//! headers say, then its instructions, listed read-only beside the text and
//! hex views.
//!
//! The editor parses no container. Its opening bytes are matched against the
//! container magics (`tairix_binfmt::detect`, a bounded compare) only to offer
//! the display. [`list_code`] runs on one of the editor's worker threads
//! (`run.rs`) and has every container and instruction it lists parsed by a
//! sandboxed decode worker of its own, so a hostile executable is parsed in
//! neither the editor nor the worker that colours text. The window keeps the
//! lines that come back, where it is scrolled to, and which instruction set
//! the user chose.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_sandbox::decode::{
    self, CodeWindow, DecodeFailure, Isa, Region, RegionKind, Summarise, SymbolRecord, MAX_INPUT,
    MAX_WINDOW_INSNS,
};
use tairix_sandbox::listing::{code_lines, failure_reason, CodeLine};
use tairix_sandbox::{Launcher, ParserSandbox, SandboxError};

use crate::answer::{Answered, Unlisted};

/// Most lines a listing holds: past it the listing stops and says so.
pub const MAX_LISTING_LINES: usize = 32 * 1024;

/// How long a listing may take before it stops and says so.
///
/// A containment bound: a decode worker that drags out every answer, each
/// inside its own reply deadline, holds the editor's code worker this long
/// rather than a deadline a window. A whole listing takes well under a second
/// on a desktop core.
pub const LISTING_BUDGET_NS: u64 = 30_000_000_000;

/// Code bytes one disassembly request decodes from: a whole window of the
/// longest x86-64 instructions, the longest of the native sets.
const WINDOW_BYTES: usize = MAX_WINDOW_INSNS * Isa::X86_64.longest_encoding();

/// What the display says while the worker lists the code.
pub const LISTING: &str = "Listing the code\u{2026}";

/// A listing the decode worker built.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeListing {
    /// The lines, top first.
    pub lines: Vec<CodeLine>,
    /// The instruction set the code was read as.
    pub isa: Isa,
    /// The widest line, in columns.
    pub widest: usize,
}

impl Unlisted {
    /// The document is past what one decode may carry.
    #[must_use]
    pub fn too_large() -> Self {
        Self {
            reason: format!(
                "too large to list as code: over {} MiB",
                MAX_INPUT / (1024 * 1024)
            ),
            lasting: true,
        }
    }

    /// The allocator would not hold the document's bytes for a listing.
    #[must_use]
    pub fn no_memory() -> Self {
        Self {
            reason: String::from("there is not enough memory to list the code"),
            lasting: false,
        }
    }

    /// Why `failure` left no listing or report: lasting, unless the decode
    /// worker could not start or memory ran short, which says nothing of the
    /// document.
    pub(crate) fn of(failure: DecodeFailure) -> Self {
        let passing = match failure {
            DecodeFailure::Sandbox(error) => {
                error.out_of_memory() || matches!(error, SandboxError::WorkerUnavailable(_))
            }
            DecodeFailure::NoMemory => true,
            DecodeFailure::Refused(_) | DecodeFailure::ReplyMalformed => false,
        };
        Self {
            reason: String::from(failure_reason(failure)),
            lasting: !passing,
        }
    }
}

/// Why a listing stopped short of the code.
enum Stop {
    Lines,
    Time,
}

/// List `image` through `sandbox`: what its headers say, then each code
/// region's instructions under its heading.
///
/// Read as the instruction set the user chose, else as the one the container
/// names, else as the `system`'s, which is what an `rxe` installed here is
/// written in. A listing that outgrows [`MAX_LISTING_LINES`], or outlasts
/// [`LISTING_BUDGET_NS`] by `clock`, stops and says so.
///
/// # Errors
///
/// Why there is no listing: the image is too large to decode, the container
/// is not one, no instruction set is known, or the worker failed.
pub fn list_code<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    image: &[u8],
    (chosen, system): (Option<Isa>, Option<Isa>),
    clock: &mut dyn FnMut() -> u64,
) -> Result<CodeListing, Unlisted> {
    if image.len() > MAX_INPUT {
        return Err(Unlisted::too_large());
    }
    let deadline = clock().saturating_add(LISTING_BUDGET_NS);
    let mut summary =
        decode::container_summary(sandbox, image, Summarise::WithReport).map_err(Unlisted::of)?;
    summary.symbols.sort_by_key(|symbol| symbol.addr);
    let isa = chosen.or(summary.isa).or(system).ok_or_else(|| Unlisted {
        reason: String::from("no instruction set is known to read this code as"),
        lasting: true,
    })?;
    let mut lines: Vec<CodeLine> = core::mem::take(&mut summary.report)
        .into_iter()
        .map(|line| CodeLine {
            text: line.text,
            is_label: line.heading,
        })
        .collect();
    if summary.regions_truncated {
        lines.push(plain(String::from(
            "  \u{2026} and more regions than are listed",
        )));
    }
    if summary.symbols_truncated {
        lines.push(plain(String::from(
            "  \u{2026} and more symbols than are named",
        )));
    }
    let mut stop = None;
    let mut code = summary
        .regions
        .iter()
        .filter(|region| region.kind == RegionKind::Code)
        .peekable();
    if code.peek().is_some() {
        lines.push(plain(String::new()));
        lines.push(label(format!("Instructions, read as {}", isa.name())));
    }
    for region in code {
        lines.push(plain(String::new()));
        lines.push(label(region_heading(region)));
        let Some(bytes) = region_bytes(image, region) else {
            lines.push(plain(String::from("  its bytes lie outside the file")));
            continue;
        };
        let mut out_of_time = || clock() >= deadline;
        stop = list_region(
            sandbox,
            (isa, &summary.symbols),
            (region, bytes),
            &mut lines,
            &mut out_of_time,
        )?;
        if stop.is_some() {
            break;
        }
    }
    if lines.len() > MAX_LISTING_LINES {
        lines.truncate(MAX_LISTING_LINES);
        stop = Some(Stop::Lines);
    }
    match stop {
        Some(Stop::Lines) => lines.push(plain(format!(
            "\u{2026} the listing stops here, at {MAX_LISTING_LINES} lines"
        ))),
        Some(Stop::Time) => lines.push(plain(format!(
            "\u{2026} the listing stops here, after {} seconds of decoding",
            LISTING_BUDGET_NS / 1_000_000_000
        ))),
        None => {}
    }
    let widest = lines
        .iter()
        .map(|line| line.text.chars().count())
        .max()
        .unwrap_or(0);
    Ok(CodeListing { lines, isa, widest })
}

/// Add `region`'s instructions, read as `isa` from its file `bytes`, to
/// `lines`, answering why the listing stops there, if it does.
fn list_region<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    (isa, symbols): (Isa, &[SymbolRecord]),
    (region, bytes): (&Region, &[u8]),
    lines: &mut Vec<CodeLine>,
    out_of_time: &mut dyn FnMut() -> bool,
) -> Result<Option<Stop>, Unlisted> {
    let end = region.code_end();
    let reach = WINDOW_BYTES + isa.longest_encoding();
    let (mut address, mut depth) = (region.addr, 0);
    while address < end {
        let room = MAX_LISTING_LINES.saturating_sub(lines.len());
        if room == 0 {
            return Ok(Some(Stop::Lines));
        }
        if out_of_time() {
            return Ok(Some(Stop::Time));
        }
        let at = usize::try_from(address - region.addr).unwrap_or(usize::MAX);
        let left = usize::try_from(end - address).unwrap_or(usize::MAX);
        let Some(rest) = bytes.get(at..).map(|rest| &rest[..rest.len().min(left)]) else {
            break;
        };
        let window = CodeWindow {
            address,
            depth,
            code: &rest[..rest.len().min(reach)],
            continues: rest.len() > reach,
        };
        let want = u32::try_from(room.min(MAX_WINDOW_INSNS)).unwrap_or(u32::MAX);
        let decoded = decode::disassemble(sandbox, isa, window, want).map_err(Unlisted::of)?;
        // A window that decoded nothing ends the region rather than asking
        // again for the same bytes.
        if decoded.insns.is_empty() {
            break;
        }
        lines.extend(code_lines(&decoded.insns, symbols, room));
        address = decoded.next_address;
        depth = decoded.next_depth;
    }
    Ok(None)
}

fn label(text: String) -> CodeLine {
    CodeLine {
        text,
        is_label: true,
    }
}

fn plain(text: String) -> CodeLine {
    CodeLine {
        text,
        is_label: false,
    }
}

/// The heading over a code region's instructions.
fn region_heading(region: &Region) -> String {
    if region.name.is_empty() {
        format!("Code at {:#x}:", region.addr)
    } else {
        format!("{} at {:#x}:", region.name, region.addr)
    }
}

/// The file bytes of `region`, or `None` when they lie outside the image.
fn region_bytes<'a>(image: &'a [u8], region: &Region) -> Option<&'a [u8]> {
    let start = usize::try_from(region.file_offset).ok()?;
    let len = usize::try_from(region.file_size).ok()?;
    image.get(start..start.checked_add(len)?)
}

/// What the window shows of an executable as code.
#[derive(Debug)]
pub struct CodeDisplay {
    showing: bool,
    /// The instruction set the user chose, when they chose one.
    choice: Option<Isa>,
    listing: Answered<CodeListing>,
    /// The first line shown.
    top: usize,
}

impl CodeDisplay {
    /// The display for a document opening with `head`, shown at once, or
    /// `None` when `head` names no container of code.
    #[must_use]
    pub fn of(head: &[u8]) -> Option<Self> {
        let format = tairix_binfmt::detect(head)?;
        format.holds_code().then(|| Self {
            showing: true,
            choice: None,
            listing: Answered::new(),
            top: 0,
        })
    }

    /// Whether the window shows its document as code.
    #[must_use]
    pub const fn showing(&self) -> bool {
        self.showing
    }

    /// Show the document as code, or stop, answering whether that changed.
    /// Shown afresh, a display refused for a passing reason asks again.
    pub fn show(&mut self, showing: bool) -> bool {
        let changed = self.showing != showing;
        self.showing = showing;
        if changed && showing {
            self.listing.forget_passing();
        }
        changed
    }

    /// The instruction set the user chose, if they did.
    #[must_use]
    pub const fn choice(&self) -> Option<Isa> {
        self.choice
    }

    /// Read the code as `isa` from now on, answering whether the display
    /// changed: code not already listed as `isa` is listed again.
    pub fn choose(&mut self, isa: Isa) -> bool {
        let unchanged = self.choice == Some(isa) || self.isa() == Some(isa);
        self.choice = Some(isa);
        if unchanged {
            return false;
        }
        self.listing.forget();
        self.top = 0;
        true
    }

    /// The listing to ask for of the document at `generation`, when one is
    /// wanted: the display shows, and what it holds is of another generation
    /// or nothing. Answers its id and the chosen instruction set, and records
    /// that it was asked.
    pub fn want(&mut self, generation: u64) -> Option<(u64, Option<Isa>)> {
        if !self.showing {
            return None;
        }
        self.listing.want(generation).map(|id| (id, self.choice))
    }

    /// Take in the listing asked as `id`, answering whether it was the one
    /// waited for: an answer to an ask since replaced is let go.
    pub fn listed(&mut self, id: u64, listing: Result<CodeListing, Unlisted>) -> bool {
        let taken = self.listing.take(id, listing);
        self.top = self.top.min(self.line_count().saturating_sub(1));
        taken
    }

    /// Refuse the listing of the document at `generation` without asking the
    /// worker, as `unlisted` says.
    pub fn refuse(&mut self, unlisted: Unlisted, generation: u64) {
        self.listing.refuse(unlisted, generation);
        self.top = 0;
    }

    /// The lines listed, empty until a listing lands.
    #[must_use]
    pub fn lines(&self) -> &[CodeLine] {
        self.listing
            .answer()
            .map_or(&[], |listing| listing.lines.as_slice())
    }

    /// How many lines the display scrolls through: the listing's, or the one
    /// line saying why there is none yet.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.lines().len().max(1)
    }

    /// The widest listed line, in columns.
    #[must_use]
    pub fn widest(&self) -> usize {
        self.listing.answer().map_or(0, |listing| listing.widest)
    }

    /// What the display says in place of a listing, while it has none.
    #[must_use]
    pub fn note(&self) -> Option<&str> {
        self.listing.note(LISTING)
    }

    /// The instruction set the listing was read as, once it is listed.
    #[must_use]
    pub fn isa(&self) -> Option<Isa> {
        self.listing.answer().map(|listing| listing.isa)
    }

    /// The first line shown.
    #[must_use]
    pub const fn top(&self) -> usize {
        self.top
    }

    /// Show from line `top`, clamped to the listing, answering whether the
    /// view moved.
    pub fn set_top(&mut self, top: usize) -> bool {
        let top = top.min(self.line_count().saturating_sub(1));
        let moved = top != self.top;
        self.top = top;
        moved
    }

    /// Scroll by `lines`, up for a negative count, answering whether the view
    /// moved.
    pub fn scroll(&mut self, lines: isize) -> bool {
        self.set_top(self.top.saturating_add_signed(lines))
    }
}

#[cfg(test)]
#[path = "code_tests.rs"]
mod tests;
