//! The aarch64 backend, AAPCS64 calling convention. Each function makes a
//! frame record (`stp x29, x30, [sp, #-16]!; mov x29, sp`), so a
//! frame-pointer walk from a helper it calls passes through it. X0–X14 and
//! X19–X28 are allocated; X15, X16 and X17 are the backend's own scratch,
//! X16 also a call's target and X17 a parallel move's cycle; X18, the
//! platform's, is never touched. A 32-bit value is held with its register's
//! upper half clear, as every 32-bit operation leaves it. Division and its
//! remainder are total as the processor makes them: by zero the quotient is
//! zero, and the most negative dividend over minus one wraps. The second
//! operand of an addition, subtraction or comparison is folded where it, or
//! its negation, is a twelve-bit immediate, shifted or not; of a logical
//! operation where it is a logical immediate; and a shift or rotate count
//! wherever it is a constant.

pub(crate) mod inst;
#[cfg(test)]
mod render;
#[cfg(any(test, feature = "test-util"))]
pub(crate) mod sim;

use alloc::vec::Vec;

use crate::ir::{BinOp, Cond as IrCond, Function, Inst as Ir, Type, UnOp, Value, Width};
use crate::lower::{self, Backend, Context, Label, Machine, Test};
use crate::moves::{sequence, Move};
use crate::regalloc::{Allocation, Loc, RegClass};
use crate::uses::Folding;
use crate::CompileError;
use inst::{bitmask, reg, Cond, Dp2, Inst, Logic, Offset, Size};

/// The registers a call keeps that are allocated: all of them but the frame
/// pointer.
pub(crate) const CALLEE_SAVED: [u8; 10] = [19, 20, 21, 22, 23, 24, 25, 26, 27, 28];

/// The registers the arguments of a call arrive in.
pub(crate) const ARGS: [u8; 6] = [0, 1, 2, 3, 4, 5];

/// The target as the shared lowering sees it.
const MACHINE: Machine = Machine {
    regs: RegClass::new(
        &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14],
        &CALLEE_SAVED,
    ),
    folding: Folding {
        inst: folds,
        branch: |_, ty, imm| add_imm(true, ty, imm).is_some(),
        select: true,
    },
};

/// The scratch registers: the first operand and a result bound for a slot;
/// the second operand; a third, and an offset too far to encode.
const S0: u8 = reg::X16;
const S1: u8 = reg::X17;
const S2: u8 = reg::X15;

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
pub(crate) fn build(func: &Function) -> Result<(Lowered, Vec<u8>), CompileError> {
    let mut backend = Arm::default();
    lower::lower(func, &MACHINE, &mut backend)?;
    let lowered = Lowered {
        insts: backend.insts,
        labels: backend.labels,
    };
    let bytes = inst::encode(&lowered.insts, lowered.labels)?;
    Ok((lowered, bytes))
}

/// An addition's or subtraction's twelve-bit immediate, shifted or not.
fn imm12(value: u64) -> Option<(u16, bool)> {
    if value < 1 << 12 {
        Some((u16::try_from(value).ok()?, false))
    } else if value.trailing_zeros() >= 12 && value < 1 << 24 {
        Some((u16::try_from(value >> 12).ok()?, true))
    } else {
        None
    }
}

/// How `x + imm` (or `x - imm` where `sub`) of `ty` is made from an
/// immediate: the operation, which subtracts where it says so, and its
/// encoding.
fn add_imm(sub: bool, ty: Type, imm: u64) -> Option<(bool, u16, bool)> {
    let value = ty.wrap(imm);
    if let Some((imm, high)) = imm12(value) {
        return Some((sub, imm, high));
    }
    let (imm, high) = imm12(ty.wrap(value.wrapping_neg()))?;
    Some((!sub, imm, high))
}

/// Whether operand `operand` of `inst`, a constant `imm` of `ty`, is folded.
fn folds(inst: &Ir, operand: usize, ty: Type, imm: u64) -> bool {
    match *inst {
        Ir::Binary { op, .. } if operand == 1 => match op {
            BinOp::Add | BinOp::Sub => add_imm(false, ty, imm).is_some(),
            BinOp::And | BinOp::Or | BinOp::Xor => bitmask(imm, ty == Type::I64).is_some(),
            BinOp::Shl | BinOp::Shr | BinOp::Sar | BinOp::Rotl | BinOp::Rotr => true,
            BinOp::Mul | BinOp::Udiv | BinOp::Sdiv | BinOp::Urem | BinOp::Srem => false,
        },
        Ir::Icmp { .. } if operand == 1 => add_imm(true, ty, imm).is_some(),
        // Zero is the zero register: a select's choices and a store's value
        // read it as one.
        Ir::Select { .. } if operand != 0 => imm == 0,
        Ir::Store { .. } if operand == 0 => imm == 0,
        _ => false,
    }
}

/// The condition a comparison is.
const fn cond(cond: IrCond) -> Cond {
    match cond {
        IrCond::Eq => Cond::Eq,
        IrCond::Ne => Cond::Ne,
        IrCond::Slt => Cond::Lt,
        IrCond::Sle => Cond::Le,
        IrCond::Sgt => Cond::Gt,
        IrCond::Sge => Cond::Ge,
        IrCond::Ult => Cond::Lo,
        IrCond::Ule => Cond::Ls,
        IrCond::Ugt => Cond::Hi,
        IrCond::Uge => Cond::Hs,
    }
}

/// The size a load or store of `width` moves.
const fn size(width: Width) -> Size {
    match width {
        Width::W8 => Size::B,
        Width::W16 => Size::H,
        Width::W32 => Size::W,
        Width::W64 => Size::X,
    }
}

/// The offset form reaching `offset` bytes for an access of `size`, where
/// one does.
fn offset(size: Size, offset: i64) -> Option<Offset> {
    let scale = 1i64 << size.log2();
    if offset >= 0 && offset % scale == 0 && offset / scale < 1 << 12 {
        return Some(Offset::Scaled(u16::try_from(offset / scale).ok()?));
    }
    i16::try_from(offset)
        .ok()
        .filter(|imm| (-256..=255).contains(imm))
        .map(Offset::Unscaled)
}

/// The backend's state while lowering.
#[derive(Default)]
struct Arm {
    insts: Vec<Inst>,
    labels: usize,
    /// The bytes the prologue took off the stack pointer below its saves.
    frame: u64,
}

impl Arm {
    fn emit(&mut self, inst: Inst) {
        self.insts.push(inst);
    }

    /// `rd = imm`, in as few instructions as its halfwords allow.
    fn constant(&mut self, wide: bool, rd: u8, imm: u64) {
        let value = if wide { imm } else { imm & 0xFFFF_FFFF };
        let halves: Vec<u16> = (0..if wide { 4 } else { 2 })
            .map(|n| crate::bits::low16(value >> (16 * n)))
            .collect();
        let zeros = halves.iter().filter(|&&h| h == 0).count();
        let ones = halves.iter().filter(|&&h| h == 0xFFFF).count();
        if zeros + 1 < halves.len() && ones + 1 < halves.len() {
            if let Some(mask) = bitmask(value, wide) {
                self.emit(Inst::LogicImm {
                    op: Logic::Orr,
                    wide,
                    rd,
                    rn: reg::ZR,
                    mask,
                });
                return;
            }
        }
        // The halves to set over what the first instruction leaves: all
        // ones where more halves are all ones than zero, else zero.
        let inverted = ones > zeros;
        let background = if inverted { 0xFFFF } else { 0 };
        let mut first = true;
        for (hw, &half) in (0u8..).zip(&halves) {
            if half == background {
                continue;
            }
            self.emit(match (first, inverted) {
                (true, true) => Inst::Movn {
                    wide,
                    rd,
                    imm: !half,
                    hw,
                },
                (true, false) => Inst::Movz {
                    wide,
                    rd,
                    imm: half,
                    hw,
                },
                (false, _) => Inst::Movk {
                    wide,
                    rd,
                    imm: half,
                    hw,
                },
            });
            first = false;
        }
        if first {
            self.emit(if inverted {
                Inst::Movn {
                    wide,
                    rd,
                    imm: 0,
                    hw: 0,
                }
            } else {
                Inst::Movz {
                    wide,
                    rd,
                    imm: 0,
                    hw: 0,
                }
            });
        }
    }

    /// `[base + offset]` for an access of `size`, through `scratch` where
    /// no immediate reaches.
    fn address(&mut self, size: Size, offset: i64, scratch: u8) -> Offset {
        self::offset(size, offset).unwrap_or_else(|| {
            self.constant(true, scratch, offset.cast_unsigned());
            Offset::Index(scratch)
        })
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
                let offset = self.address(Size::X, Self::slot(slot), scratch);
                self.emit(Inst::Load {
                    size: Size::X,
                    signed: false,
                    wide: true,
                    rt: scratch,
                    rn: reg::SP,
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

    /// Put the result computed in `reg`, never S2, where `loc` is.
    fn put(&mut self, loc: Option<Loc>, reg: u8) {
        if let Some(Loc::Slot(slot)) = loc {
            let offset = self.address(Size::X, Self::slot(slot), S2);
            self.emit(Inst::Store {
                size: Size::X,
                rt: reg,
                rn: reg::SP,
                offset,
            });
        }
    }

    /// `rd = rn`, the whole word or the low half.
    fn mov(&mut self, wide: bool, rd: u8, rn: u8) {
        self.emit(Inst::Logic {
            op: Logic::Orr,
            wide,
            rd,
            rn: reg::ZR,
            rm: rn,
        });
    }

    /// Move the whole word at `from` to `to`, through `scratch` between two
    /// slots.
    fn copy(&mut self, from: Loc, to: Loc, scratch: u8) {
        match (from, to) {
            (Loc::Reg(src), Loc::Reg(dst)) => self.mov(true, dst, src),
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
        let wide = ty == Type::I64;
        let ra = self.get(cx.loc(a)?, S0);
        let rd = Self::result(d);
        if let Some(imm) = cx.folded(inst, 1, b) {
            self.binary_imm(op, ty, (rd, ra), imm)?;
            self.put(d, rd);
            return Ok(());
        }
        let rb = self.get(cx.loc(b)?, S1);
        let dp2 = |op| Inst::Dp2 {
            op,
            wide,
            rd,
            rn: ra,
            rm: rb,
        };
        match op {
            BinOp::Add | BinOp::Sub => self.emit(Inst::AddReg {
                sub: op == BinOp::Sub,
                flags: false,
                wide,
                rd,
                rn: ra,
                rm: rb,
            }),
            BinOp::And | BinOp::Or | BinOp::Xor => self.emit(Inst::Logic {
                op: match op {
                    BinOp::And => Logic::And,
                    BinOp::Or => Logic::Orr,
                    _ => Logic::Eor,
                },
                wide,
                rd,
                rn: ra,
                rm: rb,
            }),
            BinOp::Mul => self.emit(Inst::Madd {
                sub: false,
                wide,
                rd,
                rn: ra,
                rm: rb,
                ra: reg::ZR,
            }),
            BinOp::Udiv => self.emit(dp2(Dp2::Udiv)),
            BinOp::Sdiv => self.emit(dp2(Dp2::Sdiv)),
            BinOp::Urem | BinOp::Srem => {
                let divide = if op == BinOp::Urem {
                    Dp2::Udiv
                } else {
                    Dp2::Sdiv
                };
                self.emit(Inst::Dp2 {
                    op: divide,
                    wide,
                    rd: S2,
                    rn: ra,
                    rm: rb,
                });
                self.emit(Inst::Madd {
                    sub: true,
                    wide,
                    rd,
                    rn: S2,
                    rm: rb,
                    ra,
                });
            }
            BinOp::Shl => self.emit(dp2(Dp2::Lslv)),
            BinOp::Shr => self.emit(dp2(Dp2::Lsrv)),
            BinOp::Sar => self.emit(dp2(Dp2::Asrv)),
            BinOp::Rotr => self.emit(dp2(Dp2::Rorv)),
            BinOp::Rotl => {
                // A rotation left is one right by the count's negation,
                // which the processor takes modulo the width.
                self.emit(Inst::AddReg {
                    sub: true,
                    flags: false,
                    wide,
                    rd: S2,
                    rn: reg::ZR,
                    rm: rb,
                });
                self.emit(Inst::Dp2 {
                    op: Dp2::Rorv,
                    wide,
                    rd,
                    rn: ra,
                    rm: S2,
                });
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
        let wide = ty == Type::I64;
        let bits = ty.bits();
        let n = crate::bits::low8(imm % u64::from(bits));
        let top = crate::bits::low8(u64::from(bits - 1));
        let width = crate::bits::low8(u64::from(bits));
        match op {
            BinOp::Add | BinOp::Sub => {
                let (sub, imm, high) =
                    add_imm(op == BinOp::Sub, ty, imm).ok_or(CompileError::Malformed)?;
                self.emit(Inst::AddImm {
                    sub,
                    flags: false,
                    wide,
                    rd,
                    rn: ra,
                    imm,
                    high,
                });
            }
            BinOp::And | BinOp::Or | BinOp::Xor => self.emit(Inst::LogicImm {
                op: match op {
                    BinOp::And => Logic::And,
                    BinOp::Or => Logic::Orr,
                    _ => Logic::Eor,
                },
                wide,
                rd,
                rn: ra,
                mask: bitmask(imm, wide).ok_or(CompileError::Malformed)?,
            }),
            BinOp::Mul | BinOp::Udiv | BinOp::Sdiv | BinOp::Urem | BinOp::Srem => {
                return Err(CompileError::Malformed)
            }
            _ if n == 0 => self.mov(wide, rd, ra),
            BinOp::Shl => self.emit(Inst::Bfm {
                signed: false,
                wide,
                rd,
                rn: ra,
                immr: width - n,
                imms: top - n,
            }),
            BinOp::Shr | BinOp::Sar => self.emit(Inst::Bfm {
                signed: op == BinOp::Sar,
                wide,
                rd,
                rn: ra,
                immr: n,
                imms: top,
            }),
            BinOp::Rotr | BinOp::Rotl => self.emit(Inst::Extr {
                wide,
                rd,
                rn: ra,
                rm: ra,
                lsb: if op == BinOp::Rotr { n } else { width - n },
            }),
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
        let wide = cx.ty(a) == Type::I64;
        let ra = self.get(cx.loc(a)?, S0);
        let rd = Self::result(d);
        let field = |signed, wide, imms| Inst::Bfm {
            signed,
            wide,
            rd,
            rn: ra,
            immr: 0,
            imms,
        };
        self.emit(match op {
            UnOp::Neg => Inst::AddReg {
                sub: true,
                flags: false,
                wide,
                rd,
                rn: reg::ZR,
                rm: ra,
            },
            UnOp::Not => Inst::Logic {
                op: Logic::Orn,
                wide,
                rd,
                rn: reg::ZR,
                rm: ra,
            },
            // A 32-bit field zero-extends to 64 bits on its own.
            UnOp::Uext8 => field(false, false, 7),
            UnOp::Uext16 => field(false, false, 15),
            UnOp::Sext8 => field(true, wide, 7),
            UnOp::Sext16 => field(true, wide, 15),
        });
        self.put(d, rd);
        Ok(())
    }

    /// The flags of `a` against `b`, or against `imm` where it is folded.
    fn flags(
        &mut self,
        cx: &Context<'_>,
        (a, b): (Value, Value),
        imm: Option<u64>,
    ) -> Result<(), CompileError> {
        let ty = cx.ty(a);
        let wide = ty == Type::I64;
        let ra = self.get(cx.loc(a)?, S0);
        if let Some(imm) = imm {
            let (sub, imm, high) = add_imm(true, ty, imm).ok_or(CompileError::Malformed)?;
            self.emit(Inst::AddImm {
                sub,
                flags: true,
                wide,
                rd: reg::ZR,
                rn: ra,
                imm,
                high,
            });
        } else {
            let rb = self.get(cx.loc(b)?, S1);
            self.emit(Inst::AddReg {
                sub: true,
                flags: true,
                wide,
                rd: reg::ZR,
                rn: ra,
                rm: rb,
            });
        }
        Ok(())
    }

    fn compare(
        &mut self,
        cx: &Context<'_>,
        inst: &Ir,
        (c, a, b): (IrCond, Value, Value),
        d: Option<Loc>,
    ) -> Result<(), CompileError> {
        self.flags(cx, (a, b), cx.folded(inst, 1, b))?;
        let rd = Self::result(d);
        // `cset`: one where the condition holds.
        self.emit(Inst::Csel {
            inc: true,
            wide: false,
            rd,
            rn: reg::ZR,
            rm: reg::ZR,
            cond: cond(c).invert(),
        });
        self.put(d, rd);
        Ok(())
    }

    /// `a` where the condition holds, else `b`: the flags set — by the
    /// comparison the select tests directly, where it does — then `csel`.
    /// A load leaves the flags.
    /// The register holding operand `operand` of `inst`, `value`: the zero
    /// register where it is a zero folded in, else its own or `scratch`.
    fn zero_or(
        &mut self,
        cx: &Context<'_>,
        (inst, operand, value): (&Ir, usize, Value),
        scratch: u8,
    ) -> Result<u8, CompileError> {
        if cx.folded(inst, operand, value).is_some() {
            return Ok(reg::ZR);
        }
        Ok(self.get(cx.loc(value)?, scratch))
    }

    fn select(
        &mut self,
        cx: &Context<'_>,
        (defined, inst): (Option<Value>, &Ir),
        (c, a, b): (Value, Value, Value),
        d: Option<Loc>,
    ) -> Result<(), CompileError> {
        let wide = cx.ty(a) == Type::I64;
        let holds = if let Some(Test::Compare {
            cond: test,
            a,
            b,
            imm,
        }) = cx.select_test(defined)
        {
            self.flags(cx, (a, b), imm)?;
            cond(test)
        } else {
            let rc = self.get(cx.loc(c)?, S2);
            self.emit(Inst::AddImm {
                sub: true,
                flags: true,
                wide: false,
                rd: reg::ZR,
                rn: rc,
                imm: 0,
                high: false,
            });
            Cond::Ne
        };
        let ra = self.zero_or(cx, (inst, 1, a), S0)?;
        let rb = self.zero_or(cx, (inst, 2, b), S1)?;
        let rd = Self::result(d);
        self.emit(Inst::Csel {
            inc: false,
            wide,
            rd,
            rn: ra,
            rm: rb,
            cond: holds,
        });
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
        let rn = self.get(cx.loc(base)?, S0);
        let offset = self.address(size(width), i64::from(offset), S1);
        let rt = Self::result(d);
        self.emit(Inst::Load {
            size: size(width),
            signed,
            wide: ty == Type::I64,
            rt,
            rn,
            offset,
        });
        self.put(d, rt);
        Ok(())
    }

    fn store(
        &mut self,
        cx: &Context<'_>,
        inst: &Ir,
        (width, value, base, offset): (Width, Value, Value, i32),
    ) -> Result<(), CompileError> {
        let rt = self.zero_or(cx, (inst, 0, value), S0)?;
        let rn = self.get(cx.loc(base)?, S1);
        let offset = self.address(size(width), i64::from(offset), S2);
        self.emit(Inst::Store {
            size: size(width),
            rt,
            rn,
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
        let rt = self.get(cx.loc(table)?, S0);
        let offset = self.address(Size::X, i64::from(index) * 8, S1);
        self.emit(Inst::Load {
            size: Size::X,
            signed: false,
            wide: true,
            rt: S0,
            rn: rt,
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
        self.emit(Inst::Blr(S0));
        if let Some((d, ty)) = ret {
            let rd = Self::result(Some(d));
            self.mov(ty == Type::I64, rd, 0);
            self.put(Some(d), rd);
        }
        Ok(())
    }

    /// `sp = sp - bytes`, or `+` where `add`.
    fn adjust_sp(&mut self, add: bool, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let sub = !add;
        if bytes < 1 << 24 {
            let (high, low) = (bytes >> 12, bytes & 0xFFF);
            for (imm, shifted) in [(high, true), (low, false)] {
                if let (Ok(imm), true) = (u16::try_from(imm), imm != 0) {
                    self.emit(Inst::AddImm {
                        sub,
                        flags: false,
                        wide: true,
                        rd: reg::SP,
                        rn: reg::SP,
                        imm,
                        high: shifted,
                    });
                }
            }
        } else {
            self.constant(true, S0, bytes);
            self.emit(Inst::AddSp {
                sub,
                rd: reg::SP,
                rn: reg::SP,
                rm: S0,
            });
        }
    }
}

impl Backend for Arm {
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
        self.emit(Inst::PushPair {
            rt: reg::FP,
            rt2: reg::LR,
        });
        self.emit(Inst::AddImm {
            sub: false,
            flags: false,
            wide: true,
            rd: reg::FP,
            rn: reg::SP,
            imm: 0,
            high: false,
        });
        for pair in alloc.saved.chunks(2) {
            self.emit(Inst::PushPair {
                rt: pair[0],
                rt2: pair.get(1).copied().unwrap_or(reg::ZR),
            });
        }
        self.frame = (u64::from(alloc.slots) * 8).next_multiple_of(16);
        self.adjust_sp(false, self.frame);
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
                self.constant(ty == Type::I64, rd, imm);
                self.put(d, rd);
                Ok(())
            }
            Ir::Unary { op, a } => self.unary(cx, op, a, d),
            Ir::Binary { op, a, b } => self.binary(cx, inst, (op, a, b), d),
            Ir::Icmp { cond, a, b } => self.compare(cx, inst, (cond, a, b), d),
            Ir::Select { cond, a, b } => self.select(cx, (defined, inst), (cond, a, b), d),
            Ir::Extend { signed, a } => {
                let ra = self.get(cx.loc(a)?, S0);
                let rd = Self::result(d);
                if signed {
                    self.emit(Inst::Bfm {
                        signed: true,
                        wide: true,
                        rd,
                        rn: ra,
                        immr: 0,
                        imms: 31,
                    });
                } else {
                    self.mov(false, rd, ra);
                }
                self.put(d, rd);
                Ok(())
            }
            Ir::Reduce { a } => {
                let ra = self.get(cx.loc(a)?, S0);
                let rd = Self::result(d);
                self.mov(false, rd, ra);
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
        self.emit(Inst::B(label));
    }

    fn jump_unless(
        &mut self,
        cx: &Context<'_>,
        test: Test,
        label: Label,
    ) -> Result<(), CompileError> {
        match test {
            Test::Value(cond) => {
                let rt = self.get(cond, S0);
                self.emit(Inst::Cbz {
                    wide: false,
                    rt,
                    label,
                });
            }
            Test::Compare { cond: c, a, b, imm } => {
                self.flags(cx, (a, b), imm)?;
                self.emit(Inst::BCond(cond(c).invert(), label));
            }
        }
        Ok(())
    }

    fn ret(&mut self, value: Option<(Loc, Type)>, alloc: &Allocation) -> Result<(), CompileError> {
        if let Some((loc, _)) = value {
            self.copy(loc, Loc::Reg(0), S0);
        }
        self.adjust_sp(true, self.frame);
        for pair in alloc.saved.chunks(2).rev() {
            self.emit(Inst::PopPair {
                rt: pair[0],
                rt2: pair.get(1).copied().unwrap_or(reg::ZR),
            });
        }
        self.emit(Inst::PopPair {
            rt: reg::FP,
            rt2: reg::LR,
        });
        self.emit(Inst::Ret);
        Ok(())
    }
}

#[cfg(test)]
#[path = "aarch64_tests.rs"]
mod tests;
