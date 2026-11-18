//! Sixteen-bit operations on zero page words, emitted in line.

use crate::rom::asm::{imm, zp, Asm};

/// `dst = src`.
pub(super) fn mov16(a: &mut Asm, dst: u16, src: u16) {
    a.lda(zp(src));
    a.sta(zp(dst));
    a.lda(zp(src + 1));
    a.sta(zp(dst + 1));
}

/// `dst = value`.
pub(super) fn set16(a: &mut Asm, dst: u16, value: u16) {
    let [low, high] = value.to_le_bytes();
    a.lda(imm(low));
    a.sta(zp(dst));
    a.lda(imm(high));
    a.sta(zp(dst + 1));
}

/// `v += 1`.
pub(super) fn inc16(a: &mut Asm, v: u16) {
    let done = a.local();
    a.inc(zp(v));
    a.bne(done);
    a.inc(zp(v + 1));
    a.bind(done);
}

/// `v -= 1`.
pub(super) fn dec16(a: &mut Asm, v: u16) {
    let low = a.local();
    a.lda(zp(v));
    a.bne(low);
    a.dec(zp(v + 1));
    a.bind(low);
    a.dec(zp(v));
}

/// `dst = x + y`.
pub(super) fn add16(a: &mut Asm, dst: u16, x: u16, y: u16) {
    a.clc();
    a.lda(zp(x));
    a.adc(zp(y));
    a.sta(zp(dst));
    a.lda(zp(x + 1));
    a.adc(zp(y + 1));
    a.sta(zp(dst + 1));
}

/// `dst = x - y`.
pub(super) fn sub16(a: &mut Asm, dst: u16, x: u16, y: u16) {
    a.sec();
    a.lda(zp(x));
    a.sbc(zp(y));
    a.sta(zp(dst));
    a.lda(zp(x + 1));
    a.sbc(zp(y + 1));
    a.sta(zp(dst + 1));
}

/// Carry set where `x >= y`, unsigned.
pub(super) fn cmp16(a: &mut Asm, x: u16, y: u16) {
    a.lda(zp(x));
    a.cmp(zp(y));
    a.lda(zp(x + 1));
    a.sbc(zp(y + 1));
}

/// Zero set where `v` is 0.
pub(super) fn zero16(a: &mut Asm, v: u16) {
    a.lda(zp(v));
    a.ora(zp(v + 1));
}
