//! A machine window's menu: what it shows of the machine as it runs — paused
//! or not, its processor's speed, its tube, full screen or not, whether its
//! screen shows text to copy, whether text is being typed on it, whether
//! another folder may be shared and whether its debugger is open — and the
//! command each row is. A machine's card in the library carries the same
//! rows while the machine runs.

use tairix_abi::window_ipc::{
    AppMenu, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuMark, AppMenuReason, AppMenuRow,
    AppMenuSymbol,
};
use tairix_abi::Errno;
use tairix_crt::Tube;
use tairix_machines::menu::{
    effect_of, item, marked, push_display, submenu, Acts, Effect, CARD_ROWS,
};

use super::settings::SPEEDS;
use crate::time::CpuClock;

/// What a row of the menu does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Stop the machine where it is, or let it go on.
    Pause,
    /// Power it off and on.
    Reset,
    /// Turn it off, its state not kept, and close its window.
    ShutDown,
    /// Press and let go Ctrl+Alt+Delete on its keyboard.
    CtrlAltDelete,
    /// Put the text its screen shows on the clipboard.
    CopyText,
    /// Type the clipboard's text on its keyboard.
    Paste,
    /// Type no more of what is being pasted.
    StopPasting,
    /// Share a folder the user picks with it, as a drive.
    ShareFolder,
    /// Run its processor at this rate.
    Speed(CpuClock),
    /// Turn one of its tube's effects off or on.
    Effect(Effect),
    /// Keep its screen as a picture.
    Screenshot,
    /// Fill the display with its window, or give the display back.
    FullScreen,
    /// Open its debugger, or bring the debugger forward.
    Debugger,
}

/// The machine as the menu shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown {
    /// Whether it is paused.
    pub paused: bool,
    /// The rate its processor runs at.
    pub speed: CpuClock,
    /// The tube its picture is drawn through.
    pub tube: Tube,
    /// What it offers to copy, and whether it is being pasted into.
    pub clipboard: Clipboard,
    /// Whether it has room for another shared folder.
    pub can_share: bool,
    /// How its windows stand.
    pub windows: Windows,
}

/// How a machine's windows stand.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Windows {
    /// Its window fills the display.
    pub fullscreen: bool,
    /// Its debugger is open.
    pub debugging: bool,
}

/// What a machine offers the clipboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clipboard {
    /// Its screen shows text to copy.
    pub copyable: bool,
    /// Text is being typed on its keyboard.
    pub pasting: bool,
}

const PAUSE: u16 = 1;
const RESET: u16 = 2;
const CTRL_ALT_DELETE: u16 = 3;
const SCREENSHOT: u16 = 4;
const FULL_SCREEN: u16 = 5;
const SHUT_DOWN: u16 = 6;
const COPY_TEXT: u16 = 7;
const PASTE: u16 = 8;
const STOP_PASTING: u16 = 9;
const SHARE_FOLDER: u16 = 30;
const DEBUGGER: u16 = 31;
/// The first speed row's id; each speed of [`SPEEDS`] follows in order.
const FIRST_SPEED: u16 = 10;
/// The first effect row's id; each of [`Effect::ALL`] follows in order.
const FIRST_EFFECT: u16 = 20;

const _: () = assert!(FIRST_SPEED as usize + SPEEDS.len() <= FIRST_EFFECT as usize);
const _: () = assert!(FIRST_EFFECT as usize + Effect::ALL.len() <= SHARE_FOLDER as usize);
const _: () = assert!(DEBUGGER < CARD_ROWS);

/// The menu of a machine `shown` as it stands.
///
/// # Errors
///
/// The menu builder's refusal, which rows this size never meet.
pub fn menu(shown: &Shown) -> Result<AppMenu, Errno> {
    let mut menu = AppMenu::EMPTY;
    push_machine(&mut menu, shown)?;
    Ok(menu)
}

/// Push the machine's own rows, `shown` as it stands.
///
/// # Errors
///
/// The menu builder's refusal, which rows this size never meet.
pub fn push_machine(menu: &mut AppMenu, shown: &Shown) -> Result<(), Errno> {
    menu.push(item(
        PAUSE,
        "Pause",
        AppMenuSymbol::Pause,
        (marked(shown.paused, AppMenuMark::Check), Acts::InPlace),
    )?)?;
    menu.push(item(
        RESET,
        "Reset",
        AppMenuSymbol::Refresh,
        (AppMenuMark::None, Acts::InPlace),
    )?)?;
    menu.push(item(
        CTRL_ALT_DELETE,
        "Send Ctrl+Alt+Delete",
        AppMenuSymbol::Restart,
        (AppMenuMark::None, Acts::InPlace),
    )?)?;
    menu.push(AppMenuRow::Separator)?;
    let copy = AppMenuItem::new(
        AppMenuItemId::new(COPY_TEXT)?,
        AppMenuLabel::new("Copy Screen Text")?,
        AppMenuSymbol::Copy,
    );
    menu.push(AppMenuRow::Item(if shown.clipboard.copyable {
        copy.repeatable()
    } else {
        copy.with_reason(AppMenuReason::new("The screen shows graphics")?)
            .disabled()
    }))?;
    menu.push(if shown.clipboard.pasting {
        item(
            STOP_PASTING,
            "Stop Pasting",
            AppMenuSymbol::Close,
            (AppMenuMark::None, Acts::InPlace),
        )?
    } else {
        item(
            PASTE,
            "Paste",
            AppMenuSymbol::Paste,
            (AppMenuMark::None, Acts::InPlace),
        )?
    })?;
    menu.push(AppMenuRow::Separator)?;
    let share = AppMenuItem::new(
        AppMenuItemId::new(SHARE_FOLDER)?,
        AppMenuLabel::new("Share a Folder\u{2026}")?,
        AppMenuSymbol::Folder,
    );
    menu.push(AppMenuRow::Item(if shown.can_share {
        share
    } else {
        share
            .with_reason(AppMenuReason::new(
                "As many folders as a machine takes are shared",
            )?)
            .disabled()
    }))?;
    menu.push(AppMenuRow::Separator)?;
    let speed = submenu(menu, "Speed", AppMenuSymbol::Clock)?;
    for ((rate, label), id) in SPEEDS.iter().zip(FIRST_SPEED..) {
        let mark = marked(*rate == shown.speed, AppMenuMark::Radio);
        let row = item(id, label, AppMenuSymbol::Clock, (mark, Acts::InPlace))?;
        menu.push_under(row, speed)?;
    }
    push_display(menu, shown.tube, FIRST_EFFECT)?;
    push_windows(menu, shown.windows)
}

/// The menu's tail: the screen's picture, its windows, and shutting down.
fn push_windows(menu: &mut AppMenu, windows: Windows) -> Result<(), Errno> {
    menu.push(AppMenuRow::Separator)?;
    menu.push(item(
        SCREENSHOT,
        "Take Screenshot\u{2026}",
        AppMenuSymbol::Image,
        (AppMenuMark::None, Acts::Elsewhere),
    )?)?;
    menu.push(item(
        FULL_SCREEN,
        "Full Screen",
        AppMenuSymbol::ZoomFit,
        (
            marked(windows.fullscreen, AppMenuMark::Check),
            Acts::InPlace,
        ),
    )?)?;
    menu.push(AppMenuRow::Separator)?;
    menu.push(item(
        DEBUGGER,
        "Debugger",
        AppMenuSymbol::ViewCode,
        (
            marked(windows.debugging, AppMenuMark::Check),
            Acts::Elsewhere,
        ),
    )?)?;
    menu.push(AppMenuRow::Separator)?;
    menu.push(item(
        SHUT_DOWN,
        "Shut Down",
        AppMenuSymbol::ShutDown,
        (AppMenuMark::None, Acts::Elsewhere),
    )?)
}

/// What the row `item` of the menu does; none for an id it never carries.
#[must_use]
pub fn command(item: AppMenuItemId) -> Option<Command> {
    let id = item.get();
    let nth = |first: u16| id.checked_sub(first).map(usize::from);
    Some(match id {
        PAUSE => Command::Pause,
        RESET => Command::Reset,
        CTRL_ALT_DELETE => Command::CtrlAltDelete,
        SCREENSHOT => Command::Screenshot,
        FULL_SCREEN => Command::FullScreen,
        SHUT_DOWN => Command::ShutDown,
        COPY_TEXT => Command::CopyText,
        PASTE => Command::Paste,
        STOP_PASTING => Command::StopPasting,
        SHARE_FOLDER => Command::ShareFolder,
        DEBUGGER => Command::Debugger,
        _ => {
            if let Some((rate, _)) = nth(FIRST_SPEED).and_then(|n| SPEEDS.get(n)) {
                return Some(Command::Speed(*rate));
            }
            Command::Effect(effect_of(id, FIRST_EFFECT)?)
        }
    })
}

#[cfg(test)]
#[path = "menu_tests.rs"]
mod tests;
