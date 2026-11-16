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

pub fn emit(a: &mut Asm) {
    groups(a);
    streams(a);
    blocks(a);
    unblocks(a);
    a.later(|a| {
        a.here("gcr_codes");
        a.bytes(&CODES);
        a.here("gcr_nybbles");
        a.bytes(&NYBBLES);
    });
}

/// `encode_group`: PLAIN's four bytes as GCR's five, each nybble's code
/// shifted in from the right, the high nybble first. `decode_group`: GCR's
/// five back into PLAIN's four, C set where a value is no code.
fn groups(a: &mut Asm) {
    a.here("encode_group");
    let (byte, shift, bit) = (a.local(), a.local(), a.local());
    a.ldy(imm(0));
    a.bind(byte);
    a.lda(abs(zp::PLAIN).y());
    for _ in 0..4 {
        a.lsr(Acc);
    }
    a.jsr(shift);
    a.lda(abs(zp::PLAIN).y());
    a.and(imm(0x0F));
    a.jsr(shift);
    a.iny();
    a.cpy(imm(4));
    a.bne(byte);
    a.rts();
    // Nybble A's five-bit code shifted into GCR's forty bits.
    a.bind(shift);
    a.tax();
    a.lda(abs("gcr_codes").x());
    for _ in 0..3 {
        a.asl(Acc);
    }
    a.ldx(imm(5));
    a.bind(bit);
    a.asl(Acc);
    for offset in (0..5).rev() {
        a.rol(z(zp::GCR + offset));
    }
    a.dex();
    a.bne(bit);
    a.rts();

    a.here("decode_group");
    let (byte, take, bit, invalid) = (a.local(), a.local(), a.local(), a.local());
    a.ldy(imm(0));
    a.bind(byte);
    a.jsr(take);
    for _ in 0..4 {
        a.asl(Acc);
    }
    a.sta(abs(zp::PLAIN).y());
    a.jsr(take);
    a.ora(abs(zp::PLAIN).y());
    a.sta(abs(zp::PLAIN).y());
    a.iny();
    a.cpy(imm(4));
    a.bne(byte);
    a.clc();
    a.rts();
    // The next five bits off GCR's top as their nybble; no code returns
    // from `decode_group` with C set.
    a.bind(take);
    a.lda(imm(0));
    a.ldx(imm(5));
    a.bind(bit);
    for offset in (0..5).rev() {
        if offset == 4 {
            a.asl(z(zp::GCR + offset));
        } else {
            a.rol(z(zp::GCR + offset));
        }
    }
    a.rol(Acc);
    a.dex();
    a.bne(bit);
    a.tax();
    a.lda(abs("gcr_nybbles").x());
    a.bmi(invalid);
    a.rts();
    a.bind(invalid);
    a.pla();
    a.pla();
    a.sec();
    a.rts();
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

    a.here("stream_get_group");
    let get = a.local();
    a.ldx(imm(0));
    a.bind(get);
    a.jsr("stream_get");
    a.sta(z(zp::GCR).x());
    a.inx();
    a.cpx(imm(5));
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
