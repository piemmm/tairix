//! ScreenModes 0.74: `*LoadModeFile` reads a monitor definition file — its
//! title and each mode's size and timings — makes the current monitor type
//! 7, and from then on gives the timings for a selector of a size the file
//! defines (`Service_ModeExtension`), lists the file's modes
//! (`Service_EnumerateScreenModes`) and offers the file's nearest for a
//! numbered mode (`Service_ModeTranslation`). `ScreenModes_ReadInfo` 0
//! gives the monitor's title.

use alloc::vec::Vec;

use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::module::{Builtin, Command, CommandKind, Help, Resource, Services, Swis};
use tairix_riscos_kernel::msgtrans::{Own, OWN_DESCRIPTOR, REQUEST};
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs, Unwind, World};
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

/// Its messages, and where its workspace keeps their descriptor, the
/// file's name and a request's room.
const MESSAGES: &[u8] = include_bytes!("../resources/ScreenModes/Messages");
const MESSAGES_NAME: &[u8] = b"Resources:$.Resources.ScreenMode.Messages\0";
const OWN_AT: u32 = 0;
const NAME_AT: u32 = OWN_DESCRIPTOR;
const REQUEST_AT: u32 = (NAME_AT + len32(MESSAGES_NAME.len())).next_multiple_of(4);
const TITLE_AT: u32 = REQUEST_AT + REQUEST;
/// The most bytes of a monitor's title kept.
const TITLE_ROOM: u32 = 64;
/// A VIDC list of type 3 for the mode a service answers with.
const VIDC_AT: u32 = TITLE_AT + TITLE_ROOM;
const VIDC_WORDS: u32 = 17;
const WORKSPACE: u32 = VIDC_AT + 4 * VIDC_WORDS;

/// "Line %0 of %1 cannot be read as part of a monitor definition".
const BAD_MODE_FILE: Error = Error {
    number: 0xBE8,
    token: "BadModeFile",
};
/// "%0 is too large to be a monitor definition", a bound of the
/// emulator's own.
const MODE_FILE_TOO_LARGE: Error = Error {
    number: 0xBE8,
    token: "ModeFileTooLarge",
};

/// The services it takes.
mod service {
    pub(super) const MODE_EXTENSION: u32 = 0x50;
    pub(super) const MODE_TRANSLATION: u32 = 0x51;
    pub(super) const ENUMERATE_SCREEN_MODES: u32 = 0x8D;
}

/// ScreenModes.
pub static SCREEN_MODES: Builtin = Builtin {
    title: "ScreenModes",
    help_name: None,
    bridge: false,
    swis: Some(Swis {
        chunk: 0x4_87C0,
        prefix: "ScreenModes",
        names: &["ReadInfo"],
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
        file_type: 0xFFF,
        data: MESSAGES,
    }],
    code: None,
};

/// A mode a monitor definition file defines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModeLine {
    pub(crate) name: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Its horizontal sync, back porch, left border, display, right border
    /// and front porch, in pixels; and the same vertically, in lines.
    pub(crate) h: [u32; 6],
    pub(crate) v: [u32; 6],
    /// Its pixel rate in kHz and its syncs' polarities.
    pub(crate) pixel_rate: u32,
    pub(crate) sync: u32,
}

impl ModeLine {
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
}

fn own(world: &mut World) -> Option<&mut Own> {
    world.extension::<State>().map(|state| &mut state.own)
}

fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let workspace = os.rma_claim(WORKSPACE)?;
    os.write_word(regs.r[12], workspace)?;
    os.write_bytes(workspace + NAME_AT, MESSAGES_NAME)?;
    let own = Own::at(
        workspace + OWN_AT,
        workspace + NAME_AT,
        workspace + REQUEST_AT,
    );
    os.write_byte(workspace + TITLE_AT, 0)?;
    os.hold_extension(State {
        own,
        workspace,
        modes: Vec::new(),
    })
}

fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    if let Some(state) = os.release_extension::<State>() {
        os.rma_free(state.workspace)?;
    }
    Ok(())
}

/// `ScreenModes_ReadInfo` 0: R0 the loaded monitor's title, empty with none
/// loaded.
fn swi(os: &mut Os<'_>, _offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[0] != 0 {
        return Err(os.error(tairix_riscos_kernel::error::BAD_PARAMETERS));
    }
    let workspace = os.extension::<State>().map_or(0, |s| s.workspace);
    regs.r[0] = workspace + TITLE_AT;
    Ok(())
}

/// `*LoadModeFile`: the file read and its modes kept, the monitor type made
/// 7.
fn load_mode_file(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let name = os.read_text(regs.r[0], 256)?;
    let text = read_file(os, &name)?;
    let (title, modes) = match parse(&text) {
        Ok(read) => read,
        Err(Unread::NoMemory) => return Err(os.no_memory()),
        Err(Unread::Line(line)) => {
            let mut digits = [0; 12];
            let line = tairix_util::fmt::format_usize(line, &mut digits).as_bytes();
            return Err(os.own_error(own, BAD_MODE_FILE, &[line, &name]));
        }
    };
    let workspace = os.extension::<State>().map_or(0, |s| s.workspace);
    let title = &title[..title.len().min(TITLE_ROOM as usize - 1)];
    os.write_terminated(workspace + TITLE_AT, title, 0, TITLE_ROOM as usize)?;
    if let Some(state) = os.extension::<State>() {
        state.modes = modes;
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
    let buffer = os.rma_claim(length.max(4))?;
    let mut load = Regs::new([16, at, buffer, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let loaded = os.perform(X | FILE, &mut load);
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(length as usize).is_err() {
        os.rma_free(buffer)?;
        return Err(os.no_memory());
    }
    bytes.resize(length as usize, 0);
    let read = loaded.and_then(|()| os.read_bytes(buffer, &mut bytes));
    os.rma_free(buffer)?;
    read.map(|()| bytes)
}

/// Why a monitor definition file was not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unread {
    /// The line, from 1, it cannot be read past.
    Line(usize),
    /// There was no room for what it defines.
    NoMemory,
}

/// A monitor definition file's title and modes: `key:value` lines, `#` to
/// a line's end a comment, each mode between `startmode` and `endmode`.
pub(crate) fn parse(text: &[u8]) -> Result<(Vec<u8>, Vec<ModeLine>), Unread> {
    let mut reading = Reading::default();
    let mut last = 1;
    for (raw, at) in text.split(|&b| b == b'\n').zip(1..) {
        if !raw.trim_ascii().is_empty() {
            last = at;
        }
        for piece in raw.split(|&b| b == b'\r') {
            reading.line(piece, at)?;
        }
    }
    if reading.format.is_none() || reading.current.is_some() {
        return Err(Unread::Line(last));
    }
    reading
        .title
        .map(|title| (title, reading.modes))
        .ok_or(Unread::Line(last))
}

/// The longest key a monitor definition file has.
const LONGEST_KEY: usize = b"monitor_title".len();

/// A monitor definition file read so far.
#[derive(Default)]
struct Reading {
    title: Option<Vec<u8>>,
    format: Option<u32>,
    current: Option<ModeLine>,
    modes: Vec<ModeLine>,
}

impl Reading {
    /// Line `at` read, its format's line first.
    fn line(&mut self, raw: &[u8], at: usize) -> Result<(), Unread> {
        let bad = Unread::Line(at);
        let line = raw.split(|&b| b == b'#').next().unwrap_or(&[]).trim_ascii();
        if line.is_empty() {
            return Ok(());
        }
        if self.format.is_none()
            && !line
                .get(..11)
                .is_some_and(|key| key.eq_ignore_ascii_case(b"file_format"))
        {
            return Err(bad);
        }
        if line.eq_ignore_ascii_case(b"startmode") {
            if self.current.is_some() {
                return Err(bad);
            }
            self.current = Some(ModeLine {
                name: Vec::new(),
                width: 0,
                height: 0,
                h: [0; 6],
                v: [0; 6],
                pixel_rate: 0,
                sync: 0,
            });
            return Ok(());
        }
        if line.eq_ignore_ascii_case(b"endmode") {
            let mode = self.current.take().ok_or(bad)?;
            if mode.width == 0 || mode.height == 0 || self.modes.len() >= MOST_MODES {
                return Err(bad);
            }
            return tairix_util::fallible::try_push(&mut self.modes, mode)
                .map_err(|_| Unread::NoMemory);
        }
        let colon = line.iter().position(|&b| b == b':').ok_or(bad)?;
        let (key, value) = (line[..colon].trim_ascii(), line[colon + 1..].trim_ascii());
        let mut folded = [0; LONGEST_KEY];
        let folded = folded.get_mut(..key.len()).ok_or(bad)?;
        for (to, from) in folded.iter_mut().zip(key) {
            *to = from.to_ascii_lowercase();
        }
        let copy = |value: &[u8]| tairix_util::fallible::copied(value).ok_or(Unread::NoMemory);
        match (self.current.as_mut(), &*folded) {
            (None, b"file_format") => {
                self.format = Some(number(value).filter(|&n| n == 1).ok_or(bad)?);
            }
            (None, b"monitor_title") => self.title = Some(copy(value)?),
            (None, b"dpms_state" | b"lcd_support") => {}
            (Some(mode), b"mode_name") => mode.name = copy(value)?,
            (Some(mode), b"x_res") => mode.width = number(value).ok_or(bad)?,
            (Some(mode), b"y_res") => mode.height = number(value).ok_or(bad)?,
            (Some(mode), b"pixel_rate") => mode.pixel_rate = number(value).ok_or(bad)?,
            (Some(mode), b"sync_pol") => mode.sync = number(value).ok_or(bad)?,
            (Some(mode), b"h_timings") => mode.h = six(value).ok_or(bad)?,
            (Some(mode), b"v_timings") => mode.v = six(value).ok_or(bad)?,
            _ => return Err(bad),
        }
        Ok(())
    }
}

fn number(digits: &[u8]) -> Option<u32> {
    core::str::from_utf8(digits).ok()?.parse().ok()
}

fn six(value: &[u8]) -> Option<[u32; 6]> {
    let mut parts = value.split(|&b| b == b',');
    let mut six = [0; 6];
    for slot in &mut six {
        *slot = number(parts.next()?.trim_ascii())?;
    }
    parts.next().is_none().then_some(six)
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
                m.width == width
                    && m.height == height
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
                u64::from(m.width.abs_diff(wanted.0)) + u64::from(m.height.abs_diff(wanted.1))
            });
            if let Some(mode) = nearest {
                let words = [
                    1,
                    mode.width,
                    mode.height,
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
            let entry = [size, 1, mode.width, mode.height, depth, mode.frame_rate()]
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
mod tests {
    use super::*;

    const FILE: &[u8] = b"file_format:1\nmonitor_title:Test monitor\n# a comment\n\
startmode\nmode_name:VGA\nx_res:640\ny_res:480\npixel_rate:25175\n\
h_timings:96,40,8,640,8,8\nv_timings:2,25,8,480,8,2\nsync_pol:3\nendmode\n";

    #[test]
    fn a_monitor_file_s_title_and_modes_are_read() {
        let (title, modes) = parse(FILE).expect("read");
        assert_eq!(title, b"Test monitor");
        assert_eq!(modes.len(), 1);
        assert_eq!((modes[0].width, modes[0].height), (640, 480));
        assert_eq!(modes[0].frame_rate(), 60);
        assert_eq!(modes[0].name, b"VGA");
    }

    #[test]
    fn a_malformed_monitor_file_is_refused_at_the_line_it_fails() {
        let refused = |text: &[u8]| parse(text).err();
        assert_eq!(
            refused(b"garbage line\nfile_format:1\n"),
            Some(Unread::Line(1)),
            "its format's line comes first"
        );
        assert_eq!(
            refused(b"# a comment\n\nfile_format:2\nmonitor_title:x\n"),
            Some(Unread::Line(3))
        );
        assert_eq!(
            refused(b"file_format:1\nmonitor_title:x\nstartmode\n"),
            Some(Unread::Line(3)),
            "a mode left open"
        );
        assert_eq!(
            refused(b"file_format:1\nmonitor_title:x\nstartmode\nx_res:1\nendmode\n"),
            Some(Unread::Line(5))
        );
        assert_eq!(
            refused(b"file_format:1\r\nmonitor_title:x\r\nwhat:3\r\n"),
            Some(Unread::Line(3))
        );
        assert_eq!(
            refused(b"file_format:1\n"),
            Some(Unread::Line(1)),
            "no title"
        );
        assert_eq!(
            refused(b"file_format:1\nmonitor_title:x\nmonitor_title_and_more:y\n"),
            Some(Unread::Line(3)),
            "a key longer than any there is"
        );
        assert_eq!(
            refused(b"file_format:1\nmonitor_title:x\nstartmode\nh_timings:1,2,3,4,5,6,7\n"),
            Some(Unread::Line(4))
        );
    }

    #[test]
    fn a_key_s_case_and_its_spacing_do_not_matter() {
        let (title, modes) = parse(b"FILE_FORMAT : 1\nMonitor_Title:  Wide \nStartMode\nMODE_NAME:W\nX_Res:800\n\
Y_RES:600\nPixel_Rate:40000\nH_Timings: 128, 88, 0, 800, 0, 40\nv_timings:4,23,0,600,0,1\nENDMODE\n")
        .expect("read");
        assert_eq!(title, b"Wide");
        assert_eq!(
            (modes[0].name.as_slice(), modes[0].width, modes[0].height),
            (b"W".as_slice(), 800, 600)
        );
        assert_eq!(modes[0].h, [128, 88, 0, 800, 0, 40]);
    }
}
