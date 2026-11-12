//! Building an application's own menu a row at a time, and remembering what
//! each statement of an open menu's rows stood for.
//!
//! A menu is incidental to what an application does, so a row the bounded
//! menu cannot hold is left out rather than the whole menu; a submenu that
//! cannot be added takes its rows with it rather than letting them land on
//! another plate.

use tairix_abi::window_ipc::{
    AppMenu, AppMenuEntry, AppMenuEntryText, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuMark,
    AppMenuReason, AppMenuRole, AppMenuRow, AppMenuShortcut, AppMenuSymbol, MENU_OPEN_REVISION,
};
use tairix_abi::Errno;

/// Which plate of a menu a row is laid on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Plate {
    /// The plate the menu opens with.
    Root,
    /// The submenu the row at this index opens.
    Under(usize),
}

/// A command an application's menu rows choose, which says whether its work
/// is done where the menu is.
pub trait MenuCommand: Copy + Into<u16> {
    /// Whether choosing it leaves the menu with something to show — an undo,
    /// a zoom — so its row may be chosen again and again with the menu kept
    /// up; never a command that opens a window, a picker, an alert or a
    /// field, or ends what the menu acts on.
    fn acts_in_place(self) -> bool;
}

/// A menu being built.
#[derive(Clone, Debug)]
pub struct MenuBuilder {
    menu: AppMenu,
}

impl Default for MenuBuilder {
    fn default() -> Self {
        Self {
            menu: AppMenu::EMPTY,
        }
    }
}

/// How a chooseable row is marked, whether it may be chosen, the emphasis it
/// draws with, and whether a secondary click keeps its menu up.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Look {
    enabled: bool,
    mark: AppMenuMark,
    role: AppMenuRole,
    repeatable: bool,
}

impl Look {
    /// An unmarked, neutral row that may be chosen when `enabled`.
    const fn plain(enabled: bool) -> Self {
        Self {
            enabled,
            mark: AppMenuMark::None,
            role: AppMenuRole::Neutral,
            repeatable: false,
        }
    }

    /// This look drawing `mark` while `on`: a switch or an alternative, which
    /// sets something in place and so may be chosen with its menu kept up.
    const fn marked(self, mark: AppMenuMark, on: bool) -> Self {
        Self {
            mark: if on { mark } else { AppMenuMark::None },
            repeatable: true,
            ..self
        }
    }
}

/// What a chooseable row says: the id an answer names, the function its
/// picture shows, its label and its accelerator caption.
#[derive(Copy, Clone, Debug)]
struct Says<'a> {
    id: u16,
    symbol: AppMenuSymbol,
    label: &'a str,
    shortcut: &'a str,
}

impl MenuBuilder {
    /// An empty menu with no title.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty menu whose plate is titled `title`, or an untitled one where
    /// the title cannot be carried.
    #[must_use]
    pub fn titled(title: &str) -> Self {
        AppMenuLabel::new(title).map_or_else(
            |_| Self::default(),
            |title| Self {
                menu: AppMenu::titled(title),
            },
        )
    }

    fn push(&mut self, row: &AppMenuRow, plate: Plate) -> Result<(), Errno> {
        match plate {
            Plate::Root => self.menu.push(*row),
            Plate::Under(parent) => self.menu.push_under(*row, parent),
        }
    }

    /// The chooseable row `says` describes, or `None` where its label cannot
    /// be carried.
    fn row(says: Says<'_>, look: Look) -> Option<AppMenuItem> {
        let (Ok(id), Ok(label)) = (AppMenuItemId::new(says.id), AppMenuLabel::new(says.label))
        else {
            return None;
        };
        let mut item = AppMenuItem::new(id, label, says.symbol)
            .with_mark(look.mark)
            .with_role(look.role);
        if look.repeatable {
            item = item.repeatable();
        }
        if !says.shortcut.is_empty() {
            if let Ok(caption) = AppMenuShortcut::new(says.shortcut) {
                item = item.with_shortcut(caption);
            }
        }
        Some(if look.enabled { item } else { item.disabled() })
    }

    fn build(&mut self, says: Says<'_>, look: Look, plate: Plate) {
        if let Some(item) = Self::row(says, look) {
            let _ = self.push(&AppMenuRow::Item(item), plate);
        }
    }

    /// A row choosing `id`, doing `symbol`'s function, which may be chosen
    /// when `enabled`.
    pub fn item(
        &mut self,
        id: impl Into<u16>,
        symbol: AppMenuSymbol,
        label: &str,
        shortcut: &str,
        enabled: bool,
        plate: Plate,
    ) {
        let says = Says {
            id: id.into(),
            symbol,
            label,
            shortcut,
        };
        self.build(says, Look::plain(enabled), plate);
    }

    /// `command`'s row drawn as `look`, its menu kept up where
    /// [`MenuCommand::acts_in_place`] says so whatever `look` would keep.
    fn command_row(
        &mut self,
        command: impl MenuCommand,
        (symbol, label, shortcut): (AppMenuSymbol, &str, &str),
        look: Look,
        plate: Plate,
    ) {
        let says = Says {
            id: command.into(),
            symbol,
            label,
            shortcut,
        };
        let look = Look {
            repeatable: command.acts_in_place(),
            ..look
        };
        self.build(says, look, plate);
    }

    /// A row choosing `command`, which may be chosen when `enabled`, and with
    /// its menu kept up where [`MenuCommand::acts_in_place`] says so.
    pub fn command(
        &mut self,
        command: impl MenuCommand,
        symbol: AppMenuSymbol,
        label: &str,
        shortcut: &str,
        enabled: bool,
        plate: Plate,
    ) {
        let says = (symbol, label, shortcut);
        self.command_row(command, says, Look::plain(enabled), plate);
    }

    /// [`Self::mark`] for `command`: its menu is kept up where
    /// [`MenuCommand::acts_in_place`] says so, since a switch can open a
    /// window too.
    pub fn command_mark(
        &mut self,
        command: impl MenuCommand,
        symbol: AppMenuSymbol,
        label: &str,
        shortcut: &str,
        on: bool,
        plate: Plate,
    ) {
        let look = Look::plain(true).marked(AppMenuMark::Check, on);
        self.command_row(command, (symbol, label, shortcut), look, plate);
    }

    /// [`Self::radio`] for `command`: its menu is kept up where
    /// [`MenuCommand::acts_in_place`] says so.
    pub fn command_radio(
        &mut self,
        command: impl MenuCommand,
        symbol: AppMenuSymbol,
        label: &str,
        shortcut: &str,
        on: bool,
        plate: Plate,
    ) {
        let look = Look::plain(true).marked(AppMenuMark::Radio, on);
        self.command_row(command, (symbol, label, shortcut), look, plate);
    }

    /// A row choosing `id`, a command acting in place — an undo, a zoom — that
    /// may be chosen again and again with its menu kept up: never one that
    /// opens a window, a picker or an alert.
    pub fn repeating(
        &mut self,
        id: impl Into<u16>,
        symbol: AppMenuSymbol,
        label: &str,
        shortcut: &str,
        enabled: bool,
        plate: Plate,
    ) {
        let says = Says {
            id: id.into(),
            symbol,
            label,
            shortcut,
        };
        let look = Look {
            repeatable: true,
            ..Look::plain(enabled)
        };
        self.build(says, look, plate);
    }

    /// A row choosing `id` that destroys something or is otherwise hard to
    /// undo, drawn with the destructive emphasis.
    pub fn destructive(
        &mut self,
        id: impl Into<u16>,
        symbol: AppMenuSymbol,
        label: &str,
        shortcut: &str,
        enabled: bool,
        plate: Plate,
    ) {
        let says = Says {
            id: id.into(),
            symbol,
            label,
            shortcut,
        };
        let look = Look {
            role: AppMenuRole::Destructive,
            ..Look::plain(enabled)
        };
        self.build(says, look, plate);
    }

    /// A row turning a setting on or off, ticked while `on`. A secondary click
    /// turns it with the menu kept up.
    pub fn mark(
        &mut self,
        id: impl Into<u16>,
        symbol: AppMenuSymbol,
        label: &str,
        shortcut: &str,
        on: bool,
        plate: Plate,
    ) {
        let says = Says {
            id: id.into(),
            symbol,
            label,
            shortcut,
        };
        self.build(
            says,
            Look::plain(true).marked(AppMenuMark::Check, on),
            plate,
        );
    }

    /// A row choosing one of a group of alternatives, the chosen one while
    /// `on`. A secondary click chooses it with the menu kept up.
    pub fn radio(
        &mut self,
        id: impl Into<u16>,
        symbol: AppMenuSymbol,
        label: &str,
        shortcut: &str,
        on: bool,
        plate: Plate,
    ) {
        let says = Says {
            id: id.into(),
            symbol,
            label,
            shortcut,
        };
        self.build(
            says,
            Look::plain(true).marked(AppMenuMark::Radio, on),
            plate,
        );
    }

    /// A row choosing `id` that carries a quick-entry field starting at
    /// `initial`, whose commit answers with `entry`.
    pub fn entry(
        &mut self,
        id: impl Into<u16>,
        entry: impl Into<u16>,
        symbol: AppMenuSymbol,
        label: &str,
        initial: &str,
        plate: Plate,
    ) {
        let says = Says {
            id: id.into(),
            symbol,
            label,
            shortcut: "",
        };
        let (Some(item), Ok(entry), Ok(initial)) = (
            Self::row(says, Look::plain(true)),
            AppMenuItemId::new(entry.into()),
            AppMenuEntryText::new(initial),
        ) else {
            return;
        };
        let item = item.with_entry(AppMenuEntry::new(entry, initial));
        let _ = self.push(&AppMenuRow::Item(item), plate);
    }

    /// A rule between rows.
    pub fn separator(&mut self, plate: Plate) {
        let _ = self.push(&AppMenuRow::Separator, plate);
    }

    /// The icon bar's information row, which the desktop draws from the
    /// bundle's signed identity: the first row of an application's own
    /// icon-bar menu, as [`crate::declaration`] lays a flat one out.
    pub fn info(&mut self) {
        let _ = self.push(&AppMenuRow::Info, Plate::Root);
    }

    /// Open a submenu labelled `label` on `plate`, for rows doing `symbol`'s
    /// function, answering the plate its rows go on, or `None` when it could
    /// not be added.
    pub fn submenu(&mut self, label: &str, symbol: AppMenuSymbol, plate: Plate) -> Option<Plate> {
        let index = self.menu.len();
        let label = AppMenuLabel::new(label).ok()?;
        self.push(
            &AppMenuRow::Submenu {
                label,
                enabled: true,
                symbol,
                reason: AppMenuReason::EMPTY,
            },
            plate,
        )
        .ok()?;
        Some(Plate::Under(index))
    }

    /// The menu built.
    #[must_use]
    pub fn finish(self) -> AppMenu {
        self.menu
    }
}

/// What the rows of an open menu stood for, statement by statement: those it
/// shows, under the revision they were given, and those it showed before its
/// last restate, so a row chosen as that restate crossed it still names what
/// the user saw.
///
/// For a menu whose row ids are places in a list that can move. One whose ids
/// keep one meaning in every statement restates under [`MENU_OPEN_REVISION`]
/// and reads any answer alike.
#[derive(Clone, Debug)]
pub struct MenuStatements<T> {
    open_id: u64,
    revision: u32,
    shown: T,
    before: Option<(u32, T)>,
}

impl<T> MenuStatements<T> {
    /// The menu opened as `open_id`, its rows standing for `shown`.
    #[must_use]
    pub const fn new(open_id: u64, shown: T) -> Self {
        Self {
            open_id,
            revision: MENU_OPEN_REVISION,
            shown,
            before: None,
        }
    }

    /// The open the desktop minted for it.
    #[must_use]
    pub const fn open_id(&self) -> u64 {
        self.open_id
    }

    /// What the rows it shows stand for.
    #[must_use]
    pub const fn shown(&self) -> &T {
        &self.shown
    }

    /// The revision the rows it shows were given.
    #[must_use]
    pub const fn revision(&self) -> u32 {
        self.revision
    }

    /// The revision rows standing for something else are restated under.
    #[must_use]
    pub const fn next_revision(&self) -> u32 {
        self.revision.wrapping_add(1)
    }

    /// Its rows restated under `revision`, standing for `next`. Under the
    /// revision shown no row has changed its meaning, so `next` replaces what
    /// is shown and the statement before it is kept.
    pub fn adopt(&mut self, revision: u32, next: T) {
        if revision == self.revision {
            self.shown = next;
            return;
        }
        let was = core::mem::replace(&mut self.shown, next);
        self.before = Some((core::mem::replace(&mut self.revision, revision), was));
    }

    /// What the rows of `revision` stood for: those shown, or those shown
    /// before the last restate; `None` for older rows, whose ids may stand for
    /// anything now.
    #[must_use]
    pub fn chosen_from(&self, revision: u32) -> Option<&T> {
        if revision == self.revision {
            return Some(&self.shown);
        }
        self.before
            .as_ref()
            .filter(|(was, _)| *was == revision)
            .map(|(_, before)| before)
    }
}

#[cfg(test)]
#[path = "menu_tests.rs"]
mod tests;
