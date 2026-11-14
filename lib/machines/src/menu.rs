//! A machine card's menu in the library window, the rows every machine
//! window's menu builds from — its tube's effects among them — and a menu kept
//! up that is restated only when what it shows changes.
//!
//! A row acting on the machine in place may be chosen with a secondary click
//! and the menu kept up; one opening a window or a picker, or ending the
//! machine, may not.

use tairix_abi::window_ipc::{
    AppBarClick, AppMenu, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuMark, AppMenuReason,
    AppMenuRow, AppMenuSymbol,
};
use tairix_abi::Errno;
use tairix_crt::Tube;

use crate::standing::Standing;

/// The first id the card's own rows take: an application numbers its machine
/// rows below it.
pub const CARD_ROWS: u16 = 0xFF00;

/// The icon-bar row that brings the library back.
pub const LIBRARY_ROW: u16 = 2;

/// What a primary press on the application's icon-bar slot does: the library
/// is the way back once every window is closed.
pub const BAR_CLICK: AppBarClick = AppBarClick::RaiseOrOpen;

/// The application's own rows on its icon-bar menu, between the information
/// row and *Quit*: the library's.
///
/// # Errors
///
/// [`Errno`] from the menu's bounds, which these fixed rows stay within.
pub fn bar_rows() -> Result<[AppMenuRow; 1], Errno> {
    Ok([AppMenuRow::Item(AppMenuItem::new(
        AppMenuItemId::new(LIBRARY_ROW)?,
        AppMenuLabel::new("Machine Library")?,
        AppMenuSymbol::Application,
    ))])
}
const START: u16 = CARD_ROWS;
const SETTINGS: u16 = CARD_ROWS + 1;
const DUPLICATE: u16 = CARD_ROWS + 2;
const DELETE: u16 = CARD_ROWS + 3;

/// What a row of a machine card's menu does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardCommand {
    /// Start the machine, or bring its window forward.
    Start,
    /// Its settings.
    Settings,
    /// A copy of it.
    Duplicate,
    /// Delete it, once asked.
    Delete,
    /// One of the machine's own rows, which its application reads.
    Machine(AppMenuItemId),
}

/// A machine card's menu, for a machine standing as `standing`: what starts
/// it, configures it, copies and deletes it, and, where it runs, the rows
/// `running` adds of the machine's own.
///
/// # Errors
///
/// The menu builder's refusal, and any `running` makes.
pub fn card_menu(
    standing: Standing,
    running: Option<impl FnOnce(&mut AppMenu) -> Result<(), Errno>>,
) -> Result<AppMenu, Errno> {
    let mut menu = AppMenu::EMPTY;
    let (start, symbol) = match (standing, running.is_some()) {
        (_, true) => ("Show Window", AppMenuSymbol::SwitchTo),
        (Standing::Suspended, false) => ("Resume", AppMenuSymbol::Resume),
        _ => ("Start", AppMenuSymbol::Play),
    };
    let start = AppMenuItem::new(
        AppMenuItemId::new(START)?,
        AppMenuLabel::new(start)?,
        symbol,
    );
    menu.push(AppMenuRow::Item(if standing.startable() {
        start
    } else {
        start
            .with_reason(AppMenuReason::new(
                "The machine is being kept as it suspends",
            )?)
            .disabled()
    }))?;
    if let Some(push) = running {
        menu.push(AppMenuRow::Separator)?;
        push(&mut menu)?;
    }
    menu.push(AppMenuRow::Separator)?;
    let settings = AppMenuItem::new(
        AppMenuItemId::new(SETTINGS)?,
        AppMenuLabel::new("Settings\u{2026}")?,
        AppMenuSymbol::Settings,
    );
    menu.push(AppMenuRow::Item(settings))?;
    let at_rest = |id: u16, label: &str, symbol: AppMenuSymbol| -> Result<AppMenuRow, Errno> {
        let row = AppMenuItem::new(AppMenuItemId::new(id)?, AppMenuLabel::new(label)?, symbol);
        Ok(AppMenuRow::Item(if standing.at_rest() {
            row
        } else {
            row.with_reason(AppMenuReason::new("Shut the machine down first")?)
                .disabled()
        }))
    };
    menu.push(at_rest(DUPLICATE, "Duplicate", AppMenuSymbol::Copy)?)?;
    menu.push(at_rest(DELETE, "Delete\u{2026}", AppMenuSymbol::Delete)?)?;
    Ok(menu)
}

/// What the row `item` of a machine card's menu does.
#[must_use]
pub const fn card_command(item: AppMenuItemId) -> CardCommand {
    match item.get() {
        START => CardCommand::Start,
        SETTINGS => CardCommand::Settings,
        DUPLICATE => CardCommand::Duplicate,
        DELETE => CardCommand::Delete,
        _ => CardCommand::Machine(item),
    }
}

/// Whether a row acts on the machine in place, so a secondary click may choose
/// it with the menu kept up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Acts {
    /// On the machine as it runs.
    InPlace,
    /// It opens a window or a picker, or ends the machine.
    Elsewhere,
}

/// A row of `id`, `label` and `symbol`, marked with `mark`, that acts as
/// `acts` says.
///
/// # Errors
///
/// The menu builder's refusal of an id or a label.
pub fn item(
    id: u16,
    label: &str,
    symbol: AppMenuSymbol,
    (mark, acts): (AppMenuMark, Acts),
) -> Result<AppMenuRow, Errno> {
    let item = AppMenuItem::new(AppMenuItemId::new(id)?, AppMenuLabel::new(label)?, symbol)
        .with_mark(mark);
    Ok(AppMenuRow::Item(match acts {
        Acts::InPlace => item.repeatable(),
        Acts::Elsewhere => item,
    }))
}

/// `mark` where `on`, else none.
#[must_use]
pub const fn marked(on: bool, mark: AppMenuMark) -> AppMenuMark {
    if on {
        mark
    } else {
        AppMenuMark::None
    }
}

/// Push a submenu row for the rows then pushed under it; where it lies.
///
/// # Errors
///
/// The menu builder's refusal.
pub fn submenu(menu: &mut AppMenu, label: &str, symbol: AppMenuSymbol) -> Result<usize, Errno> {
    let at = menu.rows().count();
    menu.push(AppMenuRow::Submenu {
        label: AppMenuLabel::new(label)?,
        enabled: true,
        symbol,
        reason: AppMenuReason::EMPTY,
    })?;
    Ok(at)
}

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
    /// Every effect, in the order the Display submenu lists them.
    pub const ALL: [Self; 5] = [
        Self::ScanLines,
        Self::Glow,
        Self::Phosphor,
        Self::Noise,
        Self::Wobble,
    ];

    /// What its row is called.
    #[must_use]
    pub const fn label(self) -> &'static str {
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

/// Push the Display submenu: a row for each of [`Effect::ALL`], ticked where
/// `tube` draws it, numbered in order from `first`.
///
/// # Errors
///
/// The menu builder's refusal.
pub fn push_display(menu: &mut AppMenu, tube: Tube, first: u16) -> Result<(), Errno> {
    let display = submenu(menu, "Display", AppMenuSymbol::Display)?;
    for (effect, id) in Effect::ALL.into_iter().zip(first..) {
        let mark = marked(effect.is_on(tube), AppMenuMark::Check);
        let row = item(id, effect.label(), effect.symbol(), (mark, Acts::InPlace))?;
        menu.push_under(row, display)?;
    }
    Ok(())
}

/// The effect row `id` of a Display submenu numbered from `first` turns; none
/// for a row it does not hold.
#[must_use]
pub fn effect_of(id: u16, first: u16) -> Option<Effect> {
    Effect::ALL
        .get(usize::from(id.checked_sub(first)?))
        .copied()
}

/// A menu up, which menu it is, and the rows it was last given: restated
/// only where the rows its subject now builds differ, so the desktop hears of
/// a change once and of no change at all.
#[derive(Clone, Debug)]
pub struct Followed<K> {
    open: u64,
    kind: K,
    rows: AppMenu,
}

impl<K: Copy> Followed<K> {
    /// The menu opened as `open`, of `kind`, given `rows`.
    #[must_use]
    pub const fn new(open: u64, kind: K, rows: AppMenu) -> Self {
        Self { open, kind, rows }
    }

    /// The open the desktop minted for it.
    #[must_use]
    pub const fn open(&self) -> u64 {
        self.open
    }

    /// Which menu it is.
    #[must_use]
    pub const fn kind(&self) -> K {
        self.kind
    }

    /// The rows to restate it with where `now`, the rows as its subject now
    /// stands, are not those it was last given; they are what it was given
    /// from then on.
    pub fn follow(&mut self, now: AppMenu) -> Option<&AppMenu> {
        if now == self.rows {
            return None;
        }
        self.rows = now;
        Some(&self.rows)
    }
}

#[cfg(test)]
#[path = "menu_tests.rs"]
mod tests;
