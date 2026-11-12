//! The settings `EDIT` keeps between runs, in `EDIT.INI` beside the
//! program: a line a setting, `name=value`, the names in any case. The
//! file is read as untrusted text: a line it cannot take is passed over, a
//! value out of range keeps the setting as it was, and only the first
//! [`LEN`] bytes are read at all.

use super::ui::Settings;
use alloc::vec::Vec;

/// The most of the file read.
pub const LEN: usize = 256;

/// The file's name, beside the program's.
pub const FILE: &[u8] = b"EDIT.INI";

const COLOURS: &[u8] = b"Colors";
const BARS: &[u8] = b"ScrollBars";
const TABS: &[u8] = b"TabStops";

/// `value` as a number in `radix`, if it is one that fits a byte.
fn number(value: &[u8], radix: u32) -> Option<u8> {
    let digits = core::str::from_utf8(value).ok()?.trim();
    u8::from_str_radix(digits, radix).ok()
}

/// The settings `text` gives over `settings`.
#[must_use]
pub fn parse(text: &[u8], settings: Settings) -> Settings {
    let mut out = settings;
    for line in text[..text.len().min(LEN)].split(|&byte| byte == b'\n' || byte == b'\r') {
        let Some(at) = line.iter().position(|&byte| byte == b'=') else {
            continue;
        };
        let (name, value) = (&line[..at], &line[at + 1..]);
        let name = name.trim_ascii();
        if name.eq_ignore_ascii_case(COLOURS) {
            if let Some(colour) = number(value, 16).filter(|colour| colour & 0x80 == 0) {
                out.colour = colour;
            }
        } else if name.eq_ignore_ascii_case(BARS) {
            if let Some(bars) = number(value, 10).filter(|&bars| bars <= 1) {
                out.bars = bars == 1;
            }
        } else if name.eq_ignore_ascii_case(TABS) {
            if let Some(tab) = number(value, 10).filter(|tab| (1..=99).contains(tab)) {
                out.tab = tab;
            }
        }
    }
    out
}

/// The file's text for `settings`.
#[must_use]
pub fn compose(settings: Settings) -> Vec<u8> {
    alloc::format!(
        "[EDIT]\r\nColors={:02X}\r\nScrollBars={}\r\nTabStops={}\r\n",
        settings.colour,
        u8::from(settings.bars),
        settings.tab
    )
    .into_bytes()
}

/// The settings file's path: the program's own `path` with its name
/// changed to [`FILE`]; none where that would not fit `room` bytes.
#[must_use]
pub fn beside(path: &[u8], room: usize) -> Option<Vec<u8>> {
    let directory = path
        .iter()
        .rposition(|&byte| byte == b'\\' || byte == b':')
        .map_or(0, |at| at + 1);
    let out = [&path[..directory], FILE].concat();
    (out.len() < room).then_some(out)
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
