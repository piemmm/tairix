//! The machine's menu, on the window and its icon-bar icon alike: its rows,
//! built from what the machine shows, and the command each chosen row is.

use tairix_abi::window_ipc::{
    AppMenu, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuMark, AppMenuReason, AppMenuRow,
    AppMenuSymbol,
};
use tairix_abi::Errno;
use tairix_crt::screen::Scaling;

use super::keys::Mapping;
use super::pacing::Speed;
use crate::machine::fdc::PhysicalDrive;

/// What a chosen row asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Pick a disc image for a drive.
    InsertDisc(PhysicalDrive),
    /// Take a drive's disc out.
    EjectDisc(PhysicalDrive),
    /// Save a drive's disc as an image file of the user's choosing.
    SaveDisc(PhysicalDrive),
    /// Pick a tape image.
    InsertTape,
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
    /// Open the settings.
    Settings,
}

/// Every command: its row's id is [`FIRST_ID`] plus its place here.
const COMMANDS: [Command; 26] = [
    Command::InsertDisc(PhysicalDrive::Zero),
    Command::EjectDisc(PhysicalDrive::Zero),
    Command::SaveDisc(PhysicalDrive::Zero),
    Command::InsertDisc(PhysicalDrive::One),
    Command::EjectDisc(PhysicalDrive::One),
    Command::SaveDisc(PhysicalDrive::One),
    Command::InsertTape,
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
];

/// The first row id the menu uses, past the icon bar's Quit.
pub const FIRST_ID: u16 = tairix_window::QUIT_ROW + 1;

/// What the menu shows of the machine and its window.
// Each flag is one row's mark, independent of the others, so they are a flat
// record rather than a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shown {
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

fn place(command: Command) -> Option<usize> {
    COMMANDS.iter().position(|&listed| listed == command)
}

/// The id of `command`'s row.
///
/// # Errors
///
/// [`Errno::OutOfRange`] for a command the menu has no row for.
pub fn id(command: Command) -> Result<AppMenuItemId, Errno> {
    let place = place(command).ok_or(Errno::OutOfRange)?;
    let id = u16::try_from(place).map_err(|_| Errno::OutOfRange)? + FIRST_ID;
    AppMenuItemId::new(id)
}

/// The command row `item` asks; `None` for an id the menu does not use.
#[must_use]
pub fn command(item: AppMenuItemId) -> Option<Command> {
    let place = usize::from(item.get().checked_sub(FIRST_ID)?);
    COMMANDS.get(place).copied()
}

fn row(command: Command, label: &str, symbol: AppMenuSymbol) -> Result<AppMenuItem, Errno> {
    Ok(AppMenuItem::new(
        id(command)?,
        AppMenuLabel::new(label)?,
        symbol,
    ))
}

fn item(
    command: Command,
    label: &str,
    symbol: AppMenuSymbol,
    enabled: bool,
) -> Result<AppMenuRow, Errno> {
    let item = row(command, label, symbol)?;
    Ok(AppMenuRow::Item(if enabled {
        item
    } else {
        item.disabled()
    }))
}

/// A row ticked or chosen in place, the menu staying open.
fn marked(
    command: Command,
    label: &str,
    symbol: AppMenuSymbol,
    (mark, on): (AppMenuMark, bool),
) -> Result<AppMenuRow, Errno> {
    let mark = if on { mark } else { AppMenuMark::None };
    Ok(AppMenuRow::Item(
        row(command, label, symbol)?.with_mark(mark).repeatable(),
    ))
}

fn submenu(menu: &mut AppMenu, label: &str, symbol: AppMenuSymbol) -> Result<usize, Errno> {
    let at = menu.rows().count();
    menu.push(AppMenuRow::Submenu {
        label: AppMenuLabel::new(label)?,
        enabled: true,
        symbol,
        reason: AppMenuReason::EMPTY,
    })?;
    Ok(at)
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
        menu.push_under(marked(command(value), label, symbol, mark)?, at)?;
    }
    Ok(at)
}

/// The drives' and the deck's rows.
fn push_media(menu: &mut AppMenu, shown: &Shown) -> Result<(), Errno> {
    for (drive, label) in PhysicalDrive::ALL.into_iter().zip(["Drive 0", "Drive 1"]) {
        let index = drive.index();
        let at = submenu(menu, label, AppMenuSymbol::Drive)?;
        let rows = [
            (
                Command::InsertDisc(drive),
                "Insert Disc…",
                AppMenuSymbol::Open,
                true,
            ),
            (
                Command::EjectDisc(drive),
                "Eject",
                AppMenuSymbol::Drive,
                shown.discs[index],
            ),
            (
                Command::SaveDisc(drive),
                "Save Disc As…",
                AppMenuSymbol::SaveAs,
                shown.images[index],
            ),
        ];
        for (command, label, symbol, enabled) in rows {
            menu.push_under(item(command, label, symbol, enabled)?, at)?;
        }
    }
    let at = submenu(menu, "Tape", AppMenuSymbol::Play)?;
    menu.push_under(
        item(
            Command::InsertTape,
            "Insert Tape…",
            AppMenuSymbol::Open,
            true,
        )?,
        at,
    )?;
    menu.push_under(
        item(
            Command::RewindTape,
            "Rewind",
            AppMenuSymbol::Previous,
            shown.tape,
        )?,
        at,
    )?;
    let record = if shown.tape {
        let mark = (AppMenuMark::Check, shown.recording);
        marked(Command::Record, "Record", AppMenuSymbol::Play, mark)?
    } else {
        item(Command::Record, "Record", AppMenuSymbol::Play, false)?
    };
    menu.push_under(record, at)?;
    menu.push_under(
        item(
            Command::EjectTape,
            "Eject Tape",
            AppMenuSymbol::Drive,
            shown.tape,
        )?,
        at,
    )?;
    Ok(())
}

/// The rows that drive the machine and say how it is shown and typed at.
fn push_machine(menu: &mut AppMenu, shown: &Shown) -> Result<(), Errno> {
    menu.push(item(Command::Break, "BREAK", AppMenuSymbol::Refresh, true)?)?;
    menu.push(item(Command::Reset, "Reset", AppMenuSymbol::Restart, true)?)?;
    let pause = (AppMenuMark::Check, shown.paused);
    menu.push(marked(
        Command::Pause,
        "Pause",
        AppMenuSymbol::Pause,
        pause,
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
    menu.push(marked(
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
        marked(
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
    menu.push(item(Command::Paste, "Paste", AppMenuSymbol::Paste, true)?)?;
    Ok(())
}

/// The icon bar's menu: the application's Info, the machine's rows, and
/// Quit last.
///
/// # Errors
///
/// [`Errno`] for rows that do not fit a menu.
pub fn bar(shown: &Shown) -> Result<AppMenu, Errno> {
    let mut menu = AppMenu::EMPTY;
    menu.push(AppMenuRow::Info)?;
    push_rows(&mut menu, shown)?;
    menu.push(AppMenuRow::Separator)?;
    let quit = AppMenuItem::new(
        AppMenuItemId::new(tairix_window::QUIT_ROW)?,
        AppMenuLabel::new(tairix_window::QUIT_LABEL)?,
        AppMenuSymbol::Quit,
    );
    menu.push(AppMenuRow::Item(quit))?;
    Ok(menu)
}

/// The machine's rows appended to `menu`, as `shown` makes them.
///
/// # Errors
///
/// [`Errno`] for a menu with no room left for them.
pub fn push_rows(menu: &mut AppMenu, shown: &Shown) -> Result<(), Errno> {
    push_media(menu, shown)?;
    menu.push(AppMenuRow::Separator)?;
    push_machine(menu, shown)?;
    menu.push(AppMenuRow::Separator)?;
    menu.push(item(
        Command::SaveSnapshot,
        "Save Snapshot…",
        AppMenuSymbol::Save,
        true,
    )?)?;
    menu.push(item(
        Command::RestoreSnapshot,
        "Restore Snapshot…",
        AppMenuSymbol::Open,
        true,
    )?)?;
    menu.push(AppMenuRow::Separator)?;
    menu.push(item(
        Command::Settings,
        "Settings…",
        AppMenuSymbol::Settings,
        true,
    )?)
}

#[cfg(test)]
#[path = "menu_tests.rs"]
mod tests;
