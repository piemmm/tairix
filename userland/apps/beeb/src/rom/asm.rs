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
//! The first mistake is kept and returned by [`Asm::finish`]; the methods
//! themselves never fail, so a ROM's source reads as its instructions.

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Add;

use crate::cpu::isa::{self, Mnemonic, Mode};
use crate::cpu::ADDRESS_SPACE;

/// The byte an unplaced part of an image holds: an erased EPROM's.
pub const ERASED: u8 = 0xFF;

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
    error: Option<AsmError>,
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
        } else {
            slot.address = Some(at);
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
            }
            _ => self.fail(AsmError::OutsideImage {
                offset: usize::from(address.wrapping_sub(self.origin)),
            }),
        }
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
        match byte.into() {
            Byte::Value(value) => self.put(value),
            byte => {
                let at = self.pc();
                self.defer(at, Use::Byte(byte), 1);
            }
        }
    }

    /// Data bytes.
    pub fn bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.put(byte);
        }
    }

    /// A data word, little-endian.
    pub fn word(&mut self, word: impl Into<Addr>) {
        let at = self.pc();
        match word.into() {
            Addr::At(value) => self.put_word(value),
            word => self.defer(at, Use::Word(word), 2),
        }
    }

    /// ASCII text, as its bytes.
    pub fn text(&mut self, text: &str) {
        if !text.is_ascii() {
            let at = self.pc();
            self.fail(AsmError::NotAscii { at });
        }
        self.bytes(text.as_bytes());
    }

    /// `count` bytes of `value`.
    pub fn fill(&mut self, count: usize, value: u8) {
        for _ in 0..count {
            self.put(value);
        }
    }

    fn instruction(&mut self, mnemonic: Mnemonic, operand: Option<Operand>) {
        let at = self.pc();
        let operand = operand.map(|operand| operand.0);
        let mode = match operand {
            None => Mode::Implied,
            Some(Kind::Accumulator) => Mode::Accumulator,
            Some(Kind::Immediate(_)) => Mode::Immediate,
            Some(Kind::Memory(mode, _)) => mode,
        };
        let Some(code) = isa::encode(mnemonic, mode) else {
            self.fail(AsmError::NoEncoding { at, mnemonic, mode });
            return;
        };
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
    }

    fn branch(&mut self, mnemonic: Mnemonic, target: Addr) {
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
