//! Building an application's own menu a row at a time.
//!
//! A menu is incidental to what an application does, so a row the bounded
//! menu cannot hold is left out rather than the whole menu; a submenu that
//! cannot be added takes its rows with it rather than letting them land on
//! another plate.

use tairix_abi::window_ipc::{
    AppMenu, AppMenuEntry, AppMenuEntryText, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuMark,
    AppMenuReason, AppMenuRole, AppMenuRow, AppMenuShortcut, AppMenuSymbol,
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

/// How a chooseable row is marked, whether it may be chosen, and the
/// emphasis it draws with.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Look {
    enabled: bool,
    mark: AppMenuMark,
    role: AppMenuRole,
}

impl Look {
    /// An unmarked, neutral row that may be chosen when `enabled`.
    const fn plain(enabled: bool) -> Self {
        Self {
            enabled,
            mark: AppMenuMark::None,
            role: AppMenuRole::Neutral,
        }
    }

    /// This look drawing `mark` while `on`.
    const fn marked(self, mark: AppMenuMark, on: bool) -> Self {
        Self {
            mark: if on { mark } else { AppMenuMark::None },
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

    /// A row turning a setting on or off, ticked while `on`.
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
    /// `on`.
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

#[cfg(test)]
#[path = "menu_tests.rs"]
mod tests;
