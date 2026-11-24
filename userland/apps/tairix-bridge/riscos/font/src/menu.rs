//! Font menus as `Font_ListFonts` builds them and `Font_DecodeMenu` reads
//! them back, laid out as RISC OS 5.30 lays them: the families' menu, then
//! every family's submenu in turn — built even where the family has nothing
//! to choose, though then not linked — each item's text in the indirected
//! data, the families' names first and then each submenu's, a title too
//! long for its menu's field among them.

use alloc::vec::Vec;

use tairix_riscos_kernel::msgtrans::menus::{self, header, item};
use tairix_riscos_kernel::os::len32;

use crate::catalogue::Family;

/// A menu's header and each item's size, and the room a title's text has,
/// as a buffer is indexed.
const HEADER: usize = header::LEN as usize;
const ITEM: usize = item::LEN as usize;
const TITLE_ROOM: usize = header::TITLE_ROOM as usize;
/// The colours: the title's foreground and background, the work area's.
const COLOURS: [u8; 4] = [7, 2, 7, 0];
/// Every item's height and the gap between items.
const HEIGHT: u32 = 44;
const GAP: u32 = 0;
/// Each item's icon: indirected text, black on white.
const ICON_FLAGS: u32 = 0x0700_0101;
/// What no submenu or validation string is.
pub(crate) const NONE: u32 = u32::MAX;
/// How many characters of a title its width leaves out.
const TITLE_SLACK: usize = 2;

/// The words a menu is labelled with.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Labels<'a> {
    pub(crate) title: &'a [u8],
    /// A family's own font.
    pub(crate) regular: &'a [u8],
    /// The system font's item, where the menu has one.
    pub(crate) system: &'a [u8],
}

/// How a menu is built.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Options {
    /// Whether the system font heads it.
    pub(crate) system: bool,
    /// The item ticked: the system font's, or a family's and its member's,
    /// by index.
    pub(crate) tick: Option<Tick>,
    /// What ends each text.
    pub(crate) terminator: u8,
}

/// The item a menu ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tick {
    System,
    Font { family: usize, member: usize },
}

/// What a menu needs: its bytes, and its indirected data's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Sizes {
    pub(crate) menu: usize,
    pub(crate) data: usize,
}

/// One menu: its title and items, each a text, its submenu and its flags.
struct Menu<'a> {
    title: &'a [u8],
    items: Vec<(&'a [u8], Option<usize>, u32)>,
}

/// The menus `families` make, in their order: the families' first.
fn menus<'a>(families: &[Family<'a>], labels: Labels<'a>, options: Options) -> Vec<Menu<'a>> {
    let mut main = Menu {
        title: labels.title,
        items: Vec::new(),
    };
    if options.system {
        let ticked = options.tick == Some(Tick::System);
        main.items.push((labels.system, None, ticked_flag(ticked)));
    }
    let mut menus = Vec::new();
    for (index, family) in families.iter().enumerate() {
        let ticked_member = match options.tick {
            Some(Tick::Font { family: f, member }) if f == index => Some(member),
            _ => None,
        };
        let submenu = family.opens().then_some(index + 1);
        main.items
            .push((family.name, submenu, ticked_flag(ticked_member.is_some())));
        menus.push(Menu {
            title: family.name,
            items: family
                .members
                .iter()
                .enumerate()
                .map(|(m, member)| {
                    let ticked = ticked_member == Some(m);
                    (
                        member.label.unwrap_or(labels.regular),
                        None,
                        ticked_flag(ticked),
                    )
                })
                .collect(),
        });
    }
    menus.insert(0, main);
    menus
}

/// An item's tick, as its flags have it.
const fn ticked_flag(ticked: bool) -> u32 {
    if ticked {
        item::TICKED
    } else {
        0
    }
}

/// The sizes the menu of `families` needs.
pub(crate) fn sizes(families: &[Family<'_>], labels: Labels<'_>, options: Options) -> Sizes {
    let menus = menus(families, labels, options);
    let menu = menus.iter().map(|m| HEADER + ITEM * m.items.len()).sum();
    let data = menus
        .iter()
        .map(|m| {
            let title = if m.title.len() > TITLE_ROOM {
                m.title.len() + 1
            } else {
                0
            };
            title
                + m.items
                    .iter()
                    .map(|(text, ..)| text.len() + 1)
                    .sum::<usize>()
        })
        .sum();
    Sizes { menu, data }
}

/// The menu of `families` laid over `menu` — the guest's buffer as it was,
/// at `menu_at` — with its indirected data, at `data_at`, made: a title
/// written only as far as its terminator, the rest of its field as it was.
pub(crate) fn build(
    families: &[Family<'_>],
    labels: Labels<'_>,
    options: Options,
    menu: &mut [u8],
    menu_at: u32,
    data_at: u32,
) -> Vec<u8> {
    let menus = menus(families, labels, options);
    let mut starts = Vec::with_capacity(menus.len());
    let mut at = 0usize;
    for m in &menus {
        starts.push(at);
        at += HEADER + ITEM * m.items.len();
    }
    let address = |base: u32, offset: usize| base.wrapping_add(len32(offset));
    let mut data = Vec::new();
    let mut text_at = |text: &[u8]| {
        let offset = data.len();
        data.extend_from_slice(text);
        data.push(options.terminator);
        address(data_at, offset)
    };
    for (m, &start) in menus.iter().zip(&starts) {
        let indirected_title = m.title.len() > TITLE_ROOM;
        let title_text = indirected_title.then(|| text_at(m.title));
        let head = &mut menu[start..start + HEADER];
        if let Some(pointer) = title_text {
            put(head, header::TITLE, pointer);
            put(head, header::TITLE + 4, 0);
            put(head, header::TITLE + 8, 0);
        } else {
            let shown = m.title.len();
            head[..shown].copy_from_slice(m.title);
            if shown < TITLE_ROOM {
                head[shown] = options.terminator;
            }
        }
        let colours = header::COLOURS as usize;
        head[colours..colours + COLOURS.len()].copy_from_slice(&COLOURS);
        put(head, header::WIDTH, width(m));
        put(head, header::HEIGHT, HEIGHT);
        put(head, header::GAP, GAP);
        let count = m.items.len();
        for (i, &(text, submenu, ticked)) in m.items.iter().enumerate() {
            let mut flags = ticked;
            if i + 1 == count {
                flags |= item::LAST;
            }
            if i == 0 && indirected_title {
                flags |= item::INDIRECTED_TITLE;
            }
            let pointer = text_at(text);
            let entry = start + HEADER + ITEM * i;
            let entry = &mut menu[entry..entry + ITEM];
            put(entry, item::FLAGS, flags);
            put(
                entry,
                item::SUBMENU,
                submenu.map_or(NONE, |s| address(menu_at, starts[s])),
            );
            put(entry, item::ICON_FLAGS, ICON_FLAGS);
            put(entry, item::TEXT, pointer);
            put(entry, item::VALIDATION, NONE);
            put(entry, item::BUFFER_SIZE, len32(text.len() + 1));
        }
    }
    data
}

/// A menu's width: room for its longest text, or for its title less two
/// characters where that is longer.
fn width(menu: &Menu<'_>) -> u32 {
    let longest = menu
        .items
        .iter()
        .map(|(text, ..)| text.len())
        .chain([menu.title.len().saturating_sub(TITLE_SLACK)])
        .max()
        .unwrap_or(0);
    menus::width(len32(longest))
}

/// The word at field `at` of a menu's block set to `value`.
fn put(bytes: &mut [u8], at: u32, value: u32) {
    crate::put_word(bytes, at as usize, value);
}

#[cfg(test)]
mod tests;
