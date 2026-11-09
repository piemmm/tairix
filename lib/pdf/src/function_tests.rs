//! Functions: each type, their domains and ranges, the calculator, and how a
//! function shared or nested is read.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::{Function, Kind, Values, Work, MAX_PROGRAM, MAX_STACK};
use crate::document::Document;
use crate::fixture::{reference, with_objects, OPTIONS};
use crate::limits::{MAX_EVALUATION_STEPS, MAX_FUNCTION_NODES};
use crate::memo::Outcome;
use crate::PdfError;

impl Work {
    /// The steps still left.
    pub(crate) fn left(&self) -> u64 {
        self.0.get()
    }
}

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

fn load(file: &[u8], num: u32) -> Result<Arc<Function>, PdfError> {
    let document = Document::open(crate::fixture::kept(file), &OPTIONS)?;
    Function::load(&document, &reference(num))
}

fn calculator(program: &str, domain: &str, range: &str) -> Arc<Function> {
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
    let document = Document::open(crate::fixture::kept(&file), &options).expect("a document");
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

/// A stitching function of `pieces` pieces, each `piece`, evenly bounded and
/// each encoded whole, `bounds` of its bounds written.
fn stitched(piece: &str, pieces: usize, bounds: usize) -> String {
    let count = f64::from(u32::try_from(pieces).expect("few pieces"));
    let written: Vec<String> = (1..=bounds)
        .map(|at| {
            format!(
                "{}",
                f64::from(u32::try_from(at).expect("few bounds")) / count
            )
        })
        .collect();
    let functions = format!("{piece} ").repeat(pieces);
    format!(
        "<</FunctionType 3 /Domain [0 1] /Functions [{functions}] /Bounds [{}] /Encode [{}]>>",
        written.join(" "),
        "0 1 ".repeat(pieces)
    )
}

/// A function shared by reference is read once wherever it is met: three
/// levels of 16, 16 and 8 pieces, each level one function every piece of the
/// level above shares, are four functions read rather than 2321.
#[test]
fn a_function_shared_by_reference_is_read_once() {
    let file = with_objects(
        &[
            (9, &stitched("8 0 R", 16, 15)),
            (8, &stitched("7 0 R", 16, 15)),
            (7, &stitched("6 0 R", 8, 7)),
        ],
        &[(6, "/FunctionType 4 /Domain [0 1] /Range [0 1]", b"{ }")],
    );
    let function = load(&file, 9).expect("each function read once");
    let mut level = &function;
    for _ in 0..3 {
        let Kind::Stitching { functions, .. } = &level.kind else {
            panic!("a stitching function");
        };
        assert!(functions
            .iter()
            .all(|piece| Arc::ptr_eq(piece, &functions[0])));
        level = &functions[0];
    }
    // Pieces 4, 12 and 6 down, three quarters into the last.
    close(&function.values_at(&[2459.0 / 8192.0]), &[0.75]);
}

/// Every function one is built of counts against the bound whether it was
/// read for it or before it, so a function too complex alone is too complex
/// once its pieces are known.
#[test]
fn whether_a_function_is_too_complex_never_depends_on_what_was_read_before() {
    let file = with_objects(
        &[
            (
                10,
                &stitched("<</FunctionType 2 /Domain [0 1] /N 1>>", 256, 255),
            ),
            (11, &stitched("10 0 R", 16, 15)),
        ],
        &[],
    );
    assert_eq!(1 + 16 * (1 + 256), MAX_FUNCTION_NODES + 17);
    assert_eq!(load(&file, 11).err(), Some(PdfError::TooComplex));
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    assert!(
        Function::load(&document, &reference(10)).is_ok(),
        "its piece alone fits"
    );
    assert_eq!(
        Function::load(&document, &reference(11)).err(),
        Some(PdfError::TooComplex)
    );
}

/// A stitching function whose bounds do not fit its pieces is refused before
/// any piece is read, and the refusal is kept.
#[test]
fn a_stitching_function_that_cannot_stand_reads_none_of_its_pieces() {
    let file = with_objects(
        &[
            (9, &stitched("8 0 R", 256, 254)),
            (8, &stitched("7 0 R", 256, 255)),
        ],
        &[(7, "/FunctionType 4 /Domain [0 1] /Range [0 1]", b"{ }")],
    );
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    assert_eq!(
        Function::load(&document, &reference(9)).err(),
        Some(PdfError::Malformed)
    );
    assert!(
        document.functions().outcome(8).is_none(),
        "a piece was read"
    );
    assert!(matches!(
        document.functions().outcome(9),
        Some(Outcome::Damaged(PdfError::Malformed))
    ));
}

/// A function that could not be read is not read again: its program is
/// decoded once however often it is asked for.
#[test]
fn a_function_that_could_not_be_read_is_not_read_again() {
    let entries = "/FunctionType 4 /Domain [0 1] /Range [0 1] /Filter /ASCIIHexDecode";
    // `{1 frob}`: an operator no calculator has.
    let file = with_objects(&[], &[(10, entries, b"7B3120 66726F62 7D>")]);
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let read = || Function::load(&document, &reference(10)).err();
    assert_eq!(read(), Some(PdfError::Malformed));
    let budget = document.budget().get();
    assert_eq!(read(), Some(PdfError::Malformed));
    assert_eq!(document.budget().get(), budget, "its program decoded again");
}

/// Whether a function nests too deep is judged where it is met: one refused
/// deep down is read near the top, and one read near the top is refused deep
/// down, whichever was met first.
#[test]
fn how_deep_a_function_nests_is_judged_where_it_is_met() {
    // 20 stitches 19, and so on down to 11 stitching 10: 20 nests ten levels.
    let chain: Vec<(u32, String)> = (11..=20u32)
        .map(|num| (num, stitched(&format!("{} 0 R", num - 1), 1, 0)))
        .collect();
    let mut objects = alloc::vec![(10, "<</FunctionType 2 /Domain [0 1] /N 1>>")];
    objects.extend(chain.iter().map(|(num, body)| (*num, body.as_str())));
    let file = with_objects(&objects, &[]);
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let read = |num| Function::load(&document, &reference(num));
    assert_eq!(read(20).err(), Some(PdfError::Malformed), "ten levels");
    close(&read(12).expect("two levels").values_at(&[0.25]), &[0.25]);
    assert_eq!(read(19).err(), Some(PdfError::Malformed), "nine levels");
    close(&read(18).expect("eight levels").values_at(&[0.25]), &[0.25]);
}

/// A function built of more functions than the bound is too complex, one it
/// shares counted each time it is met.
#[test]
fn a_function_built_of_more_functions_than_its_bound_is_too_complex() {
    let wide = 256;
    let piece = stitched("10 0 R", wide, wide - 1);
    for (pieces, fits) in [(15, true), (16, false)] {
        assert_eq!(1 + pieces + pieces * wide <= MAX_FUNCTION_NODES, fits);
        let file = with_objects(
            &[
                (10, "<</FunctionType 2 /Domain [0 1] /N 1>>"),
                (11, &stitched(&piece, pieces, pieces - 1)),
            ],
            &[],
        );
        let read = load(&file, 11);
        if fits {
            assert!(read.is_ok(), "{pieces} pieces");
        } else {
            assert_eq!(read.err(), Some(PdfError::TooComplex), "{pieces} pieces");
        }
    }
}

/// Reading a function is charged to the document's work as evaluating one
/// is: a function set inline, read again wherever what holds it is read
/// again, runs the work out rather than costing nothing.
#[test]
fn reading_a_function_is_charged_to_the_documents_work() {
    let file = with_objects(
        &[(
            10,
            "<</FunctionType 2 /Domain [0 1] /C0 [0 0] /C1 [1 1] /N 1>>",
        )],
        &[],
    );
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let inline = document
        .resolve(&reference(10))
        .expect("a dictionary")
        .into_owned()
        .expect("room");
    let work = document.work();
    let left = work.left();
    Function::load(&document, &inline).expect("work to spare");
    let cost = left - work.left();
    assert_eq!(
        cost,
        1 + 2 + 2 + 2,
        "the function and the six numbers it holds"
    );
    let spare = usize::try_from(work.left() - (cost - 1)).expect("a step count");
    work.charge(spare).expect("steps to take");
    assert_eq!(
        Function::load(&document, &inline).err(),
        Some(PdfError::TooComplex)
    );
}

/// A sampled function is charged what an evaluation takes: each corner found
/// across every input and read into every output.
#[test]
fn a_sampled_function_is_charged_for_its_corners_inputs_and_outputs() {
    let file = with_objects(
        &[],
        &[
            (
                10,
                "/FunctionType 0 /Domain [0 1 0 1] /Range [0 1 0 1 0 1] /Size [2 2] /BitsPerSample 8",
                &[0u8; 12],
            ),
            (
                11,
                "/FunctionType 0 /Domain [0 1 0 1 0 1 0 1 0 1 0 1 0 1 0 1 0 1] /Range [0 1] /Size [2 2 2 2 2 2 2 2 2] /BitsPerSample 8",
                &[0u8; 512],
            ),
        ],
    );
    // Four corners of two inputs read into three outputs; nine inputs, past
    // interpolation, read at one corner.
    for (num, steps) in [(10, 4 * (2 + 3)), (11, 9 + 1)] {
        let function = load(&file, num).expect("a sampled function");
        let work = Work::new();
        let mut outputs = Values::new();
        function
            .evaluate(&[0.5; 9], &work, &mut outputs)
            .expect("work to spare");
        assert_eq!(MAX_EVALUATION_STEPS - work.left(), steps, "function {num}");
    }
}

/// Every output of a sampled function is read from the same corners about
/// its inputs.
#[test]
fn a_sampled_function_reads_every_output_from_the_corners_about_it() {
    let file = with_objects(
        &[],
        &[(
            10,
            "/FunctionType 0 /Domain [0 1 0 1] /Range [0 1 0 1] /Size [2 2] /BitsPerSample 8",
            &[0, 255, 255, 0, 255, 255, 0, 0],
        )],
    );
    let function = load(&file, 10).expect("a sampled function");
    close(&function.values_at(&[0.25, 0.75]), &[0.625, 0.75]);
}

/// `index` reaching past the bottom of the stack fails the calculator
/// however far it reaches, and within it copies.
#[test]
fn index_past_the_stack_fails_however_far_it_reaches() {
    let copying = calculator("{ 0 index }", "0 1", "0 1 0 1");
    close(&copying.values_at(&[0.5]), &[0.5, 0.5]);
    for depth in ["1", "4294967295", "8999999999999999"] {
        let reaching = calculator(&format!("{{ {depth} index }}"), "0 1", "0 1");
        assert!(reaching.values_at(&[0.5]).is_empty(), "{depth} index");
    }
}

/// A calculator program holds at most 65 536 operators, a conditional's own
/// jump among them.
#[test]
fn a_calculator_program_holds_at_most_its_bound_of_operators() {
    let entries = "/FunctionType 4 /Domain [0 1] /Range [0 1]";
    for (numbers, tail, fits) in [
        (MAX_PROGRAM, "", true),
        (MAX_PROGRAM + 1, "", false),
        (MAX_PROGRAM - 2, " true { } if", true),
        (MAX_PROGRAM - 1, " true { } if", false),
    ] {
        let program = format!("{{{}{tail} }}", " 1".repeat(numbers));
        let file = with_objects(&[], &[(10, entries, program.as_bytes())]);
        let read = load(&file, 10);
        if fits {
            assert!(read.is_ok(), "{numbers} numbers and {tail:?}");
        } else {
            assert_eq!(
                read.err(),
                Some(PdfError::TooComplex),
                "{numbers} numbers and {tail:?}"
            );
        }
    }
}

/// An array of numbers longer than its reader uses is refused before any of
/// it is resolved, so a long one of references costs nothing to read.
#[test]
fn numbers_refuses_an_array_longer_than_its_reader_uses() {
    let refs = "9 0 R ".repeat(4);
    let file = with_objects(
        &[
            (9, "1"),
            (10, &format!("<</Short [1 2 3] /Long [{refs}]>>")),
        ],
        &[],
    );
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let held = reference(10);
    let dict = document.resolve(&held).expect("a dictionary");
    let dict = dict.as_dict().expect("a dictionary");
    assert_eq!(
        super::numbers(&document, dict, b"Short", 3),
        Ok(Some(alloc::vec![1.0, 2.0, 3.0]))
    );
    assert_eq!(
        super::numbers(&document, dict, b"Long", 3),
        Err(PdfError::Malformed)
    );
    assert_eq!(
        super::numbers(&document, dict, b"Long", 4),
        Ok(Some(alloc::vec![1.0; 4]))
    );
    assert_eq!(super::numbers(&document, dict, b"Absent", 3), Ok(None));
}
