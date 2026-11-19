//! The 6502 builder: one ROM image assembled from Rust.
//!
//! Each instruction is a method taking a typed operand — [`imm`], [`zp`] with
//! `.x()`, `.y()`, `.ind_x()` or `.ind_y()`, [`abs`] with `.x()`, `.y()` or
//! `.ind()`, [`Acc`], or a label for an absolute address — and encodes
//! through [`isa::encode`], so only documented opcodes exist and the encoding
//! is the CPU's own. A label is a [`Label`] handle or, for code that refers
//! across a whole ROM, a name; either may be used before it is bound, and
//! every use is resolved when the image is finished, where a branch out of
//! range, a zero-page operand outside page zero or a label never bound is
//! refused.
//! Code placed over bytes already placed is refused too, so code growing
//! into a table at a fixed address cannot do so unseen.
//!
//! [`Asm::pin`] places code at an address ahead of the code that will flow
//! past it, for an entry point software calls by its address. An instruction
//! that would leave no room for a `JMP` before a pin hops over it: with that
//! `JMP`, or a branch the last instruction's flags make certain, where the
//! code before runs on, otherwise by moving its labels past the pin. Data
//! meeting a pin moves past it, from where the last instruction ended or the
//! named label last bound in it, its labels and uses with it, unless code
//! runs on into it or an [`Asm::org`] fixed it there. A table held by
//! [`Asm::later`], or a block [`Asm::movable`] could not fit before a pin,
//! fills the room a pin leaves unused, or goes into the free space
//! [`Asm::place_later`] packs the rest into; what is placed so is then a pin
//! itself. [`Asm::error`] and [`Asm::call_with_text`] place an instruction
//! and the data after it whole.
//!
//! The first mistake is kept and returned by [`Asm::finish`]; the methods
//! themselves never fail, so a ROM's source reads as its instructions.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::{Add, Range};

use crate::cpu::isa::{self, Mnemonic, Mode};
use crate::cpu::ADDRESS_SPACE;

/// The byte an unplaced part of an image holds: an erased EPROM's.
pub const ERASED: u8 = 0xFF;
/// Whether a read of `page` acts on no device where RAM fills the address
/// space's lower half, as on the Acorn machines and the Commodore 64: the
/// pages [`Asm::skip_over`]'s read may name unless [`Asm::with_quiet_pages`]
/// says otherwise.
const fn lower_half_ram(page: u8) -> bool {
    page < 0x80
}

/// A place in the image, bound once; meaningful only to the [`Asm`] that
/// made it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Label(usize);

/// An address an operand or data word names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Addr {
    /// A fixed address.
    At(u16),
    /// A label's address plus a displacement.
    Label(Label, i32),
    /// The address of the label of this name plus a displacement.
    Named(&'static str, i32),
    /// The nearest `RTS` within a branch's reach; only a branch may name it.
    Return,
    /// A fixed address displaced past the address space, or the nearest
    /// `RTS` displaced at all: refused wherever it is used.
    Invalid,
}

/// A branch target: any `RTS` the image holds within the branch's reach,
/// since every one returns alike.
pub const RTS: Addr = Addr::Return;

/// The label named `name`, to add a displacement to.
#[must_use]
pub const fn sym(name: &'static str) -> Addr {
    Addr::Named(name, 0)
}

/// A byte BIT of which sets V and clears N.
pub const V_SET: u8 = 0x40;

/// `n`, a count or length the image holds in a byte.
///
/// # Panics
///
/// When it is built, if `n` does not fit.
#[must_use]
pub const fn byte(n: usize) -> u8 {
    assert!(n <= 0xFF, "a count past a byte");
    n.to_le_bytes()[0]
}

impl From<&'static str> for Addr {
    fn from(name: &'static str) -> Self {
        Self::Named(name, 0)
    }
}

impl From<u16> for Addr {
    fn from(address: u16) -> Self {
        Self::At(address)
    }
}

impl From<Label> for Addr {
    fn from(label: Label) -> Self {
        Self::Label(label, 0)
    }
}

impl Add<i32> for Addr {
    type Output = Self;

    fn add(self, displacement: i32) -> Self {
        match self {
            Self::At(address) => i32::from(address)
                .checked_add(displacement)
                .and_then(|address| u16::try_from(address).ok())
                .map_or(Self::Invalid, Self::At),
            Self::Label(label, by) => Self::Label(label, by.saturating_add(displacement)),
            Self::Named(name, by) => Self::Named(name, by.saturating_add(displacement)),
            Self::Return if displacement == 0 => Self::Return,
            Self::Return | Self::Invalid => Self::Invalid,
        }
    }
}

impl Add<i32> for Label {
    type Output = Addr;

    fn add(self, displacement: i32) -> Addr {
        Addr::Label(self, displacement)
    }
}

/// A byte an immediate operand or a data byte holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Byte {
    /// The byte itself.
    Value(u8),
    /// An address's low byte.
    Low(Addr),
    /// An address's high byte.
    High(Addr),
}

impl From<u8> for Byte {
    fn from(value: u8) -> Self {
        Self::Value(value)
    }
}

/// The low byte of `address`.
pub fn lo(address: impl Into<Addr>) -> Byte {
    Byte::Low(address.into())
}

/// The high byte of `address`.
pub fn hi(address: impl Into<Addr>) -> Byte {
    Byte::High(address.into())
}

/// An instruction's operand, made only by [`imm`], [`zp`], [`abs`], [`Acc`]
/// or a label, so its mode and the bytes it takes always agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Operand(Kind);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Accumulator,
    Immediate(Byte),
    /// An address, in the mode that reaches it.
    Memory(Mode, Addr),
}

/// The immediate operand `byte`.
pub fn imm(byte: impl Into<Byte>) -> Operand {
    Operand(Kind::Immediate(byte.into()))
}

/// The accumulator as an operand: `ASL A`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Acc;

impl From<Acc> for Operand {
    fn from(_: Acc) -> Self {
        Self(Kind::Accumulator)
    }
}

/// A zero-page operand, and the modes indexed from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zp(Addr);

/// The zero-page operand `address`.
pub fn zp(address: impl Into<Addr>) -> Zp {
    Zp(address.into())
}

impl Zp {
    /// `zp,X`.
    #[must_use]
    pub const fn x(self) -> Operand {
        Operand(Kind::Memory(Mode::ZeroPageX, self.0))
    }

    /// `zp,Y`.
    #[must_use]
    pub const fn y(self) -> Operand {
        Operand(Kind::Memory(Mode::ZeroPageY, self.0))
    }

    /// `(zp,X)`.
    #[must_use]
    pub const fn ind_x(self) -> Operand {
        Operand(Kind::Memory(Mode::IndexedIndirect, self.0))
    }

    /// `(zp),Y`.
    #[must_use]
    pub const fn ind_y(self) -> Operand {
        Operand(Kind::Memory(Mode::IndirectIndexed, self.0))
    }
}

impl From<Zp> for Operand {
    fn from(operand: Zp) -> Self {
        Self(Kind::Memory(Mode::ZeroPage, operand.0))
    }
}

/// An absolute operand, and the modes indexed or indirect from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Abs(Addr);

/// The absolute operand `address`.
pub fn abs(address: impl Into<Addr>) -> Abs {
    Abs(address.into())
}

impl Abs {
    /// `abs,X`.
    #[must_use]
    pub const fn x(self) -> Operand {
        Operand(Kind::Memory(Mode::AbsoluteX, self.0))
    }

    /// `abs,Y`.
    #[must_use]
    pub const fn y(self) -> Operand {
        Operand(Kind::Memory(Mode::AbsoluteY, self.0))
    }

    /// `(abs)`, for `JMP`.
    #[must_use]
    pub const fn ind(self) -> Operand {
        Operand(Kind::Memory(Mode::Indirect, self.0))
    }
}

impl From<Abs> for Operand {
    fn from(operand: Abs) -> Self {
        Self(Kind::Memory(Mode::Absolute, operand.0))
    }
}

impl From<Label> for Operand {
    fn from(label: Label) -> Self {
        Self(Kind::Memory(Mode::Absolute, label.into()))
    }
}

impl From<&'static str> for Operand {
    fn from(name: &'static str) -> Self {
        Self(Kind::Memory(Mode::Absolute, name.into()))
    }
}

/// Why an image could not be built, with the address it went wrong at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsmError {
    /// No documented opcode performs the mnemonic in the mode.
    NoEncoding {
        /// The address.
        at: u16,
        /// The mnemonic.
        mnemonic: Mnemonic,
        /// The mode asked for.
        mode: Mode,
    },
    /// A byte was placed where one already was.
    Overlap {
        /// The address.
        at: u16,
    },
    /// A byte was placed outside the image.
    OutsideImage {
        /// Bytes from the origin.
        offset: usize,
    },
    /// A branch's target is more than a signed byte away.
    BranchRange {
        /// The branch's address.
        at: u16,
        /// Its target.
        target: u16,
        /// The target's label, or `""` for a local one or an address.
        name: &'static str,
        /// The last named label at or before the branch.
        after: &'static str,
    },
    /// A zero-page operand's address lies outside page zero.
    NotZeroPage {
        /// The instruction's address.
        at: u16,
    },
    /// An address displaced past the address space, or the nearest `RTS`
    /// displaced.
    AddressRange {
        /// Where it is used.
        at: u16,
    },
    /// A label another image made.
    ForeignLabel {
        /// Where it is bound or used.
        at: u16,
    },
    /// A label was used but never bound.
    Unbound {
        /// Its name, or `""` for a local one.
        name: &'static str,
    },
    /// A label was bound twice.
    Rebound {
        /// Its name, or `""` for a local one.
        name: &'static str,
        /// The second address.
        at: u16,
    },
    /// Two labels share a name.
    Duplicate {
        /// The name.
        name: &'static str,
    },
    /// Text that is not ASCII.
    NotAscii {
        /// Where it starts.
        at: u16,
    },
    /// An error message holding a zero, which ends it there.
    ZeroInMessage {
        /// Where its block starts.
        at: u16,
    },
    /// A branch to an `RTS` had none within its reach.
    NoReturnInReach {
        /// The branch's address.
        at: u16,
        /// The last named label bound before it.
        after: &'static str,
    },
    /// Something other than a branch named the nearest `RTS`.
    ReturnOutsideBranch {
        /// Where it is used.
        at: u16,
    },
    /// Code meant to run on into a label placed elsewhere.
    NotNext {
        /// Where the code ends.
        at: u16,
        /// The label's name, or `""` for a local one.
        name: &'static str,
    },
    /// A table or block [`Asm::place_later`] holds is longer than every free
    /// stretch left for it.
    NoRoom {
        /// Its length.
        len: usize,
    },
    /// A table held for later places a pin, which could land where other
    /// held tables had gone first.
    HeldPin {
        /// Where it was held.
        at: u16,
    },
    /// Code running on into a pin had no room left for a `JMP` over it.
    PinBlocked {
        /// Where the code stands.
        at: u16,
        /// The pin's address.
        pin: u16,
    },
    /// What [`Asm::skip_over`] skips is not one instruction of one or two
    /// bytes, those bytes known, or two make an address beyond RAM.
    SkipOperand {
        /// The skipping `BIT`'s address.
        at: u16,
    },
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    name: &'static str,
    address: Option<u16>,
}

#[derive(Clone, Copy, Debug)]
enum Use {
    Byte(Byte),
    Word(Addr),
    ZeroPage(Addr),
    /// A branch's displacement from the instruction after it.
    Branch(Addr),
}

#[derive(Clone, Copy, Debug)]
struct Fixup {
    offset: usize,
    /// The address of the instruction or datum the fixup belongs to.
    at: u16,
    what: Use,
}

/// An assembled image and where its named labels lie.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// The bytes, from the origin.
    pub bytes: Vec<u8>,
    /// Each named label's address, by name.
    pub symbols: Vec<(&'static str, u16)>,
}

impl Image {
    /// The address of the label named `name`.
    #[must_use]
    pub fn symbol(&self, name: &str) -> Option<u16> {
        self.symbols
            .binary_search_by(|&(symbol, _)| symbol.cmp(name))
            .ok()
            .map(|index| self.symbols[index].1)
    }
}

/// An image being assembled.
#[derive(Debug)]
pub struct Asm {
    origin: u16,
    image: Vec<u8>,
    placed: Vec<bool>,
    /// Bytes from the origin to the next one placed.
    offset: usize,
    labels: Vec<Slot>,
    /// The named labels, by name.
    names: BTreeMap<&'static str, Label>,
    fixups: Vec<Fixup>,
    /// Where each `RTS` instruction is.
    returns: Vec<u16>,
    /// Where code runs on into a label, and the label.
    falls: Vec<(u16, Addr)>,
    /// What [`Asm::pin`] placed, by offset and in order.
    pins: Vec<Range<usize>>,
    /// The labels bound since the last byte was placed.
    pending: Vec<Label>,
    /// The last thing placed was an instruction that runs on into the next
    /// byte: not a jump, a return or data.
    runs_on: bool,
    /// The branch the flags that instruction leaves make certain, if any.
    certain: Option<Mnemonic>,
    /// Nothing has been placed since the origin was set, so what comes next
    /// belongs exactly there.
    anchored: bool,
    data_run: DataRun,
    /// A pin is being placed, where it is put.
    pinning: bool,
    later: Later,
    /// The rooms hops left free, filled from what is held once it all is.
    rooms: Vec<Range<usize>>,
    /// The start of the pin the last block [`Asm::movable`] held would have
    /// run over: the next not fitting before it either moves the flow past.
    straddled: Option<usize>,
    /// The pages a read of acts on no device, on the machine the image runs.
    quiet_page: fn(u8) -> bool,
    error: Option<AsmError>,
}

/// What [`Asm::later`] and [`Asm::movable`] hold for [`Asm::place_later`],
/// in order.
struct Later(Vec<Held>);

/// A table or block held for later, and the bytes it places.
struct Held {
    body: Box<dyn Fn(&mut Asm)>,
    len: usize,
}

impl core::fmt::Debug for Later {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} held", self.0.len())
    }
}

/// The bytes a `JMP` over a pin takes.
const HOP: usize = 3;

/// How far past a hop's own two bytes a branch over a pin reaches.
const BRANCH_REACH: usize = 2 + 127;

/// The items, by index in the order given, whose lengths together fill the
/// most of `room` bytes.
fn fullest(lens: &[usize], room: usize) -> Vec<usize> {
    const NONE_YET: usize = usize::MAX;
    // by[total]: the item that first brought a choice to `total` bytes.
    let mut by: Vec<Option<usize>> = alloc::vec![None; room + 1];
    by[0] = Some(NONE_YET);
    for (index, &len) in lens.iter().enumerate() {
        if len == 0 || len > room {
            continue;
        }
        for total in (len..=room).rev() {
            if by[total].is_none() && by[total - len].is_some() {
                by[total] = Some(index);
            }
        }
    }
    let mut total = (0..=room)
        .rev()
        .find(|&total| by[total].is_some())
        .unwrap_or(0);
    let mut chosen = Vec::new();
    while let Some(index) = by[total].filter(|&index| index != NONE_YET) {
        chosen.push(index);
        total -= lens[index];
    }
    chosen.reverse();
    chosen
}

/// The tables at `chosen`, indices in the order held, taken out of `held`
/// and kept in that order.
fn take_chosen(held: &mut Vec<Held>, chosen: &[usize]) -> Vec<Held> {
    let mut taken: Vec<Held> = chosen
        .iter()
        .rev()
        .map(|&index| held.remove(index))
        .collect();
    taken.reverse();
    taken
}

/// For each held item, the stretch it goes in. All of it fitting in the
/// first stretch goes there; otherwise each stretch, the smallest first,
/// takes the items still unplaced that fill the most of it, the larger
/// stretches then left the larger items. An item no stretch took is refused
/// with its length.
fn pack(held: &[Held], stretches: &[Range<usize>]) -> Result<Vec<usize>, usize> {
    let size = |stretch: usize| stretches[stretch].end - stretches[stretch].start;
    let total: usize = held.iter().map(|table| table.len).sum();
    if !stretches.is_empty() && size(0) >= total {
        return Ok(vec![0; held.len()]);
    }
    let mut smallest_first: Vec<usize> = (0..stretches.len()).collect();
    smallest_first.sort_by_key(|&stretch| size(stretch));
    let mut into: Vec<Option<usize>> = held
        .iter()
        .map(|table| smallest_first.first().copied().filter(|_| table.len == 0))
        .collect();
    for &stretch in &smallest_first {
        let unplaced: Vec<usize> = (0..held.len())
            .filter(|&index| into[index].is_none())
            .collect();
        let lens: Vec<usize> = unplaced.iter().map(|&index| held[index].len).collect();
        for chosen in fullest(&lens, size(stretch)) {
            into[unplaced[chosen]] = Some(stretch);
        }
    }
    into.iter()
        .zip(held)
        .map(|(&stretch, table)| stretch.ok_or(table.len))
        .collect()
}

/// The branch a conditional branch not taken makes certain.
const fn opposite(branch: Mnemonic) -> Option<Mnemonic> {
    match branch {
        Mnemonic::Bcc => Some(Mnemonic::Bcs),
        Mnemonic::Bcs => Some(Mnemonic::Bcc),
        Mnemonic::Beq => Some(Mnemonic::Bne),
        Mnemonic::Bne => Some(Mnemonic::Beq),
        Mnemonic::Bmi => Some(Mnemonic::Bpl),
        Mnemonic::Bpl => Some(Mnemonic::Bmi),
        Mnemonic::Bvc => Some(Mnemonic::Bvs),
        Mnemonic::Bvs => Some(Mnemonic::Bvc),
        _ => None,
    }
}

/// The data placed since the last instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DataRun {
    /// There is none: an instruction or an origin was the last thing.
    Absent,
    /// Its latest item, from its run's start or the named label last bound
    /// in it, starts at this offset; nothing runs on into the run, so the
    /// item may move past a pin.
    Movable(usize),
    /// It stays where it is placed.
    Fixed,
}

macro_rules! implied {
    ($($name:ident $mnemonic:ident),* $(,)?) => {$(
        #[doc = concat!("`", stringify!($mnemonic), "`.")]
        pub fn $name(&mut self) {
            self.instruction(Mnemonic::$mnemonic, None);
        }
    )*};
}

macro_rules! with_operand {
    ($($name:ident $mnemonic:ident),* $(,)?) => {$(
        #[doc = concat!("`", stringify!($mnemonic), "` with `operand`.")]
        pub fn $name(&mut self, operand: impl Into<Operand>) {
            self.instruction(Mnemonic::$mnemonic, Some(operand.into()));
        }
    )*};
}

macro_rules! branches {
    ($($name:ident $mnemonic:ident),* $(,)?) => {$(
        #[doc = concat!("`", stringify!($mnemonic), "` to `target`.")]
        pub fn $name(&mut self, target: impl Into<Addr>) {
            self.branch(Mnemonic::$mnemonic, target.into());
        }
    )*};
}

impl Asm {
    /// An image of `len` bytes assembled from `origin`; one running past the
    /// address space is refused before anything is allocated for it.
    #[must_use]
    pub fn new(origin: u16, len: usize) -> Self {
        let fits = usize::from(origin)
            .checked_add(len)
            .is_some_and(|end| end <= ADDRESS_SPACE);
        let held = if fits { len } else { 0 };
        let mut asm = Self {
            origin,
            image: vec![ERASED; held],
            placed: vec![false; held],
            offset: 0,
            labels: Vec::new(),
            names: BTreeMap::new(),
            fixups: Vec::new(),
            returns: Vec::new(),
            falls: Vec::new(),
            pins: Vec::new(),
            pending: Vec::new(),
            runs_on: false,
            certain: None,
            anchored: true,
            data_run: DataRun::Absent,
            pinning: false,
            later: Later(Vec::new()),
            rooms: Vec::new(),
            straddled: None,
            quiet_page: lower_half_ram,
            error: None,
        };
        if !fits {
            asm.fail(AsmError::OutsideImage { offset: len });
        }
        asm
    }

    /// The address the next byte is placed at.
    #[must_use]
    pub fn pc(&self) -> u16 {
        self.address(self.offset)
    }

    fn address(&self, offset: usize) -> u16 {
        self.origin
            .wrapping_add(u16::try_from(offset).unwrap_or(u16::MAX))
    }

    fn fail(&mut self, error: AsmError) {
        self.error.get_or_insert(error);
    }

    /// A label named `name`, in the symbol map once bound and reachable by
    /// its name from anywhere in the image.
    pub fn label(&mut self, name: &'static str) -> Label {
        self.labels.push(Slot {
            name,
            address: None,
        });
        let label = Label(self.labels.len() - 1);
        if self.names.insert(name, label).is_some() {
            self.fail(AsmError::Duplicate { name });
        }
        label
    }

    /// A label of no name, kept out of the symbol map: a loop's, a skip's.
    pub fn local(&mut self) -> Label {
        self.labels.push(Slot {
            name: "",
            address: None,
        });
        Label(self.labels.len() - 1)
    }

    /// Bind `label` to the address the next byte is placed at.
    pub fn bind(&mut self, label: Label) {
        let at = self.pc();
        let Some(slot) = self.labels.get_mut(label.0) else {
            self.fail(AsmError::ForeignLabel { at });
            return;
        };
        if slot.address.is_some() {
            let name = slot.name;
            self.fail(AsmError::Rebound { name, at });
            return;
        }
        slot.address = Some(at);
        let named = !slot.name.is_empty();
        self.pending.push(label);
        if named && matches!(self.data_run, DataRun::Movable(_)) {
            self.data_run = DataRun::Movable(self.offset);
        }
    }

    /// A label named `name`, bound here.
    pub fn here(&mut self, name: &'static str) -> Label {
        let label = self.label(name);
        self.bind(label);
        label
    }

    /// A local label, bound here.
    pub fn here_local(&mut self) -> Label {
        let label = self.local();
        self.bind(label);
        label
    }

    /// Run on into `target`, which must be the next thing placed. Nothing
    /// is emitted; [`Asm::finish`] refuses the image if `target` lies
    /// anywhere else.
    pub fn fall_into(&mut self, target: impl Into<Addr>) {
        let at = self.pc();
        self.falls.push((at, target.into()));
    }

    /// Place what follows at `address`.
    pub fn org(&mut self, address: u16) {
        match address.checked_sub(self.origin) {
            Some(offset) if usize::from(offset) <= self.image.len() => {
                self.offset = usize::from(offset);
                self.pending.clear();
                self.runs_on = false;
                self.anchored = true;
                self.data_run = DataRun::Absent;
                self.straddled = None;
            }
            _ => self.fail(AsmError::OutsideImage {
                offset: usize::from(address.wrapping_sub(self.origin)),
            }),
        }
    }

    /// Emit `body` where [`Asm::place_later`] is next called, or sooner in
    /// the room a pin leaves unused before it, not here: for tables, which
    /// their labels reach wherever they lie. A body placing a pin is refused.
    pub fn later(&mut self, body: impl Fn(&mut Self) + 'static) {
        let measure = self.measure(&body);
        if !measure.pins.is_empty() {
            let at = self.pc();
            self.fail(AsmError::HeldPin { at });
        }
        let len = measure.offset;
        self.later.0.push(Held {
            body: Box::new(body),
            len,
        });
    }

    /// `body` emitted alone, to learn what it places.
    fn measure(&self, body: &impl Fn(&mut Self)) -> Self {
        let mut measure = Self::new(self.origin, self.image.len());
        body(&mut measure);
        measure
    }

    /// A block placed whole where it fits before the next pin, the room for
    /// a hop after it not needed as nothing runs on past it. One that does
    /// not fit is held as [`Asm::later`] holds a table, and the blocks after
    /// it go on into the room before the pin until one does not fit either,
    /// which moves the flow past the pin, the room left to be filled. A block
    /// the flow runs on into, one a label just bound or an [`Asm::org`] puts
    /// here, one that runs on itself and one placing a pin stay where they
    /// are, placed as [`Asm::whole`] places one.
    pub fn movable(&mut self, body: impl Fn(&mut Self) + 'static) {
        let measure = self.measure(&body);
        let len = measure.offset;
        let entered = self.runs_on || self.anchored || !self.pending.is_empty();
        if self.pinning || entered || measure.runs_on || !measure.pins.is_empty() {
            self.make_way(len, false);
            body(self);
            return;
        }
        while let Some(pin) = self.next_pin().filter(|pin| self.offset + len > pin.start) {
            if self.straddled != Some(pin.start) {
                self.straddled = Some(pin.start);
                self.later.0.push(Held {
                    body: Box::new(body),
                    len,
                });
                return;
            }
            self.straddled = None;
            self.pass(&pin);
        }
        // Ending the flow, it needs no room for a hop after it.
        self.pinning = true;
        body(self);
        self.pinning = false;
    }

    /// The first pin not wholly before the flow.
    fn next_pin(&self) -> Option<Range<usize>> {
        let first = self.pins.partition_point(|pin| pin.end <= self.offset);
        self.pins.get(first).cloned()
    }

    /// Move the flow past `pin` and the pins it abuts, leaving the room
    /// before it to be filled.
    fn pass(&mut self, pin: &Range<usize>) {
        let mut past = pin.end;
        let after = self.pins.partition_point(|other| other.end <= past);
        for other in &self.pins[after..] {
            if other.start != past {
                break;
            }
            past = other.end;
        }
        self.leave_rooms(self.offset, past);
        self.offset = past;
        self.certain = None;
        self.data_run = DataRun::Absent;
    }

    /// Emit what [`Asm::later`] and [`Asm::movable`] hold: into the rooms
    /// hops left, filled as [`Asm::place_later_before`] fills them, then
    /// into the free stretches from here to the image's end: all in the
    /// first where it fits, else the smallest stretch first, each taking
    /// what fills the most of it, and each stretch's in the order held. What
    /// is held by a body placed so is placed after it.
    pub fn place_later(&mut self) {
        self.fill_rooms();
        while !self.later.0.is_empty() && self.error.is_none() {
            let held = core::mem::take(&mut self.later.0);
            let stretches = self.free_runs(self.offset..self.image.len());
            let into = match pack(&held, &stretches) {
                Ok(into) => into,
                Err(len) => {
                    self.fail(AsmError::NoRoom { len });
                    return;
                }
            };
            let mut each: Vec<Vec<Held>> = stretches.iter().map(|_| Vec::new()).collect();
            for (table, stretch) in held.into_iter().zip(into) {
                each[stretch].push(table);
            }
            for (stretch, tables) in stretches.iter().zip(each) {
                if !tables.is_empty() {
                    self.fill_room(stretch.start, tables);
                }
            }
        }
    }

    /// Fill each room hops left, in address order, with the held tables
    /// that fill the most of it: only what is still free of it, as code
    /// flowing since may have taken part.
    fn fill_rooms(&mut self) {
        let mut rooms = core::mem::take(&mut self.rooms);
        rooms.sort_by_key(|room| room.start);
        for room in rooms {
            for free in self.free_runs(room) {
                let lens: Vec<usize> = self.later.0.iter().map(|table| table.len).collect();
                let chosen = fullest(&lens, free.end - free.start);
                let tables = take_chosen(&mut self.later.0, &chosen);
                self.fill_room(free.start, tables);
            }
        }
    }

    /// The runs of `range` nothing has been placed in.
    fn free_runs(&self, range: Range<usize>) -> Vec<Range<usize>> {
        let mut runs = Vec::new();
        let mut begun = None;
        for at in range.clone() {
            match (self.placed.get(at).copied().unwrap_or(true), begun) {
                (false, None) => begun = Some(at),
                (true, Some(start)) => {
                    runs.push(start..at);
                    begun = None;
                }
                _ => {}
            }
        }
        if let Some(begin) = begun {
            runs.push(begin..range.end);
        }
        runs
    }

    /// Place `tables` in turn from `start`, the flow left as it was, and
    /// make what they take a pin that code flowing after hops.
    fn fill_room(&mut self, start: usize, tables: Vec<Held>) {
        let flow = (
            self.offset,
            self.runs_on,
            self.certain,
            self.anchored,
            self.data_run,
        );
        let pending = core::mem::take(&mut self.pending);
        self.pinning = true;
        let mut at = start;
        for table in tables {
            (self.offset, self.runs_on, self.anchored, self.data_run) =
                (at, false, false, DataRun::Absent);
            (table.body)(self);
            at = self.offset;
        }
        self.pinning = false;
        if at > start {
            self.merge_pin(start..at);
        }
        (
            self.offset,
            self.runs_on,
            self.certain,
            self.anchored,
            self.data_run,
        ) = flow;
        self.pending = pending;
    }

    /// Leave the room from `start` to `end` the pins leave free to be filled.
    fn leave_rooms(&mut self, start: usize, end: usize) {
        if self.pinning {
            return;
        }
        let first = self.pins.partition_point(|pin| pin.end <= start);
        let mut at = start;
        let mut free = Vec::new();
        for pin in &self.pins[first..] {
            if pin.start >= end {
                break;
            }
            if pin.start > at {
                free.push(at..pin.start);
            }
            at = at.max(pin.end);
        }
        if at < end {
            free.push(at..end);
        }
        self.rooms.extend(free);
    }

    /// Fill the room from here to `end`, and those hops left, with what
    /// [`Asm::later`] holds that fits, then carry on from `end`.
    pub fn place_later_before(&mut self, end: u16) {
        if let Some(room) = end.checked_sub(self.origin) {
            self.leave_rooms(self.offset, usize::from(room));
        }
        self.fill_rooms();
        self.org(end);
    }

    /// Leave `len` bytes here for held tables to fill, as many as fit: the
    /// slots a table laid out on a stride does not use.
    pub fn room(&mut self, len: usize) {
        self.rooms.push(self.offset..self.offset + len);
        self.offset += len;
        self.runs_on = false;
    }

    /// Leave the bytes from here to `address` for held tables to fill, then
    /// carry on there: what a pin leaves before an address of its own. Being
    /// past `address` already is the overlap [`Asm::finish`] reports.
    pub fn room_to(&mut self, address: u16) {
        match address.checked_sub(self.pc()) {
            Some(len) => self.room(usize::from(len)),
            None => self.org(address),
        }
    }

    /// Emit what `body` emits with no pin inside it, the flow hopping first
    /// if one would be: for code whose branches must reach across it.
    pub fn whole(&mut self, body: impl Fn(&mut Self)) {
        let len = self.measure(&body).offset;
        self.make_way(len, false);
        body(self);
    }

    /// Treat each run of bytes already placed from `start` to `end` as a pin
    /// that code flowing later hops past, merged with the pins it meets; the
    /// gaps between the runs stay free for that code.
    pub fn claim(&mut self, start: u16, end: u16) {
        let offset = |address: u16| usize::from(address.wrapping_sub(self.origin));
        let (from, to) = (offset(start), offset(end));
        let Some(placed) = self.placed.get(from..to).filter(|_| start >= self.origin) else {
            self.fail(AsmError::OutsideImage { offset: to });
            return;
        };
        let mut runs = Vec::new();
        let mut begun = None;
        for (at, &taken) in (from..).zip(placed) {
            match (taken, begun) {
                (true, None) => begun = Some(at),
                (false, Some(start)) => {
                    runs.push(start..at);
                    begun = None;
                }
                _ => {}
            }
        }
        if let Some(begin) = begun {
            runs.push(begin..to);
        }
        for run in runs {
            self.merge_pin(run);
        }
    }

    /// `range` made a pin, merged with the pins it meets: they are sorted
    /// and apart, so only the first and last met can widen it.
    fn merge_pin(&mut self, mut range: Range<usize>) {
        self.pins.retain(|pin| {
            let apart = pin.end <= range.start || pin.start >= range.end;
            if !apart {
                range = range.start.min(pin.start)..range.end.max(pin.end);
            }
            apart
        });
        let place = self.pins.partition_point(|pin| pin.start < range.start);
        self.pins.insert(place, range);
    }

    /// Place the code `body` emits at `address` now, ahead of the code that
    /// will flow past it, which then hops over it.
    pub fn pin(&mut self, address: u16, body: impl FnOnce(&mut Self)) {
        let flow = (
            self.offset,
            self.runs_on,
            self.certain,
            self.anchored,
            self.data_run,
        );
        let pending = core::mem::take(&mut self.pending);
        self.org(address);
        let start = self.offset;
        self.pinning = true;
        body(self);
        self.pinning = false;
        let place = self.pins.partition_point(|pin| pin.start < start);
        self.pins.insert(place, start..self.offset);
        (
            self.offset,
            self.runs_on,
            self.certain,
            self.anchored,
            self.data_run,
        ) = flow;
        self.pending = pending;
    }

    /// Where something of `len` bytes from `start` can go: there, or past
    /// each pin it would meet.
    fn clear_of_pins(&self, start: usize, len: usize) -> usize {
        let first = self.pins.partition_point(|pin| pin.end <= start);
        let mut at = start;
        for pin in &self.pins[first..] {
            if pin.start >= at + len {
                break;
            }
            at = pin.end;
        }
        at
    }

    /// Move the flow past the pins an instruction of `len` bytes would meet,
    /// leaving room for a `JMP` before the next unless the instruction ends
    /// the flow.
    fn make_way(&mut self, len: usize, ends_flow: bool) {
        if self.pinning || self.anchored {
            return;
        }
        let needed = if ends_flow { len } else { len + HOP };
        let resume = self.clear_of_pins(self.offset, needed);
        if resume == self.offset {
            return;
        }
        if self.runs_on {
            let first = self.pins.partition_point(|pin| pin.end <= self.offset);
            let pin = self.pins[first].start;
            if pin < self.offset + HOP {
                let (at, pin) = (self.pc(), self.address(pin));
                self.fail(AsmError::PinBlocked { at, pin });
                return;
            }
            // A label here may be reached with other flags.
            let target = self.address(resume);
            let reach = resume - self.offset <= BRANCH_REACH;
            match self.certain.filter(|_| reach && self.pending.is_empty()) {
                Some(branch) => self.place_branch(branch, Addr::At(target)),
                None => self.jmp(abs(target)),
            }
        } else {
            let at = self.address(resume);
            for &label in &self.pending {
                if let Some(slot) = self.labels.get_mut(label.0) {
                    slot.address = Some(at);
                }
            }
        }
        self.leave_rooms(self.offset, resume);
        self.offset = resume;
    }

    fn put(&mut self, byte: u8) {
        match self.placed.get_mut(self.offset) {
            None => {
                let offset = self.offset;
                self.fail(AsmError::OutsideImage { offset });
            }
            Some(true) => {
                let at = self.pc();
                self.fail(AsmError::Overlap { at });
            }
            Some(placed) => {
                *placed = true;
                self.image[self.offset] = byte;
            }
        }
        self.offset += 1;
        self.pending.clear();
        self.anchored = false;
    }

    /// Make way for `len` bytes of data: the item they extend moves whole
    /// past any pin they would meet, if it may.
    fn make_way_for_data(&mut self, len: usize) {
        if self.pinning {
            return;
        }
        if self.data_run == DataRun::Absent {
            self.data_run = if self.anchored || self.runs_on {
                DataRun::Fixed
            } else {
                DataRun::Movable(self.offset)
            };
        }
        let DataRun::Movable(start) = self.data_run else {
            return;
        };
        let run = self.offset - start;
        let to = self.clear_of_pins(start, run + len);
        if to == start {
            return;
        }
        let Some(gap) = self.image.get(to..to + run).map(|_| to - start) else {
            let offset = to + run;
            self.fail(AsmError::OutsideImage { offset });
            return;
        };
        self.relocate(start, gap);
    }

    /// Move the data from `start` to the current offset on by `gap` bytes,
    /// with the labels bound in it or at its end and the uses in it.
    fn relocate(&mut self, start: usize, gap: usize) {
        let end = self.offset;
        let (first, last) = (self.address(start), self.address(end));
        let shift = u16::try_from(gap).unwrap_or(u16::MAX);
        for from in start..end {
            let to = from + gap;
            if self.placed[to] {
                let at = self.address(to);
                self.fail(AsmError::Overlap { at });
                return;
            }
            self.image[to] = self.image[from];
            self.placed[to] = true;
            self.image[from] = ERASED;
            self.placed[from] = false;
        }
        for fixup in self
            .fixups
            .iter_mut()
            .filter(|fixup| (start..end).contains(&fixup.offset))
        {
            fixup.offset += gap;
            fixup.at = fixup.at.wrapping_add(shift);
        }
        for slot in &mut self.labels {
            if let Some(address) = slot
                .address
                .filter(|address| (first..=last).contains(address))
            {
                slot.address = Some(address.wrapping_add(shift));
            }
        }
        for (at, _) in self
            .falls
            .iter_mut()
            .filter(|(at, _)| (first..=last).contains(at))
        {
            *at = at.wrapping_add(shift);
        }
        self.offset = end + gap;
        self.data_run = DataRun::Movable(start + gap);
        self.leave_rooms(start, start + gap);
    }

    fn put_word(&mut self, word: u16) {
        let [low, high] = word.to_le_bytes();
        self.put(low);
        self.put(high);
    }

    fn defer(&mut self, at: u16, what: Use, len: usize) {
        self.fixups.push(Fixup {
            offset: self.offset,
            at,
            what,
        });
        for _ in 0..len {
            self.put(0);
        }
    }

    /// A data byte.
    pub fn byte(&mut self, byte: impl Into<Byte>) {
        self.make_way_for_data(1);
        match byte.into() {
            Byte::Value(value) => self.put(value),
            byte => {
                let at = self.pc();
                self.defer(at, Use::Byte(byte), 1);
            }
        }
        self.runs_on = false;
    }

    /// Data bytes.
    pub fn bytes(&mut self, bytes: &[u8]) {
        self.make_way_for_data(bytes.len());
        for &byte in bytes {
            self.put(byte);
        }
        self.runs_on = false;
    }

    /// A data word, little-endian.
    pub fn word(&mut self, word: impl Into<Addr>) {
        self.make_way_for_data(2);
        let at = self.pc();
        match word.into() {
            Addr::At(value) => self.put_word(value),
            word => self.defer(at, Use::Word(word), 2),
        }
        self.runs_on = false;
    }

    /// ASCII text, as its bytes.
    pub fn text(&mut self, text: &str) {
        if !text.is_ascii() {
            let at = self.pc();
            self.fail(AsmError::NotAscii { at });
        }
        self.bytes(text.as_bytes());
    }

    /// `BRK` and the block its handler reads after it: the error's number,
    /// its message and a zero, placed whole, with nothing running on past it.
    pub fn error(&mut self, number: u8, message: &[u8]) {
        let Some(brk) = self.error_opcode(message) else {
            return;
        };
        self.make_way(message.len() + 3, true);
        self.put_error(brk, number, message);
        self.put(0);
        self.runs_on = false;
        self.data_run = DataRun::Absent;
    }

    /// Error blocks one after another, each at its label, placed whole: a
    /// block's `BRK` is also the zero that ends the message before it.
    pub fn error_run(&mut self, blocks: &[(&'static str, u8, &[u8])]) {
        let mut opcode = None;
        for &(_, _, message) in blocks {
            opcode = self.error_opcode(message);
        }
        let Some(brk) = opcode else {
            return;
        };
        let len: usize = blocks
            .iter()
            .map(|&(_, _, message)| message.len() + 2)
            .sum();
        self.make_way(len + 1, true);
        for &(label, number, message) in blocks {
            self.here(label);
            self.put_error(brk, number, message);
        }
        self.put(0);
        self.runs_on = false;
        self.data_run = DataRun::Absent;
    }

    /// The `BRK` opcode, `message` refused if a zero would end it early.
    fn error_opcode(&mut self, message: &[u8]) -> Option<u8> {
        if message.contains(&0) {
            let at = self.pc();
            self.fail(AsmError::ZeroInMessage { at });
        }
        let brk = isa::encode(Mnemonic::Brk, Mode::Implied);
        if brk.is_none() {
            let at = self.pc();
            self.fail(AsmError::NoEncoding {
                at,
                mnemonic: Mnemonic::Brk,
                mode: Mode::Implied,
            });
        }
        brk
    }

    /// `BRK`, the error's number and its message, unterminated.
    fn put_error(&mut self, brk: u8, number: u8, message: &[u8]) {
        self.put(brk);
        self.put(number);
        for &byte in message {
            self.put(byte);
        }
    }

    /// The image for a machine where a read of a page `quiet` holds acts on
    /// no device, which [`Asm::skip_over`]'s reads keep to.
    #[must_use]
    pub fn with_quiet_pages(mut self, quiet: fn(u8) -> bool) -> Self {
        self.quiet_page = quiet;
        self
    }

    /// `BIT` with the instruction `body` places as its operand: code running
    /// on into it reads past that instruction with no effect but on N, V
    /// and Z, while a label bound in `body` enters it. The read is of the
    /// address the instruction's bytes make, so those must be known: one
    /// byte is a zero-page address, two must keep it to a quiet page.
    pub fn skip_over(&mut self, body: impl FnOnce(&mut Self)) {
        self.make_way(3, false);
        let at = self.pc();
        let Some(bit) = isa::encode(Mnemonic::Bit, Mode::Absolute) else {
            self.fail(AsmError::NoEncoding {
                at,
                mnemonic: Mnemonic::Bit,
                mode: Mode::Absolute,
            });
            return;
        };
        self.put(bit);
        let start = self.offset;
        body(self);
        let known = !self
            .fixups
            .iter()
            .any(|fixup| (start..self.offset).contains(&fixup.offset));
        let quiet = self.quiet_page;
        let mode = match self.offset - start {
            1 => Some(Mode::ZeroPage),
            2 => self
                .image
                .get(start + 1)
                .is_some_and(|&page| quiet(page))
                .then_some(Mode::Absolute),
            _ => None,
        };
        let opcode = mode
            .filter(|_| known)
            .and_then(|mode| isa::encode(Mnemonic::Bit, mode));
        match (opcode, self.image.get_mut(start - 1)) {
            (Some(opcode), Some(bit)) => *bit = opcode,
            _ => self.fail(AsmError::SkipOperand { at }),
        }
        self.certain = None;
    }

    /// `JSR target` with the text after it that the routine reads and
    /// returns past, `parts` in turn and a zero, placed whole.
    pub fn call_with_text(&mut self, target: impl Into<Addr>, parts: &[&str]) {
        let len = parts.iter().map(|part| part.len()).sum::<usize>();
        self.make_way(3 + len + 1, false);
        self.jsr(target);
        for part in parts {
            self.text(part);
        }
        self.put(0);
        self.runs_on = true;
        self.certain = None;
        self.data_run = DataRun::Absent;
    }

    /// `count` bytes of `value`.
    pub fn fill(&mut self, count: usize, value: u8) {
        self.make_way_for_data(count);
        for _ in 0..count {
            self.put(value);
        }
        self.runs_on = false;
    }

    fn instruction(&mut self, mnemonic: Mnemonic, operand: Option<Operand>) {
        let operand = operand.map(|operand| operand.0);
        let mode = match operand {
            None => Mode::Implied,
            Some(Kind::Accumulator) => Mode::Accumulator,
            Some(Kind::Immediate(_)) => Mode::Immediate,
            Some(Kind::Memory(mode, _)) => mode,
        };
        let Some(code) = isa::encode(mnemonic, mode) else {
            let at = self.pc();
            self.fail(AsmError::NoEncoding { at, mnemonic, mode });
            return;
        };
        let ends_flow = matches!(mnemonic, Mnemonic::Jmp | Mnemonic::Rts | Mnemonic::Rti);
        let certain = match (mnemonic, operand) {
            (Mnemonic::Clc, _) => Some(Mnemonic::Bcc),
            (Mnemonic::Sec, _) => Some(Mnemonic::Bcs),
            (Mnemonic::Clv, _) => Some(Mnemonic::Bvc),
            (
                Mnemonic::Lda | Mnemonic::Ldx | Mnemonic::Ldy,
                Some(Kind::Immediate(Byte::Value(0))),
            ) => Some(Mnemonic::Beq),
            (
                Mnemonic::Lda | Mnemonic::Ldx | Mnemonic::Ldy,
                Some(Kind::Immediate(Byte::Value(_))),
            ) => Some(Mnemonic::Bne),
            _ => None,
        };
        self.make_way(1 + usize::from(mode.operand_bytes()), ends_flow);
        let at = self.pc();
        if mnemonic == Mnemonic::Rts {
            self.returns.push(at);
        }
        self.put(code);
        match operand {
            None | Some(Kind::Accumulator) => {}
            Some(Kind::Immediate(byte)) => match byte {
                Byte::Value(value) => self.put(value),
                byte => self.defer(at, Use::Byte(byte), 1),
            },
            Some(Kind::Memory(mode, address)) => match (mode.operand_bytes(), address) {
                (1, Addr::At(address)) => {
                    if let Ok(address) = u8::try_from(address) {
                        self.put(address);
                    } else {
                        self.fail(AsmError::NotZeroPage { at });
                        self.put(0);
                    }
                }
                (1, address) => self.defer(at, Use::ZeroPage(address), 1),
                (_, Addr::At(address)) => self.put_word(address),
                (_, address) => self.defer(at, Use::Word(address), 2),
            },
        }
        self.runs_on = !ends_flow;
        self.certain = certain;
        self.data_run = DataRun::Absent;
    }

    fn branch(&mut self, mnemonic: Mnemonic, target: Addr) {
        self.make_way(2, false);
        self.place_branch(mnemonic, target);
    }

    fn place_branch(&mut self, mnemonic: Mnemonic, target: Addr) {
        let at = self.pc();
        let Some(code) = isa::encode(mnemonic, Mode::Relative) else {
            self.fail(AsmError::NoEncoding {
                at,
                mnemonic,
                mode: Mode::Relative,
            });
            return;
        };
        self.put(code);
        self.defer(at, Use::Branch(target), 1);
        self.runs_on = true;
        self.certain = opposite(mnemonic);
        self.data_run = DataRun::Absent;
    }

    implied! {
        brk Brk, clc Clc, cld Cld, cli Cli, clv Clv, dex Dex, dey Dey, inx Inx, iny Iny,
        nop Nop, pha Pha, php Php, pla Pla, plp Plp, rti Rti, rts Rts, sec Sec, sed Sed,
        sei Sei, tax Tax, tay Tay, tsx Tsx, txa Txa, txs Txs, tya Tya,
    }

    with_operand! {
        adc Adc, and And, asl Asl, bit Bit, cmp Cmp, cpx Cpx, cpy Cpy, dec Dec, eor Eor,
        inc Inc, jmp Jmp, lda Lda, ldx Ldx, ldy Ldy, lsr Lsr, ora Ora, rol Rol, ror Ror,
        sbc Sbc, sta Sta, stx Stx, sty Sty,
    }

    branches! {
        bcc Bcc, bcs Bcs, beq Beq, bmi Bmi, bne Bne, bpl Bpl, bvc Bvc, bvs Bvs,
    }

    /// `JSR` to `target`.
    pub fn jsr(&mut self, target: impl Into<Addr>) {
        self.instruction(
            Mnemonic::Jsr,
            Some(Operand(Kind::Memory(Mode::Absolute, target.into()))),
        );
    }

    fn resolve(&self, address: Addr, at: u16) -> Result<u16, AsmError> {
        let (label, displacement) = match address {
            Addr::At(address) => return Ok(address),
            Addr::Label(label, displacement) => (label, displacement),
            Addr::Named(name, displacement) => (
                *self.names.get(name).ok_or(AsmError::Unbound { name })?,
                displacement,
            ),
            Addr::Return => return Err(AsmError::ReturnOutsideBranch { at }),
            Addr::Invalid => return Err(AsmError::AddressRange { at }),
        };
        let slot = *self
            .labels
            .get(label.0)
            .ok_or(AsmError::ForeignLabel { at })?;
        let bound = slot.address.ok_or(AsmError::Unbound { name: slot.name })?;
        u16::try_from(i64::from(bound) + i64::from(displacement))
            .map_err(|_| AsmError::AddressRange { at })
    }

    fn name_of(&self, address: Addr) -> &'static str {
        match address {
            Addr::At(_) | Addr::Return | Addr::Invalid => "",
            Addr::Label(label, _) => self.labels.get(label.0).map_or("", |slot| slot.name),
            Addr::Named(name, _) => name,
        }
    }

    /// The last named label bound at or before `at`.
    fn label_before(&self, at: u16) -> &'static str {
        self.labels
            .iter()
            .filter_map(|slot| {
                slot.address
                    .filter(|&address| address <= at && !slot.name.is_empty())
                    .map(|address| (address, slot.name))
            })
            .max_by_key(|&(address, _)| address)
            .map_or("", |(_, name)| name)
    }

    /// The `RTS` nearest the branch at `at` within its reach, the earlier of
    /// two as near; `returns` sorted.
    fn nearest_return(&self, at: u16) -> Result<u16, AsmError> {
        let returns = &self.returns;
        let from = i32::from(at) + 2;
        let reach = |rts: u16| i8::try_from(i32::from(rts) - from).is_ok();
        let split = returns.partition_point(|&rts| i32::from(rts) < from);
        let before = split
            .checked_sub(1)
            .and_then(|i| returns.get(i))
            .copied()
            .filter(|&rts| reach(rts));
        let after = returns.get(split).copied().filter(|&rts| reach(rts));
        match (before, after) {
            (Some(before), Some(after)) if i32::from(after) - from < from - i32::from(before) => {
                Ok(after)
            }
            (Some(rts), _) | (None, Some(rts)) => Ok(rts),
            (None, None) => Err(AsmError::NoReturnInReach {
                at,
                after: self.label_before(at),
            }),
        }
    }

    fn resolve_byte(&self, byte: Byte, at: u16) -> Result<u8, AsmError> {
        match byte {
            Byte::Value(value) => Ok(value),
            Byte::Low(address) => Ok(self.resolve(address, at)?.to_le_bytes()[0]),
            Byte::High(address) => Ok(self.resolve(address, at)?.to_le_bytes()[1]),
        }
    }

    /// The finished image, every label resolved.
    ///
    /// # Errors
    ///
    /// The first [`AsmError`] met.
    pub fn finish(mut self) -> Result<Image, AsmError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        self.returns.sort_unstable();
        for fixup in &self.fixups {
            let at = fixup.at;
            match fixup.what {
                Use::Byte(byte) => self.image[fixup.offset] = self.resolve_byte(byte, at)?,
                Use::Word(address) => {
                    let [low, high] = self.resolve(address, at)?.to_le_bytes();
                    self.image[fixup.offset] = low;
                    self.image[fixup.offset + 1] = high;
                }
                Use::ZeroPage(address) => {
                    self.image[fixup.offset] = u8::try_from(self.resolve(address, at)?)
                        .map_err(|_| AsmError::NotZeroPage { at })?;
                }
                Use::Branch(address) => {
                    let target = match address {
                        Addr::Return => self.nearest_return(at)?,
                        address => self.resolve(address, at)?,
                    };
                    let from = i32::from(at) + 2;
                    let name = self.name_of(address);
                    let displacement = i8::try_from(i32::from(target) - from).map_err(|_| {
                        AsmError::BranchRange {
                            at,
                            target,
                            name,
                            after: self.label_before(at),
                        }
                    })?;
                    self.image[fixup.offset] = displacement.cast_unsigned();
                }
            }
        }
        for &(at, target) in &self.falls {
            if self.resolve(target, at)? != at {
                let name = self.name_of(target);
                return Err(AsmError::NotNext { at, name });
            }
        }
        let mut symbols: Vec<(&'static str, u16)> = self
            .labels
            .iter()
            .filter(|slot| !slot.name.is_empty())
            .map(|slot| {
                slot.address
                    .map(|address| (slot.name, address))
                    .ok_or(AsmError::Unbound { name: slot.name })
            })
            .collect::<Result<_, _>>()?;
        symbols.sort_unstable();
        Ok(Image {
            bytes: self.image,
            symbols,
        })
    }
}

#[cfg(test)]
#[path = "asm_tests.rs"]
mod tests;
