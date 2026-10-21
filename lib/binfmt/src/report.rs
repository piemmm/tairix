//! What an executable or a signed manifest says, as a person reads it: each
//! part of the file at the offset and length it occupies, with what it
//! declares beneath it.
//!
//! The file is untrusted, so the report is built where the decoders run — a
//! parser sandbox — and held to the shared report's bounds
//! (`tairix_util::report`). A part that will not read ends the walk with where
//! and why. Keys and signatures are shown, never checked: admitting a program
//! is the load gate's judgement, not a viewer's.

use alloc::string::{String, ToString};
use core::fmt::{self, Write as _};

use tairix_abi::{
    browse_type_at, decode_bind_keys, decode_capability_ids, mime_type_at, AppInfoHeader,
    CapabilityId, DriverBindKey, DriverError, DriverImage, DriverKind, DriverManifest, HwMatchKey,
    HwMatchKind, LoadHeader, ManifestHeader, NeededLibrary, ProgramKind, PublisherBinding,
    RxeError, RxePermission, Segment, APPINFO_MAX_CAPABILITIES, BROWSE_ENTRY_LEN,
    DRIVER_MANIFEST_MAX_BIND_KEYS, DRIVER_MANIFEST_MAX_CAPABILITIES, LOAD_FLAG_PIE, LOAD_MAGIC,
    MANIFEST_MAX_CAPABILITIES, MIME_ENTRY_LEN,
};
use tairix_util::report::{clean, digest, Builder, Report};

use crate::elf::{
    ElfView, ProgramHeader, Section, ELF_HEADER_LEN, PROGRAM_HEADER_LEN, SECTION_HEADER_LEN,
    SHT_DYNSYM, SHT_SYMTAB,
};
use crate::rxe::RxeView;
use crate::wasm::{self, WasmView};
use crate::{detect, Format};

/// The most capability ids any manifest may request.
const CAPABILITY_ROOM: usize = max(
    max(MANIFEST_MAX_CAPABILITIES, APPINFO_MAX_CAPABILITIES),
    DRIVER_MANIFEST_MAX_CAPABILITIES,
) as usize;

const fn max(a: u16, b: u16) -> u16 {
    if a > b {
        a
    } else {
        b
    }
}

/// The report of `bytes`, or `None` when [`detect`] recognises no format.
#[must_use]
pub fn report(bytes: &[u8]) -> Option<Report> {
    let format = detect(bytes)?;
    let mut out = Builder::default();
    match format {
        Format::Rxe => program(bytes, 0, &mut out),
        Format::Driver => driver(bytes, &mut out),
        Format::Elf64 => elf(bytes, &mut out),
        Format::Wasm => module(bytes, &mut out),
        Format::Manifest => manifest(bytes, &mut out),
        Format::AppInfo => appinfo(bytes, &mut out),
    }
    Some(out.finish())
}

/// An `rxe` image found at `base` in the file.
fn program(image: &[u8], base: usize, out: &mut Builder) {
    let header = LoadHeader::from_bytes(image).and_then(|header| {
        if header.magic == LOAD_MAGIC {
            Ok(header)
        } else {
            Err(RxeError::BadMagic)
        }
    });
    let header = match header {
        Ok(header) => header,
        Err(error) => {
            out.heading(format_args!("TAIRiX program (rxe)"));
            return out.stop(base, &error.to_string());
        }
    };
    let pie = if header.flags & LOAD_FLAG_PIE == 0 {
        "fixed address"
    } else {
        "position-independent"
    };
    out.heading(format_args!(
        "TAIRiX program (rxe), ABI {}, {pie}",
        header.abi_version
    ));
    out.record(
        base,
        "header",
        LoadHeader::WIRE_LEN,
        format_args!("entry {:#x}", header.entry),
    );
    out.fact(0, format_args!("CFI tag {}", digest(&header.cfi_tag)));
    let view = match RxeView::parse(image) {
        Ok(view) => view,
        Err(error) => return out.stop(base + LoadHeader::WIRE_LEN, &error.to_string()),
    };
    let table = base + LoadHeader::WIRE_LEN;
    let segments = view.segments();
    out.record(
        table,
        "segments",
        segments.len() * Segment::WIRE_LEN,
        format_args!("{}", counted(segments.len(), "segment", "segments")),
    );
    for segment in segments {
        out.fact(
            0,
            format_args!(
                "{} at {:#x}: {:#x} file bytes at {:#x}, {:#x} in memory",
                permission(segment.permission),
                segment.vaddr,
                segment.file_size,
                segment.file_offset,
                segment.mem_size
            ),
        );
    }
    let needed = view.needed_libraries().count();
    if needed > 0 {
        out.record(
            table + segments.len() * Segment::WIRE_LEN,
            "libraries",
            needed * NeededLibrary::WIRE_LEN,
            format_args!("{needed} needed"),
        );
        for library in view.needed_libraries() {
            out.fact(0, format_args!("{}", clean(library.as_bytes())));
        }
    }
    for (index, segment) in segments.iter().enumerate() {
        let at = usize::try_from(segment.file_offset).unwrap_or(usize::MAX);
        let len = usize::try_from(segment.file_size).unwrap_or(usize::MAX);
        out.record(
            base.saturating_add(at),
            "segment",
            len,
            format_args!("{index}, {}", contents(segment.permission)),
        );
    }
}

/// A driver bundle's image: its manifest, then the program it runs.
fn driver(bytes: &[u8], out: &mut Builder) {
    let image = match DriverImage::parse(bytes) {
        Ok(image) => image,
        Err(error) => {
            out.heading(format_args!("TAIRiX driver"));
            return out.stop(0, driver_refusal(error));
        }
    };
    let manifest = image.manifest;
    let runs = match manifest.kind {
        DriverKind::InKernel => "runs in the kernel",
        _ => "runs in user space",
    };
    out.heading(format_args!(
        "TAIRiX driver, ABI {}, {runs}",
        manifest.abi_version
    ));
    out.record(
        0,
        "manifest",
        DriverManifest::WIRE_LEN,
        format_args!("signed driver manifest"),
    );
    signing(
        out,
        &manifest.syscall_table_hash,
        &manifest.signer_pubkey,
        &manifest.signature,
    );
    let at = DriverManifest::WIRE_LEN;
    if !capabilities(out, at, image.capability_body) {
        return;
    }
    let at = at + image.capability_body.len();
    let count = usize::from(manifest.bind_key_count);
    out.record(
        at,
        "bind table",
        image.bind_table.len(),
        format_args!("{}", counted(count, "key", "keys")),
    );
    let mut keys =
        [DriverBindKey::new(0, HwMatchKey::virtio(0)); DRIVER_MANIFEST_MAX_BIND_KEYS as usize];
    match decode_bind_keys(image.bind_table, count, &mut keys) {
        Ok(decoded) => {
            for key in &keys[..decoded] {
                out.fact(
                    0,
                    format_args!("priority {}: {}", key.priority, MatchKey(&key.key)),
                );
            }
        }
        Err(error) => return out.stop(at, driver_refusal(error)),
    }
    let payload = image.payload_offset();
    out.record(
        payload,
        "program",
        image.payload.len(),
        format_args!("the code the driver runs"),
    );
    program(image.payload, payload, out);
}

/// A program's signed manifest.
fn manifest(bytes: &[u8], out: &mut Builder) {
    let header = match ManifestHeader::from_bytes(bytes) {
        Ok(header) => header,
        Err(error) => {
            out.heading(format_args!("TAIRiX program manifest"));
            return out.stop(0, &error.to_string());
        }
    };
    out.heading(format_args!(
        "TAIRiX program manifest, ABI {}",
        header.abi_version
    ));
    out.record(
        0,
        "header",
        ManifestHeader::WIRE_LEN,
        format_args!("flags {:#x}", header.flags),
    );
    signing(
        out,
        &header.syscall_table_hash,
        &header.signer_pubkey,
        &header.signature,
    );
    let body = bytes.get(ManifestHeader::WIRE_LEN..).unwrap_or_default();
    let len = usize::from(header.capability_count) * 2;
    let Some(ids) = body.get(..len) else {
        return out.stop(
            ManifestHeader::WIRE_LEN,
            "the file ends before the capabilities it lists",
        );
    };
    capabilities(out, ManifestHeader::WIRE_LEN, ids);
}

/// A bundle's `AppInfo`.
fn appinfo(bytes: &[u8], out: &mut Builder) {
    let header = match AppInfoHeader::from_bytes(bytes) {
        Ok(header) => header,
        Err(error) => {
            out.heading(format_args!("Bundle manifest (AppInfo)"));
            return out.stop(0, &error.to_string());
        }
    };
    let kind = header.program_kind().map_or("program", ProgramKind::as_str);
    out.heading(format_args!(
        "Bundle manifest (AppInfo) of {} {}, ABI {}",
        article(kind),
        kind,
        header.abi_version
    ));
    out.record(
        0,
        "header",
        AppInfoHeader::WIRE_LEN,
        format_args!("{}", clean(header.bundle_id().as_bytes())),
    );
    identity(out, &header);
    let binding = match header.publisher_binding() {
        Ok(PublisherBinding::SelfPublished) => "signs its own builds",
        Ok(PublisherBinding::Delegated) => "certifies the signer",
        Err(_) => "names no valid binding to the signer",
    };
    out.fact(
        0,
        format_args!("publisher {} {binding}", digest(&header.publisher_pubkey)),
    );
    out.fact(0, format_args!("content {}", digest(&header.content_hash)));
    signing(
        out,
        &header.syscall_table_hash,
        &header.signer_pubkey,
        &header.signature,
    );
    let body = bytes.get(AppInfoHeader::WIRE_LEN..).unwrap_or_default();
    let Some(body) = header.body_len().ok().and_then(|len| body.get(..len)) else {
        return out.stop(
            AppInfoHeader::WIRE_LEN,
            "the file ends before the capabilities, file types and services its header counts",
        );
    };
    let caps = usize::from(header.capability_count);
    if !capabilities(out, AppInfoHeader::WIRE_LEN, &body[..caps * 2]) {
        return;
    }
    let mimes = usize::from(header.mime_count);
    let at = AppInfoHeader::WIRE_LEN + caps * 2;
    if mimes > 0 {
        out.record(
            at,
            "file types",
            mimes * MIME_ENTRY_LEN,
            format_args!("{mimes} claimed"),
        );
        for index in 0..mimes {
            match mime_type_at(body, caps, index) {
                Ok(mime) => out.fact(0, format_args!("{}", clean(mime.as_bytes()))),
                Err(error) => {
                    return out.stop(at + index * MIME_ENTRY_LEN, &error.to_string());
                }
            };
        }
    }
    let browses = usize::from(header.browse_count);
    let at = at + mimes * MIME_ENTRY_LEN;
    if browses > 0 {
        out.record(
            at,
            "browses",
            browses * BROWSE_ENTRY_LEN,
            format_args!("{}", counted(browses, "service type", "service types")),
        );
        for index in 0..browses {
            match browse_type_at(body, caps, mimes, index) {
                Ok(service) => out.fact(
                    0,
                    format_args!("_{}.{}", clean(service.name), service.transport.label()),
                ),
                Err(error) => {
                    return out.stop(at + index * BROWSE_ENTRY_LEN, &error.to_string());
                }
            };
        }
    }
}

/// What a bundle's `AppInfo` says the bundle is.
fn identity(out: &mut Builder, header: &AppInfoHeader) {
    out.fact(
        0,
        format_args!("name {}", clean(header.bundle_name().as_bytes())),
    );
    if header.title_len > 0 {
        out.fact(
            0,
            format_args!("title {}", clean(header.bundle_title().as_bytes())),
        );
    }
    out.fact(
        0,
        format_args!("version {}", clean(header.bundle_version().as_bytes())),
    );
    for (label, text) in [
        ("purpose", header.bundle_purpose()),
        ("author", header.bundle_author()),
        ("icon", header.library_icon()),
    ] {
        if let Some(text) = text {
            out.fact(0, format_args!("{label} {}", clean(text.as_bytes())));
        }
    }
    match header.library_category() {
        Some(category) => out.fact(
            0,
            format_args!("listed in the program library under {}", category.as_str()),
        ),
        None => out.fact(0, format_args!("not listed in the program library")),
    };
    let instances = if header.runs_one_instance() {
        "one instance a user"
    } else {
        "any number of instances"
    };
    let slot = if header.presents_icon_bar_slot() {
        "a slot on the icon bar"
    } else {
        "no slot on the icon bar"
    };
    out.fact(0, format_args!("runs {instances}, with {slot}"));
    if header.writes_documents() {
        out.fact(0, format_args!("may write the documents it opens"));
    }
}

/// The interface hash, signer and signature a signed part carries.
fn signing(out: &mut Builder, interface: &[u8], signer: &[u8], signature: &[u8]) {
    out.fact(0, format_args!("syscall table {}", digest(interface)));
    out.fact(0, format_args!("signer {}", digest(signer)));
    out.fact(
        0,
        format_args!("signature {} (not checked here)", digest(signature)),
    );
}

/// The capability list at `at`, answering whether it read.
fn capabilities(out: &mut Builder, at: usize, body: &[u8]) -> bool {
    let count = body.len() / 2;
    out.record(
        at,
        "capabilities",
        body.len(),
        format_args!("{count} requested"),
    );
    let mut ids = [CapabilityId::FS_MOUNT; CAPABILITY_ROOM];
    match decode_capability_ids(body, count, &mut ids) {
        Ok(decoded) => {
            for id in &ids[..decoded] {
                match id.name() {
                    Some(name) => out.fact(0, format_args!("{name}")),
                    None => out.fact(0, format_args!("capability {}", id.as_u16())),
                };
            }
            true
        }
        Err(error) => {
            out.stop(at, &error.to_string());
            false
        }
    }
}

/// An ELF64 file.
fn elf(bytes: &[u8], out: &mut Builder) {
    let view = match ElfView::parse(bytes) {
        Ok(view) => view,
        Err(error) => {
            out.heading(format_args!("ELF64 file"));
            return out.stop(0, &error.to_string());
        }
    };
    let header = *view.header();
    out.heading(format_args!(
        "ELF64 {} for {}",
        elf_type(header.e_type),
        header.machine.name()
    ));
    out.record(
        0,
        "header",
        ELF_HEADER_LEN,
        format_args!("entry {:#x}, flags {:#x}", header.entry, header.flags),
    );
    if header.phnum > 0 {
        let at = usize::try_from(header.phoff).unwrap_or(usize::MAX);
        out.record(
            at,
            "segments",
            usize::from(header.phnum) * PROGRAM_HEADER_LEN,
            format_args!(
                "{}",
                counted(usize::from(header.phnum), "segment", "segments")
            ),
        );
        for index in 0..header.phnum {
            if out.full() {
                return;
            }
            match view.program_header(index) {
                Ok(segment) => out.fact(0, format_args!("{}", ElfSegment(&segment))),
                Err(error) => return out.stop(at, &error.to_string()),
            };
        }
    }
    if header.shnum > 0 {
        let at = usize::try_from(header.shoff).unwrap_or(usize::MAX);
        out.record(
            at,
            "sections",
            usize::from(header.shnum) * SECTION_HEADER_LEN,
            format_args!(
                "{}",
                counted(usize::from(header.shnum), "section", "sections")
            ),
        );
        for index in 0..header.shnum {
            if out.full() {
                return;
            }
            let section = match view.section(index) {
                Ok(section) => section,
                Err(error) => return out.stop(at, &error.to_string()),
            };
            let name = view.section_name(&section).unwrap_or_default();
            let symbols = matches!(section.sh_type, SHT_SYMTAB | SHT_DYNSYM)
                .then(|| view.symbol_table(index).map(|table| table.len()).ok())
                .flatten();
            out.fact(
                0,
                format_args!("{}", ElfSection(&clean(name.as_bytes()), &section, symbols)),
            );
        }
    }
}

/// A WebAssembly module.
fn module(bytes: &[u8], out: &mut Builder) {
    let view = match WasmView::parse(bytes) {
        Ok(view) => view,
        Err(error) => {
            out.heading(format_args!("WebAssembly module"));
            return out.stop(0, &error.to_string());
        }
    };
    out.heading(format_args!(
        "WebAssembly module, {}",
        counted(view.sections().len(), "section", "sections")
    ));
    out.record(0, "header", 8, format_args!("binary format version 1"));
    let mut detail = String::new();
    for entry in view.sections() {
        if out.full() {
            return;
        }
        let payload = view.section_bytes(entry);
        detail.clear();
        match entry.id {
            0 => {
                if let Some(name) = wasm::custom_name(payload) {
                    let _ = write!(detail, "named {}", clean(name));
                }
            }
            wasm::SECTION_START => {
                if let Ok(index) = wasm::leading_u32(payload) {
                    let _ = write!(detail, "function {index}");
                }
            }
            _ => {
                if let Ok(count) = wasm::leading_u32(payload) {
                    let count = usize::try_from(count).unwrap_or(usize::MAX);
                    let _ = write!(detail, "{}", counted(count, "entry", "entries"));
                }
            }
        }
        out.record(
            entry.offset,
            wasm::section_name(entry.id),
            entry.size,
            format_args!("{detail}"),
        );
    }
}

/// How a driver image's part was refused, in words.
const fn driver_refusal(error: DriverError) -> &'static str {
    match error {
        DriverError::BufferTooSmall => "the file ends before the parts its manifest lists",
        DriverError::BadMagic => "a magic number or reserved field is wrong",
        DriverError::AbiVersionUnsupported => "it is built for another ABI",
        DriverError::LengthOutOfRange => "a count is past its bound",
        _ => "a field names nothing this system knows",
    }
}

/// How a segment may be used once mapped.
const fn permission(permission: RxePermission) -> &'static str {
    match permission {
        RxePermission::ReadOnly => "r--",
        RxePermission::ReadExecute => "r-x",
        RxePermission::ReadWrite => "rw-",
    }
}

/// What a segment holds, as its permission says.
const fn contents(permission: RxePermission) -> &'static str {
    match permission {
        RxePermission::ReadOnly => "read-only data",
        RxePermission::ReadExecute => "code",
        RxePermission::ReadWrite => "data",
    }
}

/// `count` with its noun.
fn counted(count: usize, one: &str, many: &str) -> String {
    let mut text = count.to_string();
    text.push(' ');
    text.push_str(if count == 1 { one } else { many });
    text
}

const fn article(word: &str) -> &'static str {
    match word.as_bytes().first() {
        Some(b'a' | b'e' | b'i' | b'o' | b'u') => "an",
        _ => "a",
    }
}

/// `e_type` in words.
const fn elf_type(e_type: u16) -> &'static str {
    match e_type {
        1 => "relocatable object",
        2 => "executable",
        3 => "shared object",
        4 => "core dump",
        _ => "file",
    }
}

/// One hardware match key as a bind table names it.
struct MatchKey<'a>(&'a HwMatchKey);

impl fmt::Display for MatchKey<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key = self.0;
        match key.kind() {
            Some(HwMatchKind::Compatible) => {
                write!(f, "compatible \"{}\"", clean(key.compatible_bytes()))
            }
            Some(HwMatchKind::Pci) => write!(
                f,
                "PCI {:04x}:{:04x}, class {:06x}",
                key.vendor(),
                key.product(),
                key.class()
            ),
            Some(HwMatchKind::Usb) => write!(
                f,
                "USB {:04x}:{:04x}, class {:06x}",
                key.vendor(),
                key.product(),
                key.class()
            ),
            Some(HwMatchKind::Virtio) => write!(f, "virtio device {}", key.class()),
            None => f.write_str("a key of no kind this system knows"),
        }
    }
}

/// One ELF program header.
struct ElfSegment<'a>(&'a ProgramHeader);

impl fmt::Display for ElfSegment<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ph = self.0;
        match segment_type(ph.p_type) {
            Some(name) => f.write_str(name)?,
            None => write!(f, "type {:#x}", ph.p_type)?,
        }
        write!(
            f,
            " {}{}{} at {:#x}: {:#x} file bytes at {:#x}, {:#x} in memory",
            if ph.flags & 0b100 != 0 { 'r' } else { '-' },
            if ph.flags & 0b010 != 0 { 'w' } else { '-' },
            if ph.flags & 0b001 != 0 { 'x' } else { '-' },
            ph.vaddr,
            ph.file_size,
            ph.offset,
            ph.mem_size
        )
    }
}

/// One ELF section header, under its name, with the symbols a table holds.
struct ElfSection<'a>(&'a str, &'a Section, Option<usize>);

impl fmt::Display for ElfSection<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(name, section, symbols) = *self;
        let name = if name.is_empty() { "-" } else { name };
        write!(f, "{name} ")?;
        match section_type(section.sh_type) {
            Some(kind) => f.write_str(kind)?,
            None => write!(f, "type {:#x}", section.sh_type)?,
        }
        write!(
            f,
            " {}{}{} at {:#x}: {:#x} bytes at {:#x}",
            if section.flags & 0b010 != 0 { 'a' } else { '-' },
            if section.flags & 0b001 != 0 { 'w' } else { '-' },
            if section.flags & 0b100 != 0 { 'x' } else { '-' },
            section.addr,
            section.size,
            section.offset
        )?;
        if let Some(count) = symbols {
            write!(f, ", {}", counted(count, "symbol", "symbols"))?;
        }
        Ok(())
    }
}

/// `p_type`'s name, for the types the ELF and GNU specifications define.
const fn segment_type(p_type: u32) -> Option<&'static str> {
    Some(match p_type {
        0 => "NULL",
        1 => "LOAD",
        2 => "DYNAMIC",
        3 => "INTERP",
        4 => "NOTE",
        5 => "SHLIB",
        6 => "PHDR",
        7 => "TLS",
        0x6474_e550 => "GNU_EH_FRAME",
        0x6474_e551 => "GNU_STACK",
        0x6474_e552 => "GNU_RELRO",
        0x6474_e553 => "GNU_PROPERTY",
        _ => return None,
    })
}

/// `sh_type`'s name, for the types the ELF specification defines.
const fn section_type(sh_type: u32) -> Option<&'static str> {
    Some(match sh_type {
        0 => "NULL",
        1 => "PROGBITS",
        2 => "SYMTAB",
        3 => "STRTAB",
        4 => "RELA",
        5 => "HASH",
        6 => "DYNAMIC",
        7 => "NOTE",
        8 => "NOBITS",
        9 => "REL",
        11 => "DYNSYM",
        14 => "INIT_ARRAY",
        15 => "FINI_ARRAY",
        16 => "PREINIT_ARRAY",
        17 => "GROUP",
        18 => "SYMTAB_SHNDX",
        _ => return None,
    })
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
