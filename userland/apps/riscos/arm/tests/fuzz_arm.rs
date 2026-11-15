//! Deterministic fuzz harness for the ARM processor: the decoder, the
//! encoder and the assembler's text over any word, and the interpreter over
//! any program, registers and modes.
//!
//! A guest's code is a stranger's bytes. The invariants:
//!
//! * decoding any word on any profile never panics; whatever decodes has an
//!   encoding that decodes back to it exactly, a text of bounded length,
//!   and an answer to whether it is UNPREDICTABLE;
//! * running any words from any state never panics, never leaves the
//!   processor in a mode its profile lacks, and leaves a state that loads
//!   back as itself.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

use tairix_fuzzseed::Prng;
use tairix_riscos_arm::a32::decode;
use tairix_riscos_arm::bus::{Abort, Bus, Fetch, Width};
use tairix_riscos_arm::cond::Condition;
use tairix_riscos_arm::cpu::{Cpu, State};
use tairix_riscos_arm::isa::{Feature, Isa};
use tairix_riscos_arm::profile::{Profile, BIT26, BIT32};
use tairix_riscos_arm::psr::{self, Mode};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 400;
/// Words decoded each iteration.
const WORDS: usize = 256;
/// Instructions each program runs.
const STEPS: u64 = 64;
/// The longest text an instruction has.
const LONGEST_TEXT: usize = 64;
/// Every mode a processor may be given.
const MODES: [Mode; 11] = [
    Mode::User26,
    Mode::Fiq26,
    Mode::Irq26,
    Mode::Supervisor26,
    Mode::User,
    Mode::Fiq,
    Mode::Irq,
    Mode::Supervisor,
    Mode::Abort,
    Mode::Undefined,
    Mode::System,
];

/// Four pages of words a program runs in and addresses: two of code and
/// data, one that refuses User writes, one unmapped; the rest of memory
/// unmapped. Any page may be a host page.
struct Memory {
    words: [u32; 3 * 1024],
    host: [bool; 3],
}

impl Memory {
    fn page(address: u32) -> Option<usize> {
        usize::try_from(address >> 12).ok().filter(|&page| page < 3)
    }

    fn index(address: u32) -> Option<usize> {
        Self::page(address).map(|_| (address as usize >> 2) & 0xFFF)
    }
}

impl Bus for Memory {
    fn fetch(&mut self, address: u32, _privileged: bool) -> Result<Fetch, Abort> {
        let page = Self::page(address).ok_or(Abort::Translation)?;
        let index = Self::index(address).ok_or(Abort::Translation)?;
        Ok(Fetch {
            word: self.words[index],
            host: self.host[page],
        })
    }

    fn read(&mut self, address: u32, width: Width, _privileged: bool) -> Result<u32, Abort> {
        let index = Self::index(address).ok_or(Abort::Translation)?;
        let word = self.words[index];
        let shift = 8 * (address & 3);
        Ok(match width {
            Width::Byte => (word >> shift) & 0xFF,
            Width::Half => (word >> shift) & 0xFFFF,
            Width::Word => word,
        })
    }

    fn write(
        &mut self,
        address: u32,
        width: Width,
        value: u32,
        privileged: bool,
    ) -> Result<(), Abort> {
        if Self::page(address) == Some(2) && !privileged {
            return Err(Abort::Permission);
        }
        let index = Self::index(address).ok_or(Abort::Translation)?;
        let shift = 8 * (address & 3);
        let mask = match width {
            Width::Byte => 0xFF << shift,
            Width::Half => 0xFFFF << shift,
            Width::Word => u32::MAX,
        };
        self.words[index] = (self.words[index] & !mask) | ((value << shift) & mask);
        Ok(())
    }
}

/// A word: anything, or a mutated encoding of a common shape.
fn drawn_word(rng: &mut Prng) -> u32 {
    match rng.below(4) {
        0 => rng.next_u32(),
        1 => (rng.next_u32() & 0x0FFF_FFFF) | 0xE000_0000,
        2 => (rng.next_u32() & 0x0FFF_FFFF) | 0xF000_0000,
        _ => {
            let shapes = [
                0xE59F_0000u32,
                0xE8BD_8000,
                0xE12F_FF10,
                0xEE10_0F10,
                0xE7F0_00F0,
                0xEF00_0000,
            ];
            *rng.pick(&shapes) ^ (rng.next_u32() & 0x000F_FFFF)
        }
    }
}

fn check_decoding(word: u32) {
    for isa in [BIT26.isa, BIT32.isa, Isa::ALL] {
        let Some(instruction) = decode(word, isa) else {
            continue;
        };
        let _ = instruction.unpredictable(isa);
        let text = format!("{}", instruction.text(word));
        assert!(
            !text.is_empty() && text.len() <= LONGEST_TEXT,
            "{word:08X}: {text}"
        );
        let check_isa = if instruction.cond == Condition::Nv && isa.has(Feature::ConditionNever) {
            Isa::ALL.with(Feature::ConditionNever)
        } else {
            Isa::ALL
        };
        if let Ok(encoded) = instruction.encode() {
            assert_eq!(
                decode(encoded, check_isa),
                Some(instruction),
                "{word:08X} -> {encoded:08X}"
            );
        } else {
            assert_ne!(isa, Isa::ALL, "{word:08X}: every word ALL decodes encodes");
        }
    }
}

fn drawn_state(rng: &mut Prng, profile: &Profile) -> State {
    let modes: Vec<Mode> = MODES
        .iter()
        .copied()
        .filter(|mode| !mode.is_26bit() || profile.modes26())
        .collect();
    let mode = *rng.pick(&modes);
    let flags = rng.next_u32() & (psr::NZCV | psr::Q | psr::GE | psr::E | psr::A | psr::I | psr::F);
    let mut state = State {
        pc: 4 * u32::try_from(rng.below(2048)).unwrap_or(0),
        cpsr: flags | mode.bits(),
        ..State::default()
    };
    let word = |rng: &mut Prng| match rng.below(3) {
        0 => rng.next_u32(),
        1 => 4 * u32::try_from(rng.below(3 * 1024)).unwrap_or(0),
        _ => u32::try_from(rng.below(64)).unwrap_or(0),
    };
    for value in state.user.iter_mut().chain(&mut state.fiq) {
        *value = word(rng);
    }
    for pair in &mut state.banked {
        *pair = [word(rng), word(rng)];
    }
    for spsr in &mut state.spsr {
        *spsr = rng.next_u32() & profile.psr;
    }
    state
}

fn check_running(rng: &mut Prng, profile: &'static Profile) {
    let mut memory = Memory {
        words: [0; 3 * 1024],
        host: [rng.below(4) == 0, rng.below(4) == 0, false],
    };
    for word in &mut memory.words {
        *word = drawn_word(rng);
    }
    let mut cpu = Cpu::new(profile, 2);
    let state = drawn_state(rng, profile);
    cpu.set_state(&state).expect("a mode the profile has");
    if rng.below(3) == 0 {
        cpu.set_irq(true);
    }
    for _ in 0..4 {
        let _ = cpu.run(&mut memory, STEPS);
        let mode = Mode::from_bits(cpu.cpsr()).expect("a valid mode");
        assert!(
            !mode.is_26bit() || profile.modes26(),
            "{mode:?} on {}",
            profile.name
        );
        assert_eq!(cpu.mode(), mode);
        let state = cpu.state();
        let mut again = Cpu::new(profile, 0);
        again.set_state(&state).expect("its own state loads");
        assert_eq!(again.state(), state);
    }
}

#[test]
fn any_word_decodes_encodes_and_runs_within_the_architecture() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_word_decodes_encodes_and_runs_within_the_architecture",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        for _ in 0..WORDS {
            check_decoding(drawn_word(&mut rng));
        }
        check_running(&mut rng, &BIT26);
        check_running(&mut rng, &BIT32);
        iteration += 1;
        match deadline {
            None if iteration >= SMOKE_ITERATIONS => break,
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            _ => {}
        }
    }
}
