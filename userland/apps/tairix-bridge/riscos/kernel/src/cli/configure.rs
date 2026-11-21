//! `*Configure` and `*Status`: the kernel's keywords, each a field of the
//! CMOS RAM, then the modules' configuration keywords, then a module that
//! answers the service call.
//!
//! A module's keyword is matched as a command is, a line that is refused
//! changes nothing, and a listing turns paged mode off however it ends.
//! `Ignore` takes the number its value starts with, none being nought, and
//! turns the ignore character on only for a number that ends the line, a
//! comma straight after it allowed, as RISC OS 5 does.

use alloc::vec::Vec;

use crate::convert::Decimal;
use tairix_obey::gstrans::{gstrans, Flags};

use super::commands::TOO_MANY;
use super::table::{Item, Wanted, CONFIGURE, FILING};
use crate::cmos::{self, Spread};
use crate::error::{Error, SYNTAX};
use crate::module::ModuleKey;
use crate::os::{Fail, Os, Regs};
use crate::service;
use crate::swi::os::read_unsigned::DECIMAL;
use crate::swi::os::{POINTER, READ_SYS_INFO};
use crate::swi::X;

/// `OS_Pointer` 1: the pointing device's type set.
const SET_POINTER_TYPE: u32 = 1;
/// The page a size counts in, but for the font cache's 4 KiB.
const PAGE: u32 = 4096;
const FONT_UNIT: u32 = 4096;
/// The column a listing's values start at, past a name that reaches it.
const VALUE_COLUMN: usize = 11;
/// What a configuration's code returns in R0 for the kernel's errors.
const OWN_ERRORS: u32 = 3;

/// "Bad configure option".
const BAD_OPTION: Error = Error {
    number: SYNTAX,
    token: "UnknownConfigureOption",
};
/// "Numeric parameter needed".
const NOT_NUMERIC: Error = Error {
    number: SYNTAX,
    token: "NumberNeeded",
};
/// "Configure parameter too large".
const TOO_BIG: Error = Error {
    number: SYNTAX,
    token: "ConfigureValueTooLarge",
};
/// "Bad status option".
const BAD_STATUS: Error = Error {
    number: SYNTAX,
    token: "UnknownStatusOption",
};

/// The errors a configuration's code names by number.
const NUMBERED: [Error; 4] = [BAD_OPTION, NOT_NUMERIC, TOO_BIG, TOO_MANY];
/// The number a configuration's code answers for too many parameters.
pub(crate) const TOO_MANY_PARAMETERS: u32 = 3;

/// A field of a CMOS byte: its location, its lowest bit and its width.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Field {
    location: u8,
    bit: u8,
    width: u8,
}

impl Field {
    const fn new(location: u8, bit: u8, width: u8) -> Self {
        Self {
            location,
            bit,
            width,
        }
    }

    /// The most the field holds.
    const fn most(self) -> u32 {
        (1 << self.width) - 1
    }
}

/// How a keyword is given and shown, and where it is kept.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// No value: the field is set to the one the keyword names.
    NoParm(Field, u8),
    /// A number.
    Number(Field),
    /// A size, in units of `unit` bytes or of a kibibyte after a `K`.
    Size(Field, u32),
    /// Its own reading, showing and keeping.
    Special(Special),
}

/// The keywords with handling of their own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Special {
    Baud,
    Cache,
    Ignore,
    Mode,
    MonitorType,
    MouseStep,
    MouseType,
    Sync,
    Tv,
}

impl Special {
    /// What `*Configure` alone shows the keyword takes.
    const fn syntax(self) -> &'static [u8] {
        match self {
            Self::Baud | Self::MouseStep | Self::MouseType => b"<D>",
            Self::Cache => b"On|Off",
            Self::Ignore => b"[<D>]",
            Self::Mode | Self::Sync | Self::MonitorType => b"<D> | Auto",
            Self::Tv => b"[<D> [[,] <D>]]",
        }
    }
}

/// One of the kernel's keywords.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Keyword {
    name: &'static str,
    kind: Kind,
}

const fn keyword(name: &'static str, kind: Kind) -> Keyword {
    Keyword { name, kind }
}

/// The fields the special keywords keep their values in.
const BAUD: Field = Field::new(cmos::PRINT_SERIAL, 2, 3);
const NO_IGNORE: Field = Field::new(cmos::PRINT_SERIAL, 1, 1);
const IGNORED: Field = Field::new(cmos::PRINTER_IGNORE, 0, 8);
const CACHE_OFF: Field = Field::new(cmos::SYSTEM_SPEED, 5, 1);
const MOUSE_STEP: Field = Field::new(cmos::MOUSE_STEP, 0, 8);
const MOUSE_TYPE: Field = Field::new(cmos::MOUSE_TYPE, 0, 8);
const TV: Field = Field::new(cmos::MODE_TV, 4, 4);
/// The fields kept for keywords with no value.
const BOOT: Field = Field::new(cmos::DBTB, 4, 1);
const CAPS: Field = Field::new(cmos::START, 3, 3);
// A bit the PRM reserves, where genuine RISC OS 5 keeps `Num` and `NoNum`.
const NUM: Field = Field::new(cmos::START, 7, 1);
const LOUD: Field = Field::new(cmos::DBTB, 1, 1);
const SCROLL: Field = Field::new(cmos::DBTB, 3, 1);

/// The kernel's keywords: alphabetical, each followed by its opposites, so an
/// abbreviation names the first of them it matches.
pub(crate) const KEYWORDS: [Keyword; 34] = [
    keyword("Baud", Kind::Special(Special::Baud)),
    keyword("Boot", Kind::NoParm(BOOT, 1)),
    keyword("NoBoot", Kind::NoParm(BOOT, 0)),
    keyword("Cache", Kind::Special(Special::Cache)),
    keyword("Caps", Kind::NoParm(CAPS, 4)),
    keyword("NoCaps", Kind::NoParm(CAPS, 2)),
    keyword("ShCaps", Kind::NoParm(CAPS, 1)),
    keyword("Data", Kind::Number(Field::new(cmos::DBTB, 5, 3))),
    keyword("Delay", Kind::Number(Field::new(cmos::KEY_DELAY, 0, 8))),
    keyword(
        "DumpFormat",
        Kind::Number(Field::new(cmos::PRINT_SOUND, 0, 5)),
    ),
    keyword(
        "FontSize",
        Kind::Size(Field::new(cmos::FONT_SIZE, 0, 8), FONT_UNIT),
    ),
    keyword("Ignore", Kind::Special(Special::Ignore)),
    keyword("Language", Kind::Number(Field::new(cmos::LANGUAGE, 0, 8))),
    keyword("Loud", Kind::NoParm(LOUD, 1)),
    keyword("Quiet", Kind::NoParm(LOUD, 0)),
    keyword("Mode", Kind::Special(Special::Mode)),
    keyword("MonitorType", Kind::Special(Special::MonitorType)),
    keyword("MouseStep", Kind::Special(Special::MouseStep)),
    keyword("MouseType", Kind::Special(Special::MouseType)),
    keyword("Num", Kind::NoParm(NUM, 0)),
    keyword("NoNum", Kind::NoParm(NUM, 1)),
    keyword("Print", Kind::Number(Field::new(cmos::PRINT_SERIAL, 5, 3))),
    keyword(
        "PrinterBufferSize",
        Kind::Size(Field::new(cmos::PRINTER_BUFFER, 0, 7), PAGE),
    ),
    keyword(
        "RamFSSize",
        Kind::Size(Field::new(cmos::RAM_DISC, 0, 7), PAGE),
    ),
    keyword("Repeat", Kind::Number(Field::new(cmos::KEY_REPEAT, 0, 8))),
    keyword(
        "RMASize",
        Kind::Size(Field::new(cmos::RMA_SIZE, 0, 7), PAGE),
    ),
    keyword(
        "ScreenSize",
        Kind::Size(Field::new(cmos::SCREEN_SIZE, 0, 7), PAGE),
    ),
    keyword("Scroll", Kind::NoParm(SCROLL, 0)),
    keyword("NoScroll", Kind::NoParm(SCROLL, 1)),
    keyword(
        "SpriteSize",
        Kind::Size(Field::new(cmos::SPRITE_SIZE, 0, 7), PAGE),
    ),
    keyword("Sync", Kind::Special(Special::Sync)),
    keyword(
        "SystemSize",
        Kind::Size(Field::new(cmos::SYSTEM_HEAP, 0, 6), PAGE),
    ),
    keyword("TV", Kind::Special(Special::Tv)),
    keyword("WimpMode", Kind::Special(Special::Mode)),
];

/// The keyword `typed` names and where its value starts in it: the same
/// letters in either case until both end at a space or control character,
/// or a `.` after at least one of them; the first in the table's order.
pub(crate) fn find(typed: &[u8]) -> Option<(&'static Keyword, usize)> {
    KEYWORDS.iter().find_map(|keyword| {
        let name = keyword.name.as_bytes();
        let mut i = 0;
        loop {
            let t = typed.get(i).copied().unwrap_or(0);
            let n = name.get(i).copied().unwrap_or(0);
            if t <= b' ' && n <= b' ' {
                return Some((keyword, i));
            }
            if t.eq_ignore_ascii_case(&n) {
                i += 1;
                continue;
            }
            return (t == b'.' && i > 0).then_some((keyword, i + 1));
        }
    })
}

/// `name` as `*Status` shows a keyword with no value: a space before each
/// capital that follows a small letter, and `ShCaps` in full.
fn spaced(name: &str) -> Vec<u8> {
    let name = if name == "ShCaps" { "ShiftCaps" } else { name };
    let mut out = Vec::new();
    let mut capital_last = true;
    for &c in name.as_bytes() {
        let capital = c <= b'Z';
        if capital && !capital_last {
            out.push(b' ');
        }
        capital_last = capital;
        out.push(c);
    }
    out
}

/// The keyword the value `value` of `keyword`'s field means, for a
/// `*Status` of one with no value whose field holds another.
fn set_instead(keyword: &Keyword, value: u32) -> Option<&'static Keyword> {
    let named = |name: &str| KEYWORDS.iter().find(|k| k.name == name);
    match keyword.name {
        "Boot" | "NoBoot" => named(if value == 0 { "NoBoot" } else { "Boot" }),
        "Caps" | "NoCaps" | "ShCaps" => named(match value {
            0 | 1 => "ShCaps",
            2 | 3 => "NoCaps",
            _ => "Caps",
        }),
        "Num" | "NoNum" => named(if value == 0 { "Num" } else { "NoNum" }),
        "Loud" | "Quiet" => named(if value == 0 { "Quiet" } else { "Loud" }),
        "Scroll" | "NoScroll" => named(if value == 0 { "Scroll" } else { "NoScroll" }),
        _ => None,
    }
}

/// `name` and the spaces that bring a listing to its value's column.
fn padded(name: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(name);
    out.resize(
        out.len() + VALUE_COLUMN.saturating_sub(name.len()).max(1),
        b' ',
    );
}

/// What a listing is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Listing {
    /// `*Configure`: what each keyword takes.
    Configure,
    /// `*Status`: what each is set to.
    Status,
    /// `*Status` of one keyword: what it is set to, whichever it names.
    One,
}

impl Os<'_> {
    /// `field`'s value.
    fn read_field(&mut self, field: Field) -> Result<u32, Fail> {
        let byte = u32::from(self.read_cmos(field.location)?);
        Ok((byte >> field.bit) & field.most())
    }

    /// `value` written into `field`; "too big" if it does not fit.
    fn write_field(&mut self, field: Field, value: u32) -> Result<(), Fail> {
        if value > field.most() {
            return Err(self.error(TOO_BIG));
        }
        let mask = field.most() << field.bit;
        let byte = u32::from(self.read_cmos(field.location)?);
        let new = (byte & !mask) | (value << field.bit);
        self.write_cmos(field.location, new.to_le_bytes()[0])
    }

    /// The decimal number after the spaces at `at`, and where it ends;
    /// none if there is no number there.
    fn configured_number(&mut self, at: u32) -> Result<Option<(u32, u32)>, Fail> {
        let (_, at) = self.skip_spaces(at)?;
        match self.number_at(at, DECIMAL, 0) {
            Ok(read) => Ok(Some(read)),
            Err(Fail::Error(_)) => Ok(None),
            Err(unwind) => Err(unwind),
        }
    }

    /// The number a value must be, after spaces, ending at a space or the
    /// line's end; "numeric parameter needed" otherwise.
    fn numeric(&mut self, at: u32) -> Result<(u32, u32), Fail> {
        match self.configured_number(at)? {
            Some((value, end)) if self.read_byte(end)? <= b' ' => Ok((value, end)),
            _ => Err(self.error(NOT_NUMERIC)),
        }
    }

    /// A number or `Auto`, after spaces, as `None` for `Auto`.
    fn number_or_auto(&mut self, at: u32) -> Result<(Option<u32>, u32), Fail> {
        let (_, start) = self.skip_spaces(at)?;
        if let Some(end) = self.word_at(start, b"Auto")? {
            return Ok((None, end));
        }
        let (value, end) = self.numeric(start)?;
        Ok((Some(value), end))
    }

    /// Where `word`, in any case, ends at `at`, if it is there and is
    /// followed by a space or the line's end.
    fn word_at(&mut self, at: u32, word: &[u8]) -> Result<Option<u32>, Fail> {
        let (text, _) = self.read_until(at, word.len() + 1, |_| false)?;
        let fits = text.len() == word.len() + 1
            && text[..word.len()].eq_ignore_ascii_case(word)
            && text[word.len()] <= b' ';
        Ok(fits.then(|| at.wrapping_add(crate::os::len32(word.len()))))
    }

    /// Whether only spaces stand between `at` and the line's end.
    fn ends_line(&mut self, at: u32) -> Result<bool, Fail> {
        Ok(matches!(self.skip_spaces(at)?.0, b'\r' | b'\n' | 0))
    }

    /// "Too many parameters" unless only spaces stand between `at` and the
    /// line's end.
    fn line_ends(&mut self, at: u32) -> Result<(), Fail> {
        if self.ends_line(at)? {
            Ok(())
        } else {
            Err(self.error(TOO_MANY))
        }
    }

    /// A size's units: a number of them, or a number of kibibytes after
    /// which a `K` stands, rounded up to a whole unit of `unit` bytes.
    fn size_units(&mut self, unit: u32, at: u32) -> Result<(u32, u32), Fail> {
        let Some((value, end)) = self.configured_number(at)? else {
            return Err(self.error(NOT_NUMERIC));
        };
        let after = self.read_byte(end)?;
        if after <= b' ' {
            return Ok((value, end));
        }
        if !after.eq_ignore_ascii_case(&b'K') {
            return Err(self.error(NOT_NUMERIC));
        }
        let per = unit / 1024;
        let units = value.wrapping_add(per - 1) & !(per - 1);
        Ok((units / per, end.wrapping_add(1)))
    }

    /// `*Configure` of the kernel's `keyword`, its value at `at`.
    fn configure_keyword(&mut self, keyword: &Keyword, at: u32) -> Result<(), Fail> {
        let (field, (value, end)) = match keyword.kind {
            Kind::NoParm(field, value) => (field, (u32::from(value), at)),
            Kind::Number(field) => (field, self.numeric(at)?),
            Kind::Size(field, unit) => (field, self.size_units(unit, at)?),
            Kind::Special(special) => return self.configure_special(special, at),
        };
        self.line_ends(end)?;
        self.write_field(field, value)
    }

    /// `*Configure` of a keyword with handling of its own.
    fn configure_special(&mut self, special: Special, at: u32) -> Result<(), Fail> {
        match special {
            Special::Baud => {
                let (baud, end) = self.numeric(at)?;
                if baud.cast_signed() > 8 {
                    return Err(self.error(TOO_BIG));
                }
                self.line_ends(end)?;
                // Nought is 9600, as 7 is.
                let stored = baud.wrapping_sub(1);
                self.write_field(BAUD, if stored.cast_signed() < 0 { 6 } else { stored })
            }
            Special::Cache => {
                let (_, start) = self.skip_spaces(at)?;
                let off = if let Some(end) = self.word_at(start, b"On")? {
                    (false, end)
                } else if let Some(end) = self.word_at(start, b"Off")? {
                    (true, end)
                } else {
                    return Err(self.error(BAD_OPTION));
                };
                if self.skip_spaces(off.1)?.0 >= b' ' {
                    return Err(self.error(BAD_OPTION));
                }
                self.write_field(CACHE_OFF, u32::from(off.0))
            }
            Special::Ignore => {
                if self.ends_line(at)? {
                    self.write_field(NO_IGNORE, 1)?;
                    return self.write_field(IGNORED, 0);
                }
                let read = self.configured_number(at)?;
                let (character, _) = read.unwrap_or((0, at));
                if character > IGNORED.most() {
                    return Err(self.error(TOO_BIG));
                }
                self.write_field(IGNORED, character)?;
                if let Some((_, end)) = read {
                    let after = if self.read_byte(end)? == b',' {
                        end.wrapping_add(1)
                    } else {
                        end
                    };
                    if self.ends_line(after)? {
                        self.write_field(NO_IGNORE, 0)?;
                    }
                }
                Ok(())
            }
            Special::Mode => self.configure_spread(&cmos::MODE, at).map(drop),
            Special::Sync => self.configure_spread(&cmos::SYNC, at).map(drop),
            Special::MonitorType => {
                let monitor = self.configure_spread(&cmos::MONITOR_TYPE, at)?;
                if let Some(vdu) = self.world.vdu {
                    (vdu.set_monitor)(self, monitor);
                }
                let mut changed = Regs::default();
                changed.r[1] = service::MODE_FILE_CHANGED;
                self.service(&mut changed)?;
                Ok(())
            }
            Special::MouseStep => {
                let Some((step, end)) = self.configured_number(at)? else {
                    return Err(self.error(NOT_NUMERIC));
                };
                if step == 0 {
                    return Err(self.error(BAD_OPTION));
                }
                if step > 127 {
                    return Err(self.error(TOO_BIG));
                }
                self.line_ends(end)?;
                self.write_field(MOUSE_STEP, step)
            }
            Special::MouseType => {
                let Some((kind, end)) = self.configured_number(at)? else {
                    return Err(self.error(NOT_NUMERIC));
                };
                if kind > MOUSE_TYPE.most() {
                    return Err(self.error(TOO_BIG));
                }
                self.line_ends(end)?;
                let mut pointer = Regs::default();
                pointer.r[..2].copy_from_slice(&[SET_POINTER_TYPE, kind]);
                // The configured device is what matters; one the pointer
                // cannot use now is still what starts next time.
                if let Err(Fail::Unwind(unwind)) = self.perform(X | POINTER, &mut pointer) {
                    return Err(Fail::Unwind(unwind));
                }
                self.write_field(MOUSE_TYPE, kind)
            }
            Special::Tv => self.configure_tv(at),
        }
    }

    /// `*Configure` of a value spread over fields: a number at most its
    /// most, or `Auto`; its value.
    fn configure_spread(&mut self, spread: &Spread, at: u32) -> Result<u32, Fail> {
        let (value, end) = self.number_or_auto(at)?;
        let value = match value {
            None => spread.automatic,
            Some(value) if value > spread.most => return Err(self.error(TOO_BIG)),
            Some(value) => value,
        };
        self.line_ends(end)?;
        self.write_spread(spread, value)?;
        Ok(value)
    }

    /// `*Configure TV`: the position, 0 to 3 or 252 to 255, then after an
    /// optional comma the interlace, 0 or 1; each nought if left out.
    fn configure_tv(&mut self, at: u32) -> Result<(), Fail> {
        let Some((position, end)) = self.configured_number(at)? else {
            self.line_ends(at)?;
            return self.write_field(TV, 0);
        };
        if !matches!(position, 0..=3 | 252..=255) {
            return Err(self.error(BAD_OPTION));
        }
        let position = position & 7;
        let (byte, mut next) = self.skip_spaces(end)?;
        let comma = byte == b',';
        if comma {
            next = self.skip_spaces(next.wrapping_add(1))?.1;
        }
        let (interlace, end) = match self.configured_number(next)? {
            Some(read) => read,
            None if comma => return Err(self.error(BAD_OPTION)),
            None => (0, next),
        };
        if interlace > 1 {
            return Err(self.error(TOO_BIG));
        }
        self.line_ends(end)?;
        self.write_field(TV, position.wrapping_shl(1) | interlace)
    }

    /// The line `keyword` shows in `listing`, into `out`; nothing for a
    /// keyword with no value that is not what its field is set to.
    fn keyword_line(
        &mut self,
        keyword: &Keyword,
        listing: Listing,
        out: &mut Vec<u8>,
    ) -> Result<(), Fail> {
        let name = keyword.name.as_bytes();
        match (keyword.kind, listing) {
            (Kind::NoParm(..), Listing::Configure) => out.extend_from_slice(name),
            (Kind::NoParm(field, value), _) => {
                let set = self.read_field(field)?;
                let shown = if set == u32::from(value) {
                    Some(keyword)
                } else if listing == Listing::One {
                    set_instead(keyword, set)
                } else {
                    None
                };
                let Some(shown) = shown else {
                    return Ok(());
                };
                out.extend_from_slice(&spaced(shown.name));
            }
            (Kind::Number(_), Listing::Configure) => {
                padded(name, out);
                out.extend_from_slice(b"<D>");
            }
            (Kind::Size(..), Listing::Configure) => {
                padded(name, out);
                out.extend_from_slice(b"<D>[K]");
            }
            (Kind::Special(special), Listing::Configure) => {
                padded(name, out);
                out.extend_from_slice(special.syntax());
            }
            (Kind::Number(field), _) => {
                padded(name, out);
                let value = self.read_field(field)?;
                out.extend_from_slice(Decimal::of(value.cast_signed()).as_bytes());
            }
            (Kind::Size(field, unit), _) => {
                padded(name, out);
                let units = self.read_field(field)?;
                let mut kibibytes = units.wrapping_mul(unit / 1024);
                if kibibytes == 0 && keyword.name == "ScreenSize" {
                    let mut info = Regs::default();
                    self.perform(X | READ_SYS_INFO, &mut info)?;
                    kibibytes = info.r[0] / 1024;
                }
                out.extend_from_slice(Decimal::of(kibibytes.cast_signed()).as_bytes());
                out.push(b'K');
            }
            (Kind::Special(special), _) => self.special_status(keyword, special, out)?,
        }
        out.extend_from_slice(b"\n\r");
        Ok(())
    }

    /// What a keyword with handling of its own is set to.
    fn special_status(
        &mut self,
        keyword: &Keyword,
        special: Special,
        out: &mut Vec<u8>,
    ) -> Result<(), Fail> {
        let name = keyword.name.as_bytes();
        let number = |value: u32, out: &mut Vec<u8>| {
            if value == u32::MAX {
                out.extend_from_slice(b"Auto");
            } else {
                out.extend_from_slice(Decimal::of(value.cast_signed()).as_bytes());
            }
        };
        match special {
            Special::Ignore => {
                if self.read_field(NO_IGNORE)? != 0 {
                    out.extend_from_slice(b"No Ignore");
                    return Ok(());
                }
                padded(name, out);
                number(self.read_field(IGNORED)?, out);
            }
            Special::Baud => {
                padded(name, out);
                number(self.read_field(BAUD)? + 1, out);
            }
            Special::Cache => {
                padded(name, out);
                out.extend_from_slice(if self.read_field(CACHE_OFF)? == 0 {
                    b"On"
                } else {
                    b"Off"
                });
            }
            Special::Mode => {
                padded(name, out);
                number(self.read_spread(&cmos::MODE)?, out);
            }
            Special::Sync => {
                padded(name, out);
                number(self.read_spread(&cmos::SYNC)?, out);
            }
            Special::MonitorType => {
                padded(name, out);
                number(self.read_spread(&cmos::MONITOR_TYPE)?, out);
            }
            Special::MouseStep => {
                padded(name, out);
                number(self.read_field(MOUSE_STEP)?.max(1), out);
            }
            Special::MouseType => {
                padded(name, out);
                number(self.read_field(MOUSE_TYPE)?, out);
            }
            Special::Tv => {
                padded(name, out);
                let tv = self.read_field(TV)?;
                // The position is a signed three bits, shown as a byte.
                let position = (tv >> 1) | if tv & 0b1000 != 0 { 0xF8 } else { 0 };
                number(position, out);
                out.push(b',');
                number(tv & 1, out);
            }
        }
        Ok(())
    }

    /// Calls each of the modules' configuration keywords with R0 `reason`,
    /// as a listing does, their errors passed over.
    fn list_module_keywords(&mut self, reason: u32) -> Result<(), Fail> {
        let chain = self.world.modules.chain().to_vec();
        for key in chain {
            self.walk_table(key, |os, item, _| {
                if item.code == 0 || item.info & (FILING | CONFIGURE) != CONFIGURE {
                    return Ok(None::<()>);
                }
                let mut regs = Regs::default();
                regs.r[0] = reason;
                match os.call_keyword(item, key, &mut regs) {
                    Ok(()) | Err(Fail::Error(_)) => Ok(None),
                    Err(unwind) => Err(unwind),
                }
            })?;
        }
        Ok(())
    }

    /// Calls a module's configuration keyword `item` with `regs`, R1 its
    /// module's base and R12 its private word.
    fn call_keyword(&mut self, item: &Item, key: ModuleKey, regs: &mut Regs) -> Result<(), Fail> {
        regs.r[1] = item.base;
        regs.r[12] = self.private_of(Some(key));
        self.call_code(item, regs)
    }

    /// The first module keyword the keyword at `typed` names, and where its
    /// value starts.
    fn module_keyword(&mut self, typed: &[u8]) -> Result<Option<(ModuleKey, Item, usize)>, Fail> {
        let chain = self.world.modules.chain().to_vec();
        for key in chain {
            if let Some((item, end)) = self.find_in_table(key, typed, Wanted::Configure)? {
                return Ok(Some((key, item, end)));
            }
        }
        Ok(None)
    }

    /// An error a configuration's code or a service answer gives: the
    /// kernel's own for the numbers that name them, else the block.
    fn configure_error(&mut self, answer: u32) -> Fail {
        match NUMBERED.get(answer as usize) {
            Some(&error) if answer <= OWN_ERRORS => self.error(error),
            _ => Fail::Error(answer),
        }
    }

    /// Everything `*Configure` or `*Status` lists, paged.
    fn list_all(&mut self, status: bool) -> Result<(), Fail> {
        let vdu = self.paged()?;
        let listed = self.list_everything(status);
        self.unpaged(vdu)?;
        listed
    }

    fn list_everything(&mut self, status: bool) -> Result<(), Fail> {
        let heading = self.message_with(b"ConfigureListHeading", &[])?;
        let kind = self.message_with(
            if status {
                b"StatusListKind"
            } else {
                b"ConfigureListKind"
            },
            &[],
        )?;
        let mut out = heading;
        out.push(b' ');
        out.extend_from_slice(&kind);
        out.extend_from_slice(b"\n\r\n\r");
        self.write_text(&out)?;
        let listing = if status {
            Listing::Status
        } else {
            Listing::Configure
        };
        let caps = |keyword: &Keyword| matches!(keyword.kind, Kind::NoParm(CAPS, _));
        let mut keywords = KEYWORDS.iter().peekable();
        while let Some(keyword) = keywords.next() {
            out.clear();
            if status && caps(keyword) {
                // One line for the group: whichever it is set to.
                self.keyword_line(keyword, Listing::One, &mut out)?;
                while keywords.next_if(|&next| caps(next)).is_some() {}
            } else {
                self.keyword_line(keyword, listing, &mut out)?;
            }
            self.write_text(&out)?;
        }
        self.list_module_keywords(u32::from(status))?;
        let mut service = Regs::default();
        service.r[1] = if status {
            service::UK_STATUS
        } else {
            service::UK_CONFIG
        };
        self.service(&mut service)?;
        let tails: &[&[u8]] = if status {
            &[b"StatusFooter"]
        } else {
            &[b"ConfigureFooterNumbers", b"ConfigureFooterOptional"]
        };
        for &tail in tails {
            let text = self.message_with(tail, &[])?;
            let text = self.translated(&text)?;
            self.write_text(&text)?;
        }
        Ok(())
    }

    /// `text` GSTrans'd, as the listings' closing messages are.
    fn translated(&mut self, text: &[u8]) -> Result<Vec<u8>, Fail> {
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(text.len())
            .map_err(|_| self.no_memory())?;
        buffer.resize(text.len(), 0);
        let translation = self.with_variables(0, |vars, budget| {
            Ok(gstrans(text, Flags::NONE, vars, budget, &mut buffer))
        })?;
        buffer.truncate(translation.stored);
        Ok(buffer)
    }
}

/// `*Configure`: everything it can set, or the keyword given set — the
/// kernel's, a module's, or one a module claims.
pub(crate) fn configure(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[1] == 0 {
        return os.list_all(false);
    }
    let (typed, _) = os.read_until(regs.r[0], super::LONGEST, super::is_end)?;
    if let Some((keyword, end)) = find(&typed) {
        return os.configure_keyword(keyword, regs.r[0].wrapping_add(crate::os::len32(end)));
    }
    if let Some((key, item, end)) = os.module_keyword(&typed)? {
        let (_, value) = os.skip_spaces(regs.r[0].wrapping_add(crate::os::len32(end)))?;
        let mut call = Regs::default();
        call.r[0] = value;
        return match os.call_keyword(&item, key, &mut call) {
            Err(Fail::Error(answer)) => Err(os.configure_error(answer)),
            other => other,
        };
    }
    let mut service = Regs::default();
    service.r[..2].copy_from_slice(&[regs.r[0], service::UK_CONFIG]);
    os.service(&mut service)?;
    if service.r[1] != 0 {
        return Err(os.error(BAD_OPTION));
    }
    // A claimant answers with a negative R0 for done.
    if service.r[0].cast_signed() < 0 {
        Ok(())
    } else {
        Err(os.configure_error(service.r[0]))
    }
}

/// `*Status`: what everything is set to, or the keyword given.
pub(crate) fn status(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[1] == 0 {
        return os.list_all(true);
    }
    let (typed, _) = os.read_until(regs.r[0], super::LONGEST, super::is_end)?;
    if let (1, Some((keyword, _))) = (regs.r[1], find(&typed)) {
        let mut out = Vec::new();
        os.keyword_line(keyword, Listing::One, &mut out)?;
        return os.write_text(&out);
    }
    if let Some((key, item, _)) = os.module_keyword(&typed)? {
        let mut call = Regs::default();
        call.r[0] = 1;
        return os.call_keyword(&item, key, &mut call);
    }
    let mut service = Regs::default();
    service.r[..2].copy_from_slice(&[regs.r[0], service::UK_STATUS]);
    os.service(&mut service)?;
    if service.r[1] == 0 {
        Ok(())
    } else {
        Err(os.error(BAD_STATUS))
    }
}

#[cfg(test)]
mod tests;
