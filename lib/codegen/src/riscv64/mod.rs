//! The riscv64 backend, LP64 calling convention, on the RV64GC base every
//! riscv64 port has. Each function makes a frame record — `ra` and the
//! caller's `s0` just below the frame's top, `s0` pointing at it — so a
//! frame-pointer walk from a helper it calls passes through it. A0–A7,
//! T4–T6 and S1–S11 are allocated; T0–T3 are the backend's own scratch, T0
//! also a call's target and a far jump's, T1 a parallel move's cycle; GP and
//! TP are never touched. A 32-bit value is held sign-extended, as the
//! word instructions leave it, and is zero-extended where the convention
//! passes it: arguments in, the answer out, a helper's arguments. Division
//! by zero is made total by masking the quotient the processor gives, all
//! ones, to zero; its remainder is the dividend already. Rotations, which
//! the base ISA lacks, are a shift each way, and a 16-bit or signed 8-bit
//! extension a shift up and back; where every core has Zbb, each is the one
//! instruction it names. A twelve-bit immediate is
//! folded into an addition, subtraction, logical operation or comparison,
//! and a shift or rotate count wherever it is a constant; a branch compares
//! against zero in its own encoding.

pub(crate) mod inst;
#[cfg(test)]
mod render;
#[cfg(any(test, feature = "test-util"))]
pub(crate) mod sim;

use alloc::vec::Vec;

use crate::bits::sext32;
use crate::ir::{BinOp, Cond as IrCond, Function, Inst as Ir, Type, UnOp, Value, Width};
use crate::lower::{self, Backend, Context, Label, Machine, Test};
use crate::moves::{sequence, Move};
use crate::regalloc::{Allocation, Loc, RegClass};
use crate::uses::Folding;
use crate::{CompileError, Target};
use inst::{reg, Cond, Ext, ImmOp, Inst, Op, Size};
use tairix_abi::cpufeatures::CpuFeature;

/// The registers a call keeps that are allocated: S1–S11, all of them but
/// the frame pointer.
pub(crate) const CALLEE_SAVED: [u8; 11] = [9, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27];

/// The registers the arguments of a call arrive in: A0–A5.
pub(crate) const ARGS: [u8; 6] = [10, 11, 12, 13, 14, 15];

/// The target as the shared lowering sees it.
const MACHINE: Machine = Machine {
    regs: RegClass::new(&[10, 11, 12, 13, 14, 15, 16, 17, 29, 30, 31], &CALLEE_SAVED),
    folding: Folding {
        inst: folds,
        branch: |_, ty, imm| held(ty, imm) == 0,
        select: false,
    },
};

/// The scratch registers: a result bound for a slot, a call's target and a
/// far jump's; the first operand and a cycle; the second; a third, a
/// sequence's own temporary, and an offset too far to encode.
const S0: u8 = reg::T0;
const S1: u8 = reg::T1;
const S2: u8 = reg::T2;
const S3: u8 = reg::T3;

/// The function lowered: its instructions and how many labels they bind.
pub(crate) struct Lowered {
    /// The instructions.
    pub(crate) insts: Vec<Inst>,
    /// Labels made.
    pub(crate) labels: usize,
}

/// `func`, verified, lowered and encoded.
///
/// # Errors
///
/// Why it could not be.
pub(crate) fn build(func: &Function, target: Target) -> Result<(Lowered, Vec<u8>), CompileError> {
    let mut backend = Risc {
        zbb: target.has(CpuFeature::Zbb),
        ..Risc::default()
    };
    lower::lower(func, &MACHINE, &mut backend)?;
    let lowered = Lowered {
        insts: backend.insts,
        labels: backend.labels,
    };
    let bytes = inst::encode(&lowered.insts, lowered.labels)?;
    Ok((lowered, bytes))
}

/// What a register holds for a constant `imm` of `ty`: a 32-bit one
/// sign-extended.
fn held(ty: Type, imm: u64) -> i64 {
    match ty {
        Type::I32 => sext32(imm),
        Type::I64 => imm.cast_signed(),
    }
}

/// The low twelve bits of `value`, sign-extended.
fn low12(value: i64) -> i16 {
    let bits = crate::bits::low16(value.cast_unsigned()) << 4;
    bits.cast_signed() >> 4
}

/// `value` as a twelve-bit immediate, where one holds it.
fn imm12(value: i64) -> Option<i16> {
    i16::try_from(value)
        .ok()
        .filter(|imm| (-2048..=2047).contains(imm))
}

/// The immediate a comparison of `cond` against the constant `c` held as
/// `held` is made with: `slt`/`sltu` against `c`, or `c + 1` for the
/// comparisons that include it.
fn compare_imm(cond: IrCond, held: i64) -> Option<i16> {
    match cond {
        IrCond::Eq | IrCond::Ne | IrCond::Slt | IrCond::Sge | IrCond::Ult | IrCond::Uge => {
            imm12(held)
        }
        IrCond::Sle | IrCond::Sgt => imm12(held.checked_add(1)?),
        // `c + 1` wraps where `c` is all ones.
        IrCond::Ule | IrCond::Ugt if held != -1 => imm12(held + 1),
        IrCond::Ule | IrCond::Ugt => None,
    }
}

/// Whether operand `operand` of `inst`, a constant `imm` of `ty`, is folded.
fn folds(inst: &Ir, operand: usize, ty: Type, imm: u64) -> bool {
    let held = held(ty, imm);
    match *inst {
        Ir::Binary { op, .. } if operand == 1 => match op {
            BinOp::Add | BinOp::And | BinOp::Or | BinOp::Xor => imm12(held).is_some(),
            BinOp::Sub => held.checked_neg().and_then(imm12).is_some(),
            BinOp::Shl | BinOp::Shr | BinOp::Sar | BinOp::Rotl | BinOp::Rotr => true,
            BinOp::Mul | BinOp::Udiv | BinOp::Sdiv | BinOp::Urem | BinOp::Srem => false,
        },
        Ir::Icmp { cond, .. } if operand == 1 => compare_imm(cond, held).is_some(),
        // Zero is the zero register: a select's choices and a store's value
        // read it as one.
        Ir::Select { .. } if operand != 0 => held == 0,
        Ir::Store { .. } if operand == 0 => held == 0,
        _ => false,
    }
}

/// The size a load or store of `width` moves.
const fn size(width: Width) -> Size {
    match width {
        Width::W8 => Size::B,
        Width::W16 => Size::H,
        Width::W32 => Size::W,
        Width::W64 => Size::D,
    }
}

/// The branch taken where `a cond b` holds: its condition, and whether its
/// operands are swapped.
const fn branch(cond: IrCond) -> (Cond, bool) {
    match cond {
        IrCond::Eq => (Cond::Eq, false),
        IrCond::Ne => (Cond::Ne, false),
        IrCond::Slt => (Cond::Lt, false),
        IrCond::Sge => (Cond::Ge, false),
        IrCond::Sgt => (Cond::Lt, true),
        IrCond::Sle => (Cond::Ge, true),
        IrCond::Ult => (Cond::Ltu, false),
        IrCond::Uge => (Cond::Geu, false),
        IrCond::Ugt => (Cond::Ltu, true),
        IrCond::Ule => (Cond::Geu, true),
    }
}

/// The backend's state while lowering.
#[derive(Default)]
struct Risc {
    insts: Vec<Inst>,
    labels: usize,
    /// The bytes the prologue took off the stack pointer for its saves.
    saves: i16,
    /// Whether Zbb's instructions may be used.
    zbb: bool,
}

impl Risc {
    fn emit(&mut self, inst: Inst) {
        self.insts.push(inst);
    }

    fn imm(&mut self, op: ImmOp, word: bool, (rd, rs1): (u8, u8), imm: i16) {
        self.emit(Inst::Imm {
            op,
            word,
            rd,
            rs1,
            imm,
        });
    }

    fn reg(&mut self, op: Op, word: bool, (rd, rs1, rs2): (u8, u8, u8)) {
        self.emit(Inst::Reg {
            op,
            word,
            rd,
            rs1,
            rs2,
        });
    }

    /// `rd = value`, in the fewest instructions this recursion finds: a
    /// sign-extended 32-bit value as `lui` and `addiw`, anything wider its
    /// upper bits made, shifted up, and its low twelve added.
    fn constant(&mut self, rd: u8, value: i64) {
        let low = low12(value);
        if i32::try_from(value).is_ok() {
            let high = crate::bits::low32((value - i64::from(low)).cast_unsigned()) >> 12;
            if high == 0 {
                self.imm(ImmOp::Addi, false, (rd, reg::ZERO), low);
                return;
            }
            self.emit(Inst::Lui { rd, imm: high });
            if low != 0 {
                self.imm(ImmOp::Addi, true, (rd, rd), low);
            }
            return;
        }
        // Two's complement: the sum below wraps back to `value`.
        let upper = value.wrapping_sub(i64::from(low)) >> 12;
        let shift = upper.trailing_zeros();
        self.constant(rd, upper >> shift);
        self.imm(
            ImmOp::Slli,
            false,
            (rd, rd),
            i16::from(crate::bits::low8(u64::from(shift + 12))),
        );
        if low != 0 {
            self.imm(ImmOp::Addi, false, (rd, rd), low);
        }
    }

    /// `base + offset` as a register and a twelve-bit offset: `base` and
    /// `offset` where it fits, else `scratch` holding the sum.
    fn address(&mut self, base: u8, offset: i64, scratch: u8) -> (u8, i16) {
        if let Some(imm) = imm12(offset) {
            return (base, imm);
        }
        self.constant(scratch, offset);
        self.reg(Op::Add, false, (scratch, base, scratch));
        (scratch, 0)
    }

    /// The bytes of slot `slot` above the stack pointer.
    fn slot(slot: u32) -> i64 {
        i64::from(slot) * 8
    }

    /// The register holding the value at `loc`: its own, or `scratch` with
    /// the value loaded into it.
    fn get(&mut self, loc: Loc, scratch: u8) -> u8 {
        match loc {
            Loc::Reg(reg) => reg,
            Loc::Slot(slot) => {
                let (rs1, offset) = self.address(reg::SP, Self::slot(slot), scratch);
                self.emit(Inst::Load {
                    size: Size::D,
                    signed: true,
                    rd: scratch,
                    rs1,
                    offset,
                });
                scratch
            }
        }
    }

    /// The register a result for `loc` is computed in: S0 where it is bound
    /// for a slot or nothing needs it.
    const fn result(loc: Option<Loc>) -> u8 {
        match loc {
            Some(Loc::Reg(reg)) => reg,
            Some(Loc::Slot(_)) | None => S0,
        }
    }

    /// Put the result computed in `reg`, never S3, where `loc` is.
    fn put(&mut self, loc: Option<Loc>, reg: u8) {
        if let Some(Loc::Slot(slot)) = loc {
            let (rs1, offset) = self.address(reg::SP, Self::slot(slot), S3);
            self.emit(Inst::Store {
                size: Size::D,
                rs2: reg,
                rs1,
                offset,
            });
        }
    }

    /// `rd = rs`.
    fn mov(&mut self, rd: u8, rs: u8) {
        self.imm(ImmOp::Addi, false, (rd, rs), 0);
    }

    /// `rd = rs`'s low 32 bits, zero-extended.
    fn zero_extend(&mut self, rd: u8, rs: u8) {
        self.imm(ImmOp::Slli, false, (rd, rs), 32);
        self.imm(ImmOp::Srli, false, (rd, rd), 32);
    }

    /// Move the whole word at `from` to `to`, through `scratch` between two
    /// slots.
    fn copy(&mut self, from: Loc, to: Loc, scratch: u8) {
        match (from, to) {
            (Loc::Reg(src), Loc::Reg(dst)) => self.mov(dst, src),
            (from, Loc::Reg(dst)) => {
                self.get(from, dst);
            }
            (Loc::Reg(src), to) => self.put(Some(to), src),
            (from, to) => {
                let held = self.get(from, scratch);
                self.put(Some(to), held);
            }
        }
    }

    fn binary(
        &mut self,
        cx: &Context<'_>,
        inst: &Ir,
        (op, a, b): (BinOp, Value, Value),
        d: Option<Loc>,
    ) -> Result<(), CompileError> {
        let ty = cx.ty(a);
        let word = ty == Type::I32;
        let ra = self.get(cx.loc(a)?, S1);
        let rd = Self::result(d);
        if let Some(imm) = cx.folded(inst, 1, b) {
            self.binary_imm(op, ty, (rd, ra), imm)?;
            self.put(d, rd);
            return Ok(());
        }
        let rb = self.get(cx.loc(b)?, S2);
        let simple = match op {
            BinOp::Add => Some((Op::Add, word)),
            BinOp::Sub => Some((Op::Sub, word)),
            BinOp::And => Some((Op::And, false)),
            BinOp::Or => Some((Op::Or, false)),
            BinOp::Xor => Some((Op::Xor, false)),
            BinOp::Mul => Some((Op::Mul, word)),
            BinOp::Shl => Some((Op::Sll, word)),
            BinOp::Shr => Some((Op::Srl, word)),
            BinOp::Sar => Some((Op::Sra, word)),
            BinOp::Urem => Some((Op::Remu, word)),
            BinOp::Srem => Some((Op::Rem, word)),
            BinOp::Rotl if self.zbb => Some((Op::Rol, word)),
            BinOp::Rotr if self.zbb => Some((Op::Ror, word)),
            BinOp::Udiv | BinOp::Sdiv | BinOp::Rotl | BinOp::Rotr => None,
        };
        match (simple, op) {
            (Some((op, word)), _) => self.reg(op, word, (rd, ra, rb)),
            (None, BinOp::Udiv | BinOp::Sdiv) => {
                // The quotient by zero is all ones: masked by "the divisor
                // is not zero" spread over the word, zero.
                let divide = if op == BinOp::Udiv { Op::Divu } else { Op::Div };
                self.reg(divide, word, (rd, ra, rb));
                self.reg(Op::Sltu, false, (S3, reg::ZERO, rb));
                self.reg(Op::Sub, false, (S3, reg::ZERO, S3));
                self.reg(Op::And, false, (rd, rd, S3));
            }
            (None, _) => {
                // Each way by the count, the other by its negation, which
                // the shift takes modulo the width.
                let (first, second) = if op == BinOp::Rotl {
                    (Op::Sll, Op::Srl)
                } else {
                    (Op::Srl, Op::Sll)
                };
                self.reg(first, word, (S3, ra, rb));
                self.reg(Op::Sub, false, (rd, reg::ZERO, rb));
                self.reg(second, word, (rd, ra, rd));
                self.reg(Op::Or, false, (rd, rd, S3));
            }
        }
        self.put(d, rd);
        Ok(())
    }

    /// `rd = ra op imm`, the immediate one the target folds.
    fn binary_imm(
        &mut self,
        op: BinOp,
        ty: Type,
        (rd, ra): (u8, u8),
        imm: u64,
    ) -> Result<(), CompileError> {
        let word = ty == Type::I32;
        let held = held(ty, imm);
        let bits = i16::try_from(ty.bits()).map_err(|_| CompileError::Malformed)?;
        let n = i16::try_from(imm % u64::from(ty.bits())).map_err(|_| CompileError::Malformed)?;
        let fit = |v: Option<i64>| v.and_then(imm12).ok_or(CompileError::Malformed);
        match op {
            BinOp::Add => self.imm(ImmOp::Addi, word, (rd, ra), fit(Some(held))?),
            BinOp::Sub => self.imm(ImmOp::Addi, word, (rd, ra), fit(held.checked_neg())?),
            BinOp::And => self.imm(ImmOp::Andi, false, (rd, ra), fit(Some(held))?),
            BinOp::Or => self.imm(ImmOp::Ori, false, (rd, ra), fit(Some(held))?),
            BinOp::Xor => self.imm(ImmOp::Xori, false, (rd, ra), fit(Some(held))?),
            BinOp::Shl => self.imm(ImmOp::Slli, word, (rd, ra), n),
            BinOp::Shr => self.imm(ImmOp::Srli, word, (rd, ra), n),
            BinOp::Sar => self.imm(ImmOp::Srai, word, (rd, ra), n),
            BinOp::Rotl | BinOp::Rotr if n == 0 => self.imm(ImmOp::Addi, word, (rd, ra), 0),
            BinOp::Rotl | BinOp::Rotr if self.zbb => {
                let right = if op == BinOp::Rotr { n } else { bits - n };
                self.imm(ImmOp::Rori, word, (rd, ra), right);
            }
            BinOp::Rotl | BinOp::Rotr => {
                let left = if op == BinOp::Rotl { n } else { bits - n };
                self.imm(ImmOp::Slli, word, (S3, ra), left);
                self.imm(ImmOp::Srli, word, (rd, ra), bits - left);
                self.reg(Op::Or, false, (rd, rd, S3));
            }
            BinOp::Mul | BinOp::Udiv | BinOp::Sdiv | BinOp::Urem | BinOp::Srem => {
                return Err(CompileError::Malformed)
            }
        }
        Ok(())
    }

    fn unary(
        &mut self,
        cx: &Context<'_>,
        op: UnOp,
        a: Value,
        d: Option<Loc>,
    ) -> Result<(), CompileError> {
        let word = cx.ty(a) == Type::I32;
        let ra = self.get(cx.loc(a)?, S1);
        let rd = Self::result(d);
        let field = |this: &mut Self, keep: i16, signed: bool| {
            this.imm(ImmOp::Slli, false, (rd, ra), 64 - keep);
            this.imm(
                if signed { ImmOp::Srai } else { ImmOp::Srli },
                false,
                (rd, rd),
                64 - keep,
            );
        };
        let ext = |ext| Inst::Ext { ext, rd, rs1: ra };
        match op {
            UnOp::Neg => self.reg(Op::Sub, word, (rd, reg::ZERO, ra)),
            UnOp::Not => self.imm(ImmOp::Xori, false, (rd, ra), -1),
            UnOp::Uext8 => self.imm(ImmOp::Andi, false, (rd, ra), 0xFF),
            UnOp::Uext16 if self.zbb => self.emit(ext(Ext::ZextH)),
            UnOp::Sext8 if self.zbb => self.emit(ext(Ext::SextB)),
            UnOp::Sext16 if self.zbb => self.emit(ext(Ext::SextH)),
            UnOp::Uext16 => field(self, 16, false),
            UnOp::Sext8 => field(self, 8, true),
            UnOp::Sext16 => field(self, 16, true),
        }
        self.put(d, rd);
        Ok(())
    }

    fn compare(
        &mut self,
        cx: &Context<'_>,
        inst: &Ir,
        (cond, a, b): (IrCond, Value, Value),
        d: Option<Loc>,
    ) -> Result<(), CompileError> {
        let ra = self.get(cx.loc(a)?, S1);
        let rd = Self::result(d);
        let folded = cx.folded(inst, 1, b);
        if let Some(imm) = folded {
            let imm = compare_imm(cond, held(cx.ty(b), imm)).ok_or(CompileError::Malformed)?;
            match cond {
                IrCond::Eq | IrCond::Ne => {
                    self.imm(ImmOp::Xori, false, (rd, ra), imm);
                    self.equality(cond, rd);
                }
                IrCond::Slt | IrCond::Sge | IrCond::Sle | IrCond::Sgt => {
                    self.imm(ImmOp::Slti, false, (rd, ra), imm);
                }
                IrCond::Ult | IrCond::Uge | IrCond::Ule | IrCond::Ugt => {
                    self.imm(ImmOp::Sltiu, false, (rd, ra), imm);
                }
            }
        } else {
            let rb = self.get(cx.loc(b)?, S2);
            match cond {
                IrCond::Eq | IrCond::Ne => {
                    self.reg(Op::Xor, false, (rd, ra, rb));
                    self.equality(cond, rd);
                }
                IrCond::Slt | IrCond::Sge => self.reg(Op::Slt, false, (rd, ra, rb)),
                IrCond::Sgt | IrCond::Sle => self.reg(Op::Slt, false, (rd, rb, ra)),
                IrCond::Ult | IrCond::Uge => self.reg(Op::Sltu, false, (rd, ra, rb)),
                IrCond::Ugt | IrCond::Ule => self.reg(Op::Sltu, false, (rd, rb, ra)),
            }
        }
        // Against a constant `c`, `a <= c` is `a < c + 1`; against a
        // register, `a > b` is `b < a`: what was made is the comparison or
        // its inverse.
        let inverse = if folded.is_some() {
            matches!(cond, IrCond::Sge | IrCond::Sgt | IrCond::Uge | IrCond::Ugt)
        } else {
            matches!(cond, IrCond::Sge | IrCond::Sle | IrCond::Uge | IrCond::Ule)
        };
        if inverse {
            self.imm(ImmOp::Xori, false, (rd, rd), 1);
        }
        self.put(d, rd);
        Ok(())
    }

    /// `rd = rd == 0` for `Eq`, `rd != 0` for `Ne`.
    fn equality(&mut self, cond: IrCond, rd: u8) {
        if cond == IrCond::Eq {
            self.imm(ImmOp::Sltiu, false, (rd, rd), 1);
        } else {
            self.reg(Op::Sltu, false, (rd, reg::ZERO, rd));
        }
    }

    fn select(
        &mut self,
        cx: &Context<'_>,
        inst: &Ir,
        (c, a, b): (Value, Value, Value),
        d: Option<Loc>,
    ) -> Result<(), CompileError> {
        let rc = self.get(cx.loc(c)?, S3);
        let ra = self.zero_or(cx, (inst, 1, a), S1)?;
        let rb = self.zero_or(cx, (inst, 2, b), S2)?;
        let rd = Self::result(d);
        // `b ^ ((a ^ b) & mask)`, the mask all ones where the condition is
        // not zero.
        self.reg(Op::Sltu, false, (S3, reg::ZERO, rc));
        self.reg(Op::Sub, false, (S3, reg::ZERO, S3));
        self.reg(Op::Xor, false, (rd, ra, rb));
        self.reg(Op::And, false, (rd, rd, S3));
        self.reg(Op::Xor, false, (rd, rd, rb));
        self.put(d, rd);
        Ok(())
    }

    fn load(&mut self, cx: &Context<'_>, inst: &Ir, d: Option<Loc>) -> Result<(), CompileError> {
        let Ir::Load {
            width,
            signed,
            ty,
            base,
            offset,
        } = *inst
        else {
            return Err(CompileError::Malformed);
        };
        let rn = self.get(cx.loc(base)?, S1);
        let (rs1, offset) = self.address(rn, i64::from(offset), S3);
        let rd = Self::result(d);
        // A 32-bit value is held sign-extended, whichever way its bytes
        // were.
        let signed = match (width, ty) {
            (Width::W64, _) | (Width::W32, Type::I32) => true,
            _ => signed,
        };
        self.emit(Inst::Load {
            size: size(width),
            signed,
            rd,
            rs1,
            offset,
        });
        self.put(d, rd);
        Ok(())
    }

    /// The register holding operand `operand` of `inst`, `value`: the zero
    /// register where it is a zero folded in, else its own or `scratch`.
    fn zero_or(
        &mut self,
        cx: &Context<'_>,
        (inst, operand, value): (&Ir, usize, Value),
        scratch: u8,
    ) -> Result<u8, CompileError> {
        if cx.folded(inst, operand, value).is_some() {
            return Ok(reg::ZERO);
        }
        Ok(self.get(cx.loc(value)?, scratch))
    }

    fn store(
        &mut self,
        cx: &Context<'_>,
        inst: &Ir,
        (width, value, base, offset): (Width, Value, Value, i32),
    ) -> Result<(), CompileError> {
        let rv = self.zero_or(cx, (inst, 0, value), S1)?;
        let rn = self.get(cx.loc(base)?, S2);
        let (rs1, offset) = self.address(rn, i64::from(offset), S3);
        self.emit(Inst::Store {
            size: size(width),
            rs2: rv,
            rs1,
            offset,
        });
        Ok(())
    }

    fn call(
        &mut self,
        cx: &Context<'_>,
        (table, index): (Value, u32),
        args: &[Value],
        ret: Option<(Loc, Type)>,
    ) -> Result<(), CompileError> {
        let rt = self.get(cx.loc(table)?, S1);
        let (rs1, offset) = self.address(rt, i64::from(index) * 8, S3);
        self.emit(Inst::Load {
            size: Size::D,
            signed: true,
            rd: S0,
            rs1,
            offset,
        });
        let moves = args
            .iter()
            .zip(ARGS)
            .map(|(&arg, reg)| {
                Ok(Move {
                    from: cx.loc(arg)?,
                    to: Loc::Reg(reg),
                })
            })
            .collect::<Result<Vec<_>, CompileError>>()?;
        for step in sequence(&moves, Loc::Reg(S1)) {
            self.copy(step.from, step.to, S2);
        }
        for (&arg, reg) in args.iter().zip(ARGS) {
            if cx.ty(arg) == Type::I32 {
                self.zero_extend(reg, reg);
            }
        }
        self.emit(Inst::Jalr {
            rd: reg::RA,
            rs1: S0,
            offset: 0,
        });
        if let Some((d, ty)) = ret {
            let rd = Self::result(Some(d));
            // A 32-bit answer is in the low half, whatever is above it.
            self.imm(ImmOp::Addi, ty == Type::I32, (rd, reg::A0), 0);
            self.put(Some(d), rd);
        }
        Ok(())
    }

    /// `sp = sp - bytes`, or `+` where `add`, through S0 where no immediate
    /// reaches.
    fn adjust_sp(&mut self, add: bool, bytes: u64) -> Result<(), CompileError> {
        let bytes = i64::try_from(bytes).map_err(|_| CompileError::TooLarge)?;
        let delta = if add { bytes } else { -bytes };
        if delta == 0 {
            return Ok(());
        }
        if let Some(imm) = imm12(delta) {
            self.imm(ImmOp::Addi, false, (reg::SP, reg::SP), imm);
        } else {
            self.constant(S0, delta);
            self.reg(Op::Add, false, (reg::SP, reg::SP, S0));
        }
        Ok(())
    }
}

impl Backend for Risc {
    fn temp(&self) -> Loc {
        Loc::Reg(S1)
    }

    fn arg_regs(&self) -> &'static [u8] {
        &ARGS
    }

    fn label(&mut self) -> Label {
        let label = Label(self.labels);
        self.labels += 1;
        label
    }

    fn bind(&mut self, label: Label) {
        self.emit(Inst::Bind(label));
    }

    fn prologue(&mut self, alloc: &Allocation) -> Result<(), CompileError> {
        self.imm(ImmOp::Addi, false, (reg::SP, reg::SP), -16);
        self.emit(Inst::Store {
            size: Size::D,
            rs2: reg::RA,
            rs1: reg::SP,
            offset: 8,
        });
        self.emit(Inst::Store {
            size: Size::D,
            rs2: reg::S0,
            rs1: reg::SP,
            offset: 0,
        });
        self.imm(ImmOp::Addi, false, (reg::S0, reg::SP), 16);
        let saves = (alloc.saved.len() * 8).next_multiple_of(16);
        self.saves = i16::try_from(saves).map_err(|_| CompileError::TooLarge)?;
        if self.saves > 0 {
            self.imm(ImmOp::Addi, false, (reg::SP, reg::SP), -self.saves);
            for (at, &saved) in (0i16..).step_by(8).zip(&alloc.saved) {
                self.emit(Inst::Store {
                    size: Size::D,
                    rs2: saved,
                    rs1: reg::SP,
                    offset: at,
                });
            }
        }
        self.adjust_sp(false, (u64::from(alloc.slots) * 8).next_multiple_of(16))
    }

    fn arrived(&mut self, loc: Loc, ty: Type) -> Result<(), CompileError> {
        if ty == Type::I32 {
            // Passed zero-extended, held sign-extended.
            let r = self.get(loc, S0);
            self.imm(ImmOp::Addi, true, (r, r), 0);
            self.put(Some(loc), r);
        }
        Ok(())
    }

    fn mov(&mut self, from: Loc, to: Loc) -> Result<(), CompileError> {
        self.copy(from, to, S0);
        Ok(())
    }

    fn inst(
        &mut self,
        cx: &Context<'_>,
        defined: Option<Value>,
        inst: &Ir,
    ) -> Result<(), CompileError> {
        let d = cx.dest(defined);
        match *inst {
            Ir::Const { ty, imm } => {
                let rd = Self::result(d);
                self.constant(rd, held(ty, imm));
                self.put(d, rd);
                Ok(())
            }
            Ir::Unary { op, a } => self.unary(cx, op, a, d),
            Ir::Binary { op, a, b } => self.binary(cx, inst, (op, a, b), d),
            Ir::Icmp { cond, a, b } => self.compare(cx, inst, (cond, a, b), d),
            Ir::Select { cond, a, b } => self.select(cx, inst, (cond, a, b), d),
            Ir::Extend { signed, a } => {
                let ra = self.get(cx.loc(a)?, S1);
                let rd = Self::result(d);
                if signed {
                    self.mov(rd, ra);
                } else {
                    self.zero_extend(rd, ra);
                }
                self.put(d, rd);
                Ok(())
            }
            Ir::Reduce { a } => {
                let ra = self.get(cx.loc(a)?, S1);
                let rd = Self::result(d);
                self.imm(ImmOp::Addi, true, (rd, ra), 0);
                self.put(d, rd);
                Ok(())
            }
            Ir::Load { .. } => self.load(cx, inst, d),
            Ir::Store {
                width,
                value,
                base,
                offset,
            } => self.store(cx, inst, (width, value, base, offset)),
            Ir::Call {
                table,
                index,
                ref args,
                ret,
            } => self.call(cx, (table, index), args, d.zip(ret)),
        }
    }

    fn jump(&mut self, label: Label) {
        self.emit(Inst::Jump(label));
    }

    fn jump_unless(
        &mut self,
        cx: &Context<'_>,
        test: Test,
        label: Label,
    ) -> Result<(), CompileError> {
        let (cond, rs1, rs2) = match test {
            Test::Value(cond) => (Cond::Eq, self.get(cond, S1), reg::ZERO),
            Test::Compare { cond, a, b, imm } => {
                let ra = self.get(cx.loc(a)?, S1);
                let rb = match imm {
                    Some(0) => reg::ZERO,
                    Some(_) => return Err(CompileError::Malformed),
                    None => self.get(cx.loc(b)?, S2),
                };
                let (taken, swapped) = branch(cond.inverse());
                if swapped {
                    (taken, rb, ra)
                } else {
                    (taken, ra, rb)
                }
            }
        };
        self.emit(Inst::Branch {
            cond,
            rs1,
            rs2,
            label,
        });
        Ok(())
    }

    fn ret(&mut self, value: Option<(Loc, Type)>, alloc: &Allocation) -> Result<(), CompileError> {
        if let Some((loc, ty)) = value {
            self.copy(loc, Loc::Reg(reg::A0), S0);
            if ty == Type::I32 {
                self.zero_extend(reg::A0, reg::A0);
            }
        }
        // The saves sit just below the frame record, wherever the slots
        // left the stack pointer.
        self.imm(ImmOp::Addi, false, (reg::SP, reg::S0), -16 - self.saves);
        for (at, &saved) in (0i16..).step_by(8).zip(&alloc.saved) {
            self.emit(Inst::Load {
                size: Size::D,
                signed: true,
                rd: saved,
                rs1: reg::SP,
                offset: at,
            });
        }
        if self.saves > 0 {
            self.imm(ImmOp::Addi, false, (reg::SP, reg::SP), self.saves);
        }
        self.emit(Inst::Load {
            size: Size::D,
            signed: true,
            rd: reg::RA,
            rs1: reg::SP,
            offset: 8,
        });
        self.emit(Inst::Load {
            size: Size::D,
            signed: true,
            rd: reg::S0,
            rs1: reg::SP,
            offset: 0,
        });
        self.imm(ImmOp::Addi, false, (reg::SP, reg::SP), 16);
        self.emit(Inst::Jalr {
            rd: reg::ZERO,
            rs1: reg::RA,
            offset: 0,
        });
        Ok(())
    }
}

#[cfg(test)]
#[path = "riscv64_tests.rs"]
mod tests;
