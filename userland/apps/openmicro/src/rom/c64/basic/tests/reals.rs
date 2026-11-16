//! The reals against what the original computes: a test ROM runs scripts
//! of calls into the float routines, each set up as BASIC sets its
//! operands, over values and results recorded from the original.

use alloc::vec::Vec;

use super::super::{chrget, decimal, float, trans, ws};
use crate::machine::c64::memory::{BASIC_BASE, BASIC_BYTES};
use crate::machine::c64::{Machine, Standard};
use crate::media::prg::Program;
use crate::rom::asm::{abs, hi, imm, lo, zp as z, Asm, Image};
use crate::rom::c64::system;
use crate::rom::c64::testing::machine;

mod corpus;

/// The mailbox: a script runs while COMMAND is set; ERROR is the error a
/// script stopped at, 0 for none.
const COMMAND: u16 = 0xC000;
const ERROR: u16 = 0xC001;
const SAVED_STACK: u16 = 0xC002;
/// The operands, and the result a script stores.
const X: u16 = 0xC010;
const Y: u16 = 0xC018;
const Z: u16 = 0xC020;
const RESULT: u16 = 0xC028;
/// The text `text_to_fac` reads and `fac_to_text`'s copy.
const TEXT: u16 = 0xC040;
/// The script: steps of a routine's address and the address its A (low),
/// X (low) and Y (high) are given, ending at a zero routine.
const SCRIPT: u16 = 0xC100;
/// The driver's pointers, in zero page a program has to itself.
const STEP: u16 = 0xFB;
const VECTOR: u16 = 0xFD;

/// Errors as BASIC numbers them.
const ILLEGAL_QUANTITY: u8 = 14;
const OVERFLOW: u8 = 15;
const DIVISION_BY_ZERO: u8 = 20;

/// The test ROM: the reals' routines, a driver, and the helpers a script
/// calls that BASIC's interpreter would otherwise be.
fn rom() -> Image {
    let mut a = Asm::new(BASIC_BASE, BASIC_BYTES);
    a.word("driver");
    a.word("driver");
    driver(&mut a);
    helpers(&mut a);
    float::conversions(&mut a);
    float::emit(&mut a);
    decimal::emit(&mut a);
    trans::emit(&mut a);
    a.place_later();
    a.finish().expect("the test ROM assembles")
}

fn driver(a: &mut Asm) {
    let code = chrget::code().expect("CHRGET assembles");
    a.here("driver");
    let copy = a.local();
    let wait = a.local();
    let step = a.local();
    let end = a.local();
    a.ldx(imm(crate::rom::asm::byte(code.len() - 1)));
    a.bind(copy);
    a.lda(abs("chrget_code").x());
    a.sta(z(ws::zp::CHRGET).x());
    a.dex();
    a.bpl(copy);
    a.later(move |a| {
        a.here("chrget_code");
        a.bytes(&code);
    });
    a.bind(wait);
    a.lda(abs(COMMAND));
    a.beq(wait);
    a.tsx();
    a.stx(abs(SAVED_STACK));
    a.lda(imm(0));
    a.sta(abs(ERROR));
    a.lda(imm(lo(SCRIPT)));
    a.sta(z(STEP));
    a.lda(imm(hi(SCRIPT)));
    a.sta(z(STEP + 1));
    a.bind(step);
    a.ldy(imm(0));
    a.lda(z(STEP).ind_y());
    a.sta(z(VECTOR));
    a.iny();
    a.lda(z(STEP).ind_y());
    a.sta(z(VECTOR + 1));
    a.ora(z(VECTOR));
    a.beq(end);
    a.iny();
    a.lda(z(STEP).ind_y());
    a.tax();
    a.iny();
    a.lda(z(STEP).ind_y());
    a.tay();
    a.txa();
    a.jsr("call_vector");
    a.lda(z(STEP));
    a.clc();
    a.adc(imm(4));
    a.sta(z(STEP));
    a.jmp(step);
    a.bind(end);
    a.here("script_done");
    a.lda(imm(0));
    a.sta(abs(COMMAND));
    a.jmp(wait);
    a.here("call_vector");
    a.jmp(abs(VECTOR).ind());
}

fn helpers(a: &mut Asm) {
    for (name, number) in [
        ("illegal_quantity_error", ILLEGAL_QUANTITY),
        ("overflow_error", OVERFLOW),
        ("division_by_zero_error", DIVISION_BY_ZERO),
    ] {
        a.here(name);
        a.lda(imm(number));
        a.jmp("error_exit");
    }
    a.here("error");
    a.txa();
    a.here("error_exit");
    a.sta(abs(ERROR));
    a.ldx(abs(SAVED_STACK));
    a.txs();
    a.jmp("script_done");

    a.here("print_string");
    a.rts();

    // `text_to_fac` over the text at A (low) and Y.
    a.here("fin_at");
    a.sta(z(ws::zp::TXTPTR));
    a.sty(z(ws::zp::TXTPTR + 1));
    a.jmp("text_to_fac");

    // `fac_to_text`, its text copied to TEXT.
    a.here("fout_to_text");
    let copy = a.local();
    a.jsr("fac_to_text");
    a.ldy(imm(0));
    a.bind(copy);
    a.lda(abs(decimal::NUMBER_TEXT).y());
    a.sta(abs(TEXT).y());
    a.iny();
    a.cmp(imm(0));
    a.bne(copy);
    a.rts();
}

/// A machine running the test ROM, settled at its driver.
struct Rig {
    machine: Machine,
    image: Image,
}

impl Rig {
    fn new() -> Self {
        let image = rom();
        let system = system().expect("the BASIC and KERNAL ROMs assemble");
        let mut machine = machine(Standard::Pal, &image.bytes, system.kernal());
        machine.run_until(4 * Standard::Pal.clock());
        Self { machine, image }
    }

    fn routine(&self, name: &str) -> u16 {
        self.image
            .symbol(name)
            .unwrap_or_else(|| panic!("no routine {name}"))
    }

    fn poke(&mut self, at: u16, bytes: &[u8]) {
        self.machine.load_into_memory(&Program {
            address: at,
            bytes: Vec::from(bytes),
        });
    }

    /// Run `steps` — each a routine and the address it is handed — and
    /// return the error it stopped at, 0 for none.
    fn run(&mut self, steps: &[(&str, u16)]) -> u8 {
        let mut script = Vec::new();
        for &(name, argument) in steps {
            script.extend_from_slice(&self.routine(name).to_le_bytes());
            script.extend_from_slice(&argument.to_le_bytes());
        }
        script.extend_from_slice(&[0, 0, 0, 0]);
        self.poke(SCRIPT, &script);
        self.poke(COMMAND, &[1]);
        let end = self.machine.cycles() + 2 * Standard::Pal.clock();
        while self.machine.ram(COMMAND) != 0 {
            assert!(self.machine.cycles() < end, "the script ends");
            let until = self.machine.cycles() + 5000;
            self.machine.run_until(until);
        }
        self.machine.ram(ERROR)
    }

    fn real(&self, at: u16) -> [u8; 5] {
        core::array::from_fn(|offset| self.machine.ram(at + u16::try_from(offset).unwrap_or(0)))
    }

    fn text(&self) -> Vec<u8> {
        (TEXT..=u16::MAX)
            .map(|at| self.machine.ram(at))
            .take_while(|&byte| byte != 0)
            .collect()
    }
}

/// A packed real from ten hex digits.
fn real(hex: &str) -> [u8; 5] {
    core::array::from_fn(|index| {
        u8::from_str_radix(&hex[2 * index..2 * index + 2], 16).expect("hex")
    })
}

/// A result as compared: a zero's mantissa is whatever the arithmetic
/// left, which the original's and this ROM's differ in.
fn comparable(result: [u8; 5]) -> [u8; 5] {
    if result[0] == 0 {
        [0; 5]
    } else {
        result
    }
}

/// The outcome a row records: the result, or the error BASIC stops at.
fn outcome(rig: &Rig, error: u8) -> Result<[u8; 5], u8> {
    if error == 0 {
        Ok(comparable(rig.real(RESULT)))
    } else {
        Err(error)
    }
}

fn expected(recorded: &str) -> Result<[u8; 5], u8> {
    match recorded {
        "ERR14" => Err(ILLEGAL_QUANTITY),
        "ERR15" => Err(OVERFLOW),
        "ERR20" => Err(DIVISION_BY_ZERO),
        hex => Ok(comparable(real(hex))),
    }
}

/// The right operand: B alone, or the unrounded product B × G.
fn right_operand(g: &str) -> Vec<(&'static str, u16)> {
    if g.is_empty() {
        alloc::vec![("fac_load", Y)]
    } else {
        alloc::vec![("fac_load", Z), ("arg_load", Y), ("fmul", 0)]
    }
}

#[test]
fn sums_products_and_quotients_are_the_original_s() {
    let mut rig = Rig::new();
    let mut failures = Vec::new();
    for &(a, b, product, sum, quotient, difference) in corpus::ARITHMETIC {
        rig.poke(X, &real(a));
        rig.poke(Y, &real(b));
        for (operation, recorded) in [
            ("fmul", product),
            ("fadd", sum),
            ("fdiv", quotient),
            ("fsub", difference),
        ] {
            let error = rig.run(&[
                ("fac_load", Y),
                ("arg_load", X),
                (operation, 0),
                ("fac_store", RESULT),
            ]);
            if outcome(&rig, error) != expected(recorded) {
                failures.push((a, b, operation, recorded, outcome(&rig, error)));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} differ: {:02X?}",
        failures.len(),
        &failures[..failures.len().min(8)]
    );
}

#[test]
fn a_multiplier_s_runs_of_zero_bytes_shift_as_the_original_s_do() {
    let mut rig = Rig::new();
    let mut failures = Vec::new();
    for &(a, b, recorded) in corpus::PRODUCTS {
        rig.poke(X, &real(a));
        rig.poke(Y, &real(b));
        let error = rig.run(&[
            ("fac_load", Y),
            ("arg_load", X),
            ("fmul", 0),
            ("fac_store", RESULT),
        ]);
        if outcome(&rig, error) != expected(recorded) {
            failures.push((a, b, recorded, outcome(&rig, error)));
        }
    }
    assert!(
        failures.is_empty(),
        "{} differ: {:02X?}",
        failures.len(),
        &failures[..failures.len().min(8)]
    );
}

#[test]
fn unrounded_operands_take_part_as_the_original_s_do() {
    let mut rig = Rig::new();
    let mut failures = Vec::new();
    for &(a, b, g, recorded, shape) in corpus::CHAINS {
        rig.poke(X, &real(a));
        rig.poke(Y, &real(b));
        rig.poke(Z, &real(g));
        let steps: Vec<(&str, u16)> = match shape {
            // A × B pushed rounded, then op G: the push is a store.
            "A*B+G" => alloc::vec![
                ("fac_load", Y),
                ("arg_load", X),
                ("fmul", 0),
                ("fac_store", RESULT),
                ("fac_load", Z),
                ("arg_load", RESULT),
                ("fadd", 0)
            ],
            "A*B*G" => alloc::vec![
                ("fac_load", Y),
                ("arg_load", X),
                ("fmul", 0),
                ("fac_store", RESULT),
                ("fac_load", Z),
                ("arg_load", RESULT),
                ("fmul", 0)
            ],
            // A op (B × G): the product stays in FAC unrounded.
            "A*(B*G)" => alloc::vec![
                ("fac_load", Z),
                ("arg_load", Y),
                ("fmul", 0),
                ("arg_load", X),
                ("fmul", 0)
            ],
            "A+(B*G)" => alloc::vec![
                ("fac_load", Z),
                ("arg_load", Y),
                ("fmul", 0),
                ("arg_load", X),
                ("fadd", 0)
            ],
            "A/(B*G)" => alloc::vec![
                ("fac_load", Z),
                ("arg_load", Y),
                ("fmul", 0),
                ("arg_load", X),
                ("fdiv", 0)
            ],
            "A+B-G" => alloc::vec![
                ("fac_load", Y),
                ("arg_load", X),
                ("fadd", 0),
                ("fac_store", RESULT),
                ("fac_load", Z),
                ("arg_load", RESULT),
                ("fsub", 0)
            ],
            "A/B*G" => alloc::vec![
                ("fac_load", Y),
                ("arg_load", X),
                ("fdiv", 0),
                ("fac_store", RESULT),
                ("fac_load", Z),
                ("arg_load", RESULT),
                ("fmul", 0)
            ],
            "A-(B/G)" => alloc::vec![
                ("fac_load", Z),
                ("arg_load", Y),
                ("fdiv", 0),
                ("arg_load", X),
                ("fsub", 0)
            ],
            "A*(B+G)" => alloc::vec![
                ("fac_load", Z),
                ("arg_load", Y),
                ("fadd", 0),
                ("arg_load", X),
                ("fmul", 0)
            ],
            other => panic!("no shape {other}"),
        };
        let mut steps = steps;
        steps.push(("fac_store", RESULT));
        let error = rig.run(&steps);
        if outcome(&rig, error) != expected(recorded) {
            failures.push((a, b, g, shape, recorded, outcome(&rig, error)));
        }
    }
    assert!(
        failures.is_empty(),
        "{} differ: {:02X?}",
        failures.len(),
        &failures[..failures.len().min(8)]
    );
}

#[test]
fn functions_are_the_original_s() {
    let mut rig = Rig::new();
    let mut failures = Vec::new();
    for &(function, b, g, recorded) in corpus::FUNCTIONS {
        rig.poke(Y, &real(b));
        if !g.is_empty() {
            rig.poke(Z, &real(g));
        }
        let mut steps = right_operand(g);
        if function == "power" {
            // B ↑ G: B pushed, G evaluated.
            steps = alloc::vec![("fac_load", Z), ("arg_load", Y)];
        }
        steps.push((function, 0));
        steps.push(("fac_store", RESULT));
        let error = rig.run(&steps);
        if outcome(&rig, error) != expected(recorded) {
            failures.push((function, b, g, recorded, outcome(&rig, error)));
        }
    }
    assert!(
        failures.is_empty(),
        "{} differ: {:02X?}",
        failures.len(),
        &failures[..failures.len().min(8)]
    );
}

#[test]
fn rnd_steps_and_seeds_as_the_original_s() {
    let mut rig = Rig::new();
    let mut failures = Vec::new();
    for &(argument, seed, recorded) in corpus::RND {
        rig.poke(Y, &real(argument));
        rig.poke(ws::zp::RNDX, &real(seed));
        let error = rig.run(&[("fac_load", Y), ("fn_rnd", 0), ("fac_store", RESULT)]);
        let seed_now = rig.real(ws::zp::RNDX);
        if error != 0 || rig.real(RESULT) != real(recorded) || seed_now != real(recorded) {
            failures.push((argument, seed, recorded, rig.real(RESULT), seed_now));
        }
    }
    assert!(
        failures.is_empty(),
        "{} differ: {:02X?}",
        failures.len(),
        &failures[..failures.len().min(8)]
    );
}

#[test]
fn decimal_input_is_the_original_s() {
    let mut rig = Rig::new();
    let mut failures = Vec::new();
    for &(text, recorded) in corpus::VAL {
        let mut bytes = Vec::from(text.as_bytes());
        bytes.push(0);
        rig.poke(TEXT, &bytes);
        let error = rig.run(&[("fin_at", TEXT), ("fac_store", RESULT)]);
        if outcome(&rig, error) != expected(recorded) {
            failures.push((text, recorded, outcome(&rig, error)));
        }
    }
    assert!(
        failures.is_empty(),
        "{} differ: {:02X?}",
        failures.len(),
        &failures[..failures.len().min(8)]
    );
}

#[test]
fn str_shows_numbers_as_the_original_s() {
    let mut rig = Rig::new();
    let mut failures = Vec::new();
    for &(b, g, shown) in corpus::STR {
        rig.poke(Y, &real(b));
        if !g.is_empty() {
            rig.poke(Z, &real(g));
        }
        let mut steps = right_operand(g);
        steps.push(("fout_to_text", 0));
        let error = rig.run(&steps);
        let text = rig.text();
        if error != 0 || text != shown.as_bytes() {
            failures.push((
                b,
                g,
                shown,
                alloc::string::String::from_utf8_lossy(&text).into_owned(),
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} differ: {:?}",
        failures.len(),
        &failures[..failures.len().min(8)]
    );
}
