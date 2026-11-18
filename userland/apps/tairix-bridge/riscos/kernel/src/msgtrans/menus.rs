//! `MessageTrans_MakeMenus`: Wimp menus built from a definition of tokens —
//! each menu's title, colours, height and gap, then its items, each with its
//! flags, its submenu's offset and its icon's flags — into the caller's
//! buffer, the texts looked up as they go.
//!
//! The room it reports left is RISC OS 5's — the caller's size less each
//! menu's header, never its items — and an overflow leaves R2 at the end of
//! what was built and R3 at that room, as RISC OS 5's does; but every write
//! stays within the buffer, refused with an overflow. A title is looked up
//! into all the room left, then partly overwritten by the header's words.

use super::{Found, Parameters, BUFFER_OVERFLOW};
use crate::os::{len32, Fail, Os, Regs};

/// A menu's header and an item.
const HEADER: u32 = 28;
const ITEM: u32 = 24;
/// The item flag that ends a menu, and the one that makes an item writable.
const LAST: u32 = 1 << 7;
const WRITABLE: u32 = 1 << 2;
/// The icon flag that indirects an icon's text.
const INDIRECTED: u32 = 1 << 8;
/// An icon's text when it is not indirected: eleven characters and a zero.
const DIRECT_TEXT: u32 = 12;

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
        if room < HEADER {
            (regs.r[2], regs.r[3]) = (out, reported);
            return Err(os.message_trans_global_error(BUFFER_OVERFLOW, b""));
        }
        let menu = out;
        let title_end = os.definition_token(definition)?;
        let title = os.menu_text(descriptor, definition, menu, room)?;
        let mut look = [0; 6];
        os.read_bytes(title_end.wrapping_add(1), &mut look)?;
        os.write_bytes(menu.wrapping_add(12), &look[..4])?;
        os.write_word(menu.wrapping_add(20), u32::from(look[4]))?;
        os.write_word(menu.wrapping_add(24), u32::from(look[5]))?;
        room -= HEADER;
        reported -= HEADER;
        let mut widest = title.saturating_sub(3);
        let mut item = title_end.wrapping_add(7);
        let mut at = menu.wrapping_add(HEADER);
        loop {
            os.yield_point()?;
            if room < ITEM {
                (regs.r[2], regs.r[3]) = (at, reported);
                return Err(os.message_trans_global_error(BUFFER_OVERFLOW, b""));
            }
            room -= ITEM;
            let token_end = os.definition_token(item)?;
            let words = token_end.wrapping_add(1).next_multiple_of(4);
            let item_flags = os.read_word(words)?;
            let submenu = os.read_word(words.wrapping_add(4))?;
            let icon_flags = os.read_word(words.wrapping_add(8))?;
            os.write_word(at, item_flags)?;
            os.write_word(
                at.wrapping_add(4),
                if submenu == 0 {
                    0
                } else {
                    menu.wrapping_add(submenu)
                },
            )?;
            os.write_word(at.wrapping_add(8), icon_flags)?;
            let length = if icon_flags & INDIRECTED == 0 {
                os.menu_text(descriptor, item, at.wrapping_add(12), DIRECT_TEXT)?
            } else if item_flags & WRITABLE == 0 {
                let Found { value, length, .. } = os.look_up_token(descriptor, item)?;
                os.write_word(at.wrapping_add(12), value)?;
                os.write_word(at.wrapping_add(16), 0)?;
                os.write_word(at.wrapping_add(20), length.wrapping_add(1))?;
                length
            } else {
                let buffer = os.read_word(at.wrapping_add(12))?;
                let size = os.read_word(at.wrapping_add(20))?;
                os.menu_text(descriptor, item, buffer, size)?
            };
            widest = widest.max(length);
            at = at.wrapping_add(ITEM);
            item = words.wrapping_add(12);
            if item_flags & LAST != 0 {
                break;
            }
        }
        os.write_word(
            menu.wrapping_add(16),
            widest.wrapping_mul(16).wrapping_add(12),
        )?;
        out = at;
        definition = item;
    }
    regs.r[2] = out;
    regs.r[3] = reported;
    Ok(())
}
