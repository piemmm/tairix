//! The Territory Manager (`plans/RISCOS-EMULATOR.md` §4.6): the territories
//! registered with it, the SWIs it answers itself — the current territory,
//! names, the time zone in force and the clock — and the rest passed to the
//! territory they name, each a table of entries the territory registered.
//!
//! - [`calendar`]: the calendar times are converted by.
//! - `uk`: the UK territory, whose time zone is the TAIRiX locale's.

pub(crate) mod calendar;
mod collate;
mod dst;
mod formats;
pub(crate) mod uk;

use alloc::vec::Vec;

use tairix_riscos_arm::psr;

use crate::error::Error;
use crate::module::{Builtin, Command, CommandKind, Help, Resource, Services, Swis};
use crate::msgtrans::Own;
use crate::os::{len32, Fail, Os, Regs, Unwind, World};
use crate::service;
use crate::swi::os::read_unsigned::{BYTE_SIZED, DECIMAL, TERMINATED};
use crate::swi::{os, X};
use calendar::Ordinals;

pub(crate) use crate::swi::chunk::TERRITORY as CHUNK;
/// `Territory_Register`, `Territory_Deregister` and
/// `Territory_NumberToName`.
pub(crate) const REGISTER: u32 = CHUNK + 1;
pub(crate) const DEREGISTER: u32 = CHUNK + 2;
const NUMBER_TO_NAME: u32 = CHUNK + 3;
/// `Territory_ReadTimeZones`.
pub(crate) const READ_TIME_ZONES: u32 = CHUNK + 0x0A;
/// `Territory_ConvertDateAndTime`.
pub const CONVERT_DATE_AND_TIME: u32 = CHUNK + 0x0B;
/// `Territory_ConvertStandardDateAndTime`.
pub const CONVERT_STANDARD_DATE_AND_TIME: u32 = CHUNK + 0x0C;
/// `Territory_ConvertTimeToOrdinals`.
pub const TIME_TO_ORDINALS: u32 = CHUNK + 0x0F;
/// `Territory_ConvertTimeStringToOrdinals`.
pub const STRING_TO_ORDINALS: u32 = CHUNK + 0x10;
/// `Territory_ConvertOrdinalsToTime`.
pub const ORDINALS_TO_TIME: u32 = CHUNK + 0x11;
/// `Territory_Alphabet`.
pub(crate) const ALPHABET: u32 = CHUNK + 0x12;
/// `Territory_UpperCaseTable`.
pub const UPPER_CASE_TABLE: u32 = CHUNK + 0x18;
/// `Territory_ReadSymbols`.
pub const READ_SYMBOLS: u32 = CHUNK + 0x1E;
/// `Territory_ReadCalendarInformation`.
pub(crate) const READ_CALENDAR_INFORMATION: u32 = CHUNK + 0x1F;
/// `Territory_NameToNumber`.
pub(crate) const NAME_TO_NUMBER: u32 = CHUNK + 0x20;
/// `Territory_ConvertTextToString`, which does nothing.
const CONVERT_TEXT_TO_STRING: u32 = CHUNK + 0x35;
/// `Territory_DaylightRules`' offset in the chunk.
const DAYLIGHT_RULES: u32 = 0x23;
/// `Territory_Select`'s, `Territory_DaylightSaving`'s and
/// `Territory_ConvertTimeFormats`' offsets in the chunk.
const SELECT: u32 = 0x36;
const DAYLIGHT_SAVING: u32 = 0x37;
const CONVERT_TIME_FORMATS: u32 = 0x38;

/// The SWIs a territory answers, in its table's order, from this one.
const FIRST_ENTRY: u32 = READ_TIME_ZONES;
/// The entries a territory registers: `Territory_ReadTimeZones` to &43074.
pub(crate) const ENTRIES: usize = 43;
/// The entries the Manager reaches by number.
mod entry {
    use super::{CHUNK, FIRST_ENTRY};

    const fn of(swi: u32) -> usize {
        (swi - FIRST_ENTRY) as usize
    }

    pub(super) const READ_TIME_ZONES: usize = of(super::READ_TIME_ZONES);
    pub(super) const ALPHABET: usize = of(super::ALPHABET);
    pub(super) const SELECT_KEYBOARD_HANDLER: usize = of(CHUNK + 0x14);
    pub(super) const COLLATE: usize = of(CHUNK + 0x1D);
    pub(super) const DAYLIGHT_RULES: usize = of(CHUNK + super::DAYLIGHT_RULES);
}

/// The CMOS it keeps: the configured territory, the time zone and daylight
/// saving in force.
const TERRITORY_CMOS: u8 = crate::cmos::TERRITORY;
const TIME_ZONE_CMOS: u8 = crate::cmos::TIME_ZONE;
const DST_CMOS: u8 = crate::cmos::ALARM_AND_TIME;
const DST_BIT: u8 = crate::cmos::DST;

/// The stems of the tokens naming each territory and alphabet, followed by
/// its number.
pub(crate) const TERRITORY_NAME: &[u8] = b"TerritoryName";
const ALPHABET_NAME: &[u8] = b"AlphabetName";

/// The UK, which the Manager falls back on.
pub(crate) const UK: u32 = 1;
/// A quarter of an hour and an hour, in centiseconds.
const QUARTER_HOUR: i64 = 90_000;
const HOUR: i64 = 360_000;

/// Its errors.
const UNKNOWN_TERRITORY: Error = Error {
    number: 0x190,
    token: "UnknownTerritoryNumber",
};
const UNKNOWN_ALPHABET: Error = Error {
    number: 0x191,
    token: "UnknownAlphabetNumber",
};
const NO_TERRITORY: Error = Error {
    number: 0x192,
    token: "TerritoryNotLoaded",
};
/// Ordinals naming no time, in the Manager's calls and the UK's alike.
const BAD_TIME: Error = Error {
    number: 0x195,
    token: "TimeValuesOutOfRange",
};
/// A SWI of the chunk no territory or the Manager answers, the reserved
/// entries of a territory's table among them: RISC OS 5's number, where the
/// PRM gives &43040.
pub(crate) const UNKNOWN_SWI: Error = Error {
    number: 0x1E8,
    token: "UnknownTerritorySwi",
};

/// Its messages.
const MESSAGES: &[u8] = include_bytes!("../resources/TerrMgr/Messages");
const MESSAGES_FILE: crate::msgtrans::OwnFile =
    crate::msgtrans::OwnFile::named(b"Resources:$.Resources.TerrMgr.Messages\0");
/// The path its territories are found on, set if it is not.
const PATH: &[u8] = b"TerritoryManager$Path";
const PATH_DEFAULT: &[u8] = b"Resources:$.Resources.TerrMgr.";

/// Its workspace: its messages' file, the name a time zone the territory
/// has none for goes by, and a block for what it asks others.
const CUSTOM_AT: u32 = MESSAGES_FILE.room();
const CUSTOM_ROOM: u32 = 32;
const SCRATCH_AT: u32 = CUSTOM_AT + CUSTOM_ROOM;
const WORKSPACE: u32 = SCRATCH_AT + 16;

/// Its routine: the callback that says it is loaded.
const LOADED_CALLBACK: usize = 0;

/// The SWIs' names.
const NAMES: [&str; 57] = [
    "Number",
    "Register",
    "Deregister",
    "NumberToName",
    "Exists",
    "AlphabetNumberToName",
    "SelectAlphabet",
    "SetTime",
    "ReadCurrentTimeZone",
    "ConvertTimeToUTCOrdinals",
    "ReadTimeZones",
    "ConvertDateAndTime",
    "ConvertStandardDateAndTime",
    "ConvertStandardDate",
    "ConvertStandardTime",
    "ConvertTimeToOrdinals",
    "ConvertTimeStringToOrdinals",
    "ConvertOrdinalsToTime",
    "Alphabet",
    "AlphabetIdentifier",
    "SelectKeyboardHandler",
    "WriteDirection",
    "CharacterPropertyTable",
    "LowerCaseTable",
    "UpperCaseTable",
    "ControlTable",
    "PlainTable",
    "ValueTable",
    "RepresentationTable",
    "Collate",
    "ReadSymbols",
    "ReadCalendarInformation",
    "NameToNumber",
    "TransformString",
    "IME",
    "DaylightRules",
    "Reserved1",
    "Reserved2",
    "Reserved3",
    "Reserved4",
    "Reserved5",
    "Reserved6",
    "Reserved7",
    "Reserved8",
    "Reserved9",
    "Reserved10",
    "Reserved11",
    "Reserved12",
    "Reserved13",
    "Reserved14",
    "Reserved15",
    "Reserved16",
    "Reserved17",
    "ConvertTextToString",
    "Select",
    "DaylightSaving",
    "ConvertTimeFormats",
];

/// A configuration keyword's command.
const fn configure(name: &'static str, help: &'static str, run: crate::module::Code) -> Command {
    Command {
        name,
        kind: CommandKind::Configure,
        min: 0,
        max: 1,
        translate: 0,
        help: Help::Text(help),
        syntax: None,
        run: Some(run),
    }
}

/// The Territory Manager.
pub(crate) static TERRITORY_MANAGER: Builtin = Builtin {
    title: "TerritoryManager",
    help_name: Some("Territories"),
    bridge: false,
    swis: Some(Swis {
        chunk: CHUNK,
        prefix: "Territory",
        names: &NAMES,
        handler: swi,
    }),
    services: Some(Services {
        numbers: &[service::UK_CONFIG, service::POST_INIT],
        handler: service,
    }),
    commands: &[
        Command {
            name: "Territories",
            kind: CommandKind::Command,
            min: 0,
            max: 0,
            translate: 0,
            help: Help::Text("*Territories lists the territories loaded, by number and name."),
            syntax: Some("Syntax: *Territories"),
            run: Some(territories_command),
        },
        Command {
            name: "Territory",
            kind: CommandKind::Command,
            min: 0,
            max: 1,
            translate: 0,
            help: Help::Text("*Territory selects the current territory by its name or number, or names it."),
            syntax: Some("Syntax: *Territory [<territory>]"),
            run: Some(territory_command),
        },
        configure(
            "Territory",
            "*Configure Territory sets the territory the machine starts in.",
            configure_territory,
        ),
        configure(
            "DST",
            "*Configure DST puts the clock on daylight saving time, and *Configure NoDST takes it off.",
            configure_dst,
        ),
        configure("NoDST", "*Configure NoDST takes the clock off daylight saving time.", configure_no_dst),
        configure(
            "AutoDST",
            "*Configure AutoDST puts the clock on and off daylight saving time as the territory's rules say.",
            configure_auto_dst,
        ),
        configure(
            "TimeZone",
            "*Configure TimeZone sets the standard time's offset from UTC, in hours and minutes.",
            configure_time_zone,
        ),
    ],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[loaded],
    vectors: &[],
    resources: &[Resource {
        name: "Resources.TerrMgr.Messages",
        file_type: crate::filing::filetype::TEXT,
        data: MESSAGES,
    }],
    code: None,
};

/// A territory registered: its number, its R12 and its entries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Registered {
    number: u32,
    workspace: u32,
    entries: [u32; ENTRIES],
    bits: crate::memory::Bits,
}

/// The Manager's state.
pub(crate) struct Manager {
    current: u32,
    /// The newest first.
    territories: Vec<Registered>,
    workspace: Option<u32>,
    pub(crate) own: Own,
}

impl Manager {
    pub(crate) const fn new() -> Self {
        Self {
            current: UK,
            territories: Vec::new(),
            workspace: None,
            own: Own::new(),
        }
    }

    fn find(&self, number: u32) -> Option<&Registered> {
        self.territories.iter().find(|t| t.number == number)
    }
}

#[expect(clippy::unnecessary_wraps, reason = "an own-file accessor's signature")]
fn own(world: &mut World) -> Option<&mut Own> {
    Some(&mut world.territory.own)
}

impl Os<'_> {
    /// The territory R0 names: the current one for -1.
    fn named_territory(&self, number: u32) -> u32 {
        if number == u32::MAX {
            self.world.territory.current
        } else {
            number
        }
    }

    /// Calls territory `number`'s `index`th entry with R0 to R9 of `regs`,
    /// which its answers replace, and the entry's index in R11.
    fn call_territory(&mut self, number: u32, index: usize, regs: &mut Regs) -> Result<(), Fail> {
        let number = self.named_territory(number);
        regs.r[0] = number;
        let Some(territory) = self.world.territory.find(number).copied() else {
            return Err(self.own_error(own, NO_TERRITORY, &[]));
        };
        let mut call = Regs::default();
        call.r[..10].copy_from_slice(&regs.r[..10]);
        call.r[11] = len32(index);
        call.r[12] = territory.workspace;
        call.flags = regs.flags;
        self.call_routine(
            territory.entries[index],
            territory.bits,
            crate::os::Entry::Supervisor,
            &mut call,
        )?;
        regs.r[..10].copy_from_slice(&call.r[..10]);
        if index == entry::COLLATE {
            regs.flags = call.flags & (psr::N | psr::Z | psr::C);
        }
        Ok(())
    }

    /// The current territory's time zones: `Territory_ReadTimeZones`'s
    /// answers, none where there is no current territory.
    fn current_zones(&mut self) -> Result<Option<Regs>, Fail> {
        let mut zones = Regs::default();
        match self.call_territory(u32::MAX, entry::READ_TIME_ZONES, &mut zones) {
            Ok(()) => Ok(Some(zones)),
            Err(Fail::Error(_)) => Ok(None),
            Err(unwind) => Err(unwind),
        }
    }

    /// Daylight saving put in force or taken out of it, no longer kept by
    /// the rules.
    fn set_dst(&mut self, on: bool) -> Result<(), Fail> {
        self.set_dst_state(u32::from(on))
    }

    /// `Territory_Select`: territory `number`, which must be registered, made
    /// current and `Service_TerritoryStarted` issued, nothing else set up;
    /// CMOS keeps the configured territory. The rules' next changeover is
    /// the new territory's.
    fn select_territory(&mut self, number: u32) -> Result<(), Fail> {
        if self.world.territory.find(number).is_none() {
            return Err(self.own_error(own, NO_TERRITORY, &[]));
        }
        self.world.territory.current = number;
        self.territory_started()?;
        self.arm_changeover()
    }

    /// The seven words of ordinals at `at`, centiseconds to year.
    fn ordinal_words(&mut self, at: u32) -> Result<[u32; 7], Fail> {
        let mut words = [0; 7];
        for (i, word) in (0..).zip(words.iter_mut()) {
            *word = self.read_word(at.wrapping_add(4 * i))?;
        }
        Ok(words)
    }

    /// The nine words of `time`'s ordinals, weekday and day of the year
    /// last, written at `at`.
    fn write_ordinals(&mut self, at: u32, time: u64) -> Result<(), Fail> {
        for (i, word) in (0..).zip(Ordinals::of(time).words()) {
            self.write_word(at.wrapping_add(4 * i), word)?;
        }
        Ok(())
    }

    /// Territory `number`'s name, or the Manager's word for one without.
    fn territory_name(&mut self, number: u32) -> Result<Vec<u8>, Fail> {
        let buffer = self.world.territory.workspace.map_or(0, |w| w + SCRATCH_AT);
        let mut name = Regs::default();
        name.r[..3].copy_from_slice(&[number, buffer, 16]);
        if self.perform(X | NUMBER_TO_NAME, &mut name).is_ok() {
            self.read_zstring(buffer, 16)
        } else {
            let length = self.own_lookup(own, b"UnnamedTerritory", buffer, 16)?;
            Ok(self.read_until(buffer, length as usize, |_| false)?.0)
        }
    }

    /// `Service_TerritoryStarted`.
    fn territory_started(&mut self) -> Result<(), Fail> {
        let mut regs = Regs::default();
        regs.r[1] = service::TERRITORY_STARTED;
        self.service(&mut regs)?;
        Ok(())
    }

    /// The alphabet territory `number` uses.
    fn territory_alphabet(&mut self, number: u32) -> Result<u32, Fail> {
        let mut regs = Regs::default();
        self.call_territory(number, entry::ALPHABET, &mut regs)?;
        Ok(regs.r[0])
    }

    /// The current territory set up: its keyboard handler selected and its
    /// alphabet defined, then `Service_TerritoryStarted`, and daylight saving
    /// brought up to date where its rules keep it.
    fn territory_selected(&mut self) -> Result<(), Fail> {
        let mut regs = Regs::default();
        self.call_territory(u32::MAX, entry::SELECT_KEYBOARD_HANDLER, &mut regs)?;
        let alphabet = self.territory_alphabet(u32::MAX)?;
        self.define_alphabet_fully(alphabet)?;
        self.territory_started()?;
        self.dst_changeover()
    }

    /// A territory's or alphabet's name, its token's stem and its number,
    /// into R1's buffer of R2 bytes, cut short to fit; `error` if it has none
    /// or none of it fits.
    fn number_to_name(&mut self, stem: &[u8], error: Error, regs: &Regs) -> Result<(), Fail> {
        let mut token = Vec::new();
        token
            .try_reserve_exact(stem.len() + 11)
            .map_err(|_| self.no_memory())?;
        token.extend_from_slice(stem);
        token.extend_from_slice(crate::convert::Decimal::of(regs.r[0].cast_signed()).as_bytes());
        match self.own_lookup(own, &token, regs.r[1], regs.r[2]) {
            Ok(length) if length != 0 => Ok(()),
            Ok(_) | Err(Fail::Error(_)) => Err(self.own_error(own, error, &[])),
            Err(unwind) => Err(unwind),
        }
    }

    /// `Territory_ReadCurrentTimeZone`: R0 the zone's name, R1 its offset in
    /// force. CMOS's standard offset is the current territory's zone where
    /// that zone keeps it, and otherwise one the Manager names itself.
    fn read_current_time_zone(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let quarters = self.read_cmos(TIME_ZONE_CMOS)?.cast_signed();
        let standard = i64::from(quarters) * QUARTER_HOUR;
        let dst = self.read_cmos(DST_CMOS)? & DST_BIT != 0;
        let zones = self
            .current_zones()?
            .filter(|zones| i64::from(zones.r[2].cast_signed()) == standard);
        (regs.r[0], regs.r[1]) = match zones {
            Some(zones) if dst => (zones.r[1], zones.r[3]),
            Some(zones) => (zones.r[0], zones.r[2]),
            None => {
                let offset = standard + if dst { HOUR } else { 0 };
                (
                    self.world.territory.workspace.map_or(0, |w| w + CUSTOM_AT),
                    i32::try_from(offset).unwrap_or(0).cast_unsigned(),
                )
            }
        };
        Ok(())
    }

    /// `Territory_Register`: R0's table of entries, R1 its address, R2 its
    /// R12; the first registration of a number stands.
    fn register_territory(&mut self, regs: &Regs) -> Result<(), Fail> {
        let number = regs.r[0];
        if self.world.territory.find(number).is_some() {
            return Ok(());
        }
        let mut entries = [0; ENTRIES];
        for (i, slot) in (0..).zip(entries.iter_mut()) {
            *slot = self.read_word(regs.r[1].wrapping_add(4 * i))?;
        }
        if self.world.territory.territories.try_reserve(1).is_err() {
            return Err(self.no_memory());
        }
        let bits = self.caller_bits();
        self.world.territory.territories.insert(
            0,
            Registered {
                number,
                workspace: regs.r[2],
                entries,
                bits,
            },
        );
        // Setting the territory up is the Manager's work, not the caller's:
        // its failing never undoes, or fails, a registration that stands.
        if number == self.world.territory.current {
            match self.territory_selected() {
                Ok(()) | Err(Fail::Error(_)) => {}
                Err(unwind) => return Err(unwind),
            }
        }
        Ok(())
    }

    /// A territory's name or number, as `*Configure Territory` takes one;
    /// none where more follows a number.
    fn name_or_number(&mut self, text: u32) -> Result<Option<u32>, Fail> {
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[u32::MAX, text]);
        self.perform(X | NAME_TO_NUMBER, &mut regs)?;
        if regs.r[0] != 0 {
            return Ok(Some(regs.r[0]));
        }
        let (number, end) = self.number_at(text, TERMINATED | BYTE_SIZED | DECIMAL, 0)?;
        Ok((self.skip_spaces(end)?.0 <= b' ').then_some(number))
    }

    /// `*Configure <zone>`: the current territory's daylight saving time by
    /// its name as `*Configure DST`, its standard time's as `*Configure
    /// NoDST`; whether it named either.
    fn configure_zone(&mut self, text: u32) -> Result<bool, Fail> {
        let (typed, _) = self.read_until(text, crate::error::TEXT, |b| b <= b' ')?;
        let Some(zones) = self.current_zones()? else {
            return Ok(false);
        };
        let dst = if typed.eq_ignore_ascii_case(&self.read_zstring(zones.r[1], 32)?) {
            true
        } else if typed.eq_ignore_ascii_case(&self.read_zstring(zones.r[0], 32)?) {
            false
        } else {
            return Ok(false);
        };
        self.set_dst(dst)?;
        Ok(true)
    }
}

fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let workspace = os.rma_claim(WORKSPACE)?;
    os.write_word(regs.r[12], workspace)?;
    let own = MESSAGES_FILE.place(os, workspace)?;
    os.world.territory = Manager::new();
    os.world.territory.workspace = Some(workspace);
    os.world.territory.own = own;
    if os.variable_text(PATH)?.is_none() {
        os.set_string_variable(PATH, PATH_DEFAULT)?;
    }
    let configured = os.read_cmos(TERRITORY_CMOS).unwrap_or(0);
    os.world.territory.current = u32::from(configured ^ 1);
    if os
        .own_lookup(
            self::own,
            b"UnmatchedZoneName",
            workspace + CUSTOM_AT,
            CUSTOM_ROOM,
        )
        .is_err()
    {
        os.write_bytes(workspace + CUSTOM_AT, b"Local\0")?;
    }
    if let Some(callback) = os.routine_of(&TERRITORY_MANAGER, LOADED_CALLBACK) {
        let mut add = Regs::default();
        add.r[..2].copy_from_slice(&[callback, workspace]);
        os.perform(X | os::ADD_CALLBACK, &mut add)?;
    }
    Ok(())
}

fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.close_own(own)?;
    os.world.territory = Manager::new();
    Ok(())
}

fn service(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Unwind> {
    let result = match regs.r[1] {
        service::UK_CONFIG => match os.configure_zone(regs.r[0]) {
            Ok(true) => {
                regs.r[0] = u32::MAX;
                regs.r[1] = 0;
                Ok(())
            }
            Ok(false) => Ok(()),
            Err(fail) => Err(fail),
        },
        service::POST_INIT => {
            if os
                .world
                .territory
                .find(os.world.territory.current)
                .is_none()
            {
                os.world.territory.current = UK;
            }
            os.territory_selected()
        }
        _ => Ok(()),
    };
    match result {
        Err(Fail::Unwind(unwind)) => Err(unwind),
        _ => Ok(()),
    }
}

fn swi(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    let number = CHUNK + offset;
    match offset {
        0 => {
            regs.r[0] = os.world.territory.current;
            Ok(())
        }
        1 => os.register_territory(regs),
        2 => {
            os.world
                .territory
                .territories
                .retain(|t| t.number != regs.r[0]);
            Ok(())
        }
        3 => os.number_to_name(TERRITORY_NAME, UNKNOWN_TERRITORY, regs),
        4 => {
            let exists = os.world.territory.find(regs.r[0]).is_some();
            regs.set_flag(psr::Z, exists);
            Ok(())
        }
        5 => os.number_to_name(ALPHABET_NAME, UNKNOWN_ALPHABET, regs),
        6 => {
            let alphabet = os.territory_alphabet(regs.r[0])?;
            os.define_alphabet_fully(alphabet)?;
            Ok(())
        }
        7 => {
            let block = os.world.territory.workspace.map_or(0, |w| w + SCRATCH_AT);
            let time = os.read_five(regs.r[0])?;
            os.write_byte(block, crate::word::set::UTC)?;
            os.write_five(block + 1, time)?;
            let mut word = Regs::default();
            word.r[..2].copy_from_slice(&[crate::word::SET_CLOCK, block]);
            os.perform(X | os::WORD, &mut word)?;
            // A clock set across a changeover changes what the rules say.
            os.dst_changeover()
        }
        8 => os.read_current_time_zone(regs),
        SELECT => os.select_territory(regs.r[0]),
        DAYLIGHT_SAVING => os.daylight_saving(regs),
        CONVERT_TIME_FORMATS => os.convert_time_formats(regs),
        9 => {
            let time = os.read_five(regs.r[1])?;
            os.write_ordinals(regs.r[2], time)
        }
        _ if (FIRST_ENTRY..FIRST_ENTRY + len32(ENTRIES)).contains(&number) => {
            os.call_territory(regs.r[0], (number - FIRST_ENTRY) as usize, regs)
        }
        _ if number == CONVERT_TEXT_TO_STRING => Ok(()),
        _ => Err(os.own_error(own, UNKNOWN_SWI, &[])),
    }
}

/// The callback that says the Manager is loaded, for territories waiting to
/// register.
fn loaded(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let mut regs = Regs::default();
    regs.r[1] = service::TERRITORY_MANAGER_LOADED;
    os.service(&mut regs)?;
    Ok(())
}

/// Writes `text` and a new line.
fn line(os: &mut Os<'_>, text: &[u8]) -> Result<(), Fail> {
    os.write_text(text)?;
    os.write_text(b"\n\r")
}

/// `*Territories`: each loaded territory's number and name, the newest
/// first.
/// `*Territory`: the territory named or numbered selected, or the current
/// one's name written.
fn territory_command(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[1] == 0 {
        let current = os.world.territory.current;
        let name = os.territory_name(current)?;
        return line(os, &name);
    }
    let Some(number) = os.name_or_number(regs.r[0])? else {
        return Err(Fail::Error(crate::cli::configure::TOO_MANY_PARAMETERS));
    };
    os.select_territory(number)
}

fn territories_command(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let numbers: Vec<u32> = os
        .world
        .territory
        .territories
        .iter()
        .map(|t| t.number)
        .collect();
    for number in numbers {
        let text = os.territory_name(number)?;
        let mut out = crate::convert::Decimal::of(number.cast_signed())
            .as_bytes()
            .to_vec();
        out.push(b' ');
        out.extend_from_slice(&text);
        line(os, &out)?;
    }
    Ok(())
}

/// What a configuration keyword's R0 says: list its syntax, give its state,
/// or set it from the text.
enum Asked {
    Syntax,
    Status,
    Set(u32),
}

const fn asked(r0: u32) -> Asked {
    match r0 {
        0 => Asked::Syntax,
        1 => Asked::Status,
        text => Asked::Set(text),
    }
}

/// A token of the Manager's messages, written with a new line.
fn message_line(os: &mut Os<'_>, token: &[u8]) -> Result<(), Fail> {
    let buffer = os.world.territory.own.reply();
    let length = os.own_lookup(own, token, buffer, crate::msgtrans::REPLY_ROOM)?;
    let text = os.read_until(buffer, length as usize, |_| false)?.0;
    line(os, &text)
}

fn configure_territory(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match asked(regs.r[0]) {
        Asked::Syntax => message_line(os, b"ConfigureTerritorySyntax"),
        Asked::Status => {
            let configured = os.read_cmos(TERRITORY_CMOS)? ^ 1;
            let mut out = b"Territory  ".to_vec();
            out.extend_from_slice(crate::convert::Decimal::of(i32::from(configured)).as_bytes());
            line(os, &out)
        }
        Asked::Set(text) => {
            let Some(wanted) = os.name_or_number(text)? else {
                return Err(Fail::Error(crate::cli::configure::TOO_MANY_PARAMETERS));
            };
            os.write_cmos(TERRITORY_CMOS, u8::try_from(wanted).unwrap_or(u8::MAX) ^ 1)
        }
    }
}

fn configure_dst(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match asked(regs.r[0]) {
        Asked::Syntax => message_line(os, b"ConfigureDstSyntax"),
        Asked::Status => {
            let state = os.dst_state()?;
            line(
                os,
                if state & dst::AUTOMATIC != 0 {
                    b"AutoDST"
                } else if state != 0 {
                    b"DST"
                } else {
                    b"NoDST"
                },
            )
        }
        Asked::Set(_) => os.set_dst(true),
    }
}

fn configure_no_dst(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match asked(regs.r[0]) {
        Asked::Syntax | Asked::Status => Ok(()),
        Asked::Set(_) => os.set_dst(false),
    }
}

fn configure_auto_dst(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match asked(regs.r[0]) {
        Asked::Syntax | Asked::Status => Ok(()),
        Asked::Set(_) => os.set_dst_state(dst::AUTOMATIC),
    }
}

fn configure_time_zone(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match asked(regs.r[0]) {
        Asked::Syntax => message_line(os, b"ConfigureTimeZoneSyntax"),
        Asked::Status => {
            let minutes = 15 * i32::from(os.read_cmos(TIME_ZONE_CMOS)?.cast_signed());
            let sign = if minutes < 0 { b'-' } else { b'+' };
            let (hours, rest) = (minutes.unsigned_abs() / 60, minutes.unsigned_abs() % 60);
            let mut out = b"TimeZone   ".to_vec();
            out.push(sign);
            out.extend_from_slice(crate::convert::Decimal::of(hours.cast_signed()).as_bytes());
            out.push(b':');
            out.extend([rest / 10, rest % 10].map(|digit| b'0' + u8::try_from(digit).unwrap_or(0)));
            line(os, &out)
        }
        Asked::Set(text) => {
            let first = os.read_byte(text)?;
            let negative = first == b'-';
            let at = if negative || first == b'+' {
                text.wrapping_add(1)
            } else {
                text
            };
            let mut hours = Regs::default();
            hours.r[..3].copy_from_slice(&[0x2000_000A, at, 13]);
            os.perform(X | os::READ_UNSIGNED, &mut hours)?;
            let mut minutes = 0;
            if os.read_byte(hours.r[1])? == b':' {
                let mut read = Regs::default();
                read.r[..3].copy_from_slice(&[0x2000_000A, hours.r[1].wrapping_add(1), 59]);
                os.perform(X | os::READ_UNSIGNED, &mut read)?;
                minutes = read.r[2];
            }
            let quarters = i32::try_from((hours.r[2] << 2) | (minutes / 15)).unwrap_or(0);
            let quarters = if negative { -quarters } else { quarters };
            os.write_cmos(
                TIME_ZONE_CMOS,
                i8::try_from(quarters).unwrap_or(0).cast_unsigned(),
            )?;
            os.time_zone_changed()
        }
    }
}

#[cfg(test)]
mod tests;
