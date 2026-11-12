use super::*;
use crate::cpu::decode::{Alu, Shift};

#[test]
fn add_sets_carry_overflow_and_aux() {
    let mut f = 0;
    assert_eq!(add(&mut f, 0x7F, 1, false, BYTE), 0x80);
    assert_eq!(f & (OF | SF | AF | CF | ZF), OF | SF | AF);
    let mut f = 0;
    assert_eq!(add(&mut f, 0xFFFF, 1, false, WORD), 0);
    assert_eq!(f & (CF | ZF | OF | PF), CF | ZF | PF);
}

#[test]
fn sub_borrows() {
    let mut f = 0;
    assert_eq!(sub(&mut f, 0, 1, false, BYTE), 0xFF);
    assert_eq!(f & (CF | SF | AF | OF), CF | SF | AF);
    let mut f = 0;
    assert_eq!(sub(&mut f, 0x80, 1, false, BYTE), 0x7F);
    assert_eq!(f & (OF | CF), OF);
}

#[test]
fn logic_clears_carry_and_overflow() {
    let mut f = CF | OF | AF;
    assert_eq!(alu(&mut f, Alu::Xor, 0x55, 0x55, BYTE), 0);
    assert_eq!(f & (CF | OF | AF | ZF | PF), ZF | PF);
}

#[test]
fn inc_and_dec_keep_carry() {
    let mut f = CF;
    assert_eq!(inc(&mut f, 0xFF, BYTE), 0);
    assert_eq!(f & (CF | ZF), CF | ZF);
    let mut f = 0;
    assert_eq!(dec(&mut f, 0, WORD), 0xFFFF);
    assert_eq!(f & CF, 0);
}

#[test]
fn rotates_through_carry_count_each_step() {
    let mut f = CF;
    assert_eq!(shift(&mut f, Shift::Rcl, 0x80, 1, BYTE), 0x01);
    assert_eq!(f & CF, CF);
    let mut f = 0;
    assert_eq!(shift(&mut f, Shift::Shl, 1, 16, WORD), 0);
    let mut f = 0;
    assert_eq!(shift(&mut f, Shift::Rol, 0x8001, 4, WORD), 0x0018);
    assert_eq!(shift(&mut f, Shift::Sar, 0x80, 9, BYTE), 0xFF);
}

#[test]
fn a_zero_count_changes_nothing() {
    let mut f = CF | ZF;
    assert_eq!(shift(&mut f, Shift::Shl, 0x1234, 0, WORD), 0x1234);
    assert_eq!(f, CF | ZF);
}

#[test]
fn decimal_adjust_after_addition() {
    let mut f = 0;
    let sum = add(&mut f, 0x38, 0x45, false, BYTE);
    assert_eq!(daa(&mut f, crate::bits::lo(sum)), 0x83);
    let mut f = 0;
    let sum = add(&mut f, 0x99, 0x01, false, BYTE);
    assert_eq!(daa(&mut f, crate::bits::lo(sum)), 0x00);
    assert_eq!(f & CF, CF);
}

#[test]
fn parity_counts_the_low_byte() {
    assert!(even_parity(0));
    assert!(even_parity(0x03));
    assert!(!even_parity(0x01));
    let mut f = 0;
    set_szp(&mut f, 0x0100, WORD);
    assert_eq!(f & PF, PF);
}
