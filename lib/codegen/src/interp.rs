//! The reference interpreter: what a function means, which every backend's
//! code must do. A 32-bit value is held as its bits in a 64-bit word, the
//! upper half clear.

use alloc::vec;
use alloc::vec::Vec;

use crate::bits::{low32, sext16, sext32, sext8};
use crate::ir::{BinOp, Block, Cond, Function, Inst, Terminator, Type, UnOp, Width};

/// Why a run stopped short.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trap {
    /// Memory refused an access at this address.
    Memory(u64),
    /// A helper refused its call.
    Helper(u32),
    /// The steps allowed ran out.
    Fuel,
    /// The function was not one the verifier admits.
    Malformed,
}

/// The memory a run reads and writes, little-endian.
pub trait Memory {
    /// The `width` bytes at `addr`, zero-extended.
    ///
    /// # Errors
    ///
    /// [`Trap::Memory`] for an address it does not hold.
    fn load(&mut self, addr: u64, width: Width) -> Result<u64, Trap>;

    /// Store the low `width` bytes of `value` at `addr`.
    ///
    /// # Errors
    ///
    /// [`Trap::Memory`] for an address it does not hold.
    fn store(&mut self, addr: u64, width: Width, value: u64) -> Result<(), Trap>;
}

/// The helpers a run calls.
pub trait Helpers {
    /// Helper `index` of the table at `table`, called with `args`: what it
    /// answers, wrapped to `ret`.
    ///
    /// # Errors
    ///
    /// [`Trap::Helper`] for a helper it does not have.
    fn call(
        &mut self,
        table: u64,
        index: u32,
        args: &[u64],
        ret: Option<Type>,
    ) -> Result<Option<u64>, Trap>;
}

/// `op a` of type `ty`.
#[must_use]
pub fn unary(op: UnOp, ty: Type, a: u64) -> u64 {
    let raw = match op {
        UnOp::Neg => a.wrapping_neg(),
        UnOp::Not => !a,
        UnOp::Uext8 => a & 0xFF,
        UnOp::Uext16 => a & 0xFFFF,
        UnOp::Sext8 => sext8(a).cast_unsigned(),
        UnOp::Sext16 => sext16(a).cast_unsigned(),
    };
    ty.wrap(raw)
}

/// `a op b` of type `ty`.
#[must_use]
pub fn binary(op: BinOp, ty: Type, a: u64, b: u64) -> u64 {
    let bits = ty.bits();
    let count = low32(b % u64::from(bits));
    let signed = |v: u64| -> i64 {
        match ty {
            Type::I32 => sext32(v),
            Type::I64 => v.cast_signed(),
        }
    };
    let raw = match op {
        BinOp::Add => a.wrapping_add(b),
        BinOp::Sub => a.wrapping_sub(b),
        BinOp::Mul => a.wrapping_mul(b),
        BinOp::And => a & b,
        BinOp::Or => a | b,
        BinOp::Xor => a ^ b,
        BinOp::Shl => a << count,
        BinOp::Shr => ty.wrap(a) >> count,
        BinOp::Sar => (signed(a) >> count).cast_unsigned(),
        BinOp::Rotl => rotate(ty, a, count),
        BinOp::Rotr => rotate(ty, a, (bits - count) % bits),
        BinOp::Udiv => {
            let (a, b) = (ty.wrap(a), ty.wrap(b));
            a.checked_div(b).unwrap_or(0)
        }
        BinOp::Urem => {
            let (a, b) = (ty.wrap(a), ty.wrap(b));
            a.checked_rem(b).unwrap_or(a)
        }
        BinOp::Sdiv => {
            let (a, b) = (signed(a), signed(b));
            if b == 0 {
                0
            } else {
                wrapping_sdiv(ty, a, b).cast_unsigned()
            }
        }
        BinOp::Srem => {
            let (a, b) = (signed(a), signed(b));
            if b == 0 {
                a.cast_unsigned()
            } else {
                wrapping_srem(ty, a, b).cast_unsigned()
            }
        }
    };
    ty.wrap(raw)
}

fn rotate(ty: Type, a: u64, count: u32) -> u64 {
    match ty {
        Type::I32 => u64::from(low32(a).rotate_left(count)),
        Type::I64 => a.rotate_left(count),
    }
}

/// Signed division of `ty`'s values, sign-extended in `a` and `b`, the one
/// overflow wrapping.
fn wrapping_sdiv(ty: Type, a: i64, b: i64) -> i64 {
    match ty {
        Type::I32 => i64::from(narrow(a).wrapping_div(narrow(b))),
        Type::I64 => a.wrapping_div(b),
    }
}

fn wrapping_srem(ty: Type, a: i64, b: i64) -> i64 {
    match ty {
        Type::I32 => i64::from(narrow(a).wrapping_rem(narrow(b))),
        Type::I64 => a.wrapping_rem(b),
    }
}

/// A sign-extended 32-bit value back in 32 bits.
const fn narrow(v: i64) -> i32 {
    low32(v.cast_unsigned()).cast_signed()
}

/// Whether `a cond b` of type `ty` holds.
#[must_use]
pub fn compare(cond: Cond, ty: Type, a: u64, b: u64) -> bool {
    let (ua, ub) = (ty.wrap(a), ty.wrap(b));
    let (sa, sb) = match ty {
        Type::I32 => (sext32(ua), sext32(ub)),
        Type::I64 => (ua.cast_signed(), ub.cast_signed()),
    };
    match cond {
        Cond::Eq => ua == ub,
        Cond::Ne => ua != ub,
        Cond::Slt => sa < sb,
        Cond::Sle => sa <= sb,
        Cond::Sgt => sa > sb,
        Cond::Sge => sa >= sb,
        Cond::Ult => ua < ub,
        Cond::Ule => ua <= ub,
        Cond::Ugt => ua > ub,
        Cond::Uge => ua >= ub,
    }
}

/// `width` bytes read as `raw`, extended to `ty`.
#[must_use]
pub fn loaded(width: Width, signed: bool, ty: Type, raw: u64) -> u64 {
    let bits = width.bits();
    let kept = if bits == 64 {
        raw
    } else {
        raw & ((1u64 << bits) - 1)
    };
    let negative = bits < 64 && kept >> (bits - 1) & 1 == 1;
    let value = if signed && negative {
        kept | (!0u64 << bits)
    } else {
        kept
    };
    ty.wrap(value)
}

/// Run `func` with `args`, at most `fuel` instructions and terminators.
///
/// # Errors
///
/// The trap that stopped it.
pub fn run(
    func: &Function,
    args: &[u64],
    memory: &mut dyn Memory,
    helpers: &mut dyn Helpers,
    mut fuel: u64,
) -> Result<Option<u64>, Trap> {
    let mut values = vec![0u64; func.value_count()];
    let entry = func.blocks().first().ok_or(Trap::Malformed)?;
    if entry.params.len() != args.len() {
        return Err(Trap::Malformed);
    }
    for (&param, &arg) in entry.params.iter().zip(args) {
        values[param.index()] = func.type_of(param).wrap(arg);
    }
    let mut block = Block::ENTRY;
    loop {
        let data = func.blocks().get(block.index()).ok_or(Trap::Malformed)?;
        for (defines, inst) in &data.insts {
            fuel = fuel.checked_sub(1).ok_or(Trap::Fuel)?;
            let result = step(func, inst, &values, memory, helpers)?;
            if let (Some(value), Some(result)) = (*defines, result) {
                values[value.index()] = result;
            }
        }
        fuel = fuel.checked_sub(1).ok_or(Trap::Fuel)?;
        let (to, passed): (Block, Vec<u64>) = match &data.term {
            Terminator::Return { value } => return Ok(value.map(|value| values[value.index()])),
            Terminator::Jump { to, args } => {
                (*to, args.iter().map(|a| values[a.index()]).collect())
            }
            Terminator::Branch {
                cond,
                then,
                then_args,
                other,
                other_args,
            } => {
                let (to, args) = if values[cond.index()] != 0 {
                    (*then, then_args)
                } else {
                    (*other, other_args)
                };
                (to, args.iter().map(|a| values[a.index()]).collect())
            }
        };
        let params = &func.blocks().get(to.index()).ok_or(Trap::Malformed)?.params;
        if params.len() != passed.len() {
            return Err(Trap::Malformed);
        }
        for (&param, value) in params.iter().zip(passed) {
            values[param.index()] = value;
        }
        block = to;
    }
}

fn step(
    func: &Function,
    inst: &Inst,
    values: &[u64],
    memory: &mut dyn Memory,
    helpers: &mut dyn Helpers,
) -> Result<Option<u64>, Trap> {
    let of = |v: crate::ir::Value| values[v.index()];
    Ok(match *inst {
        Inst::Const { ty, imm } => Some(ty.wrap(imm)),
        Inst::Unary { op, a } => Some(unary(op, func.type_of(a), of(a))),
        Inst::Binary { op, a, b } => Some(binary(op, func.type_of(a), of(a), of(b))),
        Inst::Icmp { cond, a, b } => Some(u64::from(compare(cond, func.type_of(a), of(a), of(b)))),
        Inst::Select { cond, a, b } => Some(if of(cond) != 0 { of(a) } else { of(b) }),
        Inst::Extend { signed, a } => Some(if signed {
            sext32(of(a)).cast_unsigned()
        } else {
            of(a) & 0xFFFF_FFFF
        }),
        Inst::Reduce { a } => Some(of(a) & 0xFFFF_FFFF),
        Inst::Load {
            width,
            signed,
            ty,
            base,
            offset,
        } => {
            let addr = of(base).wrapping_add(i64::from(offset).cast_unsigned());
            Some(loaded(width, signed, ty, memory.load(addr, width)?))
        }
        Inst::Store {
            width,
            value,
            base,
            offset,
        } => {
            let addr = of(base).wrapping_add(i64::from(offset).cast_unsigned());
            memory.store(addr, width, of(value))?;
            None
        }
        Inst::Call {
            table,
            index,
            ref args,
            ret,
        } => {
            let passed: Vec<u64> = args.iter().map(|&a| of(a)).collect();
            let answer = helpers.call(of(table), index, &passed, ret)?;
            match (ret, answer) {
                (Some(ty), Some(value)) => Some(ty.wrap(value)),
                (None, None) => None,
                _ => return Err(Trap::Helper(index)),
            }
        }
    })
}

#[cfg(test)]
#[path = "interp_tests.rs"]
mod tests;
