//! Font menus as `Font_ListFonts` builds them and `Font_DecodeMenu` reads
//! them back, laid out as RISC OS 5.30 lays them: the families' menu, then
//! every family's submenu in turn — built even where the family has nothing
//! to choose, though then not linked — each item's text in the indirected
//! data, the families' names first and then each submenu's, a title too
//! long for its menu's field among them.

use alloc::vec::Vec;

use tairix_riscos_kernel::os::len32;

use crate::catalogue::Family;

/// A menu's header and each item's size.
pub(crate) const HEADER: usize = 28;
pub(crate) const ITEM: usize = 24;
/// The title field's size.
const TITLE: usize = 12;
/// The colours: the title's foreground and background, the work area's.
const COLOURS: [u8; 4] = [7, 2, 7, 0];
/// Every item's height and the gap between items.
const HEIGHT: u32 = 44;
const GAP: u32 = 0;
/// Each item's icon: indirected text, black on white.
const ICON_FLAGS: u32 = 0x0700_0101;
/// What no submenu or validation string is.
pub(crate) const NONE: u32 = u32::MAX;
/// An item's flags.
pub(crate) mod flag {
    pub(crate) const TICKED: u32 = 1;
    pub(crate) const LAST: u32 = 1 << 7;
    pub(crate) const INDIRECTED_TITLE: u32 = 1 << 8;
}
/// An item's fields.
pub(crate) mod field {
    pub(crate) const FLAGS: usize = 0;
    pub(crate) const SUBMENU: usize = 4;
    pub(crate) const ICON_FLAGS: usize = 8;
    pub(crate) const TEXT: usize = 12;
}
/// The icon flag that makes an item's text indirected.
pub(crate) const INDIRECTED: u32 = 1 << 8;
/// A width's fixed part and each character's.
const WIDTH_BASE: u32 = 12;
const WIDTH_PER_CHARACTER: u32 = 16;
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
        flag::TICKED
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
            let title = if m.title.len() > TITLE {
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
        let indirected_title = m.title.len() > TITLE;
        let title_text = indirected_title.then(|| text_at(m.title));
        let header = &mut menu[start..start + HEADER];
        if let Some(pointer) = title_text {
            put(header, 0, pointer);
            put(header, 4, 0);
            put(header, 8, 0);
        } else {
            let shown = m.title.len();
            header[..shown].copy_from_slice(m.title);
            if shown < TITLE {
                header[shown] = options.terminator;
            }
        }
        header[TITLE..TITLE + 4].copy_from_slice(&COLOURS);
        put(header, 16, width(m));
        put(header, 20, HEIGHT);
        put(header, 24, GAP);
        let count = m.items.len();
        for (i, &(text, submenu, ticked)) in m.items.iter().enumerate() {
            let mut flags = ticked;
            if i + 1 == count {
                flags |= flag::LAST;
            }
            if i == 0 && indirected_title {
                flags |= flag::INDIRECTED_TITLE;
            }
            let pointer = text_at(text);
            let item = start + HEADER + ITEM * i;
            let item = &mut menu[item..item + ITEM];
            put(item, field::FLAGS, flags);
            put(
                item,
                field::SUBMENU,
                submenu.map_or(NONE, |s| address(menu_at, starts[s])),
            );
            put(item, field::ICON_FLAGS, ICON_FLAGS);
            put(item, field::TEXT, pointer);
            put(item, field::TEXT + 4, NONE);
            put(item, field::TEXT + 8, len32(text.len() + 1));
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
    WIDTH_BASE + WIDTH_PER_CHARACTER * len32(longest)
}

fn put(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests;
