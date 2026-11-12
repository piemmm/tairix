//! `FDISK [n [/PRI:m] [/EXT:m] [/LOG:m]] [/Q]`: the partitions of the
//! machine's fixed disks set up as DOS 4.0's `FDISK` sets them up — through
//! its full-screen menus, or with a disk and sizes in megabytes on the
//! command line, the partitions made at once. Partitions changed so that
//! drives' letters may have changed restart the machine, unless `/Q` asks
//! it not to.
//!
//! Where DOS 4.0's code is at odds with what it plainly means, the meaning
//! is followed: logical drives can be made while all four entries are in
//! use, a primary partition made from the command line is set active only
//! on the first disk, a failed write is said rather than cleared away by
//! the restart, and an error reading the disks is said once the screen is
//! given back.

mod disk;
mod input;
mod menus;
mod screen;
pub(crate) mod table;
mod text;
mod views;

pub use disk::{FdiskRead, FdiskSpace, FdiskWrite};
pub use input::{FdiskAnswer, FdiskLabel, FdiskSize, FdiskWait};
pub use menus::{
    FdiskActive, FdiskCreate, FdiskDelete, FdiskDeleteExtended, FdiskDeleteLogical,
    FdiskDeletePrimary, FdiskDisplay, FdiskExtended, FdiskLogical, FdiskMenu, FdiskPrimary,
    FdiskTell,
};
pub use screen::FdiskShow;

use super::{refuse, switch_parts, value_in, Refusal};
use crate::dos::program::{
    handler_entry, say_error, Flow, Frame, Id, Int, Key, Program, Returned, Routine,
};
use crate::dos::shell::parse;
use crate::frames::{locals, take, Outcome};
use alloc::vec::Vec;
use disk::run_for;
use screen::{Screen, LAST_COLUMN, LAST_ROW};
use table::{layout, Disk, Extent, MAX_DISKS, MAX_LETTERS};

/// Where `FDISK`'s state lies in its memory.
pub mod at {
    use super::table::{layout, MAX_DISKS};
    use crate::dos::device::generic::media;
    use crate::dos::dirops::finding;
    use crate::dos::program::at::STATE;

    /// [`super::flag`], the disks found and the one being set up.
    pub const FLAGS: u16 = STATE;
    pub const DISKS: u16 = STATE + 0x01;
    pub const DISK: u16 = STATE + 0x02;
    /// The screen as it was found: its mode, page and attribute.
    pub const MODE: u16 = STATE + 0x03;
    pub const PAGE: u16 = STATE + 0x04;
    pub const ATTRIBUTE: u16 = STATE + 0x05;
    /// Where an answer is typed: its row and column.
    pub const INPUT: u16 = STATE + 0x06;
    /// What the command line asks for: the disk, from 0, `FFh` for none,
    /// and the sizes of the partitions to make, in megabytes, `0` for none.
    pub const ASKED_DISK: u16 = STATE + 0x08;
    pub const PRIMARY: u16 = STATE + 0x0A;
    pub const EXTENDED: u16 = STATE + 0x0C;
    pub const LOGICAL: u16 = STATE + 0x0E;
    /// What is said on the standard error at the end, as [`super::ending`]
    /// numbers it.
    pub const ENDING: u16 = STATE + 0x10;
    /// The free run last found: whether there is one, its first cylinder
    /// and its last.
    pub const RUN: u16 = STATE + 0x11;
    /// The cylinders a search found bad, a first and a last a run.
    pub const BAD_COUNT: u16 = STATE + 0x16;
    pub const BAD_MAX: u8 = 16;
    pub const BAD: u16 = STATE + 0x17;
    /// A label typed, NUL-ended.
    pub const TYPED: u16 = BAD + BAD_MAX as u16 * 4;
    /// A volume search's pattern and the DTA it reports through.
    pub const PATTERN: u16 = TYPED + 0x10;
    pub const DTA: u16 = PATTERN + 0x10;
    /// An IOCTL's record, and a sector read or written.
    pub const PACKET: u16 = DTA + finding::NAME + 0x10;
    pub const SECTOR: u16 = PACKET + media::LEN + 0x07;
    /// The disks' tables, each [`TABLE_LEN`] bytes.
    pub const TABLES: u16 = SECTOR + 0x200;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a disk's tables are asserted to fit below"
    )]
    pub const TABLE_LEN: u16 = layout::LEN as u16;
    /// The screen being drawn, as its calls lie in memory.
    pub const SCREEN: u16 = TABLES + MAX_DISKS as u16 * TABLE_LEN;
    pub const SCREEN_LEN: u16 = 0x1000;
    /// Bytes of `FDISK`'s state.
    pub const LEN: u16 = SCREEN + SCREEN_LEN - STATE;

    const _: () = assert!(layout::LEN <= u16::MAX as usize);
    const _: () = assert!(PACKET + media::LEN <= SECTOR);
}

/// What the state's flags record.
pub mod flag {
    /// `/Q`: the machine is not restarted.
    pub const QUIET: u8 = 0x01;
    /// A partition was made or deleted, so drives' letters may have changed:
    /// the machine restarts at the end.
    pub const RESTART: u8 = 0x02;
    /// The screen was taken over, and is given back at the end.
    pub const SCREEN: u8 = 0x04;
    /// The tables could not be written.
    pub const UNWRITTEN: u8 = 0x08;
    /// The first screen's head has been drawn.
    pub const TITLED: u8 = 0x10;
}

/// What is said on the standard error at the end.
pub mod ending {
    pub const NONE: u8 = 0;
    pub const NO_DISKS: u8 = 1;
    pub const READ_FAILED: u8 = 2;
    pub const WRITE_FAILED: u8 = 3;
    pub const BAD_PARAMETER: u8 = 4;
}

/// The handlers `FDISK` installs.
pub mod handler {
    /// INT 23h, ignored as DOS 4.0's `FDISK` ignores it, so a half-made set
    /// of tables is never left on the disk.
    pub const BREAK: u16 = 0;
    /// INT 24h, failing the call, so a drive that cannot be read is listed
    /// without its label rather than its screen broken into.
    pub const CRITICAL: u16 = 1;
}

/// The codes `FDISK` ends with, as DOS 4.0's does: with `/Q`, `1` while the
/// first disk has no DOS primary partition and `2` when no partition was
/// asked for and none changed; `0` otherwise.
pub mod exit {
    pub const DONE: u32 = 0;
    pub const NO_PRIMARY: u32 = 1;
    pub const NOTHING_ASKED: u32 = 2;
}

/// What a menu answers once it is done: back to the one before, or the
/// machine restarted at once, as making a disk's whole DOS partition on a
/// machine of one disk does.
pub mod back {
    pub const MENU: u32 = 0;
    pub const RESTART: u32 = 1;
}

/// The network's installation check, and the bit of its answer that says
/// the machine serves the network.
const NETWORK_CHECK: u16 = 0xB800;
const SERVER: u16 = 0x0040;
/// The text modes `FDISK` draws in, and the monochrome ones it replaces
/// the colour one with.
const COLOUR_TEXT: u8 = 0x03;
const MONO_TEXT: u8 = 0x02;
const MONO_MODES: [u8; 2] = [0x07, 0x0F];
/// The bit of a mode function `0Fh` reports that kept the screen as it was.
const KEPT_SCREEN: u8 = 0x80;
/// The largest size the command line takes.
const LARGEST: u16 = 4000;

/// What `FDISK`'s command line asks for: the disk, from 0, and the sizes in
/// megabytes of the partitions to make on it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub disk: Option<u8>,
    pub primary: Option<u16>,
    pub extended: Option<u16>,
    pub logical: Option<u16>,
    pub quiet: bool,
}

impl Request {
    /// Whether it asks for a partition to be made.
    #[must_use]
    pub const fn makes(&self) -> bool {
        self.primary.is_some() || self.extended.is_some() || self.logical.is_some()
    }
}

/// `FDISK`'s command line, as DOS 4.0's parser takes it: a disk, `1` or
/// `2`, at most, each size from 1 to 4,000, and `/Q`; a switch named again
/// takes the later value.
///
/// # Errors
///
/// The [`Refusal`] the parser gives a line it cannot take.
pub fn request(tail: &[u8]) -> Result<Request, Refusal<'_>> {
    let mut request = Request::default();
    for word in parse::arguments(tail) {
        let Some(switch) = word.strip_prefix(b"/") else {
            if request.disk.is_some() {
                return Err(Refusal::TooMany(word));
            }
            let disk = value_in(word, 1..=u16::from(MAX_DISKS), word)?;
            request.disk = u8::try_from(disk - 1).ok();
            continue;
        };
        let (name, value) = switch_parts(switch);
        let size = |value| value_in(value, 1..=LARGEST, word).map(Some);
        match (name.to_ascii_uppercase().as_slice(), value) {
            (b"PRI", Some(value)) => request.primary = size(value)?,
            (b"EXT", Some(value)) => request.extended = size(value)?,
            (b"LOG", Some(value)) => request.logical = size(value)?,
            (b"Q", None) => request.quiet = true,
            _ => return Err(Refusal::Switch(word)),
        }
    }
    Ok(request)
}

pub(crate) fn has(p: &mut Program<'_, '_>, mask: u8) -> bool {
    p.get8(at::FLAGS) & mask != 0
}

pub(crate) fn set_flag(p: &mut Program<'_, '_>, mask: u8, on: bool) {
    let flags = p.get8(at::FLAGS);
    p.set8(at::FLAGS, if on { flags | mask } else { flags & !mask });
}

fn table_at(disk: u8) -> u16 {
    at::TABLES + u16::from(disk.min(MAX_DISKS - 1)) * at::TABLE_LEN
}

/// Disk `disk`'s tables, as the tables' memory holds them.
pub(crate) fn load(p: &mut Program<'_, '_>, disk: u8) -> Disk {
    let mut bytes = [0u8; layout::LEN];
    p.g.read_bytes(p.own, table_at(disk), &mut bytes);
    Disk::load(&bytes)
}

/// Disk `disk`'s tables kept.
pub(crate) fn keep(p: &mut Program<'_, '_>, disk: u8, tables: &Disk) {
    p.g.write_bytes(p.own, table_at(disk), &tables.store());
}

/// Every disk's tables.
pub(crate) fn disks(p: &mut Program<'_, '_>) -> Vec<Disk> {
    let count = p.get8(at::DISKS).min(MAX_DISKS);
    (0..count).map(|disk| load(p, disk)).collect()
}

/// The disk being set up, from 0.
pub(crate) fn current(p: &mut Program<'_, '_>) -> u8 {
    p.get8(at::DISK)
}

/// The tables of the disk being set up.
pub(crate) fn current_tables(p: &mut Program<'_, '_>) -> Disk {
    let disk = current(p);
    load(p, disk)
}

/// The free run found kept.
pub(crate) fn set_run(p: &mut Program<'_, '_>, run: Option<Extent>) {
    let kept = run.unwrap_or(Extent { start: 0, end: 0 });
    p.set8(at::RUN, u8::from(run.is_some()));
    p.set16(at::RUN + 1, kept.start);
    p.set16(at::RUN + 3, kept.end);
}

/// The free run last found.
pub(crate) fn run(p: &mut Program<'_, '_>) -> Option<Extent> {
    let found = p.get8(at::RUN) != 0;
    let run = Extent {
        start: p.get16(at::RUN + 1),
        end: p.get16(at::RUN + 3),
    };
    found.then_some(run)
}

/// `screen`'s calls laid out to be drawn, and where an answer is typed
/// taken from it.
fn lay_out(p: &mut Program<'_, '_>, screen: &Screen) {
    p.g.write_bytes(
        p.own,
        at::SCREEN,
        &screen.encoded(usize::from(at::SCREEN_LEN)),
    );
    if let Some((row, column)) = screen.input {
        p.set8(at::INPUT, row);
        p.set8(at::INPUT + 1, column);
    }
}

/// The call drawing `screen`.
pub(crate) fn show(p: &mut Program<'_, '_>, screen: &Screen) -> Frame {
    lay_out(p, screen);
    Frame::of(&FdiskShow::of(at::SCREEN))
}

/// The call drawing `screen`, then waiting for Esc.
pub(crate) fn tell(p: &mut Program<'_, '_>, screen: &Screen) -> Frame {
    lay_out(p, screen);
    Frame::of(&FdiskTell {})
}

/// The code `FDISK` ends with.
fn code(p: &mut Program<'_, '_>) -> u32 {
    if !has(p, flag::QUIET) {
        return exit::DONE;
    }
    let asked = p.get16(at::PRIMARY) | p.get16(at::EXTENDED) | p.get16(at::LOGICAL) != 0;
    let primary = !has(p, flag::UNWRITTEN) && p.get8(at::DISKS) > 0 && load(p, 0).has_dos();
    if !primary {
        exit::NO_PRIMARY
    } else if !asked && !has(p, flag::RESTART) {
        exit::NOTHING_ASKED
    } else {
        exit::DONE
    }
}

locals! {
    /// `FDISK`'s main routine.
    pub struct Fdisk {}
}

/// The main routine's phases.
mod phase {
    pub const SAID: u8 = 1;
    pub const BREAK_SET: u8 = 2;
    pub const CRITICAL_SET: u8 = 3;
    pub const NETWORK_ASKED: u8 = 4;
    pub const MODE_ASKED: u8 = 5;
    pub const ATTRIBUTE_ASKED: u8 = 6;
    pub const MODE_SET: u8 = 7;
    pub const PAGE_SET: u8 = 8;
    pub const READ: u8 = 9;
    pub const MENU_DONE: u8 = 10;
    pub const WRITTEN: u8 = 11;
    pub const RESTART_SHOWN: u8 = 12;
    pub const RESTART_PLACED: u8 = 13;
    pub const KEYED: u8 = 14;
    pub const CLEARED: u8 = 15;
    pub const MODE_BACK: u8 = 16;
    pub const PAGE_BACK: u8 = 17;
    pub const DISKS_READ: u8 = 18;
    pub const PRIMARY_FOUND: u8 = 19;
    pub const EXTENDED_FOUND: u8 = 20;
    pub const LOGICAL_FOUND: u8 = 21;
    pub const MADE_WRITTEN: u8 = 22;
}

impl Fdisk {
    /// Says what the end calls for, then ends.
    fn end(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let words = match p.get8(at::ENDING) {
            ending::NO_DISKS => text::error::NO_DISKS,
            ending::READ_FAILED => text::error::READ_FAILED,
            ending::WRITE_FAILED => text::error::WRITE_FAILED,
            ending::BAD_PARAMETER => text::error::BAD_PARAMETER,
            _ => return Flow::Return(Ok(code(p))),
        };
        p.set8(at::ENDING, ending::NONE);
        *phase = phase::SAID;
        Flow::Call(say_error(p, words))
    }

    /// Ends with what `why` calls for said, the screen given back first if
    /// it was taken.
    fn finish(p: &mut Program<'_, '_>, phase: &mut u8, why: u8) -> Flow {
        p.set8(at::ENDING, why);
        Self::give_back(p, phase)
    }

    /// The screen given back as it was found, then the end.
    fn give_back(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if !has(p, flag::SCREEN) {
            return Self::end(p, phase);
        }
        set_flag(p, flag::SCREEN, false);
        *phase = phase::CLEARED;
        Flow::Call(Frame::of(&Int {
            bx: u16::from(p.get8(at::ATTRIBUTE)) << 8,
            dx: u16::from_le_bytes([LAST_COLUMN, LAST_ROW]),
            ..Int::bios(p.own, 0x10, 0x0600)
        }))
    }

    /// The tables written, `next` the phase that learns how.
    fn write(phase: &mut u8, next: u8) -> Flow {
        *phase = next;
        Flow::Call(Frame::of(&FdiskWrite::default()))
    }

    /// The next partition the command line asks for searched for room, from
    /// the step after `done`.
    fn make_next(p: &mut Program<'_, '_>, phase: &mut u8, done: u8) -> Flow {
        let disk = current(p);
        let tables = load(p, disk);
        let all = disks(p);
        let steps = [
            (
                phase::PRIMARY_FOUND,
                at::PRIMARY,
                run_for::PRIMARY,
                !tables.has_dos(),
            ),
            (
                phase::EXTENDED_FOUND,
                at::EXTENDED,
                run_for::EXTENDED,
                (disk == 1 || tables.has_dos()) && tables.extended().is_none(),
            ),
            (
                phase::LOGICAL_FOUND,
                at::LOGICAL,
                run_for::LOGICAL,
                tables.extended().is_some() && table::letters_used(&all) < MAX_LETTERS,
            ),
        ];
        for (found, asked, kind, possible) in steps.into_iter().filter(|&(found, ..)| found > done)
        {
            if possible && p.get16(asked) != 0 {
                *phase = found;
                return Flow::Call(Frame::of(&FdiskSpace::of(kind, disk)));
            }
        }
        Self::write(phase, phase::MADE_WRITTEN)
    }

    /// The partition the command line asks for at `asked` made in the run
    /// found, if there is one.
    fn make(p: &mut Program<'_, '_>, asked: u16, kind: u8) {
        let disk = current(p);
        let mut tables = load(p, disk);
        let mbytes = p.get16(asked);
        let Some(extent) =
            run(p).and_then(|run| table::sized(tables.shape, run, table::Size::Mbytes(mbytes)))
        else {
            return;
        };
        let made = match kind {
            run_for::PRIMARY => tables.make_primary(extent, disk == 0).is_some(),
            run_for::EXTENDED => tables.make_extended(extent).is_some(),
            _ => tables.make_logical(extent).is_some(),
        };
        if made {
            keep(p, disk, &tables);
        }
    }

    /// The tables read for the menus: the first disk that could be read
    /// set up first, or, with none, the end.
    fn menus(p: &mut Program<'_, '_>, phase: &mut u8, count: u32) -> Flow {
        if count == 0 {
            return Self::finish(p, phase, ending::NO_DISKS);
        }
        let all = disks(p);
        let Some(first) = all.iter().position(|disk| disk.readable) else {
            return Self::finish(p, phase, ending::READ_FAILED);
        };
        p.set8(at::DISK, u8::try_from(first).unwrap_or(0));
        let unreadable = if first == 0 { 0 } else { views::disk_digit(0) };
        *phase = phase::MENU_DONE;
        Flow::Call(Frame::of(&FdiskMenu::of(unreadable)))
    }

    /// The tables read for the command line's partitions.
    fn batch(p: &mut Program<'_, '_>, phase: &mut u8, count: u32) -> Flow {
        if count == 0 {
            return Self::finish(p, phase, ending::NO_DISKS);
        }
        let disk = p.get8(at::ASKED_DISK);
        if u32::from(disk) >= count {
            return Self::finish(p, phase, ending::BAD_PARAMETER);
        }
        p.set8(at::DISK, disk);
        if !load(p, disk).readable {
            return Self::finish(p, phase, ending::READ_FAILED);
        }
        Self::make_next(p, phase, 0)
    }
}

impl Routine for Fdisk {
    const ID: Id = Id::Fdisk;

    #[allow(
        clippy::too_many_lines,
        reason = "one arm a phase, in the order DOS's FDISK runs"
    )]
    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                p.g.write_bytes(p.own, at::FLAGS, &[0; (at::RUN - at::FLAGS) as usize]);
                p.set8(at::ASKED_DISK, u8::MAX);
                let tail = p.tail();
                let request = match request(&tail) {
                    Ok(request) => request,
                    Err(refusal) => {
                        *phase = phase::SAID;
                        return Flow::Call(refuse(p, b"", refusal));
                    }
                };
                set_flag(p, flag::QUIET, request.quiet);
                p.set16(at::PRIMARY, request.primary.unwrap_or(0));
                p.set16(at::EXTENDED, request.extended.unwrap_or(0));
                p.set16(at::LOGICAL, request.logical.unwrap_or(0));
                if let Some(disk) = request.disk {
                    p.set8(at::ASKED_DISK, disk);
                }
                if request.disk.is_some() != request.makes() {
                    p.set8(at::ENDING, ending::BAD_PARAMETER);
                    return Self::end(p, phase);
                }
                *phase = phase::BREAK_SET;
                Flow::Call(Frame::of(&Int {
                    dx: handler_entry(handler::BREAK),
                    ..Int::dos(p.own, 0x2523)
                }))
            }
            phase::BREAK_SET => {
                take!(child);
                *phase = phase::CRITICAL_SET;
                Flow::Call(Frame::of(&Int {
                    dx: handler_entry(handler::CRITICAL),
                    ..Int::dos(p.own, 0x2524)
                }))
            }
            phase::CRITICAL_SET => {
                take!(child);
                if p.get8(at::ASKED_DISK) != u8::MAX {
                    *phase = phase::DISKS_READ;
                    return Flow::Call(Frame::of(&FdiskRead::default()));
                }
                *phase = phase::NETWORK_ASKED;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x2F, NETWORK_CHECK)))
            }
            phase::NETWORK_ASKED => {
                let installed = take!(child).to_le_bytes()[0] != 0;
                if installed && p.result(Returned::Bx) & SERVER != 0 {
                    *phase = phase::SAID;
                    return Flow::Call(say_error(p, text::error::NETWORK));
                }
                *phase = phase::MODE_ASKED;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x10, 0x0F00)))
            }
            phase::MODE_ASKED => {
                let mode = take!(child).to_le_bytes()[0];
                let page = p.result(Returned::Bx).to_le_bytes()[1];
                p.set8(at::MODE, mode);
                p.set8(at::PAGE, page);
                *phase = phase::ATTRIBUTE_ASKED;
                Flow::Call(Frame::of(&Int {
                    bx: u16::from(page) << 8,
                    ..Int::bios(p.own, 0x10, 0x0800)
                }))
            }
            phase::ATTRIBUTE_ASKED => {
                let attribute = take!(child).to_le_bytes()[1];
                p.set8(at::ATTRIBUTE, attribute);
                set_flag(p, flag::SCREEN, true);
                let mono = MONO_MODES.contains(&(p.get8(at::MODE) & !KEPT_SCREEN));
                let mode = if mono { MONO_TEXT } else { COLOUR_TEXT };
                *phase = phase::MODE_SET;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x10, u16::from(mode))))
            }
            phase::MODE_SET => {
                take!(child);
                *phase = phase::PAGE_SET;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x10, 0x0500)))
            }
            phase::PAGE_SET => {
                take!(child);
                *phase = phase::READ;
                Flow::Call(Frame::of(&FdiskRead::default()))
            }
            phase::READ => {
                let count = take!(child);
                Self::menus(p, phase, count)
            }
            // The tables in memory are whole after every change, so they are
            // written however the menus ended.
            phase::MENU_DONE => Self::write(phase, phase::WRITTEN),
            phase::WRITTEN => {
                let unwritten = child != Ok(0);
                set_flag(p, flag::UNWRITTEN, unwritten);
                if !has(p, flag::RESTART) || has(p, flag::QUIET) {
                    return Self::finish(
                        p,
                        phase,
                        if unwritten {
                            ending::WRITE_FAILED
                        } else {
                            ending::NONE
                        },
                    );
                }
                let mut screen = Screen::new();
                screen.clear(0, LAST_ROW);
                screen.draw(text::RESTART, &[]);
                if unwritten {
                    screen.draw(text::WRITE_FAILED, &[]);
                }
                *phase = phase::RESTART_SHOWN;
                Flow::Call(show(p, &screen))
            }
            phase::RESTART_SHOWN => {
                take!(child);
                let (row, column) = (p.get8(at::INPUT), p.get8(at::INPUT + 1));
                *phase = phase::RESTART_PLACED;
                Flow::Call(input::place(p, row, column))
            }
            phase::RESTART_PLACED => {
                take!(child);
                *phase = phase::KEYED;
                Flow::Call(Frame::of(&Key::of(0x0800)))
            }
            phase::KEYED => p.restart(),
            phase::CLEARED => {
                take!(child);
                *phase = phase::MODE_BACK;
                Flow::Call(Frame::of(&Int::bios(
                    p.own,
                    0x10,
                    u16::from(p.get8(at::MODE)),
                )))
            }
            phase::MODE_BACK => {
                take!(child);
                *phase = phase::PAGE_BACK;
                Flow::Call(Frame::of(&Int::bios(
                    p.own,
                    0x10,
                    0x0500 | u16::from(p.get8(at::PAGE)),
                )))
            }
            phase::DISKS_READ => {
                let count = take!(child);
                Self::batch(p, phase, count)
            }
            phase::PRIMARY_FOUND | phase::EXTENDED_FOUND | phase::LOGICAL_FOUND => {
                take!(child);
                let (asked, kind) = match *phase {
                    phase::PRIMARY_FOUND => (at::PRIMARY, run_for::PRIMARY),
                    phase::EXTENDED_FOUND => (at::EXTENDED, run_for::EXTENDED),
                    _ => (at::LOGICAL, run_for::LOGICAL),
                };
                Self::make(p, asked, kind);
                let done = *phase;
                Self::make_next(p, phase, done)
            }
            phase::MADE_WRITTEN => {
                let unwritten = child != Ok(0);
                set_flag(p, flag::UNWRITTEN, unwritten);
                Self::finish(
                    p,
                    phase,
                    if unwritten {
                        ending::WRITE_FAILED
                    } else {
                        ending::NONE
                    },
                )
            }
            _ => {
                take!(child);
                Self::end(p, phase)
            }
        }
    }
}

#[cfg(test)]
#[path = "fdisk_tests.rs"]
mod tests;
