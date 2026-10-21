//! What the decode service answers, as text a person reads: an instruction a
//! line with its encoding bytes and symbolised branch target, a symbol's label
//! line before the instruction it starts, and a region's summary line.
//!
//! Pure formatting over already-validated replies ([`crate::decode`]), so every
//! viewer of code — the `fstree` pane and the editor's Code display — lists an
//! instruction identically.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use crate::decode::{DecodeFailure, DecodeRefusal, InsnRecord, Region, RegionKind, SymbolRecord};
use crate::SandboxError;

/// Why a decode produced no listing, in the words a viewer states it in.
#[must_use]
pub const fn failure_reason(failure: DecodeFailure) -> &'static str {
    match failure {
        DecodeFailure::Refused(DecodeRefusal::UnrecognisedContainer) => {
            "not a recognised executable"
        }
        DecodeFailure::Refused(DecodeRefusal::MalformedContainer) => "malformed executable",
        DecodeFailure::Refused(DecodeRefusal::MalformedRequest) => "decode request refused",
        DecodeFailure::Sandbox(SandboxError::RequestTooLarge) => "too large to decode",
        DecodeFailure::Sandbox(_) => "the decode sandbox is unavailable",
        DecodeFailure::ReplyMalformed => "the decode worker replied nonsense",
        DecodeFailure::NoMemory => "there is not enough memory for the answer",
    }
}

/// One line of a code listing: a symbol label, or an instruction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeLine {
    /// The rendered text.
    pub text: String,
    /// Whether the line is drawn emphasised: a `<symbol>:` label, or a
    /// heading over what follows it.
    pub is_label: bool,
}

/// Encoding bytes shown per instruction line; a longer encoding elides with
/// `…` (the whole length is decoded regardless).
pub const SHOWN_BYTES: usize = 8;

/// The first `most` lines of `insns` as a listing: a `<symbol>:` label before
/// each instruction a known symbol starts, then the instruction with its
/// encoding bytes and a symbolised branch target. `symbols` is address-sorted.
#[must_use]
pub fn code_lines(insns: &[InsnRecord], symbols: &[SymbolRecord], most: usize) -> Vec<CodeLine> {
    let mut lines = Vec::with_capacity(insns.len().min(most));
    let listed = insns.iter().flat_map(|insn| {
        let labels = symbols_at(symbols, insn.address)
            .iter()
            .map(|symbol| CodeLine {
                text: format!("{:016x} <{}>:", symbol.addr, symbol.name),
                is_label: true,
            });
        labels.chain(core::iter::once_with(|| CodeLine {
            text: instruction_line(insn, symbols),
            is_label: false,
        }))
    });
    lines.extend(listed.take(most));
    lines
}

/// The run of the address-sorted `symbols` that starts at `address`.
fn symbols_at(symbols: &[SymbolRecord], address: u64) -> &[SymbolRecord] {
    let start = symbols.partition_point(|symbol| symbol.addr < address);
    let run = symbols[start..].partition_point(|symbol| symbol.addr == address);
    &symbols[start..start + run]
}

/// One instruction's line.
#[must_use]
pub fn instruction_line(insn: &InsnRecord, symbols: &[SymbolRecord]) -> String {
    let mut bytes = String::with_capacity(SHOWN_BYTES * 3);
    for byte in insn.bytes.iter().take(SHOWN_BYTES) {
        // Writing to a `String` cannot fail.
        let _ = write!(bytes, "{byte:02x} ");
    }
    if insn.bytes.len() > SHOWN_BYTES {
        bytes.pop();
        bytes.push('…');
    }
    let target = insn
        .branch_target
        .and_then(|target| symbolise(target, symbols))
        .unwrap_or_default();
    let operands = if insn.operands.is_empty() {
        String::new()
    } else {
        format!(" {}", insn.operands)
    };
    format!(
        "{:8x}:  {bytes:<width$} {}{operands}{target}",
        insn.address,
        insn.mnemonic,
        width = SHOWN_BYTES * 3,
    )
}

/// The ` <name>` or ` <name+0xoff>` a branch target lands in, when a symbol
/// covers it.
///
/// Every symbol starting at the nearest address at or below the target is a
/// candidate, so a short alias sharing a function's start cannot hide the
/// function a branch lands inside.
fn symbolise(target: u64, symbols: &[SymbolRecord]) -> Option<String> {
    let end = symbols.partition_point(|symbol| symbol.addr <= target);
    let start = symbols.get(end.checked_sub(1)?)?.addr;
    let symbol = symbols[..end]
        .iter()
        .rev()
        .take_while(|symbol| symbol.addr == start)
        .find(|symbol| target - symbol.addr < symbol.size.max(1))?;
    if target == symbol.addr {
        Some(format!(" <{}>", symbol.name))
    } else {
        Some(format!(" <{}+{:#x}>", symbol.name, target - symbol.addr))
    }
}

/// One region's summary line: kind, name, address, file extent, memory size
/// and permissions, in fixed columns.
#[must_use]
pub fn region_line(region: &Region) -> String {
    let kind = match region.kind {
        RegionKind::Code => "code",
        RegionKind::Data => "data",
    };
    let name = if region.name.is_empty() {
        "-"
    } else {
        region.name.as_str()
    };
    format!(
        "  {kind}  {:<20} addr {:#010x}  file {:#x}+{:#x}  mem {:#x}  {}{}{}",
        name,
        region.addr,
        region.file_offset,
        region.file_size,
        region.mem_size,
        if region.read { 'r' } else { '-' },
        if region.write { 'w' } else { '-' },
        if region.execute { 'x' } else { '-' },
    )
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use alloc::vec;

    use super::{code_lines, instruction_line, region_line, SHOWN_BYTES};
    use crate::decode::{InsnRecord, Region, RegionKind, SymbolRecord};

    fn insn(address: u64, bytes: &[u8], mnemonic: &str, target: Option<u64>) -> InsnRecord {
        InsnRecord {
            address,
            length: u32::try_from(bytes.len()).expect("length"),
            bytes: bytes.to_vec(),
            mnemonic: String::from(mnemonic),
            operands: String::new(),
            branch_target: target,
        }
    }

    fn symbol(name: &str, addr: u64, size: u64) -> SymbolRecord {
        SymbolRecord {
            name: String::from(name),
            addr,
            size,
        }
    }

    #[test]
    fn a_symbol_labels_the_instruction_it_starts_and_names_a_branch_into_it() {
        let symbols = vec![symbol("main", 0x1000, 0x20), symbol("start", 0x1000, 4)];
        let insns = vec![
            insn(0x1000, &[0x90], "nop", None),
            insn(0x1001, &[0xe8, 1, 2, 3, 4], "call", Some(0x1008)),
        ];
        let lines = code_lines(&insns, &symbols, usize::MAX);
        assert_eq!(lines.len(), 4, "two labels, two instructions");
        assert!(lines[0].is_label && lines[1].is_label && !lines[2].is_label);
        assert_eq!(lines[0].text, "0000000000001000 <main>:");
        assert!(
            lines[3].text.ends_with("call <main+0x8>"),
            "{}",
            lines[3].text
        );
    }

    #[test]
    fn a_listing_stops_at_its_line_budget_even_inside_a_run_of_labels() {
        let symbols: alloc::vec::Vec<SymbolRecord> = (0..100)
            .map(|n| symbol(&alloc::format!("alias{n}"), 0x2000, 1))
            .chain([symbol("before", 0x1fff, 1), symbol("after", 0x2001, 1)])
            .collect();
        let mut sorted = symbols;
        sorted.sort_by_key(|symbol| symbol.addr);
        let insns = vec![
            insn(0x2000, &[0x90], "nop", None),
            insn(0x2001, &[0xc3], "ret", None),
        ];
        let lines = code_lines(&insns, &sorted, 5);
        assert_eq!(lines.len(), 5);
        assert!(
            lines.iter().all(|line| line.is_label),
            "only the run's first labels"
        );
        let whole = code_lines(&insns, &sorted, usize::MAX);
        assert_eq!(
            whole.len(),
            100 + 1 + 1 + 1,
            "every alias, the nop, after's label, the ret"
        );
        assert!(!whole.iter().any(|line| line.text.contains("<before>")));
    }

    #[test]
    fn a_long_encoding_elides_and_a_target_no_symbol_covers_is_bare() {
        let long = insn(0x10, &[0xaa; SHOWN_BYTES + 3], "long", Some(0x9999));
        let line = instruction_line(&long, &[symbol("f", 0x0, 4)]);
        assert!(line.contains('…'));
        assert!(line.ends_with("long"), "{line}");
    }

    #[test]
    fn a_regions_line_states_its_kind_extent_and_permissions() {
        let region = Region {
            name: String::new(),
            kind: RegionKind::Code,
            addr: 0x40_0000,
            file_offset: 0x1000,
            file_size: 0x200,
            mem_size: 0x400,
            read: true,
            write: false,
            execute: true,
        };
        assert_eq!(
            region.code_end(),
            0x40_0200,
            "the zero-filled tail is not code"
        );
        let line = region_line(&region);
        assert!(line.starts_with("  code  -"), "{line}");
        assert!(line.ends_with("r-x"), "{line}");
    }
}
