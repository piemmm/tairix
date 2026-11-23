//! ScreenModes 0.74: `*LoadModeFile` reads a monitor definition file — its
//! title and each mode's size and timings — makes the current monitor type
//! 7, and from then on gives the timings for a selector of a size the file
//! defines (`Service_ModeExtension`), lists the file's modes
//! (`Service_EnumerateScreenModes`) and offers the file's nearest for a
//! numbered mode (`Service_ModeTranslation`). `ScreenModes_ReadInfo` gives
//! the file's monitor title and DPMS state and the speaker mask a display's
//! EDID describes, `ScreenModes_EnumerateAudioFormats` the sound formats it
//! describes, and `ScreenModes_Features` what the module can do. No display
//! here describes itself, so there is never a speaker or a format.

use alloc::vec::Vec;

use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::module::{Builtin, Command, CommandKind, Help, Resource, Services, Swis};
use tairix_riscos_kernel::msgtrans::{Own, OwnFile};
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs, Unwind, World};
use tairix_riscos_kernel::service;
use tairix_riscos_kernel::swi::os::FILE;
use tairix_riscos_kernel::swi::X;

/// The monitor type a loaded file makes current.
pub(crate) const FILE_MONITOR: u32 = 7;
/// The most bytes a monitor definition file may hold.
const MOST_FILE: u32 = 0x4_0000;
/// The most modes the drivers keep from one.
const MOST_MODES: usize = 256;
/// The depths each of a file's modes is offered in.
const DEPTHS: [u32; 6] = [0, 1, 2, 3, 4, 5];

/// Its messages, and their file, kept at its workspace's start.
const MESSAGES: &[u8] = include_bytes!("../resources/ScreenModes/Messages");
const MESSAGES_FILE: OwnFile = OwnFile::named(b"Resources:$.Resources.ScreenMode.Messages\0");
const TITLE_AT: u32 = MESSAGES_FILE.room();
/// A monitor's title, kept with its terminator.
const TITLE_ROOM: u32 = TITLE_MOST + 1;
/// A VIDC list of type 3 for the mode a service answers with.
const VIDC_AT: u32 = TITLE_AT + TITLE_ROOM;
const VIDC_WORDS: u32 = 17;
const WORKSPACE: u32 = VIDC_AT + 4 * VIDC_WORDS;

/// "Line %0 of %1 cannot be read as part of a monitor definition".
const BAD_MODE_FILE: Error = Error {
    number: 0xBE8,
    token: "BadModeFile",
};
/// The monitor definition errors RISC OS 5 tells apart, each with the line
/// and the file as parameters.
const MODE_SIZE_DISAGREES: Error = Error {
    number: 0xBE2,
    token: "ModeSizeDisagrees",
};
const MODE_FILE_ENDS: Error = Error {
    number: 0xBE3,
    token: "ModeFileEnds",
};
const MODE_NUMBER_LARGE: Error = Error {
    number: 0xBE5,
    token: "ModeNumberLarge",
};
const MODE_LINE_TRAILS: Error = Error {
    number: 0xBE6,
    token: "ModeLineTrails",
};
const MODE_NUMBER_EXPECTED: Error = Error {
    number: 0xBE7,
    token: "ModeNumberExpected",
};
const MODE_KEY_REPEATED: Error = Error {
    number: 0xBE9,
    token: "ModeKeyRepeated",
};
const MODE_UNNAMED: Error = Error {
    number: 0xBEA,
    token: "ModeUnnamed",
};
const MODE_INCOMPLETE: Error = Error {
    number: 0xBEB,
    token: "ModeIncomplete",
};
const MODE_KEY_UNKNOWN: Error = Error {
    number: 0xBEC,
    token: "ModeKeyUnknown",
};
const MODE_KEY_MISPLACED: Error = Error {
    number: 0xBED,
    token: "ModeKeyMisplaced",
};
const MODE_FILE_FORMAT: Error = Error {
    number: 0xBEE,
    token: "ModeFileFormat",
};
const MONITOR_TITLE_BLANK: Error = Error {
    number: 0xBEF,
    token: "MonitorTitleBlank",
};
const MODE_FILE_EMPTY: Error = Error {
    number: 0xBF2,
    token: "ModeFileEmpty",
};
const MODE_NO_SIZE: Error = Error {
    number: 0xBF3,
    token: "ModeNoSize",
};
/// "%0 is too large to be a monitor definition", a bound of the
/// emulator's own.
const MODE_FILE_TOO_LARGE: Error = Error {
    number: 0xBE8,
    token: "ModeFileTooLarge",
};
/// "No monitor definition file is loaded".
const NO_MONITOR: Error = Error {
    number: 0xBF4,
    token: "NoMonitorFile",
};
/// "`ScreenModes_ReadInfo` has no such reason".
const BAD_INFO_REASON: Error = Error {
    number: 0xBF6,
    token: "BadInfoReason",
};
/// "`ScreenModes_EnumerateAudioFormats` has no such reason".
const BAD_AUDIO_REASON: Error = Error {
    number: 0xBFC,
    token: "BadAudioReason",
};

use tairix_riscos_kernel::swi::chunk::SCREEN_MODES as CHUNK;
/// ScreenModes' SWIs' offsets in [`CHUNK`].
const READ_INFO: u32 = 0;
const ENUMERATE_AUDIO_FORMATS: u32 = 1;
const FEATURES: u32 = 2;
/// `ScreenModes_ReadInfo`'s reasons.
const INFO_TITLE: u32 = 0;
const INFO_DPMS: u32 = 1;
const INFO_SPEAKERS: u32 = 2;
/// `ScreenModes_EnumerateAudioFormats`' last reason: 0 asks for raw
/// descriptors, 1 for friendly ones.
const AUDIO_FRIENDLY: u32 = 1;
/// The flags `ScreenModes_Features` gives: RISC OS 5's one, set.
const FEATURE_FLAGS: u32 = 1;

/// ScreenModes.
pub static SCREEN_MODES: Builtin = Builtin {
    title: "ScreenModes",
    help_name: None,
    bridge: false,
    swis: Some(Swis {
        chunk: CHUNK,
        prefix: "ScreenModes",
        names: &["ReadInfo", "EnumerateAudioFormats", "Features"],
        handler: swi,
    }),
    services: Some(Services {
        numbers: &[
            service::MODE_EXTENSION,
            service::MODE_TRANSLATION,
            service::ENUMERATE_SCREEN_MODES,
        ],
        handler: serve,
    }),
    commands: &[Command {
        name: "LoadModeFile",
        kind: CommandKind::Command,
        min: 1,
        max: 1,
        translate: 1,
        help: Help::Text(
            "Loads a monitor definition file, whose modes become the ones the screen offers.",
        ),
        syntax: Some("Syntax: *LoadModeFile <file>"),
        run: Some(load_mode_file),
    }],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[],
    vectors: &[],
    resources: &[Resource {
        name: "Resources.ScreenMode.Messages",
        file_type: tairix_riscos_kernel::filing::filetype::TEXT,
        data: MESSAGES,
    }],
    code: None,
};

/// A mode a monitor definition file defines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModeLine {
    pub(crate) name: Vec<u8>,
    /// Its horizontal sync, back porch, left border, display, right border
    /// and front porch, in pixels; and the same vertically, in lines.
    pub(crate) h: [u32; 6],
    pub(crate) v: [u32; 6],
    /// Its pixel rate in kHz and its syncs' polarities.
    pub(crate) pixel_rate: u32,
    pub(crate) sync: u32,
}

impl ModeLine {
    /// Its width in pixels and height in lines: its timings' display.
    pub(crate) const fn width(&self) -> u32 {
        self.h[DISPLAY]
    }

    pub(crate) const fn height(&self) -> u32 {
        self.v[DISPLAY]
    }

    /// Its frame rate, in hertz, rounded.
    pub(crate) fn frame_rate(&self) -> u32 {
        let sum = |timings: &[u32; 6]| timings.iter().map(|&t| u128::from(t)).sum::<u128>();
        let total = sum(&self.h) * sum(&self.v);
        if total == 0 {
            return 0;
        }
        u32::try_from((u128::from(self.pixel_rate) * 1000 + total / 2) / total).unwrap_or(0)
    }
}

/// What ScreenModes holds while it is active.
pub struct State {
    own: Own,
    workspace: u32,
    modes: Vec<ModeLine>,
    /// What the loaded file gives beyond its modes; none with no file
    /// loaded.
    loaded: Option<Monitor>,
}

/// What a loaded monitor definition file gives beyond its modes.
#[derive(Clone, Copy)]
struct Monitor {
    /// Its DPMS state, if it gives one.
    dpms: Option<u32>,
}

fn own(world: &mut World) -> Option<&mut Own> {
    world.extension::<State>().map(|state| &mut state.own)
}

fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let workspace = os.rma_claim(WORKSPACE)?;
    os.write_word(regs.r[12], workspace)?;
    let own = MESSAGES_FILE.place(os, workspace)?;
    os.write_byte(workspace + TITLE_AT, 0)?;
    os.hold_extension(State {
        own,
        workspace,
        modes: Vec::new(),
        loaded: None,
    })
}

/// Its messages closed; its workspace the kernel frees from its private
/// word.
fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.close_own(own)?;
    os.release_extension::<State>();
    Ok(())
}

/// ScreenModes' SWIs.
fn swi(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    match offset {
        READ_INFO => read_info(os, regs),
        ENUMERATE_AUDIO_FORMATS => enumerate_audio_formats(os, regs),
        FEATURES => {
            regs.r[0] = FEATURE_FLAGS;
            Ok(())
        }
        _ => Err(os.no_such_swi(CHUNK + offset)),
    }
}

/// The workspace and what the loaded file gives; "no monitor file" with
/// none loaded.
fn loaded(os: &mut Os<'_>) -> Result<(u32, Monitor), Fail> {
    match os
        .extension::<State>()
        .and_then(|state| Some((state.workspace, state.loaded?)))
    {
        Some(loaded) => Ok(loaded),
        None => Err(os.own_error(own, NO_MONITOR, &[])),
    }
}

/// `ScreenModes_ReadInfo`: R0 0 the monitor's title, 1 its DPMS state, -1
/// where the file gives none, 2 the speaker mask a display describes and
/// which of its bits it describes, in R1: none here.
fn read_info(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[0] > INFO_SPEAKERS {
        return Err(os.own_error(own, BAD_INFO_REASON, &[]));
    }
    let (workspace, monitor) = loaded(os)?;
    match regs.r[0] {
        INFO_TITLE => regs.r[0] = workspace + TITLE_AT,
        INFO_DPMS => regs.r[0] = monitor.dpms.unwrap_or(u32::MAX),
        _ => regs.r[..2].copy_from_slice(&[0, 0]),
    }
    Ok(())
}

/// `ScreenModes_EnumerateAudioFormats`: the formats a display describes,
/// R0 0 raw, 1 friendly. None is described here, so the enumeration ends at
/// once, R1 and R2 -1. The file is asked for before the reason is read, as
/// RISC OS 5 does.
fn enumerate_audio_formats(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    loaded(os)?;
    if regs.r[0] > AUDIO_FRIENDLY {
        return Err(os.own_error(own, BAD_AUDIO_REASON, &[]));
    }
    regs.r[1..3].copy_from_slice(&[u32::MAX, u32::MAX]);
    Ok(())
}

/// `*LoadModeFile`: the file read and its modes kept, the monitor type made
/// 7.
fn load_mode_file(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let name = os.read_text(regs.r[0], 256)?;
    let text = read_file(os, &name)?;
    let definition = match parse(&text) {
        Ok(definition) => definition,
        Err(Unread::NoMemory) => return Err(os.no_memory()),
        Err(Unread::Line(line, fault)) => {
            let mut digits = [0; 12];
            let line = tairix_util::fmt::format_usize(line, &mut digits).as_bytes();
            return Err(os.own_error(own, fault.error(), &[line, &name]));
        }
    };
    let workspace = os.extension::<State>().map_or(0, |s| s.workspace);
    os.write_terminated(
        workspace + TITLE_AT,
        &definition.title,
        0,
        TITLE_ROOM as usize,
    )?;
    if let Some(state) = os.extension::<State>() {
        state.modes = definition.modes;
        state.loaded = Some(Monitor {
            dpms: definition.dpms,
        });
    }
    crate::select::set_monitor(os, FILE_MONITOR);
    Ok(())
}

/// The file named `name`, whole, at most [`MOST_FILE`] bytes.
fn read_file(os: &mut Os<'_>, name: &[u8]) -> Result<Vec<u8>, Fail> {
    let Some(named) = tairix_util::fallible::joined(&[name, &[0]]) else {
        return Err(os.no_memory());
    };
    let at = os.scratch_text(&named)?;
    let mut info = Regs::new([17, at, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    os.perform(X | FILE, &mut info)?;
    if info.r[0] != 1 {
        os.object_error(at, info.r[0])?;
        return Err(os.own_error(own, BAD_MODE_FILE, &[b"1", name]));
    }
    let length = info.r[4];
    if length > MOST_FILE {
        return Err(os.own_error(own, MODE_FILE_TOO_LARGE, &[name]));
    }
    os.read_through_rma(length, |os, buffer| {
        let mut load = Regs::new([16, at, buffer, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        os.perform(X | FILE, &mut load)
    })
}

/// Why a monitor definition file was not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unread {
    /// The line, from 1, it fails at, and how.
    Line(usize, Fault),
    /// There was no room for what it defines.
    NoMemory,
}

/// How a monitor definition file fails, each as RISC OS 5 tells it apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    /// A line out of place or not understood, a top-level key given twice,
    /// or no monitor title.
    Unreadable,
    /// A mode's size disagrees with its timings.
    SizeDisagrees,
    /// The file ends inside a mode.
    EndsInMode,
    /// A number past what its key takes.
    TooLarge,
    /// More after the line's word or number.
    Trailing,
    /// No number where the line needs one.
    NoNumber,
    /// A key its mode already has.
    Repeated,
    /// A mode with no name.
    Unnamed,
    /// A mode without its pixel rate, a timing or its syncs.
    Incomplete,
    /// A key no mode has.
    UnknownInMode,
    /// A line only the file's top level takes, inside a mode.
    NotInMode,
    /// A format other than 1.
    Format,
    /// A monitor title of nothing.
    BlankTitle,
    /// No modes.
    NoModes,
    /// A mode of no size, or no pixel rate.
    NoSize,
}

impl Fault {
    /// The error it is refused with.
    const fn error(self) -> Error {
        match self {
            Self::Unreadable => BAD_MODE_FILE,
            Self::SizeDisagrees => MODE_SIZE_DISAGREES,
            Self::EndsInMode => MODE_FILE_ENDS,
            Self::TooLarge => MODE_NUMBER_LARGE,
            Self::Trailing => MODE_LINE_TRAILS,
            Self::NoNumber => MODE_NUMBER_EXPECTED,
            Self::Repeated => MODE_KEY_REPEATED,
            Self::Unnamed => MODE_UNNAMED,
            Self::Incomplete => MODE_INCOMPLETE,
            Self::UnknownInMode => MODE_KEY_UNKNOWN,
            Self::NotInMode => MODE_KEY_MISPLACED,
            Self::Format => MODE_FILE_FORMAT,
            Self::BlankTitle => MONITOR_TITLE_BLANK,
            Self::NoModes => MODE_FILE_EMPTY,
            Self::NoSize => MODE_NO_SIZE,
        }
    }
}

/// What a monitor definition file defines.
#[derive(Debug)]
pub(crate) struct Definition {
    /// Its monitor's title, at most [`TITLE_MOST`] bytes of it.
    pub(crate) title: Vec<u8>,
    pub(crate) dpms: Option<u32>,
    pub(crate) modes: Vec<ModeLine>,
}

/// A monitor definition file read: `key:value` lines, keys in any case, a
/// line whose first character is `#` a comment, each mode between
/// `startmode` and `endmode`; refused as RISC OS 5 refuses it.
pub(crate) fn parse(text: &[u8]) -> Result<Definition, Unread> {
    let mut reading = Reading::default();
    let mut last = 1;
    for (raw, at) in text.split(|&b| b == b'\n').zip(1..) {
        if !raw.trim_ascii().is_empty() {
            last = at;
        }
        for piece in raw.split(|&b| b == b'\r') {
            reading.line(piece).map_err(|refused| refused.at(at))?;
        }
    }
    let ended = |fault| Unread::Line(last, fault);
    if reading.current.is_some() {
        return Err(ended(Fault::EndsInMode));
    }
    let (true, Some(title)) = (reading.format, reading.title) else {
        return Err(ended(Fault::Unreadable));
    };
    if reading.modes.is_empty() {
        return Err(ended(Fault::NoModes));
    }
    Ok(Definition {
        title,
        dpms: reading.dpms,
        modes: reading.modes,
    })
}

/// The most bytes of a monitor's title kept, as RISC OS 5 keeps them.
const TITLE_MOST: u32 = 31;
/// The most `lcd_support` takes.
const MOST_LCD_SUPPORT: u32 = 1;
/// The most `sync_pol` takes: a bit for each sync's polarity.
const MOST_SYNC: u32 = 3;
/// Where the display's extent is among a mode's timings.
const DISPLAY: usize = 3;

/// A line's refusal: how it fails, or no room for what it gives.
enum Refused {
    Fault(Fault),
    NoMemory,
}

impl From<Fault> for Refused {
    fn from(fault: Fault) -> Self {
        Self::Fault(fault)
    }
}

impl Refused {
    /// The refusal as the file's, at line `at`.
    const fn at(self, at: usize) -> Unread {
        match self {
            Self::Fault(fault) => Unread::Line(at, fault),
            Self::NoMemory => Unread::NoMemory,
        }
    }
}

/// A monitor definition file read so far.
#[derive(Default)]
struct Reading {
    format: bool,
    title: Option<Vec<u8>>,
    dpms: Option<u32>,
    lcd_support: bool,
    current: Option<ModeReading>,
    modes: Vec<ModeLine>,
}

/// A mode read so far: each value its lines have given.
#[derive(Default)]
struct ModeReading {
    name: Option<Vec<u8>>,
    width: Option<u32>,
    height: Option<u32>,
    pixel_rate: Option<u32>,
    h: Option<[u32; 6]>,
    v: Option<[u32; 6]>,
    sync: Option<u32>,
}

/// A monitor definition file's words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    FileFormat,
    MonitorTitle,
    DpmsState,
    LcdSupport,
    StartMode,
    EndMode,
    ModeName,
    XRes,
    YRes,
    PixelRate,
    HTimings,
    VTimings,
    SyncPol,
}

impl Key {
    /// Each word as a file spells it, in any case.
    const SPELT: [(&'static [u8], Self); 13] = [
        (b"file_format", Self::FileFormat),
        (b"monitor_title", Self::MonitorTitle),
        (b"dpms_state", Self::DpmsState),
        (b"lcd_support", Self::LcdSupport),
        (b"startmode", Self::StartMode),
        (b"endmode", Self::EndMode),
        (b"mode_name", Self::ModeName),
        (b"x_res", Self::XRes),
        (b"y_res", Self::YRes),
        (b"pixel_rate", Self::PixelRate),
        (b"h_timings", Self::HTimings),
        (b"v_timings", Self::VTimings),
        (b"sync_pol", Self::SyncPol),
    ];

    fn of(word: &[u8]) -> Option<Self> {
        Self::SPELT
            .iter()
            .find(|(spelt, _)| word.eq_ignore_ascii_case(spelt))
            .map(|&(_, key)| key)
    }

    /// Whether it belongs to the file rather than to a mode.
    const fn top_level(self) -> bool {
        matches!(
            self,
            Self::FileFormat | Self::MonitorTitle | Self::DpmsState | Self::LcdSupport
        )
    }
}

/// A key given once: `value` into `slot`, or [`Fault::Repeated`] where it
/// was given already.
fn once<T>(slot: &mut Option<T>, value: T) -> Result<(), Fault> {
    if slot.is_some() {
        return Err(Fault::Repeated);
    }
    *slot = Some(value);
    Ok(())
}

impl Reading {
    /// One line read, its format's line first.
    fn line(&mut self, raw: &[u8]) -> Result<(), Refused> {
        let line = raw.trim_ascii();
        if line.is_empty() || line.starts_with(b"#") {
            return Ok(());
        }
        let word = line
            .iter()
            .position(|&b| !(b.is_ascii_alphanumeric() || b == b'_'))
            .unwrap_or(line.len());
        let (word, rest) = line.split_at(word);
        let rest = rest.trim_ascii_start();
        let key = Key::of(word);
        if !self.format && key != Some(Key::FileFormat) {
            return Err(Fault::Unreadable.into());
        }
        if let Some(start @ (Key::StartMode | Key::EndMode)) = key {
            return self.boundary(start == Key::StartMode, rest);
        }
        let value = rest
            .strip_prefix(b":")
            .ok_or(Fault::Unreadable)?
            .trim_ascii();
        let copy = |value: &[u8]| tairix_util::fallible::copied(value).ok_or(Refused::NoMemory);
        match (self.current.as_mut(), key) {
            (Some(_), Some(key)) if key.top_level() => return Err(Fault::NotInMode.into()),
            (Some(mode), Some(Key::ModeName)) => once(&mut mode.name, copy(value)?)?,
            (Some(mode), Some(Key::XRes)) => once(&mut mode.width, single(value)?)?,
            (Some(mode), Some(Key::YRes)) => once(&mut mode.height, single(value)?)?,
            (Some(mode), Some(Key::PixelRate)) => once(&mut mode.pixel_rate, single(value)?)?,
            (Some(mode), Some(Key::HTimings)) => once(&mut mode.h, six(value)?)?,
            (Some(mode), Some(Key::VTimings)) => once(&mut mode.v, six(value)?)?,
            (Some(mode), Some(Key::SyncPol)) => {
                let sync = single(value)?;
                if sync > MOST_SYNC {
                    return Err(Fault::TooLarge.into());
                }
                once(&mut mode.sync, sync)?;
            }
            (Some(_), _) => return Err(Fault::UnknownInMode.into()),
            (None, Some(Key::FileFormat)) => {
                if self.format {
                    return Err(Fault::Unreadable.into());
                }
                if single(value)? != 1 {
                    return Err(Fault::Format.into());
                }
                self.format = true;
            }
            (None, Some(Key::MonitorTitle)) => {
                if self.title.is_some() {
                    return Err(Fault::Unreadable.into());
                }
                if value.is_empty() {
                    return Err(Fault::BlankTitle.into());
                }
                self.title = Some(copy(&value[..value.len().min(TITLE_MOST as usize)])?);
            }
            (None, Some(Key::DpmsState)) => {
                if self.dpms.is_some() {
                    return Err(Fault::Unreadable.into());
                }
                self.dpms = Some(single(value)?);
            }
            (None, Some(Key::LcdSupport)) => {
                if self.lcd_support || single(value)? > MOST_LCD_SUPPORT {
                    return Err(Fault::Unreadable.into());
                }
                self.lcd_support = true;
            }
            (None, _) => return Err(Fault::Unreadable.into()),
        }
        Ok(())
    }

    /// `startmode`, or `endmode` where `start` is false, with `rest` after
    /// it on its line.
    fn boundary(&mut self, start: bool, rest: &[u8]) -> Result<(), Refused> {
        match (start, self.current.take()) {
            (true, Some(_)) => Err(Fault::NotInMode.into()),
            (false, None) => Err(Fault::Unreadable.into()),
            _ if !rest.is_empty() => Err(Fault::Trailing.into()),
            (true, None) => {
                self.current = Some(ModeReading::default());
                Ok(())
            }
            (false, Some(mode)) => {
                if self.modes.len() >= MOST_MODES {
                    return Err(Fault::Unreadable.into());
                }
                tairix_util::fallible::try_push(&mut self.modes, mode.ended()?)
                    .map_err(|_| Refused::NoMemory)
            }
        }
    }
}

impl ModeReading {
    /// The mode at its `endmode`, its values checked as RISC OS 5 checks
    /// them there: its size is its timings' display, which a given `x_res`
    /// or `y_res` must agree with.
    fn ended(self) -> Result<ModeLine, Fault> {
        let name = self.name.ok_or(Fault::Unnamed)?;
        let (Some(pixel_rate), Some(h), Some(v), Some(sync)) =
            (self.pixel_rate, self.h, self.v, self.sync)
        else {
            return Err(Fault::Incomplete);
        };
        let zero = |given: Option<u32>| given == Some(0);
        if zero(self.width)
            || zero(self.height)
            || pixel_rate == 0
            || h[DISPLAY] == 0
            || v[DISPLAY] == 0
        {
            return Err(Fault::NoSize);
        }
        let disagrees = |given: Option<u32>, display: u32| given.is_some_and(|n| n != display);
        if disagrees(self.width, h[DISPLAY]) || disagrees(self.height, v[DISPLAY]) {
            return Err(Fault::SizeDisagrees);
        }
        Ok(ModeLine {
            name,
            h,
            v,
            pixel_rate,
            sync,
        })
    }
}

/// The decimal number `text` starts with, and what follows it.
fn number(text: &[u8]) -> Result<(u32, &[u8]), Fault> {
    let digits = text.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return Err(Fault::NoNumber);
    }
    let (digits, rest) = text.split_at(digits);
    let value = digits
        .iter()
        .try_fold(0u32, |n, &d| {
            n.checked_mul(10)?.checked_add(u32::from(d - b'0'))
        })
        .ok_or(Fault::TooLarge)?;
    Ok((value, rest))
}

/// A value of one number and nothing more.
fn single(value: &[u8]) -> Result<u32, Fault> {
    match number(value)? {
        (n, rest) if rest.trim_ascii().is_empty() => Ok(n),
        _ => Err(Fault::Trailing),
    }
}

/// A value of six numbers, commas between them, and nothing more.
fn six(value: &[u8]) -> Result<[u32; 6], Fault> {
    let mut six = [0; 6];
    let mut rest = value;
    for (i, slot) in six.iter_mut().enumerate() {
        if i > 0 {
            rest = rest
                .trim_ascii_start()
                .strip_prefix(b",")
                .ok_or(Fault::NoNumber)?;
        }
        let (n, after) = number(rest.trim_ascii_start())?;
        *slot = n;
        rest = after;
    }
    if rest.trim_ascii().is_empty() {
        Ok(six)
    } else {
        Err(Fault::Trailing)
    }
}

/// Its services, while a file's monitor is current: the timings for a
/// selector of a size the file defines, the file's modes enumerated, and
/// its nearest for a numbered mode.
#[expect(clippy::unnecessary_wraps, reason = "a service handler's signature")]
fn serve(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Unwind> {
    if crate::select::monitor(os) == FILE_MONITOR {
        os.with_extension::<State, _>(|state, os| answer(os, regs, &state.modes, state.workspace));
    }
    Ok(())
}

/// The service in `regs` answered from `modes`, a VIDC list made at
/// `workspace`.
fn answer(os: &mut Os<'_>, regs: &mut Regs, modes: &[ModeLine], workspace: u32) {
    let list = workspace + VIDC_AT;
    match regs.r[1] {
        service::MODE_EXTENSION if regs.r[2] >= 0x100 => {
            let selector = regs.r[2];
            let word =
                |os: &mut Os<'_>, offset: u32| os.read_word(selector.wrapping_add(offset)).ok();
            let (Some(width), Some(height)) = (word(os, 4), word(os, 8)) else {
                return;
            };
            let depth = word(os, 12).unwrap_or(0);
            let rate = word(os, 16).unwrap_or(u32::MAX);
            let found = modes.iter().find(|m| {
                m.width() == width
                    && m.height() == height
                    && (rate == u32::MAX || m.frame_rate() == rate)
            });
            if let Some(mode) = found {
                let words = [3, depth].into_iter().chain(mode.h).chain(mode.v).chain([
                    mode.pixel_rate,
                    mode.sync,
                    u32::MAX,
                ]);
                if write_words(os, list, words) {
                    regs.r[1] = 0;
                    regs.r[3] = list;
                    regs.r[4] = 0;
                }
            }
        }
        service::MODE_TRANSLATION => {
            let Some(numbered) = tairix_vdu::mode::numbered(regs.r[2] & 0x7F) else {
                return;
            };
            let wanted = (u32::from(numbered.width), u32::from(numbered.height));
            let nearest = modes.iter().min_by_key(|m| {
                u64::from(m.width().abs_diff(wanted.0)) + u64::from(m.height().abs_diff(wanted.1))
            });
            if let Some(mode) = nearest {
                let words = [
                    1,
                    mode.width(),
                    mode.height(),
                    u32::from(numbered.log2_bpp),
                    u32::MAX,
                    u32::MAX,
                ];
                if write_words(os, list, words) {
                    regs.r[1] = 0;
                    regs.r[2] = list;
                }
            }
        }
        service::ENUMERATE_SCREEN_MODES => enumerate(os, regs, modes),
        _ => {}
    }
}

/// Writes `words` from `at`, answering whether every one was written.
fn write_words(os: &mut Os<'_>, at: u32, words: impl IntoIterator<Item = u32>) -> bool {
    (0u32..)
        .zip(words)
        .all(|(i, word)| os.write_word(at.wrapping_add(4 * i), word).is_ok())
}

/// `Service_EnumerateScreenModes` over the file's modes, each in every
/// depth: R2 counts down the entries to skip, R6 and R7 the block and the
/// room left in it.
fn enumerate(os: &mut Os<'_>, regs: &mut Regs, modes: &[ModeLine]) {
    let (mut skip, mut block, mut room) = (regs.r[2], regs.r[6], regs.r[7]);
    for mode in modes {
        for depth in DEPTHS {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let name_words = len32(mode.name.len() + 1).div_ceil(4);
            let size = 24 + 4 * name_words;
            if block == 0 {
                room = room.wrapping_sub(size);
                continue;
            }
            if room < size {
                regs.r[1] = 0;
                regs.r[6] = block;
                regs.r[7] = room;
                return;
            }
            let name = mode
                .name
                .chunks(4)
                .map(|bytes| {
                    let mut word = [0; 4];
                    word[..bytes.len()].copy_from_slice(bytes);
                    u32::from_le_bytes(word)
                })
                .chain(core::iter::repeat(0))
                .take(name_words as usize);
            let entry = [
                size,
                1,
                mode.width(),
                mode.height(),
                depth,
                mode.frame_rate(),
            ]
            .into_iter()
            .chain(name);
            if !write_words(os, block, entry) {
                return;
            }
            block = block.wrapping_add(size);
            room -= size;
            regs.r[2] = regs.r[2].wrapping_sub(1);
        }
    }
    regs.r[1] = 0;
    regs.r[6] = block;
    regs.r[7] = room;
}

#[cfg(test)]
mod tests;
