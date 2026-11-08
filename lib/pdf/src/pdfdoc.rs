//! `PDFDocEncoding` (ISO 32000-2 Annex D.2): the single-byte encoding of a
//! PDF's text strings and of its older security handler's passwords.

use alloc::string::String;

use crate::PdfError;

/// The codes whose characters are not their own code point: the spacing
/// accents, the punctuation, ligatures and letters from 0x80, and the euro.
const REMAPPED: [(u8, char); 40] = [
    (0x18, '\u{2d8}'),
    (0x19, '\u{2c7}'),
    (0x1a, '\u{2c6}'),
    (0x1b, '\u{2d9}'),
    (0x1c, '\u{2dd}'),
    (0x1d, '\u{2db}'),
    (0x1e, '\u{2da}'),
    (0x1f, '\u{2dc}'),
    (0x80, '\u{2022}'),
    (0x81, '\u{2020}'),
    (0x82, '\u{2021}'),
    (0x83, '\u{2026}'),
    (0x84, '\u{2014}'),
    (0x85, '\u{2013}'),
    (0x86, '\u{192}'),
    (0x87, '\u{2044}'),
    (0x88, '\u{2039}'),
    (0x89, '\u{203a}'),
    (0x8a, '\u{2212}'),
    (0x8b, '\u{2030}'),
    (0x8c, '\u{201e}'),
    (0x8d, '\u{201c}'),
    (0x8e, '\u{201d}'),
    (0x8f, '\u{2018}'),
    (0x90, '\u{2019}'),
    (0x91, '\u{201a}'),
    (0x92, '\u{2122}'),
    (0x93, '\u{fb01}'),
    (0x94, '\u{fb02}'),
    (0x95, '\u{141}'),
    (0x96, '\u{152}'),
    (0x97, '\u{160}'),
    (0x98, '\u{178}'),
    (0x99, '\u{17d}'),
    (0x9a, '\u{131}'),
    (0x9b, '\u{142}'),
    (0x9c, '\u{153}'),
    (0x9d, '\u{161}'),
    (0x9e, '\u{17e}'),
    (0xa0, '\u{20ac}'),
];

/// The character `PDFDocEncoding` code `code` stands for, if it stands for
/// one.
fn char_of(code: u8) -> Option<char> {
    match code {
        b'\t' | b'\n' | b'\r' | b' '..=b'~' | 0xa1..=0xac | 0xae..=0xff => Some(char::from(code)),
        _ => REMAPPED
            .iter()
            .find(|&&(held, _)| held == code)
            .map(|&(_, character)| character),
    }
}

/// A text string's characters (ISO 32000-2 §7.9.2.2): UTF-16BE or UTF-8 behind
/// their byte order marks, and `PDFDocEncoding` otherwise; what spells no
/// character is read as the replacement character.
///
/// # Errors
///
/// [`PdfError::OutOfMemory`].
pub(crate) fn text(bytes: &[u8]) -> Result<String, PdfError> {
    let mut out = String::new();
    let mut put = |character: char| -> Result<(), PdfError> {
        out.try_reserve(character.len_utf8())?;
        out.push(character);
        Ok(())
    };
    if let Some(units) = bytes.strip_prefix(b"\xfe\xff") {
        let units = units
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_be_bytes(pair));
        for character in char::decode_utf16(units) {
            put(character.unwrap_or(char::REPLACEMENT_CHARACTER))?;
        }
    } else if let Some(utf8) = bytes.strip_prefix(b"\xef\xbb\xbf") {
        for chunk in utf8.utf8_chunks() {
            for character in chunk.valid().chars() {
                put(character)?;
            }
            if !chunk.invalid().is_empty() {
                put(char::REPLACEMENT_CHARACTER)?;
            }
        }
    } else {
        for &code in bytes {
            put(char_of(code).unwrap_or(char::REPLACEMENT_CHARACTER))?;
        }
    }
    Ok(out)
}

/// The code `PDFDocEncoding` gives `character`, if it gives it one.
pub(crate) fn code_of(character: char) -> Option<u8> {
    match character {
        '\t' | '\n' | '\r' | ' '..='~' | '\u{a1}'..='\u{ac}' | '\u{ae}'..='\u{ff}' => {
            u8::try_from(character).ok()
        }
        _ => REMAPPED
            .iter()
            .find(|&&(_, held)| held == character)
            .map(|&(code, _)| code),
    }
}

#[cfg(test)]
mod tests {
    use super::{code_of, text};

    #[test]
    fn a_text_string_is_read_in_its_encoding() {
        assert_eq!(
            text(b"\xfe\xff\x00T\x00\xe9\xd8\x3d\xde\x00").as_deref(),
            Ok("Té\u{1f600}")
        );
        assert_eq!(text("\u{feff}été".as_bytes()).as_deref(), Ok("été"));
        assert_eq!(text(b"\xa0 \x96\x7f").as_deref(), Ok("€ Œ\u{fffd}"));
    }

    #[test]
    fn a_character_has_its_own_code_its_remapped_one_or_none() {
        assert_eq!(code_of('a'), Some(b'a'));
        assert_eq!(code_of('\n'), Some(b'\n'));
        assert_eq!(code_of('é'), Some(0xe9));
        assert_eq!(code_of('€'), Some(0xa0));
        assert_eq!(code_of('Œ'), Some(0x96));
        assert_eq!(code_of('\u{2d8}'), Some(0x18));
        for unencoded in [
            '\u{0}', '\u{18}', '\u{7f}', '\u{9f}', '\u{a0}', '\u{ad}', '中',
        ] {
            assert_eq!(code_of(unencoded), None, "{unencoded:?}");
        }
    }
}
