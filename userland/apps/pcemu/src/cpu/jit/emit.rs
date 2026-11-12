//! Building the IR with what the translator already knows folded in. A
//! value stays a constant while every input to it is one — an instruction's
//! IP, its costs, a prefix's segment, the bus interface unit's state right
//! after a jump — so the code computes only what depends on the machine.
//! Folding uses the generator's own interpreter, so a folded value is the
//! one the code would have computed. The bits each computed value may have
//! set are known too, so a mask that would clear none of them is no
//! instruction at all.

use alloc::vec::Vec;

use tairix_codegen::interp;
use tairix_codegen::ir::{BinOp, Block, Builder, Cond, Type, UnOp, Value, Width};

/// A value: one the translator knows, or one the code computes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum V {
    /// A constant, its bits wrapped to the value's type.
    K(u64),
    /// Computed.
    R(Value),
}

/// The builder, folding.
pub(crate) struct Emit {
    pub(crate) b: Builder,
    /// The bits each computed value may have set, by its index; one not
    /// listed may have any.
    bits: Vec<u64>,
    /// Each comparison made: its value's index, type, condition, operands.
    compares: Vec<(usize, Type, Cond, V, V)>,
}

const fn all_ones(ty: Type) -> u64 {
    match ty {
        Type::I32 => 0xFFFF_FFFF,
        Type::I64 => u64::MAX,
    }
}

/// Every bit up to the highest of `n`'s.
const fn spread(n: u64) -> u64 {
    match n.checked_ilog2() {
        Some(top) => u64::MAX >> (63 - top),
        None => 0,
    }
}

impl Emit {
    pub(crate) const fn new(b: Builder) -> Self {
        Self {
            b,
            bits: Vec::new(),
            compares: Vec::new(),
        }
    }

    /// The bits `v` may have set, as a value of type `ty`.
    fn may(&self, ty: Type, v: V) -> u64 {
        let bits = match v {
            V::K(k) => k,
            V::R(r) => self.bits.get(r.index()).copied().unwrap_or(u64::MAX),
        };
        bits & all_ones(ty)
    }

    /// `r`, which has no bit set outside `bits`: zero where it has none.
    fn known(&mut self, r: Value, bits: u64) -> V {
        if bits == 0 {
            return V::K(0);
        }
        let at = r.index();
        if self.bits.len() <= at {
            self.bits.resize(at + 1, u64::MAX);
        }
        if let Some(slot) = self.bits.get_mut(at) {
            *slot = bits;
        }
        V::R(r)
    }

    /// `v` as a value the code holds.
    pub(crate) fn val(&mut self, ty: Type, v: V) -> Value {
        match v {
            V::K(k) => self.b.iconst(ty, k),
            V::R(r) => r,
        }
    }

    /// `a op b`.
    pub(crate) fn bin(&mut self, ty: Type, op: BinOp, a: V, b: V) -> V {
        if let (V::K(x), V::K(y)) = (a, b) {
            return V::K(interp::binary(op, ty, x, y));
        }
        let (a, b) = match (a, b) {
            (V::K(_), V::R(_)) if op.commutes() => (b, a),
            _ => (a, b),
        };
        let identity = matches!(
            (op, b),
            (
                BinOp::Add
                    | BinOp::Sub
                    | BinOp::Or
                    | BinOp::Xor
                    | BinOp::Shl
                    | BinOp::Shr
                    | BinOp::Sar,
                V::K(0)
            ) | (BinOp::Mul, V::K(1))
        );
        if identity {
            return a;
        }
        if matches!((op, b), (BinOp::And | BinOp::Mul, V::K(0))) {
            return V::K(0);
        }
        if a == b {
            match op {
                BinOp::Sub | BinOp::Xor => return V::K(0),
                BinOp::And | BinOp::Or => return a,
                _ => {}
            }
        }
        if let (BinOp::Mul, V::K(k)) = (op, b) {
            if k.is_power_of_two() {
                return self.bin(ty, BinOp::Shl, a, V::K(u64::from(k.trailing_zeros())));
            }
        }
        let (ma, mb) = (self.may(ty, a), self.may(ty, b));
        let ones = all_ones(ty);
        if op == BinOp::And {
            match b {
                V::K(k) if ma & !k == 0 => return a,
                V::K(0xFF) if ty == Type::I32 => return self.un(ty, UnOp::Uext8, a),
                V::K(0xFFFF) if ty == Type::I32 => return self.un(ty, UnOp::Uext16, a),
                _ => {}
            }
        }
        let shift = match b {
            V::K(n) if n < u64::from(ty.bits()) => Some(n),
            _ => None,
        };
        let bits = match (op, shift) {
            (BinOp::And, _) => ma & mb,
            (BinOp::Or | BinOp::Xor, _) => ma | mb,
            (BinOp::Add, _) => ma
                .checked_add(mb)
                .filter(|&sum| sum <= ones)
                .map_or(ones, spread),
            (BinOp::Mul, _) => ma
                .checked_mul(mb)
                .filter(|&product| product <= ones)
                .map_or(ones, spread),
            (BinOp::Shl, Some(n)) => (ma << n) & ones,
            (BinOp::Shr, Some(n)) => ma >> n,
            (BinOp::Sar, Some(n)) if ma & !(ones >> 1) == 0 => ma >> n,
            _ => ones,
        };
        let (first, second) = (self.val(ty, a), self.val(ty, b));
        let made = self.b.binary(op, first, second);
        self.known(made, bits)
    }

    pub(crate) fn add(&mut self, ty: Type, a: V, b: V) -> V {
        self.bin(ty, BinOp::Add, a, b)
    }

    pub(crate) fn sub(&mut self, ty: Type, a: V, b: V) -> V {
        self.bin(ty, BinOp::Sub, a, b)
    }

    pub(crate) fn and(&mut self, ty: Type, a: V, b: V) -> V {
        self.bin(ty, BinOp::And, a, b)
    }

    pub(crate) fn or(&mut self, ty: Type, a: V, b: V) -> V {
        self.bin(ty, BinOp::Or, a, b)
    }

    pub(crate) fn xor(&mut self, ty: Type, a: V, b: V) -> V {
        self.bin(ty, BinOp::Xor, a, b)
    }

    pub(crate) fn shl(&mut self, ty: Type, a: V, n: u32) -> V {
        self.bin(ty, BinOp::Shl, a, V::K(u64::from(n)))
    }

    pub(crate) fn shr(&mut self, ty: Type, a: V, n: u32) -> V {
        self.bin(ty, BinOp::Shr, a, V::K(u64::from(n)))
    }

    /// `op a`.
    pub(crate) fn un(&mut self, ty: Type, op: UnOp, a: V) -> V {
        let r = match a {
            V::K(k) => return V::K(interp::unary(op, ty, k)),
            V::R(r) => r,
        };
        let ma = self.may(ty, a);
        // The bits an extension keeps, and the sign bit that may set the rest.
        let (kept, sign) = match op {
            UnOp::Uext8 => (0xFF, 0),
            UnOp::Uext16 => (0xFFFF, 0),
            UnOp::Sext8 => (0x7F, 0x80),
            UnOp::Sext16 => (0x7FFF, 0x8000),
            UnOp::Neg | UnOp::Not => {
                let r = self.b.unary(op, r);
                return self.known(r, all_ones(ty));
            }
        };
        if ma & !kept == 0 {
            return a;
        }
        let bits = if ma & sign == 0 {
            ma & kept
        } else {
            all_ones(ty)
        };
        let r = self.b.unary(op, r);
        self.known(r, bits)
    }

    /// One where `a cond b`, else zero: a 32-bit value.
    pub(crate) fn cmp(&mut self, ty: Type, cond: Cond, a: V, b: V) -> V {
        if let (V::K(x), V::K(y)) = (a, b) {
            return V::K(u64::from(interp::compare(cond, ty, x, y)));
        }
        let (first, second) = (self.val(ty, a), self.val(ty, b));
        let made = self.b.icmp(cond, first, second);
        self.compares.push((made.index(), ty, cond, a, b));
        self.known(made, 1)
    }

    /// The 32-bit zero or one `v` inverted: where `v` is a comparison, the
    /// opposite one, which a branch on it can still test directly.
    pub(crate) fn not(&mut self, v: V) -> V {
        if let V::R(r) = v {
            let made = self
                .compares
                .iter()
                .rev()
                .find(|compare| compare.0 == r.index())
                .copied();
            if let Some((_, ty, cond, a, b)) = made {
                return self.cmp(ty, cond.inverse(), a, b);
            }
        }
        self.xor(Type::I32, v, V::K(1))
    }

    /// `a` where the 32-bit `c` is not zero, else `b`.
    pub(crate) fn select(&mut self, ty: Type, cond: V, a: V, b: V) -> V {
        match cond {
            V::K(0) => return b,
            V::K(_) => return a,
            V::R(_) if a == b => return a,
            V::R(_) => {}
        }
        let bits = self.may(ty, a) | self.may(ty, b);
        let cond = self.val(Type::I32, cond);
        let (first, second) = (self.val(ty, a), self.val(ty, b));
        let made = self.b.select(cond, first, second);
        self.known(made, bits)
    }

    /// The 32-bit `a` zero-extended to 64 bits.
    pub(crate) fn widen(&mut self, a: V) -> V {
        match a {
            V::K(k) => V::K(k & 0xFFFF_FFFF),
            V::R(r) => {
                let bits = self.may(Type::I32, a);
                let wide = self.b.extend(false, r);
                self.known(wide, bits)
            }
        }
    }

    /// The 64-bit `a`'s low half.
    pub(crate) fn narrow(&mut self, a: V) -> V {
        match a {
            V::K(k) => V::K(k & 0xFFFF_FFFF),
            V::R(r) => {
                let bits = self.may(Type::I32, a);
                let low = self.b.reduce(r);
                self.known(low, bits)
            }
        }
    }

    /// `a - b` where `a` exceeds `b`, else zero.
    pub(crate) fn saturating_sub(&mut self, ty: Type, a: V, b: V) -> V {
        match (a, b) {
            (_, V::K(0)) => return a,
            (V::K(0), _) => return V::K(0),
            _ => {}
        }
        let more = self.cmp(ty, Cond::Ugt, a, b);
        let diff = self.sub(ty, a, b);
        self.select(ty, more, diff, V::K(0))
    }

    /// The lesser of `a` and `b`.
    pub(crate) fn min(&mut self, ty: Type, a: V, b: V) -> V {
        let less = self.cmp(ty, Cond::Ult, a, b);
        self.select(ty, less, a, b)
    }

    /// `width` bytes at `base + offset`, zero-extended to 32 bits.
    pub(crate) fn load(&mut self, width: Width, base: Value, offset: i32) -> V {
        let r = self.b.load(Type::I32, width, false, base, offset);
        let bits = match width {
            Width::W8 => 0xFF,
            Width::W16 => 0xFFFF,
            Width::W32 | Width::W64 => 0xFFFF_FFFF,
        };
        self.known(r, bits)
    }

    /// The 64-bit word at `base + offset`.
    pub(crate) fn load64(&mut self, base: Value, offset: i32) -> V {
        V::R(self.b.load(Type::I64, Width::W64, false, base, offset))
    }

    /// `value`'s low `width` bytes stored at `base + offset`: a 64-bit
    /// value for a 64-bit store, else a 32-bit one.
    pub(crate) fn store(&mut self, width: Width, value: V, base: Value, offset: i32) {
        let ty = if width == Width::W64 {
            Type::I64
        } else {
            Type::I32
        };
        let value = self.val(ty, value);
        self.b.store(width, value, base, offset);
    }

    /// End the block in a branch on the 32-bit `cond`: to `then` where it
    /// is not zero, else to `other`; a constant condition jumps.
    pub(crate) fn branch(&mut self, cond: V, then: Block, other: Block) {
        match cond {
            V::K(0) => self.b.jump(other, &[]),
            V::K(_) => self.b.jump(then, &[]),
            V::R(c) => self.b.branch(c, (then, &[]), (other, &[])),
        }
    }
}

#[cfg(test)]
#[path = "emit_tests.rs"]
mod tests;
