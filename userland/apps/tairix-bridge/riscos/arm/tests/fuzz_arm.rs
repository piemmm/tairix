//! Deterministic fuzz harness for the ARM processor: the decoders, the
//! encoders and the assembler's text over any A32 word and any T32
//! halfwords at any place in an `IT` block, and the interpreter over any
//! program, registers, modes and state.
//!
//! A guest's code is a stranger's bytes. The invariants:
//!
//! * decoding any word or halfwords on any profile never panics; whatever
//!   decodes has an encoding that decodes back to it exactly, in a T32
//!   instruction's own width and in the narrowest, a text of bounded length,
//!   and an answer to whether it is UNPREDICTABLE;
//! * running any words from any state, ARM or Thumb, never panics, never
//!   leaves the processor in a mode its profile lacks, and leaves a state
//!   that loads back as itself.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

use tairix_fuzzseed::Prng;
use tairix_riscos_arm::a32::{self, decode};
use tairix_riscos_arm::bus::{Abort, Bus, Fetch, Width};
use tairix_riscos_arm::cond::Condition;
use tairix_riscos_arm::cpu::{Cpu, State};
use tairix_riscos_arm::instruction::Set;
use tairix_riscos_arm::isa::{Feature, Isa};
use tairix_riscos_arm::mmu::{
    Access, FrameKind, Mmu, Rights, Table, Watcher, GRANULE_SIZE, PAGE_BITS, PAGE_SIZE,
};
use tairix_riscos_arm::profile::{Profile, BIT26, BIT32};
use tairix_riscos_arm::psr::{self, Mode};
use tairix_riscos_arm::t32::{self, Encoding, Position, Size};

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
            page: address >> PAGE_BITS,
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
            Width::Word => u32::MAX,
            narrow => narrow.mask() << shift,
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
        let _ = a32::unpredictable(&instruction, isa);
        let text = format!("{}", instruction.text(Set::A32, word));
        assert!(
            !text.is_empty() && text.len() <= LONGEST_TEXT,
            "{word:08X}: {text}"
        );
        let check_isa = if instruction.cond == Condition::Nv && isa.has(Feature::ConditionNever) {
            Isa::ALL.with(Feature::ConditionNever)
        } else {
            Isa::ALL
        };
        if let Ok(encoded) = a32::encode(&instruction) {
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

/// A T32 instruction's halfwords, the first in the low half: anything, a
/// 32-bit one, or a mutated common shape of either width.
fn drawn_halfwords(rng: &mut Prng) -> u32 {
    match rng.below(4) {
        0 => rng.next_u32(),
        1 => {
            let prefix = *rng.pick(&[0xE800u32, 0xF000, 0xF800]);
            (rng.next_u32() & 0xFFFF_07FF) | prefix
        }
        2 => {
            let shapes = [0x4770u32, 0xBD00, 0xBF08, 0xDF00, 0xB500, 0x4687, 0xE7FE];
            (*rng.pick(&shapes) ^ (rng.next_u32() & 0xFF)) | (rng.next_u32() & 0xFFFF_0000)
        }
        _ => {
            let shapes = [
                0xF800_F000u32,
                0x8000_E8BD,
                0xF000_F8DF,
                0xF000_E8DF,
                0x8000_F3AF,
            ];
            *rng.pick(&shapes) ^ (rng.next_u32() & 0x0FFF_000F)
        }
    }
}

/// A place in an `IT` block: mostly outside one.
fn drawn_position(rng: &mut Prng) -> Position {
    let cond = Condition::field(rng.next_u32());
    match rng.below(4) {
        0 => Position::Inside(cond),
        1 => Position::Last(cond),
        _ => Position::Outside,
    }
}

fn halves(encoding: Encoding) -> (u16, u16) {
    match encoding {
        Encoding::Narrow(first) => (first, 0),
        Encoding::Wide(first, second) => (first, second),
    }
}

fn check_thumb_decoding(bits: u32, position: Position) {
    let [a, b, c, d] = bits.to_le_bytes();
    let (first, second) = (u16::from_le_bytes([a, b]), u16::from_le_bytes([c, d]));
    for isa in [BIT32.isa, Isa::ALL] {
        let Some(decoded) = t32::decode(first, second, isa, position) else {
            continue;
        };
        let text = format!("{}", decoded.instruction.text(Set::T32, bits));
        assert!(
            !text.is_empty() && text.len() <= LONGEST_TEXT,
            "{bits:08X}: {text}"
        );
        let own = if decoded.wide {
            Size::Wide
        } else {
            Size::Narrow
        };
        for size in [Size::Narrowest, own] {
            let encoded = t32::encode(&decoded.instruction, position, size)
                .unwrap_or_else(|e| panic!("{bits:08X} at {position:?}: {e:?}"));
            let (again_first, again_second) = halves(encoded);
            let again = t32::decode(again_first, again_second, Isa::ALL, position);
            assert_eq!(
                again.map(|again| again.instruction),
                Some(decoded.instruction),
                "{bits:08X} -> {encoded:04X?}"
            );
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
    // Thumb state, and any IT state, where the profile has them.
    let thumb = rng.next_u32() & (psr::T | psr::IT_LOW | psr::IT_HIGH) & profile.psr;
    let mut state = State {
        pc: 2 * u32::try_from(rng.below(4096)).unwrap_or(0),
        cpsr: flags | thumb | mode.bits(),
        ..State::default()
    };
    if thumb & psr::T == 0 {
        state.pc &= !3;
    }
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
    let thumb_code = rng.below(2) == 0;
    for word in &mut memory.words {
        *word = if thumb_code {
            drawn_halfwords(rng)
        } else {
            drawn_word(rng)
        };
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

/// A program run over the guest MMU: a page of code and data, a watched
/// one, one that refuses User writes and a host page, the rest unmapped.
/// A host page is never written, and every granule stored into in the
/// watched page is reported.
fn check_running_over_the_mmu(rng: &mut Prng, profile: &'static Profile) {
    let mut mmu = Mmu::new(16).expect("an MMU");
    let thumb_code = rng.below(2) == 0;
    let mut frames = Vec::new();
    for page in 0..4u32 {
        let kind = if page == 3 {
            FrameKind::Host
        } else {
            FrameKind::Memory
        };
        let frame = mmu.allocate(kind).expect("a frame");
        for word in mmu.words_mut(frame).expect("its words").iter_mut() {
            *word = if thumb_code {
                drawn_halfwords(rng)
            } else {
                drawn_word(rng)
            };
        }
        let (user, privileged) = match page {
            2 => (Rights::Read, Rights::ReadWrite),
            3 => (Rights::Read, Rights::Read),
            _ => (Rights::ReadWrite, Rights::ReadWrite),
        };
        let access = Access {
            user,
            privileged,
            execute: true,
        };
        mmu.map(Table::Global, page << PAGE_BITS, frame, access)
            .expect("mapped");
        mmu.release(frame).expect("held");
        frames.push(frame);
    }
    let (target, host) = (frames[1], frames[3]);
    let watcher = Watcher::new(0).expect("a watcher");
    mmu.watch(target, u16::MAX, watcher).expect("watched");
    let before = mmu.words(target).copied().expect("the watched page");
    let host_words = mmu.words(host).copied();
    let mut cpu = Cpu::new(profile, 2);
    cpu.set_state(&drawn_state(rng, profile))
        .expect("a mode the profile has");
    for _ in 0..4 {
        let _ = cpu.run(&mut mmu, STEPS);
    }
    assert_eq!(mmu.words(host).copied(), host_words, "a host page written");
    let mut reported = 0u16;
    mmu.take_written(watcher, |frame, granules| {
        if frame == target {
            reported |= granules;
        }
    });
    let after = mmu.words(target).copied().expect("the watched page");
    let per_granule = (GRANULE_SIZE / 4) as usize;
    for granule in 0..PAGE_SIZE / GRANULE_SIZE {
        let span = granule as usize * per_granule..(granule as usize + 1) * per_granule;
        if after[span.clone()] != before[span] {
            assert_ne!(
                reported & (1 << granule),
                0,
                "granule {granule} stored, unreported"
            );
        }
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
            let position = drawn_position(&mut rng);
            check_thumb_decoding(drawn_halfwords(&mut rng), position);
        }
        check_running(&mut rng, &BIT26);
        check_running(&mut rng, &BIT32);
        check_running_over_the_mmu(&mut rng, &BIT26);
        check_running_over_the_mmu(&mut rng, &BIT32);
        iteration += 1;
        match deadline {
            None if iteration >= SMOKE_ITERATIONS => break,
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            _ => {}
        }
    }
}
