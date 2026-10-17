//! Unit tests for the Code display: listing an executable through the real
//! sandboxed decode path (the in-process loopback worker), the instruction
//! set it is read as, and the display's ask, answer and scroll.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_log::DiscardSink;
use tairix_sandbox::decode::{DecodeFailure, DecodeRefusal, DecodeService, Isa, MAX_INPUT};
use tairix_sandbox::loopback::LoopbackLauncher;
use tairix_sandbox::{ParserSandbox, SandboxError, WorkerEnd};

use super::{
    list_code, CodeDisplay, CodeListing, Unlisted, LISTING, LISTING_BUDGET_NS, MAX_LISTING_LINES,
};

fn sandbox() -> ParserSandbox<LoopbackLauncher<fn() -> DecodeService>, DiscardSink> {
    ParserSandbox::new(
        LoopbackLauncher::new(DecodeService::default as fn() -> _),
        DiscardSink,
    )
}

/// `image` listed with no time passing.
fn listing_of(
    image: &[u8],
    chosen: Option<Isa>,
    system: Option<Isa>,
) -> Result<CodeListing, Unlisted> {
    list_code(&mut sandbox(), image, (chosen, system), &mut || 0)
}

fn texts(listing: &CodeListing) -> Vec<&str> {
    listing
        .lines
        .iter()
        .map(|line| line.text.as_str())
        .collect()
}

/// `value` as unsigned LEB128.
fn uleb(mut value: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let low = u8::try_from(value & 0x7f).expect("seven bits");
        value >>= 7;
        if value == 0 {
            bytes.push(low);
            return bytes;
        }
        bytes.push(low | 0x80);
    }
}

/// A wasm module of one function body: no locals, then `code`.
fn wasm(code: &[u8]) -> Vec<u8> {
    let mut body = vec![0x00];
    body.extend_from_slice(code);
    let mut section = uleb(1);
    section.extend(uleb(body.len()));
    section.extend_from_slice(&body);
    let mut bytes = b"\0asm\x01\0\0\0".to_vec();
    bytes.push(10);
    bytes.extend(uleb(section.len()));
    bytes.extend_from_slice(&section);
    bytes
}

/// A minimal ELF64 x86-64 executable whose one read-and-execute segment is
/// the whole file, followed by `code`.
fn elf(code: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0u8; 64 + 56];
    bytes[0..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&0x40_0078u64.to_le_bytes());
    bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
    bytes[58..60].copy_from_slice(&64u16.to_le_bytes());
    let size = u64::try_from(bytes.len() + code.len()).expect("small");
    let ph = 64;
    bytes[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes());
    bytes[ph + 4..ph + 8].copy_from_slice(&0b101u32.to_le_bytes());
    bytes[ph + 16..ph + 24].copy_from_slice(&0x40_0000u64.to_le_bytes());
    bytes[ph + 24..ph + 32].copy_from_slice(&0x40_0000u64.to_le_bytes());
    bytes[ph + 32..ph + 40].copy_from_slice(&size.to_le_bytes());
    bytes[ph + 40..ph + 48].copy_from_slice(&size.to_le_bytes());
    bytes[ph + 48..ph + 56].copy_from_slice(&0x1000u64.to_le_bytes());
    bytes.extend_from_slice(code);
    bytes
}

#[test]
fn an_executable_is_listed_as_its_own_instruction_set() {
    // nop; ret
    let listing = listing_of(&elf(&[0x90, 0xc3]), None, Some(Isa::Aarch64)).expect("a listing");
    assert_eq!(listing.isa, Isa::X86_64, "the container names its own set");
    assert!(listing.lines[0].is_label);
    assert!(
        listing.lines[0].text.starts_with("ELF64 \u{b7} x86-64"),
        "{}",
        listing.lines[0].text
    );
    let text = texts(&listing);
    assert!(
        text.iter().any(|line| line.contains("code ")),
        "a region line"
    );
    assert!(text.iter().any(|line| line.contains(" nop")), "{text:?}");
    assert!(text.iter().any(|line| line.contains(" ret")), "{text:?}");
    let widest = text.iter().map(|line| line.chars().count()).max();
    assert_eq!(Some(listing.widest), widest);
}

#[test]
fn a_chosen_instruction_set_overrides_the_containers() {
    let listing =
        listing_of(&elf(&[0x1f, 0x20, 0x03, 0xd5]), Some(Isa::Aarch64), None).expect("a listing");
    assert_eq!(listing.isa, Isa::Aarch64);
    assert!(listing.lines[0].text.contains("aarch64"));
}

#[test]
fn what_is_not_an_executable_or_is_too_large_is_refused_in_words() {
    let refused = listing_of(b"just some text", None, None).expect_err("refused");
    assert_eq!(refused.reason, "not a recognised executable");
    assert!(refused.lasting);
    let huge = vec![0u8; MAX_INPUT + 1];
    assert_eq!(listing_of(&huge, None, None), Err(Unlisted::too_large()));
}

#[test]
fn only_a_want_of_memory_or_of_a_worker_passes() {
    let passing = [
        DecodeFailure::Sandbox(SandboxError::WorkerUnavailable(Errno::PermissionDenied)),
        DecodeFailure::Sandbox(SandboxError::WorkerFailed(WorkerEnd::OutOfMemory)),
    ];
    let lasting = [
        DecodeFailure::Sandbox(SandboxError::WorkerFailed(WorkerEnd::Crashed)),
        DecodeFailure::Sandbox(SandboxError::WorkerFailed(WorkerEnd::OverBound)),
        DecodeFailure::Sandbox(SandboxError::RequestTooLarge),
        DecodeFailure::Refused(DecodeRefusal::MalformedContainer),
        DecodeFailure::ReplyMalformed,
    ];
    for failure in passing {
        assert!(!Unlisted::of(failure).lasting, "{failure:?}");
    }
    for failure in lasting {
        assert!(Unlisted::of(failure).lasting, "{failure:?}");
    }
    assert!(!Unlisted::no_memory().lasting);
}

#[test]
fn an_instruction_a_window_would_cut_is_listed_whole() {
    // A body of br_tables a hundred labels wide runs past several windows,
    // and one straddles where the first would have ended.
    let mut code = Vec::new();
    for _ in 0..1000 {
        code.extend_from_slice(&[0x0e, 100]);
        code.extend_from_slice(&[0; 101]);
    }
    code.push(0x0b);
    let listing = listing_of(&wasm(&code), None, None).expect("a listing");
    let text = texts(&listing);
    assert_eq!(
        text.iter().filter(|line| line.contains("br_table")).count(),
        1000
    );
    assert!(!text.iter().any(|line| line.contains("(bad)")), "{text:?}");
}

#[test]
fn a_listing_stops_at_its_line_bound_and_says_so() {
    let nops = vec![0x90; MAX_LISTING_LINES + 100];
    let listing = listing_of(&elf(&nops), None, None).expect("a listing");
    assert_eq!(listing.lines.len(), MAX_LISTING_LINES + 1);
    let last = &listing.lines[MAX_LISTING_LINES].text;
    assert!(last.contains("stops here"), "{last}");
}

#[test]
fn a_listing_that_outlasts_its_budget_stops_and_says_so() {
    let mut now = 0;
    let mut clock = || {
        let then = now;
        now += LISTING_BUDGET_NS;
        then
    };
    let listing = list_code(
        &mut sandbox(),
        &elf(&[0x90, 0xc3]),
        (None, None),
        &mut clock,
    )
    .expect("a listing");
    let text = texts(&listing);
    assert!(!text.iter().any(|line| line.contains(" nop")), "{text:?}");
    let last = text.last().expect("a line");
    assert!(last.contains("after 30 seconds"), "{last}");
}

#[test]
fn a_listing_says_when_not_every_symbol_is_named() {
    // A symbol table with no entry size cannot be read.
    let mut image = elf(&[0x90]);
    let sections = u64::try_from(image.len()).expect("small");
    image[40..48].copy_from_slice(&sections.to_le_bytes());
    image[60..62].copy_from_slice(&2u16.to_le_bytes());
    image.extend_from_slice(&[0; 64]);
    let mut symtab = [0u8; 64];
    symtab[4..8].copy_from_slice(&2u32.to_le_bytes());
    image.extend_from_slice(&symtab);
    let listing = listing_of(&image, None, None).expect("a listing");
    assert!(
        texts(&listing)
            .iter()
            .any(|line| line.contains("more symbols than are named")),
        "{:?}",
        texts(&listing)
    );
}

#[test]
fn a_document_is_offered_as_code_only_when_its_head_names_a_container() {
    assert!(CodeDisplay::of(&elf(&[])).is_some_and(|display| display.showing()));
    assert!(CodeDisplay::of(b"\0asm\x01\0\0\0").is_some());
    assert!(CodeDisplay::of(b"fn main() {}").is_none());
    assert!(CodeDisplay::of(&[]).is_none());
}

fn listed(lines: usize) -> CodeListing {
    let lines: Vec<_> = (0..lines)
        .map(|n| tairix_sandbox::listing::CodeLine {
            text: alloc::format!("line {n}"),
            is_label: false,
        })
        .collect();
    let widest = lines.iter().map(|line| line.text.len()).max().unwrap_or(0);
    CodeListing {
        lines,
        isa: Isa::X86_64,
        widest,
    }
}

fn refusal(reason: &str, lasting: bool) -> Unlisted {
    Unlisted {
        reason: String::from(reason),
        lasting,
    }
}

#[test]
fn a_listing_is_asked_once_per_generation_and_an_overtaken_answer_is_let_go() {
    let mut display = CodeDisplay::of(&elf(&[])).expect("an executable");
    assert_eq!(display.note(), Some(LISTING));
    let (first, isa) = display.want(1).expect("asked");
    assert_eq!(isa, None);
    assert_eq!(display.want(1), None, "asked once");

    // The document changed while the worker listed it: asked again, and the
    // first answer belongs to nobody.
    let (second, _) = display.want(2).expect("asked again");
    assert!(!display.listed(first, Ok(listed(3))));
    assert!(display.listed(second, Ok(listed(3))));
    assert_eq!(display.lines().len(), 3);
    assert_eq!(display.widest(), 6);
    assert_eq!(display.want(2), None, "already listed");

    // A chosen set lists afresh.
    assert!(display.choose(Isa::Riscv64));
    assert!(!display.choose(Isa::Riscv64));
    let (third, isa) = display.want(2).expect("asked for the new set");
    assert_eq!(isa, Some(Isa::Riscv64));
    assert!(display.listed(third, Err(refusal("refused", true))));
    assert_eq!(display.note(), Some("refused"));
    assert_eq!(
        display.want(2),
        None,
        "a refusal stands until the document changes"
    );
}

#[test]
fn a_display_not_shown_asks_for_nothing_and_scrolls_within_its_lines() {
    let mut display = CodeDisplay::of(&elf(&[])).expect("an executable");
    assert!(display.show(false));
    assert_eq!(display.want(1), None);
    assert!(display.show(true));
    let (id, _) = display.want(1).expect("asked");
    assert!(display.listed(id, Ok(listed(10))));
    assert!(display.scroll(4));
    assert_eq!(display.top(), 4);
    assert!(display.scroll(100));
    assert_eq!(display.top(), 9, "clamped to the last line");
    assert!(display.scroll(-100));
    assert_eq!(display.top(), 0);
    assert!(!display.scroll(-1));
}

#[test]
fn a_passing_refusal_is_asked_again_when_the_display_is_shown_afresh() {
    let mut display = CodeDisplay::of(&elf(&[])).expect("an executable");
    let (id, _) = display.want(1).expect("asked");
    assert!(display.listed(id, Err(Unlisted::no_memory())));
    assert_eq!(display.want(1), None, "it stands while shown");
    assert!(display.show(false));
    assert!(display.show(true));
    let (id, _) = display.want(1).expect("asked again");
    assert!(display.listed(id, Err(refusal("malformed executable", true))));
    assert!(display.show(false));
    assert!(display.show(true));
    assert_eq!(display.want(1), None, "a lasting refusal stands");
}

#[test]
fn choosing_the_set_already_listed_lists_nothing_again() {
    let mut display = CodeDisplay::of(&elf(&[])).expect("an executable");
    let (id, _) = display.want(1).expect("asked");
    assert!(display.listed(id, Ok(listed(3))));
    assert!(!display.choose(Isa::X86_64));
    assert_eq!(display.choice(), Some(Isa::X86_64));
    assert_eq!(display.lines().len(), 3);
    assert_eq!(display.want(1), None);
    assert_eq!(display.want(2).map(|(_, isa)| isa), Some(Some(Isa::X86_64)));
}

#[test]
fn a_refusal_shows_its_one_line_from_the_top() {
    let mut display = CodeDisplay::of(&elf(&[])).expect("an executable");
    let (id, _) = display.want(1).expect("asked");
    assert!(display.listed(id, Ok(listed(10))));
    assert!(display.set_top(9));
    display.refuse(Unlisted::too_large(), 2);
    assert_eq!(display.top(), 0);
    assert_eq!(display.line_count(), 1);
}
