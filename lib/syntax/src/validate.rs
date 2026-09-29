//! What the system would make of a settings store, reported by the parser
//! the system reads it with.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use tairix_util::conf::Located;

use crate::Format;

/// Most diagnostics [`validate`] answers: a strict store's one refusal, or a
/// warning for each line an application settings document ignores, of which
/// it holds at most this many.
pub const MAX_DIAGNOSTICS: usize = tairix_appconf::MAX_LINES;

/// The longest document any store reads: a longer one is refused by its own
/// parser.
pub const MAX_STORE_LEN: usize = {
    let mut most = 0;
    let mut at = 0;
    while at < Format::COUNT {
        if let Some(len) = Format::ALL[at].store_len() {
            if len > most {
                most = len;
            }
        }
        at += 1;
    }
    most
};

/// How much a diagnostic matters.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Severity {
    /// The system refuses the document.
    Error,
    /// The system reads the document but ignores part of it.
    Warning,
}

/// One thing the system would say about a document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    /// The 1-based line it concerns, or `None` for the whole document.
    pub line: Option<u32>,
    /// How much it matters.
    pub severity: Severity,
    /// What it is, in the system's own words.
    pub message: String,
}

/// Validate `text` as a store of `format`: the first refusal of a strict
/// store with its line, and every line a tolerant one ignores. A format
/// that is not a store answers nothing.
#[must_use]
pub fn validate(format: Format, text: &[u8]) -> Vec<Diagnostic> {
    if !format.is_store() {
        return Vec::new();
    }
    let text = match core::str::from_utf8(text) {
        Ok(text) => text,
        Err(err) => {
            let line = line_of(text, err.valid_up_to());
            return alloc::vec![Diagnostic {
                line: Some(line),
                severity: Severity::Error,
                message: String::from("the document is not UTF-8 text"),
            }];
        }
    };
    match format {
        Format::SystemConfig => refusal(tairix_sysconfig::SystemConfig::parse(text).err()),
        Format::NetworkConfig => refusal(tairix_netconfig::NetworkConfig::parse(text).err()),
        Format::ServiceOverrides => refusal(tairix_enrolment::EnrolmentOverride::parse(text).err()),
        Format::UsersDb => refusal(tairix_users::UsersDb::parse(text).err()),
        Format::GroupsDb => refusal(tairix_users::GroupsDb::parse(text).err()),
        Format::FontFamily => refusal(tairix_fontface::check_manifest(text).err()),
        Format::AppSettings => match tairix_appconf::Document::parse(text) {
            Err(refused) => refusal(Some(Located::whole(refused))),
            Ok(document) => document
                .unparsed()
                .map(|line| Diagnostic {
                    line: Some(saturate(line.line)),
                    severity: Severity::Warning,
                    message: format!("this line is ignored: {}", line.reason),
                })
                .collect(),
        },
        Format::ProgramLibrary => match tairix_appconf::Document::parse(text) {
            Err(refused) => refusal(Some(Located::whole(refused))),
            Ok(document) => refusal(tairix_proglib::load(&document).err()),
        },
        _ => Vec::new(),
    }
}

/// The one diagnostic a strict store's refusal is, or none when it read.
fn refusal<E: fmt::Display>(refused: Option<Located<E>>) -> Vec<Diagnostic> {
    refused
        .map(|refused| Diagnostic {
            line: refused.line.map(saturate),
            severity: Severity::Error,
            message: refused.kind.to_string(),
        })
        .into_iter()
        .collect()
}

/// The 1-based line of byte `offset` in `text`.
fn line_of(text: &[u8], offset: usize) -> u32 {
    saturate(
        text[..offset.min(text.len())]
            .split(|&b| b == b'\n')
            .count(),
    )
}

/// A line number held within the wire's width.
fn saturate(line: usize) -> u32 {
    u32::try_from(line).unwrap_or(u32::MAX)
}
