//! Text SWIs: the CRC, sorting, pretty-printing and line input.

use super::*;

#[test]
fn the_crc_is_risc_os_5s() {
    assert_eq!(crc16(0, b"123456789"), 0xBB3D);
    assert_eq!(crc16(0, b""), 0);
    assert_eq!(
        crc16(0x1234_5678, &[1, 2, 3, 4]),
        0x72CF,
        "a wide start carried down"
    );
    assert_eq!(crc16(0xFFFF_0000, &[1, 2, 3, 4]), 0xBFA0);
}

mod swis {
    use alloc::vec::Vec;

    use tairix_riscos_arm::psr;

    use crate::memory::layout::RISC_OS_5;
    use crate::memory::Bits;
    use crate::os::Outcome;
    use crate::testkit::{asm, with_application, Session, CODE, DATA};

    const X: u32 = 0x2_0000;
    const PRETTY_PRINT: u32 = 0x44;
    const HEAP_SORT: u32 = 0x4F;
    const CONFIRM: u32 = 0x59;
    const CRC: u32 = 0x5B;
    const HEAP_SORT32: u32 = 0x7F;

    fn session() -> Session {
        with_application(&RISC_OS_5, Bits::Bit32)
    }

    fn registers(low: &[u32]) -> [u32; 13] {
        let mut r = [0; 13];
        r[..low.len()].copy_from_slice(low);
        r
    }

    fn words(s: &mut Session, at: u32, n: u32) -> Vec<u32> {
        (0..n).map(|i| s.peek(at + 4 * i)).collect()
    }

    #[test]
    fn crc_steps_through_memory_by_its_increment() {
        let mut s = session();
        s.write(DATA, b"123456789");
        let (regs, _) = s.user_swi(X | CRC, registers(&[0, DATA, DATA + 9, 1]));
        assert_eq!(regs.r[0], 0xBB3D);
        // Every other byte, a step of two, of "1x2x3x...".
        s.write(DATA + 0x100, b"1x2x3x4x5x6x7x8x9x");
        let (regs, _) = s.user_swi(X | CRC, registers(&[0, DATA + 0x100, DATA + 0x112, 2]));
        assert_eq!(regs.r[0], 0xBB3D);
        let (regs, _) = s.user_swi(X | CRC, registers(&[0, DATA, DATA + 9, 0]));
        assert!(regs.flag(psr::V), "no increment");
    }

    #[test]
    fn long_work_in_a_swi_stops_when_the_host_says() {
        let mut s = crate::testkit::boot(&RISC_OS_5);
        s.application(Bits::Bit32, 0x10_0000);
        let start = crate::memory::layout::APPLICATION;
        s.stop_after.set(Some(2));
        let outcome = s.start_swi(X | CRC, registers(&[0, start, start + 0xF_FFFF, 1]));
        assert_eq!(outcome, Outcome::Stopped, "the host asked between steps");
    }

    #[test]
    fn heap_sort_orders_words_by_each_built_in_comparison() {
        let mut s = session();
        let unsorted = [5, 0xFFFF_FFFF, 3, 0, 9, 3];
        s.poke(DATA, &unsorted);
        s.user_swi(X | HEAP_SORT32, registers(&[6, DATA, 0]));
        assert_eq!(words(&mut s, DATA, 6), [0, 3, 3, 5, 9, 0xFFFF_FFFF]);
        s.poke(DATA, &unsorted);
        s.user_swi(X | HEAP_SORT32, registers(&[6, DATA, 1]));
        assert_eq!(
            words(&mut s, DATA, 6),
            [0xFFFF_FFFF, 0, 3, 3, 5, 9],
            "signed"
        );
        // Pointers to strings, folded or not, terminators compared too.
        let strings: [&[u8]; 4] = [b"beta\0", b"Alpha\r", b"alpha\0", b"BETA\0"];
        let mut pointers = Vec::new();
        for (i, text) in (0..).zip(strings) {
            let at = DATA + 0x100 + 0x10 * i;
            s.write(at, text);
            pointers.push(at);
        }
        s.poke(DATA, &pointers);
        s.user_swi(X | HEAP_SORT32, registers(&[4, DATA, 5]));
        let sorted: Vec<u32> = words(&mut s, DATA, 4);
        let order: Vec<u32> = sorted.iter().map(|p| (p - DATA - 0x100) / 0x10).collect();
        assert_eq!(order, [1, 3, 2, 0], "case-sensitive: A, B, a, b");
        s.poke(DATA, &pointers);
        s.user_swi(X | HEAP_SORT32, registers(&[4, DATA, 4]));
        let sorted: Vec<u32> = words(&mut s, DATA, 4);
        let first = (sorted[0] - DATA - 0x100) / 0x10;
        let second = (sorted[1] - DATA - 0x100) / 0x10;
        assert_eq!(first, 2, "alpha\\0 before Alpha\\r: the terminators decide");
        assert_eq!(second, 1);
    }

    #[test]
    fn heap_sort_calls_a_guest_comparison_and_reorders_the_blocks() {
        let mut s = session();
        // Descending by the blocks' first words: R0 sorts first if its is the
        // greater, CMP R1, R0 setting LT then.
        s.poke(
            CODE,
            &[
                asm::ldr(0, 0, 0),
                asm::ldr(1, 1, 0),
                asm::cmp(1, 0),
                asm::mov_pc_lr(),
            ],
        );
        let blocks = DATA + 0x200;
        for (i, value) in (0..).zip([20u32, 40, 10, 30]) {
            s.poke(blocks + 8 * i, &[value, value + 1]);
        }
        // Pointers built from the blocks, then the blocks put in order.
        let flags = (1 << 30) | (1 << 31);
        s.user_swi(
            X | HEAP_SORT32,
            registers(&[4, DATA, CODE, 0, blocks, 8, 0, flags]),
        );
        assert_eq!(
            words(&mut s, DATA, 4),
            [blocks, blocks + 8, blocks + 16, blocks + 24],
            "each pointer at its block, where it is now"
        );
        assert_eq!(
            words(&mut s, blocks, 8),
            [40, 41, 30, 31, 20, 21, 10, 11],
            "the blocks moved into descending order"
        );
        // OS_HeapSort's flags are R1's top bits; bit 30 asks for both. The
        // pointers are compared by the words they point at.
        for (i, value) in (0..).zip([3u32, 1, 2]) {
            s.poke(blocks + 4 * i, &[value]);
        }
        s.user_swi(
            X | HEAP_SORT,
            registers(&[3, DATA | (1 << 30), 2, 0, blocks, 4]),
        );
        assert_eq!(words(&mut s, blocks, 3), [1, 2, 3]);
        // Bit 31 alone reorders blocks by pointers the caller built.
        for (i, value) in (0..).zip([9u32, 7, 8]) {
            s.poke(blocks + 4 * i, &[value]);
            s.poke(DATA + 4 * i, &[blocks + 4 * i]);
        }
        s.user_swi(
            X | HEAP_SORT,
            registers(&[3, DATA | (1 << 31), 2, 0, blocks, 4]),
        );
        assert_eq!(words(&mut s, blocks, 3), [7, 8, 9]);
    }

    #[test]
    fn a_count_past_memory_wraps_its_addresses_rather_than_overflowing() {
        let mut s = session();
        s.stop_after.set(Some(1));
        let outcome = s.start_swi(X | HEAP_SORT32, registers(&[0x8000_0001, DATA, 0]));
        assert!(
            matches!(
                outcome,
                Outcome::Ended(_) | Outcome::Stopped | Outcome::Budget
            ),
            "{outcome:?}"
        );
    }

    #[test]
    fn heap_sort_keeps_equal_keys_in_risc_os_s_order() {
        // Knuth's algorithm H over keys that compare by their low byte only:
        // the order equal keys end in is the algorithm's, which this pins.
        let mut s = session();
        // CMP of the low bytes: AND R0, R0, #&FF; AND R1, R1, #&FF.
        let and = |rd: u32| {
            tairix_riscos_arm::a32::encode(&tairix_riscos_arm::instruction::Instruction {
                cond: tairix_riscos_arm::cond::Condition::Al,
                op: tairix_riscos_arm::instruction::Op::Data {
                    op: tairix_riscos_arm::instruction::DataOp::And,
                    s: false,
                    rd: tairix_riscos_arm::reg::Reg::field(rd),
                    rn: tairix_riscos_arm::reg::Reg::field(rd),
                    operand: tairix_riscos_arm::instruction::Operand::Immediate(
                        tairix_riscos_arm::instruction::Immediate::encode(0xFF).expect("encodable"),
                    ),
                },
            })
            .expect("encodable")
        };
        s.poke(CODE, &[and(0), and(1), asm::cmp(0, 1), asm::mov_pc_lr()]);
        let keys = [0x101, 0x202, 0x301, 0x402, 0x501, 0x601];
        s.poke(DATA, &keys);
        s.user_swi(X | HEAP_SORT32, registers(&[6, DATA, CODE]));
        assert_eq!(
            words(&mut s, DATA, 6),
            [0x601, 0x301, 0x101, 0x501, 0x402, 0x202],
            "as s/HeapSort's steps, transcribed by hand, leave them"
        );
    }

    #[test]
    fn pretty_print_lays_words_out_and_expands_a_dictionary() {
        let mut s = session();
        s.capture_output();
        s.write(DATA, b"Hello   wide\tworld\0");
        s.user_swi(X | PRETTY_PRINT, registers(&[DATA]));
        assert_eq!(s.output(), b"Hello wide      world");
        // A dictionary of two entries, the second nesting the first and the
        // text R2 names; each entry its length byte, its text and a zero.
        let dictionary = DATA + 0x80;
        s.write(dictionary, b"\x06big \0\x09\x1B\x01\x1B\0dog\0\0");
        s.write(DATA, b"\x1B\x02!\0");
        s.write(DATA + 0x40, b"red \0");
        s.user_swi(
            X | PRETTY_PRINT,
            registers(&[DATA, dictionary, DATA + 0x40]),
        );
        assert_eq!(s.output(), b"big red dog!");
        s.write(DATA, b"a\x1B\x01b\0");
        s.user_swi(X | PRETTY_PRINT, registers(&[DATA]));
        assert_eq!(s.output(), b"aSyntax: *b", "the kernel's dictionary");
        s.write(DATA, b"\r\x1B\0\0");
        s.write(DATA + 0x40, b"Xy\0");
        s.user_swi(X | PRETTY_PRINT, registers(&[DATA, 0, DATA + 0x40]));
        assert_eq!(
            s.output(),
            b"\n\rXy",
            "the zero after an escape no terminator"
        );
    }

    #[test]
    fn confirm_without_vdu_drivers_fails_reading_the_pointer_as_risc_os_does() {
        let mut s = session();
        let (regs, _) = s.user_swi(X | CONFIRM, [0; 13]);
        assert!(regs.flag(psr::V));
        assert_eq!(s.peek(regs.r[0]), 0xFE, "OS_Byte 106 is the VDU drivers'");
    }
}
