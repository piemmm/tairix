//! The dictionaries' layout, and `OS_PrettyPrint` expanding the kernel's.

use super::*;

use crate::os::len32;

use crate::memory::layout::RISC_OS_5;
use crate::memory::Bits;
use crate::testkit::{with_application, Session, DATA};

#[test]
fn a_dictionary_is_counted_entries_ending_at_a_zero_length() {
    let bytes = dictionary(&[b"ab", b"", b"\x1b\x00x"]).unwrap();
    assert_eq!(bytes, [4, b'a', b'b', 0, 2, 0, 5, 0x1B, 0, b'x', 0, 0]);
    assert_eq!(dictionary(&[]).unwrap(), [0]);
}

fn expand(s: &mut Session, token: u8, special: &[u8]) -> Vec<u8> {
    s.write(DATA, &[0x1B, token, 0]);
    s.write(DATA + 0x10, special);
    s.write(DATA + 0x10 + len32(special.len()), &[0]);
    let mut r = [0; 13];
    r[..3].copy_from_slice(&[DATA, 0, DATA + 0x10]);
    s.user_swi(0x2_0044, r);
    s.output()
}

#[test]
fn the_kernel_s_dictionary_expands_its_tokens_through_pretty_print() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    s.capture_output();
    assert_eq!(expand(&mut s, 1, b"Run"), b"Syntax: *Run");
    assert_eq!(expand(&mut s, 3, b""), b"director");
    assert_eq!(
        expand(&mut s, 34, b""),
        b"{off}",
        "a space at column 0 dropped"
    );
    assert_eq!(expand(&mut s, 22, b""), b"", "the PRM's missing token");
    assert_eq!(expand(&mut s, 42, b""), b"", "past the last");
    let messages = s.kernel.os().world.dictionaries.messages;
    assert_eq!(
        s.kernel.os().read_byte(messages).ok(),
        Some(0),
        "MessageTrans's has no entries"
    );
}
