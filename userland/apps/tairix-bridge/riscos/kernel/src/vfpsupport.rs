//! VFPSupport 0.18's interface, as the ROOL wiki documents it: the contexts
//! the VFP and Advanced SIMD unit's state is kept in, switched now or on a
//! context's first instruction, the unit's features, the exception dump, the
//! fast entry points and the elementary functions.
//!
//! The unit is the 32-bit profile's processor's, whichever profile runs.
//! While a context is active its state is the unit's, and CPACR grants the
//! unit to the modes the context was created for. With no context active,
//! or one awaiting lazy activation, CPACR grants it to none, so the unit's
//! next instruction takes the undefined instruction exception VFPSupport
//! claims: the context awaiting is activated and the instruction run again.
//! Each of the session's contexts has its own active context, and a switch
//! between them saves and lets go of the unit while the area it saves to is
//! still mapped, so a context's next instruction activates its own.
//!
//! A context's layout is VFPSupport's own, as the wiki allows, and
//! `ExamineContext`'s descriptor gives it: a mark, its flags and register
//! count, the FPSCR, FPEXC, then each doubleword register low word first.

use alloc::vec::Vec;

use tairix_riscos_arm::a32;
use tairix_riscos_arm::instruction::{BranchOp, Instruction, Op, SimdFp, VfpMonadicOp};
use tairix_riscos_arm::isa::{Feature, Isa};
use tairix_riscos_arm::psr;
use tairix_riscos_arm::reg::Reg;
use tairix_riscos_arm::t32::{self, Position};
use tairix_riscos_arm::vfp::{fpexc, fpscr, Access, System, Unit};
use tairix_softfloat::{Binary, Ieee, Outcome};

use crate::error::BAD_PARAMETERS;
use crate::kheap::UNKNOWN_REASON;
use crate::memory::MemoryError;
use crate::module::{instruction, routine_branch, Builtin, Code, Swis, SWI_OUT_OF_RANGE};
use crate::os::{Fail, Os, Regs, Unwind};
use crate::swi::chunk::VFP_SUPPORT as CHUNK;

/// VFPSupport's version, in hundredths: the first whose elementary functions
/// are whole.
const VERSION: u32 = crate::module::inventory::VFP_SUPPORT_ROM.version as u32;

/// The context pointer naming none, the null context.
pub(crate) const NO_CONTEXT: u32 = 0;

/// What marks a context's first word.
const MARK: u32 = u32::from_le_bytes(*b"VFPC");

/// Where a context keeps each part: its header, the mark, flags and register
/// count, from the start.
mod at {
    pub(super) const MARK: u32 = 0;
    pub(super) const FPSCR: u32 = 12;
    pub(super) const FPEXC: u32 = 16;
    pub(super) const DUMP: u32 = 20;
}

/// The header's words.
const HEADER_WORDS: usize = 3;

/// The flags a context keeps, at the places `ExamineContext` answers them.
mod kept {
    /// User mode may use the unit while the context is active.
    pub(super) const USER: u32 = 1;
    /// The context lies in application space.
    pub(super) const APPLICATION: u32 = 1 << 1;
    /// VFPSupport claimed its memory.
    pub(super) const OWNED: u32 = 1 << 31;
    pub(super) const ALL: u32 = USER | APPLICATION | OWNED;
}

/// `CreateContext`'s flags beside the kept ones, which `CheckContext`
/// ignores.
mod create {
    /// Activate it on the unit's next instruction, over [`NOW`].
    pub(super) const LAZILY: u32 = 1 << 30;
    /// Activate it now.
    pub(super) const NOW: u32 = 1 << 31;
}

/// `ChangeContext`'s flags.
mod change {
    pub(super) const LAZILY: u32 = 1;
    /// Application space is about to change: a context there is saved now.
    pub(super) const APPLICATION_CHANGING: u32 = 1 << 1;
}

/// `ExamineContext`'s flags in and out.
mod examined {
    /// In: the unit's state saved into the context's area first.
    pub(super) const SERIALISE: u32 = 1;
    /// Out: the context awaits lazy activation.
    pub(super) const AWAITING: u32 = 1 << 29;
    /// Out: the context's status registers are the unit's.
    pub(super) const LIVE: u32 = 1 << 30;
}

/// `ExceptionDump`'s flags, handled in this order: create, the two gets,
/// clear.
mod dump {
    pub(super) const COPY: u32 = 1;
    pub(super) const ORIGINAL: u32 = 1 << 1;
    pub(super) const CLEAR: u32 = 1 << 2;
    pub(super) const CREATE: u32 = 1 << 3;
    pub(super) const ALL: u32 = COPY | ORIGINAL | CLEAR | CREATE;
}

/// The FPSCR's exception trap enables, `Features` 1's answer being those the
/// unit can set.
const TRAP_ENABLES: u32 = 0x9F00;

/// The most words a context's state takes after its header: the FPSCR,
/// FPEXC and 32 doublewords.
const STATE_WORDS: usize = 2 + 2 * 32;

/// `ExamineContext`'s descriptor: the FPSCR, FPEXC and register dump field
/// identifiers at their offsets, then the end.
const DESCRIPTOR_WORDS: u32 = 4;
const DESCRIPTOR: [u32; DESCRIPTOR_WORDS as usize] = [
    at::FPSCR << 16,
    at::FPEXC << 16 | 1,
    at::DUMP << 16 | 5,
    u32::MAX,
];

/// An elementary function: the operation on one or two operands, or the
/// fused multiply-add on three.
#[derive(Clone, Copy)]
enum Function {
    Monadic(fn(Ieee, u64) -> Outcome),
    Dyadic(fn(Ieee, u64, u64) -> Outcome),
    FusedMultiplyAdd,
}

/// The elementary functions in their branch tables' order: sin, cos, tan,
/// asin, acos, atan, atan2, log, log10, exp, pow, fma.
const FUNCTION_COUNT: u32 = 12;
const FUNCTIONS: [Function; FUNCTION_COUNT as usize] = [
    Function::Monadic(Ieee::sin),
    Function::Monadic(Ieee::cos),
    Function::Monadic(Ieee::tan),
    Function::Monadic(Ieee::asin),
    Function::Monadic(Ieee::acos),
    Function::Monadic(Ieee::atan),
    Function::Dyadic(Ieee::atan2),
    Function::Monadic(Ieee::log),
    Function::Monadic(Ieee::log10),
    Function::Monadic(Ieee::exp),
    Function::Dyadic(Ieee::pow),
    Function::FusedMultiplyAdd,
];

/// The routines' places: the claimed vector's, the fast entry points', then
/// each function single precision and then double.
mod routine {
    pub(super) const UNDEFINED: usize = 0;
    pub(super) const FAST: usize = 1;
    pub(super) const SINGLE: usize = 5;
}

/// The guest code's parts: the descriptor, the single and the double
/// precision branch tables, then a stub for each table entry.
const DESCRIPTOR_AT: u32 = 0;
const SINGLE_TABLE_AT: u32 = DESCRIPTOR_AT + 4 * DESCRIPTOR_WORDS;
const DOUBLE_TABLE_AT: u32 = SINGLE_TABLE_AT + 4 * FUNCTION_COUNT;
const STUBS_AT: u32 = DOUBLE_TABLE_AT + 4 * FUNCTION_COUNT;

/// A context's header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Header {
    flags: u32,
    registers: u32,
}

impl Header {
    /// The header these words are, if they are one of a context holding at
    /// most `most` registers.
    fn of([mark, flags, registers]: [u32; HEADER_WORDS], most: u32) -> Option<Self> {
        (mark == MARK && flags & !kept::ALL == 0 && (1..=most).contains(&registers))
            .then_some(Self { flags, registers })
    }

    /// What CPACR grants while the context is active.
    const fn access(self) -> Access {
        if self.flags & kept::USER != 0 {
            Access::Full
        } else {
            Access::Privileged
        }
    }
}

/// VFPSupport's state.
pub(crate) struct State {
    /// The undefined instruction vector's owner before VFPSupport claimed
    /// it; none while it holds no claim.
    passes_to: Option<u32>,
    /// The current context's context active, or awaiting lazy activation.
    pub(crate) active: u32,
    /// The context whose state the unit holds, and its header.
    loaded: Option<(u32, Header)>,
    /// The exception dump: the copy kept in the RMA, and the context it was
    /// taken of.
    dump: Option<(u32, u32)>,
}

impl State {
    pub(crate) const fn new() -> Self {
        Self {
            passes_to: None,
            active: NO_CONTEXT,
            loaded: None,
            dump: None,
        }
    }

    fn holds(&self, context: u32) -> bool {
        self.loaded.is_some_and(|(held, _)| held == context)
    }

    fn awaiting(&self) -> bool {
        self.active != NO_CONTEXT && !self.holds(self.active)
    }
}

const ROUTINES: [Code; routine::SINGLE + 2 * FUNCTIONS.len()] = [
    undefined,
    check_context,
    create_context,
    destroy_context,
    change_context,
    elementary::<0, false>,
    elementary::<1, false>,
    elementary::<2, false>,
    elementary::<3, false>,
    elementary::<4, false>,
    elementary::<5, false>,
    elementary::<6, false>,
    elementary::<7, false>,
    elementary::<8, false>,
    elementary::<9, false>,
    elementary::<10, false>,
    elementary::<11, false>,
    elementary::<0, true>,
    elementary::<1, true>,
    elementary::<2, true>,
    elementary::<3, true>,
    elementary::<4, true>,
    elementary::<5, true>,
    elementary::<6, true>,
    elementary::<7, true>,
    elementary::<8, true>,
    elementary::<9, true>,
    elementary::<10, true>,
    elementary::<11, true>,
];

/// VFPSupport.
pub(crate) static VFP_SUPPORT: Builtin = Builtin {
    title: "VFPSupport",
    help_name: None,
    help_suffix: None,
    bridge: false,
    swis: Some(Swis {
        chunk: CHUNK,
        prefix: "VFPSupport",
        names: &[
            "CheckContext",
            "CreateContext",
            "DestroyContext",
            "ChangeContext",
            "ExamineContext",
            "FastAPI",
            "ActiveContext",
            "Version",
            "Features",
            "ExceptionDump",
            "ElementaryFunctions",
        ],
        handler: swi,
    }),
    services: None,
    commands: &[],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &ROUTINES,
    vectors: &[],
    resources: &[],
    code: Some(emit),
};

fn swi(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    match offset {
        0 => check_context(os, regs),
        1 => create_context(os, regs),
        2 => destroy_context(os, regs),
        3 => change_context(os, regs),
        4 => examine_context(os, regs),
        5 => fast_api(os, regs),
        6 => {
            regs.r[0] = os.world.vfp_support.active;
            Ok(())
        }
        7 => {
            regs.r[0] = VERSION;
            Ok(())
        }
        8 => features(os, regs),
        9 => exception_dump(os, regs),
        10 => elementary_functions(os, regs),
        _ => Err(os.error_with(SWI_OUT_OF_RANGE, &[VFP_SUPPORT.title.as_bytes()])),
    }
}

/// The unit's instruction set.
fn isa(os: &mut Os<'_>) -> Isa {
    os.world.processor.with_vfp().profile().isa
}

/// The doubleword registers the unit has, from MVFR0's register file.
fn registers_most(os: &mut Os<'_>) -> u32 {
    match System::Mvfr0.identity(isa(os)) & 0xF {
        2 => 32,
        1 => 16,
        _ => 0,
    }
}

const fn context_bytes(registers: u32) -> u32 {
    at::DUMP + 8 * registers
}

/// The bytes a context of `registers` doublewords created with `flags`
/// takes, where the unit can have one.
fn checked(os: &mut Os<'_>, flags: u32, registers: u32) -> Result<u32, Fail> {
    let creatable = flags & !(kept::USER | kept::APPLICATION) == 0;
    if !creatable || !(1..=registers_most(os)).contains(&registers) {
        return Err(os.error(BAD_PARAMETERS));
    }
    Ok(context_bytes(registers))
}

/// The header of the context at `context`, refused where none lies there.
fn header(os: &mut Os<'_>, context: u32) -> Result<Header, Fail> {
    let most = registers_most(os);
    if !context.is_multiple_of(4) {
        return Err(os.error(BAD_PARAMETERS));
    }
    let mut words = [0; HEADER_WORDS];
    os.read_words(context, &mut words)?;
    Header::of(words, most).ok_or_else(|| os.error(BAD_PARAMETERS))
}

/// A doubleword as a context's dump keeps it: its low word first.
fn halves(d: u64) -> [u32; 2] {
    let bytes = d.to_le_bytes();
    let (words, _) = bytes.as_chunks::<4>();
    [u32::from_le_bytes(words[0]), u32::from_le_bytes(words[1])]
}

const fn doubleword([low, high]: [u32; 2]) -> u64 {
    (high as u64) << 32 | low as u64
}

/// A context's state after its header: its FPSCR, FPEXC and dump, the first
/// `len` of `words`.
struct Kept {
    words: [u32; STATE_WORDS],
    len: usize,
}

impl Kept {
    /// Room for a context of `registers` doublewords, which a header holds
    /// to the unit's 32 at most.
    const fn of(registers: u32) -> Self {
        Self {
            words: [0; STATE_WORDS],
            len: 2 + 2 * registers as usize,
        }
    }

    fn words(&self) -> &[u32] {
        &self.words[..self.len]
    }

    /// The registers' words, each doubleword's two.
    fn dump(&mut self) -> &mut [[u32; 2]] {
        self.words[2..self.len].as_chunks_mut::<2>().0
    }
}

/// The unit's state as a context of `registers` doublewords keeps it.
fn unit_state(os: &mut Os<'_>, registers: u32) -> Kept {
    let vfp = os.world.processor.with_vfp().vfp();
    let mut kept = Kept::of(registers);
    kept.words[..2].copy_from_slice(&[vfp.fpscr(), vfp.fpexc()]);
    for (n, pair) in (0u8..).zip(kept.dump()) {
        *pair = halves(vfp.d(n));
    }
    kept
}

/// The state the area of the context at `context` keeps.
fn area_state(os: &mut Os<'_>, context: u32, header: Header) -> Result<Kept, Fail> {
    let mut kept = Kept::of(header.registers);
    os.read_words(context + at::FPSCR, &mut kept.words[..kept.len])?;
    Ok(kept)
}

/// The state of the context at `context`: the unit's where it holds it.
fn state_of(os: &mut Os<'_>, context: u32, header: Header) -> Result<Kept, Fail> {
    if os.world.vfp_support.holds(context) {
        return Ok(unit_state(os, header.registers));
    }
    area_state(os, context, header)
}

/// The unit's state kept in the context at `context`, if one still lies
/// there: memory that is no longer a context is not written.
fn save(os: &mut Os<'_>, context: u32, header: Header) -> Result<(), Fail> {
    if os.read_word(context + at::MARK)? != MARK {
        return Ok(());
    }
    let kept = unit_state(os, header.registers);
    os.write_words(context + at::FPSCR, kept.words())
}

/// The unit loaded from the context at `context`, and enabled: its status
/// registers, and its data registers only where its FPEXC says they were
/// saved. The area is read whole before the unit changes.
fn load(os: &mut Os<'_>, context: u32, header: Header) -> Result<(), Fail> {
    let mut kept = area_state(os, context, header)?;
    let [kept_fpscr, kept_fpexc] = [kept.words[0], kept.words[1]];
    let vfp = os.world.processor.with_vfp().vfp_mut();
    vfp.set_fpscr(kept_fpscr);
    if kept_fpexc & fpexc::EN != 0 {
        for (n, pair) in (0u8..).zip(kept.dump()) {
            vfp.set_d(n, doubleword(*pair));
        }
    }
    vfp.set_fpexc(fpexc::EN);
    Ok(())
}

fn grant(os: &mut Os<'_>, access: Access) {
    os.world.processor.with_vfp().grant_simdfp(access);
}

/// The context at `context` given the unit now: the one the unit holds
/// saved first, then this one loaded unless the unit holds it already.
fn switch_to(os: &mut Os<'_>, context: u32, header: Header) -> Result<(), Fail> {
    if !os.world.vfp_support.holds(context) {
        // Unloaded first, so a load that fails leaves the unit denied to
        // all rather than granted with no context in it.
        unload(os)?;
        load(os, context, header)?;
        os.world.vfp_support.loaded = Some((context, header));
    }
    grant(os, header.access());
    os.world.vfp_support.active = context;
    Ok(())
}

/// The context the unit holds saved into its area, and the unit let go of
/// it: its next instruction finds the unit denied.
fn unload(os: &mut Os<'_>) -> Result<(), Fail> {
    if let Some((held, header)) = os.world.vfp_support.loaded {
        save(os, held, header)?;
        os.world.vfp_support.loaded = None;
    }
    grant(os, Access::Denied);
    Ok(())
}

/// The unit let go of as the current context is set aside: the context it
/// holds saved while its area is mapped, then the unit denied. An area that
/// refuses the save loses only what it would have kept.
pub(crate) fn release_unit(os: &mut Os<'_>) {
    if let Some((held, header)) = os.world.vfp_support.loaded.take() {
        let _ = save(os, held, header);
    }
    grant(os, Access::Denied);
}

/// The context at `context` made active, now or on the unit's next
/// instruction where `lazily`: the context active before. One whose area is
/// not yet mapped, as application space changes, is checked only as it is
/// activated.
fn change(os: &mut Os<'_>, context: u32, lazily: bool, unmapped: bool) -> Result<u32, Fail> {
    let previous = os.world.vfp_support.active;
    if context == NO_CONTEXT {
        if lazily {
            grant(os, Access::Denied);
        } else {
            unload(os)?;
        }
        os.world.vfp_support.active = NO_CONTEXT;
        return Ok(previous);
    }
    if lazily && unmapped {
        grant(os, Access::Denied);
        os.world.vfp_support.active = context;
        return Ok(previous);
    }
    let header = header(os, context)?;
    if lazily && !os.world.vfp_support.holds(context) {
        grant(os, Access::Denied);
        os.world.vfp_support.active = context;
    } else {
        switch_to(os, context, header)?;
    }
    Ok(previous)
}

/// `VFPSupport_CheckContext`.
fn check_context(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = checked(os, regs.r[0] & !(create::LAZILY | create::NOW), regs.r[1])?;
    Ok(())
}

/// `VFPSupport_CreateContext`.
fn create_context(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let [flags, registers, buffer, initial] = [regs.r[0], regs.r[1], regs.r[2], regs.r[3]];
    let kept_flags = flags & !(create::LAZILY | create::NOW);
    let bytes = checked(os, kept_flags, registers)?;
    let owned = buffer == 0;
    if owned && kept_flags & kept::APPLICATION != 0 || buffer % 4 != 0 {
        return Err(os.error(BAD_PARAMETERS));
    }
    let context = if owned { os.rma_claim(bytes)? } else { buffer };
    let ownership = if owned { kept::OWNED } else { 0 };
    let header = [
        MARK,
        kept_flags | ownership,
        registers,
        initial & fpscr::WRITABLE,
        0,
    ];
    let activation = match (flags & create::LAZILY != 0, flags & create::NOW != 0) {
        (true, _) => Some(true),
        (false, true) => Some(false),
        (false, false) => None,
    };
    let made = os.write_words(context, &header).and_then(|()| {
        activation.map_or(Ok(regs.r[1]), |lazily| change(os, context, lazily, false))
    });
    match made {
        Ok(previous) => {
            regs.r[0] = context;
            regs.r[1] = previous;
            Ok(())
        }
        Err(fail) => {
            if owned {
                os.rma_free(context)?;
            }
            Err(fail)
        }
    }
}

/// `VFPSupport_DestroyContext`.
fn destroy_context(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (context, replacement) = (regs.r[0], regs.r[1]);
    let destroyed = header(os, context)?;
    // The replacement in first: one that cannot be loaded leaves the
    // context active, its registers saved, and destroys nothing.
    if os.world.vfp_support.active == context {
        change(os, replacement, false, false)?;
    }
    if os.world.vfp_support.holds(context) {
        os.world.vfp_support.loaded = None;
        grant(os, Access::Denied);
    }
    os.write_word(context + at::MARK, 0)?;
    if destroyed.flags & kept::OWNED != 0 {
        os.rma_free(context)?;
    }
    regs.r[0] = os.world.vfp_support.active;
    Ok(())
}

/// `VFPSupport_ChangeContext`.
fn change_context(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let flags = regs.r[1];
    if flags & !(change::LAZILY | change::APPLICATION_CHANGING) != 0 {
        return Err(os.error(BAD_PARAMETERS));
    }
    let changing = flags & change::APPLICATION_CHANGING != 0;
    if changing {
        let held_there = os
            .world
            .vfp_support
            .loaded
            .is_some_and(|(_, header)| header.flags & kept::APPLICATION != 0);
        if held_there {
            unload(os)?;
        }
    }
    regs.r[0] = change(os, regs.r[0], flags & change::LAZILY != 0, changing)?;
    Ok(())
}

/// `VFPSupport_ExamineContext`: serialising leaves the context's state in
/// its area alone, so what is written there is what its next activation
/// loads.
fn examine_context(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (context, flags) = (regs.r[0], regs.r[1]);
    if flags & !examined::SERIALISE != 0 {
        return Err(os.error(BAD_PARAMETERS));
    }
    let header = header(os, context)?;
    if flags & examined::SERIALISE != 0 && os.world.vfp_support.holds(context) {
        unload(os)?;
    }
    let state = &os.world.vfp_support;
    let live = state.holds(context);
    let awaiting = state.active == context && !live;
    let descriptor = os
        .guest_code_of(&VFP_SUPPORT)
        .ok_or_else(|| os.error(BAD_PARAMETERS))?;
    regs.r[0] = header.flags
        | if awaiting { examined::AWAITING } else { 0 }
        | if live { examined::LIVE } else { 0 };
    regs.r[1] = header.registers;
    regs.r[2] = if live {
        u32::MAX >> (32 - header.registers)
    } else {
        0
    };
    regs.r[3] = descriptor + DESCRIPTOR_AT;
    regs.r[4] = context_bytes(header.registers);
    Ok(())
}

/// `VFPSupport_FastAPI`: the routines need no workspace, so the pointer
/// for R12 is nought.
fn fast_api(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    for (n, slot) in regs.r[1..5].iter_mut().enumerate() {
        *slot = os
            .routine_of(&VFP_SUPPORT, routine::FAST + n)
            .ok_or_else(|| os.error(BAD_PARAMETERS))?;
    }
    regs.r[0] = 0;
    Ok(())
}

/// `VFPSupport_Features`.
fn features(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let isa = isa(os);
    match regs.r[0] {
        0 => {
            regs.r[0] = System::Fpsid.identity(isa);
            regs.r[1] = System::Mvfr0.identity(isa);
            regs.r[2] = System::Mvfr1.identity(isa);
        }
        1 => regs.r[0] = fpscr::WRITABLE & TRAP_ENABLES,
        // MVFR0's short vectors field: the unit executes them itself.
        2 => regs.r[0] = u32::from((System::Mvfr0.identity(isa) >> 24) & 0xF != 0),
        3 => {
            regs.r[0] = System::Mvfr2.identity(isa);
            regs.r[1..4].fill(0);
        }
        _ => return Err(os.error(UNKNOWN_REASON)),
    }
    Ok(())
}

/// A context in the RMA holding `kept`, which a caller frees: the dump and
/// its copies. Its user-mode flag is the one it was taken of.
fn copied(os: &mut Os<'_>, of: Header, kept: &Kept) -> Result<u32, Fail> {
    let copy = os.rma_claim(context_bytes(of.registers))?;
    let made = os
        .write_words(copy, &[MARK, of.flags & kept::USER, of.registers])
        .and_then(|()| os.write_words(copy + at::FPSCR, kept.words()));
    if let Err(fail) = made {
        os.rma_free(copy)?;
        return Err(fail);
    }
    Ok(copy)
}

/// A copy of the context at `context`, as it is now.
fn copy_of(os: &mut Os<'_>, context: u32) -> Result<u32, Fail> {
    let header = header(os, context)?;
    let kept = state_of(os, context, header)?;
    copied(os, header, &kept)
}

fn clear_dump(os: &mut Os<'_>) -> Result<(), Fail> {
    if let Some((copy, _)) = os.world.vfp_support.dump.take() {
        os.rma_free(copy)?;
    }
    Ok(())
}

/// `VFPSupport_ExceptionDump`. The unit traps no exception, so the dump
/// holds only what a caller made.
fn exception_dump(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let flags = regs.r[0];
    if flags & !dump::ALL != 0 {
        return Err(os.error(BAD_PARAMETERS));
    }
    if flags & dump::CREATE != 0 {
        let of = regs.r[1];
        let made = if of == NO_CONTEXT {
            None
        } else {
            Some((copy_of(os, of)?, of))
        };
        if let Err(fail) = clear_dump(os) {
            if let Some((copy, _)) = made {
                os.rma_free(copy)?;
            }
            return Err(fail);
        }
        os.world.vfp_support.dump = made;
    }
    let held = os.world.vfp_support.dump;
    let copy = match held {
        _ if flags & dump::COPY == 0 => None,
        Some((block, _)) => Some(copy_of(os, block)),
        None => Some(Ok(0)),
    };
    if flags & dump::CLEAR != 0 {
        if let Err(fail) = clear_dump(os) {
            if let Some(Ok(made)) = copy {
                if made != 0 {
                    os.free_rma_quietly(made);
                }
            }
            return Err(fail);
        }
    }
    if let Some(copy) = copy {
        regs.r[0] = copy?;
    }
    if flags & dump::ORIGINAL != 0 {
        regs.r[1] = held.map_or(0, |(_, of)| of);
    }
    Ok(())
}

/// `VFPSupport_ElementaryFunctions`.
fn elementary_functions(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[0] != 0 {
        return Err(os.error(BAD_PARAMETERS));
    }
    let code = os
        .guest_code_of(&VFP_SUPPORT)
        .ok_or_else(|| os.error(BAD_PARAMETERS))?;
    regs.r[0] = FUNCTION_COUNT;
    regs.r[1] = code + SINGLE_TABLE_AT;
    regs.r[2] = code + DOUBLE_TABLE_AT;
    Ok(())
}

/// Function `N` of the branch tables, in double precision where `DOUBLE`:
/// its operands in the first registers, its result in the first, computed
/// under the FPSCR's controls, which gather its flags. Its stub's
/// instruction has already found the unit granted.
#[expect(clippy::unnecessary_wraps, reason = "a routine's signature")]
fn elementary<const N: usize, const DOUBLE: bool>(
    os: &mut Os<'_>,
    _regs: &mut Regs,
) -> Result<(), Fail> {
    let vfp = os.world.processor.with_vfp().vfp_mut();
    let format = if DOUBLE {
        Binary::DOUBLE
    } else {
        Binary::SINGLE
    };
    let [x, y, z] = [0, 1, 2].map(|n| {
        if DOUBLE {
            vfp.d(n)
        } else {
            u64::from(vfp.s(n))
        }
    });
    let mut unit = Unit::new(vfp.fpscr());
    let result = match FUNCTIONS[N] {
        Function::Monadic(operation) => unit.monadic(format, x, operation),
        Function::Dyadic(operation) => unit.dyadic(format, x, y, operation),
        Function::FusedMultiplyAdd => unit.mul_add(format, z, x, y),
    };
    if DOUBLE {
        vfp.set_d(0, result);
    } else {
        vfp.set_s(0, halves(result)[0]);
    }
    vfp.set_fpscr(vfp.fpscr() | unit.flags);
    Ok(())
}

/// Whether the instruction at `at`, which the processor in state `saved`
/// found undefined, is one of the unit's.
fn is_unit_s(os: &mut Os<'_>, at: u32, saved: u32, isa: Isa) -> bool {
    let simdfp = |instruction: Option<Instruction>| {
        matches!(
            instruction,
            Some(Instruction {
                op: Op::SimdFp(_),
                ..
            })
        )
    };
    if saved & psr::T == 0 {
        return os
            .peek_word(at)
            .is_some_and(|word| simdfp(a32::decode(word, isa)));
    }
    let halfword = |os: &mut Os<'_>, address: u32| {
        Some(u16::from_le_bytes([
            os.peek_byte(address)?,
            os.peek_byte(address.wrapping_add(1))?,
        ]))
    };
    let Some(first) = halfword(os, at) else {
        return false;
    };
    let second = if t32::is_wide(first) {
        halfword(os, at.wrapping_add(2))
    } else {
        Some(0)
    };
    let position = Position::of(psr::itstate(saved));
    second.is_some_and(|second| {
        simdfp(t32::decode(first, second, isa, position).map(|decoded| decoded.instruction))
    })
}

/// An undefined instruction VFPSupport activates the context awaiting for:
/// where it lies, the processor's state as it found it, and the context's
/// header.
struct Awaited {
    at: u32,
    saved: u32,
    header: Header,
}

/// The undefined instruction the processor entered the vector for, if it is
/// one of the unit's while a context awaits activation; a context no longer
/// there leaves the null context active.
fn awaited(os: &mut Os<'_>) -> Option<Awaited> {
    if !os.world.vfp_support.awaiting() {
        return None;
    }
    let cpu = os.world.processor.cpu();
    let isa = cpu.profile().isa;
    let saved = cpu.spsr()?;
    if !isa.has(Feature::Vfp) {
        return None;
    }
    let back = if saved & psr::T != 0 { 2 } else { 4 };
    let at = cpu.reg(Reg::LR).wrapping_sub(back);
    if !is_unit_s(os, at, saved, isa) {
        return None;
    }
    let header = peeked_header(os, os.world.vfp_support.active);
    if header.is_none() {
        os.world.vfp_support.active = NO_CONTEXT;
    }
    Some(Awaited {
        at,
        saved,
        header: header?,
    })
}

/// The undefined instruction vector's claimant: where a context awaits
/// lazy activation and the instruction is the unit's, the context is
/// activated and the instruction run again; anything else is passed on.
///
/// The activation is the instruction's own work, so it runs in the
/// instruction's mode, and an access of it that aborts aborts the
/// instruction.
fn undefined(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let entered = os.registers();
    let handling = os.world.processor.cpu().cpsr();
    let activation = awaited(os);
    let Some(Awaited { at, saved, header }) = activation else {
        pass_on(os, entered);
        return Ok(());
    };
    if os.world.processor.cpu().set_cpsr(saved).is_err() {
        pass_on(os, entered);
        return Ok(());
    }
    let context = os.world.vfp_support.active;
    let outer = core::mem::replace(&mut os.world.swi_at, at);
    let switched = switch_to(os, context, header);
    os.world.swi_at = outer;
    match switched {
        Ok(()) => {
            let mut again = os.registers();
            again.r[15] = at;
            os.leave_with(again);
            Ok(())
        }
        Err(Fail::Unwind(unwind)) => Err(Fail::Unwind(unwind)),
        // Only an abort stops an activation; were one refused otherwise, the
        // instruction would stay undefined.
        Err(Fail::Error(_)) => {
            if os.world.processor.cpu().set_cpsr(handling).is_err() {
                return Err(Fail::Unwind(Unwind::Ended(crate::os::Ended::Fault)));
            }
            pass_on(os, entered);
            Ok(())
        }
    }
}

/// The undefined instruction passed on to the vector's owner before
/// VFPSupport, the registers as the vector was entered with.
fn pass_on(os: &mut Os<'_>, mut registers: crate::env::Registers) {
    registers.r[15] = os.world.vfp_support.passes_to.unwrap_or_else(|| {
        os.world
            .routine_address(crate::host::Routine::Exception(crate::host::UNDEFINED))
    });
    os.leave_with(registers);
}

/// The header of the context at `context`, read without aborting: none
/// where no context lies there.
fn peeked_header(os: &mut Os<'_>, context: u32) -> Option<Header> {
    let mut words = [0; HEADER_WORDS];
    for (n, word) in (0..).zip(&mut words) {
        *word = os.peek_word(context.wrapping_add(4 * n))?;
    }
    Header::of(words, registers_most(os))
}

/// The guest code: the descriptor, each table's branches to its stubs, and
/// each stub, an instruction of the unit's then a branch to its function's
/// routine. The unit's gate stands before a function as before any of its
/// instructions: the context awaiting is activated, or with none the
/// instruction is undefined.
fn emit(first: i32) -> Result<Vec<u32>, MemoryError> {
    let entries = 2 * FUNCTIONS.len();
    let mut words = Vec::new();
    words.try_reserve_exact(DESCRIPTOR.len() + 3 * entries)?;
    words.extend_from_slice(&DESCRIPTOR);
    let stubs = STUBS_AT as usize / 4;
    for entry in 0..entries {
        let offset = 4 * (stubs + 2 * entry) - (4 * words.len() + 8);
        words.push(instruction(Op::Branch {
            op: BranchOp::B,
            offset: i32::try_from(offset).map_err(|_| MemoryError::HostFull)?,
        })?);
    }
    let touch = instruction(Op::SimdFp(SimdFp::Monadic {
        op: VfpMonadicOp::Vmov,
        double: true,
        d: 0,
        m: 0,
    }))?;
    for entry in 0..entries {
        words.push(touch);
        words.push(routine_branch(words.len(), first, routine::SINGLE + entry)?);
    }
    Ok(words)
}

/// Claims the undefined instruction vector and leaves the null context
/// active.
fn init(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let entry = os
        .routine_of(&VFP_SUPPORT, routine::UNDEFINED)
        .ok_or_else(|| os.error(BAD_PARAMETERS))?;
    let mut claim = Regs::default();
    claim.r[0] = crate::host::UNDEFINED | crate::traps::CLAIM;
    claim.r[1] = entry;
    os.claim_processor_vector(&mut claim)?;
    os.world.vfp_support = State {
        passes_to: Some(claim.r[1]),
        ..State::new()
    };
    grant(os, Access::Denied);
    os.world.processor.with_vfp().vfp_mut().set_fpexc(0);
    Ok(())
}

/// The context the unit holds saved, then the claim released: refused, with
/// VFPSupport staying, where another claimed the vector after it.
fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    unload(os)?;
    if let Some(previous) = os.world.vfp_support.passes_to {
        let entry = os
            .routine_of(&VFP_SUPPORT, routine::UNDEFINED)
            .ok_or_else(|| os.error(BAD_PARAMETERS))?;
        let mut release = Regs::default();
        release.r[0] = crate::host::UNDEFINED;
        release.r[1] = previous;
        release.r[2] = entry;
        os.claim_processor_vector(&mut release)?;
    }
    // Quietly: with the vector given back, nothing may refuse the kill.
    if let Some((copy, _)) = os.world.vfp_support.dump.take() {
        os.free_rma_quietly(copy);
    }
    os.world.vfp_support = State::new();
    os.world.processor.with_vfp().vfp_mut().set_fpexc(0);
    for kept in os.world.contexts.parked() {
        kept.vfp_active = NO_CONTEXT;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
