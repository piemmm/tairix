//! GCR in the drive: four bytes coded as five and back, and a data block's
//! 260 bytes — the mark, the buffer's 256, their checksum and two zeros —
//! as the 325 that fill its buffer, then the overflow. Coding works in
//! place: a block is encoded from its end and decoded from its start, so
//! neither ever writes over a byte it has still to read.

use super::ws::{zp, OVERFLOW, OVERFLOW_BYTES};
use crate::media::c1541::gcr::CODES;
use crate::media::c1541::recording::{job, DATA_MARK};
use crate::rom::asm::{abs, hi, imm, lo, zp as z, Acc, Asm, RTS};

/// A five-bit value that is no code's.
const NOT_A_CODE: u8 = 0xFF;
/// The groups of a block between its first and its last: the buffer's
/// bytes 3 to 254, four a group.
const MIDDLE_GROUPS: u8 = 63;

/// Each five-bit value's nybble, where it is a code.
const NYBBLES: [u8; 32] = {
    let mut table = [NOT_A_CODE; 32];
    let mut nybble: u8 = 0;
    while (nybble as usize) < CODES.len() {
        table[CODES[nybble as usize] as usize] = nybble;
        nybble += 1;
    }
    table
};

/// A group's eight codes, in the order its nybbles come, high first.
const CODES_IN_GROUP: u8 = 8;
/// The GCR bytes of a group.
const GCR_BYTES: u8 = 5;
/// The bits of a code.
const CODE_BITS: u8 = 5;

/// The GCR bytes code `code` of a group reaches into, first and last.
const fn bytes_of(code: u8) -> (u8, u8) {
    let first_bit = CODE_BITS * code;
    (first_bit / 8, (first_bit + CODE_BITS - 1) / 8)
}

/// What each nybble's code puts in byte `byte` of a group as code `code`:
/// its bits there, where the group's forty bits place them.
const fn part(code: u8, byte: u8) -> [u8; 16] {
    let mut table = [0; 16];
    let mut nybble = 0;
    while nybble < 16 {
        let mut bit = 0;
        while bit < CODE_BITS {
            let at = CODE_BITS * code + bit;
            if at / 8 == byte && CODES[nybble] & (1 << (CODE_BITS - 1 - bit)) != 0 {
                table[nybble] |= 0x80 >> (at % 8);
            }
            bit += 1;
        }
        nybble += 1;
    }
    table
}

/// Each code's parts, one table for each byte it reaches into, by code.
const PARTS: [(&str, &str); CODES_IN_GROUP as usize] = [
    ("gcr_part_0", ""),
    ("gcr_part_1a", "gcr_part_1b"),
    ("gcr_part_2", ""),
    ("gcr_part_3a", "gcr_part_3b"),
    ("gcr_part_4a", "gcr_part_4b"),
    ("gcr_part_5", ""),
    ("gcr_part_6a", "gcr_part_6b"),
    ("gcr_part_7", ""),
];
const _: () = {
    let mut code = 0;
    while code < CODES_IN_GROUP {
        let (first, last) = bytes_of(code);
        assert!(
            last < GCR_BYTES && (PARTS[code as usize].1.is_empty() == (first == last)),
            "a code's second table only where it reaches a second byte"
        );
        code += 1;
    }
};

pub fn emit(a: &mut Asm) {
    encoding(a);
    decoding(a);
    streams(a);
    blocks(a);
    unblocks(a);
    a.later(|a| {
        a.here("gcr_nybbles");
        a.bytes(&NYBBLES);
    });
    for code in 0..CODES_IN_GROUP {
        let (first, last) = bytes_of(code);
        let (into_first, into_last) = PARTS[usize::from(code)];
        a.later(move |a| {
            a.here(into_first);
            a.bytes(&part(code, first));
        });
        if first != last {
            a.later(move |a| {
                a.here(into_last);
                a.bytes(&part(code, last));
            });
        }
    }
}

/// `encode_group`: PLAIN's four bytes as GCR's five, each nybble's code put
/// where the group's forty bits hold it, the high nybble first.
fn encoding(a: &mut Asm) {
    a.here("encode_group");
    for code in 0..CODES_IN_GROUP {
        a.lda(z(zp::PLAIN + u16::from(code / 2)));
        if code % 2 == 0 {
            for _ in 0..4 {
                a.lsr(Acc);
            }
        } else {
            a.and(imm(0x0F));
        }
        a.tax();
        let (first, last) = bytes_of(code);
        let (into_first, into_last) = PARTS[usize::from(code)];
        for (byte, table) in [(first, into_first), (last, into_last)] {
            if table.is_empty() {
                continue;
            }
            let gcr = z(zp::GCR + u16::from(byte));
            a.lda(abs(table).x());
            // A byte's first code starts it.
            if code > 0 && bytes_of(code - 1).1 == byte {
                a.ora(gcr);
            }
            a.sta(gcr);
        }
    }
    a.rts();
}

/// `decode_group`: GCR's five back into PLAIN's four, C set where a value
/// is no code; each code is taken from where it lies, so GCR is spent.
fn decoding(a: &mut Asm) {
    // Two exits for no code, each within its half's branches.
    let (early, late) = (a.local(), a.local());
    a.bind(early);
    a.sec();
    a.rts();
    a.here("decode_group");
    for code in 0..CODES_IN_GROUP {
        take_code(a, code);
        a.tax();
        a.lda(abs("gcr_nybbles").x());
        a.bmi(if code < CODES_IN_GROUP / 2 {
            early
        } else {
            late
        });
        let plain = z(zp::PLAIN + u16::from(code / 2));
        if code % 2 == 0 {
            for _ in 0..4 {
                a.asl(Acc);
            }
        } else {
            a.ora(plain);
        }
        a.sta(plain);
    }
    a.clc();
    a.rts();
    a.bind(late);
    a.sec();
    a.rts();
}

/// A group's code `code` into A from where its five bits lie, the codes in
/// order: one over two bytes leaves the first shifted.
fn take_code(a: &mut Asm, code: u8) {
    let gcr = |byte: u8| z(zp::GCR + u16::from(byte));
    match code {
        0 => {
            a.lda(gcr(0));
            for _ in 0..3 {
                a.lsr(Acc);
            }
        }
        // The next byte's top bits shifted into this one's bottom, which
        // then holds the code.
        1 | 6 => {
            let (first, _) = bytes_of(code);
            a.lda(gcr(first + 1));
            for _ in 0..(CODE_BITS * code % 8 + CODE_BITS - 8) {
                a.asl(Acc);
                a.rol(gcr(first));
            }
            a.lda(gcr(first));
            a.and(imm(0x1F));
        }
        2 => {
            a.lda(gcr(1));
            a.lsr(Acc);
            a.and(imm(0x1F));
        }
        3 => {
            a.lda(gcr(1));
            a.lsr(Acc);
            a.lda(gcr(2));
            a.ror(Acc);
            for _ in 0..3 {
                a.lsr(Acc);
            }
        }
        4 => {
            a.lda(gcr(3));
            a.asl(Acc);
            a.lda(gcr(2));
            a.rol(Acc);
            a.and(imm(0x1F));
        }
        5 => {
            a.lda(gcr(3));
            a.lsr(Acc);
            a.lsr(Acc);
            a.and(imm(0x1F));
        }
        _ => {
            a.lda(gcr(4));
            a.and(imm(0x1F));
        }
    }
}

/// The GCR stream through the job's buffer and on into the overflow:
/// `stream_get` takes its next byte forward, `stream_put` puts A and steps
/// back, from the overflow's first byte to the buffer's last. Y is 0
/// after either.
fn streams(a: &mut Asm) {
    a.here("stream_get");
    a.ldy(imm(0));
    a.lda(z(zp::STREAM).ind_y());
    a.inc(z(zp::STREAM));
    a.bne(RTS);
    a.pha();
    a.lda(imm(lo(OVERFLOW)));
    a.sta(z(zp::STREAM));
    a.lda(imm(hi(OVERFLOW)));
    a.sta(z(zp::STREAM + 1));
    a.pla();
    a.rts();

    a.here("stream_put");
    let (back, borrowed) = (a.local(), a.local());
    a.ldy(imm(0));
    a.sta(z(zp::STREAM).ind_y());
    a.lda(z(zp::STREAM));
    a.cmp(imm(lo(OVERFLOW)));
    a.bne(back);
    a.lda(z(zp::STREAM + 1));
    a.cmp(imm(hi(OVERFLOW)));
    a.bne(back);
    a.lda(imm(0xFF));
    a.sta(z(zp::STREAM));
    a.lda(z(zp::BUFFER + 1));
    a.sta(z(zp::STREAM + 1));
    a.rts();
    a.bind(back);
    a.lda(z(zp::STREAM));
    a.bne(borrowed);
    a.dec(z(zp::STREAM + 1));
    a.bind(borrowed);
    a.dec(z(zp::STREAM));
    a.rts();

    // A group that ends short of its page's last byte is taken at once; the
    // rest go byte by byte, so the buffer still hands on to the overflow.
    a.here("stream_get_group");
    let (get, singly) = (a.local(), a.local());
    a.lda(z(zp::STREAM));
    a.cmp(imm(0xFF - GCR_BYTES + 1));
    a.bcs(singly);
    a.ldy(imm(0));
    for byte in 0..GCR_BYTES {
        a.lda(z(zp::STREAM).ind_y());
        a.sta(z(zp::GCR + u16::from(byte)));
        a.iny();
    }
    a.tya();
    a.clc();
    a.adc(z(zp::STREAM));
    a.sta(z(zp::STREAM));
    a.rts();
    a.bind(singly);
    a.ldx(imm(0));
    a.bind(get);
    a.jsr("stream_get");
    a.sta(z(zp::GCR).x());
    a.inx();
    a.cpx(imm(GCR_BYTES));
    a.bne(get);
    a.rts();
}

/// `encode_block`: the buffer's 256 bytes as the block's GCR, `CHECKSUM`
/// theirs. The first group holds the mark and the buffer's first three
/// bytes, the last the buffer's last byte, the checksum and two zeros; each
/// between holds four from `INDEX`.
fn blocks(a: &mut Asm) {
    a.here("encode_block");
    let (sum, middle) = (a.local(), a.local());
    a.ldy(imm(0));
    a.tya();
    a.bind(sum);
    a.eor(z(zp::BUFFER).ind_y());
    a.iny();
    a.bne(sum);
    a.sta(z(zp::CHECKSUM));
    a.lda(imm(lo(OVERFLOW + u16::from(OVERFLOW_BYTES) - 1)));
    a.sta(z(zp::STREAM));
    a.lda(imm(hi(OVERFLOW)));
    a.sta(z(zp::STREAM + 1));
    a.ldy(imm(LAST_BYTE));
    a.lda(z(zp::BUFFER).ind_y());
    a.sta(z(zp::PLAIN));
    a.lda(z(zp::CHECKSUM));
    a.sta(z(zp::PLAIN + 1));
    a.lda(imm(0));
    a.sta(z(zp::PLAIN + 2));
    a.sta(z(zp::PLAIN + 3));
    a.jsr("encode_and_put");
    a.lda(imm(LAST_MIDDLE));
    a.sta(z(zp::INDEX));
    a.bind(middle);
    a.jsr("load_plain");
    a.jsr("encode_and_put");
    a.lda(z(zp::INDEX));
    a.sec();
    a.sbc(imm(4));
    a.sta(z(zp::INDEX));
    a.cmp(imm(FIRST_MIDDLE.wrapping_sub(4)));
    a.bne(middle);
    a.lda(imm(DATA_MARK));
    a.sta(z(zp::PLAIN));
    for offset in 0..3 {
        a.ldy(imm(offset));
        a.lda(z(zp::BUFFER).ind_y());
        a.sta(z(zp::PLAIN + 1 + u16::from(offset)));
    }
    a.here("encode_and_put");
    let put = a.local();
    a.jsr("encode_group");
    a.ldx(imm(4));
    a.bind(put);
    a.lda(z(zp::GCR).x());
    a.jsr("stream_put");
    a.dex();
    a.bpl(put);
    a.rts();

    a.here("load_plain");
    let load = a.local();
    a.ldx(imm(0));
    a.ldy(z(zp::INDEX));
    a.bind(load);
    a.lda(z(zp::BUFFER).ind_y());
    a.sta(z(zp::PLAIN).x());
    a.iny();
    a.inx();
    a.cpx(imm(4));
    a.bne(load);
    a.rts();
}

/// `decode_block`: the block's GCR back into the buffer, C set and A the
/// job's code where the mark is not there, a value is no code or the
/// checksum does not match; its groups as `encode_block` lays them out.
fn unblocks(a: &mut Asm) {
    a.here("decode_block");
    let (bad_code, no_mark, middle, check, mismatch) =
        (a.local(), a.local(), a.local(), a.local(), a.local());
    a.lda(z(zp::BUFFER));
    a.sta(z(zp::STREAM));
    a.lda(z(zp::BUFFER + 1));
    a.sta(z(zp::STREAM + 1));
    a.jsr("get_and_decode");
    a.bcs(bad_code);
    a.lda(z(zp::PLAIN));
    a.cmp(imm(DATA_MARK));
    a.bne(no_mark);
    for offset in 0..3 {
        a.ldy(imm(offset));
        a.lda(z(zp::PLAIN + 1 + u16::from(offset)));
        a.sta(z(zp::BUFFER).ind_y());
    }
    a.lda(imm(FIRST_MIDDLE));
    a.sta(z(zp::INDEX));
    a.bind(middle);
    a.jsr("get_and_decode");
    a.bcs(bad_code);
    a.jsr("store_plain");
    a.lda(z(zp::INDEX));
    a.clc();
    a.adc(imm(4));
    a.sta(z(zp::INDEX));
    a.cmp(imm(LAST_MIDDLE.wrapping_add(4)));
    a.bne(middle);
    a.jsr("get_and_decode");
    a.bcs(bad_code);
    a.ldy(imm(LAST_BYTE));
    a.lda(z(zp::PLAIN));
    a.sta(z(zp::BUFFER).ind_y());
    a.lda(z(zp::PLAIN + 1));
    a.sta(z(zp::CHECKSUM));
    a.ldy(imm(0));
    a.tya();
    a.bind(check);
    a.eor(z(zp::BUFFER).ind_y());
    a.iny();
    a.bne(check);
    a.cmp(z(zp::CHECKSUM));
    a.bne(mismatch);
    a.clc();
    a.rts();
    a.bind(mismatch);
    a.lda(imm(job::DATA_CHECKSUM));
    a.sec();
    a.rts();
    a.bind(no_mark);
    a.lda(imm(job::NO_DATA_BLOCK));
    a.sec();
    a.rts();
    a.bind(bad_code);
    a.lda(imm(job::BYTE_DECODING));
    a.sec();
    a.rts();

    a.here("get_and_decode");
    a.jsr("stream_get_group");
    a.jmp("decode_group");

    a.here("store_plain");
    let store = a.local();
    a.ldx(imm(0));
    a.ldy(z(zp::INDEX));
    a.bind(store);
    a.lda(z(zp::PLAIN).x());
    a.sta(z(zp::BUFFER).ind_y());
    a.iny();
    a.inx();
    a.cpx(imm(4));
    a.bne(store);
    a.rts();
}

/// The buffer's last byte, in the block's last group.
const LAST_BYTE: u8 = 0xFF;
/// The first and last middle groups' first bytes in the buffer.
const FIRST_MIDDLE: u8 = 3;
const LAST_MIDDLE: u8 = FIRST_MIDDLE + 4 * (MIDDLE_GROUPS - 1);
const _: () = assert!(
    LAST_MIDDLE + 3 == LAST_BYTE - 1,
    "the middle groups end where the last begins"
);
