//! Each instruction of a block lowered into the program's: its operands
//! read as registers, its result given a register of its own — the tile's
//! where every operand is the tile's and the operation is the same for
//! every lane — and composites taken apart and put together word by word.

use alloc::vec::Vec;
use core::cell::Cell;

use tairix_spirv::enumerant::{Decoration, FPRoundingMode, ImageOperands, MemorySemantics};
use tairix_spirv::{Id, Instruction, Op, Type, TypeId};

use super::layout::{Built, Instance};
use super::{component, num, push, Count, Fault, Lowering, Part, Val, MAX_PROGRAM_INSTRUCTIONS};
use crate::software::shader::ir::{
    Atomic, BitField, Cmp, Conv, Derivative, Fold, Group, Image, ImageOp, Inst, Linear, Num, Op1,
    Op2, Op3, Order, Pack, Reg, Relation, Scan, Shift, Wide,
};

/// What a lane's operand holds: its register and type.
type Data = (Reg, TypeId);

impl Lowering<'_> {
    /// Lower instruction `at` of a block of `instance` into `built`.
    pub(crate) fn select(
        &mut self,
        instances: &mut [Instance],
        built: &mut Built,
        instance: u32,
        at: u32,
    ) -> Result<(), Fault> {
        let i = *self.at(at)?;
        let op = i.op();
        let mut s = Selection {
            l: self,
            instances,
            insts: &mut built.insts,
            instance,
            i,
            result_type: Cell::new(None),
        };
        s.lower(op)
    }

    /// The memory barrier a control barrier makes of its semantics.
    pub(crate) fn barrier_fence(
        &mut self,
        built: &mut Built,
        i: &Instruction,
    ) -> Result<(), Fault> {
        let order = order(self.constant_u32(self.id(i, 2)?)?);
        if order != Order::Relaxed {
            self.count(Count::Instructions, 1)?;
            push(
                &mut built.insts,
                Inst::Fence(order),
                MAX_PROGRAM_INSTRUCTIONS,
            )?;
        }
        Ok(())
    }
}

/// The ordering a set of memory semantics asks for.
fn order(semantics: u32) -> Order {
    let has = |bits: MemorySemantics| semantics & bits.0 != 0;
    if has(MemorySemantics::SEQUENTIALLY_CONSISTENT) {
        Order::SeqCst
    } else if has(MemorySemantics::ACQUIRE_RELEASE)
        || (has(MemorySemantics::ACQUIRE) && has(MemorySemantics::RELEASE))
    {
        Order::AcquireRelease
    } else if has(MemorySemantics::ACQUIRE) {
        Order::Acquire
    } else if has(MemorySemantics::RELEASE) {
        Order::Release
    } else {
        Order::Relaxed
    }
}

/// Consecutive picks of a shuffle: where in its result they start, the word
/// of the source they start at, `None` for undefined picks, and their words.
#[derive(Copy, Clone)]
struct Run {
    at: u32,
    from: Option<(Reg, u32)>,
    words: u32,
}

impl Run {
    /// Whether a pick from `next` carries on from the run's last one.
    fn continued_by(&self, next: Option<(Reg, u32)>) -> bool {
        match (self.from, next) {
            (Some((reg, word)), Some((next, at))) => reg == next && word + self.words == at,
            (None, None) => true,
            _ => false,
        }
    }
}

/// One instruction being lowered.
struct Selection<'l, 'a, 'i> {
    l: &'l mut Lowering<'a>,
    instances: &'i mut [Instance],
    insts: &'i mut Vec<Inst>,
    instance: u32,
    i: Instruction,
    /// The instruction's result type, once it has been found.
    result_type: Cell<Option<TypeId>>,
}

impl Selection<'_, '_, '_> {
    fn id(&self, index: usize) -> Result<Id, Fault> {
        self.l.id(&self.i, index)
    }

    fn value(&mut self, index: usize) -> Result<Val, Fault> {
        let id = self.id(index)?;
        self.l.value(self.instances, self.instance, id)
    }

    fn data(&mut self, index: usize) -> Result<Data, Fault> {
        match self.value(index)? {
            Val::Data { reg, ty } => Ok((reg, ty)),
            _ => Err(Fault::Malformed),
        }
    }

    fn result_type(&self) -> Result<TypeId, Fault> {
        if let Some(ty) = self.result_type.get() {
            return Ok(ty);
        }
        let ty = self.l.result_type(&self.i)?;
        self.result_type.set(Some(ty));
        Ok(ty)
    }

    /// A register for the result, the tile's where `uniform`.
    fn result(&mut self, uniform: bool) -> Result<Reg, Fault> {
        let ty = self.result_type()?;
        let words = self.l.words(ty)?;
        let reg = self.l.fresh(words, uniform)?;
        self.define(Val::Data { reg, ty })?;
        Ok(reg)
    }

    fn define(&mut self, val: Val) -> Result<(), Fault> {
        let id = self.l.module().result(&self.i).ok_or(Fault::Malformed)?;
        self.l.define(self.instances, self.instance, id, val)
    }

    fn emit(&mut self, inst: Inst) -> Result<(), Fault> {
        self.l.count(Count::Instructions, 1)?;
        push(self.insts, inst, MAX_PROGRAM_INSTRUCTIONS)
    }

    /// The result's scalar and component count.
    fn shape(&self) -> Result<(Num, u32), Fault> {
        self.l.components(self.result_type()?)
    }

    fn lower(&mut self, op: Op) -> Result<(), Fault> {
        match op {
            Op::NOP
            | Op::LINE
            | Op::NO_LINE
            | Op::SELECTION_MERGE
            | Op::LOOP_MERGE
            | Op::VARIABLE => Ok(()),
            Op::UNDEF => {
                let ty = self.result_type()?;
                let words = self.l.words(ty)?;
                let reg = self.l.zeros(words)?;
                self.define(Val::Data { reg, ty })
            }
            Op::CONTROL_BARRIER => {
                let order = order(self.l.constant_u32(self.id(2)?)?);
                if order == Order::Relaxed {
                    Ok(())
                } else {
                    self.emit(Inst::Fence(order))
                }
            }
            Op::MEMORY_BARRIER => {
                let order = order(self.l.constant_u32(self.id(1)?)?);
                if order == Order::Relaxed {
                    Ok(())
                } else {
                    self.emit(Inst::Fence(order))
                }
            }
            Op::COPY_OBJECT | Op::COPY_LOGICAL => {
                let value = self.value(2)?;
                match value {
                    Val::Data { reg, .. } => {
                        let ty = self.result_type()?;
                        self.define(Val::Data { reg, ty })
                    }
                    other => self.define(other),
                }
            }
            Op::EXT_INST => self.extended(),
            _ => {
                if let Some(binary) = binary(op) {
                    return self.map2(binary);
                }
                if let Some(unary) = unary(op) {
                    return self.map1(unary);
                }
                if let Some(cmp) = compare(op) {
                    return self.compare(cmp);
                }
                if let Some(shift) = shift(op) {
                    return self.shift(shift);
                }
                if let Some(conv) = conversion(op) {
                    return self.convert(self.rounded(conv)?);
                }
                if let Some(atomic) = atomic(op) {
                    return self.atomic(atomic);
                }
                if let Some(group) = self.group_op(op)? {
                    return self.group(group);
                }
                self.other(op)
            }
        }
    }

    fn map2(&mut self, op: Op2) -> Result<(), Fault> {
        let (a, _) = self.data(2)?;
        let (b, _) = self.data(3)?;
        let (num, count) = self.shape()?;
        let dst = self.result(a.is_uniform() && b.is_uniform())?;
        self.emit(Inst::Map2 {
            op,
            num,
            count,
            dst,
            a,
            b,
        })
    }

    fn map1(&mut self, op: Op1) -> Result<(), Fault> {
        let (a, ty) = self.data(2)?;
        let (num, count) = self.l.components(ty)?;
        let dst = self.result(a.is_uniform())?;
        self.emit(Inst::Map1 {
            op,
            num,
            count,
            dst,
            a,
        })
    }

    fn compare(&mut self, op: Cmp) -> Result<(), Fault> {
        let (a, ty) = self.data(2)?;
        let (b, _) = self.data(3)?;
        let (num, count) = self.l.components(ty)?;
        let dst = self.result(a.is_uniform() && b.is_uniform())?;
        self.emit(Inst::Compare {
            op,
            num,
            count,
            dst,
            a,
            b,
        })
    }

    fn shift(&mut self, op: Shift) -> Result<(), Fault> {
        let (a, ty) = self.data(2)?;
        let (b, by_ty) = self.data(3)?;
        let (num, count) = self.l.components(ty)?;
        let (by, _) = self.l.components(by_ty)?;
        let dst = self.result(a.is_uniform() && b.is_uniform())?;
        self.emit(Inst::Shift {
            op,
            num,
            by,
            count,
            dst,
            a,
            b,
        })
    }

    /// `conv`, rounded toward zero where an admitted `FPRoundingMode RTZ`
    /// decorates the float conversion it lowers.
    fn rounded(&self, conv: Conv) -> Result<Conv, Fault> {
        if conv != Conv::Float {
            return Ok(conv);
        }
        let id = self.l.module().result(&self.i).ok_or(Fault::Malformed)?;
        let mode = self.l.decoration(id, Decoration::FP_ROUNDING_MODE);
        Ok(if mode == Some(FPRoundingMode::RTZ.0) {
            Conv::FloatTowardZero
        } else {
            Conv::Float
        })
    }

    fn convert(&mut self, op: Conv) -> Result<(), Fault> {
        let (a, ty) = self.data(2)?;
        let (from, count) = self.l.components(ty)?;
        let (to, _) = self.shape()?;
        let dst = self.result(a.is_uniform())?;
        self.emit(Inst::Convert {
            op,
            from,
            to,
            count,
            dst,
            a,
        })
    }

    /// The instructions that fit no family above.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per instruction no family above takes"
    )]
    fn other(&mut self, op: Op) -> Result<(), Fault> {
        match op {
            Op::LOGICAL_NOT => self.map1(Op1::LogicalNot),
            Op::ANY | Op::ALL => {
                let (a, ty) = self.data(2)?;
                let (_, count) = self.l.components(ty)?;
                let dst = self.result(a.is_uniform())?;
                self.emit(Inst::AnyAll {
                    all: op == Op::ALL,
                    count,
                    dst,
                    a,
                })
            }
            Op::SELECT => {
                let (cond, cond_ty) = self.data(2)?;
                let (a, _) = self.data(3)?;
                let (b, _) = self.data(4)?;
                let ty = self.result_type()?;
                let words = self.l.words(ty)?;
                let per = match self.l.ty(cond_ty)? {
                    Type::Vector { count, .. } => words / count.max(1),
                    _ => words,
                };
                let dst = self.result(cond.is_uniform() && a.is_uniform() && b.is_uniform())?;
                self.emit(Inst::Select {
                    dst,
                    cond,
                    a,
                    b,
                    words,
                    per,
                })
            }
            Op::QUANTIZE_TO_F16 => self.convert(Conv::QuantizeToF16),
            Op::BITCAST => self.bitcast(),
            Op::VECTOR_EXTRACT_DYNAMIC => {
                let (vector, ty) = self.data(2)?;
                let (index, index_ty) = self.data(3)?;
                let (scalar, count) = self.l.components(ty)?;
                let (by, _) = self.l.components(index_ty)?;
                let dst = self.result(vector.is_uniform() && index.is_uniform())?;
                self.emit(Inst::Extract {
                    dst,
                    vector,
                    index,
                    by,
                    count,
                    per: scalar.words(),
                })
            }
            Op::VECTOR_INSERT_DYNAMIC => {
                let (vector, ty) = self.data(2)?;
                let (value, _) = self.data(3)?;
                let (index, index_ty) = self.data(4)?;
                let (scalar, count) = self.l.components(ty)?;
                let (by, _) = self.l.components(index_ty)?;
                let dst =
                    self.result(vector.is_uniform() && value.is_uniform() && index.is_uniform())?;
                self.emit(Inst::Insert {
                    dst,
                    vector,
                    value,
                    index,
                    by,
                    count,
                    per: scalar.words(),
                })
            }
            Op::VECTOR_SHUFFLE => self.shuffle(),
            Op::COMPOSITE_CONSTRUCT => self.construct(),
            Op::COMPOSITE_EXTRACT => self.extract(),
            Op::COMPOSITE_INSERT => self.insert(),
            Op::TRANSPOSE => {
                let (a, ty) = self.data(2)?;
                let Type::Matrix {
                    columns,
                    rows,
                    scalar,
                    ..
                } = self.l.ty(ty)?
                else {
                    return Err(Fault::Malformed);
                };
                self.linear(
                    Linear::Transpose {
                        rows,
                        cols: columns,
                    },
                    num(scalar),
                    (a, a, a),
                )
            }
            Op::VECTOR_TIMES_SCALAR | Op::MATRIX_TIMES_SCALAR => {
                let (a, ty) = self.data(2)?;
                let (b, _) = self.data(3)?;
                let (scalar, count) = self.float_elements(ty)?;
                self.linear(Linear::Scale { count }, scalar, (a, b, b))
            }
            Op::DOT => {
                let (a, ty) = self.data(2)?;
                let (b, _) = self.data(3)?;
                let (scalar, n) = self.l.components(ty)?;
                self.linear(Linear::Dot { n }, scalar, (a, b, b))
            }
            Op::MATRIX_TIMES_VECTOR => {
                let (a, ty) = self.data(2)?;
                let (b, _) = self.data(3)?;
                let (scalar, cols, rows) = self.matrix(ty)?;
                self.linear(Linear::MatrixVector { rows, cols }, scalar, (a, b, b))
            }
            Op::VECTOR_TIMES_MATRIX => {
                let (a, _) = self.data(2)?;
                let (b, ty) = self.data(3)?;
                let (scalar, cols, rows) = self.matrix(ty)?;
                self.linear(Linear::VectorMatrix { rows, cols }, scalar, (a, b, b))
            }
            Op::MATRIX_TIMES_MATRIX => {
                let (a, left) = self.data(2)?;
                let (b, right) = self.data(3)?;
                let (scalar, inner, rows) = self.matrix(left)?;
                let (_, cols, _) = self.matrix(right)?;
                self.linear(
                    Linear::MatrixMatrix { rows, inner, cols },
                    scalar,
                    (a, b, b),
                )
            }
            Op::OUTER_PRODUCT => {
                let (a, ty) = self.data(2)?;
                let (b, right) = self.data(3)?;
                let (scalar, rows) = self.l.components(ty)?;
                let (_, cols) = self.l.components(right)?;
                self.linear(Linear::Outer { rows, cols }, scalar, (a, b, b))
            }
            Op::I_ADD_CARRY | Op::I_SUB_BORROW | Op::U_MUL_EXTENDED | Op::S_MUL_EXTENDED => {
                let (a, ty) = self.data(2)?;
                let (b, _) = self.data(3)?;
                let (scalar, count) = self.l.components(ty)?;
                let wide = match op {
                    Op::I_ADD_CARRY => Wide::AddCarry,
                    Op::I_SUB_BORROW => Wide::SubBorrow,
                    Op::U_MUL_EXTENDED => Wide::UMulExtended,
                    _ => Wide::SMulExtended,
                };
                let dst = self.result(a.is_uniform() && b.is_uniform())?;
                let second = dst.at(count * scalar.words());
                self.emit(Inst::Wide {
                    op: wide,
                    num: scalar,
                    count,
                    dst,
                    second,
                    a,
                    b,
                })
            }
            Op::BIT_FIELD_INSERT => {
                let (base, ty) = self.data(2)?;
                let (insert, _) = self.data(3)?;
                let (offset, by_ty) = self.data(4)?;
                let (bits, _) = self.data(5)?;
                self.bit_field(BitField::Insert, (base, ty), insert, (offset, by_ty), bits)
            }
            Op::BIT_FIELD_S_EXTRACT | Op::BIT_FIELD_U_EXTRACT => {
                let (base, ty) = self.data(2)?;
                let (offset, by_ty) = self.data(3)?;
                let (bits, _) = self.data(4)?;
                let kind = if op == Op::BIT_FIELD_S_EXTRACT {
                    BitField::SExtract
                } else {
                    BitField::UExtract
                };
                self.bit_field(kind, (base, ty), base, (offset, by_ty), bits)
            }
            Op::BIT_REVERSE => self.map1(Op1::BitReverse),
            Op::BIT_COUNT => {
                let (a, ty) = self.data(2)?;
                let (from, count) = self.l.components(ty)?;
                let (to, _) = self.shape()?;
                let uniform = a.is_uniform();
                if from == to {
                    let dst = self.result(uniform)?;
                    return self.emit(Inst::Map1 {
                        op: Op1::BitCount,
                        num: from,
                        count,
                        dst,
                        a,
                    });
                }
                let counted = self.l.fresh(count * from.words(), uniform)?;
                self.emit(Inst::Map1 {
                    op: Op1::BitCount,
                    num: from,
                    count,
                    dst: counted,
                    a,
                })?;
                let dst = self.result(uniform)?;
                self.emit(Inst::Convert {
                    op: Conv::Unsigned,
                    from,
                    to,
                    count,
                    dst,
                    a: counted,
                })
            }
            Op::D_PDX | Op::D_PDY | Op::FWIDTH => {
                let (a, ty) = self.data(2)?;
                let (scalar, count) = self.l.components(ty)?;
                let derivative = match op {
                    Op::D_PDX => Derivative::X,
                    Op::D_PDY => Derivative::Y,
                    _ => Derivative::Width,
                };
                let dst = self.result(false)?;
                self.emit(Inst::Derivative {
                    op: derivative,
                    fine: true,
                    num: scalar,
                    count,
                    dst,
                    a,
                })
            }
            Op::LOAD => self.load(),
            Op::STORE => self.store(),
            Op::COPY_MEMORY => {
                let target = self.value(0)?;
                let source = self.value(1)?;
                let Val::Pointer {
                    root,
                    pointee,
                    layout,
                    reg,
                    ..
                } = source
                else {
                    return Err(Fault::Malformed);
                };
                let words = self.l.words(pointee)?;
                let temp = self.l.fresh(words, false)?;
                let plan = self.l.plan(pointee, layout)?;
                self.emit(Inst::Load {
                    dst: temp,
                    root,
                    ptr: reg,
                    plan,
                })?;
                let Val::Pointer {
                    root,
                    pointee,
                    layout,
                    reg,
                    ..
                } = target
                else {
                    return Err(Fault::Malformed);
                };
                let plan = self.l.plan(pointee, layout)?;
                self.emit(Inst::Store {
                    src: temp,
                    root,
                    ptr: reg,
                    plan,
                })
            }
            Op::ACCESS_CHAIN | Op::IN_BOUNDS_ACCESS_CHAIN => {
                let ty = self.result_type()?;
                let Type::Pointer { pointee, .. } = self.l.ty(ty)? else {
                    return Err(Fault::Malformed);
                };
                let base = self.id(2)?;
                let count = self.l.operands(&self.i);
                let mut indexes = Vec::new();
                for index in 3..count {
                    push(&mut indexes, self.id(index)?, MAX_PROGRAM_INSTRUCTIONS)?;
                }
                let val = self.l.access_chain(
                    self.insts,
                    self.instances,
                    self.instance,
                    (base, &indexes),
                    pointee,
                )?;
                self.define(val)
            }
            Op::ARRAY_LENGTH => {
                let Val::Pointer {
                    root, pointee, reg, ..
                } = self.value(2)?
                else {
                    return Err(Fault::Malformed);
                };
                let member = self.l.word(&self.i, 3)?;
                let m = self.l.member(pointee, member)?;
                let offset = m.offset.ok_or(Fault::Malformed)?;
                let stride = self.l.admitted.array_stride(m.ty).ok_or(Fault::Malformed)?;
                let dst = self.result(reg.is_uniform())?;
                self.emit(Inst::ArrayLength {
                    dst,
                    root,
                    ptr: reg,
                    offset,
                    stride,
                })
            }
            Op::IMAGE_TEXEL_POINTER => {
                let Val::Pointer {
                    root: crate::software::shader::ir::Root::Binding(image),
                    reg: handle,
                    ..
                } = self.value(2)?
                else {
                    return Err(Fault::Malformed);
                };
                let (coord, coord_ty) = self.data(3)?;
                let (sample, _) = self.data(4)?;
                let (_, coords) = self.l.components(coord_ty)?;
                let ty = self.result_type()?;
                let Type::Pointer { pointee, .. } = self.l.ty(ty)? else {
                    return Err(Fault::Malformed);
                };
                let dst = self.l.fresh(2, false)?;
                self.emit(Inst::TexelPointer {
                    dst,
                    image,
                    handle,
                    coord,
                    coords: u8::try_from(coords).map_err(|_| Fault::Malformed)?,
                    sample,
                })?;
                self.define(Val::Pointer {
                    root: crate::software::shader::ir::Root::Binding(image),
                    pointee,
                    layout: super::Layout::Words,
                    reg: dst,
                    descriptors: false,
                })
            }
            Op::SAMPLED_IMAGE => {
                let Val::Handle {
                    image,
                    reg: image_reg,
                    ..
                } = self.value(2)?
                else {
                    return Err(Fault::Malformed);
                };
                let Val::Handle {
                    sampler,
                    reg: sampler_reg,
                    ..
                } = self.value(3)?
                else {
                    return Err(Fault::Malformed);
                };
                let ty = self.result_type()?;
                let reg = self.l.fresh(2, false)?;
                self.emit(Inst::Copy {
                    dst: reg,
                    src: image_reg,
                    words: 1,
                })?;
                self.emit(Inst::Copy {
                    dst: reg.at(1),
                    src: sampler_reg,
                    words: 1,
                })?;
                self.define(Val::Handle {
                    image,
                    sampler,
                    ty,
                    reg,
                })
            }
            Op::IMAGE => {
                let Val::Handle { image, reg, .. } = self.value(2)? else {
                    return Err(Fault::Malformed);
                };
                let ty = self.result_type()?;
                self.define(Val::Handle {
                    image,
                    sampler: None,
                    ty,
                    reg,
                })
            }
            _ => self.image(op),
        }
    }

    /// The float scalar and element count of a vector or matrix.
    fn float_elements(&self, ty: TypeId) -> Result<(Num, u32), Fault> {
        match self.l.ty(ty)? {
            Type::Matrix {
                columns,
                rows,
                scalar,
                ..
            } => Ok((num(scalar), columns * rows)),
            _ => self.l.components(ty),
        }
    }

    /// A matrix's scalar, columns and rows.
    fn matrix(&self, ty: TypeId) -> Result<(Num, u32, u32), Fault> {
        match self.l.ty(ty)? {
            Type::Matrix {
                columns,
                rows,
                scalar,
                ..
            } => Ok((num(scalar), columns, rows)),
            _ => Err(Fault::Malformed),
        }
    }

    fn linear(&mut self, op: Linear, scalar: Num, (a, b, c): (Reg, Reg, Reg)) -> Result<(), Fault> {
        let dst = self.result(a.is_uniform() && b.is_uniform() && c.is_uniform())?;
        self.emit(Inst::Linear {
            op,
            num: scalar,
            dst,
            a,
            b,
            c,
        })
    }

    fn bit_field(
        &mut self,
        op: BitField,
        (base, ty): Data,
        insert: Reg,
        (offset, by_ty): Data,
        bits: Reg,
    ) -> Result<(), Fault> {
        let (scalar, count) = self.l.components(ty)?;
        let (by, _) = self.l.components(by_ty)?;
        let uniform =
            base.is_uniform() && insert.is_uniform() && offset.is_uniform() && bits.is_uniform();
        let dst = self.result(uniform)?;
        self.emit(Inst::BitField {
            op,
            num: scalar,
            by,
            count,
            dst,
            base,
            insert,
            offset,
            bits,
        })
    }

    /// A bitcast: the words copied where both types hold them alike, and
    /// 16-bit components packed into or unpacked from whole words.
    fn bitcast(&mut self) -> Result<(), Fault> {
        let (a, from_ty) = self.data(2)?;
        let to_ty = self.result_type()?;
        let (from, from_count) = self.l.components(from_ty)?;
        let (to, to_count) = self.l.components(to_ty)?;
        let dst = self.result(a.is_uniform())?;
        let narrow = |n: Num| n.bits() == 16;
        if narrow(from) == narrow(to) {
            let words = self.l.words(to_ty)?;
            return self.emit(Inst::Copy { dst, src: a, words });
        }
        let pack = if narrow(from) {
            Pack::Halves { count: from_count }
        } else {
            Pack::UnpackHalves { count: to_count }
        };
        self.emit(Inst::Pack { op: pack, dst, a })
    }

    /// A shuffle as runs of consecutive picks from one source, each a copy,
    /// or a zero for undefined picks; a shuffle that is one run of one source
    /// reads it where it is.
    fn shuffle(&mut self) -> Result<(), Fault> {
        let (a, a_ty) = self.data(2)?;
        let (b, _) = self.data(3)?;
        let (scalar, a_count) = self.l.components(a_ty)?;
        let per = scalar.words();
        let uniform = a.is_uniform() && b.is_uniform();
        let source = |pick: u32| match pick {
            u32::MAX => None,
            pick if pick < a_count => Some((a, pick * per)),
            pick => Some((b, (pick - a_count) * per)),
        };
        let mut dst = None;
        let mut run: Option<Run> = None;
        let count = self.l.operands(&self.i);
        for (out, index) in (0u32..).zip(4..count) {
            let from = source(self.l.word(&self.i, index)?);
            match &mut run {
                Some(run) if run.continued_by(from) => run.words += per,
                _ => {
                    if let Some(done) = run.take() {
                        self.copy_run(&mut dst, uniform, done)?;
                    }
                    let at = out.checked_mul(per).ok_or(Fault::Budget)?;
                    run = Some(Run {
                        at,
                        from,
                        words: per,
                    });
                }
            }
        }
        match run {
            Some(Run { at: 0, from, words }) if dst.is_none() => {
                let reg = match from {
                    Some((reg, word)) => reg.at(word),
                    None => self.l.zeros(words)?,
                };
                let ty = self.result_type()?;
                self.define(Val::Data { reg, ty })
            }
            Some(done) => self.copy_run(&mut dst, uniform, done),
            None => Err(Fault::Malformed),
        }
    }

    /// `run` written into the shuffle's result, made the first time.
    fn copy_run(&mut self, dst: &mut Option<Reg>, uniform: bool, run: Run) -> Result<(), Fault> {
        let dst = match *dst {
            Some(reg) => reg,
            None => *dst.insert(self.result(uniform)?),
        };
        let at = dst.at(run.at);
        self.emit(match run.from {
            Some((reg, word)) => Inst::Copy {
                dst: at,
                src: reg.at(word),
                words: run.words,
            },
            None => Inst::Zero {
                dst: at,
                words: run.words,
            },
        })
    }

    /// A composite of its constituents, those lying one after another in one
    /// register copied together; one made of one register's words in order
    /// read where they lie.
    fn construct(&mut self) -> Result<(), Fault> {
        let count = self.l.operands(&self.i);
        let mut parts: Vec<(Reg, u32)> = Vec::new();
        let mut uniform = true;
        for index in 2..count {
            let (reg, ty) = self.data(index)?;
            uniform &= reg.is_uniform();
            let words = self.l.words(ty)?;
            if words == 0 {
                continue;
            }
            if let Some((last, run)) = parts.last_mut() {
                if last.at(*run) == reg && self.l.one_register(*last, reg) {
                    *run = run.checked_add(words).ok_or(Fault::Budget)?;
                    continue;
                }
            }
            push(&mut parts, (reg, words), MAX_PROGRAM_INSTRUCTIONS)?;
        }
        let ty = self.result_type()?;
        if let [(reg, words)] = parts[..] {
            if words == self.l.words(ty)? {
                return self.define(Val::Data { reg, ty });
            }
        }
        let dst = self.result(uniform)?;
        let mut offset = 0u32;
        for (reg, words) in parts {
            self.emit(Inst::Copy {
                dst: dst.at(offset),
                src: reg,
                words,
            })?;
            offset = offset.checked_add(words).ok_or(Fault::Budget)?;
        }
        Ok(())
    }

    /// The word a composite's constituent at the literal path from operand
    /// `first` starts at, and its words.
    fn path(&mut self, ty: TypeId, first: usize) -> Result<(u32, u32), Fault> {
        let count = self.l.operands(&self.i);
        let mut offset = 0u32;
        let mut part = Part::Typed(ty);
        for index in first..count {
            let Part::Typed(at) = part else {
                return Err(Fault::Malformed);
            };
            let literal = self.l.word(&self.i, index)?;
            let (within, next) = self.l.part_of(at, literal)?;
            offset = offset.checked_add(within).ok_or(Fault::Budget)?;
            part = next;
        }
        Ok((offset, self.l.part_words(part)?))
    }

    /// A component of a vector, or a part of a constant, read where it lies;
    /// a part of anything larger is copied out, as reading it in place would
    /// keep the whole live as long as the part, and stop an insertion into
    /// the whole taking its place.
    fn extract(&mut self) -> Result<(), Fault> {
        let (composite, ty) = self.data(2)?;
        let (offset, words) = self.path(ty, 3)?;
        let part = composite.at(offset);
        if super::is_constant(composite) || matches!(self.l.ty(ty)?, Type::Vector { .. }) {
            let ty = self.result_type()?;
            return self.define(Val::Data { reg: part, ty });
        }
        let dst = self.result(composite.is_uniform())?;
        self.emit(Inst::Copy {
            dst,
            src: part,
            words,
        })
    }

    fn insert(&mut self) -> Result<(), Fault> {
        let (object, _) = self.data(2)?;
        let (composite, ty) = self.data(3)?;
        let (offset, words) = self.path(ty, 4)?;
        let all = self.l.words(ty)?;
        let dst = self.result(object.is_uniform() && composite.is_uniform())?;
        self.emit(Inst::Copy {
            dst,
            src: composite,
            words: all,
        })?;
        self.emit(Inst::Copy {
            dst: dst.at(offset),
            src: object,
            words,
        })
    }

    fn load(&mut self) -> Result<(), Fault> {
        let Val::Pointer {
            root,
            pointee,
            layout,
            reg,
            ..
        } = self.value(2)?
        else {
            return Err(Fault::Malformed);
        };
        let ty = self.result_type()?;
        match self.l.ty(pointee)? {
            Type::Image(_) | Type::Sampler | Type::SampledImage { .. } => {
                let crate::software::shader::ir::Root::Binding(binding) = root else {
                    return Err(Fault::Malformed);
                };
                let (image, sampler) = match self.l.ty(pointee)? {
                    Type::Image(_) => (Some(binding), None),
                    Type::Sampler => (None, Some(binding)),
                    _ => (Some(binding), Some(binding)),
                };
                let handle = self.l.fresh(2, reg.is_uniform())?;
                self.emit(Inst::Copy {
                    dst: handle,
                    src: reg,
                    words: 1,
                })?;
                self.emit(Inst::Copy {
                    dst: handle.at(1),
                    src: reg,
                    words: 1,
                })?;
                self.define(Val::Handle {
                    image,
                    sampler,
                    ty,
                    reg: handle,
                })
            }
            _ => {
                let uniform = reg.is_uniform() && self.l.reads_shared(root);
                let plan = self.l.plan(pointee, layout)?;
                let dst = self.result(uniform)?;
                self.emit(Inst::Load {
                    dst,
                    root,
                    ptr: reg,
                    plan,
                })
            }
        }
    }

    fn store(&mut self) -> Result<(), Fault> {
        let Val::Pointer {
            root,
            pointee,
            layout,
            reg,
            ..
        } = self.value(0)?
        else {
            return Err(Fault::Malformed);
        };
        let (src, _) = self.data(1)?;
        let plan = self.l.plan(pointee, layout)?;
        self.emit(Inst::Store {
            src,
            root,
            ptr: reg,
            plan,
        })
    }

    fn atomic(&mut self, op: Atomic) -> Result<(), Fault> {
        let stores = op == Atomic::Store;
        let pointer = if stores { 0 } else { 2 };
        let Val::Pointer { root, reg: ptr, .. } = self.value(pointer)? else {
            return Err(Fault::Malformed);
        };
        let semantics = self.l.constant_u32(self.id(pointer + 2)?)?;
        let order = order(semantics);
        let operand = |s: &mut Self, index: usize| -> Result<Reg, Fault> { Ok(s.data(index)?.0) };
        let (a, b) = match op {
            Atomic::Store => (operand(self, 3)?, operand(self, 3)?),
            Atomic::Load | Atomic::Increment | Atomic::Decrement => (ptr, ptr),
            Atomic::CompareExchange => (operand(self, 6)?, operand(self, 7)?),
            _ => (operand(self, 5)?, operand(self, 5)?),
        };
        let dst = if stores {
            self.l.fresh(1, false)?
        } else {
            self.result(false)?
        };
        self.emit(Inst::Atomic {
            op,
            order,
            root,
            ptr,
            dst,
            a,
            b,
        })
    }

    /// The subgroup operation `op` is, its group operation read where it
    /// takes one.
    fn group_op(&mut self, op: Op) -> Result<Option<Group>, Fault> {
        let scan = |s: &Self| -> Result<Scan, Fault> {
            Ok(match s.l.word(&s.i, 3)? {
                0 => Scan::Reduce,
                1 => Scan::Inclusive,
                2 => Scan::Exclusive,
                _ => return Err(Fault::Malformed),
            })
        };
        let fold = |f: Fold, s: &Self| -> Result<Option<Group>, Fault> {
            let (num, count) = s.l.components(s.result_type()?)?;
            Ok(Some(Group::Fold {
                op: f,
                num,
                count,
                scan: scan(s)?,
            }))
        };
        match op {
            Op::GROUP_NON_UNIFORM_ELECT => Ok(Some(Group::Elect)),
            Op::GROUP_NON_UNIFORM_ALL => Ok(Some(Group::All)),
            Op::GROUP_NON_UNIFORM_ANY => Ok(Some(Group::Any)),
            Op::GROUP_NON_UNIFORM_ALL_EQUAL => {
                let value = self.id(3)?;
                let instruction = self.l.module().definition(value);
                let ty = match instruction {
                    Some(def) if self.l.module().result_type(def).is_some() => {
                        self.l.result_type(def)?
                    }
                    _ => return Err(Fault::Malformed),
                };
                let (num, count) = self.l.components(ty)?;
                Ok(Some(Group::AllEqual { num, count }))
            }
            Op::GROUP_NON_UNIFORM_BROADCAST | Op::GROUP_NON_UNIFORM_BROADCAST_FIRST => {
                let ty = self.result_type()?;
                let words = self.l.words(ty)?;
                Ok(Some(if op == Op::GROUP_NON_UNIFORM_BROADCAST {
                    Group::Broadcast { words }
                } else {
                    Group::BroadcastFirst { words }
                }))
            }
            Op::GROUP_NON_UNIFORM_BALLOT => Ok(Some(Group::Ballot)),
            Op::GROUP_NON_UNIFORM_INVERSE_BALLOT => Ok(Some(Group::InverseBallot)),
            Op::GROUP_NON_UNIFORM_BALLOT_BIT_EXTRACT => Ok(Some(Group::BallotBitExtract)),
            Op::GROUP_NON_UNIFORM_BALLOT_BIT_COUNT => Ok(Some(Group::BallotBitCount(scan(self)?))),
            Op::GROUP_NON_UNIFORM_BALLOT_FIND_LSB => Ok(Some(Group::BallotFindLsb)),
            Op::GROUP_NON_UNIFORM_BALLOT_FIND_MSB => Ok(Some(Group::BallotFindMsb)),
            Op::GROUP_NON_UNIFORM_I_ADD => fold(Fold::IAdd, self),
            Op::GROUP_NON_UNIFORM_I_MUL => fold(Fold::IMul, self),
            Op::GROUP_NON_UNIFORM_S_MIN => fold(Fold::SMin, self),
            Op::GROUP_NON_UNIFORM_S_MAX => fold(Fold::SMax, self),
            Op::GROUP_NON_UNIFORM_U_MIN => fold(Fold::UMin, self),
            Op::GROUP_NON_UNIFORM_U_MAX => fold(Fold::UMax, self),
            Op::GROUP_NON_UNIFORM_F_ADD => fold(Fold::FAdd, self),
            Op::GROUP_NON_UNIFORM_F_MUL => fold(Fold::FMul, self),
            Op::GROUP_NON_UNIFORM_F_MIN => fold(Fold::FMin, self),
            Op::GROUP_NON_UNIFORM_F_MAX => fold(Fold::FMax, self),
            Op::GROUP_NON_UNIFORM_BITWISE_AND => fold(Fold::BitAnd, self),
            Op::GROUP_NON_UNIFORM_BITWISE_OR => fold(Fold::BitOr, self),
            Op::GROUP_NON_UNIFORM_BITWISE_XOR => fold(Fold::BitXor, self),
            Op::GROUP_NON_UNIFORM_LOGICAL_AND => fold(Fold::LogicalAnd, self),
            Op::GROUP_NON_UNIFORM_LOGICAL_OR => fold(Fold::LogicalOr, self),
            Op::GROUP_NON_UNIFORM_LOGICAL_XOR => fold(Fold::LogicalXor, self),
            _ => Ok(None),
        }
    }

    fn group(&mut self, op: Group) -> Result<(), Fault> {
        let value = match op {
            Group::Elect => None,
            Group::BallotBitCount(_) | Group::Fold { .. } => Some(4),
            _ => Some(3),
        };
        let a = match value {
            Some(index) => self.data(index)?.0,
            None => self.l.zeros(1)?,
        };
        let b = match op {
            Group::Broadcast { .. } | Group::BallotBitExtract => self.data(4)?.0,
            _ => a,
        };
        let dst = self.result(false)?;
        self.emit(Inst::Group { op, dst, a, b })
    }

    /// An image instruction: its handle, coordinate, and the operands the
    /// image-operand mask names, each in its bit's order.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per image instruction's operands"
    )]
    fn image(&mut self, op: Op) -> Result<(), Fault> {
        let (kind, dref, first_mask) = match op {
            Op::IMAGE_SAMPLE_IMPLICIT_LOD | Op::IMAGE_SAMPLE_EXPLICIT_LOD => (
                ImageOp::Sample {
                    projective: false,
                    dref: false,
                },
                false,
                4,
            ),
            Op::IMAGE_SAMPLE_DREF_IMPLICIT_LOD | Op::IMAGE_SAMPLE_DREF_EXPLICIT_LOD => (
                ImageOp::Sample {
                    projective: false,
                    dref: true,
                },
                true,
                5,
            ),
            Op::IMAGE_SAMPLE_PROJ_IMPLICIT_LOD | Op::IMAGE_SAMPLE_PROJ_EXPLICIT_LOD => (
                ImageOp::Sample {
                    projective: true,
                    dref: false,
                },
                false,
                4,
            ),
            Op::IMAGE_SAMPLE_PROJ_DREF_IMPLICIT_LOD | Op::IMAGE_SAMPLE_PROJ_DREF_EXPLICIT_LOD => (
                ImageOp::Sample {
                    projective: true,
                    dref: true,
                },
                true,
                5,
            ),
            Op::IMAGE_FETCH => (ImageOp::Fetch, false, 4),
            Op::IMAGE_GATHER => (
                ImageOp::Gather {
                    component: None,
                    dref: false,
                },
                false,
                5,
            ),
            Op::IMAGE_DREF_GATHER => (
                ImageOp::Gather {
                    component: None,
                    dref: true,
                },
                true,
                5,
            ),
            Op::IMAGE_READ => (ImageOp::Read, false, 4),
            Op::IMAGE_WRITE => (
                ImageOp::Write {
                    texel: Reg::uniform(0),
                    components: 0,
                },
                false,
                3,
            ),
            Op::IMAGE_QUERY_SIZE_LOD => (
                ImageOp::QuerySizeLod {
                    lod: Reg::uniform(0),
                },
                false,
                usize::MAX,
            ),
            Op::IMAGE_QUERY_SIZE => (ImageOp::QuerySize, false, usize::MAX),
            Op::IMAGE_QUERY_LEVELS => (ImageOp::QueryLevels, false, usize::MAX),
            Op::IMAGE_QUERY_SAMPLES => (ImageOp::QuerySamples, false, usize::MAX),
            Op::IMAGE_QUERY_LOD => (ImageOp::QueryLod, false, usize::MAX),
            _ => return Err(Fault::Malformed),
        };
        let writes = op == Op::IMAGE_WRITE;
        let handle_at = if writes { 0 } else { 2 };
        let Val::Handle {
            image: Some(image),
            sampler,
            ty: handle_ty,
            reg: handle,
        } = self.value(handle_at)?
        else {
            return Err(Fault::Malformed);
        };
        let component = self.texel_component(handle_ty)?;
        let mut operands = crate::software::shader::ir::ImageOperands::default();
        // The queries of size, levels and samples name no coordinate; the
        // query of a level of detail names one, as a sample does.
        let sized = matches!(
            kind,
            ImageOp::QuerySize
                | ImageOp::QuerySizeLod { .. }
                | ImageOp::QueryLevels
                | ImageOp::QuerySamples
        );
        let (coord, coord_num, coords) = if sized {
            let (scalar, count) = self.shape()?;
            (handle, scalar, count)
        } else {
            let (coord, coord_ty) = self.data(handle_at + 1)?;
            let (scalar, count) = self.l.components(coord_ty)?;
            (coord, scalar, count)
        };
        let mut kind = kind;
        match &mut kind {
            ImageOp::Write { texel, components } => {
                let (reg, ty) = self.data(2)?;
                *texel = reg;
                *components =
                    u8::try_from(self.l.components(ty)?.1).map_err(|_| Fault::Malformed)?;
            }
            ImageOp::QuerySizeLod { lod } => *lod = self.data(3)?.0,
            ImageOp::Gather {
                component,
                dref: false,
            } => *component = Some(self.data(4)?.0),
            _ => {}
        }
        if dref {
            operands.dref = Some(self.data(4)?.0);
        }
        if first_mask < self.l.operands(&self.i) {
            self.image_operands(first_mask, &mut operands)?;
        }
        let dst = if writes {
            Reg::uniform(0)
        } else {
            self.result(false)?
        };
        let sampler = sampler.filter(|_| {
            matches!(
                kind,
                ImageOp::Sample { .. } | ImageOp::Gather { .. } | ImageOp::QueryLod
            )
        });
        let images = &mut self.l.program.images;
        let at = u32::try_from(images.len()).map_err(|_| Fault::Budget)?;
        let image = Image {
            op: kind,
            binding: image,
            sampler,
            handle,
            coord,
            coords: u8::try_from(coords).map_err(|_| Fault::Malformed)?,
            coord_num,
            operands,
            component,
            dst,
        };
        push(images, image, MAX_PROGRAM_INSTRUCTIONS)?;
        self.emit(Inst::Image(at))
    }

    /// The image operands from the mask at operand `at`.
    fn image_operands(
        &mut self,
        at: usize,
        operands: &mut crate::software::shader::ir::ImageOperands,
    ) -> Result<(), Fault> {
        let mask = self.l.word(&self.i, at)?;
        let mut next = at + 1;
        let mut take = |s: &mut Self| -> Result<Reg, Fault> {
            let reg = s.data(next)?.0;
            next += 1;
            Ok(reg)
        };
        let has = |bit: ImageOperands| mask & bit.0 != 0;
        if has(ImageOperands::BIAS) {
            operands.bias = Some(take(self)?);
        }
        if has(ImageOperands::LOD) {
            operands.lod = Some(take(self)?);
        }
        if has(ImageOperands::GRAD) {
            let x = take(self)?;
            let y = take(self)?;
            operands.grad = Some((x, y));
        }
        if has(ImageOperands::CONST_OFFSET) || has(ImageOperands::OFFSET) {
            operands.offset = Some(take(self)?);
        }
        if has(ImageOperands::CONST_OFFSETS) {
            operands.offsets = Some(take(self)?);
        }
        if has(ImageOperands::SAMPLE) {
            operands.sample = Some(take(self)?);
        }
        if has(ImageOperands::MIN_LOD) {
            return Err(Fault::Malformed);
        }
        // SignExtend and ZeroExtend agree with the view's format wherever
        // Vulkan defines the read, and where they disagree the format's
        // widening stands, as a hardware descriptor's does.
        Ok(())
    }

    /// What the texels of the image a handle of `ty` names read as.
    fn texel_component(&self, ty: TypeId) -> Result<tairix_spirv::Component, Fault> {
        let image = match self.l.ty(ty)? {
            Type::SampledImage { image } => image,
            _ => ty,
        };
        let Type::Image(image) = self.l.ty(image)? else {
            return Err(Fault::Malformed);
        };
        let Type::Scalar(scalar) = self.l.ty(image.sampled)? else {
            return Err(Fault::Malformed);
        };
        Ok(component(scalar))
    }

    /// A `GLSL.std.450` instruction.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per extended-instruction family"
    )]
    fn extended(&mut self) -> Result<(), Fault> {
        let number = self.l.word(&self.i, 3)?;
        let arg = 4;
        match number {
            1..=24 | 27..=32 | 73..=75 => {
                let op = glsl_unary(number).ok_or(Fault::Malformed)?;
                let (a, ty) = self.data(arg)?;
                let (scalar, count) = self.l.components(ty)?;
                let dst = self.result(a.is_uniform())?;
                self.emit(Inst::Map1 {
                    op,
                    num: scalar,
                    count,
                    dst,
                    a,
                })
            }
            25 | 26 | 37..=42 | 48 | 79 | 80 => {
                let op = glsl_binary(number).ok_or(Fault::Malformed)?;
                let (a, ty) = self.data(arg)?;
                let (b, _) = self.data(arg + 1)?;
                let (scalar, count) = self.l.components(ty)?;
                let dst = self.result(a.is_uniform() && b.is_uniform())?;
                self.emit(Inst::Map2 {
                    op,
                    num: scalar,
                    count,
                    dst,
                    a,
                    b,
                })
            }
            43..=46 | 49 | 50 | 81 => {
                let op = glsl_ternary(number).ok_or(Fault::Malformed)?;
                let (a, ty) = self.data(arg)?;
                let (b, _) = self.data(arg + 1)?;
                let (c, _) = self.data(arg + 2)?;
                let (scalar, count) = self.l.components(ty)?;
                let dst = self.result(a.is_uniform() && b.is_uniform() && c.is_uniform())?;
                self.emit(Inst::Map3 {
                    op,
                    num: scalar,
                    count,
                    dst,
                    a,
                    b,
                    c,
                })
            }
            33 | 34 => {
                let (a, ty) = self.data(arg)?;
                let (scalar, n, _) = self.matrix(ty)?;
                let op = if number == 33 {
                    Linear::Determinant { n }
                } else {
                    Linear::Inverse { n }
                };
                self.linear(op, scalar, (a, a, a))
            }
            35 | 36 | 51 | 52 => self.split_float(number),
            53 => {
                let (a, ty) = self.data(arg)?;
                let (b, by_ty) = self.data(arg + 1)?;
                let (scalar, count) = self.l.components(ty)?;
                let (by, _) = self.l.components(by_ty)?;
                let dst = self.result(a.is_uniform() && b.is_uniform())?;
                self.emit(Inst::Ldexp {
                    num: scalar,
                    by,
                    count,
                    dst,
                    a,
                    b,
                })
            }
            54..=58 | 60..=64 => {
                let pack = match number {
                    54 => Pack::Snorm4x8,
                    55 => Pack::Unorm4x8,
                    56 => Pack::Snorm2x16,
                    57 => Pack::Unorm2x16,
                    58 => Pack::Half2x16,
                    60 => Pack::UnpackSnorm2x16,
                    61 => Pack::UnpackUnorm2x16,
                    62 => Pack::UnpackHalf2x16,
                    63 => Pack::UnpackSnorm4x8,
                    _ => Pack::UnpackUnorm4x8,
                };
                let (a, _) = self.data(arg)?;
                let dst = self.result(a.is_uniform())?;
                self.emit(Inst::Pack { op: pack, dst, a })
            }
            59 | 65 => {
                let (a, ty) = self.data(arg)?;
                let words = self.l.words(ty)?;
                let dst = self.result(a.is_uniform())?;
                self.emit(Inst::Copy { dst, src: a, words })
            }
            66..=72 => {
                let (a, ty) = self.data(arg)?;
                let (scalar, n) = self.l.components(ty)?;
                let b = if number == 66 || number == 69 {
                    a
                } else {
                    self.data(arg + 1)?.0
                };
                let (c, eta) = if number == 70 || number == 72 {
                    let (c, c_ty) = self.data(arg + 2)?;
                    (c, self.l.components(c_ty)?.0)
                } else {
                    (a, scalar)
                };
                let op = match number {
                    66 => Linear::Length { n },
                    67 => Linear::Distance { n },
                    68 => Linear::Cross,
                    69 => Linear::Normalize { n },
                    70 => Linear::FaceForward { n },
                    71 => Linear::Reflect { n },
                    _ => Linear::Refract { n, eta },
                };
                self.linear(op, scalar, (a, b, c))
            }
            _ => Err(Fault::Malformed),
        }
    }

    /// `Modf`, `ModfStruct`, `Frexp` and `FrexpStruct`: a float split in
    /// two, the second part stored through a pointer or kept beside the
    /// first.
    fn split_float(&mut self, number: u32) -> Result<(), Fault> {
        let (a, ty) = self.data(4)?;
        let (scalar, count) = self.l.components(ty)?;
        let op = if number <= 36 {
            Wide::Modf
        } else {
            Wide::Frexp
        };
        let structured = number == 36 || number == 52;
        let uniform = a.is_uniform();
        if structured {
            let dst = self.result(uniform)?;
            let second = dst.at(count * scalar.words());
            return self.emit(Inst::Wide {
                op,
                num: scalar,
                count,
                dst,
                second,
                a,
                b: a,
            });
        }
        let Val::Pointer {
            root,
            pointee,
            layout,
            reg,
            ..
        } = self.value(5)?
        else {
            return Err(Fault::Malformed);
        };
        let dst = self.result(uniform)?;
        let words = self.l.words(pointee)?;
        let second = self.l.fresh(words, uniform)?;
        self.emit(Inst::Wide {
            op,
            num: scalar,
            count,
            dst,
            second,
            a,
            b: a,
        })?;
        let plan = self.l.plan(pointee, layout)?;
        self.emit(Inst::Store {
            src: second,
            root,
            ptr: reg,
            plan,
        })
    }
}

fn binary(op: Op) -> Option<Op2> {
    Some(match op {
        Op::I_ADD => Op2::IAdd,
        Op::I_SUB => Op2::ISub,
        Op::I_MUL => Op2::IMul,
        Op::S_DIV => Op2::SDiv,
        Op::U_DIV => Op2::UDiv,
        Op::S_REM => Op2::SRem,
        Op::S_MOD => Op2::SMod,
        Op::U_MOD => Op2::UMod,
        Op::BITWISE_AND => Op2::BitAnd,
        Op::BITWISE_OR => Op2::BitOr,
        Op::BITWISE_XOR => Op2::BitXor,
        Op::F_ADD => Op2::FAdd,
        Op::F_SUB => Op2::FSub,
        Op::F_MUL => Op2::FMul,
        Op::F_DIV => Op2::FDiv,
        Op::F_REM => Op2::FRem,
        Op::F_MOD => Op2::FMod,
        Op::LOGICAL_AND => Op2::LogicalAnd,
        Op::LOGICAL_OR => Op2::LogicalOr,
        Op::LOGICAL_EQUAL => Op2::LogicalEqual,
        Op::LOGICAL_NOT_EQUAL => Op2::LogicalNotEqual,
        _ => return None,
    })
}

fn unary(op: Op) -> Option<Op1> {
    Some(match op {
        Op::S_NEGATE => Op1::SNegate,
        Op::NOT => Op1::Not,
        Op::F_NEGATE => Op1::FNegate,
        Op::IS_NAN => Op1::IsNan,
        Op::IS_INF => Op1::IsInf,
        _ => return None,
    })
}

fn compare(op: Op) -> Option<Cmp> {
    let float = |relation, ordered| Cmp::Float { relation, ordered };
    Some(match op {
        Op::I_EQUAL => Cmp::IEqual,
        Op::I_NOT_EQUAL => Cmp::INotEqual,
        Op::U_GREATER_THAN => Cmp::UGreater,
        Op::U_GREATER_THAN_EQUAL => Cmp::UGreaterEqual,
        Op::U_LESS_THAN => Cmp::ULess,
        Op::U_LESS_THAN_EQUAL => Cmp::ULessEqual,
        Op::S_GREATER_THAN => Cmp::SGreater,
        Op::S_GREATER_THAN_EQUAL => Cmp::SGreaterEqual,
        Op::S_LESS_THAN => Cmp::SLess,
        Op::S_LESS_THAN_EQUAL => Cmp::SLessEqual,
        Op::F_ORD_EQUAL => float(Relation::Equal, true),
        Op::F_UNORD_EQUAL => float(Relation::Equal, false),
        Op::F_ORD_NOT_EQUAL => float(Relation::NotEqual, true),
        Op::F_UNORD_NOT_EQUAL => float(Relation::NotEqual, false),
        Op::F_ORD_LESS_THAN => float(Relation::Less, true),
        Op::F_UNORD_LESS_THAN => float(Relation::Less, false),
        Op::F_ORD_GREATER_THAN => float(Relation::Greater, true),
        Op::F_UNORD_GREATER_THAN => float(Relation::Greater, false),
        Op::F_ORD_LESS_THAN_EQUAL => float(Relation::LessEqual, true),
        Op::F_UNORD_LESS_THAN_EQUAL => float(Relation::LessEqual, false),
        Op::F_ORD_GREATER_THAN_EQUAL => float(Relation::GreaterEqual, true),
        Op::F_UNORD_GREATER_THAN_EQUAL => float(Relation::GreaterEqual, false),
        _ => return None,
    })
}

fn shift(op: Op) -> Option<Shift> {
    Some(match op {
        Op::SHIFT_LEFT_LOGICAL => Shift::Left,
        Op::SHIFT_RIGHT_LOGICAL => Shift::RightLogical,
        Op::SHIFT_RIGHT_ARITHMETIC => Shift::RightArithmetic,
        _ => return None,
    })
}

fn conversion(op: Op) -> Option<Conv> {
    Some(match op {
        Op::CONVERT_F_TO_U => Conv::FloatToUnsigned,
        Op::CONVERT_F_TO_S => Conv::FloatToSigned,
        Op::CONVERT_S_TO_F => Conv::SignedToFloat,
        Op::CONVERT_U_TO_F => Conv::UnsignedToFloat,
        Op::U_CONVERT => Conv::Unsigned,
        Op::S_CONVERT => Conv::Signed,
        Op::F_CONVERT => Conv::Float,
        _ => return None,
    })
}

fn atomic(op: Op) -> Option<Atomic> {
    Some(match op {
        Op::ATOMIC_LOAD => Atomic::Load,
        Op::ATOMIC_STORE => Atomic::Store,
        Op::ATOMIC_EXCHANGE => Atomic::Exchange,
        Op::ATOMIC_COMPARE_EXCHANGE => Atomic::CompareExchange,
        Op::ATOMIC_I_INCREMENT => Atomic::Increment,
        Op::ATOMIC_I_DECREMENT => Atomic::Decrement,
        Op::ATOMIC_I_ADD => Atomic::Add,
        Op::ATOMIC_I_SUB => Atomic::Sub,
        Op::ATOMIC_S_MIN => Atomic::SMin,
        Op::ATOMIC_U_MIN => Atomic::UMin,
        Op::ATOMIC_S_MAX => Atomic::SMax,
        Op::ATOMIC_U_MAX => Atomic::UMax,
        Op::ATOMIC_AND => Atomic::And,
        Op::ATOMIC_OR => Atomic::Or,
        Op::ATOMIC_XOR => Atomic::Xor,
        _ => return None,
    })
}

/// The component-wise operation of one operand a `GLSL.std.450`
/// instruction is.
const fn glsl_unary(number: u32) -> Option<Op1> {
    Some(match number {
        1 => Op1::Round,
        2 => Op1::RoundEven,
        3 => Op1::Trunc,
        4 => Op1::FAbs,
        5 => Op1::SAbs,
        6 => Op1::FSign,
        7 => Op1::SSign,
        8 => Op1::Floor,
        9 => Op1::Ceil,
        10 => Op1::Fract,
        11 => Op1::Radians,
        12 => Op1::Degrees,
        13 => Op1::Sin,
        14 => Op1::Cos,
        15 => Op1::Tan,
        16 => Op1::Asin,
        17 => Op1::Acos,
        18 => Op1::Atan,
        19 => Op1::Sinh,
        20 => Op1::Cosh,
        21 => Op1::Tanh,
        22 => Op1::Asinh,
        23 => Op1::Acosh,
        24 => Op1::Atanh,
        27 => Op1::Exp,
        28 => Op1::Log,
        29 => Op1::Exp2,
        30 => Op1::Log2,
        31 => Op1::Sqrt,
        32 => Op1::InverseSqrt,
        73 => Op1::FindILsb,
        74 => Op1::FindSMsb,
        75 => Op1::FindUMsb,
        _ => return None,
    })
}

const fn glsl_binary(number: u32) -> Option<Op2> {
    Some(match number {
        25 => Op2::Atan2,
        26 => Op2::Pow,
        37 => Op2::FMin,
        38 => Op2::UMin,
        39 => Op2::SMin,
        40 => Op2::FMax,
        41 => Op2::UMax,
        42 => Op2::SMax,
        48 => Op2::Step,
        79 => Op2::NMin,
        80 => Op2::NMax,
        _ => return None,
    })
}

const fn glsl_ternary(number: u32) -> Option<Op3> {
    Some(match number {
        43 => Op3::FClamp,
        44 => Op3::UClamp,
        45 => Op3::SClamp,
        46 => Op3::FMix,
        49 => Op3::SmoothStep,
        50 => Op3::Fma,
        81 => Op3::NClamp,
        _ => return None,
    })
}
