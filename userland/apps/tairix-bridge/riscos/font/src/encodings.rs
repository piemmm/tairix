//! The encoding files ResourceFS holds in `Resources:$.Fonts.Encodings`,
//! made from the ROM encodings' tables in the syntax the PRM gives: the base
//! encoding `/Base0` naming each base glyph, `/Default` naming the glyphs of
//! a symbol font that has no encoding of its own, and each encoding naming
//! the glyph each code shows. A glyph is named as the Adobe Glyph List names
//! its first character, and a glyph RISC OS alone has, whose characters UTF-8
//! gives from the private use area in turn, by a name no list knows; a code
//! the font leaves undefined is named as its alphabet's character, which no
//! base glyph is.
//!
//! The files in `resources/ROMFonts/Encodings/` are these, made once and
//! held to them by a test, since ResourceFS serves bytes the image holds.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_riscos_kernel::{ALPHABETS, UTF8_ALPHABET};

use crate::encoding::{NOTDEF, PRIVATE_USE};
use crate::glyph_names::GlyphNames;
use crate::rom_encodings::{RomEncoding, BASE0, NONE, ROM_ENCODINGS};

/// What the PRM names a base glyph no code reaches.
const BASE_NOTDEF: &str = ".NotDef";
/// The names a line holds.
const PER_LINE: usize = 8;

/// Latin 1's encoding, which `/Default` follows, and the codes whose glyph
/// `/Default` gives a name of its own: no-break space and soft hyphen, whose
/// Latin 1 names are the space's and the hyphen's.
const LATIN1: usize = 0;
const OWN_NAMES: [u32; 2] = [0xA0, 0xAD];

/// The name International gives alphabet `number`, which its encoding's file
/// is named by; none for a number it names no alphabet by.
fn alphabet_name(number: u32) -> Option<&'static str> {
    ALPHABETS
        .iter()
        .find(|&&(alphabet, _)| alphabet == number)
        .map(|&(_, name)| name)
}

/// The name base glyph `glyph` goes by.
fn base_name(names: &GlyphNames, glyph: usize) -> String {
    match BASE0.get(glyph).and_then(|characters| characters.first()) {
        Some(&character)
            if PRIVATE_USE.contains(&character) && names.listed(character).is_none() =>
        {
            format!("glyph{glyph}")
        }
        Some(&character) => names.name(character),
        None if glyph == 0 => String::from(NOTDEF),
        None => String::from(BASE_NOTDEF),
    }
}

/// `names` as an encoding's body: each after a `/`, eight to a line.
fn body(out: &mut String, names: impl Iterator<Item = String>) {
    let names: Vec<String> = names.collect();
    for line in names.chunks(PER_LINE) {
        let mut first = true;
        for name in line {
            if !first {
                out.push(' ');
            }
            first = false;
            out.push('/');
            out.push_str(name);
        }
        out.push('\n');
    }
}

/// `/Base0`: every base glyph's name, in order.
pub(crate) fn base0(names: &GlyphNames) -> Vec<u8> {
    let mut out =
        String::from("% /Base0, the base encoding of RISC OS's own fonts\n%%RISCOS_Alphabet 0\n");
    body(
        &mut out,
        (0..BASE0.len()).map(|glyph| base_name(names, glyph)),
    );
    out.into_bytes()
}

/// An encoding's file, `name` its alphabet's: based on `/Base0`, its
/// alphabet's number, and the name of the glyph each code shows.
pub(crate) fn encoding(names: &GlyphNames, name: &str, rom: &RomEncoding) -> Vec<u8> {
    let mut out = format!(
        "% {name}, over the base encoding of RISC OS's own fonts\n%%RISCOS_BasedOn 0\n%%RISCOS_Alphabet {}\n",
        rom.alphabet
    );
    let code_name = |code: usize| {
        let glyph = rom.glyphs[code];
        if glyph != NONE {
            return base_name(names, usize::from(glyph));
        }
        let character = u8::try_from(code)
            .ok()
            .and_then(|code| tairix_riscos_kernel::alphabet_character(rom.alphabet, code))
            .filter(|&c| c >= ' ' && c != '\u{7F}');
        character.map_or_else(|| String::from(NOTDEF), |c| names.name(u32::from(c)))
    };
    body(&mut out, (0..256).map(code_name));
    out.into_bytes()
}

/// `/Default`: Latin 1's glyphs named by its codes, a code Latin 1 leaves
/// undefined naming none.
pub(crate) fn default(names: &GlyphNames) -> Vec<u8> {
    let latin1 = &ROM_ENCODINGS[LATIN1];
    let mut out =
        String::from("% /Default, the base encoding of a symbol font with none of its own\n");
    let code_name = |code: u32| match latin1.glyphs[code as usize] {
        NONE => String::from(NOTDEF),
        _ if OWN_NAMES.contains(&code) => names.name(code),
        glyph => base_name(names, usize::from(glyph)),
    };
    body(&mut out, (0..256).map(code_name));
    out.into_bytes()
}

/// The UTF-8 encoding's file, `name` its alphabet's, which names no glyph: a
/// font in it reads a string a character at a time, each the base glyph it
/// reaches, and is based as the alphabets' encodings are.
pub(crate) fn utf8(name: &str) -> Vec<u8> {
    format!(
        "% {name}: each character shows the glyph it reaches\n%%RISCOS_BasedOn 0\n%%RISCOS_Alphabet {UTF8_ALPHABET}\n"
    )
    .into_bytes()
}

/// Every encoding file, by its leaf in `Encodings`: an alphabet's named as
/// International names the alphabet.
pub(crate) fn files(names: &GlyphNames) -> Vec<(String, Vec<u8>)> {
    let mut files = alloc::vec![
        (String::from("/Base0"), base0(names)),
        (String::from("/Default"), default(names)),
    ];
    for rom in &ROM_ENCODINGS {
        if let Some(name) = alphabet_name(rom.alphabet) {
            files.push((String::from(name), encoding(names, name, rom)));
        }
    }
    if let Some(name) = alphabet_name(UTF8_ALPHABET) {
        files.push((String::from(name), utf8(name)));
    }
    files
}

#[cfg(test)]
mod tests;
