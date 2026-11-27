//! Deterministic fuzz harness for the module chain: module images with random
//! headers, entries, service tables, decoding tables and squeezed data, linked
//! through `OS_Module`, then random reasons, service calls and module SWIs
//! performed on what is linked, under a host that stops endless work.
//!
//! The invariants, checked after every step:
//!
//! * nothing panics, and every refusal is a value;
//! * nothing runs for ever: the host's word stops it;
//! * the kernel still answers a SWI;
//! * the chain walks to its end, the `UtilityModule` first.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

#[path = "support/boot.rs"]
mod boot;
#[path = "support/still.rs"]
mod still;

use tairix_fuzzseed::Prng;
use tairix_riscos_arm::a32;
use tairix_riscos_arm::cond::Condition;
use tairix_riscos_arm::instruction::{
    DataOp, Immediate, Instruction, MoveOp, Op, Operand, Shift, Source,
};
use tairix_riscos_arm::reg::Reg;
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::os::{Fail, Kernel, Regs};
use tairix_riscos_kernel::swi::os::environment::CURRENT_ACTIVE_OBJECT;
use tairix_riscos_kernel::swi::os::{CHANGE_ENVIRONMENT, MODULE};
use tairix_riscos_kernel::swi::X;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_RUNS: u64 = 30;
/// Steps in one run.
const STEPS: usize = 60;
/// Where a run writes the names it gives `OS_Module`.
const NAMES: u32 = APPLICATION + 0x100;
/// Where a run writes a module image.
const IMAGE: u32 = APPLICATION + 0x4000;
/// The instructions a run of the guest is given.
const BUDGET: u64 = 20_000;
/// The SWI chunks images claim, most often one of a few.
const CHUNKS: [u32; 3] = [0x5_A5C0, 0x5_A600, 0x8_0040];
/// The services images list and runs issue, most often one of a few.
const SERVICES: [u32; 6] = [0x27, 0x4E, 0x5A, 0xB9, 0xDA, 0xDB];
/// The header word holding the offset of a module's title.
const TITLE: usize = 4;

fn encode(op: Op) -> u32 {
    match a32::encode(&Instruction {
        cond: Condition::Al,
        op,
    }) {
        Ok(word) => word,
        Err(error) => panic!("unencodable {op:?}: {error:?}"),
    }
}

fn immediate(value: u32) -> Operand {
    match Immediate::encode(value) {
        Some(imm) => Operand::Immediate(imm),
        None => panic!("{value:#X} is no immediate"),
    }
}

/// The few instructions an image's entries are made of.
struct Code {
    /// `MOV PC, R14`.
    ret: u32,
    /// `MOV R1, #0`: a service claimed.
    claim: u32,
    /// `ADD R0, PC, #4`: R0 at the word after the next two.
    error_at: u32,
    /// `MSR CPSR_f, #V`.
    overflow: u32,
}

impl Code {
    fn new() -> Self {
        let mov = |rd: Reg, operand| {
            encode(Op::Move {
                op: MoveOp::Mov,
                s: false,
                rd,
                operand,
            })
        };
        Self {
            ret: mov(
                Reg::PC,
                Operand::Register {
                    rm: Reg::LR,
                    shift: Shift::NONE,
                },
            ),
            claim: mov(Reg::field(1), immediate(0)),
            error_at: encode(Op::Data {
                op: DataOp::Add,
                s: false,
                rd: Reg::field(0),
                rn: Reg::PC,
                operand: immediate(4),
            }),
            overflow: encode(Op::StatusWrite {
                spsr: false,
                fields: 0b1000,
                source: Source::Immediate(Immediate::encode(1 << 28).expect("V")),
            }),
        }
    }
}

/// A module image under construction.
struct Image {
    bytes: Vec<u8>,
}

impl Image {
    fn at(&self) -> u32 {
        u32::try_from(self.bytes.len()).expect("small")
    }

    fn set(&mut self, word: usize, value: u32) {
        self.bytes[4 * word..4 * word + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn words(&mut self, words: &[u32]) -> u32 {
        let at = self.at();
        for word in words {
            self.bytes.extend_from_slice(&word.to_le_bytes());
        }
        at
    }

    fn text(&mut self, text: &[u8]) -> u32 {
        let at = self.at();
        self.bytes.extend_from_slice(text);
        self.bytes.push(0);
        self.bytes.resize(self.bytes.len().next_multiple_of(4), 0);
        at
    }

    /// An entry that returns, refuses with an error, claims, or is noise.
    fn entry(&mut self, rng: &mut Prng, code: &Code) -> u32 {
        match rng.below(5) {
            0 => 0,
            1 => self.words(&[code.ret]),
            2 => self.words(&[
                code.error_at,
                code.overflow,
                code.ret,
                0x1234,
                0x6F6E_2121,
                0,
            ]),
            3 => self.words(&[code.claim, code.ret]),
            _ => {
                let noise = [rng.next_u32(), rng.next_u32(), code.ret];
                self.words(&noise)
            }
        }
    }
}

/// A module image: one RISC OS would take, then sometimes mutated.
fn image(rng: &mut Prng, code: &Code, title: &[u8]) -> Vec<u8> {
    let mut image = Image { bytes: vec![0; 52] };
    let title_at = image.text(title);
    image.set(TITLE, title_at);
    let mut help = title.to_vec();
    help.extend_from_slice(b"\t\t1.00 (01 Jan 2000)");
    let help_at = image.text(&help);
    image.set(5, help_at);
    for field in [0, 1, 2] {
        let entry = image.entry(rng, code);
        image.set(field, entry);
    }
    match rng.below(3) {
        0 => {}
        1 => {
            let entry = image.entry(rng, code);
            image.set(3, entry);
        }
        _ => {
            let anchor = image.words(&[0]);
            let entry = image.words(&[0xE1A0_0000]);
            let handler = image.entry(rng, code);
            let flags = u32::from(rng.below(2) == 1);
            let table = image.words(&[flags, handler]);
            for _ in 0..rng.below(4) {
                image.words(&[*rng.pick(&SERVICES)]);
            }
            image.words(&[0]);
            image.bytes[anchor as usize..anchor as usize + 4].copy_from_slice(&table.to_le_bytes());
            image.set(3, entry);
        }
    }
    if rng.below(4) != 0 {
        image.set(7, *rng.pick(&CHUNKS));
        let handler = image.entry(rng, code);
        image.set(8, handler);
        if rng.below(2) == 0 {
            let table = image.at();
            image.bytes.extend_from_slice(b"Fuzz\0Zero\0One\0\0");
            image.bytes.resize(image.bytes.len().next_multiple_of(4), 0);
            image.set(9, table);
        }
    }
    if rng.below(4) != 0 {
        let flags = image.words(&[u32::from(rng.below(5) != 0)]);
        image.set(12, flags);
    }
    if rng.below(8) == 0 {
        let length = 20 + rng.below(40);
        let trailer = rng.noise(length);
        image.bytes.extend_from_slice(&trailer);
        let end = image.at();
        image.set(1, end | 0x8000_0000);
    }
    if rng.below(2) == 0 {
        rng.mutated(&image.bytes)
    } else {
        image.bytes
    }
}

/// A run's kernel, the titles it has linked, and whether control left a
/// call for somewhere the run must go.
struct Run {
    kernel: Kernel,
    titles: Vec<Vec<u8>>,
    code: Code,
    unwound: bool,
}

fn boot(rng: &mut Prng) -> Run {
    let (kernel, _) = still::boot(rng);
    Run {
        kernel,
        titles: vec![b"UtilityModule".to_vec()],
        code: Code::new(),
        unwound: false,
    }
}

/// A number below `n`.
fn below(rng: &mut Prng, n: usize) -> u32 {
    u32::try_from(rng.below(n)).unwrap_or(0)
}

/// Performs SWI `number` with `registers`, its outcome a value.
fn swi(run: &mut Run, number: u32, registers: [u32; 13]) -> Result<Regs, Fail> {
    let mut regs = Regs::new(registers);
    let result = run.kernel.os().perform(X | number, &mut regs);
    run.unwound |= matches!(result, Err(Fail::Unwind(_)));
    result.map(|()| regs)
}

/// Writes a name for `OS_Module` to give at `NAMES`: a title the run knows,
/// perhaps abbreviated or postfixed, or noise.
fn name(run: &mut Run, rng: &mut Prng) {
    let mut text = match rng.below(6) {
        0 => {
            let length = 1 + rng.below(12);
            rng.noise(length)
        }
        _ => rng.pick(&run.titles).clone(),
    };
    match rng.below(5) {
        0 => {
            text.truncate(1 + rng.below(text.len().max(1)));
            text.push(b'.');
        }
        1 | 2 => {
            text.push(b'%');
            text.extend_from_slice(rng.pick(&[&b"Base"[..], b"Two", b"x", b""]));
        }
        _ => {}
    }
    text.extend_from_slice(rng.pick(&[&b"\r"[..], b" tail\r", b"\0"]));
    let _ = run.kernel.os().write_bytes(NAMES, &text);
}

/// One step: a module linked, or something done with what is linked.
fn step(run: &mut Run, rng: &mut Prng) {
    match rng.below(10) {
        0..=2 => {
            let title = format!("Fz{}", rng.below(6)).into_bytes();
            let bytes = image(rng, &run.code, &title);
            run.titles.push(title);
            let _ = run.kernel.os().write_bytes(IMAGE, &bytes);
            let length = u32::try_from(bytes.len()).expect("small");
            let noise = rng.next_u32();
            let size = *rng.pick(&[length, length, length / 2, 0, 52, noise]);
            let _ = swi(run, MODULE, [11, IMAGE, size, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        }
        3 => {
            name(run, rng);
            let reason = *rng.pick(&[3, 4, 14, 16, 18]);
            let _ = swi(
                run,
                MODULE,
                [reason, NAMES, NAMES, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            );
        }
        4 => {
            name(run, rng);
            let _ = run.kernel.os().write_bytes(NAMES + 0x80, b"Renamed\r");
            let _ = swi(
                run,
                MODULE,
                [15, NAMES, NAMES + 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            );
        }
        5 => {
            let reason = *rng.pick(&[12, 22, 19, 20]);
            let (module, incarnation) = (below(rng, 8), below(rng, 3));
            let section = *rng.pick(&[u32::MAX, 0, 3]);
            let second = if reason < 19 { incarnation } else { section };
            let _ = swi(
                run,
                MODULE,
                [reason, module, second, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            );
        }
        6 => {
            let number = if rng.below(4) == 0 {
                rng.next_u32()
            } else {
                *rng.pick(&SERVICES)
            };
            let _ = swi(run, 0x30, [0, number, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        }
        7 => {
            let number = rng.pick(&CHUNKS) | below(rng, 64);
            let _ = swi(run, number, [0; 13]);
            let room = below(rng, 40);
            let _ = swi(
                run,
                0x38,
                [number, NAMES, room, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            );
        }
        8 => {
            let _ = run.kernel.os().write_bytes(
                NAMES,
                rng.pick(&[&b"Fuzz_One\0"[..], b"XFz1_3\0", b"Fuzz_99\0"]),
            );
            let _ = swi(run, 0x39, [0, NAMES, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        }
        _ => {
            let reason = *rng.pick(&[5, 6, 7, 8, 9, 13, 21, 24]);
            let mut registers = [0; 13];
            registers[0] = reason;
            for register in &mut registers[1..5] {
                let noise = rng.next_u32();
                *register = *rng.pick(&[0, 1, 32, 4096, noise, APPLICATION]);
            }
            let _ = swi(run, MODULE, registers);
        }
    }
    // A start entry, an error handler or an exit the step unwound to runs.
    if core::mem::take(&mut run.unwound) {
        let _ = run.kernel.run(BUDGET);
    }
}

/// The title the module whose code is at `code` gives in its header.
fn title(run: &mut Run, code: u32) -> Option<Vec<u8>> {
    let mut os = run.kernel.os();
    let offset = 4 * u32::try_from(TITLE).ok()?;
    let at = os.read_word(code.wrapping_add(offset)).ok()?;
    os.read_zstring(code.wrapping_add(at), 64).ok()
}

/// The kernel still answers, and its chain walks to an end with the
/// `UtilityModule` first, found by its place: a name written into
/// application space is gone once a claim of the RMA's takes that space.
fn still_sound(run: &mut Run, step: usize) {
    assert!(
        swi(run, 0x42, [0; 13]).is_ok(),
        "OS_ReadMonotonicTime after step {step}"
    );
    let first = swi(run, MODULE, [12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        .ok()
        .and_then(|regs| title(run, regs.r[3]));
    assert_eq!(
        first.as_deref(),
        Some(&b"UtilityModule"[..]),
        "the kernel first after step {step}"
    );
    let (mut module, mut incarnation) = (0, 0);
    for _ in 0..10_000 {
        let Ok(regs) = swi(
            run,
            MODULE,
            [12, module, incarnation, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        ) else {
            return;
        };
        (module, incarnation) = (regs.r[1], regs.r[2]);
    }
    panic!("the chain has no end after step {step}");
}

#[test]
fn any_module_image_and_any_reason_answer_as_values() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_module_image_and_any_reason_answer_as_values",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        let mut run = boot(&mut rng);
        for step_number in 0..STEPS {
            step(&mut run, &mut rng);
            still_sound(&mut run, step_number);
        }
        runs += 1;
    }
}

/// The soak's find: the RMA's claims, grown past the free pool, take all of
/// application space, where a name written for `OS_Module` would lie; the
/// kernel still answers and its chain stays sound.
#[test]
fn a_claim_taking_application_space_leaves_the_chain_sound() {
    let mut run = boot(&mut Prng::new(0));
    let kernel = swi(&mut run, MODULE, [12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        .expect("the kernel")
        .r[3];
    swi(
        &mut run,
        CHANGE_ENVIRONMENT,
        [
            CURRENT_ACTIVE_OBJECT,
            kernel,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ],
    )
    .expect("the kernel the current active object");
    while swi(
        &mut run,
        MODULE,
        [6, 0, 0, 0x1000, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .is_ok()
    {}
    assert!(
        run.kernel.os().read_byte(NAMES).is_err(),
        "application space taken"
    );
    still_sound(&mut run, 0);
}

/// The soak's find: a module whose finalisation is noise that calls on into
/// `*Modules` without end, reinitialised under a host that stops work at its
/// first word. The listing passes the host's word on rather than taking it
/// for the chain's end, so the call ends.
#[test]
fn a_finalisation_listing_modules_without_end_is_stopped() {
    let code = Code::new();
    let mut image = Image { bytes: vec![0; 52] };
    let title = image.text(b"Fz3");
    image.set(TITLE, title);
    let help = image.text(b"Fz3\t\t1.00 (01 Jan 2000)");
    image.set(5, help);
    let start = image.words(&[code.claim, code.ret]);
    let init = image.words(&[code.ret]);
    let finalise = image.words(&[0x7CFE_61CD, 0x68D5_6053, code.ret]);
    for (field, entry) in [(0, start), (1, init), (2, finalise)] {
        image.set(field, entry);
    }
    let flags = image.words(&[1]);
    image.set(12, flags);
    // The find was made on RISC OS 5's layout, which a boot takes two times
    // in three.
    let seed = (0..64)
        .find(|&seed| Prng::new(seed).below(3) != 0)
        .expect("a seed among the first sixty-four");
    let mut run = boot(&mut Prng::new(seed));
    // Both enumerations are the find's: without either, the finalisation's
    // noise never reaches `*Modules`.
    let _ = swi(&mut run, MODULE, [12, 13, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let _ = run.kernel.os().write_bytes(IMAGE, &image.bytes);
    let length = u32::try_from(image.bytes.len()).expect("small");
    swi(
        &mut run,
        MODULE,
        [11, IMAGE, length, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .expect("linked");
    let _ = swi(&mut run, MODULE, [12, 14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let _ = run.kernel.os().write_bytes(NAMES, b"Fz.\0");
    let _ = swi(
        &mut run,
        MODULE,
        [3, NAMES, NAMES, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    still_sound(&mut run, 0);
}
