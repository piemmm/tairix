//! IBM code page 437 — the IBM PC's character set — and Unicode, read as text
//! or as the glyphs a PC's screen draws.
//!
//! The upper half, `80h`–`FFh`, is Unicode's mapping
//! (`MAPPINGS/VENDORS/MICSFT/PC/CP437.TXT`) both ways. Below it, text is ASCII
//! with its controls, as zip spells a member's name; the screen draws
//! `01h`–`1Fh` and `7Fh` as the faces, arrows and suits of IBM's character
//! generator, and `00h` as nothing.

#![no_std]
#![forbid(unsafe_code)]

/// What `00h`–`1Fh` draw: nothing, then IBM's glyphs.
const LOW: [char; 32] = [
    ' ', '\u{263A}', '\u{263B}', '\u{2665}', '\u{2666}', '\u{2663}', '\u{2660}', '\u{2022}',
    '\u{25D8}', '\u{25CB}', '\u{25D9}', '\u{2642}', '\u{2640}', '\u{266A}', '\u{266B}', '\u{263C}',
    '\u{25BA}', '\u{25C4}', '\u{2195}', '\u{203C}', '\u{00B6}', '\u{00A7}', '\u{25AC}', '\u{21A8}',
    '\u{2191}', '\u{2193}', '\u{2192}', '\u{2190}', '\u{221F}', '\u{2194}', '\u{25B2}', '\u{25BC}',
];

/// What `7Fh` draws.
const DELETE: char = '\u{2302}';

/// `80h`–`FFh`, as text and as drawn.
const HIGH: [char; 128] = [
    '\u{00C7}', '\u{00FC}', '\u{00E9}', '\u{00E2}', '\u{00E4}', '\u{00E0}', '\u{00E5}', '\u{00E7}',
    '\u{00EA}', '\u{00EB}', '\u{00E8}', '\u{00EF}', '\u{00EE}', '\u{00EC}', '\u{00C4}', '\u{00C5}',
    '\u{00C9}', '\u{00E6}', '\u{00C6}', '\u{00F4}', '\u{00F6}', '\u{00F2}', '\u{00FB}', '\u{00F9}',
    '\u{00FF}', '\u{00D6}', '\u{00DC}', '\u{00A2}', '\u{00A3}', '\u{00A5}', '\u{20A7}', '\u{0192}',
    '\u{00E1}', '\u{00ED}', '\u{00F3}', '\u{00FA}', '\u{00F1}', '\u{00D1}', '\u{00AA}', '\u{00BA}',
    '\u{00BF}', '\u{2310}', '\u{00AC}', '\u{00BD}', '\u{00BC}', '\u{00A1}', '\u{00AB}', '\u{00BB}',
    '\u{2591}', '\u{2592}', '\u{2593}', '\u{2502}', '\u{2524}', '\u{2561}', '\u{2562}', '\u{2556}',
    '\u{2555}', '\u{2563}', '\u{2551}', '\u{2557}', '\u{255D}', '\u{255C}', '\u{255B}', '\u{2510}',
    '\u{2514}', '\u{2534}', '\u{252C}', '\u{251C}', '\u{2500}', '\u{253C}', '\u{255E}', '\u{255F}',
    '\u{255A}', '\u{2554}', '\u{2569}', '\u{2566}', '\u{2560}', '\u{2550}', '\u{256C}', '\u{2567}',
    '\u{2568}', '\u{2564}', '\u{2565}', '\u{2559}', '\u{2558}', '\u{2552}', '\u{2553}', '\u{256B}',
    '\u{256A}', '\u{2518}', '\u{250C}', '\u{2588}', '\u{2584}', '\u{258C}', '\u{2590}', '\u{2580}',
    '\u{03B1}', '\u{00DF}', '\u{0393}', '\u{03C0}', '\u{03A3}', '\u{03C3}', '\u{00B5}', '\u{03C4}',
    '\u{03A6}', '\u{0398}', '\u{03A9}', '\u{03B4}', '\u{221E}', '\u{03C6}', '\u{03B5}', '\u{2229}',
    '\u{2261}', '\u{00B1}', '\u{2265}', '\u{2264}', '\u{2320}', '\u{2321}', '\u{00F7}', '\u{2248}',
    '\u{00B0}', '\u{2219}', '\u{00B7}', '\u{221A}', '\u{207F}', '\u{00B2}', '\u{25A0}', '\u{00A0}',
];

/// The bytes past printable ASCII that draw a character of their own:
/// `01h`–`1Fh`, `7Fh` and the upper half.
const DRAWN: usize = 31 + 1 + HIGH.len();

/// What each of those bytes draws, with the byte, in character order.
const REVERSE: [(char, u8); DRAWN] = reverse();

const fn reverse() -> [(char, u8); DRAWN] {
    let mut table = [('\0', 0u8); DRAWN];
    let mut len = 0;
    let mut byte = 1u8;
    loop {
        if !matches!(byte, 0x20..=0x7E) {
            let ch = glyph_of(byte);
            let mut at = len;
            while at > 0 && table[at - 1].0 as u32 > ch as u32 {
                table[at] = table[at - 1];
                at -= 1;
            }
            table[at] = (ch, byte);
            len += 1;
        }
        if byte == u8::MAX {
            return table;
        }
        byte += 1;
    }
}

/// The character `byte` spells as text: ASCII below `80h`, its controls
/// included, and the code page's upper half.
#[must_use]
pub const fn char_of(byte: u8) -> char {
    if byte < 0x80 {
        byte as char
    } else {
        HIGH[(byte - 0x80) as usize]
    }
}

/// The character `byte` draws on a PC's screen. `00h` draws nothing, so it
/// reads as a space.
#[must_use]
pub const fn glyph_of(byte: u8) -> char {
    match byte {
        0x00..=0x1F => LOW[byte as usize],
        0x7F => DELETE,
        _ => char_of(byte),
    }
}

/// The byte past printable ASCII that draws `ch`.
fn drawing(ch: char) -> Option<u8> {
    let at = REVERSE
        .binary_search_by_key(&ch, |&(drawn, _)| drawn)
        .ok()?;
    REVERSE.get(at).map(|&(_, byte)| byte)
}

/// The byte that spells `ch` as text, if one does.
#[must_use]
pub fn byte_of(ch: char) -> Option<u8> {
    if ch.is_ascii() {
        return u8::try_from(ch).ok();
    }
    drawing(ch).filter(|&byte| byte >= 0x80)
}

/// The byte that draws `ch` on a PC's screen, if one does; none for a
/// control, which draws nothing.
#[must_use]
pub fn glyph_byte_of(ch: char) -> Option<u8> {
    if matches!(ch, ' '..='~') {
        return u8::try_from(ch).ok();
    }
    drawing(ch)
}

#[cfg(test)]
mod tests;
