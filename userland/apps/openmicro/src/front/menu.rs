//! A machine's menu, on its window and on its card while it runs: its rows,
//! built from what the machine shows and the model it is, and the command
//! each chosen row is.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::window_ipc::{
    AppMenu, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuMark, AppMenuReason, AppMenuRow,
    AppMenuSymbol,
};
use tairix_abi::Errno;
use tairix_crt::screen::Scaling;
use tairix_machines::menu::{item, marked, submenu, Acts};

use super::keys::Mapping;
use super::pacing::Speed;
use crate::machine::bbc::fdc::PhysicalDrive;
use crate::machine::Model;

/// What a chosen row asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Pick a disc image for a drive.
    InsertDisc(PhysicalDrive),
    /// Put the folder's disc image of this place in its listing in a drive.
    FolderDisc(PhysicalDrive, u8),
    /// Make a blank disc in the folder and put it in a drive.
    NewDisc(PhysicalDrive),
    /// Take a drive's disc out.
    EjectDisc(PhysicalDrive),
    /// Save a drive's disc as an image file of the user's choosing.
    SaveDisc(PhysicalDrive),
    /// Pick a tape image.
    InsertTape,
    /// Put the folder's tape image of this place in its listing in the deck.
    FolderTape(u8),
    /// Make a blank tape in the folder and put it in the deck.
    NewTape,
    /// Wind the tape back.
    RewindTape,
    /// Press or release RECORD.
    Record,
    /// Take the tape out.
    EjectTape,
    /// Press BREAK.
    Break,
    /// Switch the machine off and on.
    Reset,
    /// Pause or resume.
    Pause,
    /// Run at a speed.
    Speed(Speed),
    /// Fill the screen, or leave it.
    FullScreen,
    /// Enlarge the picture so.
    Scaling(Scaling),
    /// Show scan lines, or not.
    ScanLines,
    /// Map the keyboard so.
    Keyboard(Mapping),
    /// Type the clipboard's text.
    Paste,
    /// Save the whole machine to a file.
    SaveSnapshot,
    /// Take a saved machine back.
    RestoreSnapshot,
    /// Open the machine's settings.
    Settings,
    /// Take its media out and turn it off, keeping only its last screen.
    ShutDown,
    /// Bring the library back.
    Library,
}

/// Every command a row of fixed id is: its id is [`FIRST_ID`] plus its place
/// here.
const COMMANDS: [Command; 30] = [
    Command::InsertDisc(PhysicalDrive::Zero),
    Command::NewDisc(PhysicalDrive::Zero),
    Command::EjectDisc(PhysicalDrive::Zero),
    Command::SaveDisc(PhysicalDrive::Zero),
    Command::InsertDisc(PhysicalDrive::One),
    Command::NewDisc(PhysicalDrive::One),
    Command::EjectDisc(PhysicalDrive::One),
    Command::SaveDisc(PhysicalDrive::One),
    Command::InsertTape,
    Command::NewTape,
    Command::RewindTape,
    Command::Record,
    Command::EjectTape,
    Command::Break,
    Command::Reset,
    Command::Pause,
    Command::Speed(Speed::Normal),
    Command::Speed(Speed::Double),
    Command::Speed(Speed::Fastest),
    Command::FullScreen,
    Command::Scaling(Scaling::Sharp),
    Command::Scaling(Scaling::Smooth),
    Command::ScanLines,
    Command::Keyboard(Mapping::Logical),
    Command::Keyboard(Mapping::Physical),
    Command::Paste,
    Command::SaveSnapshot,
    Command::RestoreSnapshot,
    Command::Settings,
    Command::ShutDown,
];

/// `n` rows as row ids count them.
const fn rows(n: usize) -> u16 {
    assert!(n < 1 << 16, "more rows than ids");
    let bytes = n.to_le_bytes();
    u16::from_le_bytes([bytes[0], bytes[1]])
}

/// The first fixed row id.
pub const FIRST_ID: u16 = 1;
const LIBRARY: u16 = FIRST_ID + rows(COMMANDS.len());

/// The first id of a folder's listings: drive 0's discs, drive 1's, then the
/// tapes, [`FOLDER_ROWS`] apart.
const FIRST_FOLDER: u16 = 0x1000;

/// The most images of a kind a folder's listing offers: what a menu's rows
/// and text leave room for beside the rest.
pub const FOLDER_ROWS: usize = 8;
const FOLDER_SPAN: u16 = rows(FOLDER_ROWS);

const NO_FOLDER: &str = "The documents folder cannot be reached";
const NO_TAPE: &str = "The deck is empty";

/// The longest a folder's image is named in its row, in bytes, an ellipsis
/// ending one cut short.
const FOLDER_LABEL: usize = 24;

// Every row id stays clear of the library card's own rows.
const _: () =
    assert!((FIRST_FOLDER as usize) + 3 * FOLDER_ROWS < tairix_machines::menu::CARD_ROWS as usize);

/// What a machine's folder offers it, as last listed: its disc images and
/// tape images by name, and whether the folder can be reached at all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Offered {
    /// Whether the application's documents folder was granted.
    pub reachable: bool,
    /// The disc images, at most [`FOLDER_ROWS`].
    pub discs: Vec<String>,
    /// The tape images, at most [`FOLDER_ROWS`].
    pub tapes: Vec<String>,
}

/// What the menu shows of the machine and its window.
// Each flag is one row's mark, independent of the others, so they are a flat
// record rather than a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown {
    /// The model, which decides whether there are drives.
    pub model: Model,
    /// Whether each drive holds a disc.
    pub discs: [bool; 2],
    /// Whether each drive's disc image is held, to be saved as a file.
    pub images: [bool; 2],
    /// Whether the deck holds a tape.
    pub tape: bool,
    /// Whether RECORD is down.
    pub recording: bool,
    /// Whether the machine is paused.
    pub paused: bool,
    /// How fast it runs.
    pub speed: Speed,
    /// Whether the window fills the screen.
    pub full_screen: bool,
    /// How the picture is enlarged.
    pub scaling: Scaling,
    /// Whether scan lines show.
    pub scan_lines: bool,
    /// How the keyboard is mapped.
    pub keyboard: Mapping,
}

/// Where the rows are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// The machine's window, which adds its Settings and the library.
    Window,
    /// The machine's card, which has its own.
    Card,
}

fn place(command: Command) -> Option<usize> {
    COMMANDS.iter().position(|&listed| listed == command)
}

/// The id of `command`'s row.
///
/// # Errors
///
/// [`Errno::OutOfRange`] for a command the menu has no row for.
pub fn id(command: Command) -> Result<u16, Errno> {
    let folder = |kind: u16, index: u8| {
        (usize::from(index) < FOLDER_ROWS)
            .then(|| FIRST_FOLDER + kind * FOLDER_SPAN + u16::from(index))
            .ok_or(Errno::OutOfRange)
    };
    match command {
        Command::FolderDisc(drive, index) => folder(u16::from(drive.index() == 1), index),
        Command::FolderTape(index) => folder(2, index),
        Command::Library => Ok(LIBRARY),
        command => {
            let place = place(command).ok_or(Errno::OutOfRange)?;
            Ok(u16::try_from(place).map_err(|_| Errno::OutOfRange)? + FIRST_ID)
        }
    }
}

/// The command row `item` asks; `None` for an id the menu does not use.
#[must_use]
pub fn command(item: AppMenuItemId) -> Option<Command> {
    let id = item.get();
    if id == LIBRARY {
        return Some(Command::Library);
    }
    if let Some(at) = id.checked_sub(FIRST_FOLDER) {
        let (kind, index) = (at / FOLDER_SPAN, at % FOLDER_SPAN);
        let index = u8::try_from(index).ok()?;
        return match kind {
            0 => Some(Command::FolderDisc(PhysicalDrive::Zero, index)),
            1 => Some(Command::FolderDisc(PhysicalDrive::One, index)),
            2 => Some(Command::FolderTape(index)),
            _ => None,
        };
    }
    COMMANDS
        .get(usize::from(id.checked_sub(FIRST_ID)?))
        .copied()
}

fn row(
    command: Command,
    label: &str,
    symbol: AppMenuSymbol,
    acts: Acts,
) -> Result<AppMenuRow, Errno> {
    item(id(command)?, label, symbol, (AppMenuMark::None, acts))
}

/// A row that cannot be chosen now, and why.
fn unavailable(
    command: Command,
    label: &str,
    symbol: AppMenuSymbol,
    why: &str,
) -> Result<AppMenuRow, Errno> {
    let item = AppMenuItem::new(
        AppMenuItemId::new(id(command)?)?,
        AppMenuLabel::new(label)?,
        symbol,
    )
    .with_reason(AppMenuReason::new(why)?)
    .disabled();
    Ok(AppMenuRow::Item(item))
}

/// A row ticked or chosen in place, the menu staying open.
fn ticked(
    command: Command,
    label: &str,
    symbol: AppMenuSymbol,
    (mark, on): (AppMenuMark, bool),
) -> Result<AppMenuRow, Errno> {
    item(
        id(command)?,
        label,
        symbol,
        (marked(on, mark), Acts::InPlace),
    )
}

/// A submenu of `choices`, the one `chosen` marked, each row `command` of
/// its value.
fn radios<T: Copy + PartialEq>(
    menu: &mut AppMenu,
    (label, symbol): (&str, AppMenuSymbol),
    choices: &[(T, &str)],
    chosen: T,
    command: fn(T) -> Command,
) -> Result<usize, Errno> {
    let at = submenu(menu, label, symbol)?;
    for &(value, label) in choices {
        let mark = (AppMenuMark::Radio, value == chosen);
        menu.push_under(ticked(command(value), label, symbol, mark)?, at)?;
    }
    Ok(at)
}

/// A *From Folder* submenu of `names`, under the submenu at `at`, each row
/// `command` of its place.
fn push_folder(
    menu: &mut AppMenu,
    at: usize,
    (names, offered): (&[String], &Offered),
    command: impl Fn(u8) -> Command,
) -> Result<(), Errno> {
    if !offered.reachable || names.is_empty() {
        let why = if offered.reachable {
            "The machine's folder holds none"
        } else {
            NO_FOLDER
        };
        let first = command(0);
        return menu.push_under(
            unavailable(first, "From Folder", AppMenuSymbol::Folder, why)?,
            at,
        );
    }
    let from = menu.rows().count();
    menu.push_under(
        AppMenuRow::Submenu {
            label: AppMenuLabel::new("From Folder")?,
            enabled: true,
            symbol: AppMenuSymbol::Folder,
            reason: AppMenuReason::EMPTY,
        },
        at,
    )?;
    for (index, name) in (0u8..).zip(names.iter().take(FOLDER_ROWS)) {
        let label = clipped(name);
        menu.push_under(
            row(command(index), &label, AppMenuSymbol::File, Acts::Elsewhere)?,
            from,
        )?;
    }
    Ok(())
}

/// `name` as a row shows it: whole, or cut at a character to
/// [`FOLDER_LABEL`] bytes with an ellipsis.
fn clipped(name: &str) -> String {
    if name.len() <= FOLDER_LABEL {
        return String::from(name);
    }
    let ellipsis = '\u{2026}';
    let keep = name.floor_char_boundary(FOLDER_LABEL - ellipsis.len_utf8());
    let mut label = String::from(&name[..keep]);
    label.push(ellipsis);
    label
}

/// `command`'s row, or, where `refused` says why not, one that cannot be
/// chosen.
fn offer(
    (command, label, symbol): (Command, &str, AppMenuSymbol),
    acts: Acts,
    refused: Option<&str>,
) -> Result<AppMenuRow, Errno> {
    match refused {
        None => row(command, label, symbol, acts),
        Some(why) => unavailable(command, label, symbol, why),
    }
}

/// A drive's submenu.
fn push_drive(
    menu: &mut AppMenu,
    (drive, label): (PhysicalDrive, &str),
    shown: &Shown,
    offered: &Offered,
) -> Result<(), Errno> {
    let index = drive.index();
    let at = submenu(menu, label, AppMenuSymbol::Drive)?;
    let insert = (
        Command::InsertDisc(drive),
        "Insert Disc\u{2026}",
        AppMenuSymbol::Open,
    );
    menu.push_under(offer(insert, Acts::Elsewhere, None)?, at)?;
    push_folder(menu, at, (&offered.discs, offered), |n| {
        Command::FolderDisc(drive, n)
    })?;
    let no_folder = (!offered.reachable).then_some(NO_FOLDER);
    let new = (
        Command::NewDisc(drive),
        "New Disc",
        AppMenuSymbol::NewDocument,
    );
    menu.push_under(offer(new, Acts::Elsewhere, no_folder)?, at)?;
    let empty = (!shown.discs[index]).then_some("The drive is empty");
    let eject = (Command::EjectDisc(drive), "Eject", AppMenuSymbol::Drive);
    menu.push_under(offer(eject, Acts::InPlace, empty)?, at)?;
    let unheld = (!shown.images[index]).then_some("The drive holds no disc of its own");
    let save = (
        Command::SaveDisc(drive),
        "Save Disc As\u{2026}",
        AppMenuSymbol::SaveAs,
    );
    menu.push_under(offer(save, Acts::Elsewhere, unheld)?, at)
}

/// The deck's submenu.
fn push_tape(menu: &mut AppMenu, shown: &Shown, offered: &Offered) -> Result<(), Errno> {
    let at = submenu(menu, "Tape", AppMenuSymbol::Play)?;
    let insert = (
        Command::InsertTape,
        "Insert Tape\u{2026}",
        AppMenuSymbol::Open,
    );
    menu.push_under(offer(insert, Acts::Elsewhere, None)?, at)?;
    push_folder(menu, at, (&offered.tapes, offered), Command::FolderTape)?;
    let no_folder = (!offered.reachable).then_some(NO_FOLDER);
    let new = (Command::NewTape, "New Tape", AppMenuSymbol::NewDocument);
    menu.push_under(offer(new, Acts::Elsewhere, no_folder)?, at)?;
    let no_tape = (!shown.tape).then_some(NO_TAPE);
    let rewind = (Command::RewindTape, "Rewind", AppMenuSymbol::Previous);
    menu.push_under(offer(rewind, Acts::InPlace, no_tape)?, at)?;
    let record = if shown.tape {
        let mark = (AppMenuMark::Check, shown.recording);
        ticked(Command::Record, "Record", AppMenuSymbol::Play, mark)?
    } else {
        unavailable(Command::Record, "Record", AppMenuSymbol::Play, NO_TAPE)?
    };
    menu.push_under(record, at)?;
    let eject = (Command::EjectTape, "Eject Tape", AppMenuSymbol::Drive);
    menu.push_under(offer(eject, Acts::InPlace, no_tape)?, at)
}

/// The drives' rows, where the model has drives, and the deck's.
fn push_media(menu: &mut AppMenu, shown: &Shown, offered: &Offered) -> Result<(), Errno> {
    if shown.model.has_drives() {
        for drive in PhysicalDrive::ALL.into_iter().zip(["Drive 0", "Drive 1"]) {
            push_drive(menu, drive, shown, offered)?;
        }
    }
    push_tape(menu, shown, offered)
}

/// The rows that drive the machine and say how it is shown and typed at.
fn push_machine(menu: &mut AppMenu, shown: &Shown) -> Result<(), Errno> {
    menu.push(row(
        Command::Break,
        "BREAK",
        AppMenuSymbol::Refresh,
        Acts::InPlace,
    )?)?;
    menu.push(row(
        Command::Reset,
        "Reset",
        AppMenuSymbol::Restart,
        Acts::InPlace,
    )?)?;
    menu.push(ticked(
        Command::Pause,
        "Pause",
        AppMenuSymbol::Pause,
        (AppMenuMark::Check, shown.paused),
    )?)?;
    let speeds = [
        (Speed::Normal, "Normal"),
        (Speed::Double, "Double"),
        (Speed::Fastest, "Fastest"),
    ];
    radios(
        menu,
        ("Speed", AppMenuSymbol::Clock),
        &speeds,
        shown.speed,
        Command::Speed,
    )?;
    menu.push(AppMenuRow::Separator)?;
    let full = (AppMenuMark::Check, shown.full_screen);
    menu.push(ticked(
        Command::FullScreen,
        "Full Screen",
        AppMenuSymbol::ZoomFit,
        full,
    )?)?;
    let scalings = [(Scaling::Sharp, "Sharp"), (Scaling::Smooth, "Smooth")];
    let display = ("Display", AppMenuSymbol::Display);
    let at = radios(menu, display, &scalings, shown.scaling, Command::Scaling)?;
    let lines = (AppMenuMark::Check, shown.scan_lines);
    menu.push_under(
        ticked(
            Command::ScanLines,
            "Scan Lines",
            AppMenuSymbol::Noise,
            lines,
        )?,
        at,
    )?;
    let mappings = [
        (Mapping::Logical, "By Character"),
        (Mapping::Physical, "By Position"),
    ];
    radios(
        menu,
        ("Keyboard", AppMenuSymbol::Adjust),
        &mappings,
        shown.keyboard,
        Command::Keyboard,
    )?;
    menu.push(row(
        Command::Paste,
        "Paste",
        AppMenuSymbol::Paste,
        Acts::InPlace,
    )?)?;
    Ok(())
}

/// The machine's rows appended to `menu`, as `shown` and its folder make
/// them, for `place`.
///
/// # Errors
///
/// [`Errno`] for a menu with no room left for them.
pub fn push_rows(
    menu: &mut AppMenu,
    (shown, offered): (&Shown, &Offered),
    place: Place,
) -> Result<(), Errno> {
    push_media(menu, shown, offered)?;
    menu.push(AppMenuRow::Separator)?;
    push_machine(menu, shown)?;
    menu.push(AppMenuRow::Separator)?;
    menu.push(row(
        Command::SaveSnapshot,
        "Save Snapshot\u{2026}",
        AppMenuSymbol::Save,
        Acts::Elsewhere,
    )?)?;
    menu.push(row(
        Command::RestoreSnapshot,
        "Restore Snapshot\u{2026}",
        AppMenuSymbol::Open,
        Acts::Elsewhere,
    )?)?;
    if place == Place::Window {
        menu.push(AppMenuRow::Separator)?;
        menu.push(row(
            Command::Settings,
            "Settings\u{2026}",
            AppMenuSymbol::Settings,
            Acts::Elsewhere,
        )?)?;
        menu.push(row(
            Command::Library,
            "Machine Library",
            AppMenuSymbol::Application,
            Acts::Elsewhere,
        )?)?;
    }
    menu.push(AppMenuRow::Separator)?;
    menu.push(row(
        Command::ShutDown,
        "Shut Down",
        AppMenuSymbol::ShutDown,
        Acts::Elsewhere,
    )?)
}

#[cfg(test)]
#[path = "menu_tests.rs"]
mod tests;
