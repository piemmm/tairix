//! What each floating-point and Advanced SIMD row decodes to: a
//! [`SimdFpKind`] names the form, and builds a [`SimdFp`] from the fields its
//! row's layout read, refusing the combinations the architecture leaves
//! UNDEFINED. [`SimdFp::fields`] is the inverse.
//!
//! The register fields are split: `D:Vd` and `M:Vm` read as a double
//! register's number, `Vn:N` as a single's, each rotated for the other.

use super::layout::{Field, Fields, Layout};
use crate::cond::Condition;
use crate::instruction::{
    BlockMode, ConvertRounding, SimdFp, SimdImmediateOp, SimdLongOp, SimdMiscOp, SimdScalarOp,
    SimdShiftOp, SimdThreeOp, Structure, StructureLanes, VfpDyadicOp, VfpMonadicOp, Width,
};
use crate::reg::Reg;
use crate::vfp::System;

/// The form a row decodes, without its operands.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum SimdFpKind {
    Dyadic(VfpDyadicOp),
    Select,
    Monadic(VfpMonadicOp),
    Compare,
    CompareZero,
    Precision,
    Half { double: bool },
    ToInteger(ConvertRounding),
    FromInteger,
    Fixed,
    MoveImmediate,
    CoreSingle,
    CorePair,
    CoreScalar { to_core: bool },
    CoreDuplicate,
    System,
    Transfer,
    Multiple,
    Three(SimdThreeOp),
    Long(SimdLongOp),
    ByScalar(SimdScalarOp),
    Shift(SimdShiftOp),
    Misc(SimdMiscOp),
    Immediate,
    Extract,
    Table,
    Duplicate,
    StructureMultiple,
    StructureLane,
    StructureAll,
}

/// A field of at most eight bits.
fn byte(f: &Fields, field: Field) -> u8 {
    f.get(field).to_le_bytes()[0]
}

/// `D:Vd` as `Vd:D`: a double's number field as a single's number.
const fn single_of(value: u8) -> u8 {
    (value & 15) << 1 | value >> 4
}

/// `Vn:N` as `N:Vn`: a single's number field as a double's number.
const fn double_of(value: u8) -> u8 {
    (value & 1) << 4 | value >> 1
}

fn reg_d(f: &Fields, double: bool) -> u8 {
    let value = byte(f, Field::CRd);
    if double {
        value
    } else {
        single_of(value)
    }
}

fn reg_n(f: &Fields, double: bool) -> u8 {
    let value = byte(f, Field::CRn);
    if double {
        double_of(value)
    } else {
        value
    }
}

fn reg_m(f: &Fields, double: bool) -> u8 {
    let value = byte(f, Field::CRm);
    if double {
        value
    } else {
        single_of(value)
    }
}

fn set_d(f: &mut Fields, n: u8, double: bool) {
    f.set(Field::CRd, u32::from(if double { n } else { double_of(n) }));
}

fn set_n(f: &mut Fields, n: u8, double: bool) {
    f.set(Field::CRn, u32::from(if double { single_of(n) } else { n }));
}

fn set_m(f: &mut Fields, n: u8, double: bool) {
    f.set(Field::CRm, u32::from(if double { n } else { double_of(n) }));
}

/// A register number above the file's 32, so a deposit of it fails.
const OUT_OF_RANGE: u32 = u32::MAX;

/// Whether every register named is even, as a quadword operand's is.
const fn even(registers: &[u8]) -> bool {
    let mut n = 0;
    while n < registers.len() {
        if registers[n] & 1 != 0 {
            return false;
        }
        n += 1;
    }
    true
}

/// `VSEL`'s conditions, by its two-bit field.
const SELECTED: [Condition; 4] = [Condition::Eq, Condition::Vs, Condition::Ge, Condition::Gt];

/// `VSEL`'s field for `cond`; a condition it cannot test is out of range.
const fn selected_bits(cond: Condition) -> u32 {
    match cond {
        Condition::Eq => 0,
        Condition::Vs => 1,
        Condition::Ge => 2,
        Condition::Gt => 3,
        _ => OUT_OF_RANGE,
    }
}

impl SimdFpKind {
    /// The instruction `f` holds under `layout`, or `None` where the
    /// architecture leaves the combination UNDEFINED.
    #[allow(clippy::too_many_lines)] // One arm per form: the inverse of `SimdFp::fields`.
    pub(crate) fn build(self, layout: &Layout, f: &Fields) -> Option<SimdFp> {
        let double = f.bit(Field::Precision);
        let quad = f.bit(Field::Q);
        Some(match self {
            Self::Dyadic(op) => SimdFp::Dyadic {
                op,
                double,
                d: reg_d(f, double),
                n: reg_n(f, double),
                m: reg_m(f, double),
            },
            Self::Select => SimdFp::Select {
                cond: SELECTED[usize::from(byte(f, Field::Opc1) & 3)],
                double,
                d: reg_d(f, double),
                n: reg_n(f, double),
                m: reg_m(f, double),
            },
            Self::Monadic(op) => SimdFp::Monadic {
                op,
                double,
                d: reg_d(f, double),
                m: reg_m(f, double),
            },
            Self::Compare => SimdFp::Compare {
                signalling: f.bit(Field::X),
                double,
                d: reg_d(f, double),
                m: Some(reg_m(f, double)),
            },
            Self::CompareZero => SimdFp::Compare {
                signalling: f.bit(Field::X),
                double,
                d: reg_d(f, double),
                m: None,
            },
            Self::Precision => SimdFp::Precision {
                to_double: !double,
                d: reg_d(f, !double),
                m: reg_m(f, double),
            },
            Self::Half { double } => {
                let to_half = f.bit(Field::Opc1);
                SimdFp::Half {
                    to_half,
                    double,
                    top: f.bit(Field::X),
                    d: reg_d(f, double && !to_half),
                    m: reg_m(f, double && to_half),
                }
            }
            Self::ToInteger(rounding) => SimdFp::ToInteger {
                rounding,
                unsigned: !f.bit(Field::Y),
                double,
                d: reg_d(f, false),
                m: reg_m(f, double),
            },
            Self::FromInteger => SimdFp::FromInteger {
                unsigned: !f.bit(Field::X),
                double,
                d: reg_d(f, double),
                m: reg_m(f, false),
            },
            Self::Fixed => {
                let word = f.bit(Field::X);
                let size: u8 = if word { 32 } else { 16 };
                let encoded = byte(f, Field::Imm) << 1 | byte(f, Field::Y);
                SimdFp::Fixed {
                    to_fixed: f.bit(Field::Opc1),
                    unsigned: f.bit(Field::U),
                    word,
                    double,
                    d: reg_d(f, double),
                    // A negative count of fraction bits is UNPREDICTABLE.
                    fraction: size.checked_sub(encoded)?,
                }
            }
            Self::MoveImmediate => SimdFp::MoveImmediate {
                double,
                d: reg_d(f, double),
                imm: byte(f, Field::Imm),
            },
            Self::CoreSingle => SimdFp::CoreSingle {
                to_core: f.bit(Field::L),
                n: reg_n(f, false),
                rt: Reg::field(f.get(Field::Rt)),
            },
            Self::CorePair => SimdFp::CorePair {
                to_core: f.bit(Field::L),
                double,
                m: reg_m(f, double),
                rt: Reg::field(f.get(Field::Rt)),
                rt2: Reg::field(f.get(Field::Rt2)),
            },
            Self::CoreScalar { to_core } => {
                let selector = byte(f, Field::Opc1) << 2 | byte(f, Field::Opc2);
                let unsigned = to_core && f.bit(Field::U);
                let (size, index) = match selector {
                    s if s & 0b1000 != 0 => (0, s & 7),
                    s if s & 1 != 0 => (1, s >> 1 & 3),
                    s if s.trailing_zeros() >= 2 && !unsigned => (2, s >> 2 & 1),
                    _ => return None,
                };
                SimdFp::CoreScalar {
                    to_core,
                    size,
                    unsigned,
                    index,
                    d: reg_n(f, true),
                    rt: Reg::field(f.get(Field::Rt)),
                }
            }
            Self::CoreDuplicate => {
                let d = reg_n(f, true);
                if quad && !even(&[d]) {
                    return None;
                }
                let size = match (f.bit(Field::B), f.bit(Field::Endian)) {
                    (false, false) => 2,
                    (false, true) => 1,
                    (true, false) => 0,
                    (true, true) => return None,
                };
                SimdFp::CoreDuplicate {
                    size,
                    q: quad,
                    d,
                    rt: Reg::field(f.get(Field::Rt)),
                }
            }
            Self::System => {
                let read = f.bit(Field::L);
                let register = System::of(byte(f, Field::CRn))?;
                if !read && !matches!(register, System::Fpsid | System::Fpscr | System::Fpexc) {
                    return None;
                }
                SimdFp::System {
                    read,
                    register,
                    rt: Reg::field(f.get(Field::Rt)),
                }
            }
            Self::Transfer => SimdFp::Transfer {
                load: f.bit(Field::L),
                double,
                d: reg_d(f, double),
                rn: Reg::field(f.get(Field::Rn)),
                up: f.bit(Field::U),
                offset: byte(f, Field::Imm),
            },
            Self::Multiple => {
                let (p, up, writeback) = (f.bit(Field::P), f.bit(Field::U), f.bit(Field::W));
                // P set with W clear is VLDR's; P and U alike are UNDEFINED
                // or the 64-bit transfers' space.
                if p == up || (p && !writeback) {
                    return None;
                }
                SimdFp::Multiple {
                    load: f.bit(Field::L),
                    double,
                    d: reg_d(f, double),
                    rn: Reg::field(f.get(Field::Rn)),
                    mode: BlockMode::of(p, up),
                    writeback,
                    words: byte(f, Field::Imm),
                }
            }
            Self::Three(op) => three(op, layout, f)?,
            Self::Long(op) => long(op, layout, f)?,
            Self::ByScalar(op) => by_scalar(op, layout, f)?,
            Self::Shift(op) => shift(op, layout, f)?,
            Self::Misc(op) => misc(op, layout, f)?,
            Self::Immediate => {
                let cmode = byte(f, Field::X) << 4 | byte(f, Field::Cmode);
                let d = reg_d(f, true);
                SimdImmediateOp::of(cmode)?;
                if quad && !even(&[d]) {
                    return None;
                }
                SimdFp::Immediate {
                    cmode,
                    q: quad,
                    d,
                    imm: byte(f, Field::Imm),
                }
            }
            Self::Extract => {
                let (d, n, m) = (reg_d(f, true), reg_n(f, true), reg_m(f, true));
                let start = byte(f, Field::Imm);
                if quad && !even(&[d, n, m]) || !quad && start >= 8 {
                    return None;
                }
                SimdFp::Extract {
                    q: quad,
                    d,
                    n,
                    m,
                    start,
                }
            }
            Self::Table => SimdFp::Table {
                extend: f.bit(Field::X),
                length: byte(f, Field::Count) + 1,
                d: reg_d(f, true),
                n: reg_n(f, true),
                m: reg_m(f, true),
            },
            Self::Duplicate => {
                let selector = byte(f, Field::Imm);
                let (size, index) = match selector {
                    s if s & 1 != 0 => (0, s >> 1),
                    s if s & 0b10 != 0 => (1, s >> 2),
                    s if s & 0b100 != 0 => (2, s >> 3),
                    _ => return None,
                };
                let (d, m) = (reg_d(f, true), reg_m(f, true));
                if quad && !even(&[d]) {
                    return None;
                }
                SimdFp::Duplicate {
                    size,
                    q: quad,
                    d,
                    m,
                    index,
                }
            }
            Self::StructureMultiple => structure(structure_multiple(f)?, f),
            Self::StructureLane => structure(structure_lane(f)?, f),
            // Only a load reaches every lane.
            Self::StructureAll if !f.bit(Field::L) => return None,
            Self::StructureAll => structure(structure_all(f)?, f),
        })
    }
}

fn structure(shape: Structure, f: &Fields) -> SimdFp {
    SimdFp::Structure {
        load: f.bit(Field::L),
        shape,
        d: reg_d(f, true),
        rn: Reg::field(f.get(Field::Rn)),
        rm: Reg::field(f.get(Field::Rm)),
    }
}

/// An alignment field of multiple structures: none, or 64 << `align` bits.
const fn alignment(align: u8) -> u8 {
    if align == 0 {
        1
    } else {
        4 << align
    }
}

/// `VLDn` and `VSTn` of multiple structures, by the type field.
fn structure_multiple(f: &Fields) -> Option<Structure> {
    let size = byte(f, Field::Precision);
    let align = byte(f, Field::Align);
    let (elements, parts, spacing) = match byte(f, Field::Opc1) {
        0b0111 if align & 2 == 0 => (1, 1, 1),
        0b1010 if align != 3 => (1, 2, 1),
        0b0110 if align & 2 == 0 => (1, 3, 1),
        0b0010 => (1, 4, 1),
        0b1000 if align != 3 && size != 3 => (2, 1, 1),
        0b1001 if align != 3 && size != 3 => (2, 1, 2),
        0b0011 if size != 3 => (2, 2, 2),
        0b0100 if align & 2 == 0 && size != 3 => (3, 1, 1),
        0b0101 if align & 2 == 0 && size != 3 => (3, 1, 2),
        0b0000 if size != 3 => (4, 1, 1),
        0b0001 if size != 3 => (4, 1, 2),
        _ => return None,
    };
    let align = if elements == 3 {
        if align & 1 == 0 {
            1
        } else {
            8
        }
    } else {
        alignment(align)
    };
    Some(Structure {
        lanes: StructureLanes::Multiple,
        elements,
        parts,
        spacing,
        size,
        align,
    })
}

/// `VLDn` and `VSTn` of one structure to one lane, from the size and the
/// index and alignment field.
fn structure_lane(f: &Fields) -> Option<Structure> {
    let size = byte(f, Field::Precision);
    let elements = byte(f, Field::Count) + 1;
    let field = byte(f, Field::Align);
    let low = field & 3;
    let (index, spacing, align) = match (elements, size) {
        (1 | 3, 0) if field & 1 == 0 => (field >> 1, 1, 1),
        (1, 1) if field & 2 == 0 => (field >> 2, 1, if field & 1 == 0 { 1 } else { 2 }),
        (1, 2) if field & 4 == 0 && (low == 0 || low == 3) => {
            (field >> 3, 1, if low == 0 { 1 } else { 4 })
        }
        (2, 0) => (field >> 1, 1, if field & 1 == 0 { 1 } else { 2 }),
        (2, 1) => (
            field >> 2,
            1 + (field >> 1 & 1),
            if field & 1 == 0 { 1 } else { 4 },
        ),
        (2, 2) if field & 2 == 0 => (
            field >> 3,
            1 + (field >> 2 & 1),
            if field & 1 == 0 { 1 } else { 8 },
        ),
        (3, 1) if field & 1 == 0 => (field >> 2, 1 + (field >> 1 & 1), 1),
        (3, 2) if low == 0 => (field >> 3, 1 + (field >> 2 & 1), 1),
        (4, 0) => (field >> 1, 1, if field & 1 == 0 { 1 } else { 4 }),
        (4, 1) => (
            field >> 2,
            1 + (field >> 1 & 1),
            if field & 1 == 0 { 1 } else { 8 },
        ),
        (4, 2) if low != 3 => (
            field >> 3,
            1 + (field >> 2 & 1),
            if low == 0 { 1 } else { 4 << low },
        ),
        _ => return None,
    };
    Some(Structure {
        lanes: StructureLanes::One(index),
        elements,
        parts: 1,
        spacing,
        size,
        align,
    })
}

/// `VLDn` of one structure to all lanes.
fn structure_all(f: &Fields) -> Option<Structure> {
    let size = byte(f, Field::Precision);
    let elements = byte(f, Field::Count) + 1;
    let t = f.bit(Field::X);
    let aligned = f.bit(Field::Align);
    let ebytes = 1u8 << (size & 3);
    let (size, align) = match (elements, size) {
        (4, 3) if aligned => (2, 16),
        (_, 3) | (1, 0) | (3, _) if aligned || size == 3 => return None,
        (1, _) => (size, if aligned { ebytes } else { 1 }),
        (2, _) => (size, if aligned { 2 * ebytes } else { 1 }),
        (4, 2) => (size, if aligned { 8 } else { 1 }),
        (4, _) => (size, if aligned { 4 * ebytes } else { 1 }),
        _ => (size, 1),
    };
    let (parts, spacing) = if elements == 1 {
        (1 + u8::from(t), 1)
    } else {
        (1, 1 + u8::from(t))
    };
    Some(Structure {
        lanes: StructureLanes::All,
        elements,
        parts,
        spacing,
        size,
        align,
    })
}

/// The size an operation whose encoding fixes it has: single-precision and
/// 32-bit lanes for the floating-point and SHA operations, bytes for the
/// bitwise ones.
const fn fixed_size(op: SimdThreeOp) -> Option<u8> {
    use SimdThreeOp as O;
    match op {
        O::Vand | O::Vbic | O::Vorr | O::Vorn | O::Veor | O::Vbsl | O::Vbit | O::Vbif => Some(0),
        O::Sha1c
        | O::Sha1p
        | O::Sha1m
        | O::Sha1su0
        | O::Sha256h
        | O::Sha256h2
        | O::Sha256su1
        | O::Vfma
        | O::Vfms
        | O::VaddF
        | O::VsubF
        | O::VpaddF
        | O::VabdF
        | O::VmlaF
        | O::VmlsF
        | O::VmulF
        | O::VceqF
        | O::VcgeF
        | O::VcgtF
        | O::Vacge
        | O::Vacgt
        | O::VmaxF
        | O::VminF
        | O::VpmaxF
        | O::VpminF
        | O::Vrecps
        | O::Vrsqrts
        | O::Vmaxnm
        | O::Vminnm => Some(2),
        _ => None,
    }
}

fn three(op: SimdThreeOp, layout: &Layout, f: &Fields) -> Option<SimdFp> {
    use SimdThreeOp as O;
    let quad = f.bit(Field::Q);
    let (d, n, m) = (reg_d(f, true), reg_n(f, true), reg_m(f, true));
    if quad && !even(&[d, n, m]) {
        return None;
    }
    let size = match fixed_size(op) {
        Some(size) => size,
        None => byte(f, Field::Precision),
    };
    let sizes_allowed = match op {
        O::Vqadd | O::Vqsub | O::Vshl | O::Vqshl | O::Vrshl | O::Vqrshl | O::Vadd | O::Vsub => true,
        O::Vqdmulh | O::Vqrdmulh => size == 1 || size == 2,
        O::Vmulp => size == 0,
        _ => fixed_size(op).is_some() || size != 3,
    };
    let pairwise = matches!(
        op,
        O::Vpmax | O::Vpmin | O::Vpadd | O::VpaddF | O::VpmaxF | O::VpminF
    );
    let sha = matches!(
        op,
        O::Sha1c | O::Sha1p | O::Sha1m | O::Sha1su0 | O::Sha256h | O::Sha256h2 | O::Sha256su1
    );
    if !sizes_allowed || (pairwise && quad) || (sha && !quad) {
        return None;
    }
    Some(SimdFp::Three {
        op,
        unsigned: layout.field(Field::U) != 0 && f.bit(Field::U),
        size,
        q: quad,
        d,
        n,
        m,
    })
}

fn long(op: SimdLongOp, layout: &Layout, f: &Fields) -> Option<SimdFp> {
    use SimdLongOp as O;
    let (d, n, m) = (reg_d(f, true), reg_n(f, true), reg_m(f, true));
    let size = match op {
        O::Vmullp => 0,
        O::Vmullp64 => 2,
        _ => byte(f, Field::Precision),
    };
    let registers_allowed = match op.width() {
        Width::Long => even(&[d]),
        Width::Wide => even(&[d, n]),
        Width::Narrow => even(&[n, m]),
    };
    let doubling = matches!(op, O::Vqdmlal | O::Vqdmlsl | O::Vqdmull);
    if size == 3 || !registers_allowed || (doubling && size == 0) {
        return None;
    }
    Some(SimdFp::Long {
        op,
        unsigned: layout.field(Field::U) != 0 && f.bit(Field::U),
        size,
        d,
        n,
        m,
    })
}

fn by_scalar(op: SimdScalarOp, layout: &Layout, f: &Fields) -> Option<SimdFp> {
    let size = byte(f, Field::Precision);
    let (d, n) = (reg_d(f, true), reg_n(f, true));
    let scalar = byte(f, Field::CRm);
    let (m, index) = if size == 1 {
        (scalar & 7, scalar >> 3)
    } else {
        (scalar & 15, scalar >> 4)
    };
    let quad = !op.long() && f.bit(Field::Q);
    let registers_allowed = if op.long() {
        even(&[d])
    } else {
        !quad || even(&[d, n])
    };
    if size == 0 || size == 3 || (op.float() && size != 2) || !registers_allowed {
        return None;
    }
    Some(SimdFp::ByScalar {
        op,
        unsigned: layout.field(Field::U) != 0 && f.bit(Field::U),
        size,
        q: quad,
        d,
        n,
        m,
        index,
    })
}

/// A shift's lane size from `L:imm6`, its leading one: none in the
/// modified-immediate space.
const fn shift_size(long: bool, imm6: u8) -> Option<u8> {
    if long {
        Some(3)
    } else if imm6 >> 5 != 0 {
        Some(2)
    } else if imm6 >> 4 != 0 {
        Some(1)
    } else if imm6 >> 3 != 0 {
        Some(0)
    } else {
        None
    }
}

fn shift(op: SimdShiftOp, layout: &Layout, f: &Fields) -> Option<SimdFp> {
    use SimdShiftOp as O;
    // VMOVL's row holds only imm6's top three bits: the size alone.
    let imm6 = if op == O::Vmovl {
        byte(f, Field::Imm) << 3
    } else {
        byte(f, Field::Imm)
    };
    let q = f.bit(Field::Q);
    let (d, m) = (reg_d(f, true), reg_m(f, true));
    let size = shift_size(f.bit(Field::L), imm6)?;
    let esize = 8u8 << size;
    let amount = match op {
        O::VcvtFromFixed | O::VcvtToFixed if size != 2 => return None,
        O::VcvtFromFixed | O::VcvtToFixed => 64 - imm6,
        O::Vmovl if imm6 != esize => return None,
        O::Vmovl => 0,
        _ if op.right() => (if size == 3 { 64 } else { 2 * esize }) - imm6,
        _ if size == 3 => imm6,
        _ => imm6 - esize,
    };
    let registers_allowed = if op.narrow() {
        even(&[m])
    } else if op.long() {
        even(&[d])
    } else {
        !q || even(&[d, m])
    };
    if !registers_allowed || (op == O::Vshll && amount == 0) {
        return None;
    }
    Some(SimdFp::Shift {
        op,
        unsigned: layout.field(Field::U) != 0 && f.bit(Field::U),
        size,
        q: q && !op.narrow() && !op.long(),
        d,
        m,
        amount,
    })
}

fn misc(op: SimdMiscOp, layout: &Layout, f: &Fields) -> Option<SimdFp> {
    use SimdMiscOp as O;
    let size = byte(f, Field::Precision);
    let quadword = layout.field(Field::Q) != 0;
    let (d, m) = (reg_d(f, true), reg_m(f, true));
    let crypto = matches!(
        op,
        O::Aese | O::Aesd | O::Aesmc | O::Aesimc | O::Sha1h | O::Sha1su1 | O::Sha256su0
    );
    let q = if quadword { f.bit(Field::Q) } else { crypto };
    let sizes_allowed = match op {
        O::Vrev64 => size <= 2,
        O::Vrev32 => size <= 1,
        O::Vrev16 | O::Vcnt | O::Vmvn | O::Vswp | O::Aese | O::Aesd | O::Aesmc | O::Aesimc => {
            size == 0
        }
        O::Vuzp | O::Vzip => size <= 1 || (size == 2 && q),
        O::VcvtToHalf | O::VcvtFromHalf => size == 1,
        O::Vcgt0F
        | O::Vcge0F
        | O::Vceq0F
        | O::Vcle0F
        | O::Vclt0F
        | O::VabsF
        | O::VnegF
        | O::Sha1h
        | O::Sha1su1
        | O::Sha256su0
        | O::Vrintn
        | O::Vrintx
        | O::Vrinta
        | O::Vrintz
        | O::Vrintm
        | O::Vrintp
        | O::Vcvta
        | O::Vcvtn
        | O::Vcvtp
        | O::Vcvtm
        | O::Vrecpe
        | O::VrecpeF
        | O::Vrsqrte
        | O::VrsqrteF
        | O::VcvtToFloat
        | O::VcvtToInteger => size == 2,
        _ => size != 3,
    };
    let registers_allowed = match op {
        O::Vmovn | O::Vqmovun | O::Vqmovn | O::VcvtToHalf => even(&[m]),
        O::Vshll | O::VcvtFromHalf => even(&[d]),
        _ => !q || even(&[d, m]),
    };
    if !sizes_allowed || !registers_allowed {
        return None;
    }
    Some(SimdFp::Misc {
        op,
        unsigned: layout.field(Field::U) != 0 && f.bit(Field::U),
        size,
        q,
        d,
        m,
    })
}

impl SimdFp {
    /// The form without its operands.
    pub(crate) const fn kind(&self) -> SimdFpKind {
        match *self {
            Self::Dyadic { op, .. } => SimdFpKind::Dyadic(op),
            Self::Select { .. } => SimdFpKind::Select,
            Self::Monadic { op, .. } => SimdFpKind::Monadic(op),
            Self::Compare { m: Some(_), .. } => SimdFpKind::Compare,
            Self::Compare { m: None, .. } => SimdFpKind::CompareZero,
            Self::Precision { .. } => SimdFpKind::Precision,
            Self::Half { double, .. } => SimdFpKind::Half { double },
            Self::ToInteger { rounding, .. } => SimdFpKind::ToInteger(rounding),
            Self::FromInteger { .. } => SimdFpKind::FromInteger,
            Self::Fixed { .. } => SimdFpKind::Fixed,
            Self::MoveImmediate { .. } => SimdFpKind::MoveImmediate,
            Self::CoreSingle { .. } => SimdFpKind::CoreSingle,
            Self::CorePair { .. } => SimdFpKind::CorePair,
            Self::CoreScalar { to_core, .. } => SimdFpKind::CoreScalar { to_core },
            Self::CoreDuplicate { .. } => SimdFpKind::CoreDuplicate,
            Self::System { .. } => SimdFpKind::System,
            Self::Transfer { .. } => SimdFpKind::Transfer,
            Self::Multiple { .. } => SimdFpKind::Multiple,
            Self::Three { op, .. } => SimdFpKind::Three(op),
            Self::Long { op, .. } => SimdFpKind::Long(op),
            Self::ByScalar { op, .. } => SimdFpKind::ByScalar(op),
            Self::Shift { op, .. } => SimdFpKind::Shift(op),
            Self::Misc { op, .. } => SimdFpKind::Misc(op),
            Self::Immediate { .. } => SimdFpKind::Immediate,
            Self::Extract { .. } => SimdFpKind::Extract,
            Self::Table { .. } => SimdFpKind::Table,
            Self::Duplicate { .. } => SimdFpKind::Duplicate,
            Self::Structure { shape, .. } => match shape.lanes {
                StructureLanes::Multiple => SimdFpKind::StructureMultiple,
                StructureLanes::One(_) => SimdFpKind::StructureLane,
                StructureLanes::All => SimdFpKind::StructureAll,
            },
        }
    }

    /// Every field the operands set. A row writes those its layout has; the
    /// encoder decodes the result to refuse operands a row cannot hold.
    #[allow(clippy::too_many_lines)] // The inverse of `SimdFpKind::build`, arm for arm.
    pub(crate) fn fields(&self, f: &mut Fields) {
        match *self {
            Self::Dyadic {
                double, d, n, m, ..
            } => {
                f.flag(Field::Precision, double);
                set_d(f, d, double);
                set_n(f, n, double);
                set_m(f, m, double);
            }
            Self::Select {
                cond,
                double,
                d,
                n,
                m,
            } => {
                f.set(Field::Opc1, selected_bits(cond));
                f.flag(Field::Precision, double);
                set_d(f, d, double);
                set_n(f, n, double);
                set_m(f, m, double);
            }
            Self::Monadic { double, d, m, .. } => {
                f.flag(Field::Precision, double);
                set_d(f, d, double);
                set_m(f, m, double);
            }
            Self::Compare {
                signalling,
                double,
                d,
                m,
            } => {
                f.flag(Field::X, signalling);
                f.flag(Field::Precision, double);
                set_d(f, d, double);
                if let Some(m) = m {
                    set_m(f, m, double);
                }
            }
            Self::Precision { to_double, d, m } => {
                f.flag(Field::Precision, !to_double);
                set_d(f, d, to_double);
                set_m(f, m, !to_double);
            }
            Self::Half {
                to_half,
                double,
                top,
                d,
                m,
            } => {
                f.flag(Field::Opc1, to_half);
                f.flag(Field::X, top);
                set_d(f, d, double && !to_half);
                set_m(f, m, double && to_half);
            }
            Self::ToInteger {
                unsigned,
                double,
                d,
                m,
                ..
            } => {
                f.flag(Field::Y, !unsigned);
                f.flag(Field::Precision, double);
                set_d(f, d, false);
                set_m(f, m, double);
            }
            Self::FromInteger {
                unsigned,
                double,
                d,
                m,
            } => {
                f.flag(Field::X, !unsigned);
                f.flag(Field::Precision, double);
                set_d(f, d, double);
                set_m(f, m, false);
            }
            Self::Fixed {
                to_fixed,
                unsigned,
                word,
                double,
                d,
                fraction,
            } => {
                let size: u8 = if word { 32 } else { 16 };
                let encoded = size.checked_sub(fraction).map_or(OUT_OF_RANGE, u32::from);
                f.flag(Field::Opc1, to_fixed);
                f.flag(Field::U, unsigned);
                f.flag(Field::X, word);
                f.flag(Field::Precision, double);
                set_d(f, d, double);
                f.set(Field::Imm, encoded >> 1);
                f.set(Field::Y, encoded & 1);
            }
            Self::MoveImmediate { double, d, imm } => {
                f.flag(Field::Precision, double);
                set_d(f, d, double);
                f.set(Field::Imm, u32::from(imm));
            }
            Self::CoreSingle { to_core, n, rt } => {
                f.flag(Field::L, to_core);
                set_n(f, n, false);
                f.set(Field::Rt, rt.number());
            }
            Self::CorePair {
                to_core,
                double,
                m,
                rt,
                rt2,
            } => {
                f.flag(Field::L, to_core);
                f.flag(Field::Precision, double);
                set_m(f, m, double);
                f.set(Field::Rt, rt.number());
                f.set(Field::Rt2, rt2.number());
            }
            Self::CoreScalar {
                size,
                unsigned,
                index,
                d,
                rt,
                ..
            } => {
                let selector = match size {
                    0 => 0b1000 | u32::from(index),
                    1 => 0b0001 | u32::from(index) << 1,
                    2 => u32::from(index) << 2,
                    _ => OUT_OF_RANGE,
                };
                f.set(Field::Opc1, selector >> 2);
                f.set(Field::Opc2, selector & 3);
                f.flag(Field::U, unsigned);
                set_n(f, d, true);
                f.set(Field::Rt, rt.number());
            }
            Self::CoreDuplicate { size, q, d, rt } => {
                f.flag(Field::B, size == 0);
                f.flag(Field::Endian, size == 1);
                if size > 2 {
                    f.set(Field::B, OUT_OF_RANGE);
                }
                f.flag(Field::Q, q);
                set_n(f, d, true);
                f.set(Field::Rt, rt.number());
            }
            Self::System { read, register, rt } => {
                f.flag(Field::L, read);
                f.set(Field::CRn, u32::from(register.number()));
                f.set(Field::Rt, rt.number());
            }
            Self::Transfer {
                load,
                double,
                d,
                rn,
                up,
                offset,
            } => {
                f.flag(Field::L, load);
                f.flag(Field::Precision, double);
                set_d(f, d, double);
                f.set(Field::Rn, rn.number());
                f.flag(Field::U, up);
                f.set(Field::Imm, u32::from(offset));
            }
            Self::Multiple {
                load,
                double,
                d,
                rn,
                mode,
                writeback,
                words,
            } => {
                f.flag(Field::L, load);
                f.flag(Field::Precision, double);
                set_d(f, d, double);
                f.set(Field::Rn, rn.number());
                f.flag(Field::P, mode.before());
                f.flag(Field::U, mode.up());
                f.flag(Field::W, writeback);
                f.set(Field::Imm, u32::from(words));
            }
            Self::Three {
                unsigned,
                size,
                q,
                d,
                n,
                m,
                ..
            } => {
                set_lanes(f, unsigned, size, q);
                set_d(f, d, true);
                set_n(f, n, true);
                set_m(f, m, true);
            }
            Self::Long {
                unsigned,
                size,
                d,
                n,
                m,
                ..
            } => {
                set_lanes(f, unsigned, size, false);
                set_d(f, d, true);
                set_n(f, n, true);
                set_m(f, m, true);
            }
            Self::ByScalar {
                unsigned,
                size,
                q,
                d,
                n,
                m,
                index,
                ..
            } => {
                set_lanes(f, unsigned, size, q);
                set_d(f, d, true);
                set_n(f, n, true);
                let scalar = if size == 1 {
                    u32::from(index) << 3 | u32::from(m & 0x0F)
                } else {
                    u32::from(index) << 4 | u32::from(m)
                };
                f.set(Field::CRm, scalar);
            }
            Self::Shift {
                op,
                unsigned,
                size,
                q,
                d,
                m,
                amount,
            } => {
                f.flag(Field::U, unsigned);
                f.flag(Field::Q, q);
                set_d(f, d, true);
                set_m(f, m, true);
                let (long, imm6) = shift_fields(op, size, amount);
                f.flag(Field::L, long);
                f.set(Field::Imm, imm6);
            }
            Self::Misc {
                unsigned,
                size,
                q,
                d,
                m,
                ..
            } => {
                set_lanes(f, unsigned, size, q);
                set_d(f, d, true);
                set_m(f, m, true);
            }
            Self::Immediate { cmode, q, d, imm } => {
                f.set(Field::X, u32::from(cmode >> 4));
                f.set(Field::Cmode, u32::from(cmode & 0xF));
                f.flag(Field::Q, q);
                set_d(f, d, true);
                f.set(Field::Imm, u32::from(imm));
            }
            Self::Extract { q, d, n, m, start } => {
                f.flag(Field::Q, q);
                set_d(f, d, true);
                set_n(f, n, true);
                set_m(f, m, true);
                f.set(Field::Imm, u32::from(start));
            }
            Self::Table {
                extend,
                length,
                d,
                n,
                m,
            } => {
                f.flag(Field::X, extend);
                f.set(
                    Field::Count,
                    length.checked_sub(1).map_or(OUT_OF_RANGE, u32::from),
                );
                set_d(f, d, true);
                set_n(f, n, true);
                set_m(f, m, true);
            }
            Self::Duplicate {
                size,
                q,
                d,
                m,
                index,
            } => {
                let selector = match size {
                    0 => u32::from(index) << 1 | 1,
                    1 => u32::from(index) << 2 | 0b10,
                    2 => u32::from(index) << 3 | 0b100,
                    _ => OUT_OF_RANGE,
                };
                f.set(Field::Imm, selector);
                f.flag(Field::Q, q);
                set_d(f, d, true);
                set_m(f, m, true);
            }
            Self::Structure {
                load,
                shape,
                d,
                rn,
                rm,
            } => {
                f.flag(Field::L, load);
                set_d(f, d, true);
                f.set(Field::Rn, rn.number());
                f.set(Field::Rm, rm.number());
                set_structure(f, shape);
            }
        }
    }
}

fn set_lanes(f: &mut Fields, unsigned: bool, size: u8, q: bool) {
    f.flag(Field::U, unsigned);
    f.set(Field::Precision, u32::from(size));
    f.flag(Field::Q, q);
}

/// `L` and `imm6` for a shift of `amount` on lanes of 8 << `size` bits.
fn shift_fields(op: SimdShiftOp, size: u8, amount: u8) -> (bool, u32) {
    use SimdShiftOp as O;
    let esize = 8u32 << size;
    let amount = u32::from(amount);
    let long = size == 3;
    let imm6 = match op {
        O::VcvtFromFixed | O::VcvtToFixed => 64u32.checked_sub(amount),
        O::Vmovl => Some(esize >> 3),
        _ if op.right() => (if long { 64 } else { 2 * esize }).checked_sub(amount),
        _ if long => Some(amount),
        _ => Some(esize + amount),
    };
    (long, imm6.unwrap_or(OUT_OF_RANGE))
}

fn set_structure(f: &mut Fields, shape: Structure) {
    f.set(Field::Precision, u32::from(shape.size));
    f.set(Field::Count, u32::from(shape.elements.wrapping_sub(1)));
    match shape.lanes {
        StructureLanes::Multiple => {
            let kind = match (shape.elements, shape.parts, shape.spacing) {
                (1, 1, _) => 0b0111,
                (1, 2, _) => 0b1010,
                (1, 3, _) => 0b0110,
                (1, 4, _) => 0b0010,
                (2, 1, 1) => 0b1000,
                (2, 1, 2) => 0b1001,
                (2, 2, _) => 0b0011,
                (3, _, 1) => 0b0100,
                (3, _, 2) => 0b0101,
                (4, _, 1) => 0b0000,
                (4, _, 2) => 0b0001,
                _ => OUT_OF_RANGE,
            };
            f.set(Field::Opc1, kind);
            let align = match (shape.elements, shape.align) {
                (_, 1) => 0,
                (3, 8) => 1,
                (3, _) => OUT_OF_RANGE,
                (_, 8) => 1,
                (_, 16) => 2,
                (_, 32) => 3,
                _ => OUT_OF_RANGE,
            };
            f.set(Field::Align, align);
        }
        StructureLanes::One(index) => {
            let index = u32::from(index);
            let spaced = u32::from(shape.spacing == 2);
            let aligned = u32::from(shape.align > 1);
            let field = match (shape.elements, shape.size) {
                (1 | 3, 0) => index << 1,
                (2 | 4, 0) => index << 1 | aligned,
                (1, 1) => index << 2 | aligned,
                (2 | 4, 1) => index << 2 | spaced << 1 | aligned,
                (3, 1) => index << 2 | spaced << 1,
                (1, 2) => index << 3 | (aligned * 3),
                (2, 2) => index << 3 | spaced << 2 | aligned,
                (3, 2) => index << 3 | spaced << 2,
                (4, 2) => {
                    let low = match shape.align {
                        1 => 0,
                        8 => 1,
                        16 => 2,
                        _ => OUT_OF_RANGE,
                    };
                    index << 3 | spaced << 2 | low
                }
                _ => OUT_OF_RANGE,
            };
            f.set(Field::Align, field);
        }
        StructureLanes::All => {
            let (size, aligned) = match (shape.elements, shape.size, shape.align) {
                (4, 2, 16) => (3, true),
                (_, size, align) => (u32::from(size), align > 1),
            };
            f.set(Field::Precision, size);
            f.flag(Field::Align, aligned);
            let t = if shape.elements == 1 {
                shape.parts == 2
            } else {
                shape.spacing == 2
            };
            f.flag(Field::X, t);
        }
    }
}
