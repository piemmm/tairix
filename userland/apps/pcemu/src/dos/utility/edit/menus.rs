//! `EDIT`'s menus, as MS-DOS 5's has them: each entry's label with its key
//! letter after `&`, the shortcut shown beside it, and the command it gives;
//! drawn dropped from the menu bar, an entry greyed while it cannot be
//! chosen, and moved through with the arrows, its letter, or Enter.

use super::view::{menu_columns, Cells, Palette};

/// What `EDIT` can be told to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    New,
    Open,
    Save,
    SaveAs,
    Print,
    Exit,
    Cut,
    Copy,
    Paste,
    Clear,
    Find,
    Repeat,
    Change,
    Display,
    Started,
    Keys,
    About,
}

/// An entry: its label, its shortcut, what the status line says of it,
/// and its command; none for a rule.
pub struct Item {
    pub label: &'static [u8],
    pub shortcut: &'static [u8],
    pub hint: &'static [u8],
    pub command: Option<Command>,
}

const fn item(
    label: &'static [u8],
    shortcut: &'static [u8],
    hint: &'static [u8],
    command: Command,
) -> Item {
    Item {
        label,
        shortcut,
        hint,
        command: Some(command),
    }
}

const RULE: Item = Item {
    label: b"",
    shortcut: b"",
    hint: b"",
    command: None,
};

/// Each menu's entries, in the menu bar's order.
pub const MENUS: [&[Item]; 5] = [
    &[
        item(
            b"&New",
            b"",
            b"Clears the document, to start a new one",
            Command::New,
        ),
        item(
            b"&Open...",
            b"",
            b"Reads a file into the editor",
            Command::Open,
        ),
        item(
            b"&Save",
            b"",
            b"Writes the document to its file",
            Command::Save,
        ),
        item(
            b"Save &As...",
            b"",
            b"Writes the document to a file you name",
            Command::SaveAs,
        ),
        RULE,
        item(
            b"&Print...",
            b"",
            b"Sends the document, or the selection, to the printer",
            Command::Print,
        ),
        RULE,
        item(b"E&xit", b"", b"Leaves the editor", Command::Exit),
    ],
    &[
        item(
            b"Cu&t",
            b"Shift+Del",
            b"Moves the selection to the clipboard",
            Command::Cut,
        ),
        item(
            b"&Copy",
            b"Ctrl+Ins",
            b"Copies the selection to the clipboard",
            Command::Copy,
        ),
        item(
            b"&Paste",
            b"Shift+Ins",
            b"Puts the clipboard's text in at the cursor",
            Command::Paste,
        ),
        item(b"Cl&ear", b"Del", b"Deletes the selection", Command::Clear),
    ],
    &[
        item(b"&Find...", b"", b"Looks for text", Command::Find),
        item(
            b"&Repeat Last Find",
            b"F3",
            b"Looks for the same text again",
            Command::Repeat,
        ),
        item(
            b"&Change...",
            b"",
            b"Finds text and changes it",
            Command::Change,
        ),
    ],
    &[item(
        b"&Display...",
        b"",
        b"Sets the colours, the scroll bars and the tab stops",
        Command::Display,
    )],
    &[
        item(
            b"&Getting Started",
            b"",
            b"How to use the editor",
            Command::Started,
        ),
        item(
            b"&Keyboard",
            b"",
            b"The keys the editor answers",
            Command::Keys,
        ),
        RULE,
        item(b"&About...", b"", b"The editor's version", Command::About),
    ],
];

/// What the status line says of entry `item` of menu `menu`.
#[must_use]
pub fn hint(menu: u8, item: u8) -> &'static [u8] {
    MENUS[usize::from(menu.min(4))]
        .get(usize::from(item))
        .map_or(b"", |each| each.hint)
}

/// What a command needs to be chosen: a selection, the clipboard holding
/// something, a search made before.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Able {
    pub selection: bool,
    pub clipboard: bool,
    pub searched: bool,
}

impl Able {
    #[must_use]
    pub const fn allows(self, command: Command) -> bool {
        match command {
            Command::Cut | Command::Copy | Command::Clear => self.selection,
            Command::Paste => self.clipboard,
            Command::Repeat => self.searched,
            _ => true,
        }
    }
}

/// The letter after `&` in `label`, in capitals.
#[must_use]
pub fn key_letter(label: &[u8]) -> Option<u8> {
    let at = label.iter().position(|&byte| byte == b'&')?;
    label.get(at + 1).map(u8::to_ascii_uppercase)
}

/// The menu bar entry whose key letter is `letter`.
#[must_use]
pub fn menu_for(letter: u8) -> Option<u8> {
    let letter = letter.to_ascii_uppercase();
    let found = super::view::BAR
        .iter()
        .position(|&(label, _)| key_letter(label) == Some(letter))?;
    u8::try_from(found).ok()
}

/// Where menu `menu`'s box lies on a screen `columns` wide: its top, left,
/// bottom and right.
#[must_use]
pub fn rect(menu: u8, columns: u8) -> (u8, u8, u8, u8) {
    let items = MENUS[usize::from(menu.min(4))];
    let width = items
        .iter()
        .map(|each| {
            each.label.len()
                + if each.shortcut.is_empty() {
                    0
                } else {
                    each.shortcut.len() + 2
                }
        })
        .max()
        .unwrap_or(0);
    let width = u8::try_from(width + 3).unwrap_or(u8::MAX);
    let at = menu_columns(columns)[usize::from(menu.min(4))].saturating_sub(1);
    let left = at.min(columns.saturating_sub(width + 4));
    let bottom = 1 + u8::try_from(items.len()).unwrap_or(0) + 1;
    (1, left, bottom, left + width + 1)
}

/// Menu `menu` drawn with entry `chosen` lit, those `able` forbids greyed.
pub fn draw(cells: &mut Cells, palette: &Palette, menu: u8, chosen: u8, able: Able) {
    let (top, left, bottom, right) = rect(menu, cells.columns);
    cells.frame(top, left, bottom, right, palette.menu, b"");
    cells.shadow(top, left, bottom, right, palette.shadow);
    for (n, each) in MENUS[usize::from(menu.min(4))].iter().enumerate() {
        let row = top + 1 + u8::try_from(n).unwrap_or(0);
        let Some(command) = each.command else {
            cells.put(row, left, super::view::glyph::TEE_LEFT, palette.menu);
            for column in left + 1..right {
                cells.put(row, column, super::view::glyph::ACROSS, palette.menu);
            }
            cells.put(row, right, super::view::glyph::TEE_RIGHT, palette.menu);
            continue;
        };
        let lit = usize::from(chosen) == n;
        let (attribute, hot) = match (lit, able.allows(command)) {
            (true, _) => (palette.chosen, palette.chosen_hot),
            (false, true) => (palette.menu, palette.hot),
            (false, false) => (palette.disabled, palette.disabled),
        };
        cells.fill(row, left + 1, row, right - 1, b' ', attribute);
        cells.label(row, left + 2, each.label, attribute, hot);
        let shortcut = u8::try_from(each.shortcut.len()).unwrap_or(0);
        cells.write(row, right - 1 - shortcut, each.shortcut, attribute);
    }
}

/// What a key does to an open menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Menu `menu` open at entry `item`.
    At(u8, u8),
    Chosen(Command),
    Closed,
    /// The key did nothing here.
    Ignored,
}

/// The keys a menu answers, as the editor names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKey {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Escape,
    Letter(u8),
}

/// The entry `from` moves to in `items` by `step`, rules and greyed entries
/// passed over.
fn next_item(items: &[Item], from: u8, step: i8, able: Able) -> u8 {
    let count = i16::try_from(items.len()).unwrap_or(1);
    let mut at = i16::from(from);
    for _ in 0..count {
        at = (at + i16::from(step)).rem_euclid(count);
        let index = usize::try_from(at).unwrap_or(0);
        if items[index]
            .command
            .is_some_and(|command| able.allows(command))
        {
            return u8::try_from(at).unwrap_or(0);
        }
    }
    from
}

/// The first entry of `menu` that can be chosen.
#[must_use]
pub fn first(menu: u8, able: Able) -> u8 {
    let items = MENUS[usize::from(menu.min(4))];
    next_item(items, u8::try_from(items.len()).unwrap_or(1) - 1, 1, able)
}

/// Menu `menu`, open at `item`, given `key`.
#[must_use]
pub fn key(menu: u8, item: u8, key: MenuKey, able: Able) -> Outcome {
    let items = MENUS[usize::from(menu.min(4))];
    let count = u8::try_from(MENUS.len()).unwrap_or(1);
    match key {
        MenuKey::Up => Outcome::At(menu, next_item(items, item, -1, able)),
        MenuKey::Down => Outcome::At(menu, next_item(items, item, 1, able)),
        MenuKey::Left => {
            let menu = (menu + count - 1) % count;
            Outcome::At(menu, first(menu, able))
        }
        MenuKey::Right => {
            let menu = (menu + 1) % count;
            Outcome::At(menu, first(menu, able))
        }
        MenuKey::Enter => items
            .get(usize::from(item))
            .and_then(|each| each.command)
            .filter(|&command| able.allows(command))
            .map_or(Outcome::Ignored, Outcome::Chosen),
        MenuKey::Escape => Outcome::Closed,
        MenuKey::Letter(letter) => items
            .iter()
            .filter_map(|each| {
                each.command
                    .map(|command| (key_letter(each.label), command))
            })
            .find(|&(found, command)| {
                found == Some(letter.to_ascii_uppercase()) && able.allows(command)
            })
            .map_or(Outcome::Ignored, |(_, command)| Outcome::Chosen(command)),
    }
}

#[cfg(test)]
#[path = "menus_tests.rs"]
mod tests;
