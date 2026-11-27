//! Conversions: number, file-size and string forms, and the SWIs that write them.

use super::*;

#[test]
fn numbers_convert_as_risc_os_writes_them() {
    assert_eq!(decimal(1_234_567, Some(b" ")), b"1 234 567");
    assert_eq!(decimal(-1000, Some(b",")), b"-1,000");
    assert_eq!(decimal(0, None), b"0");
    assert_eq!(sized(0xFF, 1, true), -1);
    assert_eq!(sized(0x1_FF, 1, false), 255);
    assert_eq!(hex(&0x1234_5678u32.to_le_bytes(), 6, true), b"345678");
}

#[test]
fn file_sizes_round_and_pick_their_unit() {
    assert_eq!(file_size(1, false), b"1 byte");
    assert_eq!(file_size(12, false), b"12 bytes");
    assert_eq!(file_size(5 * 1024 * 1024, false), b"5 Mbytes");
    assert_eq!(file_size(4095 * 1024, false), b"4095 kbytes");
    assert_eq!(file_size(4096 * 1024 - 1, false), b"4 Mbytes");
    assert_eq!(file_size(1234 * 1024, true), b"1234 kbytes");
    assert_eq!(file_size(1, true), b"   1  byte ");
}

#[test]
fn ipv6_writes_its_groups_and_shortens_the_longest_zero_run() {
    let address: [u8; 16] = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0xFE];
    assert_eq!(ipv6(&address, true), b"fe80::1");
    assert_eq!(ipv6(&address, false), b"fe80:0:0:0:0:0:0:1");
}

mod swis {
    use tairix_riscos_arm::psr;

    use crate::convert::{CONVERT_FILE_SIZE, CONVERT_FIXED_FILE_SIZE, CONVERT_VARIFORM};
    use crate::memory::layout::RISC_OS_5;
    use crate::memory::Bits;
    use crate::os::Regs;
    use crate::swi::os::{
        BINARY_TO_DECIMAL, EVALUATE_EXPRESSION, GS_INIT, GS_READ, GS_TRANS, READ_UNSIGNED,
        SUBSTITUTE_ARGS,
    };
    use crate::swi::X;
    use crate::testkit::{with_application, Session, DATA};

    const CONVERT_HEX8: u32 = 0xD4;
    const CONVERT_INTEGER4: u32 = 0xDC;
    const CONVERT_BINARY1: u32 = 0xDD;
    const CONVERT_SPACED_CARDINAL4: u32 = 0xE4;
    const OUT: u32 = DATA + 0x100;

    /// The conversions the kernel names for its own callers are RISC OS's.
    #[test]
    fn the_named_conversions_are_risc_os_s() {
        assert_eq!(
            [CONVERT_FIXED_FILE_SIZE, CONVERT_FILE_SIZE, CONVERT_VARIFORM],
            [0xEB, 0xEC, 0xED]
        );
    }

    fn session() -> Session {
        with_application(&RISC_OS_5, Bits::Bit32)
    }

    fn call(s: &mut Session, swi: u32, low: &[u32]) -> Regs {
        let mut r = [0; 13];
        r[..low.len()].copy_from_slice(low);
        s.user_swi(X | swi, r).0
    }

    /// The terminated text a conversion wrote at `OUT`.
    fn converted(s: &mut Session, regs: &Regs) -> alloc::vec::Vec<u8> {
        assert!(!regs.flag(psr::V), "converted");
        assert_eq!(regs.r[0], OUT);
        let length = (regs.r[1] - OUT) as usize;
        assert_eq!(s.read(OUT + regs.r[1] - OUT, 1), [0], "terminated");
        s.read(OUT, length)
    }

    #[test]
    fn a_conversion_writes_its_text_terminated_and_reports_the_room_left() {
        let mut s = session();
        let regs = call(&mut s, CONVERT_HEX8, &[0xDEAD_BEEF, OUT, 20]);
        assert_eq!(converted(&mut s, &regs), b"DEADBEEF");
        assert_eq!(regs.r[2], 12);
        let regs = call(&mut s, CONVERT_HEX8, &[0xDEAD_BEEF, OUT, 8]);
        assert!(regs.flag(psr::V), "no room for the terminator");
        assert_eq!(s.peek(regs.r[0]), 0x1E4);
        let regs = call(
            &mut s,
            CONVERT_INTEGER4,
            &[(-12345i32).cast_unsigned(), OUT, 20],
        );
        assert_eq!(converted(&mut s, &regs), b"-12345");
        let regs = call(&mut s, CONVERT_BINARY1, &[0x105, OUT, 20]);
        assert_eq!(converted(&mut s, &regs), b"00000101");
        let regs = call(&mut s, CONVERT_SPACED_CARDINAL4, &[1_234_567, OUT, 20]);
        assert_eq!(converted(&mut s, &regs), b"1 234 567");
        let regs = call(&mut s, CONVERT_FILE_SIZE, &[4096 * 1024, OUT, 20]);
        assert_eq!(converted(&mut s, &regs), b"4 Mbytes");
        let regs = call(&mut s, CONVERT_FIXED_FILE_SIZE, &[1, OUT, 20]);
        assert_eq!(converted(&mut s, &regs), b"   1  byte ");
    }

    #[test]
    fn variform_counts_when_asked_and_refuses_types_it_has_not() {
        let mut s = session();
        s.poke(DATA, &[0x0102_0304]);
        let regs = call(&mut s, CONVERT_VARIFORM, &[DATA, OUT, 0x8000_0000, 4, 10]);
        assert!(regs.flag(psr::V), "count mode always overflows");
        assert_eq!(regs.r[2], !7, "\"1.2.3.4\"");
        let regs = call(&mut s, CONVERT_VARIFORM, &[DATA, OUT, 20, 4, 10]);
        assert_eq!(converted(&mut s, &regs), b"1.2.3.4");
        let regs = call(&mut s, CONVERT_VARIFORM, &[DATA, OUT, 20, 4, 16]);
        assert!(regs.flag(psr::V));
        assert_eq!(s.peek(regs.r[0]), 0x1F1);
    }

    #[test]
    fn binary_to_decimal_writes_unterminated() {
        let mut s = session();
        s.write(OUT, b"xxxx");
        let regs = call(
            &mut s,
            BINARY_TO_DECIMAL,
            &[(-7i32).cast_unsigned(), OUT, 10],
        );
        assert_eq!(regs.r[2], 2);
        assert_eq!(s.read(OUT, 3), b"-7x");
    }

    #[test]
    fn read_unsigned_takes_bases_and_its_limit() {
        let mut s = session();
        for (text, base, value) in [
            (&b"&FF\0"[..], 10, 255),
            (b"2_101\0", 10, 5),
            (b"1F\0", 16, 31),
            (b"123\0", 10, 123),
        ] {
            s.write(DATA, text);
            let regs = call(&mut s, READ_UNSIGNED, &[base, DATA]);
            assert!(!regs.flag(psr::V), "{text:?}");
            assert_eq!(regs.r[2], value, "{text:?}");
        }
        s.write(DATA, b"300\0");
        let limited = (1 << 29) | 0xA;
        let regs = call(&mut s, READ_UNSIGNED, &[limited, DATA, 255]);
        assert!(regs.flag(psr::V), "past the limit R2 gives");
    }

    #[test]
    fn expressions_evaluate_to_integers_or_strings() {
        let mut s = session();
        s.write(DATA, b"2*3+1\0");
        let regs = call(&mut s, EVALUATE_EXPRESSION, &[DATA, OUT, 64]);
        assert_eq!((regs.r[1], regs.r[2]), (0, 7));
        s.write(DATA, b"\"ab\"+\"c\"\0");
        let regs = call(&mut s, EVALUATE_EXPRESSION, &[DATA, OUT, 64]);
        assert_ne!(regs.r[1], 0, "a string");
        assert_eq!(s.read(OUT, regs.r[2] as usize), b"abc");
        s.write(DATA, b"(1+\0");
        let regs = call(&mut s, EVALUATE_EXPRESSION, &[DATA, OUT, 64]);
        assert!(regs.flag(psr::V));
    }

    #[test]
    fn a_string_result_too_long_for_its_buffer_fills_what_there_is() {
        let mut s = session();
        s.write(DATA, b"\"abcdef\"\0");
        s.write(OUT, b"--------");
        let regs = call(&mut s, EVALUATE_EXPRESSION, &[DATA, OUT, 4]);
        assert!(regs.flag(psr::V), "Buffer overflow");
        assert_eq!(s.read(OUT, 8), b"abcd----", "as far as it goes");
        // A negative size, as *IF gives to want integers only, writes
        // nothing at all where R1 points.
        let regs = call(&mut s, EVALUATE_EXPRESSION, &[DATA, OUT, u32::MAX]);
        assert!(regs.flag(psr::V));
        assert_eq!(s.read(OUT, 8), b"abcd----");
    }

    /// A string an earlier `OS_GSInit` began reads on from its own R0 and
    /// R2, whatever string a later one began.
    #[test]
    fn gsread_continues_the_string_its_state_names_after_another_is_begun() {
        let mut s = session();
        s.write(DATA, b"abc\0");
        s.write(DATA + 0x40, b"xyz\0");
        let first = call(&mut s, GS_INIT, &[DATA, 0, 0]);
        assert_eq!(first.r[1], u32::from(b'a'));
        let first = call(&mut s, GS_READ, &[first.r[0], 0, first.r[2]]);
        assert_eq!(first.r[1], u32::from(b'a'));
        let second = call(&mut s, GS_INIT, &[DATA + 0x40, 0, 0]);
        assert_eq!(second.r[1], u32::from(b'x'));
        let first = call(&mut s, GS_READ, &[first.r[0], 0, first.r[2]]);
        assert_eq!(first.r[1], u32::from(b'b'), "the first string's own next");
        let second = call(&mut s, GS_READ, &[second.r[0], 0, second.r[2]]);
        assert_eq!(second.r[1], u32::from(b'x'));
    }

    #[test]
    fn gstrans_expands_escapes_and_substitute_args_fills_a_template() {
        let mut s = session();
        s.write(DATA, b"a|Jb<Alias$.>\0");
        let regs = call(&mut s, GS_TRANS, &[DATA, OUT, 64]);
        assert!(!regs.flag(psr::V));
        assert_eq!(s.read(OUT, regs.r[2] as usize), b"a\nbCat ");
        s.write(DATA, b"one two three\0");
        s.write(DATA + 0x40, b"%1-%0 %*1\0");
        let regs = call(&mut s, SUBSTITUTE_ARGS, &[DATA, OUT, 64, DATA + 0x40, 9]);
        assert!(!regs.flag(psr::V));
        assert_eq!(
            s.read(OUT, regs.r[2] as usize),
            b"two-one two three\0",
            "ended as the arguments end"
        );
    }
}
