use alloc::string::String;
use alloc::vec::Vec;

use super::*;
use crate::c::testing::{Client, CODE, FREE};

const MOV: u32 = 0xE1A0_C00D;
const STORE_FRAME: u32 = 0xE92D_D800;
const STORE_ARGUMENTS: u32 = 0xE92D_D80F;
const SUB_FP: u32 = 0xE24C_B004;
const OUTER_ENTRY: u32 = CODE + 12;
const INNER_ENTRY: u32 = CODE + 52;
const OUTER_FRAME: u32 = FREE + 0x1000;
const INNER_FRAME: u32 = FREE + 0x0F00;
const POINTED: u32 = FREE + 0x80;

/// Two APCS functions, each its name and the name's word before its entry,
/// `outer` storing a frame and `inner_fn` its four arguments too; and their
/// frames, `inner_fn`'s called from `outer`.
fn client() -> Client {
    let mut client = Client::new();
    let mut code = Vec::new();
    code.extend_from_slice(b"outer\0\0\0");
    code.extend_from_slice(&0xFF00_0008_u32.to_le_bytes());
    for word in [MOV, STORE_FRAME, SUB_FP, 0, 0, 0] {
        code.extend_from_slice(&word.to_le_bytes());
    }
    code.extend_from_slice(b"inner_fn\0\0\0\0");
    code.extend_from_slice(&0xFF00_000C_u32.to_le_bytes());
    for word in [MOV, STORE_ARGUMENTS, SUB_FP, 0] {
        code.extend_from_slice(&word.to_le_bytes());
    }
    client.put(CODE, &code);
    assert_eq!(client.word(INNER_ENTRY), MOV, "laid as planned");
    let outer_record = [0, OUTER_FRAME + 4, 0x9000, OUTER_ENTRY + 4 + 8];
    let words: Vec<u8> = outer_record.iter().flat_map(|w| w.to_le_bytes()).collect();
    client.put(OUTER_FRAME - 12, &words);
    let inner_saved = [
        0x11,
        0x22,
        0x33,
        POINTED,
        OUTER_FRAME,
        INNER_FRAME + 4,
        OUTER_ENTRY + 20,
        INNER_ENTRY + 4 + 8,
    ];
    let words: Vec<u8> = inner_saved.iter().flat_map(|w| w.to_le_bytes()).collect();
    client.put(INNER_FRAME - 28, &words);
    client.put(POINTED, &[1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0]);
    client
}

fn written(client: &mut Client, heading: Heading, pc: u32, fp: u32) -> String {
    let bytes = text(&mut client.session.kernel.os(), heading, pc, fp).expect("written");
    String::from_utf8(bytes).expect("text")
}

#[test]
fn each_frame_shows_its_saved_arguments_then_its_function_by_name() {
    let mut client = client();
    let pc = INNER_ENTRY + 24;
    let shown = written(&mut client, Heading::Postmortem, pc, INNER_FRAME);
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(lines[0], "", "a blank line before the heading");
    assert_eq!(lines.len(), 8, "{shown}");
    assert!(
        lines[2].contains('4') && lines[2].contains(&alloc::format!("0x{POINTED:08x}")),
        "{}",
        lines[2]
    );
    assert!(
        lines[2].ends_with("-> [0x00000001 0x00000002 0x00000003 0x00000004]"),
        "{}",
        lines[2]
    );
    assert!(
        lines[3].contains("0x00000033 51") && !lines[3].contains("->"),
        "{}",
        lines[3]
    );
    assert!(
        lines[5].contains("0x00000011 17"),
        "Argument 1 last: {}",
        lines[5]
    );
    assert!(
        lines[6].starts_with(&alloc::format!("{pc:x} ")) && lines[6].ends_with("inner_fn"),
        "{}",
        lines[6]
    );
    assert!(
        lines[7].starts_with(&alloc::format!("{:x} ", OUTER_ENTRY + 20))
            && lines[7].ends_with("outer"),
        "{}",
        lines[7]
    );
}

#[test]
fn a_nought_shows_as_eight_noughts_and_a_frame_without_a_name_is_unnamed() {
    assert_eq!(hex_alternate(0), b"00000000");
    assert_eq!(hex_alternate(0x44), b"0x00000044");
    assert_eq!(hex_short(0x82d8), b"82d8");
    assert_eq!(hex_short(0), b"0");
    let mut client = client();
    client.put(OUTER_ENTRY - 4, &[0; 4]);
    let shown = written(
        &mut client,
        Heading::Postmortem,
        OUTER_ENTRY + 20,
        OUTER_FRAME,
    );
    assert!(!shown.contains("outer"), "{shown}");
}

#[test]
fn a_chain_that_does_not_climb_is_reported_as_the_stack_overwritten() {
    let mut client = client();
    client.put(OUTER_FRAME - 12, &(OUTER_FRAME - 0x100).to_le_bytes());
    let shown = written(
        &mut client,
        Heading::Postmortem,
        OUTER_ENTRY + 20,
        OUTER_FRAME,
    );
    let overwritten = String::from_utf8(
        crate::module::message(&mut client.session.kernel.os(), "StackOverwritten").expect("text"),
    )
    .expect("text");
    assert!(
        shown.ends_with(&alloc::format!("\n{overwritten}\n")),
        "{shown}"
    );
}

#[test]
fn a_failed_check_s_address_heads_the_backtrace_in_place_of_the_request() {
    let mut client = client();
    let shown = written(&mut client, Heading::Address(0x104), 0, 0);
    assert!(
        shown.starts_with('(') && shown.contains("00000104") && shown.ends_with("\n\n"),
        "{shown:?}"
    );
    let library = crate::kernel::template(&mut client.session.kernel.os()).expect("template");
    let shown = written(&mut client, Heading::Postmortem, library + 4, OUTER_FRAME);
    let lines: Vec<&str> = shown.lines().collect();
    assert!(
        lines[2].starts_with(&alloc::format!("{:x} ", library + 4)),
        "the library's own frame: {shown}"
    );
}
