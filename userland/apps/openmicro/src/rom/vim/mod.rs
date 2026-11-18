//! The clean-room VIM ROM: a vi-style text editor for the BBC Micro, a
//! language a sideways slot holds, that edits text files and BASIC
//! programs and colours them as the mode allows (`plans/BEEB-VIM.md`).
//! It calls the MOS only through its documented interfaces, so it runs
//! with the genuine MOS, BASIC and DFS as with this tree's.

mod basic;
mod buffer;
mod dis;
mod edit;
mod entry;
mod ex;
mod files;
mod help;
mod insert;
mod motion;
mod normal;
mod ops;
mod paging;
mod screen;
mod search;
mod subst;
mod syntax;
mod text;
mod ws;

#[cfg(test)]
mod tests;

use crate::machine::{ROM_SIZE, SIDEWAYS_BASE};
use crate::rom::asm::{Asm, AsmError, Image};

/// Build the VIM image.
///
/// # Errors
///
/// The builder's first [`AsmError`].
pub fn build() -> Result<Image, AsmError> {
    let ram = paging::assemble()?;
    let mut a = Asm::new(SIDEWAYS_BASE, ROM_SIZE);
    entry::emit(&mut a);
    normal::emit(&mut a);
    motion::emit(&mut a);
    edit::emit(&mut a);
    insert::emit(&mut a);
    search::emit(&mut a);
    subst::emit(&mut a);
    ex::emit(&mut a);
    files::emit(&mut a);
    help::emit(&mut a);
    dis::emit(&mut a);
    basic::emit(&mut a);
    buffer::emit(&mut a, &ram);
    paging::emit(&mut a, &ram);
    screen::emit(&mut a);
    syntax::emit(&mut a);
    text::emit(&mut a);
    a.place_later();
    a.finish()
}
