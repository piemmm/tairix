//! `FDISK`'s menus, as DOS 4.0's runs them: the main menu and what each of
//! its choices leads to — partitions and logical drives made, the active
//! partition set, partitions and drives deleted, and the tables shown. Each
//! menu leaves the screen for the one it returns to to clear.

use super::disk::{run_for, FdiskSpace};
use super::input::{answer, prompt, size_question, FdiskAnswer, FdiskLabel, FdiskSize, FdiskWait};
use super::screen::{FdiskShow, Screen, LAST_ROW, NO, YES};
use super::table::{
    letters_used, percent, sized, Disk, Extent, Logical, Shape, Size, LABEL_LEN, MAX_LETTERS,
};
use super::text;
use super::views::{self, disk_digit, drives, heading, lettered, table};
use super::{
    at, back, current, current_tables, disks, flag, has, keep, load, run, set_flag, show, tell,
};
use crate::dos::formats::is_dos;
use crate::dos::program::{Flow, Frame, Id, Program, Routine};
use crate::dos::shell::asciz_at;
use crate::frames::{locals, take, Outcome};
use alloc::vec::Vec;

/// What a free run offers a partition: its size in megabytes, and as a
/// share of the cylinders a share is reckoned of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Offer {
    mbytes: u16,
    share: u16,
    whole: u16,
}

impl Offer {
    fn of(shape: Shape, run: Extent, whole: u16) -> Self {
        Self {
            mbytes: shape.mbytes(run.cylinders()),
            share: percent(run.cylinders(), whole),
            whole,
        }
    }

    /// The size Enter takes before a key is typed: the megabytes, or the
    /// share where the run is under half a megabyte.
    const fn default(self) -> (u16, bool) {
        if self.mbytes == 0 {
            (self.share, true)
        } else {
            (self.mbytes, false)
        }
    }

    /// The question `prompt` asks drawn, its default shown.
    fn ask(self, screen: &mut Screen, prompt: u8) {
        let (default, percent) = self.default();
        size_question(screen, prompt, default, percent);
    }

    /// The size typed for the question `prompt` asks.
    fn sizing(self, prompt: u8) -> FdiskSize {
        let (default, percent) = self.default();
        FdiskSize::of(prompt, default, percent, self.mbytes, self.share)
    }

    /// The size an [`FdiskSize`] answer gives.
    fn size(self, answered: u32) -> Size {
        let value = u16::try_from(answered & answer::VALUE).unwrap_or(0);
        if answered & answer::SHARE == 0 {
            Size::Mbytes(value)
        } else {
            Size::Share {
                share: value,
                whole: self.whole,
            }
        }
    }
}

/// The free run found on the current disk, with what it offers a partition
/// reckoned of `whole` cylinders; none where it offers nothing.
fn offered(p: &mut Program<'_, '_>, tables: &Disk, whole: u16) -> Option<(Extent, Offer)> {
    let run = run(p)?;
    let offer = Offer::of(tables.shape, run, whole);
    (offer.share != 0).then_some((run, offer))
}

/// A choice refused: `why` said where the question was, then Esc waited
/// for, `next` the phase the wait returns to.
fn refuse_choice(p: &mut Program<'_, '_>, phase: &mut u8, why: &str, next: u8) -> Flow {
    let mut screen = Screen::new();
    screen.clear(17, 17);
    screen.draw(why, &[]);
    *phase = next;
    Flow::Call(tell(p, &screen))
}

/// The current disk and every disk's tables.
fn current_disks(p: &mut Program<'_, '_>) -> (u8, Vec<Disk>) {
    let disk = current(p);
    (disk, disks(p))
}

locals! {
    /// The screen laid out last drawn, then keys read until Esc.
    pub struct FdiskTell {}
}

mod telling {
    pub const SHOWN: u8 = 1;
    pub const WAITED: u8 = 2;
}

impl Routine for FdiskTell {
    const ID: Id = Id::FdiskTell;

    fn step(&mut self, _p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            telling::SHOWN => {
                take!(child);
                *phase = telling::WAITED;
                Flow::Call(Frame::of(&FdiskWait {}))
            }
            telling::WAITED => {
                take!(child);
                Flow::Return(Ok(back::MENU))
            }
            _ => {
                *phase = telling::SHOWN;
                Flow::Call(Frame::of(&FdiskShow::of(at::SCREEN)))
            }
        }
    }
}

locals! {
    /// The main menu until Esc, each choice's menu run and `5` turning to
    /// the other disk. Answers [`back::RESTART`] when a menu restarts the
    /// machine at once.
    pub struct FdiskMenu {
        /// The digit of a disk found unreadable, to say so; `0` for none.
        unreadable: u8,
        /// The screen is cleared before the menu is drawn.
        clear: bool,
    }
}

mod menu {
    pub const SHOWN: u8 = 1;
    pub const CHOSEN: u8 = 2;
    pub const DONE: u8 = 3;
}

impl FdiskMenu {
    #[must_use]
    pub const fn of(unreadable: u8) -> Self {
        Self {
            unreadable,
            clear: true,
        }
    }

    /// The menu drawn over what the screen shows, or a cleared screen; the
    /// program's name with it the first time.
    fn draw(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let (disk, all) = current_disks(p);
        let mut screen = Screen::new();
        if self.clear {
            screen.clear(0, LAST_ROW);
        }
        if !has(p, flag::TITLED) {
            set_flag(p, flag::TITLED, true);
            views::title(&mut screen);
        }
        views::main_menu(&mut screen, &all, usize::from(disk));
        if self.unreadable != 0 {
            screen.draw(text::UNREADABLE, &[self.unreadable]);
            self.unreadable = 0;
        }
        self.clear = false;
        *phase = menu::SHOWN;
        Flow::Call(show(p, &screen))
    }

    /// The other disk set up, or said to be unreadable.
    fn turn(&mut self, p: &mut Program<'_, '_>) {
        let other = u8::from(current(p) == 0);
        if load(p, other).readable {
            p.set8(at::DISK, other);
        } else {
            self.unreadable = disk_digit(usize::from(other));
        }
    }
}

impl Routine for FdiskMenu {
    const ID: Id = Id::FdiskMenu;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            menu::SHOWN => {
                take!(child);
                let most = if p.get8(at::DISKS) > 1 { 5 } else { 4 };
                *phase = menu::CHOSEN;
                Flow::Call(Frame::of(&FdiskAnswer::choice(1, most)))
            }
            menu::CHOSEN => {
                let chosen = match u8::try_from(take!(child)) {
                    Ok(b'1') => Frame::of(&FdiskCreate {}),
                    Ok(b'2') => Frame::of(&FdiskActive::default()),
                    Ok(b'3') => Frame::of(&FdiskDelete {}),
                    Ok(b'4') => Frame::of(&FdiskDisplay {}),
                    Ok(b'5') => {
                        self.turn(p);
                        return self.draw(p, phase);
                    }
                    _ => return Flow::Return(Ok(back::MENU)),
                };
                *phase = menu::DONE;
                Flow::Call(chosen)
            }
            menu::DONE => {
                if take!(child) == back::RESTART {
                    return Flow::Return(Ok(back::RESTART));
                }
                self.clear = true;
                self.draw(p, phase)
            }
            _ => self.draw(p, phase),
        }
    }
}

locals! {
    /// The create menu: a primary or an extended partition, or logical
    /// drives in the extended partition.
    pub struct FdiskCreate {}
}

mod creating {
    pub const SHOWN: u8 = 1;
    pub const CHOSEN: u8 = 2;
    pub const DONE: u8 = 3;
}

impl Routine for FdiskCreate {
    const ID: Id = Id::FdiskCreate;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            creating::SHOWN => {
                take!(child);
                *phase = creating::CHOSEN;
                Flow::Call(Frame::of(&FdiskAnswer::choice(1, 3)))
            }
            creating::CHOSEN => {
                let chosen = take!(child);
                let disk = current(p);
                let tables = load(p, disk);
                let menu = match u8::try_from(chosen) {
                    Ok(b'1') => Frame::of(&FdiskPrimary {}),
                    Ok(b'2') if disk == 1 || tables.has_dos() => Frame::of(&FdiskExtended {}),
                    Ok(b'3') if tables.extended().is_some() => Frame::of(&FdiskLogical {}),
                    Ok(b'2') => {
                        return refuse_choice(p, phase, text::PRIMARY_FIRST, creating::DONE)
                    }
                    Ok(b'3') => {
                        return refuse_choice(p, phase, text::EXTENDED_FIRST, creating::DONE)
                    }
                    _ => return Flow::Return(Ok(back::MENU)),
                };
                *phase = creating::DONE;
                Flow::Call(menu)
            }
            creating::DONE => Flow::Return(Ok(take!(child))),
            _ => {
                let disk = current(p);
                let mut screen = Screen::new();
                heading(&mut screen, text::CREATE_TITLE, usize::from(disk));
                let tables = load(p, disk);
                if tables.free_slot().is_none() && tables.extended().is_none() {
                    screen.draw(text::NO_SPACE, &[]);
                    *phase = creating::DONE;
                    return Flow::Call(tell(p, &screen));
                }
                for message in [
                    text::CHOOSE,
                    text::CREATE_PARTITIONS,
                    text::ENTER_CHOICE,
                    text::CREATE_LOGICAL_OPTION,
                ] {
                    screen.draw(message, &[]);
                }
                *phase = creating::SHOWN;
                Flow::Call(show(p, &screen))
            }
        }
    }
}

/// The phases of the menus that make a partition of a size typed.
mod making {
    pub const FOUND: u8 = 1;
    pub const ASKED_SHOWN: u8 = 2;
    pub const ANSWERED: u8 = 3;
    pub const SIZE_SHOWN: u8 = 4;
    pub const SIZED: u8 = 5;
    pub const CREATED_TOLD: u8 = 6;
    pub const DONE: u8 = 7;
    pub const TOLD: u8 = 8;
}

/// `screen` headed for a menu that makes a partition: `title`, the disk,
/// and its table.
fn partition_head(p: &mut Program<'_, '_>, screen: &mut Screen, title: &str) {
    let (disk, all) = current_disks(p);
    heading(screen, title, usize::from(disk));
    table(screen, &all, usize::from(disk));
}

/// The size question the run found offers drawn under the disk's table and
/// size, or, where it offers nothing, that there is no space; `prompt`
/// the question.
fn size_screen(p: &mut Program<'_, '_>, phase: &mut u8, title: &str, prompt: u8) -> Flow {
    let disk = current(p);
    let tables = load(p, disk);
    let mut screen = Screen::new();
    partition_head(p, &mut screen, title);
    let Some((run, offer)) = offered(p, &tables, tables.shape.cylinders) else {
        screen.draw(text::NO_SPACE, &[]);
        *phase = making::TOLD;
        return Flow::Call(tell(p, &screen));
    };
    views::disk_space(&mut screen, &tables);
    views::maximum(
        &mut screen,
        text::MAXIMUM_SPACE,
        tables.shape,
        run,
        tables.shape.cylinders,
    );
    offer.ask(&mut screen, prompt);
    *phase = making::SIZE_SHOWN;
    Flow::Call(show(p, &screen))
}

/// The size of the partition `prompt` asks for typed.
fn size_typed(p: &mut Program<'_, '_>, phase: &mut u8, prompt: u8) -> Flow {
    let tables = current_tables(p);
    let whole = if prompt == prompt::LOGICAL {
        tables.extended().map_or(0, Extent::cylinders)
    } else {
        tables.shape.cylinders
    };
    let Some((_, offer)) = offered(p, &tables, whole) else {
        return Flow::Return(Ok(back::MENU));
    };
    *phase = making::SIZED;
    Flow::Call(Frame::of(&offer.sizing(prompt)))
}

/// The extent the size `answered` takes of the run found, the share of
/// `whole` cylinders; none for Esc or a size of nothing.
fn extent_typed(
    p: &mut Program<'_, '_>,
    tables: &Disk,
    whole: u16,
    answered: u32,
) -> Option<Extent> {
    if answered == answer::ESCAPED {
        return None;
    }
    let (run, offer) = offered(p, tables, whole)?;
    sized(tables.shape, run, offer.size(answered))
}

locals! {
    /// A primary DOS partition made: the largest free run, active on the
    /// first disk, or a size typed for it.
    pub struct FdiskPrimary {}
}

impl Routine for FdiskPrimary {
    const ID: Id = Id::FdiskPrimary;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let disk = current(p);
        match *phase {
            making::FOUND => {
                take!(child);
                let mut screen = Screen::new();
                heading(&mut screen, text::PRIMARY_TITLE, usize::from(disk));
                if run(p).is_none() || load(p, disk).free_slot().is_none() {
                    screen.draw(text::NO_SPACE, &[]);
                    *phase = making::TOLD;
                    return Flow::Call(tell(p, &screen));
                }
                screen.draw(
                    if disk == 0 {
                        text::MAXIMUM_ACTIVE
                    } else {
                        text::MAXIMUM
                    },
                    &[],
                );
                *phase = making::ASKED_SHOWN;
                Flow::Call(show(p, &screen))
            }
            making::ASKED_SHOWN => {
                take!(child);
                *phase = making::ANSWERED;
                Flow::Call(Frame::of(&FdiskAnswer::yes_no(YES)))
            }
            making::ANSWERED => match take!(child) {
                1 => {
                    let mut tables = load(p, disk);
                    let made = run(p).and_then(|run| tables.make_primary(run, disk == 0));
                    if made.is_none() {
                        return Flow::Return(Ok(back::MENU));
                    }
                    keep(p, disk, &tables);
                    set_flag(p, flag::RESTART, true);
                    if p.get8(at::DISKS) == 1 {
                        return Flow::Return(Ok(back::RESTART));
                    }
                    let mut screen = Screen::new();
                    screen.clear(16, 23);
                    screen.draw(text::PRIMARY_CREATED_LETTERS, &[]);
                    *phase = making::TOLD;
                    Flow::Call(tell(p, &screen))
                }
                0 => size_screen(p, phase, text::PRIMARY_TITLE, prompt::PRIMARY),
                _ => Flow::Return(Ok(back::MENU)),
            },
            making::SIZE_SHOWN => {
                take!(child);
                size_typed(p, phase, prompt::PRIMARY)
            }
            making::SIZED => {
                let answered = take!(child);
                let mut tables = load(p, disk);
                let whole = tables.shape.cylinders;
                let made = extent_typed(p, &tables, whole, answered)
                    .and_then(|extent| tables.make_primary(extent, false));
                if made.is_none() {
                    return Flow::Return(Ok(back::MENU));
                }
                keep(p, disk, &tables);
                set_flag(p, flag::RESTART, true);
                let all = disks(p);
                let mut screen = Screen::new();
                screen.clear(13, 19);
                table(&mut screen, &all, usize::from(disk));
                if all.len() == 1 {
                    screen.draw(text::PRIMARY_CREATED, &[]);
                } else {
                    screen.clear(16, 23);
                    screen.draw(text::PRIMARY_CREATED_LETTERS, &[]);
                }
                *phase = making::TOLD;
                Flow::Call(tell(p, &screen))
            }
            making::TOLD => {
                take!(child);
                Flow::Return(Ok(back::MENU))
            }
            _ => {
                if load(p, disk).has_dos() {
                    let mut screen = Screen::new();
                    partition_head(p, &mut screen, text::PRIMARY_TITLE);
                    screen.draw(text::PRIMARY_EXISTS, &[]);
                    *phase = making::TOLD;
                    return Flow::Call(tell(p, &screen));
                }
                *phase = making::FOUND;
                Flow::Call(Frame::of(&FdiskSpace::of(run_for::PRIMARY, disk)))
            }
        }
    }
}

locals! {
    /// An extended DOS partition made of a size typed, then logical drives
    /// in it: once the first disk has a primary DOS partition, and while
    /// the disk has no extended one.
    pub struct FdiskExtended {}
}

impl Routine for FdiskExtended {
    const ID: Id = Id::FdiskExtended;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let disk = current(p);
        match *phase {
            making::FOUND => {
                take!(child);
                size_screen(p, phase, text::EXTENDED_TITLE, prompt::EXTENDED)
            }
            making::SIZE_SHOWN => {
                take!(child);
                size_typed(p, phase, prompt::EXTENDED)
            }
            making::SIZED => {
                let answered = take!(child);
                let mut tables = load(p, disk);
                let whole = tables.shape.cylinders;
                let made = extent_typed(p, &tables, whole, answered)
                    .and_then(|extent| tables.make_extended(extent));
                if made.is_none() {
                    return Flow::Return(Ok(back::MENU));
                }
                keep(p, disk, &tables);
                set_flag(p, flag::RESTART, true);
                let all = disks(p);
                let mut screen = Screen::new();
                screen.clear(13, 19);
                table(&mut screen, &all, usize::from(disk));
                screen.clear(LAST_ROW, LAST_ROW);
                screen.draw(text::CONTINUE, &[]);
                screen.draw(text::EXTENDED_CREATED, &[]);
                *phase = making::CREATED_TOLD;
                Flow::Call(tell(p, &screen))
            }
            making::CREATED_TOLD => {
                take!(child);
                *phase = making::DONE;
                Flow::Call(Frame::of(&FdiskLogical {}))
            }
            making::DONE => Flow::Return(Ok(take!(child))),
            making::TOLD => {
                take!(child);
                Flow::Return(Ok(back::MENU))
            }
            _ => {
                let tables = load(p, disk);
                let refused = if disk != 1 && !tables.has_dos() {
                    Some(text::PRIMARY_FIRST)
                } else if tables.extended().is_some() {
                    Some(text::EXTENDED_EXISTS)
                } else if tables.free_slot().is_none() {
                    Some(text::NO_SPACE)
                } else {
                    None
                };
                if let Some(why) = refused {
                    let mut screen = Screen::new();
                    partition_head(p, &mut screen, text::EXTENDED_TITLE);
                    screen.draw(why, &[]);
                    *phase = making::TOLD;
                    return Flow::Call(tell(p, &screen));
                }
                *phase = making::FOUND;
                Flow::Call(Frame::of(&FdiskSpace::of(run_for::EXTENDED, disk)))
            }
        }
    }
}

locals! {
    /// Logical drives made in the extended partition, a size typed for
    /// each, until Esc, the partition is full or the letters run out.
    pub struct FdiskLogical {}
}

mod logical_phase {
    pub const LISTED: u8 = 1;
    pub const FOUND: u8 = 2;
    pub const TOLD: u8 = 3;
}

impl FdiskLogical {
    /// `screen` with `why` said in the space under the drives, and the
    /// drives listed again.
    fn full(p: &mut Program<'_, '_>, phase: &mut u8, why: &str) -> Flow {
        let (disk, all) = current_disks(p);
        let mut screen = Screen::new();
        screen.clear(17, 21);
        screen.draw(why, &[]);
        drives(&mut screen, &all, usize::from(disk));
        *phase = logical_phase::TOLD;
        Flow::Call(tell(p, &screen))
    }
}

impl Routine for FdiskLogical {
    const ID: Id = Id::FdiskLogical;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let disk = current(p);
        match *phase {
            logical_phase::LISTED => {
                take!(child);
                if letters_used(&disks(p)) >= MAX_LETTERS {
                    return Self::full(p, phase, text::TOO_MANY_DRIVES);
                }
                *phase = logical_phase::FOUND;
                Flow::Call(Frame::of(&FdiskSpace::of(run_for::LOGICAL, disk)))
            }
            logical_phase::FOUND => {
                take!(child);
                let tables = load(p, disk);
                let whole = tables.extended().map_or(0, Extent::cylinders);
                let Some((run, offer)) = offered(p, &tables, whole) else {
                    return Self::full(p, phase, text::EXTENDED_FULL);
                };
                let mut screen = Screen::new();
                views::extended_space(&mut screen, &tables);
                views::maximum(&mut screen, text::MAXIMUM_LOGICAL, tables.shape, run, whole);
                offer.ask(&mut screen, prompt::LOGICAL);
                *phase = making::SIZE_SHOWN;
                Flow::Call(show(p, &screen))
            }
            making::SIZE_SHOWN => {
                take!(child);
                size_typed(p, phase, prompt::LOGICAL)
            }
            making::SIZED => {
                let answered = take!(child);
                let mut tables = load(p, disk);
                let whole = tables.extended().map_or(0, Extent::cylinders);
                let made = extent_typed(p, &tables, whole, answered)
                    .and_then(|extent| tables.make_logical(extent));
                if made.is_none() {
                    return Flow::Return(Ok(back::MENU));
                }
                keep(p, disk, &tables);
                set_flag(p, flag::RESTART, true);
                let all = disks(p);
                let mut screen = Screen::new();
                screen.clear(15, 19);
                drives(&mut screen, &all, usize::from(disk));
                screen.draw(text::LOGICAL_CREATED, &[]);
                *phase = logical_phase::LISTED;
                Flow::Call(show(p, &screen))
            }
            logical_phase::TOLD => {
                take!(child);
                Flow::Return(Ok(back::MENU))
            }
            _ => {
                let all = disks(p);
                let mut screen = Screen::new();
                screen.clear(0, LAST_ROW);
                screen.draw(text::LOGICAL_TITLE, &[]);
                screen.draw(text::RETURN, &[]);
                drives(&mut screen, &all, usize::from(disk));
                *phase = logical_phase::LISTED;
                Flow::Call(show(p, &screen))
            }
        }
    }
}

locals! {
    /// The first disk's active partition set: any but an extended
    /// partition or a table, chosen by its number in the table.
    pub struct FdiskActive {
        /// The number Enter takes before a key is typed: the last chosen.
        default: u8,
    }
}

mod activating {
    pub const SHOWN: u8 = 1;
    pub const CHOSEN: u8 = 2;
    pub const TOLD: u8 = 3;
}

impl Routine for FdiskActive {
    const ID: Id = Id::FdiskActive;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            activating::SHOWN => {
                take!(child);
                let count = u8::try_from(load(p, 0).listed().len()).unwrap_or(0);
                *phase = activating::CHOSEN;
                Flow::Call(Frame::of(&FdiskAnswer::choice(self.default, count)))
            }
            activating::CHOSEN => {
                let Ok(digit) = u8::try_from(take!(child)) else {
                    return Flow::Return(Ok(back::MENU));
                };
                self.default = digit.saturating_sub(b'0');
                let mut tables = load(p, 0);
                let chosen = tables
                    .listed()
                    .get(usize::from(digit.saturating_sub(b'1')))
                    .copied();
                let mut screen = Screen::new();
                screen.clear(18, 23);
                match chosen {
                    Some(entry) if entry.kind().startable() => {
                        tables.set_active(entry.slot);
                        keep(p, 0, &tables);
                        table(&mut screen, &disks(p), 0);
                        screen.clear(16, 21);
                        screen.draw(text::MADE_ACTIVE, &[digit]);
                        *phase = activating::TOLD;
                        Flow::Call(tell(p, &screen))
                    }
                    _ => {
                        screen.draw(text::NOT_STARTABLE, &[digit]);
                        *phase = activating::SHOWN;
                        Flow::Call(show(p, &screen))
                    }
                }
            }
            activating::TOLD => {
                take!(child);
                Flow::Return(Ok(back::MENU))
            }
            _ => {
                let (disk, all) = current_disks(p);
                let mut screen = Screen::new();
                heading(&mut screen, text::ACTIVE_TITLE, usize::from(disk));
                let why = if disk != 0 {
                    Some(text::FIRST_DISK_ONLY)
                } else if !table(&mut screen, &all, 0) {
                    Some(text::NOTHING_TO_ACTIVATE)
                } else {
                    let tables = load(p, 0);
                    if tables
                        .active()
                        .is_some_and(|active| !active.kind().startable())
                    {
                        screen.draw(text::ACTIVE_NOT_STARTABLE, &[]);
                    }
                    let listed = tables.listed();
                    let mut startable = listed.iter().filter(|entry| entry.kind().startable());
                    match (startable.next(), startable.next()) {
                        (None, _) => Some(text::NONE_STARTABLE),
                        (Some(only), None) if only.active => Some(text::ONLY_ONE_ACTIVE),
                        _ => None,
                    }
                };
                if let Some(why) = why {
                    screen.draw(why, &[]);
                    *phase = activating::TOLD;
                    return Flow::Call(tell(p, &screen));
                }
                views::disk_space(&mut screen, &load(p, 0));
                screen.draw(text::WHICH_ACTIVE, &[]);
                *phase = activating::SHOWN;
                Flow::Call(show(p, &screen))
            }
        }
    }
}

locals! {
    /// The delete menu: the primary DOS partition, the extended partition,
    /// or logical drives in it.
    pub struct FdiskDelete {}
}

mod deleting {
    pub const SHOWN: u8 = 1;
    pub const CHOSEN: u8 = 2;
    pub const DONE: u8 = 3;
}

impl Routine for FdiskDelete {
    const ID: Id = Id::FdiskDelete;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            deleting::SHOWN => {
                take!(child);
                *phase = deleting::CHOSEN;
                Flow::Call(Frame::of(&FdiskAnswer::choice(0, 3)))
            }
            deleting::CHOSEN => {
                let chosen = take!(child);
                let tables = current_tables(p);
                let extended = tables.extended().is_some();
                let menu = match u8::try_from(chosen) {
                    Ok(b'1') if tables.has_dos() => Frame::of(&FdiskDeletePrimary {}),
                    Ok(b'2') if extended => Frame::of(&FdiskDeleteExtended {}),
                    Ok(b'3') if extended && tables.drives().next().is_some() => {
                        Frame::of(&FdiskDeleteLogical::default())
                    }
                    Ok(b'1') => return refuse_choice(p, phase, text::NO_PRIMARY, deleting::DONE),
                    Ok(b'2') => return refuse_choice(p, phase, text::NO_EXTENDED, deleting::DONE),
                    Ok(b'3') => {
                        return refuse_choice(p, phase, text::NO_DRIVES_TO_DELETE, deleting::DONE)
                    }
                    _ => return Flow::Return(Ok(back::MENU)),
                };
                *phase = deleting::DONE;
                Flow::Call(menu)
            }
            deleting::DONE => {
                take!(child);
                Flow::Return(Ok(back::MENU))
            }
            _ => {
                let disk = current(p);
                let mut screen = Screen::new();
                heading(&mut screen, text::DELETE_TITLE, usize::from(disk));
                if load(p, disk).primaries().next().is_none() {
                    screen.draw(text::NOTHING_TO_DELETE, &[]);
                    *phase = deleting::DONE;
                    return Flow::Call(tell(p, &screen));
                }
                for message in [
                    text::CHOOSE,
                    text::DELETE_PARTITIONS,
                    text::ENTER_CHOICE,
                    text::DELETE_LOGICAL_OPTION,
                ] {
                    screen.draw(message, &[]);
                }
                *phase = deleting::SHOWN;
                Flow::Call(show(p, &screen))
            }
        }
    }
}

/// The phases of the menus that delete a partition once it is confirmed.
mod confirming {
    pub const SHOWN: u8 = 1;
    pub const ANSWERED: u8 = 2;
    pub const TOLD: u8 = 3;
}

/// A deleting menu's first screen: its `title` and the disk's table and
/// size, then `warning` asking whether to go on, or `refused` said where
/// it may not.
fn confirm(
    p: &mut Program<'_, '_>,
    phase: &mut u8,
    title: &str,
    warning: &str,
    refused: Option<&str>,
) -> Flow {
    let mut screen = Screen::new();
    partition_head(p, &mut screen, title);
    views::disk_space(&mut screen, &current_tables(p));
    if let Some(why) = refused {
        screen.draw(why, &[]);
        *phase = confirming::TOLD;
        return Flow::Call(tell(p, &screen));
    }
    screen.draw(warning, &[]);
    *phase = confirming::SHOWN;
    Flow::Call(show(p, &screen))
}

/// The partition deleted once a deleting menu's warning is answered yes:
/// `delete` made of the tables and `done` said under their table.
fn confirmed(
    p: &mut Program<'_, '_>,
    phase: &mut u8,
    child: Outcome,
    delete: fn(&mut Disk),
    cleared_from: u8,
    done: &str,
) -> Flow {
    match *phase {
        confirming::SHOWN => {
            take!(child);
            *phase = confirming::ANSWERED;
            Flow::Call(Frame::of(&FdiskAnswer::yes_no(NO)))
        }
        confirming::ANSWERED => {
            if take!(child) != 1 {
                return Flow::Return(Ok(back::MENU));
            }
            let disk = current(p);
            let mut tables = load(p, disk);
            delete(&mut tables);
            keep(p, disk, &tables);
            set_flag(p, flag::RESTART, true);
            let mut screen = Screen::new();
            table(&mut screen, &disks(p), usize::from(disk));
            screen.clear(cleared_from, 23);
            screen.draw(done, &[]);
            *phase = confirming::TOLD;
            Flow::Call(tell(p, &screen))
        }
        _ => {
            take!(child);
            Flow::Return(Ok(back::MENU))
        }
    }
}

locals! {
    /// The primary DOS partition deleted, once confirmed; not the first
    /// disk's while it has an extended partition.
    pub struct FdiskDeletePrimary {}
}

impl Routine for FdiskDeletePrimary {
    const ID: Id = Id::FdiskDeletePrimary;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase != 0 {
            let delete: fn(&mut Disk) = |tables| {
                if let Some(primary) = tables.first(super::table::Kind::Dos) {
                    tables.clear(primary.slot);
                }
            };
            return confirmed(p, phase, child, delete, 16, text::PRIMARY_DELETED);
        }
        let disk = current(p);
        let blocked = disk == 0 && load(p, disk).extended().is_some();
        let refused = blocked.then_some(text::EXTENDED_IN_THE_WAY);
        confirm(
            p,
            phase,
            text::DELETE_PRIMARY_TITLE,
            text::DELETE_PRIMARY_WARNING,
            refused,
        )
    }
}

locals! {
    /// The extended partition deleted, once confirmed; not while logical
    /// drives are in it.
    pub struct FdiskDeleteExtended {}
}

impl Routine for FdiskDeleteExtended {
    const ID: Id = Id::FdiskDeleteExtended;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase != 0 {
            return confirmed(
                p,
                phase,
                child,
                Disk::clear_extended,
                17,
                text::EXTENDED_DELETED,
            );
        }
        let occupied = current_tables(p).drives().next().is_some();
        let refused = occupied.then_some(text::DRIVES_EXIST);
        confirm(
            p,
            phase,
            text::DELETE_EXTENDED_TITLE,
            text::DELETE_EXTENDED_WARNING,
            refused,
        )
    }
}

locals! {
    /// Logical drives deleted until Esc, each named by its letter and its
    /// label typed, then confirmed. The drives keep the letters they were
    /// listed with until the menu ends, when those left are lettered again.
    pub struct FdiskDeleteLogical {
        /// The letter the first drive listed has, and the drives listed.
        first: u8,
        count: u8,
        /// The drive named.
        named: u8,
        /// A drive was deleted.
        deleted: bool,
    }
}

mod volume_phase {
    pub const ASK: u8 = 1;
    pub const WHICH_SHOWN: u8 = 2;
    pub const WHICH: u8 = 3;
    pub const LABEL_SHOWN: u8 = 4;
    pub const LABELLED: u8 = 5;
    pub const SURE_SHOWN: u8 = 6;
    pub const SURE: u8 = 7;
    pub const ALL_TOLD: u8 = 8;
    pub const TOLD: u8 = 9;
}

impl FdiskDeleteLogical {
    /// The drives listed, each where it lies in the disk's drives.
    fn listed(self, tables: &Disk) -> impl Iterator<Item = (usize, &Logical)> {
        let count = usize::from(self.count);
        tables
            .logical
            .iter()
            .enumerate()
            .filter(|(_, drive)| is_dos(drive.type_byte()))
            .take(count)
    }

    /// The letters of the drives listed not yet deleted.
    fn live(self, tables: &Disk) -> impl Iterator<Item = u8> + '_ {
        self.listed(tables)
            .zip(self.first..)
            .filter(|((_, drive), _)| !drive.deleted)
            .map(|(_, letter)| letter)
    }

    /// The drive with letter `letter`, if it was listed: where it lies in
    /// the disk's drives.
    fn lettered(self, tables: &Disk, letter: u8) -> Option<(usize, &Logical)> {
        self.listed(tables)
            .nth(usize::from(letter.checked_sub(self.first)?))
    }

    /// `screen` shown, then which drive asked again.
    fn again(p: &mut Program<'_, '_>, phase: &mut u8, screen: &Screen) -> Flow {
        *phase = volume_phase::ASK;
        Flow::Call(show(p, screen))
    }

    /// The drives deleted taken out of the disk's and the rest lettered
    /// again, or, with none deleted, the end.
    fn finish(self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if !self.deleted {
            return Flow::Return(Ok(back::MENU));
        }
        let disk = current(p);
        let mut tables = load(p, disk);
        tables.sweep();
        keep(p, disk, &tables);
        let mut screen = Screen::new();
        drives(&mut screen, &disks(p), usize::from(disk));
        screen.clear(16, 23);
        screen.draw(text::LETTERS_CHANGED, &[]);
        *phase = volume_phase::TOLD;
        Flow::Call(tell(p, &screen))
    }
}

/// Whether the label typed is `label`, as C's `strcmp` compares them.
fn typed_is(p: &mut Program<'_, '_>, label: &[u8; LABEL_LEN]) -> bool {
    let typed = asciz_at(
        p,
        at::TYPED,
        u16::try_from(LABEL_LEN + 1).unwrap_or(u16::MAX),
    );
    let own = label
        .iter()
        .position(|&byte| byte == 0)
        .map_or(&label[..], |end| &label[..end]);
    typed == own
}

impl Routine for FdiskDeleteLogical {
    const ID: Id = Id::FdiskDeleteLogical;

    #[allow(
        clippy::too_many_lines,
        reason = "one arm a phase, in the order DOS's FDISK asks"
    )]
    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let disk = current(p);
        match *phase {
            volume_phase::ASK => {
                take!(child);
                let tables = load(p, disk);
                if self.live(&tables).next().is_none() {
                    let mut screen = Screen::new();
                    screen.clear(16, 21);
                    screen.draw(text::ALL_DRIVES_DELETED, &[]);
                    *phase = volume_phase::ALL_TOLD;
                    return Flow::Call(tell(p, &screen));
                }
                let mut screen = Screen::new();
                screen.draw(text::DELETE_LOGICAL_WARNING, &[]);
                screen.clear(21, 21);
                *phase = volume_phase::WHICH_SHOWN;
                Flow::Call(show(p, &screen))
            }
            volume_phase::WHICH_SHOWN => {
                take!(child);
                let tables = load(p, disk);
                let mut live = self.live(&tables);
                let low = live.next().unwrap_or(self.first);
                let high = live.last().unwrap_or(low);
                let last = self.first.saturating_add(self.count.saturating_sub(1));
                *phase = volume_phase::WHICH;
                Flow::Call(Frame::of(&FdiskAnswer::letter(self.first, last, low, high)))
            }
            volume_phase::WHICH => {
                let Ok(letter) = u8::try_from(take!(child)) else {
                    return self.finish(p, phase);
                };
                self.named = letter;
                let tables = load(p, disk);
                let mut screen = Screen::new();
                if self
                    .lettered(&tables, letter)
                    .is_some_and(|(_, drive)| !drive.deleted)
                {
                    screen.clear(22, 23);
                    screen.draw(text::ENTER_LABEL, &[]);
                    *phase = volume_phase::LABEL_SHOWN;
                    return Flow::Call(show(p, &screen));
                }
                screen.clear(21, 22);
                screen.draw(text::ALREADY_DELETED, &[letter, b':']);
                Self::again(p, phase, &screen)
            }
            volume_phase::LABEL_SHOWN => {
                take!(child);
                *phase = volume_phase::LABELLED;
                Flow::Call(Frame::of(&FdiskLabel::default()))
            }
            volume_phase::LABELLED => {
                if take!(child) == answer::ESCAPED {
                    return self.finish(p, phase);
                }
                let tables = load(p, disk);
                let label = self
                    .lettered(&tables, self.named)
                    .map(|(_, drive)| drive.label);
                let mut screen = Screen::new();
                if label.is_some_and(|label| typed_is(p, &label)) {
                    screen.draw(text::SURE, &[]);
                    *phase = volume_phase::SURE_SHOWN;
                    return Flow::Call(show(p, &screen));
                }
                screen.draw(text::LABEL_MISMATCH, &[]);
                Self::again(p, phase, &screen)
            }
            volume_phase::SURE_SHOWN => {
                take!(child);
                *phase = volume_phase::SURE;
                Flow::Call(Frame::of(&FdiskAnswer::yes_no(NO)))
            }
            volume_phase::SURE => {
                let answered = take!(child);
                if answered == answer::ESCAPED {
                    return self.finish(p, phase);
                }
                let mut screen = Screen::new();
                if answered == 1 {
                    let mut tables = load(p, disk);
                    if let Some((at, _)) = self.lettered(&tables, self.named) {
                        if let Some(drive) = tables.logical.get_mut(at) {
                            drive.deleted = true;
                        }
                    }
                    keep(p, disk, &tables);
                    self.deleted = true;
                    set_flag(p, flag::RESTART, true);
                    let gone: Vec<usize> = self
                        .listed(&tables)
                        .enumerate()
                        .filter(|(_, (_, drive))| drive.deleted)
                        .map(|(n, _)| n)
                        .collect();
                    for n in gone {
                        views::deleted(&mut screen, n);
                    }
                }
                screen.clear(21, 23);
                Self::again(p, phase, &screen)
            }
            volume_phase::ALL_TOLD => {
                take!(child);
                self.finish(p, phase)
            }
            volume_phase::TOLD => {
                take!(child);
                Flow::Return(Ok(back::MENU))
            }
            _ => {
                let all = disks(p);
                let listed = lettered(&all, usize::from(disk));
                self.first = listed.first().map_or(b'C', |&(letter, _)| letter);
                self.count =
                    u8::try_from(listed.iter().filter(|&&(letter, _)| letter != b' ').count())
                        .unwrap_or(0);
                let mut screen = Screen::new();
                screen.clear(0, LAST_ROW);
                screen.draw(text::DELETE_LOGICAL_TITLE, &[]);
                screen.draw(text::RETURN, &[]);
                drives(&mut screen, &all, usize::from(disk));
                if let Some(tables) = all.get(usize::from(disk)) {
                    views::extended_space(&mut screen, tables);
                }
                *phase = volume_phase::ASK;
                Flow::Call(show(p, &screen))
            }
        }
    }
}

locals! {
    /// The disk's partitions shown, and its logical drives if asked for.
    pub struct FdiskDisplay {}
}

mod displaying {
    pub const ASKED: u8 = 1;
    pub const ANSWERED: u8 = 2;
    pub const TOLD: u8 = 3;
}

impl Routine for FdiskDisplay {
    const ID: Id = Id::FdiskDisplay;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let (disk, all) = current_disks(p);
        let Some(tables) = all.get(usize::from(disk)) else {
            return Flow::Return(Ok(back::MENU));
        };
        match *phase {
            displaying::ASKED => {
                take!(child);
                *phase = displaying::ANSWERED;
                Flow::Call(Frame::of(&FdiskAnswer::yes_no(YES)))
            }
            displaying::ANSWERED => {
                if take!(child) != 1 {
                    return Flow::Return(Ok(back::MENU));
                }
                let mut screen = Screen::new();
                screen.clear(0, LAST_ROW);
                screen.draw(text::DRIVES_TITLE, &[]);
                drives(&mut screen, &all, usize::from(disk));
                views::extended_space(&mut screen, tables);
                screen.draw(text::RETURN, &[]);
                *phase = displaying::TOLD;
                Flow::Call(tell(p, &screen))
            }
            displaying::TOLD => {
                take!(child);
                Flow::Return(Ok(back::MENU))
            }
            _ => {
                let mut screen = Screen::new();
                heading(&mut screen, text::DISPLAY_TITLE, usize::from(disk));
                let listed = table(&mut screen, &all, usize::from(disk));
                if listed {
                    views::disk_space(&mut screen, tables);
                }
                if listed && tables.extended().is_some() && tables.drives().next().is_some() {
                    screen.draw(text::SHOW_DRIVES, &[]);
                    *phase = displaying::ASKED;
                    return Flow::Call(show(p, &screen));
                }
                *phase = displaying::TOLD;
                Flow::Call(tell(p, &screen))
            }
        }
    }
}
