//! The UK territory (`plans/RISCOS-EMULATOR.md` §4.6): RISC OS's territory 1
//! — its names, date formats, symbols, calendar and Latin1 tables — whose
//! one time zone is the TAIRiX locale's, its standard and daylight saving
//! readings and its rules taken from `lib/tz`'s transitions.
//!
//! Its tables and names live in its workspace, where its entries hand them
//! out; its names come from its message file, found on `UK$Path`.

use alloc::vec::Vec;

use tairix_abi::time::Time64;
use tairix_riscos_arm::psr;

use super::calendar::{self, Field, Ordinals, Piece, Pieces};
use super::collate;
use super::{BAD_TIME, CONVERT_DATE_AND_TIME, DEREGISTER, ENTRIES, REGISTER, UK as NUMBER};
use crate::clock::{FIVE_BYTES, UNIX_EPOCH};
use crate::error::Error;
use crate::module::{Builtin, Code, Resource, Services};
use crate::msgtrans::{Own, BUFFER_OVERFLOW, ENUMERATE_TOKENS, LOOKUP};
use crate::os::{len32, Fail, Os, Regs, Unwind, World};
use crate::swi::X;

mod rules;

/// `Service_TerritoryManagerLoaded`: the Manager is there to register with.
const MANAGER_LOADED: u32 = 0x64;
/// Its alphabet.
const ALPHABET: u32 = crate::international::LATIN1;

/// Tokens of its message file and their texts, in the file's order.
type Tokens = Vec<(Vec<u8>, Vec<u8>)>;

/// Its errors.
const UNKNOWN_PROPERTY: Error = Error {
    number: 0x193,
    token: "UnknownCharacterProperty",
};
const BAD_STRING: Error = Error {
    number: 0x194,
    token: "UnreadableTimeString",
};
const BAD_FIELD: Error = Error {
    number: 0x2C2,
    token: "UnknownFormatField",
};

/// Its messages, and the path they are found on.
const MESSAGES: &[u8] = include_bytes!("../../resources/UK/Messages");
const MESSAGES_NAME: &[u8] = b"UK:Messages\0";
const PATH: &[u8] = b"UK$Path";
const PATH_DEFAULT: &[u8] = b"Resources:$.Resources.UK.";

/// Its formats: date and time, date, time.
const DATE_AND_TIME: &[u8] = b"%24:%mi:%se %dy-%m3-%ce%yr\0";
const DATE: &[u8] = b"%dy-%m3-%ce%yr\0";
const TIME: &[u8] = b"%24:%mi:%se\0";

/// Its symbols, by `Territory_ReadSymbols`'s index: a string, or a number.
#[derive(Clone, Copy)]
enum Symbol {
    Text(&'static [u8]),
    Number(u32),
}

const SYMBOLS: [Symbol; 19] = [
    Symbol::Text(b".\0"),
    Symbol::Text(b",\0"),
    Symbol::Text(&[3, 0]),
    Symbol::Text(b"GBP \0"),
    Symbol::Text(&[0xA3, 0]),
    Symbol::Text(b".\0"),
    Symbol::Text(b" \0"),
    Symbol::Text(&[3, 0]),
    Symbol::Text(b"\0"),
    Symbol::Text(b"-\0"),
    Symbol::Number(2),
    Symbol::Number(2),
    Symbol::Number(1),
    Symbol::Number(0),
    Symbol::Number(1),
    Symbol::Number(0),
    Symbol::Number(1),
    Symbol::Number(1),
    Symbol::Text(b";\0"),
];

/// Its calendar: the first and last working days, the months, then the
/// longest am or pm, weekday, short weekday, day, suffix, month and short
/// month.
const WORKING_DAYS: (u32, u32) = (2, 6);
const MONTHS: u32 = 12;
const LONGEST: [u32; 7] = [2, 9, 3, 2, 2, 9, 3];

/// The room each name a conversion looks up is given.
const NAME_ROOM: u32 = 180;
/// The room a token takes in the request: the lookup's, an enumeration's
/// pattern and the token it finds each have their own.
const TOKEN_ROOM: u32 = 64;
/// The longest zone name kept.
const ZONE_NAME_ROOM: u32 = 16;

/// The workspace, laid out from nought.
mod at {
    use super::{MESSAGES_NAME, NAME_ROOM, ZONE_NAME_ROOM};
    use crate::os::len32;
    use crate::territory::collate::PROPERTY_COUNT;
    use crate::territory::ENTRIES;

    pub(super) const OWN: u32 = 0;
    pub(super) const NAME: u32 = crate::msgtrans::OWN_DESCRIPTOR;
    pub(super) const REQUEST: u32 = (NAME + len32(MESSAGES_NAME.len())).next_multiple_of(4);
    pub(super) const TABLE: u32 = REQUEST + crate::msgtrans::REQUEST;
    pub(super) const PROPERTIES: u32 = TABLE + 4 * len32(ENTRIES);
    pub(super) const LOWER: u32 = PROPERTIES + 32 * len32(PROPERTY_COUNT);
    pub(super) const UPPER: u32 = LOWER + 256;
    pub(super) const CONTROL: u32 = UPPER + 256;
    pub(super) const PLAIN: u32 = CONTROL + 256;
    pub(super) const VALUE: u32 = PLAIN + 256;
    pub(super) const REPRESENTATION: u32 = VALUE + 256;
    pub(super) const IDENTIFIER: u32 = REPRESENTATION + 16;
    pub(super) const SYMBOLS: u32 = IDENTIFIER + 8;
    pub(super) const FORMATS: u32 = SYMBOLS + 64;
    pub(super) const STANDARD_NAME: u32 = FORMATS + 64;
    pub(super) const SUMMER_NAME: u32 = STANDARD_NAME + ZONE_NAME_ROOM;
    pub(super) const SCRATCH: u32 = SUMMER_NAME + ZONE_NAME_ROOM;
    pub(super) const END: u32 = SCRATCH + NAME_ROOM;
}

/// The entries it registers, in the Manager's order.
const ROUTINES: [Code; ENTRIES] = [
    read_time_zones,
    convert_date_and_time,
    convert_standard_date_and_time,
    convert_standard_date,
    convert_standard_time,
    convert_time_to_ordinals,
    convert_time_string_to_ordinals,
    convert_ordinals_to_time,
    alphabet,
    alphabet_identifier,
    select_keyboard_handler,
    write_direction,
    character_property_table,
    lower_case_table,
    upper_case_table,
    control_table,
    plain_table,
    value_table,
    representation_table,
    collate,
    read_symbols,
    read_calendar_information,
    name_to_number,
    transform_string,
    ime,
    rules::daylight_rules,
    reserved,
    reserved,
    reserved,
    reserved,
    reserved,
    reserved,
    reserved,
    reserved,
    reserved,
    reserved,
    reserved,
    reserved,
    reserved,
    reserved,
    reserved,
    reserved,
    reserved,
];

/// The UK territory.
pub(crate) static UK: Builtin = Builtin {
    title: "UK",
    help_name: None,
    bridge: false,
    swis: None,
    services: Some(Services {
        numbers: &[MANAGER_LOADED],
        handler: service,
    }),
    commands: &[],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &ROUTINES,
    resources: &[Resource {
        name: "Resources.UK.Messages",
        file_type: 0xFFF,
        data: MESSAGES,
    }],
};

/// The UK territory's state: its workspace, and its messages.
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

fn own(world: &mut World) -> &mut Own {
    &mut world.uk.own
}

/// One of the zone's two readings: what it calls itself, and its offset
/// from UTC in centiseconds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Reading {
    name: &'static str,
    offset: i32,
}

/// The instant of the five-byte time `time`.
fn instant(time: u64) -> Time64 {
    let cs = i64::try_from(time % FIVE_BYTES).unwrap_or(0) - UNIX_EPOCH.cast_signed();
    Time64::from_secs(cs.div_euclid(100))
}

/// The five-byte time of `instant`, if it has one.
fn five_byte(instant: Time64) -> Option<u64> {
    let cs = instant
        .secs()
        .checked_mul(100)?
        .checked_add(UNIX_EPOCH.cast_signed())?;
    u64::try_from(cs).ok().filter(|&cs| cs < FIVE_BYTES)
}

/// The reading a zone offset is.
fn reading(offset: tairix_tz::ZoneOffset<'static>) -> Reading {
    Reading {
        name: offset.abbreviation,
        offset: offset.utc_offset.saturating_mul(100),
    }
}

/// The zone's standard and daylight saving readings around `at`: of its
/// reading then and the one it changes to next, where one is daylight
/// saving and the other not, the one further east is daylight saving. A
/// zone that keeps one reading saves an hour by its own name.
fn readings(zone: tairix_tz::Zone<'static>, at: Time64) -> (Reading, Reading) {
    let now = zone.offset_at(at);
    if let Some(change) = zone.next_change(at) {
        if change.after.daylight != now.daylight {
            let (west, east) = if change.after.utc_offset < now.utc_offset {
                (change.after, now)
            } else {
                (now, change.after)
            };
            return (reading(west), reading(east));
        }
    }
    let standard = reading(now);
    let summer = Reading {
        name: standard.name,
        offset: standard.offset.saturating_add(360_000),
    };
    (standard, summer)
}

/// The CMOS settings that keep `zone`'s time at the five-byte time `now`:
/// its standard time in quarter hours, and daylight saving in force while
/// the zone keeps its summer reading.
pub(crate) fn locale(zone: tairix_tz::Zone<'static>, now: u64) -> crate::cmos::Locale {
    let at = instant(now);
    let (standard, summer) = readings(zone, at);
    let quarters = (i64::from(standard.offset) + 45_000).div_euclid(90_000);
    crate::cmos::Locale {
        time_zone: i8::try_from(quarters).unwrap_or(0),
        dst: summer.offset != standard.offset
            && reading(zone.offset_at(at)).offset == summer.offset,
    }
}

impl Os<'_> {
    /// The UK territory's workspace.
    fn uk_workspace(&self) -> u32 {
        self.world.uk.workspace.unwrap_or(0)
    }

    /// The zone's readings now.
    fn uk_readings(&mut self) -> (Reading, Reading) {
        let now = self.real_time();
        readings(self.world.zone, instant(now))
    }

    /// The time zone `Territory_ReadCurrentTimeZone` gives: its name's
    /// address and its offset.
    fn current_zone(&mut self) -> Result<(u32, i32), Fail> {
        let mut regs = Regs::default();
        self.perform(X | (super::CHUNK + 8), &mut regs)?;
        Ok((regs.r[0], regs.r[1].cast_signed()))
    }

    /// `token` of the territory's messages into its scratch: the text.
    fn uk_message(&mut self, token: &[u8]) -> Result<Vec<u8>, Fail> {
        let file = self.own_open(own)?;
        let scratch = self.uk_workspace() + at::SCRATCH;
        self.write_bytes(file.request(), token)?;
        self.write_byte(file.request().wrapping_add(len32(token.len())), 0)?;
        let mut regs = Regs::default();
        regs.r[..4].copy_from_slice(&[file.descriptor(), file.request(), scratch, NAME_ROOM]);
        self.perform(X | LOOKUP, &mut regs)?;
        Ok(self.read_until(scratch, regs.r[3] as usize, |_| false)?.0)
    }

    /// The territory's tokens `pattern` names, in its file's order, with
    /// their texts.
    fn uk_tokens(&mut self, pattern: &[u8]) -> Result<Tokens, Fail> {
        let file = self.own_open(own)?;
        // Clear of the lookup's own request, at the room's start.
        let pattern_at = file.request().wrapping_add(TOKEN_ROOM);
        let token_at = pattern_at.wrapping_add(TOKEN_ROOM);
        self.write_bytes(pattern_at, pattern)?;
        self.write_byte(pattern_at.wrapping_add(len32(pattern.len())), 0)?;
        let mut found = Vec::new();
        let mut from = 0;
        loop {
            let mut regs = Regs::default();
            regs.r[..5].copy_from_slice(&[
                file.descriptor(),
                pattern_at,
                token_at,
                TOKEN_ROOM,
                from,
            ]);
            self.perform(X | ENUMERATE_TOKENS, &mut regs)?;
            if regs.r[2] == 0 {
                return Ok(found);
            }
            from = regs.r[4];
            let token = self.read_until(token_at, regs.r[3] as usize, |_| false)?.0;
            let text = self.uk_message(&token)?;
            found.try_reserve(1).map_err(|_| self.no_memory())?;
            found.push((token, text));
            self.yield_point()?;
        }
    }

    /// Writes a zone's two names into the workspace: their addresses.
    fn uk_zone_names(&mut self, standard: Reading, summer: Reading) -> Result<(u32, u32), Fail> {
        let workspace = self.uk_workspace();
        let mut addresses = [0; 2];
        for ((slot, reading), at) in addresses
            .iter_mut()
            .zip([standard, summer])
            .zip([at::STANDARD_NAME, at::SUMMER_NAME])
        {
            let name = reading.name.as_bytes();
            let name = &name[..name.len().min(ZONE_NAME_ROOM as usize - 1)];
            self.write_bytes(workspace + at, name)?;
            self.write_byte(workspace + at + len32(name.len()), 0)?;
            *slot = workspace + at;
        }
        Ok((addresses[0], addresses[1]))
    }

    /// `time`, five bytes UTC, as local ordinals at the current zone.
    fn uk_local(&mut self, time: u64) -> Result<Ordinals, Fail> {
        let (_, offset) = self.current_zone()?;
        let local = i64::try_from(time).unwrap_or(0) + i64::from(offset);
        Ok(Ordinals::of(
            u64::try_from(local.rem_euclid(FIVE_BYTES.cast_signed())).unwrap_or(0),
        ))
    }
}

fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let workspace = os.rma_claim(at::END)?;
    os.write_word(regs.r[12], workspace)?;
    os.write_bytes(workspace + at::NAME, MESSAGES_NAME)?;
    for (n, map) in (0..).zip(collate::PROPERTY_MAPS) {
        os.write_bytes(workspace + at::PROPERTIES + 32 * n, &map)?;
    }
    for (offset, table) in [
        (at::LOWER, &collate::LOWER),
        (at::UPPER, &collate::UPPER),
        (at::CONTROL, &collate::CONTROL),
        (at::PLAIN, &collate::PLAIN),
        (at::VALUE, &collate::VALUE),
    ] {
        os.write_bytes(workspace + offset, table)?;
    }
    os.write_bytes(workspace + at::REPRESENTATION, collate::REPRESENTATION)?;
    os.write_bytes(workspace + at::IDENTIFIER, b"Latin1\0")?;
    let mut symbol = workspace + at::SYMBOLS;
    for entry in SYMBOLS {
        if let Symbol::Text(text) = entry {
            os.write_bytes(symbol, text)?;
            symbol += len32(text.len());
        }
    }
    let mut format = workspace + at::FORMATS;
    for text in [DATE_AND_TIME, DATE, TIME] {
        os.write_bytes(format, text)?;
        format += len32(text.len());
    }
    os.world.uk = State::new();
    os.world.uk.workspace = Some(workspace);
    os.world.uk.own = Own::at(
        workspace + at::OWN,
        workspace + at::NAME,
        workspace + at::REQUEST,
    );
    if os.variable_text(PATH)?.is_none() {
        os.set_string_variable(PATH, PATH_DEFAULT)?;
    }
    for (i, _) in ROUTINES.iter().enumerate() {
        let address = os.routine_of(&UK, i).unwrap_or(0);
        os.write_word(workspace + at::TABLE + 4 * len32(i), address)?;
    }
    register(os)
}

/// Registers with the Manager, if it is there.
fn register(os: &mut Os<'_>) -> Result<(), Fail> {
    let workspace = os.uk_workspace();
    let mut regs = Regs::default();
    regs.r[..3].copy_from_slice(&[NUMBER, workspace + at::TABLE, workspace]);
    os.perform_quietly(X | REGISTER, &mut regs)?;
    Ok(())
}

fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let mut regs = Regs::default();
    regs.r[0] = NUMBER;
    os.perform_quietly(X | DEREGISTER, &mut regs)?;
    os.close_own(own)?;
    os.world.uk = State::new();
    Ok(())
}

fn service(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Unwind> {
    if regs.r[1] == MANAGER_LOADED {
        if let Err(Fail::Unwind(unwind)) = register(os) {
            return Err(unwind);
        }
    }
    Ok(())
}

/// `Territory_ReadTimeZones`: R0 and R1 the zone's names, R2 and R3 its
/// offsets.
fn read_time_zones(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (standard, summer) = os.uk_readings();
    let (standard_name, summer_name) = os.uk_zone_names(standard, summer)?;
    regs.r[..4].copy_from_slice(&[
        standard_name,
        summer_name,
        standard.offset.cast_unsigned(),
        summer.offset.cast_unsigned(),
    ]);
    Ok(())
}

/// `Territory_ConvertDateAndTime`: R1's time formatted by R4's format into
/// R2's buffer of R3 bytes, in the current time zone.
fn convert_date_and_time(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let size = regs.r[3];
    let (name, offset) = os.current_zone()?;
    let zone_name = os.read_zstring(name, ZONE_NAME_ROOM as usize)?;
    let time = os.read_five(regs.r[1])?;
    let local = i64::try_from(time).unwrap_or(0) + i64::from(offset);
    let ordinals =
        Ordinals::of(u64::try_from(local.rem_euclid(FIVE_BYTES.cast_signed())).unwrap_or(0));
    let format = os.read_zstring(regs.r[4], crate::error::TEXT)?;
    let mut out = Vec::new();
    out.try_reserve(64).map_err(|_| os.no_memory())?;
    let mut left = size;
    let take = |left: &mut u32| -> bool {
        let fits = *left > 0;
        *left = left.saturating_sub(1);
        fits
    };
    for piece in Pieces::of(&format) {
        let Ok(piece) = piece else {
            return Err(os.own_error(own, BAD_FIELD, &[]));
        };
        if !take(&mut left) {
            return Err(os.own_error(own, BUFFER_OVERFLOW, &[]));
        }
        match piece {
            Piece::End => out.push(0),
            Piece::Byte(byte) => out.push(byte),
            Piece::Field(field, suppress) => {
                left += 1;
                let shown = if let Some((value, digits)) = calendar::number(field, &ordinals) {
                    let (text, length) = calendar::digits(value, digits, suppress);
                    text[..length].to_vec()
                } else if field == Field::Week {
                    let (text, length) = calendar::digits(ordinals.iso_week(), 2, suppress);
                    text[..length].to_vec()
                } else if field == Field::Zone {
                    zone_name.clone()
                } else if let Some(token) = calendar::name_token(field, &ordinals) {
                    os.uk_message(token.as_bytes())?
                } else {
                    Vec::new()
                };
                if shown.is_empty() {
                    left = left.saturating_sub(1);
                }
                for &byte in &shown {
                    if !take(&mut left) {
                        return Err(os.own_error(own, BUFFER_OVERFLOW, &[]));
                    }
                    out.push(byte);
                }
            }
        }
    }
    let written = out.len();
    os.write_bytes(regs.r[2], &out)?;
    regs.r[0] = regs.r[2];
    regs.r[1] = regs.r[2].wrapping_add(len32(written)).wrapping_sub(1);
    regs.r[2] = left;
    regs.r[3] = regs.r[4];
    Ok(())
}

/// A standard format: `Territory_ConvertDateAndTime` with the format at
/// `offset` of the workspace's.
fn standard_format(os: &mut Os<'_>, regs: &mut Regs, offset: u32) -> Result<(), Fail> {
    let format = os.uk_workspace() + at::FORMATS + offset;
    let mut call = *regs;
    call.r[4] = format;
    os.perform(X | CONVERT_DATE_AND_TIME, &mut call)?;
    regs.r[..3].copy_from_slice(&call.r[..3]);
    Ok(())
}

fn convert_standard_date_and_time(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    standard_format(os, regs, 0)
}

fn convert_standard_date(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    standard_format(os, regs, len32(DATE_AND_TIME.len()))
}

fn convert_standard_time(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    standard_format(os, regs, len32(DATE_AND_TIME.len() + DATE.len()))
}

/// `Territory_ConvertTimeToOrdinals`: R1's time as local ordinals at R2.
fn convert_time_to_ordinals(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let time = os.read_five(regs.r[1])?;
    let ordinals = os.uk_local(time)?;
    for (i, word) in (0..).zip(ordinals.words()) {
        os.write_word(regs.r[2].wrapping_add(4 * i), word)?;
    }
    Ok(())
}

/// `Territory_ConvertTimeStringToOrdinals`: R2's text read as R1 says, as
/// ordinals at R3.
fn convert_time_string_to_ordinals(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut pattern = [0; calendar::SHORT_MONTH.len() + 2];
    pattern[..calendar::SHORT_MONTH.len()].copy_from_slice(calendar::SHORT_MONTH);
    pattern[calendar::SHORT_MONTH.len()..].copy_from_slice(b"??");
    let months = os.uk_tokens(&pattern)?;
    let names: Vec<&[u8]> = months.iter().map(|(_, text)| text.as_slice()).collect();
    let (text, _) = os.read_until(regs.r[2], crate::error::TEXT, |b| b < b' ')?;
    let (written, read) = calendar::read(&text, regs.r[1], &names);
    for (i, word) in (0..).zip(written) {
        if let Some(word) = word {
            os.write_word(regs.r[3].wrapping_add(4 * i), word)?;
        }
    }
    read.map_err(|calendar::BadString| os.own_error(own, BAD_STRING, &[]))
}

/// `Territory_ConvertOrdinalsToTime`: R2's local ordinals as R1's UTC
/// time, at the current zone.
fn convert_ordinals_to_time(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut words = [0; 7];
    for (i, word) in (0..).zip(words.iter_mut()) {
        *word = os.read_word(regs.r[2].wrapping_add(4 * i))?;
    }
    let Some(local) = calendar::time_of(words) else {
        return Err(os.own_error(own, BAD_TIME, &[]));
    };
    let (_, offset) = os.current_zone()?;
    let utc = i64::try_from(local).unwrap_or(0) - i64::from(offset);
    os.write_five(
        regs.r[1],
        u64::try_from(utc.rem_euclid(FIVE_BYTES.cast_signed())).unwrap_or(0),
    )
}

#[expect(clippy::unnecessary_wraps, reason = "a territory entry's signature")]
fn alphabet(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = ALPHABET;
    Ok(())
}

#[expect(clippy::unnecessary_wraps, reason = "a territory entry's signature")]
fn alphabet_identifier(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = os.uk_workspace() + at::IDENTIFIER;
    Ok(())
}

fn select_keyboard_handler(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.os_byte(
        crate::international::ALPHABET_BYTE,
        crate::international::SET_KEYBOARD | NUMBER,
    )?;
    Ok(())
}

#[expect(clippy::unnecessary_wraps, reason = "a territory entry's signature")]
fn write_direction(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = 0;
    Ok(())
}

fn character_property_table(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[1] as usize >= collate::PROPERTY_COUNT {
        return Err(os.own_error(own, UNKNOWN_PROPERTY, &[]));
    }
    regs.r[0] = os.uk_workspace() + at::PROPERTIES + 32 * regs.r[1];
    Ok(())
}

/// A table's entry: R0 the table at `offset` of the workspace.
#[expect(
    clippy::unnecessary_wraps,
    reason = "the territory entries it answers return it"
)]
fn table(os: &Os<'_>, regs: &mut Regs, offset: u32) -> Result<(), Fail> {
    regs.r[0] = os.uk_workspace() + offset;
    Ok(())
}

fn lower_case_table(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    table(os, regs, at::LOWER)
}

fn upper_case_table(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    table(os, regs, at::UPPER)
}

fn control_table(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    table(os, regs, at::CONTROL)
}

fn plain_table(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    table(os, regs, at::PLAIN)
}

fn value_table(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    table(os, regs, at::VALUE)
}

fn representation_table(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    table(os, regs, at::REPRESENTATION)
}

/// The longest string collation reads: a containment bound on a string
/// with no terminator in reach, not a capacity.
const COLLATE_MOST: usize = 64 * 1024;

/// `Territory_Collate`: R1's string against R2's as R3 asks; R0 and the
/// flags how they order.
fn collate(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let a = os.read_until(regs.r[1], COLLATE_MOST, |b| b == 0)?.0;
    let b = os.read_until(regs.r[2], COLLATE_MOST, |b| b == 0)?.0;
    let order = collate::collate(&a, &b, regs.r[3]);
    regs.r[0] = order.cast_unsigned();
    // As a compare of the two would leave them: C set on equal too.
    regs.flags = match order.signum() {
        -1 => psr::N,
        0 => psr::Z | psr::C,
        _ => psr::C,
    };
    Ok(())
}

/// `Territory_ReadSymbols`: R0 the symbol R1 names, a string's address or
/// a number.
#[expect(clippy::unnecessary_wraps, reason = "a territory entry's signature")]
fn read_symbols(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut at = os.uk_workspace() + at::SYMBOLS;
    for (index, symbol) in (0..).zip(SYMBOLS) {
        match symbol {
            Symbol::Text(text) => {
                if index == regs.r[1] {
                    regs.r[0] = at;
                    return Ok(());
                }
                at += len32(text.len());
            }
            Symbol::Number(n) if index == regs.r[1] => {
                regs.r[0] = n;
                return Ok(());
            }
            Symbol::Number(_) => {}
        }
    }
    Ok(())
}

/// `Territory_ReadCalendarInformation`: R1's time's calendar into R2's
/// twelve words.
fn read_calendar_information(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let time = os.read_five(regs.r[1])?;
    let ordinals = os.uk_local(time)?;
    let (standard, summer) = os.uk_readings();
    let zone_longest = len32(standard.name.len().max(summer.name.len()));
    let mut words = [0; 12];
    words[..4].copy_from_slice(&[
        WORKING_DAYS.0,
        WORKING_DAYS.1,
        MONTHS,
        calendar::days_in_month(ordinals.year, ordinals.month),
    ]);
    words[4..11].copy_from_slice(&LONGEST);
    words[11] = zone_longest;
    for (i, word) in (0..).zip(words) {
        os.write_word(regs.r[2].wrapping_add(4 * i), word)?;
    }
    Ok(())
}

/// `Territory_NameToNumber`: R1's name, to its first control character, as
/// a territory's number; nought for none.
fn name_to_number(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (typed, _) = os.read_until(regs.r[1], crate::error::TEXT, |b| b < b' ')?;
    let fold = |b: u8| collate::LOWER[usize::from(b)];
    let mut pattern = [0; super::TERRITORY_NAME.len() + 1];
    pattern[..super::TERRITORY_NAME.len()].copy_from_slice(super::TERRITORY_NAME);
    pattern[super::TERRITORY_NAME.len()] = b'*';
    for (token, name) in os.uk_tokens(&pattern)? {
        if typed.len() < name.len() {
            if typed.iter().zip(&name).all(|(&t, &n)| fold(t) == fold(n)) {
                break;
            }
            continue;
        }
        if typed.len() == name.len() && typed.iter().zip(&name).all(|(&t, &n)| fold(t) == fold(n)) {
            let digits = &token[super::TERRITORY_NAME.len()..];
            regs.r[0] = digits
                .iter()
                .try_fold(0u32, |n, &d| {
                    d.is_ascii_digit().then(|| n * 10 + u32::from(d - b'0'))
                })
                .unwrap_or(0);
            return Ok(());
        }
    }
    regs.r[0] = 0;
    Ok(())
}

/// `Territory_TransformString`: R2's string as it sorts into R1's buffer of
/// R3 bytes; R0 its length, nothing written where it does not fit.
fn transform_string(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let text = os.read_until(regs.r[2], COLLATE_MOST, |b| b == 0)?.0;
    let mut out = Vec::new();
    out.try_reserve(3 * text.len() + 4)
        .map_err(|_| os.no_memory())?;
    collate::transform(&text, &mut out);
    let length = len32(out.len());
    if length < regs.r[3] {
        out.push(0);
        os.write_bytes(regs.r[1], &out)?;
    }
    regs.r[0] = length;
    Ok(())
}

/// The table's entries reserved for SWIs not yet defined.
fn reserved(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    Err(os.own_error(own, super::UNKNOWN_SWI, &[]))
}

/// `Territory_IME`: no input method.
#[expect(clippy::unnecessary_wraps, reason = "a territory entry's signature")]
fn ime(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = 0;
    Ok(())
}

#[cfg(test)]
mod tests;
