//! What `play` says about a playback: the `stdinfo` records on fd 3, and the
//! lines on standard error.
//!
//! Each record is advisory: none of it changes standard output, the exit
//! status, or a pipeline.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_abi::stdinfo::{Human, JsonStr, Severity, StdInfoKind, StdInfoRecord};
use tairix_sound::SoundInfo;

use tairix_player::Span;
use tairix_player::{Outcome, Skip, Status};
use tairix_util::quote::quote_subject;

/// The command word every record and line names.
pub const PRODUCER: &str = "play";

/// `info`'s format, rate, channels and length as people read them.
#[must_use]
pub fn describe(info: &SoundInfo) -> String {
    let mut text = tairix_player::describe(info);
    if let Some(frames) = info.frames {
        let _ = write!(
            text,
            ", {}",
            Span::of_frames(frames, info.rate.hz()).clock()
        );
    }
    text
}

/// `file` as a line leads with it ([`quote_subject`]), a chained file's later
/// links by their place, counted from one as people count.
#[must_use]
pub fn naming(file: &str, link: u32) -> String {
    let file = quote_subject(file);
    if link == 0 {
        format!("{file}")
    } else {
        format!("{file} (link {})", u64::from(link) + 1)
    }
}

/// The `schema` record for link `link` of `file`, about to be played as
/// `info` describes.
#[must_use]
pub fn opened(file: &str, link: u32, info: &SoundInfo) -> Vec<u8> {
    let human = format!("Playing {}: {}.", naming(file, link), describe(info));
    let mut ai = format!(
        "{{\"subject\":\"playback\",\"file\":{},\"link\":{link},\"format\":\"{}\",\"encoding\":\"{}\",",
        JsonStr(file),
        info.format.token(),
        info.encoding.token()
    );
    if let Some(bits) = info.encoding.bits() {
        let _ = write!(ai, "\"bits\":{bits},");
    }
    let _ = write!(
        ai,
        "\"rate_hz\":{},\"channels\":{},\"frames\":",
        info.rate.hz(),
        info.channels.channels()
    );
    match info.frames {
        Some(frames) => {
            let _ = write!(ai, "{frames}");
        }
        None => ai.push_str("null"),
    }
    ai.push('}');
    frame(StdInfoKind::Schema, "audio.playing", &human, &ai)
}

/// The `omission` record for `file`, left out or cut short because of `why`.
#[must_use]
pub fn left_out(file: &str, why: Skip, whole: bool) -> Vec<u8> {
    let (code, human) = if whole {
        (
            "audio.file_skipped",
            format!("{} was not played: {why}.", quote_subject(file)),
        )
    } else {
        (
            "audio.file_cut_short",
            format!("{} stopped early: {why}.", quote_subject(file)),
        )
    };
    let ai = format!(
        "{{\"subject\":\"playlist\",\"omission\":{{\"reason\":\"{}\",\"file\":{},\"whole_file\":{whole}}}}}",
        why.token(),
        JsonStr(file)
    );
    frame(StdInfoKind::Omission, code, &human, &ai)
}

/// The `summary` record for a playback that ended with `outcome`.
#[must_use]
pub fn summary(status: &Status, rate_hz: Option<u32>, outcome: Outcome) -> Vec<u8> {
    let heard = rate_hz.map_or(Span::ZERO, |hz| Span::of_frames(status.heard_frames, hz));
    let ending = match outcome {
        Outcome::Played => "played",
        Outcome::Stopped => "stopped",
        Outcome::Failed(_) => "failed",
    };
    let human = format!(
        "Played {}; {} underrun{}.",
        heard.clock(),
        status.underruns,
        if status.underruns == 1 { "" } else { "s" }
    );
    let ai = format!(
        "{{\"subject\":\"playback\",\"outcome\":\"{ending}\",\"frames_heard\":{},\"underruns\":{},\"lost_frames\":{}}}",
        status.heard_frames, status.underruns, status.lost_frames
    );
    frame(StdInfoKind::Summary, "audio.playback_summary", &human, &ai)
}

/// One framed record, or nothing when it cannot be framed: fd 3 is advisory
/// and never worth a malformed line.
fn frame(kind: StdInfoKind, code: &str, message: &str, ai: &str) -> Vec<u8> {
    let record = StdInfoRecord::new(
        PRODUCER,
        kind,
        code,
        Severity::Info,
        Human::message(message),
    )
    .with_ai(ai);
    let mut line = alloc::vec![0u8; 256 + message.len() + ai.len()];
    match record.write_jsonl(&mut line) {
        Ok(len) => {
            line.truncate(len);
            line
        }
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
