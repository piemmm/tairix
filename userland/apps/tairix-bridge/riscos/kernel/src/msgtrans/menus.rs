//! The layout of a Wimp menu, which every module that builds or reads one
//! shares; and `MessageTrans_MakeMenus`: menus built from a definition of
//! tokens — each menu's title, colours, height and gap, then its items, each
//! with its flags, its submenu's offset and its icon's flags — into the
//! caller's buffer, the texts looked up as they go.
//!
//! The room it reports left is RISC OS 5's — the caller's size less each
//! menu's header, never its items — and an overflow leaves R2 at the end of
//! what was built and R3 at that room, as RISC OS 5's does; but every write
//! stays within the buffer, refused with an overflow. A title is looked up
//! into all the room left, then partly overwritten by the header's words.

use super::{Found, Parameters, BUFFER_OVERFLOW};
use crate::os::{len32, Fail, Os, Regs};

/// A menu's header: each field's offset, and its length.
pub mod header {
    /// The title: its text, or where the first item has
    /// [`super::item::INDIRECTED_TITLE`], the text's address.
    pub const TITLE: u32 = 0;
    /// The room the title's text has.
    pub const TITLE_ROOM: u32 = 12;
    /// The colours, a byte each: the title's foreground and background, then
    /// the work area's.
    pub const COLOURS: u32 = TITLE + TITLE_ROOM;
    /// The items' width, in OS units.
    pub const WIDTH: u32 = 16;
    /// Each item's height.
    pub const HEIGHT: u32 = 20;
    /// The gap between the items.
    pub const GAP: u32 = 24;
    /// The header's length: where the first item is.
    pub const LEN: u32 = 28;
}

/// A menu's item: each field's offset, its length, and its flags.
pub mod item {
    /// Its flags.
    pub const FLAGS: u32 = 0;
    /// Its submenu.
    pub const SUBMENU: u32 = 4;
    /// Its icon's flags.
    pub const ICON_FLAGS: u32 = 8;
    /// Its text, or with [`INDIRECTED`] the text's address.
    pub const TEXT: u32 = 12;
    /// An indirected text's validation string.
    pub const VALIDATION: u32 = 16;
    /// An indirected text's buffer size.
    pub const BUFFER_SIZE: u32 = 20;
    /// The room a text that is not indirected has.
    pub const TEXT_ROOM: u32 = 12;
    /// The item's length.
    pub const LEN: u32 = 24;

    /// The flag that ticks it.
    pub const TICKED: u32 = 1;
    /// The flag that makes its text writable.
    pub const WRITABLE: u32 = 1 << 2;
    /// The flag that ends its menu.
    pub const LAST: u32 = 1 << 7;
    /// The flag, on a menu's first item, that indirects the menu's title.
    pub const INDIRECTED_TITLE: u32 = 1 << 8;
    /// The icon flag that indirects its text.
    pub const INDIRECTED: u32 = 1 << 8;
}

/// The width of a menu whose longest text is `characters` long, in OS units.
#[must_use]
pub const fn width(characters: u32) -> u32 {
    characters.wrapping_mul(16).wrapping_add(12)
}

/// Whether `byte` ends a token in a definition.
const fn ends_token(byte: u8) -> bool {
    byte <= b' ' || byte == b',' || byte == b')'
}

impl Os<'_> {
    /// A definition's token at `at`, and where its terminator is.
    fn definition_token(&mut self, at: u32) -> Result<u32, Fail> {
        let (token, _) = self.read_until(at, crate::error::TEXT, ends_token)?;
        Ok(at.wrapping_add(len32(token.len())))
    }

    /// The token at `token` looked up and copied into `out`, at most `size`
    /// bytes with its terminator: its length.
    fn menu_text(&mut self, descriptor: u32, token: u32, out: u32, size: u32) -> Result<u32, Fail> {
        let found = self.look_up_token(descriptor, token)?;
        let none: Parameters = [None, None, None, None];
        self.expand_into(found, &none, out, size)
    }
}

pub(super) fn make_menus(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let descriptor = regs.r[0];
    let mut definition = regs.r[1];
    let mut out = regs.r[2];
    let mut room = regs.r[3];
    let mut reported = regs.r[3];
    loop {
        os.yield_point()?;
        if os.read_byte(definition)? == 0 {
            break;
        }
        if room < header::LEN {
            (regs.r[2], regs.r[3]) = (out, reported);
            return Err(os.message_trans_global_error(BUFFER_OVERFLOW, b""));
        }
        let menu = out;
        let title_end = os.definition_token(definition)?;
        let title = os.menu_text(descriptor, definition, menu, room)?;
        let mut look = [0; 6];
        os.read_bytes(title_end.wrapping_add(1), &mut look)?;
        os.write_bytes(menu.wrapping_add(header::COLOURS), &look[..4])?;
        os.write_word(menu.wrapping_add(header::HEIGHT), u32::from(look[4]))?;
        os.write_word(menu.wrapping_add(header::GAP), u32::from(look[5]))?;
        room -= header::LEN;
        reported -= header::LEN;
        let mut widest = title.saturating_sub(3);
        let mut token = title_end.wrapping_add(7);
        let mut at = menu.wrapping_add(header::LEN);
        loop {
            os.yield_point()?;
            if room < item::LEN {
                (regs.r[2], regs.r[3]) = (at, reported);
                return Err(os.message_trans_global_error(BUFFER_OVERFLOW, b""));
            }
            room -= item::LEN;
            let token_end = os.definition_token(token)?;
            let words = token_end.wrapping_add(1).next_multiple_of(4);
            let item_flags = os.read_word(words)?;
            let submenu = os.read_word(words.wrapping_add(4))?;
            let icon_flags = os.read_word(words.wrapping_add(8))?;
            os.write_word(at.wrapping_add(item::FLAGS), item_flags)?;
            os.write_word(
                at.wrapping_add(item::SUBMENU),
                if submenu == 0 {
                    0
                } else {
                    menu.wrapping_add(submenu)
                },
            )?;
            os.write_word(at.wrapping_add(item::ICON_FLAGS), icon_flags)?;
            let text = at.wrapping_add(item::TEXT);
            let length = if icon_flags & item::INDIRECTED == 0 {
                os.menu_text(descriptor, token, text, item::TEXT_ROOM)?
            } else if item_flags & item::WRITABLE == 0 {
                let Found { value, length, .. } = os.look_up_token(descriptor, token)?;
                os.write_word(text, value)?;
                os.write_word(at.wrapping_add(item::VALIDATION), 0)?;
                os.write_word(at.wrapping_add(item::BUFFER_SIZE), length.wrapping_add(1))?;
                length
            } else {
                let buffer = os.read_word(text)?;
                let size = os.read_word(at.wrapping_add(item::BUFFER_SIZE))?;
                os.menu_text(descriptor, token, buffer, size)?
            };
            widest = widest.max(length);
            at = at.wrapping_add(item::LEN);
            token = words.wrapping_add(12);
            if item_flags & item::LAST != 0 {
                break;
            }
        }
        os.write_word(menu.wrapping_add(header::WIDTH), width(widest))?;
        out = at;
        definition = token;
    }
    regs.r[2] = out;
    regs.r[3] = reported;
    Ok(())
}
