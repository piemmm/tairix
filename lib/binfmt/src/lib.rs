//! TAIRiX read-only executable-container decoder (`lib/binfmt`).
//!
//! Several TAIRiX components need to *look inside* an executable file
//! without loading it: the file manager's disassembly viewer and the text
//! editor's Code display. That decoding is identical wherever it happens, so
//! it lives here once. The crate produces typed, borrowed views of three
//! containers:
//!
//! - **`rxe`** ([`rxe::RxeView`]) — the TAIRiX load image, decoded
//!   *through* the `lib/abi` wire type (`LoadImage::parse_for_inspection`),
//!   so the inspection view and the kernel load path share one definition
//!   and cannot drift.
//! - **ELF64** ([`elf::ElfView`]) — little-endian `EM_X86_64` /
//!   `EM_AARCH64` / `EM_RISCV` files: the file header, program headers,
//!   section headers, section-name strings, and symbol tables — enough to
//!   name and bound the code regions a disassembler walks.
//! - **wasm** ([`wasm::WasmView`]) — module structure: the section
//!   directory and the type/function/code section framing (function-body
//!   boundaries), with strictly validated LEB128 lengths.
//!
//! [`report`] states what any file [`detect`] recognises says — those three,
//! a driver bundle's image, and a program's or a bundle's signed manifest,
//! all read through the `lib/abi` decoders — as a person reads it.
//!
//! # Untrusted input, read-only, fail closed
//!
//! Every input is untrusted (any file a user points a viewer at). The
//! decoders never execute, map, or load anything; every offset and length
//! is bounds-checked against the input slice before use, every table
//! carries a fixed validation cap, and a malformed input is a typed error
//! naming what failed — never a panic and never a partial trust of later
//! bytes. Consumers run these decoders inside the minimum-capability
//! parser sandbox; the functions here are pure, total slice decoders
//! precisely so they link into that sandbox process unchanged.
//!
//! # Example
//!
//! ```
//! use tairix_binfmt::{detect, Format};
//!
//! assert_eq!(detect(b"\x7fELF\x02\x01\x01\x00"), Some(Format::Elf64));
//! assert_eq!(detect(b"\0asm\x01\x00\x00\x00"), Some(Format::Wasm));
//! assert_eq!(detect(b"plain text"), None);
//! ```

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod elf;
pub mod report;
pub mod rxe;
pub mod wasm;

/// A file format this crate can decode.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Format {
    /// A TAIRiX `rxe` load image (`RXEL` magic).
    Rxe,
    /// A driver bundle's image (`DRV1` magic): its signed manifest, then the
    /// `rxe` program it runs.
    Driver,
    /// A 64-bit little-endian ELF file (`\x7fELF`, `ELFCLASS64`,
    /// `ELFDATA2LSB`).
    Elf64,
    /// A WebAssembly module (`\0asm`).
    Wasm,
    /// A program's signed manifest (`RXM1` magic).
    Manifest,
    /// A bundle's signed `AppInfo` manifest (`RAI1` magic).
    AppInfo,
}

impl Format {
    /// Whether files of this format carry code to disassemble.
    #[must_use]
    pub const fn holds_code(self) -> bool {
        match self {
            Self::Rxe | Self::Driver | Self::Elf64 | Self::Wasm => true,
            Self::Manifest | Self::AppInfo => false,
        }
    }
}

/// Recognise the container format of `bytes` from its magic prefix.
///
/// Detection is deliberately shallow — it answers "which decoder should
/// look at this file?", not "is this file valid?". The matching decoder
/// still validates everything and fails closed on a malformed body. For
/// ELF, only the 64-bit little-endian class this crate decodes is
/// recognised; a 32-bit or big-endian ELF returns `None` so a caller falls
/// back honestly (to a hex view) instead of failing half-way in.
#[must_use]
pub fn detect(bytes: &[u8]) -> Option<Format> {
    let magic = bytes
        .first_chunk::<4>()
        .map(|magic| u32::from_le_bytes(*magic));
    match magic {
        Some(tairix_abi::LOAD_MAGIC) => return Some(Format::Rxe),
        Some(tairix_abi::DRIVER_MANIFEST_MAGIC) => return Some(Format::Driver),
        Some(tairix_abi::MANIFEST_MAGIC) => return Some(Format::Manifest),
        Some(tairix_abi::APPINFO_MAGIC) => return Some(Format::AppInfo),
        _ => {}
    }
    if bytes.len() >= 6
        && bytes[0..4] == *b"\x7fELF"
        && bytes[4] == elf::ELF_CLASS_64
        && bytes[5] == elf::ELF_DATA_LSB
    {
        return Some(Format::Elf64);
    }
    if bytes.len() >= 4 && bytes[0..4] == *b"\0asm" {
        return Some(Format::Wasm);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{detect, Format};

    #[test]
    fn detects_each_magic() {
        assert_eq!(detect(b"RXEL\x01\x00\x00\x00"), Some(Format::Rxe));
        assert_eq!(detect(b"DRV1\x01\x00\x00\x00"), Some(Format::Driver));
        assert_eq!(detect(b"RXM1\x01\x00\x00\x00"), Some(Format::Manifest));
        assert_eq!(detect(b"RAI1\x01\x00\x00\x00"), Some(Format::AppInfo));
        assert_eq!(detect(b"\x7fELF\x02\x01\x01\x00"), Some(Format::Elf64));
        assert_eq!(detect(b"\0asm\x01\x00\x00\x00"), Some(Format::Wasm));
    }

    #[test]
    fn only_a_container_of_code_holds_code() {
        let code = [Format::Rxe, Format::Driver, Format::Elf64, Format::Wasm];
        assert!(code.into_iter().all(Format::holds_code));
        assert!(!Format::Manifest.holds_code());
        assert!(!Format::AppInfo.holds_code());
    }

    #[test]
    fn rejects_non_executables_and_short_prefixes() {
        assert_eq!(detect(b""), None);
        assert_eq!(detect(b"RXE"), None);
        assert_eq!(detect(b"\x7fEL"), None);
        assert_eq!(detect(b"plain text file"), None);
        assert_eq!(detect(&[0u8; 16]), None);
    }

    #[test]
    fn rejects_elf_classes_this_crate_does_not_decode() {
        // 32-bit ELF.
        assert_eq!(detect(b"\x7fELF\x01\x01\x01\x00"), None);
        // Big-endian ELF64.
        assert_eq!(detect(b"\x7fELF\x02\x02\x01\x00"), None);
        // Magic alone, class byte missing.
        assert_eq!(detect(b"\x7fELF"), None);
    }
}
