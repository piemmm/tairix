//! Each machine instruction as the A64 decoder renders it, written from
//! what the instruction means rather than from its bits, so the encoding can
//! be checked by an independent decoder.

use alloc::format;
use alloc::string::String;

use super::inst::{reg, Cond, Dp2, Inst, Logic, Offset, Size};

/// Register `r` of a `wide` operation, register 31 the stack pointer where
/// `sp`, else the zero register.
fn r(wide: bool, n: u8, sp: bool) -> String {
    match (n == reg::SP, sp, wide) {
        (true, true, true) => "sp".into(),
        (true, true, false) => "wsp".into(),
        (true, false, true) => "xzr".into(),
        (true, false, false) => "wzr".into(),
        (false, _, true) => format!("x{n}"),
        (false, _, false) => format!("w{n}"),
    }
}

const fn cond(c: Cond) -> &'static str {
    match c {
        Cond::Eq => "eq",
        Cond::Ne => "ne",
        Cond::Hs => "cs",
        Cond::Lo => "cc",
        Cond::Hi => "hi",
        Cond::Ls => "ls",
        Cond::Ge => "ge",
        Cond::Lt => "lt",
        Cond::Gt => "gt",
        Cond::Le => "le",
    }
}

const fn logic(op: Logic) -> &'static str {
    match op {
        Logic::And => "and",
        Logic::Orr => "orr",
        Logic::Eor => "eor",
        Logic::Orn => "orn",
    }
}

/// A logical immediate's value: its element, a rotated run of ones,
/// repeated (Arm ARM, `DecodeBitMasks`).
#[allow(clippy::similar_names)] // `immr` and `imms` are the architecture's own field names.
fn mask_value(wide: bool, n: bool, immr: u8, imms: u8) -> u64 {
    let combined = u32::from(n) << 6 | (!u32::from(imms) & 0x3F);
    let size = 1u32 << combined.ilog2();
    let ones = u32::from(imms) & (size - 1);
    let run = (1u64 << (ones + 1)) - 1;
    let mask = if size == 64 {
        u64::MAX
    } else {
        (1u64 << size) - 1
    };
    let by = u32::from(immr) & (size - 1);
    let element = if by == 0 {
        run
    } else {
        ((run >> by) | (run << (size - by))) & mask
    };
    let mut value = 0u64;
    let mut at = 0;
    while at < 64 {
        value |= element << at;
        at += size;
    }
    if wide {
        value
    } else {
        value & 0xFFFF_FFFF
    }
}

/// A load's name: of `size`, extending its sign where `extends`.
const fn load_name(size: Size, extends: bool, unscaled: bool) -> &'static str {
    match (size, extends, unscaled) {
        (Size::B, false, false) => "ldrb",
        (Size::B, true, false) => "ldrsb",
        (Size::H, false, false) => "ldrh",
        (Size::H, true, false) => "ldrsh",
        (Size::W, true, false) => "ldrsw",
        (Size::W | Size::X, _, false) => "ldr",
        (Size::B, false, true) => "ldurb",
        (Size::B, true, true) => "ldursb",
        (Size::H, false, true) => "ldurh",
        (Size::H, true, true) => "ldursh",
        (Size::W, true, true) => "ldursw",
        (Size::W | Size::X, _, true) => "ldur",
    }
}

const fn store_name(size: Size, unscaled: bool) -> &'static str {
    match (size, unscaled) {
        (Size::B, false) => "strb",
        (Size::H, false) => "strh",
        (Size::W | Size::X, false) => "str",
        (Size::B, true) => "sturb",
        (Size::H, true) => "sturh",
        (Size::W | Size::X, true) => "stur",
    }
}

fn address(rn: u8, size: Size, offset: Offset) -> String {
    let base = r(true, rn, true);
    match offset {
        Offset::Scaled(0) | Offset::Unscaled(0) => format!("[{base}]"),
        Offset::Scaled(imm) => format!("[{base},#{}]", u64::from(imm) << size.log2()),
        Offset::Unscaled(imm) => format!("[{base},#{imm}]"),
        Offset::Index(rm) => format!("[{base},{}]", r(true, rm, false)),
    }
}

/// `inst`'s mnemonic and operands; a jump's target is left to the caller,
/// which appends it.
#[allow(clippy::too_many_lines)] // One exhaustive rendering match; splitting it would scatter the table.
pub(crate) fn text(inst: &Inst) -> (String, String) {
    match *inst {
        Inst::Movz { wide, rd, imm, hw }
        | Inst::Movk { wide, rd, imm, hw }
        | Inst::Movn { wide, rd, imm, hw } => {
            let name = match inst {
                Inst::Movz { .. } => "movz",
                Inst::Movk { .. } => "movk",
                _ => "movn",
            };
            let shift = if hw == 0 {
                String::new()
            } else {
                format!(",lsl #{}", u32::from(hw) * 16)
            };
            (
                name.into(),
                format!("{},#{imm:#x}{shift}", r(wide, rd, false)),
            )
        }
        Inst::AddImm {
            sub,
            flags,
            wide,
            rd,
            rn,
            imm,
            high,
        } => {
            let name = match (sub, flags) {
                (false, false) => "add",
                (false, true) => "adds",
                (true, false) => "sub",
                (true, true) => "subs",
            };
            let shift = if high { ",lsl #12" } else { "" };
            (
                name.into(),
                format!(
                    "{},{},#{imm:#x}{shift}",
                    r(wide, rd, !flags),
                    r(wide, rn, true)
                ),
            )
        }
        Inst::AddReg {
            sub,
            flags,
            wide,
            rd,
            rn,
            rm,
        } => {
            let name = match (sub, flags) {
                (false, false) => "add",
                (false, true) => "adds",
                (true, false) => "sub",
                (true, true) => "subs",
            };
            (
                name.into(),
                format!(
                    "{},{},{}",
                    r(wide, rd, false),
                    r(wide, rn, false),
                    r(wide, rm, false)
                ),
            )
        }
        Inst::AddSp { sub, rd, rn, rm } => (
            (if sub { "sub" } else { "add" }).into(),
            format!(
                "{},{},{},uxtx",
                r(true, rd, true),
                r(true, rn, true),
                r(true, rm, false)
            ),
        ),
        Inst::Logic {
            op,
            wide,
            rd,
            rn,
            rm,
        } => (
            logic(op).into(),
            format!(
                "{},{},{}",
                r(wide, rd, false),
                r(wide, rn, false),
                r(wide, rm, false)
            ),
        ),
        Inst::LogicImm {
            op,
            wide,
            rd,
            rn,
            mask,
        } => (
            logic(op).into(),
            format!(
                "{},{},#{:#x}",
                r(wide, rd, true),
                r(wide, rn, false),
                mask_value(wide, mask.n, mask.immr, mask.imms)
            ),
        ),
        Inst::Dp2 {
            op,
            wide,
            rd,
            rn,
            rm,
        } => {
            let name = match op {
                Dp2::Udiv => "udiv",
                Dp2::Sdiv => "sdiv",
                Dp2::Lslv => "lslv",
                Dp2::Lsrv => "lsrv",
                Dp2::Asrv => "asrv",
                Dp2::Rorv => "rorv",
            };
            (
                name.into(),
                format!(
                    "{},{},{}",
                    r(wide, rd, false),
                    r(wide, rn, false),
                    r(wide, rm, false)
                ),
            )
        }
        Inst::Madd {
            sub,
            wide,
            rd,
            rn,
            rm,
            ra,
        } => (
            (if sub { "msub" } else { "madd" }).into(),
            format!(
                "{},{},{},{}",
                r(wide, rd, false),
                r(wide, rn, false),
                r(wide, rm, false),
                r(wide, ra, false)
            ),
        ),
        Inst::Bfm {
            signed,
            wide,
            rd,
            rn,
            immr,
            imms,
        } => (
            (if signed { "sbfm" } else { "ubfm" }).into(),
            format!(
                "{},{},#{immr},#{imms}",
                r(wide, rd, false),
                r(wide, rn, false)
            ),
        ),
        Inst::Extr {
            wide,
            rd,
            rn,
            rm,
            lsb,
        } => (
            "extr".into(),
            format!(
                "{},{},{},#{lsb}",
                r(wide, rd, false),
                r(wide, rn, false),
                r(wide, rm, false)
            ),
        ),
        Inst::Csel {
            inc,
            wide,
            rd,
            rn,
            rm,
            cond: c,
        } => (
            (if inc { "csinc" } else { "csel" }).into(),
            format!(
                "{},{},{},{}",
                r(wide, rd, false),
                r(wide, rn, false),
                r(wide, rm, false),
                cond(c)
            ),
        ),
        Inst::Load {
            size,
            signed,
            wide,
            rt,
            rn,
            offset,
        } => {
            let extends = signed && size != Size::X && (wide || size != Size::W);
            let name = load_name(size, extends, matches!(offset, Offset::Unscaled(_)));
            let dest_wide = size == Size::X || (extends && wide);
            (
                name.into(),
                format!("{},{}", r(dest_wide, rt, false), address(rn, size, offset)),
            )
        }
        Inst::Store {
            size,
            rt,
            rn,
            offset,
        } => {
            let name = store_name(size, matches!(offset, Offset::Unscaled(_)));
            (
                name.into(),
                format!(
                    "{},{}",
                    r(size == Size::X, rt, false),
                    address(rn, size, offset)
                ),
            )
        }
        Inst::PushPair { rt, rt2 } => (
            "stp".into(),
            format!("{},{},[sp,#-16]!", r(true, rt, false), r(true, rt2, false)),
        ),
        Inst::PopPair { rt, rt2 } => (
            "ldp".into(),
            format!("{},{},[sp],#16", r(true, rt, false), r(true, rt2, false)),
        ),
        Inst::Blr(rn) => ("blr".into(), r(true, rn, false)),
        Inst::Ret => ("ret".into(), "x30".into()),
        Inst::B(_) => ("b".into(), String::new()),
        Inst::BCond(c, _) => (format!("b.{}", cond(c)), String::new()),
        Inst::Cbz { wide, rt, .. } => ("cbz".into(), r(wide, rt, false)),
        Inst::Bind(_) => (String::new(), String::new()),
    }
}
