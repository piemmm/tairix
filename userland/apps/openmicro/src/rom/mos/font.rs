//! The character set's definitions, characters 32 to 127: `lib/vdu`'s in the
//! BBC's arrangement, the pound sign at 96 and a hatched block at 127.

use tairix_vdu::font::BBC;

use crate::machine::OS_BASE;
use crate::rom::asm::Asm;

/// Where the font is: page-aligned, so a character's zone is a page, and
/// at the start of the ROM, where software expects to find it.
const FONT: u16 = OS_BASE;

pub fn emit(a: &mut Asm) {
    a.org(FONT);
    a.here("font");
    for glyph in &BBC {
        a.bytes(glyph);
    }
}
