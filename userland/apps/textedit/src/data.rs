//! The Data display (`plans/TEXTEDIT.md` TE16, TE18): a picture's or a signed
//! manifest's structure — the records its container holds, every EXIF tag by
//! name, what a manifest requests and claims — then the whole file as a hex
//! dump, read-only beside the text and hex views.
//!
//! The editor parses neither. Its opening bytes are matched against the
//! picture formats' signatures (`tairix_image::sniff`) and the manifest magics
//! (`tairix_binfmt::detect`), bounded compares, or its name against the one
//! picture format that carries none, only to offer the display. The report is
//! read by a sandboxed worker of the editor's own (`run.rs`) — the picture
//! worker for a picture, the decode worker for a manifest — so a hostile file
//! is parsed in neither the editor nor the worker that colours text; the hex
//! rows are the document's own bytes.

use alloc::string::ToString;
use alloc::vec::Vec;

use tairix_binfmt::Format;
use tairix_browse::{media_for_name, MediaType};
use tairix_sandbox::decode::{self, MAX_INPUT};
use tairix_sandbox::imageinspect::InspectFailure;
use tairix_sandbox::imagerender::{ViewFormat, MAX_DOCUMENT_BYTES};
use tairix_sandbox::{Launcher, ParserSandbox};
use tairix_util::hexdump::BYTES_PER_ROW;
use tairix_util::report::ReportLine;

use crate::answer::{Answered, Unlisted};

/// What a document's structure is read as, and so which worker reads it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Structure {
    /// A picture, read by the picture worker — as the format its name gives
    /// it, for one whose content carries no signature: a sprite area.
    Picture(Option<ViewFormat>),
    /// A signed manifest, read by the decode worker.
    Manifest,
}

impl Structure {
    /// What the display says while the worker reads it.
    #[must_use]
    pub const fn reading(self) -> &'static str {
        match self {
            Self::Picture(_) => "Reading the picture's structure\u{2026}",
            Self::Manifest => "Reading the manifest\u{2026}",
        }
    }

    /// The document is past what one report may read.
    #[must_use]
    pub fn too_large(self) -> Unlisted {
        let (what, limit) = match self {
            Self::Picture(_) => ("a picture", MAX_DOCUMENT_BYTES),
            Self::Manifest => ("a manifest", MAX_INPUT),
        };
        Unlisted {
            reason: alloc::format!(
                "too large to read as {what}: over {} MiB",
                limit / (1024 * 1024)
            ),
            lasting: true,
        }
    }

    /// The allocator would not hold the document's bytes for a report.
    #[must_use]
    pub fn no_memory(self) -> Unlisted {
        let what = match self {
            Self::Picture(_) => "the picture's structure",
            Self::Manifest => "the manifest",
        };
        Unlisted {
            reason: alloc::format!("there is not enough memory to read {what}"),
            lasting: false,
        }
    }
}

/// What one row of the display is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DataRow<'a> {
    /// A line of the report.
    Report(&'a ReportLine),
    /// The line standing in for the report until it lands, or saying why
    /// it will not.
    Note(&'a str),
    /// The heading over the bytes: how many there are.
    Bytes,
    /// The hex row of the bytes starting at this offset.
    Hex(usize),
}

/// What the window shows of a document as data.
#[derive(Debug)]
pub struct DataDisplay {
    showing: bool,
    structure: Structure,
    report: Answered<Vec<ReportLine>>,
    /// The widest report line, in characters.
    widest: usize,
    /// The first row shown.
    top: usize,
}

impl DataDisplay {
    /// The display for a document called `name` opening with `head`, shown
    /// at once, or `None` when it is neither a picture nor a manifest.
    #[must_use]
    pub fn of(head: &[u8], name: &str) -> Option<Self> {
        let named =
            (media_for_name(name) == Some(MediaType::ImageSprite)).then_some(ViewFormat::Sprite);
        let structure = if named.is_some() || tairix_image::sniff(head).is_some() {
            Structure::Picture(named)
        } else if matches!(
            tairix_binfmt::detect(head),
            Some(Format::Manifest | Format::AppInfo)
        ) {
            Structure::Manifest
        } else {
            return None;
        };
        Some(Self {
            showing: true,
            structure,
            report: Answered::new(),
            widest: 0,
            top: 0,
        })
    }

    /// Whether the window shows its document as data.
    #[must_use]
    pub const fn showing(&self) -> bool {
        self.showing
    }

    /// Show the document as data, or stop, answering whether that changed.
    /// Shown afresh, a display refused for a passing reason asks again.
    pub fn show(&mut self, showing: bool) -> bool {
        let changed = self.showing != showing;
        self.showing = showing;
        if changed && showing {
            self.report.forget_passing();
        }
        changed
    }

    /// What the document's structure is read as.
    #[must_use]
    pub const fn structure(&self) -> Structure {
        self.structure
    }

    /// The id to ask for the report of the document at `generation` by, when
    /// one is wanted: the display shows, and what it holds is of another
    /// generation or nothing.
    pub fn want(&mut self, generation: u64) -> Option<u64> {
        if !self.showing {
            return None;
        }
        self.report.want(generation)
    }

    /// Take in the report asked as `id`, answering whether it was the one
    /// waited for.
    pub fn reported(
        &mut self,
        id: u64,
        report: Result<Vec<ReportLine>, Unlisted>,
        len: usize,
    ) -> bool {
        let widest = report.as_ref().map_or(0, |lines| {
            lines
                .iter()
                .map(|line| line.text.chars().count())
                .max()
                .unwrap_or(0)
        });
        let taken = self.report.take(id, report);
        if taken {
            self.widest = widest;
            self.top = self.top.min(self.row_count(len).saturating_sub(1));
        }
        taken
    }

    /// Refuse the report of the document at `generation` without asking the
    /// worker, as `unlisted` says.
    pub fn refuse(&mut self, unlisted: Unlisted, generation: u64) {
        self.report.refuse(unlisted, generation);
        self.widest = 0;
        self.top = 0;
    }

    /// The report's lines, empty until it lands.
    fn lines(&self) -> &[ReportLine] {
        self.report.answer().map_or(&[], Vec::as_slice)
    }

    /// The rows before the bytes: the report's lines, or the one line
    /// standing in for them.
    fn preface(&self) -> usize {
        self.lines().len().max(1)
    }

    /// How many rows the display scrolls through over a document of `len`
    /// bytes: the report, the heading over the bytes, and a row a sixteen.
    #[must_use]
    pub fn row_count(&self, len: usize) -> usize {
        self.preface() + 1 + len.div_ceil(BYTES_PER_ROW)
    }

    /// Row `at` of the display over a document of `len` bytes, or `None`
    /// past the last.
    #[must_use]
    pub fn row(&self, at: usize, len: usize) -> Option<DataRow<'_>> {
        if let Some(note) = self.report.note(self.structure.reading()) {
            if at == 0 {
                return Some(DataRow::Note(note));
            }
        } else if let Some(line) = self.lines().get(at) {
            return Some(DataRow::Report(line));
        }
        let past = at.checked_sub(self.preface())?;
        if past == 0 {
            return Some(DataRow::Bytes);
        }
        let offset = (past - 1).checked_mul(BYTES_PER_ROW)?;
        (offset < len).then_some(DataRow::Hex(offset))
    }

    /// The widest row, in columns, beside hex rows `hex_width` wide.
    #[must_use]
    pub fn widest(&self, hex_width: usize) -> usize {
        self.widest.max(hex_width)
    }

    /// The first row shown.
    #[must_use]
    pub const fn top(&self) -> usize {
        self.top
    }

    /// Show from row `top`, clamped to the rows of a `len`-byte document,
    /// answering whether the view moved.
    pub fn set_top(&mut self, top: usize, len: usize) -> bool {
        let top = top.min(self.row_count(len).saturating_sub(1));
        let moved = top != self.top;
        self.top = top;
        moved
    }

    /// Scroll by `rows`, up for a negative count, answering whether the view
    /// moved.
    pub fn scroll(&mut self, rows: isize, len: usize) -> bool {
        self.set_top(self.top.saturating_add_signed(rows), len)
    }
}

/// Why `failure` left no report: lasting, unless it said nothing of the
/// document.
#[must_use]
pub fn unreported(failure: InspectFailure) -> Unlisted {
    Unlisted {
        reason: failure.to_string(),
        lasting: !failure.passing(),
    }
}

/// The report of the manifest `bytes`, read through `sandbox`.
///
/// # Errors
///
/// Why there is none, in the words a decode's failure is stated in.
pub fn report_manifest<L: Launcher, S: tairix_log::Sink>(
    sandbox: &mut ParserSandbox<L, S>,
    bytes: &[u8],
) -> Result<Vec<ReportLine>, Unlisted> {
    if bytes.len() > MAX_INPUT {
        return Err(Structure::Manifest.too_large());
    }
    decode::file_report(sandbox, bytes).map_err(Unlisted::of)
}

#[cfg(test)]
#[path = "data_tests.rs"]
mod tests;
