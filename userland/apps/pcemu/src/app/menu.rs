//! A machine window's menu: what it shows of the machine as it runs — paused
//! or not, its processor's speed, its tube, full screen or not, whether its
//! screen shows text to copy, whether text is being typed on it, whether
//! another folder may be shared and whether its debugger is open — and the
//! command each row is.

use tairix_abi::window_ipc::{
    AppMenu, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuMark, AppMenuReason, AppMenuRow,
    AppMenuSymbol,
};
use tairix_abi::Errno;
use tairix_crt::Tube;

use super::settings::SPEEDS;
use crate::time::CpuClock;

/// One of the tube's effects, as a row turns it on at a strength of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Alternate rows dimmed.
    ScanLines,
    /// Lit pixels' light spread into their neighbourhood.
    Glow,
    /// What was lit lingering as an afterglow.
    Phosphor,
    /// An analogue noise floor.
    Noise,
    /// Rows displaced along a travelling wave.
    Wobble,
}

impl Effect {
    /// Every effect, in the order the menu lists them.
    pub const ALL: [Self; 5] = [
        Self::ScanLines,
        Self::Glow,
        Self::Phosphor,
        Self::Noise,
        Self::Wobble,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::ScanLines => "Scan Lines",
            Self::Glow => "Glow",
            Self::Phosphor => "Phosphor Afterglow",
            Self::Noise => "Noise",
            Self::Wobble => "Wobble",
        }
    }

    const fn symbol(self) -> AppMenuSymbol {
        match self {
            Self::ScanLines => AppMenuSymbol::PixelGrid,
            Self::Glow => AppMenuSymbol::Brightness,
            Self::Phosphor => AppMenuSymbol::Visible,
            Self::Noise => AppMenuSymbol::Noise,
            Self::Wobble => AppMenuSymbol::Adjust,
        }
    }

    /// The strength a row turns the effect on at, in permille: a period
    /// monitor's, not the most the tube can draw.
    const fn strength(self) -> u16 {
        match self {
            Self::ScanLines => 450,
            Self::Glow => 350,
            Self::Phosphor => 550,
            Self::Noise => 200,
            Self::Wobble => 250,
        }
    }

    fn field(self, tube: &mut Tube) -> &mut u16 {
        match self {
            Self::ScanLines => &mut tube.scanlines,
            Self::Glow => &mut tube.glow,
            Self::Phosphor => &mut tube.phosphor,
            Self::Noise => &mut tube.fuzz,
            Self::Wobble => &mut tube.wobble,
        }
    }

    /// Whether the effect is on in `tube`.
    #[must_use]
    pub fn is_on(self, mut tube: Tube) -> bool {
        *self.field(&mut tube) > 0
    }

    /// `tube` with the effect turned off where it is on, and on where off.
    #[must_use]
    pub fn toggled(self, mut tube: Tube) -> Tube {
        let field = self.field(&mut tube);
        *field = if *field > 0 { 0 } else { self.strength() };
        tube
    }
}

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

fn item(
    id: u16,
    label: &str,
    symbol: AppMenuSymbol,
    mark: AppMenuMark,
) -> Result<AppMenuRow, Errno> {
    Ok(AppMenuRow::Item(
        AppMenuItem::new(AppMenuItemId::new(id)?, AppMenuLabel::new(label)?, symbol)
            .with_mark(mark),
    ))
}

const fn marked(on: bool, mark: AppMenuMark) -> AppMenuMark {
    if on {
        mark
    } else {
        AppMenuMark::None
    }
}

fn submenu(label: &str, symbol: AppMenuSymbol) -> Result<AppMenuRow, Errno> {
    Ok(AppMenuRow::Submenu {
        label: AppMenuLabel::new(label)?,
        enabled: true,
        symbol,
        reason: AppMenuReason::EMPTY,
    })
}

/// The menu of a machine `shown` as it stands.
///
/// # Errors
///
/// The menu builder's refusal, which rows this size never meet.
pub fn menu(shown: &Shown) -> Result<AppMenu, Errno> {
    let mut menu = AppMenu::EMPTY;
    menu.push(item(
        PAUSE,
        "Pause",
        AppMenuSymbol::Pause,
        marked(shown.paused, AppMenuMark::Check),
    )?)?;
    menu.push(item(
        RESET,
        "Reset",
        AppMenuSymbol::Refresh,
        AppMenuMark::None,
    )?)?;
    menu.push(item(
        CTRL_ALT_DELETE,
        "Send Ctrl+Alt+Delete",
        AppMenuSymbol::Restart,
        AppMenuMark::None,
    )?)?;
    menu.push(AppMenuRow::Separator)?;
    let copy = AppMenuItem::new(
        AppMenuItemId::new(COPY_TEXT)?,
        AppMenuLabel::new("Copy Screen Text")?,
        AppMenuSymbol::Copy,
    );
    menu.push(AppMenuRow::Item(if shown.clipboard.copyable {
        copy
    } else {
        copy.with_reason(AppMenuReason::new("The screen shows graphics")?)
            .disabled()
    }))?;
    menu.push(if shown.clipboard.pasting {
        item(
            STOP_PASTING,
            "Stop Pasting",
            AppMenuSymbol::Close,
            AppMenuMark::None,
        )?
    } else {
        item(PASTE, "Paste", AppMenuSymbol::Paste, AppMenuMark::None)?
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
    let speed = menu.rows().count();
    menu.push(submenu("Speed", AppMenuSymbol::Clock)?)?;
    for ((rate, label), id) in SPEEDS.iter().zip(FIRST_SPEED..) {
        let row = item(
            id,
            label,
            AppMenuSymbol::Clock,
            marked(*rate == shown.speed, AppMenuMark::Radio),
        )?;
        menu.push_under(row, speed)?;
    }
    let display = menu.rows().count();
    menu.push(submenu("Display", AppMenuSymbol::Display)?)?;
    for (effect, id) in Effect::ALL.into_iter().zip(FIRST_EFFECT..) {
        let mark = marked(effect.is_on(shown.tube), AppMenuMark::Check);
        menu.push_under(item(id, effect.label(), effect.symbol(), mark)?, display)?;
    }
    push_windows(&mut menu, shown.windows)?;
    Ok(menu)
}

/// The menu's tail: the screen's picture, its windows, and shutting down.
fn push_windows(menu: &mut AppMenu, windows: Windows) -> Result<(), Errno> {
    menu.push(AppMenuRow::Separator)?;
    menu.push(item(
        SCREENSHOT,
        "Take Screenshot\u{2026}",
        AppMenuSymbol::Image,
        AppMenuMark::None,
    )?)?;
    menu.push(item(
        FULL_SCREEN,
        "Full Screen",
        AppMenuSymbol::ZoomFit,
        marked(windows.fullscreen, AppMenuMark::Check),
    )?)?;
    menu.push(AppMenuRow::Separator)?;
    menu.push(item(
        DEBUGGER,
        "Debugger",
        AppMenuSymbol::ViewCode,
        marked(windows.debugging, AppMenuMark::Check),
    )?)?;
    menu.push(AppMenuRow::Separator)?;
    menu.push(item(
        SHUT_DOWN,
        "Shut Down",
        AppMenuSymbol::ShutDown,
        AppMenuMark::None,
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
            Command::Effect(*nth(FIRST_EFFECT).and_then(|n| Effect::ALL.get(n))?)
        }
    })
}

#[cfg(test)]
#[path = "menu_tests.rs"]
mod tests;
