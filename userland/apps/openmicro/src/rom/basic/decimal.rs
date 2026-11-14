//! Numbers in decimal, as BASIC II reads and writes them.
//!
//! Reading takes a digit alone, or several with no point or exponent that
//! fit in sixteen bits, as an integer at once. Anything else is gathered
//! into the forty-bit mantissa as an integer, keeping digits while it has
//! room for one more, and gives an integer when there is no point or
//! exponent and the value fits in 31 bits; otherwise the mantissa is scaled
//! by its decimal exponent a ten at a time. Writing
//! scales the value into 1–10 the same way, adds half a unit of the last
//! digit wanted, and takes the digits from the result's top four bits.

use super::crunch::NUMBER;
use super::float::{FAC, FAC2};
use super::ws::{kind, page, zp as z};
use crate::rom::asm::{abs, imm, zp, Acc, Asm, Label, RTS};

/// The decimal exponent gathered while reading, sixteen bits.
const DEXP: u16 = z::DECIMAL;
/// Bit 7: a point has been read; bit 6: the number is real.
const FLAGS: u16 = DEXP + 2;
const POINT_SEEN: u8 = 0x80;
const REAL: u8 = 0x40;
/// Where the number ends.
const END: u16 = FLAGS + 1;
// The sixteen bits several digits are first gathered in lie past what reading
// keeps, and below the integer left operand.
const _: () = assert!(NUMBER > END && NUMBER + 3 < z::IAC2);

/// The scaled value's decimal exponent.
const KEXP: u16 = z::DECIMAL;
/// How many digits to write.
const COUNT: u16 = KEXP + 1;
/// `@%`'s format and its digits or places, the latter capped.
pub const FORMAT: u16 = COUNT + 1;
pub const PLACES: u16 = FORMAT + 1;
/// The digits written before the point.
const POINT: u16 = PLACES + 1;
/// POINT once the point is written.
const WRITTEN: u8 = 0xFF;
/// Y while a character is put.
const Y_SAVE: u16 = POINT + 1;

/// The digit being added, or the exponent being read: past both reading's
/// and writing's own, as both add digits.
const DIGIT: u16 = Y_SAVE + 1;
const _: () = assert!(DIGIT < z::IAC2);

pub mod format {
    pub const GENERAL: u8 = 0;
    pub const EXPONENT: u8 = 1;
    pub const FIXED: u8 = 2;
}

/// The most digits a number is written with.
const MOST: u8 = 10;
/// A mantissa top byte at which the gathered integer has no room for
/// another digit.
const FULL: u8 = 0x19;
/// The exponent of a forty-bit mantissa read as an integer.
const INTEGER_40: u8 = 0x80 + 40;
/// The scaled value's bounds: ten's exponent and top byte, and one's
/// exponent.
const TEN_EXP: u8 = 0x84;
const TEN_TOP: u8 = 0xA0;
const ONE_EXP: u8 = 0x81;

pub fn emit(a: &mut Asm) {
    let (not_digit, ended) = reading(a);
    exponent(a, not_digit, ended);
    mantissa(a);
    let (digits, fixed) = writing(a);
    digits_written(a, digits, fixed);
    ending(a);
    rules(a);
}

fn reading(a: &mut Asm) -> (Label, Label) {
    // A number at (TEXT_A),Y: A its type, IAC or FAC its value, Y past it.
    // A digit alone, as most numbers in programs are, is an integer at once.
    a.here("read_number");
    let (several, full) = (a.local(), a.local());
    a.lda(zp(z::TEXT_A).ind_y());
    a.sec();
    a.sbc(imm(b'0'));
    a.cmp(imm(10));
    a.bcs(full);
    a.sta(zp(z::IAC));
    a.iny();
    a.lda(zp(z::TEXT_A).ind_y());
    a.dey();
    a.cmp(imm(b'.'));
    a.beq(full);
    a.cmp(imm(b'E'));
    a.beq(full);
    a.sec();
    a.sbc(imm(b'0'));
    a.cmp(imm(10));
    a.bcc(several);
    a.lda(imm(0));
    a.sta(zp(z::IAC + 1));
    a.sta(zp(z::IAC + 2));
    a.sta(zp(z::IAC + 3));
    a.iny();
    a.lda(imm(kind::INTEGER));
    a.rts();
    several_digits(a, several, full);
    a.bind(full);
    a.lda(imm(0));
    a.ldx(imm(4));
    let clear = a.here_local();
    a.sta(zp(FAC.m(0)).x());
    a.dex();
    a.bpl(clear);
    a.sta(zp(FAC.sign()));
    a.sta(zp(FAC.over()));
    a.sta(zp(DEXP));
    a.sta(zp(DEXP + 1));
    a.sta(zp(FLAGS));
    a.dey();
    let (next, not_point, not_digit, unkept, ended) =
        (a.local(), a.local(), a.local(), a.local(), a.local());
    a.bind(next);
    a.iny();
    a.lda(zp(z::TEXT_A).ind_y());
    a.cmp(imm(b'.'));
    a.bne(not_point);
    // A second point ends the number, as anything else but a digit or E.
    a.bit(zp(FLAGS));
    a.bmi(not_digit);
    a.lda(imm(POINT_SEEN | REAL));
    a.sta(zp(FLAGS));
    a.bne(next);
    a.bind(not_point);
    a.sec();
    a.sbc(imm(b'0'));
    a.cmp(imm(10));
    a.bcs(not_digit);
    a.ldx(zp(FAC.m(0)));
    a.cpx(imm(FULL));
    a.bcs(unkept);
    a.jsr("mantissa_ten_plus");
    a.bit(zp(FLAGS));
    a.bpl(next);
    a.lda(zp(DEXP));
    let low = a.local();
    a.bne(low);
    a.dec(zp(DEXP + 1));
    a.bind(low);
    a.dec(zp(DEXP));
    a.jmp(next);
    // A digit with no room counts in the exponent before the point and is
    // lost after it.
    a.bind(unkept);
    a.bit(zp(FLAGS));
    a.bmi(next);
    a.inc(zp(DEXP));
    a.bne(next);
    a.inc(zp(DEXP + 1));
    a.bpl(next);
    (not_digit, ended)
}

/// From `several`, with Y at the first of two digits or more: gathered in
/// sixteen bits, as the tokeniser gathers a line number, and an integer if
/// neither a point nor an exponent follows; otherwise Y back at the first,
/// read in full from `full`.
fn several_digits(a: &mut Asm, several: Label, full: Label) {
    a.bind(several);
    a.sty(zp(END));
    a.lda(imm(0));
    a.sta(zp(NUMBER));
    a.sta(zp(NUMBER + 1));
    let (digit, gathered, again) = (a.here_local(), a.local(), a.local());
    a.lda(zp(z::TEXT_A).ind_y());
    a.jsr("is_digit");
    a.bcc(gathered);
    a.and(imm(0x0F));
    a.jsr("ten_times_add");
    a.bcs(again);
    a.iny();
    a.bne(digit);
    a.bind(gathered);
    a.cmp(imm(b'.'));
    a.beq(again);
    a.cmp(imm(b'E'));
    a.beq(again);
    a.lda(zp(NUMBER));
    a.sta(zp(z::IAC));
    a.lda(zp(NUMBER + 1));
    a.sta(zp(z::IAC + 1));
    a.lda(imm(0));
    a.sta(zp(z::IAC + 2));
    a.sta(zp(z::IAC + 3));
    a.lda(imm(kind::INTEGER));
    a.rts();
    a.bind(again);
    a.ldy(zp(END));
    a.fall_into(full);
}

fn exponent(a: &mut Asm, not_digit: Label, ended: Label) {
    // E, a sign, and at most two digits.
    a.bind(not_digit);
    a.cmp(imm(b'E' - b'0'));
    a.bne(ended);
    a.lda(zp(FLAGS));
    a.ora(imm(REAL));
    a.sta(zp(FLAGS));
    a.lda(imm(0));
    a.sta(zp(DIGIT));
    a.tax();
    a.iny();
    a.lda(zp(z::TEXT_A).ind_y());
    a.cmp(imm(b'-'));
    let (unsigned, digits) = (a.local(), a.local());
    a.bne(unsigned);
    a.dex();
    a.iny();
    a.bne(digits);
    a.bind(unsigned);
    a.cmp(imm(b'+'));
    a.bne(digits);
    a.iny();
    a.bind(digits);
    let exponent_read = a.local();
    for second in [false, true] {
        a.lda(zp(z::TEXT_A).ind_y());
        a.sec();
        a.sbc(imm(b'0'));
        a.cmp(imm(10));
        a.bcs(exponent_read);
        a.iny();
        if second {
            // Ten times the first and this one, C clear throughout.
            a.asl(zp(DIGIT));
            a.adc(zp(DIGIT));
            a.asl(zp(DIGIT));
            a.asl(zp(DIGIT));
            a.adc(zp(DIGIT));
        }
        a.sta(zp(DIGIT));
    }
    a.bind(exponent_read);
    // DEXP += the exponent, negated when X says so.
    a.txa();
    let add = a.local();
    a.beq(add);
    a.lda(imm(0));
    a.sec();
    a.sbc(zp(DIGIT));
    a.sta(zp(DIGIT));
    a.bne(add);
    // A sign with no digits is no exponent.
    a.tax();
    a.bind(add);
    a.lda(zp(DEXP));
    a.clc();
    a.adc(zp(DIGIT));
    a.sta(zp(DEXP));
    a.txa();
    a.adc(zp(DEXP + 1));
    a.sta(zp(DEXP + 1));

    a.bind(ended);
    a.sty(zp(END));
    let real = a.local();
    a.bit(zp(FLAGS));
    a.bvs(real);
    a.lda(zp(FAC.m(0)));
    a.bne(real);
    a.lda(zp(FAC.m(1)));
    a.bmi(real);
    for i in 0..4 {
        a.lda(zp(FAC.m(4 - i)));
        a.sta(zp(z::IAC + i));
    }
    a.lda(imm(kind::INTEGER));
    a.rts();
    a.bind(real);
    a.lda(imm(INTEGER_40));
    a.sta(zp(FAC.exp()));
    a.jsr("fac_normalise");
    let (scale, down, scaled) = (a.here_local(), a.local(), a.local());
    a.lda(zp(DEXP + 1));
    a.bmi(down);
    a.ora(zp(DEXP));
    a.beq(scaled);
    a.jsr("fac_times_ten");
    a.lda(zp(DEXP));
    let low = a.local();
    a.bne(low);
    a.dec(zp(DEXP + 1));
    a.bind(low);
    a.dec(zp(DEXP));
    a.jmp(scale);
    a.bind(down);
    a.jsr("fac_divide_ten");
    a.inc(zp(DEXP));
    a.bne(scale);
    a.inc(zp(DEXP + 1));
    a.bmi(scale);
    a.bind(scaled);
    a.jsr("fac_finish");
    a.ldy(zp(END));
    a.lda(imm(kind::REAL));
    a.rts();
}

fn mantissa(a: &mut Asm) {
    // The gathered integer × 10 + A, the integer's fifth byte its lowest; Y
    // kept.
    a.here("mantissa_ten_plus");
    a.sta(zp(DIGIT));
    a.lda(zp(FAC.m(0)));
    for i in 1..5 {
        a.ora(zp(FAC.m(i)));
    }
    let add = a.local();
    a.beq(add);
    a.ldx(imm(4));
    let copy = a.here_local();
    a.lda(zp(FAC.m(0)).x());
    a.sta(zp(FAC2.m(0)).x());
    a.dex();
    a.bpl(copy);
    double(a);
    double(a);
    // Five times it never carries out: reading has room for a digit, and
    // writing has taken its digit's four bits.
    a.clc();
    a.jsr("fac_add_carry");
    double(a);
    a.bind(add);
    a.lda(zp(FAC.m(4)));
    a.clc();
    a.adc(zp(DIGIT));
    a.sta(zp(FAC.m(4)));
    a.bcc(RTS);
    for i in (0..4).rev() {
        a.inc(zp(FAC.m(i)));
        if i > 0 {
            a.bne(RTS);
        }
    }
    a.rts();
}

/// FAC's forty bits doubled.
fn double(a: &mut Asm) {
    a.asl(zp(FAC.m(4)));
    for i in (0..4).rev() {
        a.rol(zp(FAC.m(i)));
    }
}

fn writing(a: &mut Asm) -> (Label, Label) {
    // FAC written at the string buffer by FORMAT and PLACES, its length at
    // &36.
    a.here("format_number");
    a.lda(imm(0));
    a.sta(zp(z::STRING_LENGTH));
    a.sta(zp(KEXP));
    a.lda(zp(PLACES));
    a.cmp(imm(MOST + 1));
    let capped = a.local();
    a.bcc(capped);
    a.lda(imm(MOST));
    a.bind(capped);
    a.ldx(zp(FORMAT));
    a.cpx(imm(format::FIXED));
    let kept = a.local();
    a.beq(kept);
    a.tax();
    a.bne(kept);
    a.lda(imm(MOST));
    a.bind(kept);
    a.sta(zp(PLACES));
    a.lda(zp(FAC.m(0)));
    let (nonzero, digits) = (a.local(), a.local());
    a.bne(nonzero);
    // Zero: "0" generally, "0." and its places fixed, and as many zeros as
    // places with an exponent of 0.
    a.lda(zp(FORMAT));
    let (not_fixed, exponent_zero, fixed) = (a.local(), a.local(), a.local());
    a.cmp(imm(format::FIXED));
    a.bne(not_fixed);
    a.jmp("layout_zero");
    a.bind(not_fixed);
    a.cmp(imm(format::EXPONENT));
    a.beq(exponent_zero);
    a.lda(imm(b'0'));
    a.jmp("put_character");
    a.bind(exponent_zero);
    a.lda(zp(PLACES));
    a.sta(zp(COUNT));
    a.jmp(fixed);
    a.bind(nonzero);
    a.lda(zp(FAC.sign()));
    let positive = a.local();
    a.bpl(positive);
    a.lda(imm(b'-'));
    a.jsr("put_character");
    a.asl(zp(FAC.sign()));
    a.bind(positive);
    // Into 1–10, a ten at a time.
    let (down, up, scaled) = (a.here_local(), a.local(), a.local());
    a.jsr("at_least_ten");
    a.bcc(up);
    // KEXP stays within forty of zero, rising here and falling below.
    a.jsr("fac_divide_ten");
    a.inc(zp(KEXP));
    a.bpl(down);
    a.bind(up);
    a.lda(zp(FAC.exp()));
    a.cmp(imm(ONE_EXP));
    a.bcs(scaled);
    a.jsr("fac_times_ten");
    a.dec(zp(KEXP));
    a.bmi(up);
    a.bind(scaled);
    a.jsr("digits_wanted");
    a.lda(zp(COUNT));
    a.bmi(digits);
    // Half a unit of the last digit added: 0.5 ÷ 10 once per digit after
    // the first, or 5 when none is wanted but rounding may make one.
    a.ldx(imm(7));
    let save = a.here_local();
    a.lda(zp(z::FAC).x());
    a.sta(abs(page::TEMPS).x());
    a.dex();
    a.bpl(save);
    a.jsr("fac_one");
    a.dec(zp(FAC.exp()));
    a.ldx(zp(COUNT));
    let (ten, added) = (a.local(), a.local());
    a.beq(ten);
    let tenth = a.here_local();
    a.dex();
    a.beq(added);
    a.jsr("fac_divide_ten");
    a.jmp(tenth);
    a.bind(ten);
    a.jsr("fac_times_ten");
    a.bind(added);
    a.jsr("fac_to_fac2");
    a.ldx(imm(7));
    let restore = a.here_local();
    a.lda(abs(page::TEMPS).x());
    a.sta(zp(z::FAC).x());
    a.dex();
    a.bpl(restore);
    a.jsr("fac_sum");
    a.jsr("at_least_ten");
    a.bcc(digits);
    a.jsr("fac_divide_ten");
    a.inc(zp(KEXP));
    a.jsr("digits_wanted");
    (digits, fixed)
}

fn digits_written(a: &mut Asm, digits: Label, fixed: Label) {
    // The digits: the value as four integer bits and thirty-six of
    // fraction, each digit taken from the top before the fraction is
    // multiplied by ten.
    a.bind(digits);
    a.lda(imm(TEN_EXP));
    a.sec();
    a.sbc(zp(FAC.exp()));
    a.tax();
    a.beq(fixed);
    let shift = a.here_local();
    a.lsr(zp(FAC.m(0)));
    for i in 1..5 {
        a.ror(zp(FAC.m(i)));
    }
    a.dex();
    a.bne(shift);
    a.bind(fixed);
    a.jsr("layout_start");
    a.ldy(imm(0));
    let done = a.local();
    a.lda(zp(COUNT));
    a.bmi(done);
    let digit = a.here_local();
    a.cpy(zp(COUNT));
    a.beq(done);
    a.cpy(zp(POINT));
    let no_point = a.local();
    a.bne(no_point);
    a.jsr("put_point");
    a.bind(no_point);
    a.lda(zp(FAC.m(0)));
    for _ in 0..4 {
        a.lsr(Acc);
    }
    a.ora(imm(b'0'));
    a.jsr("put_character");
    a.lda(zp(FAC.m(0)));
    a.and(imm(0x0F));
    a.sta(zp(FAC.m(0)));
    a.lda(imm(0));
    a.jsr("mantissa_ten_plus");
    a.iny();
    a.bne(digit);
    a.bind(done);
    a.fall_into("layout_end");
}

fn ending(a: &mut Asm) {
    // After the digits: fixed places a point after a whole number; the
    // general form drops trailing zeros and a bare point; an exponent
    // follows, padded to three for the exponent form.
    a.here("layout_end");
    a.lda(zp(FORMAT));
    let not_fixed = a.local();
    a.cmp(imm(format::FIXED));
    a.bne(not_fixed);
    a.bit(zp(POINT));
    a.bmi(RTS);
    a.jmp("put_point");
    a.bind(not_fixed);
    let exponent = a.local();
    a.cmp(imm(format::EXPONENT));
    a.beq(exponent);
    a.bit(zp(POINT));
    let untrimmed = a.local();
    a.bpl(untrimmed);
    let strip = a.here_local();
    a.ldy(zp(z::STRING_LENGTH));
    a.lda(abs(page::STRING - 1).y());
    let stripped = a.local();
    a.cmp(imm(b'0'));
    a.bne(stripped);
    a.dec(zp(z::STRING_LENGTH));
    a.bne(strip);
    a.bind(stripped);
    a.cmp(imm(b'.'));
    a.bne(untrimmed);
    a.dec(zp(z::STRING_LENGTH));
    a.bind(untrimmed);
    a.bit(zp(FORMAT));
    a.bpl(RTS);
    a.bind(exponent);
    a.lda(imm(b'E'));
    a.jsr("put_character");
    a.ldx(zp(z::STRING_LENGTH));
    a.lda(zp(KEXP));
    let plus = a.local();
    a.bpl(plus);
    a.lda(imm(b'-'));
    a.jsr("put_character");
    a.lda(imm(0));
    a.sec();
    a.sbc(zp(KEXP));
    a.bind(plus);
    a.ldy(imm(b'0' - 1));
    a.sec();
    let tens = a.here_local();
    a.iny();
    a.sbc(imm(10));
    a.bcs(tens);
    a.adc(imm(10 + b'0'));
    a.pha();
    a.tya();
    a.cmp(imm(b'0'));
    let units = a.local();
    a.beq(units);
    a.jsr("put_character");
    a.bind(units);
    a.pla();
    a.jsr("put_character");
    a.lda(zp(FORMAT));
    a.cmp(imm(format::EXPONENT));
    a.bne(RTS);
    let pad = a.here_local();
    a.txa();
    a.clc();
    a.adc(imm(3));
    a.cmp(zp(z::STRING_LENGTH));
    a.beq(RTS);
    a.lda(imm(b' '));
    a.jsr("put_character");
    a.jmp(pad);
}

fn rules(a: &mut Asm) {
    // C set if FAC is at least ten.
    a.here("at_least_ten");
    a.lda(zp(FAC.exp()));
    a.cmp(imm(TEN_EXP));
    a.bne(RTS);
    a.lda(zp(FAC.m(0)));
    a.cmp(imm(TEN_TOP));
    a.rts();

    // COUNT: PLACES for the general and exponent forms; for fixed, the
    // digits to PLACES places, unless more than the most written, which
    // writes the general form with the most.
    a.here("digits_wanted");
    a.lda(zp(PLACES));
    a.ldx(zp(FORMAT));
    a.cpx(imm(format::FIXED));
    let counted = a.local();
    a.bne(counted);
    a.sec();
    a.adc(zp(KEXP));
    a.bmi(counted);
    a.cmp(imm(MOST + 1));
    a.bcc(counted);
    a.lda(imm(format::GENERAL));
    a.sta(zp(FORMAT));
    a.lda(imm(MOST));
    a.sta(zp(PLACES));
    a.bind(counted);
    a.sta(zp(COUNT));
    a.rts();

    // How many digits come before the point, and what comes before the
    // digits: "0." and its zeros when the point comes first. The general
    // form is fixed only for a point within its digits; otherwise it takes
    // the exponent form's layout, marked by FORMAT's top bit.
    a.here("layout_start");
    a.ldx(zp(KEXP));
    a.inx();
    a.txa();
    let (general, exponent_form, fixed_form) = (a.local(), a.local(), a.local());
    a.ldy(zp(FORMAT));
    a.beq(general);
    a.dey();
    a.beq(exponent_form);
    a.bne(fixed_form);
    a.bind(general);
    a.txa();
    let general_exponent = a.local();
    a.bmi(general_exponent);
    a.cmp(zp(PLACES));
    a.beq(fixed_form);
    a.bcc(fixed_form);
    a.bind(general_exponent);
    a.lda(imm(format::EXPONENT | 0x80));
    a.sta(zp(FORMAT));
    a.bind(exponent_form);
    a.lda(imm(1));
    a.bind(fixed_form);
    a.sta(zp(POINT));
    a.tax();
    a.beq("layout_zero");
    a.bpl(RTS);
    a.here("layout_zero");
    a.lda(imm(b'0'));
    a.jsr("put_character");
    a.jsr("put_point");
    // As many zeros as the point is before the first digit, at most the
    // places.
    a.lda(zp(KEXP));
    a.eor(imm(0xFF));
    a.cmp(zp(PLACES));
    let few = a.local();
    a.bcc(few);
    a.lda(zp(PLACES));
    a.bind(few);
    a.tax();
    a.fall_into("put_zeros");

    // X zeros.
    a.here("put_zeros");
    a.txa();
    a.beq(RTS);
    a.lda(imm(b'0'));
    a.jsr("put_character");
    a.dex();
    a.jmp("put_zeros");

    // The point, marked written.
    a.here("put_point");
    a.lda(imm(WRITTEN));
    a.sta(zp(POINT));
    a.lda(imm(b'.'));
    a.fall_into("put_character");

    // A appended to the string buffer; X and Y kept.
    a.here("put_character");
    a.sty(zp(Y_SAVE));
    a.ldy(zp(z::STRING_LENGTH));
    a.sta(abs(page::STRING).y());
    a.inc(zp(z::STRING_LENGTH));
    a.ldy(zp(Y_SAVE));
    a.rts();
}
