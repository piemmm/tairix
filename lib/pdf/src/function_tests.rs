//! Functions: each type, their domains and ranges, and the calculator.

use super::{Function, Values, Work, MAX_STACK};
use crate::document::Document;
use crate::fixture::{reference, with_objects, OPTIONS};
use crate::PdfError;

/// A function evaluated with work to spare, as every test here wants.
trait Evaluated {
    fn values_at(&self, inputs: &[f64]) -> Values;
    fn values_left_of(&self, inputs: &[f64]) -> Values;
}

impl Evaluated for Function {
    fn values_at(&self, inputs: &[f64]) -> Values {
        let mut outputs = Values::new();
        self.evaluate(inputs, &Work::new(), &mut outputs)
            .expect("work to spare");
        outputs
    }

    fn values_left_of(&self, inputs: &[f64]) -> Values {
        let mut outputs = Values::new();
        self.evaluate_left(inputs, &Work::new(), &mut outputs)
            .expect("work to spare");
        outputs
    }
}

#[track_caller]
fn close(actual: &[f64], expected: &[f64]) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "{actual:?} against {expected:?}"
    );
    for (a, e) in actual.iter().zip(expected) {
        assert!((a - e).abs() < 1e-9, "{actual:?} against {expected:?}");
    }
}

fn load(file: &[u8], num: u32) -> Result<Function, PdfError> {
    let document = Document::open(file, &OPTIONS)?;
    Function::load(&document, &reference(num))
}

fn calculator(program: &str, domain: &str, range: &str) -> Function {
    let entries = alloc::format!("/FunctionType 4 /Domain [{domain}] /Range [{range}]");
    let file = with_objects(&[], &[(10, &entries, program.as_bytes())]);
    load(&file, 10).expect("a calculator function")
}

#[test]
fn an_exponential_function_blends_its_ends() {
    let file = with_objects(
        &[
            (
                10,
                "<</FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1>>",
            ),
            (11, "<</FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 2>>"),
            (12, "<</FunctionType 2 /Domain [0 1] /N 1>>"),
        ],
        &[],
    );
    let linear = load(&file, 10).expect("a function");
    close(&linear.values_at(&[0.5]), &[0.5, 0.0, 0.5]);
    close(&linear.values_at(&[2.0]), &[0.0, 0.0, 1.0]);
    close(&linear.values_at(&[-1.0]), &[1.0, 0.0, 0.0]);
    close(
        &load(&file, 11).expect("a function").values_at(&[0.5]),
        &[0.25],
    );
    close(
        &load(&file, 12).expect("a function").values_at(&[0.3]),
        &[0.3],
    );
}

#[test]
fn a_stitching_function_picks_its_piece_and_tells_a_step_from_a_join() {
    let file = with_objects(
        &[
            (10, "<</FunctionType 3 /Domain [0 1] /Functions [11 0 R 12 0 R] /Bounds [0.5] /Encode [0 1 1 0]>>"),
            (11, "<</FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1>>"),
            (12, "<</FunctionType 2 /Domain [0 1] /C0 [0.25] /C1 [0.75] /N 1>>"),
        ],
        &[],
    );
    let stitched = load(&file, 10).expect("a function");
    close(&stitched.values_at(&[0.25]), &[0.5]);
    // The second piece runs its encoding backwards: 0.75 at its start.
    close(&stitched.values_at(&[0.5]), &[0.75]);
    close(&stitched.values_left_of(&[0.5]), &[1.0]);
    close(&stitched.values_at(&[1.0]), &[0.25]);
    assert_eq!(stitched.bounds(), &[0.5]);
}

#[test]
fn a_stitching_function_must_be_consistent() {
    let file = with_objects(
        &[
            (
                10,
                "<</FunctionType 3 /Domain [0 1] /Functions [11 0 R] /Bounds [0.5] /Encode [0 1]>>",
            ),
            (11, "<</FunctionType 2 /Domain [0 1] /N 1>>"),
            (
                12,
                "<</FunctionType 3 /Domain [0 1] /Functions [12 0 R] /Bounds [] /Encode [0 1]>>",
            ),
        ],
        &[],
    );
    assert_eq!(load(&file, 10).err(), Some(PdfError::Malformed));
    assert_eq!(
        load(&file, 12).err(),
        Some(PdfError::Malformed),
        "a function stitching itself"
    );
}

#[test]
fn a_sampled_function_interpolates_between_samples() {
    let file = with_objects(
        &[],
        &[
            (
                10,
                "/FunctionType 0 /Domain [0 1] /Range [0 1] /Size [3] /BitsPerSample 8",
                &[0, 102, 255],
            ),
            (
                11,
                "/FunctionType 0 /Domain [0 1 0 1] /Range [0 1] /Size [2 2] /BitsPerSample 8",
                &[0, 255, 255, 0],
            ),
            (
                12,
                "/FunctionType 0 /Domain [0 1] /Range [0 1] /Size [9] /BitsPerSample 8",
                &[0, 1],
            ),
        ],
    );
    let one = load(&file, 10).expect("a sampled function");
    close(&one.values_at(&[0.0]), &[0.0]);
    close(&one.values_at(&[0.25]), &[51.0 / 255.0]);
    close(&one.values_at(&[0.5]), &[0.4]);
    close(&one.values_at(&[1.0]), &[1.0]);
    let two = load(&file, 11).expect("a sampled function");
    close(&two.values_at(&[0.5, 0.5]), &[0.5]);
    close(&two.values_at(&[1.0, 0.0]), &[1.0]);
    close(&two.values_at(&[1.0, 1.0]), &[0.0]);
    assert_eq!(
        load(&file, 12).err(),
        Some(PdfError::Malformed),
        "fewer samples than its size"
    );
}

#[test]
fn the_calculator_runs_conditionals_and_stack_operators() {
    let greater = calculator(
        "{ 2 copy gt { pop } { exch pop } ifelse }",
        "0 10 0 10",
        "0 10",
    );
    close(&greater.values_at(&[3.0, 5.0]), &[5.0]);
    close(&greater.values_at(&[7.0, 5.0]), &[7.0]);
    let step = calculator("{ dup 0.5 gt { pop 1 } if }", "0 1", "0 1");
    close(&step.values_at(&[0.75]), &[1.0]);
    close(&step.values_at(&[0.25]), &[0.25]);
    let wave = calculator("{ 360 mul sin 2 div 0.5 add }", "0 1", "0 1");
    close(&wave.values_at(&[0.25]), &[1.0]);
    let arithmetic = calculator(
        "{ pop 7 2 idiv 7 2 mod 1 3 bitshift 3 1 2 3 1 roll 2 index }",
        "0 1",
        "0 99 0 99 0 99 0 99 0 99 0 99 0 99 0 99",
    );
    close(
        &arithmetic.values_at(&[0.0]),
        &[3.0, 1.0, 8.0, 2.0, 3.0, 1.0, 2.0],
    );
    let logic = calculator(
        "{ pop true false or { 1 } { 0 } ifelse 12 10 and 5 not }",
        "0 1",
        "-99 99 -99 99 -99 99",
    );
    close(&logic.values_at(&[0.0]), &[1.0, 8.0, -6.0]);
}

#[test]
fn a_calculator_that_fails_answers_nothing() {
    let underflow = calculator("{ pop pop }", "0 1", "0 1");
    assert!(underflow.values_at(&[0.5]).is_empty());
    let mut program = alloc::string::String::from("{");
    for _ in 0..=MAX_STACK {
        program.push_str(" 1");
    }
    program.push('}');
    assert!(calculator(&program, "0 1", "0 1")
        .values_at(&[0.5])
        .is_empty());
    let divide = calculator("{ 0 idiv }", "0 1", "0 1");
    assert!(
        divide.values_at(&[1.0]).is_empty(),
        "an integer division by zero"
    );
}

#[test]
fn a_calculator_program_must_parse() {
    for program in [
        "{ 1 frobnicate }",
        "{ 1 { 2 } }",
        "1 2 add",
        "{ 1 { 2 } { 3 } if }",
    ] {
        let entries = "/FunctionType 4 /Domain [0 1] /Range [0 1]";
        let file = with_objects(&[], &[(10, entries, program.as_bytes())]);
        assert_eq!(
            load(&file, 10).err(),
            Some(PdfError::Malformed),
            "{program}"
        );
    }
}

#[test]
fn an_unknown_function_type_is_unsupported() {
    let file = with_objects(&[(10, "<</FunctionType 1 /Domain [0 1]>>")], &[]);
    assert_eq!(load(&file, 10).err(), Some(PdfError::Unsupported));
}

#[test]
fn a_stitching_function_joins_more_pieces_than_a_colour_has_components() {
    let pieces = 40;
    let functions = "<</FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1>> ".repeat(pieces);
    let bounds: alloc::vec::Vec<alloc::string::String> = (1..pieces)
        .map(|piece| alloc::format!("{}", f64::from(u32::try_from(piece).expect("few")) / 40.0))
        .collect();
    let body = alloc::format!(
        "<</FunctionType 3 /Domain [0 1] /Functions [{functions}] /Bounds [{}] /Encode [{}]>>",
        bounds.join(" "),
        "0 1 ".repeat(pieces)
    );
    let file = with_objects(&[(10, &body)], &[]);
    let stitched = load(&file, 10).expect("forty pieces");
    close(&stitched.values_at(&[0.99]), &[0.6]);
}

#[test]
fn an_empty_stitching_function_is_damage() {
    let file = with_objects(
        &[(
            10,
            "<</FunctionType 3 /Domain [0 1] /Functions [] /Bounds [] /Encode []>>",
        )],
        &[],
    );
    assert_eq!(load(&file, 10).err(), Some(PdfError::Malformed));
}

#[test]
fn evaluation_stops_once_its_work_is_spent() {
    let adding = calculator("{ 1 add }", "0 1", "0 2");
    let work = Work(core::cell::Cell::new(5));
    let mut outputs = Values::new();
    assert!(adding.evaluate(&[0.5], &work, &mut outputs).is_ok());
    assert!(adding.evaluate(&[0.5], &work, &mut outputs).is_ok());
    assert_eq!(
        adding.evaluate(&[0.5], &work, &mut outputs).err(),
        Some(PdfError::TooComplex)
    );
}

#[test]
fn a_sampled_functions_samples_are_charged_to_the_budget() {
    let entries = "/FunctionType 0 /Domain [0 1] /Range [0 1] /Size [4096] /BitsPerSample 1";
    let file = with_objects(&[], &[(10, entries, &[0u8; 512])]);
    let options = crate::Options {
        password: None,
        budget: 8192,
    };
    let document = Document::open(&file, &options).expect("a document");
    assert_eq!(
        Function::load(&document, &reference(10)).err(),
        Some(PdfError::TooComplex)
    );
}

/// Bits are read in the order a sample stream packs them, however a take
/// crosses bytes, and as zeros past the end.
#[test]
fn bits_are_taken_in_order_across_bytes_and_as_zeros_past_the_end() {
    let data: alloc::vec::Vec<u8> = (0u32..64)
        .map(|at| u8::try_from((at * 37 + 11) % 256).expect("a byte"))
        .collect();
    let bit = |at: usize| {
        data.get(at / 8)
            .map_or(0, |&byte| u32::from(byte >> (7 - at % 8) & 1))
    };
    for width in 1..=32u32 {
        let mut bits = super::Bits::new(&data);
        let mut at = 0usize;
        for _ in 0..(data.len() * 8 / width as usize + 2) {
            let expected =
                (0..width as usize).fold(0u32, |value, offset| value << 1 | bit(at + offset));
            assert_eq!(bits.take(width), expected, "width {width} at {at}");
            at += width as usize;
        }
    }
    let mut past = super::Bits::new(&[0xff, 0xff, 0xff, 0xff, 0x80]);
    assert_eq!(past.take(40), u32::MAX, "at most 32");
    assert_eq!(past.take(1), 1, "the cursor moved 32");
}
