use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::vars::Store;

fn vars() -> Store {
    let mut vars = Store::with_room(16, 2048, 2048).unwrap();
    vars.assign(b"Num", Value::Number(10)).unwrap();
    vars.assign(b"Word", Value::String(b"1234".to_vec()))
        .unwrap();
    vars.assign(b"Mac", Value::Macro(b"  \"<Word>|J\"".to_vec()))
        .unwrap();
    vars.assign(b"Big", Value::String(vec![b'x'; 1000]))
        .unwrap();
    vars
}

fn eval(text: &[u8]) -> Result<Answer, Error> {
    evaluate(text, &mut vars(), &mut Budget::new(1000))
}

fn int(text: &[u8]) -> i32 {
    match eval(text) {
        Ok(Answer::Integer(n)) => n,
        other => panic!("{:?} gave {other:?}", core::str::from_utf8(text)),
    }
}

fn string(text: &[u8]) -> Vec<u8> {
    match eval(text) {
        Ok(Answer::String(s)) => s,
        other => panic!("{:?} gave {other:?}", core::str::from_utf8(text)),
    }
}

#[test]
fn the_documented_examples_evaluate_as_documented() {
    assert_eq!(int(b"127 * 23 >> 2"), 730);
    assert_eq!(int(b"50*-3"), -150);
    assert_eq!(string(b"\"HI\"+\"LO\""), b"HILO");
    assert_eq!(string(b"\"HELLO\" RIGHT 2"), b"LO");
    assert_eq!(string(b"\"HELLO\" LEFT 3"), b"HEL");
    assert_eq!(int(b"LEN \"HELLO\""), 5);
    assert_eq!(string(b"STR 24"), b"24");
    assert_eq!(int(b"\"2\"-1"), 1);
    assert_eq!(int(b"\"\"=0"), -1);
    assert_eq!(string(b"1234 LEFT 2"), b"12");
    assert_eq!(int(b"&3F AND &17"), 0x17);
}

#[test]
fn priorities_and_associativity_follow_basic() {
    assert_eq!(int(b"1+2*3"), 7);
    assert_eq!(int(b"(1+2)*3"), 9);
    assert_eq!(int(b"10-4-3"), 3);
    assert_eq!(int(b"1<<2=4"), -1, "shifts and comparisons share a level");
    assert_eq!(int(b"NOT 1+1"), -1);
    assert_eq!(int(b"-2*3"), -6);
    assert_eq!(int(b"1 OR 2 AND 3"), 3);
    assert_eq!(string(b"STR -12 LEFT 2"), b"-12");
    assert_eq!(string(b"\"hello\" LEFT 2 LEFT 1"), b"he");
    assert_eq!(int(b"Word LEFT 2+1"), 13);
    assert_eq!(int(b"((1))"), 1);
}

#[test]
fn numbers_end_at_the_first_byte_not_a_digit_of_their_base() {
    assert_eq!(int(b"2AND3"), 2);
    assert_eq!(int(b"2_101+16_ff"), 260);
    assert_eq!(int(b"4294967295"), -1);
    assert_eq!(
        eval(b"&FFAND1"),
        Err(Error::UnknownOperand),
        "&FFA, then a name"
    );
    assert_eq!(eval(b"&"), Err(Error::BadNumber));
    assert_eq!(eval(b"37_1"), Err(Error::BadBase));
    assert_eq!(eval(b"4294967296"), Err(Error::NumberTooBig));
}

#[test]
fn strings_convert_to_integers_only_whole() {
    assert_eq!(int(b"VAL \" -  5 \""), -5);
    assert_eq!(int(b"VAL \"&ff\""), 255);
    assert_eq!(int(b"VAL \"-\""), 0);
    assert_eq!(eval(b"VAL \"12d3\""), Err(Error::NotAnInteger));
    assert_eq!(eval(b"VAL \"99999999999\""), Err(Error::NotAnInteger));
    assert_eq!(eval(b"\"abc\"=1"), Err(Error::NotAnInteger));
    assert_eq!(int(b"\"10\">9"), -1);
    assert_eq!(int(b"LEN 1234"), 4);
    assert_eq!(int(b"LEN -5"), 2);
}

#[test]
fn division_truncates_and_a_remainder_takes_the_dividend_s_sign() {
    assert_eq!(int(b"-7/2"), -3);
    assert_eq!(int(b"7/-2"), -3);
    assert_eq!(int(b"-7 MOD 2"), -1);
    assert_eq!(int(b"7 MOD -2"), 1);
    assert_eq!(int(b"-2147483648/-1"), i32::MIN);
    assert_eq!(int(b"-2147483648 MOD -1"), 0);
    assert_eq!(eval(b"1/0"), Err(Error::DivisionByZero));
    assert_eq!(eval(b"1 MOD 0"), Err(Error::DivisionByZero));
}

#[test]
fn shifts_past_31_saturate_and_a_negative_count_turns_them() {
    assert_eq!(int(b"1<<31"), i32::MIN);
    assert_eq!(int(b"1<<32"), 0);
    assert_eq!(int(b"-1>>40"), -1);
    assert_eq!(int(b"8>>-1"), 16);
    assert_eq!(int(b"-8>>>1"), 0x7FFF_FFFC);
    assert_eq!(int(b"-8>>>32"), 0);
    assert_eq!(int(b"-4<<-1"), -2);
    assert_eq!(
        int(b"5<<-2147483648"),
        5,
        "a count still negative once negated"
    );
    assert_eq!(int(b"5>>-2147483648"), 5);
}

#[test]
fn strings_compare_byte_by_byte_and_left_and_right_keep_what_fits() {
    assert_eq!(int(b"\"abc\"<\"abd\""), -1);
    assert_eq!(int(b"\"ab\"<\"abc\""), -1);
    assert_eq!(int(b"\"B\"<\"a\""), -1, "case counts");
    assert_eq!(int(b"\"\xE9\">\"z\""), -1, "bytes are unsigned");
    assert_eq!(string(b"\"abc\" LEFT 9"), b"abc");
    assert_eq!(string(b"\"abc\" RIGHT 3"), b"abc");
    assert_eq!(string(b"\"abc\" RIGHT 4"), b"abc");
    assert_eq!(
        string(b"\"abc\" LEFT -1"),
        b"abc",
        "a negative count is huge"
    );
    assert_eq!(string(b"\"abc\" LEFT 0"), b"");
    assert_eq!(string(b"\"abc\" RIGHT 0"), b"");
}

#[test]
fn variables_are_operands_by_type() {
    assert_eq!(int(b"Num*2"), 20);
    assert_eq!(int(b"num+Word"), 1244);
    assert_eq!(
        string(b"Mac"),
        b"1234\n",
        "a macro is read as GSTrans reads it"
    );
    assert_eq!(
        eval(b"and"),
        Err(Error::UnknownOperand),
        "lower case is a name"
    );
    assert_eq!(eval(b"Missing+1"), Err(Error::UnknownOperand));
    assert_eq!(
        eval(b"ORDER"),
        Err(Error::UnknownOperand),
        "OR, then a name DER"
    );
}

#[test]
fn references_outside_quotes_are_expanded_first() {
    assert_eq!(int(b"1<<2>>1"), 0, "`<<2>` is an unset variable");
    assert_eq!(int(b"1 << 2 >> 1"), 2);
    assert_eq!(
        eval(b"1<2>0"),
        Err(Error::UnknownOperand),
        "character 2 starts no item"
    );
    assert_eq!(int(b"<Num>+1"), 11);
    assert_eq!(
        string(b"\"<Num>|G\""),
        b"10|G",
        "a literal expands references, not escapes"
    );
    assert_eq!(string(b"\"a\"\"b\""), b"a\"b");
    assert_eq!(eval(b"\"open"), Err(Error::BadString));
    assert_eq!(eval(b"1\x01"), Err(Error::BadString));
}

#[test]
fn a_malformed_expression_gives_the_error_its_stack_shows() {
    for (text, error) in [
        (&b""[..], Error::MissingOperand),
        (b"1+", Error::MissingOperand),
        (b"*2", Error::MissingOperand),
        (b"()", Error::MissingOperand),
        (b"NOT *2", Error::MissingOperand),
        (b"1 2", Error::MissingOperand),
        (b"1 2+3", Error::MissingOperator),
        (b"(1 2)", Error::MissingOperator),
        (b"2(3)", Error::MissingOperator),
        (b"1 NOT 2", Error::MissingOperator),
        (b")", Error::MismatchedBrackets),
        (b"(1", Error::MismatchedBrackets),
        (b"1@", Error::UnknownOperand),
    ] {
        assert_eq!(eval(text), Err(error), "{:?}", core::str::from_utf8(text));
    }
}

#[test]
fn strings_and_expressions_are_bounded() {
    let mut text = vec![b' '; 1023];
    text[0] = b'1';
    assert_eq!(
        eval(&text),
        Err(Error::BufferOverflow),
        "past 1022 characters"
    );
    assert_eq!(int(&text[..1022]), 1);
    let two = b"Big+Big";
    assert_eq!(eval(two), Err(Error::StringTooLong));
    let mut deep = Vec::new();
    for _ in 0..15 {
        deep.extend_from_slice(b"Big+(");
    }
    deep.extend_from_slice(b"Big");
    deep.extend(core::iter::repeat_n(b')', 15));
    assert_eq!(eval(&deep), Err(Error::ExpressionStackOverflow));
}

#[test]
fn a_bound_spent_inside_an_operand_is_passed_on_as_itself() {
    let mut vars = Store::with_room(4, 64, 64).unwrap();
    vars.assign(b"A", Value::String(b"x".to_vec())).unwrap();
    vars.assign(b"M", Value::Macro(b"<A><A><A>".to_vec()))
        .unwrap();
    assert_eq!(
        evaluate(b"M", &mut vars, &mut Budget::new(2)),
        Err(Error::ExpansionTooComplex)
    );
    assert_eq!(
        evaluate(b"M", &mut vars, &mut Budget::new(4)),
        Ok(Answer::String(b"xxx".to_vec()))
    );
}

/// A store whose every lookup the host refuses.
struct Refusing;

impl Variables for Refusing {
    fn lookup(&mut self, _: &[u8]) -> Result<Option<Value>, Error> {
        Err(Error::SystemHeapFull)
    }

    fn assign(&mut self, _: &[u8], _: Value) -> Result<(), Error> {
        Err(Error::SystemHeapFull)
    }

    fn delete(&mut self, _: &[u8]) -> Result<bool, Error> {
        Err(Error::SystemHeapFull)
    }
}

#[test]
fn an_operand_the_host_cannot_read_is_its_refusal() {
    assert_eq!(
        evaluate(b"A", &mut Refusing, &mut Budget::new(4)),
        Err(Error::SystemHeapFull)
    );
}
