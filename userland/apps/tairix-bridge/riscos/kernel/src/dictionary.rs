//! The kernel's two dictionaries (`plans/RISCOS-EMULATOR.md` §4.5): the one
//! `OS_PrettyPrint` expands escapes from when given none, and MessageTrans's
//! own, which `MessageTrans_Dictionary` hands out. Both lie in the host pages,
//! where a program may read them but not change them.
//!
//! A dictionary is entries of a length byte counting itself and the text's
//! terminator, the text, then the terminator; a zero length ends it. An
//! entry's text may itself hold escapes: an escape and zero is the special
//! string `OS_PrettyPrint` was given.

use alloc::vec::Vec;

use crate::memory::{Memory, MemoryError};

/// The escape naming a dictionary entry by the byte after it; zero names the
/// special string.
pub(crate) const ESCAPE: u8 = 0x1B;

/// The kernel's dictionary, from token 1: the PRM's table, each entry's
/// expansion matched against genuine RISC OS 5's. Token 22, which the PRM
/// leaves out, is empty there too.
const KERNEL: [&[u8]; 41] = [
    b"Syntax: *\x1b\x00",
    b" the ",
    b"director",
    b"filing system",
    b"current",
    b" to a variable. Other types of value can be assigned with *",
    b"file",
    b"default ",
    b"tion",
    b"*Configure ",
    b"name",
    b" server",
    b"number",
    b"Syntax: *\x1b\x00 <",
    b" one or more files that match the given wildcard",
    b" and ",
    b"relocatable module",
    b"\rC(onfirm)\tPrompt for confirmation of each ",
    b"sets the ",
    b"Syntax: *\x1b\x00 [<disc spec.>]",
    b"}\rV(erbose)\tPrint information on each file ",
    b"",
    b"spriteLandscape [<XScale> [<YScale> [<Margin> [<Threshold>]]]]]",
    b" is used to print a hard copy of the screen on EPSON-",
    b".\rOptions: (use ~ to force off, eg. ~",
    b"printe",
    b"Syntax: *\x1b\x00 <filename>",
    b"select",
    b"xpression",
    b"Syntax: *\x1b\x00 [",
    b"sprite",
    b" displays",
    b"free space",
    b" {off}",
    b"library",
    b"parameter",
    b"object",
    b" all ",
    b"disc",
    b" to ",
    b" is ",
];

/// Where the dictionaries are.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Dictionaries {
    /// The kernel's.
    pub kernel: u32,
    /// MessageTrans's: the emulator's own, with no entries, since genuine
    /// RISC OS 5's words are known only from its ROM.
    pub messages: u32,
}

/// `entries` laid out as a dictionary.
fn dictionary(entries: &[&[u8]]) -> Result<Vec<u8>, MemoryError> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(entries.iter().map(|text| text.len() + 2).sum::<usize>() + 1)?;
    for text in entries {
        // Every entry is far shorter than a length byte can count.
        let length = u8::try_from(text.len() + 2).map_err(|_| MemoryError::HostFull)?;
        bytes.push(length);
        bytes.extend_from_slice(text);
        bytes.push(0);
    }
    bytes.push(0);
    Ok(bytes)
}

/// Lays both dictionaries into the host pages: where they are.
pub(crate) fn install(memory: &mut Memory) -> Result<Dictionaries, MemoryError> {
    let mut bytes = dictionary(&KERNEL)?;
    let messages = u32::try_from(bytes.len()).map_err(|_| MemoryError::HostFull)?;
    bytes.extend_from_slice(&dictionary(&[])?);
    let mut words = Vec::new();
    words.try_reserve_exact(bytes.len().div_ceil(4))?;
    words.extend(bytes.chunks(4).map(|chunk| {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        u32::from_le_bytes(word)
    }));
    let base = memory.install_host(&words)?;
    Ok(Dictionaries {
        kernel: base,
        messages: base + messages,
    })
}

#[cfg(test)]
mod tests;
