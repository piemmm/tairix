//! The x86_64 backend, System V calling convention. Each function makes a
//! frame record (`push rbp; mov rbp, rsp`), so a frame-pointer walk from a
//! helper it calls passes through it; RBP is never allocated. RAX, RCX, RDX,
//! R10 and R11 are kept out of allocation too: RAX and RDX hold a division,
//! RCX a shift's count, R11 a result bound for a slot and a call's target,
//! R10 a move between two stack slots. A 32-bit value is held with its register's upper
//! half clear, as every 32-bit operation leaves it. The second operand of an
//! addition, subtraction, logical operation or comparison is folded into the
//! instruction where it is a constant its 32-bit immediate holds, and a shift
//! or rotate count wherever it is a constant. Where every core has BMI2, a
//! shift by a register count is `shlx`, `shrx` or `sarx`, which take the
//! count in any register and write a destination of their own, and a rotate
//! by a constant is `rorx`.

pub(crate) mod inst;
#[cfg(test)]
mod render;
#[cfg(any(test, feature = "test-util"))]
pub(crate) mod sim;

use alloc::vec::Vec;

use crate::ir::{BinOp, Cond, Function, Inst as Ir, Type, UnOp, Value, Width};
use crate::lower::{self, Backend, Context, Label, Machine, Test};
use crate::moves::{sequence, Move};
use crate::regalloc::{Allocation, Loc, RegClass};
use crate::uses::Folding;
use crate::{CompileError, Target};
use inst::{reg, Alu, Bytes, Cc, Count, Inst, Shift, ShiftX, Src};
use tairix_abi::cpufeatures::CpuFeature;

/// The registers the calling convention has a call keep that are
/// allocated: all of them but RBP, the frame pointer.
pub(crate) const CALLEE_SAVED: [u8; 5] = [reg::RBX, reg::R12, reg::R13, reg::R14, reg::R15];

/// The target as the shared lowering sees it.
const MACHINE: Machine = Machine {
    regs: RegClass::new(&[reg::RSI, reg::RDI, reg::R8, reg::R9], &CALLEE_SAVED),
    folding: Folding {
        inst: folds,
        branch: |_, ty, imm| imm32(ty, imm).is_some(),
        select: true,
    },
};

/// The registers the arguments of a call arrive in.
pub(crate) const ARGS: [u8; 6] = [reg::RDI, reg::RSI, reg::RDX, reg::RCX, reg::R8, reg::R9];

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
    let mut backend = X86 {
        bmi2: target.has(CpuFeature::Bmi2),
        ..X86::default()
    };
    lower::lower(func, &MACHINE, &mut backend)?;
    let lowered = Lowered {
        insts: backend.insts,
        labels: backend.labels,
    };
    let bytes = inst::encode(&lowered.insts, lowered.labels)?;
    Ok((lowered, bytes))
}

/// The 32-bit immediate a constant of `ty` is, where one holds it: any
/// 32-bit constant, and a 64-bit one its sign-extended immediate reaches.
fn imm32(ty: Type, imm: u64) -> Option<i32> {
    match ty {
        Type::I32 => Some(crate::bits::low32(imm).cast_signed()),
        Type::I64 => i32::try_from(imm.cast_signed()).ok(),
    }
}

/// Whether operand `operand` of `inst`, a constant `imm` of `ty`, is folded.
fn folds(inst: &Ir, operand: usize, ty: Type, imm: u64) -> bool {
    match *inst {
        // The first operand is moved into the result first: a constant by
        // `mov`. A division moves it to RAX, so takes it in a register.
        Ir::Binary { op, .. } if operand == 0 => {
            !matches!(op, BinOp::Udiv | BinOp::Sdiv | BinOp::Urem | BinOp::Srem)
        }
        Ir::Binary { op, .. } if operand == 1 => match op {
            BinOp::Add | BinOp::Sub | BinOp::And | BinOp::Or | BinOp::Xor => {
                imm32(ty, imm).is_some()
            }
            BinOp::Shl | BinOp::Shr | BinOp::Sar | BinOp::Rotl | BinOp::Rotr => true,
            BinOp::Mul | BinOp::Udiv | BinOp::Sdiv | BinOp::Urem | BinOp::Srem => false,
        },
        Ir::Icmp { .. } if operand == 1 => imm32(ty, imm).is_some(),
        // The choice where the condition fails is moved into the result
        // first.
        Ir::Select { .. } if operand == 2 => true,
        Ir::Store { width, .. } if operand == 0 => store_imm(width, ty, imm).is_some(),
        _ => false,
    }
}

/// The immediate a store of `width` makes of `imm`, a constant of `ty`:
/// its low bytes, or for eight bytes one that sign-extends to it.
fn store_imm(width: Width, ty: Type, imm: u64) -> Option<i32> {
    match width {
        Width::W64 => imm32(ty, imm),
        _ => Some(i32::from_le_bytes(crate::bits::low32(imm).to_le_bytes())),
    }
}

/// An instruction's first operand: a register, or a constant moved in by
/// `mov`.
#[derive(Clone, Copy)]
enum First {
    Reg(u8),
    Imm(u64),
}

/// The backend's state while lowering.
#[derive(Default)]
struct X86 {
    insts: Vec<Inst>,
    labels: usize,
    /// The bytes the prologue took off the stack pointer after its pushes.
    frame: i32,
    /// Whether BMI2's instructions may be used.
    bmi2: bool,
}

impl X86 {
    fn emit(&mut self, inst: Inst) {
        self.insts.push(inst);
    }

    /// The frame slot's address: its offset from RSP.
    fn slot(slot: u32) -> Result<i32, CompileError> {
        i32::try_from(u64::from(slot) * 8).map_err(|_| CompileError::TooLarge)
    }

    /// The register holding the value at `loc`: its own, or `scratch` with
    /// the value loaded into it.
    fn get(&mut self, loc: Loc, scratch: u8) -> Result<u8, CompileError> {
        Ok(match loc {
            Loc::Reg(reg) => reg,
            Loc::Slot(slot) => {
                self.emit(Inst::Load {
                    bytes: Bytes::B8,
                    signed: false,
                    wide: true,
                    dst: scratch,
                    base: reg::RSP,
                    disp: Self::slot(slot)?,
                });
                scratch
            }
        })
    }

    /// The register a result for `loc` is computed in: R11 where nothing
    /// needs it.
    const fn result(loc: Option<Loc>) -> u8 {
        match loc {
            Some(Loc::Reg(reg)) => reg,
            Some(Loc::Slot(_)) | None => reg::R11,
        }
    }

    /// Put the result computed in `reg` where `loc` is.
    fn put(&mut self, loc: Option<Loc>, reg: u8) -> Result<(), CompileError> {
        if let Some(Loc::Slot(slot)) = loc {
            self.emit(Inst::Store {
                bytes: Bytes::B8,
                src: reg,
                base: reg::RSP,
                disp: Self::slot(slot)?,
            });
        }
        Ok(())
    }

    /// Move the whole word at `from` to `to`, through `scratch` between two
    /// slots.
    fn copy(&mut self, from: Loc, to: Loc, scratch: u8) -> Result<(), CompileError> {
        match (from, to) {
            (Loc::Reg(src), Loc::Reg(dst)) => self.emit(Inst::Mov {
                wide: true,
                dst,
                src,
            }),
            (from, Loc::Reg(dst)) => {
                self.get(from, dst)?;
            }
            (Loc::Reg(src), to) => self.put(Some(to), src)?,
            (from, to) => {
                let held = self.get(from, scratch)?;
                self.put(Some(to), held)?;
            }
        }
        Ok(())
    }

    /// The second operand of `inst`, `b`: its immediate where it is folded.
    fn operand(
        &mut self,
        cx: &Context<'_>,
        inst: &Ir,
        b: Value,
        scratch: u8,
    ) -> Result<Src, CompileError> {
        if let Some(imm) = cx.folded(inst, 1, b) {
            return imm32(cx.ty(b), imm)
                .map(Src::Imm)
                .ok_or(CompileError::Malformed);
        }
        Ok(Src::Reg(self.get(cx.loc(b)?, scratch)?))
    }

    /// Operand 0 of `inst`, `a`: its register, loaded into `scratch` from a
    /// slot, or the constant folded in.
    fn first(
        &mut self,
        cx: &Context<'_>,
        inst: &Ir,
        a: Value,
        scratch: u8,
    ) -> Result<First, CompileError> {
        if let Some(imm) = cx.folded(inst, 0, a) {
            return Ok(First::Imm(imm));
        }
        Ok(First::Reg(self.get(cx.loc(a)?, scratch)?))
    }

    /// `dst = a`.
    fn take(&mut self, wide: bool, dst: u8, a: First) {
        self.emit(match a {
            First::Reg(src) => Inst::Mov { wide, dst, src },
            First::Imm(imm) => Inst::MovImm { wide, dst, imm },
        });
    }

    fn binary(
        &mut self,
        cx: &Context<'_>,
        inst: &Ir,
        (op, a, b): (BinOp, Value, Value),
        d: Option<Loc>,
    ) -> Result<(), CompileError> {
        let wide = cx.ty(a) == Type::I64;
        let alu = match op {
            BinOp::Add => Some(Alu::Add),
            BinOp::Sub => Some(Alu::Sub),
            BinOp::And => Some(Alu::And),
            BinOp::Or => Some(Alu::Or),
            BinOp::Xor => Some(Alu::Xor),
            _ => None,
        };
        let ra = self.first(cx, inst, a, reg::RAX)?;
        let rd = Self::result(d);
        if let Some(alu) = alu {
            let src = self.operand(cx, inst, b, reg::RCX)?;
            self.take(wide, rd, ra);
            self.emit(Inst::Alu {
                op: alu,
                wide,
                dst: rd,
                src,
            });
            return self.put(d, rd);
        }
        match op {
            BinOp::Mul => {
                let rb = self.get(cx.loc(b)?, reg::RCX)?;
                self.take(wide, rd, ra);
                self.emit(Inst::Imul {
                    wide,
                    dst: rd,
                    src: rb,
                });
            }
            BinOp::Shl | BinOp::Shr | BinOp::Sar | BinOp::Rotl | BinOp::Rotr => {
                if !(self.bmi2 && self.shift_x(cx, inst, (op, ra, b), rd)?) {
                    self.shift(cx, inst, (op, ra, b), rd)?;
                }
            }
            _ => self.divide(cx, op, (ra, b), rd)?,
        }
        self.put(d, rd)
    }

    /// A shift or rotate: the count in CL, or folded, and the operand
    /// shifted in place.
    fn shift(
        &mut self,
        cx: &Context<'_>,
        inst: &Ir,
        (op, ra, b): (BinOp, First, Value),
        rd: u8,
    ) -> Result<(), CompileError> {
        let wide = cx.ty(b) == Type::I64;
        let shift = match op {
            BinOp::Shl => Shift::Shl,
            BinOp::Shr => Shift::Shr,
            BinOp::Sar => Shift::Sar,
            BinOp::Rotl => Shift::Rol,
            _ => Shift::Ror,
        };
        let count = if let Some(n) = cx.folded(inst, 1, b) {
            Count::Imm(crate::bits::low8(n % u64::from(cx.ty(b).bits())))
        } else {
            let rb = self.get(cx.loc(b)?, reg::RCX)?;
            if rb != reg::RCX {
                self.emit(Inst::Mov {
                    wide: false,
                    dst: reg::RCX,
                    src: rb,
                });
            }
            Count::Cl
        };
        self.take(wide, rd, ra);
        if count != Count::Imm(0) {
            self.emit(Inst::Shift {
                op: shift,
                wide,
                dst: rd,
                count,
            });
        }
        Ok(())
    }

    /// A shift by a register count or a rotate by a constant, in BMI2's
    /// forms; whether the operation had one.
    fn shift_x(
        &mut self,
        cx: &Context<'_>,
        inst: &Ir,
        (op, ra, b): (BinOp, First, Value),
        rd: u8,
    ) -> Result<bool, CompileError> {
        let wide = cx.ty(b) == Type::I64;
        let bits = cx.ty(b).bits();
        let folded = cx.folded(inst, 1, b);
        let shift = match op {
            BinOp::Shl => ShiftX::Shl,
            BinOp::Shr => ShiftX::Shr,
            BinOp::Sar => ShiftX::Sar,
            _ => {
                let Some(n) = folded else { return Ok(false) };
                let n = crate::bits::low32(n % u64::from(bits));
                let right = if op == BinOp::Rotr || n == 0 {
                    n
                } else {
                    bits - n
                };
                let src = self.source(wide, ra, rd);
                self.emit(Inst::Rorx {
                    wide,
                    dst: rd,
                    src,
                    imm: crate::bits::low8(right.into()),
                });
                return Ok(true);
            }
        };
        if folded.is_some() {
            return Ok(false);
        }
        let count = self.get(cx.loc(b)?, reg::RCX)?;
        let src = self.source(wide, ra, rd);
        self.emit(Inst::ShiftX {
            op: shift,
            wide,
            dst: rd,
            src,
            count,
        });
        Ok(true)
    }

    /// The register holding operand `a`: its own, or `rd` with the constant
    /// moved in.
    fn source(&mut self, wide: bool, a: First, rd: u8) -> u8 {
        match a {
            First::Reg(r) => r,
            First::Imm(_) => {
                self.take(wide, rd, a);
                rd
            }
        }
    }

    /// A division or remainder, total: by zero the quotient zero and the
    /// remainder the dividend; a signed division by minus one the dividend
    /// negated and no remainder, which the processor would refuse to make
    /// for the most negative dividend.
    fn divide(
        &mut self,
        cx: &Context<'_>,
        op: BinOp,
        (ra, b): (First, Value),
        rd: u8,
    ) -> Result<(), CompileError> {
        let wide = cx.ty(b) == Type::I64;
        let signed = matches!(op, BinOp::Sdiv | BinOp::Srem);
        let quotient = matches!(op, BinOp::Udiv | BinOp::Sdiv);
        let rb = self.get(cx.loc(b)?, reg::RCX)?;
        if !matches!(ra, First::Reg(reg::RAX)) {
            self.take(wide, reg::RAX, ra);
        }
        if rb != reg::RCX {
            self.emit(Inst::Mov {
                wide,
                dst: reg::RCX,
                src: rb,
            });
        }
        let (zero, done) = (self.label(), self.label());
        self.emit(Inst::Test {
            wide,
            a: reg::RCX,
            b: reg::RCX,
        });
        self.emit(Inst::Jcc(Cc::E, zero));
        if signed {
            let divide = self.label();
            self.emit(Inst::Alu {
                op: Alu::Cmp,
                wide,
                dst: reg::RCX,
                src: Src::Imm(-1),
            });
            self.emit(Inst::Jcc(Cc::Ne, divide));
            self.emit(Inst::Neg {
                wide,
                dst: reg::RAX,
            });
            self.zero(reg::RDX);
            self.emit(Inst::Jmp(done));
            self.bind(divide);
            self.emit(Inst::SignExtend { wide });
        } else {
            self.zero(reg::RDX);
        }
        self.emit(Inst::Div {
            signed,
            wide,
            src: reg::RCX,
        });
        self.emit(Inst::Jmp(done));
        self.bind(zero);
        self.emit(Inst::Mov {
            wide: true,
            dst: reg::RDX,
            src: reg::RAX,
        });
        self.zero(reg::RAX);
        self.bind(done);
        self.emit(Inst::Mov {
            wide,
            dst: rd,
            src: if quotient { reg::RAX } else { reg::RDX },
        });
        Ok(())
    }

    /// `r = 0`.
    fn zero(&mut self, r: u8) {
        self.emit(Inst::Alu {
            op: Alu::Xor,
            wide: false,
            dst: r,
            src: Src::Reg(r),
        });
    }

    fn unary(
        &mut self,
        cx: &Context<'_>,
        op: UnOp,
        a: Value,
        d: Option<Loc>,
    ) -> Result<(), CompileError> {
        let wide = cx.ty(a) == Type::I64;
        let ra = self.get(cx.loc(a)?, reg::RAX)?;
        let rd = Self::result(d);
        match op {
            UnOp::Neg | UnOp::Not => {
                self.emit(Inst::Mov {
                    wide,
                    dst: rd,
                    src: ra,
                });
                self.emit(if op == UnOp::Neg {
                    Inst::Neg { wide, dst: rd }
                } else {
                    Inst::Not { wide, dst: rd }
                });
            }
            UnOp::Uext8 | UnOp::Uext16 => self.emit(Inst::Movzx {
                from: if op == UnOp::Uext8 {
                    Bytes::B1
                } else {
                    Bytes::B2
                },
                dst: rd,
                src: ra,
            }),
            UnOp::Sext8 | UnOp::Sext16 => self.emit(Inst::Movsx {
                from: if op == UnOp::Sext8 {
                    Bytes::B1
                } else {
                    Bytes::B2
                },
                wide,
                dst: rd,
                src: ra,
            }),
        }
        self.put(d, rd)
    }

    /// The flags of `a` against `b`, or against `imm` where it is folded.
    fn flags(
        &mut self,
        cx: &Context<'_>,
        (a, b): (Value, Value),
        imm: Option<u64>,
    ) -> Result<(), CompileError> {
        let wide = cx.ty(a) == Type::I64;
        let ra = self.get(cx.loc(a)?, reg::RAX)?;
        let src = match imm {
            Some(imm) => Src::Imm(imm32(cx.ty(b), imm).ok_or(CompileError::Malformed)?),
            None => Src::Reg(self.get(cx.loc(b)?, reg::RCX)?),
        };
        self.emit(Inst::Alu {
            op: Alu::Cmp,
            wide,
            dst: ra,
            src,
        });
        Ok(())
    }

    fn compare(
        &mut self,
        cx: &Context<'_>,
        inst: &Ir,
        (cond, a, b): (Cond, Value, Value),
        d: Option<Loc>,
    ) -> Result<(), CompileError> {
        self.flags(cx, (a, b), cx.folded(inst, 1, b))?;
        let rd = Self::result(d);
        self.emit(Inst::Setcc {
            cc: cc(cond),
            dst: rd,
        });
        self.emit(Inst::Movzx {
            from: Bytes::B1,
            dst: rd,
            src: rd,
        });
        self.put(d, rd)
    }

    /// `a` where the condition holds, else `b`: `b` moved in, the flags
    /// set — by the comparison the select tests directly, where it does —
    /// and `a` moved over it on the condition. A move leaves the flags.
    fn select(
        &mut self,
        cx: &Context<'_>,
        (defined, inst): (Option<Value>, &Ir),
        (cond, a, b): (Value, Value, Value),
        d: Option<Loc>,
    ) -> Result<(), CompileError> {
        let wide = cx.ty(a) == Type::I64;
        let rd = Self::result(d);
        let rb = match cx.folded(inst, 2, b) {
            Some(imm) => First::Imm(imm),
            None => First::Reg(self.get(cx.loc(b)?, reg::RCX)?),
        };
        self.take(wide, rd, rb);
        let holds = if let Some(Test::Compare { cond, a, b, imm }) = cx.select_test(defined) {
            self.flags(cx, (a, b), imm)?;
            cc(cond)
        } else {
            let rc = self.get(cx.loc(cond)?, reg::RDX)?;
            self.emit(Inst::Test {
                wide: false,
                a: rc,
                b: rc,
            });
            Cc::Ne
        };
        let ra = self.get(cx.loc(a)?, reg::RDX)?;
        self.emit(Inst::Cmov {
            cc: holds,
            wide,
            dst: rd,
            src: ra,
        });
        self.put(d, rd)
    }

    /// `ra`, or its sign or zero extension, as a 64-bit value.
    fn widen(
        &mut self,
        cx: &Context<'_>,
        (signed, a): (bool, Value),
        d: Option<Loc>,
    ) -> Result<(), CompileError> {
        let ra = self.get(cx.loc(a)?, reg::RAX)?;
        let rd = Self::result(d);
        self.emit(if signed {
            Inst::Movsx {
                from: Bytes::B4,
                wide: true,
                dst: rd,
                src: ra,
            }
        } else {
            Inst::Mov {
                wide: false,
                dst: rd,
                src: ra,
            }
        });
        self.put(d, rd)
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
        let rb = self.get(cx.loc(base)?, reg::RAX)?;
        let rd = Self::result(d);
        self.emit(Inst::Load {
            bytes: bytes(width),
            signed,
            wide: ty == Type::I64,
            dst: rd,
            base: rb,
            disp: offset,
        });
        self.put(d, rd)
    }

    fn call(
        &mut self,
        cx: &Context<'_>,
        (table, index): (Value, u32),
        args: &[Value],
        ret: Option<(Loc, Type)>,
    ) -> Result<(), CompileError> {
        let rt = self.get(cx.loc(table)?, reg::R11)?;
        let disp = i32::try_from(u64::from(index) * 8).map_err(|_| CompileError::TooLarge)?;
        self.emit(Inst::Load {
            bytes: Bytes::B8,
            signed: false,
            wide: true,
            dst: reg::R11,
            base: rt,
            disp,
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
        for step in sequence(&moves, Loc::Reg(reg::RAX)) {
            self.copy(step.from, step.to, reg::R10)?;
        }
        self.emit(Inst::Call(reg::R11));
        if let Some((d, ty)) = ret {
            let rd = Self::result(Some(d));
            self.emit(Inst::Mov {
                wide: ty == Type::I64,
                dst: rd,
                src: reg::RAX,
            });
            self.put(Some(d), rd)?;
        }
        Ok(())
    }
}

/// The flags condition a comparison is.
const fn cc(cond: Cond) -> Cc {
    match cond {
        Cond::Eq => Cc::E,
        Cond::Ne => Cc::Ne,
        Cond::Slt => Cc::L,
        Cond::Sle => Cc::Le,
        Cond::Sgt => Cc::G,
        Cond::Sge => Cc::Ge,
        Cond::Ult => Cc::B,
        Cond::Ule => Cc::Be,
        Cond::Ugt => Cc::A,
        Cond::Uge => Cc::Ae,
    }
}

/// The bytes a load or store of `width` moves.
const fn bytes(width: Width) -> Bytes {
    match width {
        Width::W8 => Bytes::B1,
        Width::W16 => Bytes::B2,
        Width::W32 => Bytes::B4,
        Width::W64 => Bytes::B8,
    }
}

impl Backend for X86 {
    fn temp(&self) -> Loc {
        Loc::Reg(reg::R11)
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
        self.emit(Inst::Push(reg::RBP));
        self.emit(Inst::Mov {
            wide: true,
            dst: reg::RBP,
            src: reg::RSP,
        });
        for &saved in &alloc.saved {
            self.emit(Inst::Push(saved));
        }
        // The call that entered left RSP eight short of sixteen; the pushes,
        // the frame record's among them, and the slots must bring it back to
        // a multiple of sixteen.
        let pushed = (alloc.saved.len() as u64 + 2) * 8;
        let frame = (u64::from(alloc.slots) * 8 + pushed).next_multiple_of(16) - pushed;
        self.frame = i32::try_from(frame).map_err(|_| CompileError::TooLarge)?;
        if self.frame > 0 {
            self.emit(Inst::Alu {
                op: Alu::Sub,
                wide: true,
                dst: reg::RSP,
                src: Src::Imm(self.frame),
            });
        }
        Ok(())
    }

    fn mov(&mut self, from: Loc, to: Loc) -> Result<(), CompileError> {
        self.copy(from, to, reg::R10)
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
                self.emit(Inst::MovImm {
                    wide: ty == Type::I64,
                    dst: rd,
                    imm,
                });
                self.put(d, rd)
            }
            Ir::Unary { op, a } => self.unary(cx, op, a, d),
            Ir::Binary { op, a, b } => self.binary(cx, inst, (op, a, b), d),
            Ir::Icmp { cond, a, b } => self.compare(cx, inst, (cond, a, b), d),
            Ir::Select { cond, a, b } => self.select(cx, (defined, inst), (cond, a, b), d),
            Ir::Extend { signed, a } => self.widen(cx, (signed, a), d),
            Ir::Reduce { a } => {
                let ra = self.get(cx.loc(a)?, reg::RAX)?;
                let rd = Self::result(d);
                self.emit(Inst::Mov {
                    wide: false,
                    dst: rd,
                    src: ra,
                });
                self.put(d, rd)
            }
            Ir::Load { .. } => self.load(cx, inst, d),
            Ir::Store {
                width,
                value,
                base,
                offset,
            } => {
                let rb = self.get(cx.loc(base)?, reg::RCX)?;
                let folded = cx
                    .folded(inst, 0, value)
                    .and_then(|imm| store_imm(width, cx.ty(value), imm));
                if let Some(imm) = folded {
                    self.emit(Inst::StoreImm {
                        bytes: bytes(width),
                        imm,
                        base: rb,
                        disp: offset,
                    });
                } else {
                    let rv = self.get(cx.loc(value)?, reg::RAX)?;
                    self.emit(Inst::Store {
                        bytes: bytes(width),
                        src: rv,
                        base: rb,
                        disp: offset,
                    });
                }
                Ok(())
            }
            Ir::Call {
                table,
                index,
                ref args,
                ret,
            } => {
                let ret = d.zip(ret);
                self.call(cx, (table, index), args, ret)
            }
        }
    }

    fn jump(&mut self, label: Label) {
        self.emit(Inst::Jmp(label));
    }

    fn jump_unless(
        &mut self,
        cx: &Context<'_>,
        test: Test,
        label: Label,
    ) -> Result<(), CompileError> {
        match test {
            Test::Value(cond) => {
                let rc = self.get(cond, reg::RAX)?;
                self.emit(Inst::Test {
                    wide: false,
                    a: rc,
                    b: rc,
                });
                self.emit(Inst::Jcc(Cc::E, label));
            }
            Test::Compare { cond, a, b, imm } => {
                self.flags(cx, (a, b), imm)?;
                self.emit(Inst::Jcc(cc(cond.inverse()), label));
            }
        }
        Ok(())
    }

    fn ret(&mut self, value: Option<(Loc, Type)>, alloc: &Allocation) -> Result<(), CompileError> {
        if let Some((loc, _)) = value {
            self.copy(loc, Loc::Reg(reg::RAX), reg::R10)?;
        }
        if self.frame > 0 {
            self.emit(Inst::Alu {
                op: Alu::Add,
                wide: true,
                dst: reg::RSP,
                src: Src::Imm(self.frame),
            });
        }
        for &saved in alloc.saved.iter().rev() {
            self.emit(Inst::Pop(saved));
        }
        self.emit(Inst::Pop(reg::RBP));
        self.emit(Inst::Ret);
        Ok(())
    }
}

#[cfg(test)]
#[path = "x86_64_tests.rs"]
mod tests;
