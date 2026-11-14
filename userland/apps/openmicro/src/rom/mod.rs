//! The ROMs the emulator ships, written in Rust: the 6502 builder and the
//! clean-room ROMs built with it. Host-only: the freestanding binary loads
//! the committed images and never builds them.

pub mod asm;
pub mod basic;
pub mod dfs;
#[cfg(test)]
mod harness;
pub mod mos;
pub mod os;
pub mod plus1;

use asm::{abs, lo, Asm, AsmError, Image};

/// What makes one ROM image.
pub type Build = fn() -> Result<Image, AsmError>;

/// A service ROM's header: no language entry, a jump to its `service`
/// routine, its type, copyright offset and version, then its title, version
/// string and the copyright.
pub(crate) fn service_header(a: &mut Asm, version: u8, title: &str, version_text: &str) {
    a.bytes(&[0, 0, 0]);
    a.jmp(abs("service"));
    a.byte(os::rom_type::SERVICE | os::rom_type::CODE);
    a.byte(lo("copyright"));
    a.byte(version);
    a.text(title);
    a.byte(0);
    a.text(version_text);
    a.here("copyright");
    a.byte(0);
    a.text(os::COPYRIGHT);
    a.byte(0);
}

/// Every image the bundle ships, by its name in `Resources/`, with the
/// builder that makes it from this tree.
pub const SHIPPED: [(&str, Build); 5] = [
    (crate::firmware::MOS, mos::model_b),
    (crate::firmware::ELECTRON_OS, mos::electron),
    (crate::firmware::BASIC, basic::build),
    (crate::firmware::DFS, dfs::build),
    (crate::firmware::PLUS1, plus1::build),
];
