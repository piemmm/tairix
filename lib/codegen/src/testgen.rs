//! Random functions, and the memory and helpers they run against, for
//! holding a backend to the interpreter. A function reads its arguments,
//! keeps many values live at once so registers run short and calls cut
//! across them, branches into diamonds, loops a bounded number of times, and
//! loads and stores within one buffer, from its start and from its middle;
//! its answer folds every value it made, so no wrong one goes unseen.

use alloc::vec;
use alloc::vec::Vec;

use crate::interp::{self, Helpers, Memory, Trap};
use crate::ir::{BinOp, Block, BuildError, Builder, Cond, Function, Type, UnOp, Value, Width};
use crate::model::{self, Calls, Fault};
use crate::Target;

/// Where the buffer a function loads and stores lies.
pub const BUFFER: u64 = 0x1000_0000;
/// Its bytes.
pub const BUFFER_BYTES: usize = 256;
/// Where the helper table lies.
pub const TABLE: u64 = 0x2000_0000;
/// The helpers it holds.
pub const HELPERS: u32 = 4;
/// The address the table gives helper `n`: `HELPER_BASE + 16 * n`.
pub const HELPER_BASE: u64 = 0x3000_0000;

/// The middle of the buffer, which loads and stores also count from.
const MIDDLE: i32 = 128;

/// Constants on the edges an encoding or an operation turns on.
const EDGES: [u64; 12] = [
    0,
    1,
    u64::MAX,
    31,
    32,
    63,
    64,
    0x7FFF_FFFF,
    0x8000_0000,
    0xFFFF_FFFF,
    0x1_0000_0000,
    0xFFFF_FFFF_8000_0000,
];

/// The memory a case runs against: the buffer and the helper table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestMemory {
    /// The buffer's bytes.
    pub buffer: Vec<u8>,
}

impl TestMemory {
    /// The buffer filled from `next`.
    pub fn new(next: &mut dyn FnMut() -> u64) -> Self {
        Self {
            buffer: (0..BUFFER_BYTES)
                .map(|_| crate::bits::low8(next()))
                .collect(),
        }
    }

    fn bytes(&mut self, addr: u64, width: Width) -> Result<&mut [u8], Trap> {
        let n = width.bytes() as usize;
        let at = addr
            .checked_sub(BUFFER)
            .and_then(|at| usize::try_from(at).ok())
            .ok_or(Trap::Memory(addr))?;
        self.buffer.get_mut(at..at + n).ok_or(Trap::Memory(addr))
    }
}

impl Memory for TestMemory {
    fn load(&mut self, addr: u64, width: Width) -> Result<u64, Trap> {
        let entry = addr.wrapping_sub(TABLE);
        if entry < u64::from(HELPERS) * 8 && entry.is_multiple_of(8) && width == Width::W64 {
            return Ok(HELPER_BASE + entry / 8 * 16);
        }
        let held = self.bytes(addr, width)?;
        let mut raw = [0u8; 8];
        raw[..held.len()].copy_from_slice(held);
        Ok(u64::from_le_bytes(raw))
    }

    fn store(&mut self, addr: u64, width: Width, value: u64) -> Result<(), Trap> {
        let held = self.bytes(addr, width)?;
        let n = held.len();
        held.copy_from_slice(&value.to_le_bytes()[..n]);
        Ok(())
    }
}

/// What helper `index` answers for `args`: a mix of every argument.
#[must_use]
pub fn helper(index: u32, args: &[u64]) -> u64 {
    args.iter().zip((3u64..).step_by(2)).fold(
        u64::from(index).wrapping_mul(0x9E37_79B9_7F4A_7C15),
        |acc, (&arg, odd)| acc.rotate_left(13) ^ arg.wrapping_mul(odd),
    )
}

/// The interpreter's helpers: [`helper`], at the table a case gives.
#[derive(Debug, Default)]
pub struct TestHelpers {
    /// The calls made, each its index and arguments, in order.
    pub calls: Vec<(u32, Vec<u64>)>,
}

impl Helpers for TestHelpers {
    fn call(
        &mut self,
        table: u64,
        index: u32,
        args: &[u64],
        ret: Option<Type>,
    ) -> Result<Option<u64>, Trap> {
        if table != TABLE || index >= HELPERS {
            return Err(Trap::Helper(index));
        }
        self.calls.push((index, args.to_vec()));
        Ok(ret.map(|ty| ty.wrap(helper(index, args))))
    }
}

/// A model's helpers: [`helper`] at the address the table gives each,
/// reading the arguments [`arity`] gives it. The answer is the whole word,
/// so code taking a 32-bit answer from its upper half goes wrong.
#[derive(Debug, Default)]
pub struct TestCalls {
    /// The calls made, each its index and arguments, in order.
    pub calls: Vec<(u32, Vec<u64>)>,
}

impl Calls for TestCalls {
    fn call(&mut self, target: u64, args: [Option<u64>; 6]) -> Result<u64, Fault> {
        let offset = target.wrapping_sub(HELPER_BASE);
        let index = u32::try_from(offset / 16)
            .ok()
            .filter(|&index| offset.is_multiple_of(16) && index < HELPERS)
            .ok_or(Fault::Call(target))?;
        let passed = args[..arity(index)]
            .iter()
            .enumerate()
            .map(|(n, arg)| arg.ok_or(Fault::Argument(n)))
            .collect::<Result<Vec<u64>, Fault>>()?;
        let answer = helper(index, &passed);
        self.calls.push((index, passed));
        Ok(answer)
    }
}

/// How a backend's code and the interpreter parted on a case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Disagreement {
    /// The interpreter stopped: the case is not one [`generate`] makes.
    Trap(Trap),
    /// The model stopped.
    Fault(Fault),
    /// They answered differently.
    Answer {
        /// The interpreter's.
        expected: Option<u64>,
        /// The model's.
        got: Option<u64>,
    },
    /// They left the buffer differently.
    Memory,
    /// They called the helpers differently.
    Calls,
}

/// The machine instructions a model may take for each IR step: the most a
/// backend lowers one instruction or edge to, with room.
const MODEL_STEPS: u64 = 64;

/// Run `case` on the interpreter, at most `fuel` steps, and on `target`'s
/// model, comparing what each answered, left in the buffer and called.
///
/// # Errors
///
/// Where they parted.
pub fn agree(target: Target, case: &Case, fuel: u64) -> Result<(), Disagreement> {
    let mut memory = case.memory.clone();
    let mut helpers = TestHelpers::default();
    let expected = interp::run(&case.func, &case.args, &mut memory, &mut helpers, fuel)
        .map_err(Disagreement::Trap)?;
    let mut model_memory = case.memory.clone();
    let mut calls = TestCalls::default();
    let got = model::run(
        target,
        &case.func,
        &case.args,
        &mut model_memory,
        &mut calls,
        fuel.saturating_mul(MODEL_STEPS),
    )
    .map_err(Disagreement::Fault)?;
    if got != expected {
        return Err(Disagreement::Answer { expected, got });
    }
    if model_memory != memory {
        return Err(Disagreement::Memory);
    }
    if calls.calls != helpers.calls {
        return Err(Disagreement::Calls);
    }
    Ok(())
}

/// The arity each helper is called with in a case: two, three, one, none.
#[must_use]
pub const fn arity(index: u32) -> usize {
    match index % 4 {
        0 => 2,
        1 => 3,
        2 => 1,
        _ => 0,
    }
}

/// A function and what it is run with.
#[derive(Clone, Debug)]
pub struct Case {
    /// The function.
    pub func: Function,
    /// Its arguments: the buffer, the table, then its own.
    pub args: Vec<u64>,
    /// The memory it starts with.
    pub memory: TestMemory,
}

struct Gen<'a> {
    b: Builder,
    next: &'a mut dyn FnMut() -> u64,
    base: Value,
    middle: Value,
    table: Value,
    /// The values defined where the code being made is, 32-bit first.
    pool: [Vec<Value>; 2],
}

const fn kind(ty: Type) -> usize {
    match ty {
        Type::I32 => 0,
        Type::I64 => 1,
    }
}

impl Gen<'_> {
    fn below(&mut self, n: usize) -> usize {
        usize::try_from((self.next)() % n.max(1) as u64).unwrap_or(0)
    }

    fn choose<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }

    fn pick(&mut self, ty: Type) -> Value {
        let n = self.pool[kind(ty)].len();
        if n == 0 {
            let imm = (self.next)();
            return self.b.iconst(ty, imm);
        }
        // Old values as often as new ones, so many stay live.
        let at = self.below(n);
        self.pool[kind(ty)][at]
    }

    fn keep(&mut self, value: Value) {
        let ty = self.b.type_of(value);
        self.pool[kind(ty)].push(value);
    }

    /// The pool's sizes, to cut it back to.
    fn mark(&self) -> [usize; 2] {
        [self.pool[0].len(), self.pool[1].len()]
    }

    fn cut(&mut self, mark: [usize; 2]) {
        for (pool, len) in self.pool.iter_mut().zip(mark) {
            pool.truncate(len);
        }
    }

    fn ty(&mut self) -> Type {
        self.choose(&[Type::I32, Type::I64])
    }

    fn constant(&mut self, ty: Type) -> Value {
        let imm = match self.below(3) {
            0 => self.choose(&EDGES),
            1 => (self.next)() % 64,
            _ => (self.next)(),
        };
        self.b.iconst(ty, imm)
    }

    /// A base and an offset from it that a load or store of `width` keeps
    /// within the buffer.
    fn address(&mut self, width: Width) -> (Value, i32) {
        let room = BUFFER_BYTES - width.bytes() as usize;
        let from_start = i32::try_from(self.below(room + 1)).unwrap_or(0);
        if self.below(2) == 0 {
            (self.base, from_start)
        } else {
            (self.middle, from_start - MIDDLE)
        }
    }

    fn width(&mut self, ty: Type) -> Width {
        let widths = [Width::W8, Width::W16, Width::W32, Width::W64];
        self.choose(if ty == Type::I32 {
            &widths[..3]
        } else {
            &widths
        })
    }

    fn operand(&mut self, ty: Type) -> Value {
        if self.below(3) == 0 {
            self.constant(ty)
        } else {
            self.pick(ty)
        }
    }

    fn op(&mut self) {
        let ty = self.ty();
        let value = match self.below(16) {
            0 => self.constant(ty),
            1 => {
                let op = self.choose(&[
                    UnOp::Neg,
                    UnOp::Not,
                    UnOp::Uext8,
                    UnOp::Uext16,
                    UnOp::Sext8,
                    UnOp::Sext16,
                ]);
                let a = self.pick(ty);
                self.b.unary(op, a)
            }
            2..=6 => {
                let op = self.choose(&[
                    BinOp::Add,
                    BinOp::Sub,
                    BinOp::Mul,
                    BinOp::And,
                    BinOp::Or,
                    BinOp::Xor,
                    BinOp::Shl,
                    BinOp::Shr,
                    BinOp::Sar,
                    BinOp::Rotl,
                    BinOp::Rotr,
                    BinOp::Udiv,
                    BinOp::Sdiv,
                    BinOp::Urem,
                    BinOp::Srem,
                ]);
                let a = self.pick(ty);
                let b = self.operand(ty);
                self.b.binary(op, a, b)
            }
            7 => {
                let cond = self.choose(&[
                    Cond::Eq,
                    Cond::Ne,
                    Cond::Slt,
                    Cond::Sle,
                    Cond::Sgt,
                    Cond::Sge,
                    Cond::Ult,
                    Cond::Ule,
                    Cond::Ugt,
                    Cond::Uge,
                ]);
                let a = self.pick(ty);
                let b = self.operand(ty);
                self.b.icmp(cond, a, b)
            }
            8 => {
                let cond = self.pick(Type::I32);
                let (a, b) = (self.pick(ty), self.pick(ty));
                self.b.select(cond, a, b)
            }
            9 => {
                let a = self.pick(Type::I32);
                let signed = self.below(2) == 0;
                self.b.extend(signed, a)
            }
            10 => {
                let a = self.pick(Type::I64);
                self.b.reduce(a)
            }
            11 | 12 => {
                let width = self.width(ty);
                let (base, offset) = self.address(width);
                let signed = self.below(2) == 0;
                self.b.load(ty, width, signed, base, offset)
            }
            13 => {
                let width = self.width(ty);
                let (base, offset) = self.address(width);
                let value = self.operand(ty);
                self.b.store(width, value, base, offset);
                return;
            }
            _ => {
                let index = self.choose(&[0, 1, 2, 3]);
                let args: Vec<Value> = (0..arity(index))
                    .map(|_| {
                        let ty = self.ty();
                        self.pick(ty)
                    })
                    .collect();
                let ret = self.choose(&[None, Some(Type::I32), Some(Type::I64)]);
                match self.b.call(self.table, index, &args, ret) {
                    Some(value) => value,
                    None => return,
                }
            }
        };
        self.keep(value);
    }

    fn straight(&mut self, ops: usize) {
        for _ in 0..ops {
            self.op();
        }
    }

    /// A diamond: two blocks each computing, joining at one whose
    /// parameters take a value from each.
    fn diamond(&mut self) {
        let cond = self.pick(Type::I32);
        let ty = self.ty();
        let then = self.b.block(&[]);
        let other = self.b.block(&[]);
        let join = self.b.block(&[ty, Type::I32]);
        self.b.branch(cond, (then, &[]), (other, &[]));
        let outer = self.mark();
        for arm in [then, other] {
            self.b.switch_to(arm);
            let ops = 1 + self.below(6);
            self.straight(ops);
            let (a, c) = (self.pick(ty), self.pick(Type::I32));
            self.b.jump(join, &[a, c]);
            // What the arm made is not defined where the arms join.
            self.cut(outer);
        }
        self.b.switch_to(join);
        let params = self.b.params(join).to_vec();
        for param in params {
            self.keep(param);
        }
    }

    /// A loop run a bounded number of times, carrying a value round.
    fn looped(&mut self) {
        let ty = self.ty();
        let carried = self.pick(ty);
        let trips = (self.next)() % 5;
        let zero = self.b.iconst(Type::I32, 0);
        let head = self.b.block(&[Type::I32, ty]);
        let exit = self.b.block(&[ty]);
        self.b.jump(head, &[zero, carried]);
        self.b.switch_to(head);
        let (count, value) = (self.b.params(head)[0], self.b.params(head)[1]);
        let outer = self.mark();
        self.keep(value);
        let ops = 1 + self.below(5);
        self.straight(ops);
        let step = self.pick(ty);
        let one = self.b.iconst(Type::I32, 1);
        let bumped = self.b.binary(BinOp::Add, count, one);
        let limit = self.b.iconst(Type::I32, trips);
        let again = self.b.icmp(Cond::Ult, bumped, limit);
        self.b
            .branch(again, (head, &[bumped, step]), (exit, &[step]));
        self.cut(outer);
        self.b.switch_to(exit);
        let out = self.b.params(exit)[0];
        self.keep(out);
    }
}

/// A random case drawn from `next`.
///
/// # Errors
///
/// The builder's refusal, which a case it makes never meets.
pub fn generate(next: &mut dyn FnMut() -> u64) -> Result<Case, BuildError> {
    let own: Vec<Type> = (0..(next() % 5))
        .map(|_| {
            if next().is_multiple_of(2) {
                Type::I32
            } else {
                Type::I64
            }
        })
        .collect();
    let mut params = vec![Type::I64, Type::I64];
    params.extend(&own);
    let mut b = Builder::new(&params, Some(Type::I64));
    let entry = b.params(Block::ENTRY).to_vec();
    // A first block of its own, which the loops' and diamonds' edges may
    // enter: no edge enters the entry block.
    let body = b.block(&[]);
    b.jump(body, &[]);
    b.switch_to(body);
    let offset = b.iconst(Type::I64, u64::from(MIDDLE.cast_unsigned()));
    let middle = b.binary(BinOp::Add, entry[0], offset);
    let mut g = Gen {
        b,
        next,
        base: entry[0],
        middle,
        table: entry[1],
        pool: [Vec::new(), Vec::new()],
    };
    for &param in &entry[2..] {
        g.keep(param);
    }
    let regions = 1 + g.below(6);
    for _ in 0..regions {
        match g.below(4) {
            0 => g.diamond(),
            1 => g.looped(),
            _ => {
                let ops = 4 + g.below(20);
                g.straight(ops);
            }
        }
    }
    // Fold every value made into the answer.
    let mut answer = g.b.iconst(Type::I64, 0x0123_4567_89AB_CDEF);
    let made: Vec<Value> = g.pool.iter().flatten().copied().collect();
    for value in made {
        let wide = if g.b.type_of(value) == Type::I32 {
            g.b.extend(false, value)
        } else {
            value
        };
        let seven = g.b.iconst(Type::I64, 7);
        let rotated = g.b.binary(BinOp::Rotl, answer, seven);
        answer = g.b.binary(BinOp::Xor, rotated, wide);
    }
    g.b.ret(Some(answer));
    let Gen { b, next, .. } = g;
    let func = b.finish()?;
    let mut args = vec![BUFFER, TABLE];
    for ty in &own {
        args.push(ty.wrap(next()));
    }
    let memory = TestMemory::new(next);
    Ok(Case { func, args, memory })
}
