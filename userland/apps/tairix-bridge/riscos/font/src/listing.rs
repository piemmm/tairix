//! `Font_ListFonts` and `Font_DecodeMenu`: the catalogue a font or encoding
//! at a time, or as a menu, and a choice from such a menu read back.

use alloc::vec::Vec;

use tairix_riscos_kernel::error::{BAD_PARAMETERS, BUFFER_OVERFLOW};
use tairix_riscos_kernel::msgtrans::menus::{header, item};
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};

use crate::catalogue::{families, Catalogue, Family};
use crate::find::read_identifier;
use crate::identifier::{letter, FIELD};
use crate::menu::{self, Labels, Options, Tick, NONE};
use crate::module::{error, message, raise, with};
use crate::scan;

/// `Font_ListFonts`'s R2: the counter, and the flags above it.
mod list {
    pub(super) const COUNTER: u32 = 0xFFFF;
    pub(super) const IDENTIFIER: u32 = 1 << 16;
    pub(super) const NAME: u32 = 1 << 17;
    pub(super) const RETURNS: u32 = 1 << 18;
    pub(super) const MENU: u32 = 1 << 19;
    pub(super) const SYSTEM: u32 = 1 << 20;
    pub(super) const TICK: u32 = 1 << 21;
    pub(super) const ENCODINGS: u32 = 1 << 22;
    /// The flags a list may have, and a menu.
    pub(super) const LIST_FLAGS: u32 = IDENTIFIER | NAME | RETURNS | ENCODINGS;
    pub(super) const MENU_FLAGS: u32 = RETURNS | MENU | SYSTEM | TICK | ENCODINGS;
    /// The room RISC OS 2's form assumes for an identifier.
    pub(super) const RISC_OS_2_ROOM: u32 = 40;
}
/// What ends a list's names unless returns are asked for.
const ZERO: u8 = 0;
const RETURN: u8 = b'\r';
/// R6's tick for the system font.
const TICK_SYSTEM: u32 = 1;
/// R2 at the end of the list.
const DONE: u32 = u32::MAX;

/// `Font_ListFonts`.
pub(crate) fn list_fonts(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut flags = regs.r[2];
    if flags & !list::COUNTER == 0 {
        flags |= list::IDENTIFIER | list::RETURNS;
        regs.r[3] = list::RISC_OS_2_ROOM;
    }
    if flags & list::MENU != 0 {
        if flags & !(list::COUNTER | list::MENU_FLAGS) != 0 {
            return Err(os.error(BAD_PARAMETERS));
        }
        return list_menu(os, regs, flags);
    }
    if flags & !(list::COUNTER | list::LIST_FLAGS) != 0 {
        return Err(os.error(BAD_PARAMETERS));
    }
    let index = (flags & list::COUNTER) as usize;
    let encodings = flags & list::ENCODINGS != 0;
    let entry = scan::with_catalogue(os, false, |_, catalogue| {
        Ok(if encodings {
            catalogue
                .listed_encodings()
                .nth(index)
                .map(|e| (e.identifier.clone(), e.shown().to_vec()))
        } else {
            catalogue
                .fonts
                .get(index)
                .map(|f| (f.identifier.clone(), f.shown().to_vec()))
        })
    })?;
    let Some((identifier, name)) = entry else {
        regs.r[2] = DONE;
        return Ok(());
    };
    let end = if flags & list::RETURNS != 0 {
        RETURN
    } else {
        ZERO
    };
    if flags & list::IDENTIFIER != 0 {
        if let Err(fail) = give(os, regs.r[1], &mut regs.r[3], &identifier, end) {
            regs.r[2] = DONE;
            return Err(fail);
        }
    }
    if flags & list::NAME != 0 {
        if let Err(fail) = give(os, regs.r[4], &mut regs.r[5], &name, end) {
            regs.r[2] = DONE;
            return Err(fail);
        }
    }
    regs.r[2] = (flags & !list::COUNTER) | len32(index + 1);
    Ok(())
}

/// `text` ended by `end` into the buffer at `at` of `*room` bytes, `*room`
/// left its length; with `at` nought, `*room` made the room it needs.
fn give(os: &mut Os<'_>, at: u32, room: &mut u32, text: &[u8], end: u8) -> Result<(), Fail> {
    let needed = len32(text.len() + 1);
    if at == 0 {
        *room = needed;
        return Ok(());
    }
    if needed > *room {
        return Err(os.error(BUFFER_OVERFLOW));
    }
    os.write_bytes(at, text)?;
    os.write_byte(at.wrapping_add(needed - 1), end)?;
    *room = needed;
    Ok(())
}

/// The words a menu is labelled with, from the Font Manager's messages.
struct Words {
    title: Vec<u8>,
    regular: Vec<u8>,
    system: Vec<u8>,
}

impl Words {
    fn read(os: &mut Os<'_>) -> Result<Self, Fail> {
        Ok(Self {
            title: message(os, "FontList")?,
            regular: message(os, "Regular")?,
            system: message(os, "SystemFont")?,
        })
    }

    fn labels(&self) -> Labels<'_> {
        Labels {
            title: &self.title,
            regular: &self.regular,
            system: &self.system,
        }
    }
}

/// The names a menu of the catalogue shows, fonts or encodings.
fn shown(catalogue: &Catalogue, encodings: bool) -> Vec<&[u8]> {
    if encodings {
        catalogue
            .listed_encodings()
            .map(crate::catalogue::Encoding::shown)
            .collect()
    } else {
        catalogue
            .fonts
            .iter()
            .map(crate::catalogue::Font::shown)
            .collect()
    }
}

/// `Font_ListFonts` building a menu: R1 its buffer of R3 bytes and R4 its
/// indirected data's of R5, either nought asking only its size.
fn list_menu(os: &mut Os<'_>, regs: &mut Regs, flags: u32) -> Result<(), Fail> {
    let words = Words::read(os)?;
    let encodings = flags & list::ENCODINGS != 0;
    let ticked = if flags & list::TICK != 0 {
        match regs.r[6] {
            0 => None,
            TICK_SYSTEM => Some(Ticked::System),
            at => Some(Ticked::Font(read_identifier(os, at)?)),
        }
    } else {
        None
    };
    let terminator = if flags & list::RETURNS != 0 {
        RETURN
    } else {
        ZERO
    };
    let (menu_at, data_at) = (regs.r[1], regs.r[4]);
    let (room_menu, room_data) = (regs.r[3], regs.r[5]);
    let made = scan::with_catalogue(os, false, |os, catalogue| {
        let names = shown(catalogue, encodings);
        let families = families(names.iter().copied());
        let tick = ticked.as_ref().and_then(|ticked| match ticked {
            Ticked::System => Some(Tick::System),
            Ticked::Font(identifier) => tick_of(catalogue, &families, identifier),
        });
        let options = Options {
            system: flags & list::SYSTEM != 0,
            tick,
            terminator,
        };
        let sizes = menu::sizes(&families, words.labels(), options);
        if families.is_empty() || menu_at == 0 || data_at == 0 {
            return Ok((sizes, families.is_empty()));
        }
        if len32(sizes.menu) > room_menu || len32(sizes.data) > room_data {
            return Err(os.error(BUFFER_OVERFLOW));
        }
        let mut bytes = os.read_vec(menu_at, sizes.menu)?;
        let data = menu::build(
            &families,
            words.labels(),
            options,
            &mut bytes,
            menu_at,
            data_at,
        );
        os.write_bytes(menu_at, &bytes)?;
        os.write_bytes(data_at, &data)?;
        Ok((sizes, false))
    })?;
    let (sizes, empty) = made;
    regs.r[3] = if empty { 0 } else { len32(sizes.menu) };
    regs.r[5] = len32(sizes.data);
    Ok(())
}

/// What a menu is to tick.
enum Ticked {
    System,
    Font(Vec<u8>),
}

/// The family and member of the font `identifier` names.
fn tick_of(catalogue: &Catalogue, families: &[Family<'_>], identifier: &[u8]) -> Option<Tick> {
    let index = catalogue
        .fonts
        .iter()
        .position(|font| font.identifier.eq_ignore_ascii_case(identifier))?;
    families.iter().enumerate().find_map(|(f, family)| {
        let member = family.members.iter().position(|m| m.index == index)?;
        Some(Tick::Font { family: f, member })
    })
}

/// `Font_DecodeMenu`'s flag for an encodings menu.
const ENCODING_MENU: u32 = 1;
/// The most items a menu read back has.
const ITEMS_MOST: usize = 1024;
/// The longest item text read back.
const TEXT_MOST: usize = 256;

/// `Font_DecodeMenu`: R2's selections from R1's menu, as an identifier into
/// R3's buffer of R4 bytes, or its size alone where R3 is nought. A choice
/// of nothing gives the last answer again, as RISC OS 5.30's does.
pub(crate) fn decode_menu(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[0] & !ENCODING_MENU != 0 {
        return Err(os.error(BAD_PARAMETERS));
    }
    let encodings = regs.r[0] & ENCODING_MENU != 0;
    let first = os.read_word(regs.r[2])?;
    let (answer, consumed) = if first.cast_signed() < 0 {
        (with(os, |state| state.decoded.clone())?, 0)
    } else {
        let chosen = chosen(os, regs.r[1], regs.r[2], first)?;
        let regular = message(os, "Regular")?;
        let territory = scan::territory(os)?;
        let answer = scan::with_catalogue(os, false, |os, catalogue| {
            answer(os, catalogue, &chosen, &regular, encodings, territory)
        })?;
        (answer, chosen.consumed)
    };
    let needed = len32(answer.len() + usize::from(!answer.is_empty()));
    if regs.r[3] != 0 {
        if needed > regs.r[4] {
            return Err(os.error(BUFFER_OVERFLOW));
        }
        os.write_bytes(regs.r[3], &answer)?;
        if !answer.is_empty() {
            os.write_byte(regs.r[3].wrapping_add(needed - 1), ZERO)?;
        }
        regs.r[2] = if answer.windows(2).any(|w| w == [FIELD, letter::FONT_NAME]) {
            0
        } else {
            regs.r[2].wrapping_add(4 * consumed)
        };
    }
    regs.r[4] = needed;
    with(os, |state| state.decoded = answer)
}

/// The texts a selection chose: its family's, its member's if it went into
/// the submenu, and how many words of the selection it used.
struct Chosen {
    family: Vec<u8>,
    member: Option<Vec<u8>>,
    consumed: u32,
}

/// The texts the selection at `selection`, whose first word is `first`,
/// chose from the menu at `menu`.
fn chosen(os: &mut Os<'_>, menu: u32, selection: u32, first: u32) -> Result<Chosen, Fail> {
    let entry = item_at(os, menu, first)?;
    let family = item_text(os, entry)?;
    let second = os.read_word(selection.wrapping_add(4))?;
    if second.cast_signed() < 0 {
        return Ok(Chosen {
            family,
            member: None,
            consumed: 1,
        });
    }
    let submenu = os.read_word(entry.wrapping_add(item::SUBMENU))?;
    if submenu == NONE || submenu == 0 {
        return Err(not_found(os, &family));
    }
    let member_item = item_at(os, submenu, second)?;
    let member = item_text(os, member_item)?;
    Ok(Chosen {
        family,
        member: Some(member),
        consumed: 2,
    })
}

/// The address of item `index` of the menu at `menu`; "font not found" past
/// its last.
fn item_at(os: &mut Os<'_>, menu: u32, index: u32) -> Result<u32, Fail> {
    let first = menu.wrapping_add(header::LEN);
    for i in 0..ITEMS_MOST {
        let entry = first.wrapping_add(item::LEN * len32(i));
        if len32(i) == index {
            return Ok(entry);
        }
        if os.read_word(entry)? & item::LAST != 0 {
            break;
        }
    }
    Err(not_found(os, &[]))
}

/// An item's text: indirected, or in the item.
fn item_text(os: &mut Os<'_>, entry: u32) -> Result<Vec<u8>, Fail> {
    let icon_flags = os.read_word(entry.wrapping_add(item::ICON_FLAGS))?;
    let text = entry.wrapping_add(item::TEXT);
    if icon_flags & item::INDIRECTED != 0 {
        let at = os.read_word(text)?;
        Ok(os.read_until(at, TEXT_MOST, |b| b < b' ')?.0)
    } else {
        Ok(os
            .read_until(text, item::TEXT_ROOM as usize, |b| b < b' ')?
            .0)
    }
}

/// "Font not found", naming `name` to its first space as an identifier.
fn not_found(os: &mut Os<'_>, name: &[u8]) -> Fail {
    let name = &name[..name.iter().position(|&b| b == b' ').unwrap_or(name.len())];
    raise(os, error::NOT_FOUND, &[name])
}

/// What `chosen` names, as an identifier: `\F` and the font, with its name
/// in the territory where its Messages file gives one, or `\E` and the
/// encoding.
fn answer(
    os: &mut Os<'_>,
    catalogue: &Catalogue,
    chosen: &Chosen,
    regular: &[u8],
    encodings: bool,
    territory: u32,
) -> Result<Vec<u8>, Fail> {
    let mut wanted = chosen.family.clone();
    if let Some(member) = chosen.member.as_deref().filter(|&m| m != regular) {
        wanted.push(b'.');
        wanted.extend_from_slice(member);
    }
    let (marker, identifier, name) = if encodings {
        let Some(encoding) = catalogue
            .listed_encodings()
            .find(|e| e.shown().eq_ignore_ascii_case(&wanted))
        else {
            let key = crate::identifier::encoding_key(&wanted);
            return Err(raise(os, error::NO_ENCODING, &[&key]));
        };
        (
            letter::ENCODING,
            &encoding.identifier,
            encoding.name.as_ref(),
        )
    } else {
        let font = if chosen.member.is_some() {
            catalogue
                .fonts
                .iter()
                .find(|f| f.shown().eq_ignore_ascii_case(&wanted))
        } else {
            default_font(catalogue, &wanted)
        };
        let Some(font) = font else {
            return Err(not_found(os, &wanted));
        };
        (letter::FONT, &font.identifier, font.name.as_ref())
    };
    let mut out = alloc::vec![FIELD, marker];
    out.extend_from_slice(identifier);
    if let Some(name) = name {
        out.extend_from_slice(&[FIELD, marker.to_ascii_lowercase()]);
        out.extend_from_slice(
            tairix_riscos_kernel::Decimal::of(territory.cast_signed()).as_bytes(),
        );
        out.push(b' ');
        out.extend_from_slice(name);
    }
    Ok(out)
}

/// The font a choice of the family `family` alone means.
fn default_font<'a>(catalogue: &'a Catalogue, family: &[u8]) -> Option<&'a crate::catalogue::Font> {
    let names = shown(catalogue, false);
    let families = families(names.iter().copied());
    let found = families
        .iter()
        .find(|f| f.name.eq_ignore_ascii_case(family))?;
    let member = found.chosen(|index| catalogue.fonts[index].default)?;
    catalogue.fonts.get(member.index)
}
