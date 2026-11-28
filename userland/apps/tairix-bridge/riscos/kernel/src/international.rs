//! International (`plans/RISCOS-EMULATOR.md` §4.6): the names and numbers of
//! countries, alphabets and keyboards, each country's alphabet and ISO 3166
//! code, every alphabet's characters, and the system font redefined for an
//! alphabet from `lib/vdu`'s glyphs — all answers to `Service_International`,
//! which the kernel's `OS_Byte` 70 and 71 and its commands ask.
//!
//! An alphabet's character without a glyph keeps the shape it had, as
//! RISC OS's International leaves one.

mod alphabets;

use alloc::vec::Vec;

use crate::error::Error;
use crate::module::{Builtin, Command, Resource, Services};
use crate::msgtrans::Own;
use crate::os::{len32, Fail, Os, Regs, Unwind, World};
use crate::service::{self, international as reason};
use crate::swi::{os, X};

/// `OS_Byte` 70 and 71: the country, and the alphabet or keyboard.
const COUNTRY_BYTE: u32 = 70;
pub(crate) const ALPHABET_BYTE: u32 = 71;
/// What they read with.
const READ_COUNTRY: u32 = 127;
pub(crate) const READ_ALPHABET: u32 = 127;
pub(crate) const READ_KEYBOARD: u32 = 0xFF;
/// The keyboard is set as 128 more than its number.
pub(crate) const SET_KEYBOARD: u32 = 0x80;
/// Latin1, the alphabet RISC OS starts in.
pub(crate) const LATIN1: u32 = 101;
/// UTF-8, whose codes from &80 are shown by their number.
pub const UTF8: u32 = 111;
/// The USA, whose keyboard is the US layout.
pub const USA: u32 = 48;
/// The configured country.
const COUNTRY_CMOS: u8 = crate::cmos::COUNTRY;

/// Its errors.
const UNKNOWN_ALPHABET: Error = Error {
    number: 0x640,
    token: "UnknownAlphabetName",
};
const UNKNOWN_KEYBOARD: Error = Error {
    number: 0x642,
    token: "UnknownKeyboardName",
};
const UNKNOWN_COUNTRY: Error = Error {
    number: 0x641,
    token: "UnknownCountryName",
};

/// Its messages.
const MESSAGES: &[u8] = include_bytes!("../resources/Internatio/Messages");
const MESSAGES_FILE: crate::msgtrans::OwnFile =
    crate::msgtrans::OwnFile::named(b"Resources:$.Resources.Internatio.Messages\0");

/// The countries by number, so an abbreviation names the first it matches:
/// each number, name, alphabet and ISO 3166 code. The keyboards that have no country's
/// name are numbered past them.
const COUNTRIES: [(u32, &str, u32, Option<&[u8; 2]>); 42] = [
    (0, "Default", 0, None),
    (1, "UK", 101, Some(b"uk")),
    (4, "Italy", 101, Some(b"it")),
    (5, "Spain", 101, Some(b"es")),
    (6, "France", 112, Some(b"fr")),
    (7, "Germany", 101, Some(b"de")),
    (8, "Portugal", 101, Some(b"pt")),
    (9, "Esperanto", 103, None),
    (10, "Greece", 107, Some(b"gr")),
    (11, "Sweden", 101, Some(b"se")),
    (12, "Finland", 101, Some(b"fi")),
    (14, "Denmark", 101, Some(b"dk")),
    (15, "Norway", 101, Some(b"no")),
    (16, "Iceland", 101, Some(b"is")),
    (17, "Canada1", 112, Some(b"ca")),
    (18, "Canada2", 101, Some(b"ca")),
    (19, "Canada", 101, Some(b"ca")),
    (20, "Turkey", 109, Some(b"tr")),
    (22, "Ireland", 101, Some(b"ie")),
    (24, "Russia", 105, Some(b"ru")),
    (26, "Israel", 108, Some(b"il")),
    (27, "Mexico", 101, Some(b"mx")),
    (28, "LatinAm", 101, None),
    (29, "Australia", 101, Some(b"au")),
    (30, "Austria", 101, Some(b"at")),
    (31, "Belgium", 101, Some(b"be")),
    (32, "Japan", 111, Some(b"jp")),
    (33, "MiddleEast", 101, None),
    (34, "Netherlands", 101, Some(b"nl")),
    (35, "Switzerland", 101, Some(b"ch")),
    (36, "Wales", 101, Some(b"uk")),
    (48, "USA", 101, Some(b"us")),
    (49, "Wales2", 110, Some(b"uk")),
    (50, "China", 111, Some(b"cn")),
    (51, "Brazil", 101, Some(b"br")),
    (52, "SAfrica", 101, Some(b"za")),
    (53, "Korea", 111, Some(b"kr")),
    (54, "Taiwan", 111, Some(b"tw")),
    (70, "DvorakUK", 101, Some(b"uk")),
    (71, "DvorakUSA", 101, Some(b"us")),
    (72, "ColemakUK", 101, None),
    (80, "ISO1", 101, None),
];

/// The keyboards past `ISO1` that have no name, and their alphabets.
const NAMELESS_KEYBOARDS: [(u32, u32); 7] = [
    (81, 102),
    (82, 103),
    (83, 104),
    (84, 105),
    (86, 107),
    (87, 108),
    (88, 109),
];
/// The ISO 3166 codes of countries International has no name for.
const NAMELESS_ISO: [(u32, &[u8; 2]); 2] = [(23, b"hk"), (25, b"ru")];

/// The alphabets by number, so an abbreviation names the first it matches.
pub const ALPHABETS: [(u32, &str); 15] = [
    (LATIN1, "Latin1"),
    (102, "Latin2"),
    (103, "Latin3"),
    (104, "Latin4"),
    (105, "Cyrillic"),
    (107, "Greek"),
    (108, "Hebrew"),
    (109, "Latin5"),
    (110, "Welsh"),
    (UTF8, "UTF8"),
    (112, "Latin9"),
    (113, "Latin6"),
    (114, "Latin7"),
    (115, "Latin8"),
    (116, "Latin10"),
];

/// A name's room when it is written for a command.
const NAME_ROOM: u32 = 16;
/// The listing `*Alphabets` and `*Countries` make, each name and a tab.
const LISTING_ROOM: u32 = 512;

/// Where its workspace keeps its messages' file, the room a listing is made
/// in, then each alphabet's table of 256 words, in [`ALPHABETS`]' order.
const LISTING_AT: u32 = MESSAGES_FILE.room();
const TABLES_AT: u32 = LISTING_AT + LISTING_ROOM;
const TABLE: u32 = 256 * 4;
const WORKSPACE: u32 = TABLES_AT + TABLE * len32(ALPHABETS.len());

/// International.
pub(crate) static INTERNATIONAL: Builtin = Builtin {
    title: "International",
    help_name: None,
    help_suffix: None,
    bridge: false,
    swis: None,
    services: Some(Services {
        numbers: &[service::INTERNATIONAL],
        handler: service,
    }),
    commands: &[
        Command::new(
            "Alphabet",
            (0, 1),
            "*Alphabet selects an alphabet by its name or a country's. With neither, it gives the current one.",
            Some("Syntax: *Alphabet [<country name> | <alphabet name>]"),
            alphabet_command,
        ),
        Command::new("Alphabets", (0, 0), "*Alphabets lists the alphabets known.", Some("Syntax: *Alphabets"), alphabets_command),
        Command::new(
            "Country",
            (0, 1),
            "*Country selects a country, its alphabet and its keyboard. With no name, it gives the current one.",
            Some("Syntax: *Country [<country name>]"),
            country_command,
        ),
        Command::new("Countries", (0, 0), "*Countries lists the countries known.", Some("Syntax: *Countries"), countries_command),
        Command::new(
            "Keyboard",
            (0, 1),
            "*Keyboard selects a country's keyboard. With no name, it gives the current one.",
            Some("Syntax: *Keyboard [<country name>]"),
            keyboard_command,
        ),
        Command {
            syntax: Some("Syntax: *Configure Country <country name>"),
            ..Command::configure(
                "Country",
                "*Configure Country sets the country the machine starts in.",
                configure_country,
            )
        },
    ],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[],
    vectors: &[],
    resources: &[Resource {
        name: "Resources.Internatio.Messages",
        file_type: crate::filing::filetype::TEXT,
        data: MESSAGES,
    }],
    code: None,
};

/// International's state.
pub(crate) struct State {
    workspace: Option<u32>,
    pub(crate) own: Own,
}

impl State {
    pub(crate) const fn new() -> Self {
        Self {
            workspace: None,
            own: Own::new(),
        }
    }
}

#[expect(clippy::unnecessary_wraps, reason = "an own-file accessor's signature")]
fn own(world: &mut World) -> Option<&mut Own> {
    Some(&mut world.international.own)
}

/// Whether `typed` names `name`: in either case, a full stop abbreviating
/// it, else the whole of it.
fn names(typed: &[u8], name: &str) -> bool {
    let name = name.as_bytes();
    for (i, &t) in typed.iter().enumerate() {
        if t == b'.' {
            return true;
        }
        match name.get(i) {
            Some(&n) if (n ^ t) & !0x20 == 0 => {}
            _ => return false,
        }
    }
    typed.len() == name.len()
}

/// The character `alphabet` puts at `code`; none where it has none or knows
/// no such alphabet.
#[must_use]
pub fn character(alphabet: u32, code: u8) -> Option<char> {
    if code < 0x80 {
        return Some(char::from(code));
    }
    let upper = usize::from(code - 0x80);
    if alphabet == LATIN1 {
        return tairix_vdu::font::RISC_OS_UPPER[upper];
    }
    let (_, half) = alphabets::UPPER_HALVES
        .iter()
        .find(|(n, _)| *n == alphabet)?;
    Some(half[upper])
        .filter(|&u| u != alphabets::NONE)
        .and_then(|u| char::from_u32(u32::from(u)))
}

/// The alphabet country or keyboard `number` uses.
fn country_alphabet(number: u32) -> Option<u32> {
    COUNTRIES
        .iter()
        .find(|c| c.0 == number && c.0 != 0)
        .map(|c| c.2)
        .or_else(|| {
            NAMELESS_KEYBOARDS
                .iter()
                .find(|k| k.0 == number)
                .map(|k| k.1)
        })
}

impl Os<'_> {
    /// Writes `name`, cut to `room`, at `buffer`: R5 the bytes written.
    fn give_name(&mut self, regs: &mut Regs, name: &[u8]) -> Result<(), Fail> {
        let written = &name[..name.len().min(regs.r[5] as usize)];
        self.write_bytes(regs.r[4], written)?;
        regs.r[5] = len32(written.len());
        regs.r[1] = 0;
        Ok(())
    }

    /// Redefines `alphabet`'s codes from `first` to `last` with VDU 23 from
    /// the system font: whether International knows the alphabet.
    fn define_alphabet(&mut self, alphabet: u32, first: u32, last: u32) -> Result<bool, Fail> {
        let known = alphabet == UTF8 || ALPHABETS.iter().any(|a| a.0 == alphabet && a.0 != UTF8);
        if !known {
            return Ok(false);
        }
        let last = u8::try_from(last).unwrap_or(u8::MAX);
        let Ok(first) = u8::try_from(first.max(0x20)) else {
            // A range wholly past the codes defines nothing.
            return Ok(alphabet != UTF8);
        };
        if alphabet == UTF8 && first >= 0x80 {
            // A range wholly past ASCII is defined but not claimed.
            for code in first..=last {
                self.define_glyph(u32::from(code), tairix_vdu::font::number(code))?;
            }
            return Ok(false);
        }
        for code in first..=last {
            self.yield_point()?;
            if alphabet == UTF8 && code >= 0x80 {
                self.define_glyph(u32::from(code), tairix_vdu::font::number(code))?;
                continue;
            }
            let shown = match character(if alphabet == UTF8 { LATIN1 } else { alphabet }, code) {
                Some('\u{7F}') => Some('\u{2588}'),
                Some(c) if u32::from(c) >= 0x20 => Some(c),
                _ => None,
            };
            if let Some(glyph) = shown.and_then(tairix_vdu::font::glyph_of) {
                self.define_glyph(u32::from(code), glyph)?;
            }
        }
        Ok(true)
    }

    /// VDU 23 defining character `code` as `glyph`.
    fn define_glyph(&mut self, code: u32, glyph: tairix_vdu::font::Glyph) -> Result<(), Fail> {
        let mut bytes = [0; 10];
        bytes[0] = 23;
        bytes[1] = u8::try_from(code).unwrap_or(0);
        bytes[2..].copy_from_slice(&glyph);
        self.write_text(&bytes)
    }

    /// `Service_International` `reason` offered with R3 to R5: R4 and R5 as
    /// the module that claimed it left them, none where no module did.
    ///
    /// # Errors
    ///
    /// What unwinds.
    pub fn ask_international(
        &mut self,
        reason: u32,
        r3: u32,
        r4: u32,
        r5: u32,
    ) -> Result<Option<(u32, u32)>, Unwind> {
        let mut regs = Regs::default();
        regs.r[1..6].copy_from_slice(&[service::INTERNATIONAL, reason, r3, r4, r5]);
        self.service(&mut regs)?;
        Ok((regs.r[1] == 0).then_some((regs.r[4], regs.r[5])))
    }

    /// The name `reason` gives `number`, or "Unknown".
    fn international_name(&mut self, reason: u32, number: u32) -> Result<Vec<u8>, Fail> {
        let buffer = self.world.international.own.reply();
        if let Some((_, length)) = self.ask_international(reason, number, buffer, NAME_ROOM - 1)? {
            let length = length.min(NAME_ROOM - 1) as usize;
            return Ok(self.read_until(buffer, length, |_| false)?.0);
        }
        let length = self.own_lookup(own, b"UnnamedEntry", buffer, NAME_ROOM)?;
        Ok(self.read_until(buffer, length as usize, |_| false)?.0)
    }

    /// The text of its message `token`.
    fn international_message(&mut self, token: &[u8]) -> Result<Vec<u8>, Fail> {
        let buffer = self.world.international.own.reply();
        let length = self.own_lookup(own, token, buffer, crate::msgtrans::REPLY_ROOM)?;
        Ok(self.read_until(buffer, length as usize, |_| false)?.0)
    }
}

fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let workspace = os.rma_claim(WORKSPACE)?;
    os.write_word(regs.r[12], workspace)?;
    let own = MESSAGES_FILE.place(os, workspace)?;
    for (n, &(alphabet, _)) in (0..).zip(ALPHABETS.iter()) {
        let at = workspace + TABLES_AT + TABLE * n;
        let mut table = [0u8; TABLE as usize];
        for code in 0..=255u8 {
            let word = character(alphabet, code).map_or(u32::MAX, u32::from);
            table[usize::from(code) * 4..][..4].copy_from_slice(&word.to_le_bytes());
        }
        os.write_bytes(at, &table)?;
    }
    os.world.international = State::new();
    os.world.international.workspace = Some(workspace);
    os.world.international.own = own;
    Ok(())
}

fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.close_own(own)?;
    os.world.international = State::new();
    Ok(())
}

fn service(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Unwind> {
    match answer(os, regs) {
        Err(Fail::Unwind(unwind)) => Err(unwind),
        _ => Ok(()),
    }
}

/// `Service_International`: R2 the reason, claimed by R1 nought.
fn answer(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match regs.r[2] {
        reason::COUNTRY_NUMBER | reason::ALPHABET_NUMBER => {
            let (typed, _) = os.read_until(regs.r[3], crate::error::TEXT, |b| b <= b' ')?;
            let found = if regs.r[2] == reason::COUNTRY_NUMBER {
                COUNTRIES
                    .iter()
                    .filter(|c| !c.1.is_empty())
                    .find(|c| names(&typed, c.1))
                    .map(|c| c.0)
            } else {
                ALPHABETS.iter().find(|a| names(&typed, a.1)).map(|a| a.0)
            };
            if let Some(number) = found {
                regs.r[4] = number;
                regs.r[1] = 0;
            }
            Ok(())
        }
        reason::COUNTRY_NAME => match COUNTRIES
            .iter()
            .find(|c| c.0 == regs.r[3] && !c.1.is_empty())
        {
            Some(country) => os.give_name(regs, country.1.as_bytes()),
            None => Ok(()),
        },
        reason::ALPHABET_NAME => match ALPHABETS.iter().find(|a| a.0 == regs.r[3]) {
            Some(alphabet) => os.give_name(regs, alphabet.1.as_bytes()),
            None => Ok(()),
        },
        reason::COUNTRY_ALPHABET => {
            if let Some(alphabet) = country_alphabet(regs.r[3]) {
                regs.r[4] = alphabet;
                regs.r[1] = 0;
            }
            Ok(())
        }
        reason::DEFINE => {
            if os.define_alphabet(regs.r[3], regs.r[4], regs.r[5])? {
                regs.r[1] = 0;
            }
            Ok(())
        }
        reason::DEFINE_UCS => {
            let glyph = char::from_u32(regs.r[4])
                .filter(|&c| u32::from(c) < 0x1_0000)
                .and_then(tairix_vdu::font::glyph_of);
            if let Some(glyph) = glyph {
                os.define_glyph(regs.r[3], glyph)?;
                regs.r[1] = 0;
            }
            Ok(())
        }
        reason::UCS_TABLE => {
            let place = ALPHABETS
                .iter()
                .position(|a| a.0 == regs.r[3] && a.0 != UTF8);
            if let (Some(place), Some(workspace)) = (place, os.world.international.workspace) {
                regs.r[4] = workspace + TABLES_AT + TABLE * len32(place);
                regs.r[1] = 0;
            }
            Ok(())
        }
        reason::ISO_3166 => {
            let code = COUNTRIES
                .iter()
                .find(|c| c.0 == regs.r[3])
                .and_then(|c| c.3)
                .or_else(|| NAMELESS_ISO.iter().find(|n| n.0 == regs.r[3]).map(|n| n.1));
            if let Some(code) = code.filter(|_| regs.r[3] < 100) {
                if regs.r[5] > 1 {
                    os.write_bytes(regs.r[4], code)?;
                    regs.r[5] = 2;
                }
                regs.r[1] = 0;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The name a command was given, asked of every module as `reason`.
fn named(os: &mut Os<'_>, reason: u32, text: u32) -> Result<Option<u32>, Fail> {
    Ok(os
        .ask_international(reason, text, 0, 0)?
        .map(|(number, _)| number))
}

fn alphabet_command(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[1] == 0 {
        let current = os.read_alphabet()?;
        let name = os.international_name(reason::ALPHABET_NAME, current)?;
        return os.write_line(&name);
    }
    let number = match named(os, reason::ALPHABET_NUMBER, regs.r[0])? {
        Some(number) => Some(number),
        None => named(os, reason::COUNTRY_NUMBER, regs.r[0])?,
    };
    let Some(number) = number else {
        return Err(os.own_error(own, UNKNOWN_ALPHABET, &[]));
    };
    os.os_byte(ALPHABET_BYTE, number)?;
    Ok(())
}

fn keyboard_command(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[1] == 0 {
        let current = os.os_byte(ALPHABET_BYTE, READ_KEYBOARD)?;
        let name = os.international_name(reason::COUNTRY_NAME, current)?;
        return os.write_line(&name);
    }
    let Some(number) = named(os, reason::COUNTRY_NUMBER, regs.r[0])? else {
        return Err(os.own_error(own, UNKNOWN_KEYBOARD, &[]));
    };
    os.select_keyboard(number)
}

impl Os<'_> {
    /// Country `number`'s keyboard selected, as `*Keyboard` selects it: the
    /// key handler lays the keys out as that keyboard has them.
    ///
    /// # Errors
    ///
    /// `OS_Byte`'s, and a claimant's of `ByteV`.
    pub fn select_keyboard(&mut self, number: u32) -> Result<(), Fail> {
        self.os_byte(ALPHABET_BYTE, number | SET_KEYBOARD).map(drop)
    }
}

fn country_command(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[1] == 0 {
        let current = os.os_byte(COUNTRY_BYTE, READ_COUNTRY)?;
        let name = os.international_name(reason::COUNTRY_NAME, current)?;
        return os.write_line(&name);
    }
    let Some(number) = named(os, reason::COUNTRY_NUMBER, regs.r[0])? else {
        return Err(os.own_error(own, UNKNOWN_COUNTRY, &[]));
    };
    os.os_byte(COUNTRY_BYTE, number)?;
    Ok(())
}

/// `*Alphabets` and `*Countries`: the heading, then every name the modules
/// give a number below 128, tab-separated and laid out by `OS_PrettyPrint`.
fn listing(os: &mut Os<'_>, heading: &[u8], reason: u32) -> Result<(), Fail> {
    let mut out = os.international_message(heading)?;
    out.extend_from_slice(b":\n\r");
    os.write_text(&out)?;
    let listing = os.world.international.workspace.unwrap_or(0) + LISTING_AT;
    // Each name is asked into the room left, a tab after it; the last tab
    // becomes its terminator.
    let mut at = listing;
    let end = listing + LISTING_ROOM - 1;
    for number in 0..128 {
        let room = end.saturating_sub(at);
        if room == 0 {
            break;
        }
        if let Some((_, length)) = os.ask_international(reason, number, at, room)? {
            at += length.min(room);
            os.write_byte(at, b'\t')?;
            at = at.wrapping_add(1);
        }
    }
    os.write_byte(if at > listing { at - 1 } else { listing }, 0)?;
    let mut pretty = Regs::default();
    pretty.r[0] = listing;
    os.perform(X | os::PRETTY_PRINT, &mut pretty)?;
    os.write_text(b"\n\r")
}

fn alphabets_command(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    listing(os, b"AlphabetsHeading", reason::ALPHABET_NAME)
}

fn countries_command(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    listing(os, b"CountriesHeading", reason::COUNTRY_NAME)
}

/// `*Configure Country`, and its lines in the listings.
fn configure_country(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match crate::module::Asked::of(regs.r[0]) {
        crate::module::Asked::Syntax => {
            let text = os.international_message(b"ConfigureCountrySyntax")?;
            os.write_line(&text)
        }
        crate::module::Asked::Status => {
            let configured = os.read_cmos(COUNTRY_CMOS)?;
            let keyword = os.international_message(b"CountryKeyword")?;
            let name = os.international_name(reason::COUNTRY_NAME, u32::from(configured))?;
            os.write_status(&keyword, &name, b"\n\r")
        }
        crate::module::Asked::Set(text) => {
            let Some(number) = named(os, reason::COUNTRY_NUMBER, text)? else {
                return Err(os.own_error(own, UNKNOWN_COUNTRY, &[]));
            };
            os.write_cmos(COUNTRY_CMOS, u8::try_from(number).unwrap_or(0))
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
