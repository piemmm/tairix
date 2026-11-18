//! Deterministic fuzz harness for the FPA: every operation over any register
//! values, operands and FPSR, and FPA programs run on the processor from any
//! registers, mode, memory and FPA state.
//!
//! A guest's FPA state is a stranger's: `LFM` loads any words, and a session
//! restores contexts a guest saved. The invariants:
//!
//! * no operation, conversion, load or store of any values under any FPSR
//!   panics, and one stops only by the trap of an exception the FPSR
//!   enables, writing nothing then but cumulative flags;
//! * the FPSR keeps its system ID, its enables and its control bits;
//! * any FPA program run from any state never panics, stops for an FPA trap
//!   only where the FPSR enables it, and leaves a state that loads back.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

use tairix_fuzzseed::Prng;
use tairix_riscos_arm::bus::{Abort, Bus, Fetch, Width};
use tairix_riscos_arm::cpu::{Cpu, State, Stop};
use tairix_riscos_arm::fpa::{fpsr, Exception, Format, Fpa, Internal, Precision, Rounding};
use tairix_riscos_arm::instruction::{FpaCompareOp, FpaDyadicOp, FpaMonadicOp, FpaOperand};
use tairix_riscos_arm::mmu::PAGE_BITS;
use tairix_riscos_arm::profile::{Profile, BIT26, BIT32};
use tairix_riscos_arm::psr::Mode;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 300;
/// Instructions each program runs at most.
const STEPS: u64 = 64;

/// A register value: any words at all, a number of a precision's own form,
/// or one of the patterns the arithmetic treats specially.
fn value(rng: &mut Prng) -> Internal {
    let sign = rng.next_u32() & 1 << 31;
    let exponent = match rng.below(10) {
        0 => 0,
        1 => 0x7FFF,
        2 => 0x3F80,
        3 => 0x3C00,
        4 => 0x407F,
        5 => 0x43FF,
        6 => 0x3FFF + u32::try_from(rng.below(128)).unwrap_or(0) - 64,
        _ => rng.next_u32() & 0x7FFF,
    };
    let uncommon = if rng.below(3) == 0 { 1 << 30 } else { 0 };
    let (high, low) = match rng.below(6) {
        0 => (0, 0),
        1 => (0x8000_0000, 0),
        2 => (0xC000_0000, 0),
        3 => (rng.next_u32() & 0x7FFF_FFFF, rng.next_u32()),
        _ => (rng.next_u32() | 0x8000_0000, rng.next_u32()),
    };
    if rng.below(8) == 0 {
        return Internal::from_words([rng.next_u32(), rng.next_u32(), rng.next_u32()]);
    }
    Internal::from_words([sign | uncommon | exponent, high, low])
}

fn operand(rng: &mut Prng) -> FpaOperand {
    let n = u8::try_from(rng.below(8)).unwrap_or(0);
    if rng.below(4) == 0 {
        FpaOperand::Constant(n)
    } else {
        FpaOperand::Register(n)
    }
}

fn register(rng: &mut Prng) -> u8 {
    u8::try_from(rng.below(8)).unwrap_or(0)
}

fn drawn_fpa(rng: &mut Prng) -> Fpa {
    let mut fpa = Fpa::new();
    for n in 0..8 {
        fpa.set_register(n, value(rng));
    }
    fpa.write_fpsr(rng.next_u32());
    fpa
}

/// What an operation may change of the FPSR: its flags, which only gather.
fn check_fpsr(before: u32, after: u32) {
    assert_eq!(
        after & !fpsr::FLAGS,
        before & !fpsr::FLAGS,
        "only flags change"
    );
    assert_eq!(
        after & before & fpsr::FLAGS,
        before & fpsr::FLAGS,
        "flags gather"
    );
}

/// An outcome as the invariants have it: a trap only by an enabled
/// exception, whose own flag stays clear.
fn check_outcome(before: u32, fpa: &Fpa, outcome: Result<(), Exception>) {
    check_fpsr(before, fpa.fpsr());
    if let Err(exception) = outcome {
        assert_ne!(
            before & exception.enable(),
            0,
            "{exception:?} trapped unenabled"
        );
        assert_eq!(fpa.fpsr() & exception.flag() & !before, 0, "its flag set");
    }
}

fn check_operations(rng: &mut Prng) {
    let mut fpa = drawn_fpa(rng);
    let precision = *rng.pick(&Precision::ALL);
    let rounding = *rng.pick(&Rounding::ALL);
    let (fd, fn_, fm) = (register(rng), register(rng), operand(rng));
    for &op in FpaDyadicOp::ALL {
        let (before, old) = (fpa.fpsr(), fpa.register(fd));
        let outcome = fpa.dyadic(op, precision, rounding, fd, fn_, fm);
        if outcome.is_err() {
            assert_eq!(fpa.register(fd), old, "{op:?} wrote under its trap");
        }
        check_outcome(before, &fpa, outcome);
    }
    for &op in FpaMonadicOp::ALL {
        let (before, old) = (fpa.fpsr(), fpa.register(fd));
        let outcome = fpa.monadic(op, precision, rounding, fd, fm);
        if outcome.is_err() {
            assert_eq!(fpa.register(fd), old, "{op:?} wrote under its trap");
        }
        check_outcome(before, &fpa, outcome);
    }
    for &op in FpaCompareOp::ALL {
        let before = fpa.fpsr();
        let outcome = fpa.compare(op, fn_, fm).map(|flags| {
            assert_eq!(flags & 0x0FFF_FFFF, 0, "flags only in bits 31 to 28");
        });
        check_outcome(before, &fpa, outcome);
    }
    let before = fpa.fpsr();
    let outcome = fpa.fix(rounding, fm).map(drop);
    check_outcome(before, &fpa, outcome);
    let before = fpa.fpsr();
    let outcome = fpa.float(precision, rounding, fd, rng.next_u32());
    check_outcome(before, &fpa, outcome);
    for format in Format::ALL {
        let before = fpa.fpsr();
        let words = [
            rng.next_u32(),
            rng.next_u32(),
            rng.next_u32(),
            rng.next_u32(),
        ];
        let outcome = fpa.load(format, fd, words);
        check_outcome(before, &fpa, outcome);
        let before = fpa.fpsr();
        let outcome = fpa.store(format, fn_).map(|stored| {
            let used = fpa.transfer_words(format);
            assert!(stored[used..].iter().all(|&word| word == 0), "{format:?}");
        });
        check_outcome(before, &fpa, outcome);
    }
    let words = [rng.next_u32(), rng.next_u32(), rng.next_u32()];
    fpa.load_multiple(fd, words);
    let stored = fpa.store_multiple(fd);
    assert_eq!(stored[0] & 0x3FFF_8000, 0, "reserved bits cleared");
    assert_eq!(stored[1..], words[1..]);
}

/// Two pages of words: FPA instructions among others, and their data.
struct Memory {
    words: [u32; 2 * 1024],
}

impl Bus for Memory {
    fn fetch(&mut self, address: u32, _privileged: bool) -> Result<Fetch, Abort> {
        let word = self.read(address & !3, Width::Word, true)?;
        Ok(Fetch {
            word,
            host: false,
            page: address >> PAGE_BITS,
        })
    }

    fn read(&mut self, address: u32, width: Width, _privileged: bool) -> Result<u32, Abort> {
        let index = usize::try_from(address >> 2).map_err(|_| Abort::Translation)?;
        let word = *self.words.get(index).ok_or(Abort::Translation)?;
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
        _privileged: bool,
    ) -> Result<(), Abort> {
        let index = usize::try_from(address >> 2).map_err(|_| Abort::Translation)?;
        let slot = self.words.get_mut(index).ok_or(Abort::Translation)?;
        let shift = 8 * (address & 3);
        let mask = match width {
            Width::Word => u32::MAX,
            narrow => narrow.mask() << shift,
        };
        *slot = (*slot & !mask) | ((value << shift) & mask);
        Ok(())
    }
}

/// A word: mostly one of the FPA's classes, its fields drawn at random.
fn word(rng: &mut Prng) -> u32 {
    let cond = if rng.below(8) == 0 {
        rng.next_u32() & 0xF000_0000
    } else {
        0xE000_0000
    };
    let bits = rng.next_u32();
    match rng.below(5) {
        0 => cond | 0x0E00_0100 | bits & 0x00FF_F0EF,
        1 => cond | 0x0E00_0110 | bits & 0x00FF_F0EF,
        2 => cond | 0x0C00_0100 | bits & 0x01FF_F0FF,
        3 => cond | 0x0C00_0200 | bits & 0x01FF_F0FF,
        _ => bits,
    }
}

fn check_running(rng: &mut Prng, profile: &'static Profile) {
    let mut memory = Memory {
        words: [0; 2 * 1024],
    };
    for slot in &mut memory.words {
        *slot = if rng.below(2) == 0 {
            word(rng)
        } else {
            rng.next_u32()
        };
    }
    let modes: Vec<Mode> = [
        Mode::User26,
        Mode::Supervisor26,
        Mode::User,
        Mode::Fiq,
        Mode::Supervisor,
        Mode::Undefined,
        Mode::System,
    ]
    .into_iter()
    .filter(|mode| !mode.is_26bit() || profile.modes26())
    .collect();
    let mut state = State {
        pc: 4 * u32::try_from(rng.below(1024)).unwrap_or(0),
        cpsr: rng.next_u32() & 0xF000_0000 | rng.pick(&modes).bits(),
        ..State::default()
    };
    let address = |rng: &mut Prng| match rng.below(3) {
        0 => rng.next_u32(),
        _ => 4 * u32::try_from(rng.below(2 * 1024)).unwrap_or(0),
    };
    for slot in state.user.iter_mut().chain(&mut state.fiq) {
        *slot = address(rng);
    }
    for pair in &mut state.banked {
        *pair = [address(rng), address(rng)];
    }
    let mut cpu = Cpu::new(profile, 2);
    cpu.set_state(&state).expect("a mode the profile has");
    *cpu.fpa_mut() = drawn_fpa(rng);
    for _ in 0..4 {
        // A WFS among the words may change the enables; the trapping
        // instruction does not.
        if let Stop::FloatingPoint { exception } = cpu.run(&mut memory, STEPS).stop {
            let enables = cpu.fpa().fpsr();
            assert_ne!(
                enables & exception.enable(),
                0,
                "{exception:?} trapped unenabled"
            );
        }
        assert_eq!(
            cpu.fpa().fpsr() >> 24,
            fpsr::SYSTEM_ID >> 24,
            "the system ID"
        );
        let state = cpu.state();
        let mut again = Cpu::new(profile, 0);
        again.set_state(&state).expect("its own state loads");
        assert_eq!(again.state(), state);
    }
}

#[test]
fn any_fpa_operation_on_any_values_traps_only_where_enabled() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_fpa_operation_on_any_values_traps_only_where_enabled",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        for _ in 0..16 {
            check_operations(&mut rng);
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
