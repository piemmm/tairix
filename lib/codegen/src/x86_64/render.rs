//! Each machine instruction as binutils renders it in Intel syntax, written
//! from what the instruction means rather than from its bytes, so the
//! encoding can be checked by an independent decoder.

use alloc::format;
use alloc::string::String;

use super::inst::{reg, Alu, Bytes, Cc, Count, Inst, Shift, ShiftX, Src};

const R64: [&str; 16] = [
    "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12", "r13",
    "r14", "r15",
];
const R32: [&str; 16] = [
    "eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi", "r8d", "r9d", "r10d", "r11d", "r12d",
    "r13d", "r14d", "r15d",
];
const R16: [&str; 16] = [
    "ax", "cx", "dx", "bx", "sp", "bp", "si", "di", "r8w", "r9w", "r10w", "r11w", "r12w", "r13w",
    "r14w", "r15w",
];
const R8: [&str; 16] = [
    "al", "cl", "dl", "bl", "spl", "bpl", "sil", "dil", "r8b", "r9b", "r10b", "r11b", "r12b",
    "r13b", "r14b", "r15b",
];

fn r(width: Bytes, n: u8) -> &'static str {
    let n = usize::from(n);
    match width {
        Bytes::B1 => R8[n],
        Bytes::B2 => R16[n],
        Bytes::B4 => R32[n],
        Bytes::B8 => R64[n],
    }
}

const fn word(wide: bool) -> Bytes {
    if wide {
        Bytes::B8
    } else {
        Bytes::B4
    }
}

const fn ptr(width: Bytes) -> &'static str {
    match width {
        Bytes::B1 => "BYTE PTR ",
        Bytes::B2 => "WORD PTR ",
        Bytes::B4 => "DWORD PTR ",
        Bytes::B8 => "QWORD PTR ",
    }
}

fn mem(width: Bytes, base: u8, disp: i32) -> String {
    let shown = if disp < 0 {
        format!("-{:#x}", disp.unsigned_abs())
    } else if disp > 0 || base & 7 == reg::RBP {
        format!("+{disp:#x}")
    } else {
        String::new()
    };
    format!("{}[{}{shown}]", ptr(width), R64[usize::from(base)])
}

fn imm(value: i64, width: Bytes) -> String {
    let raw = value.cast_unsigned();
    match width {
        Bytes::B4 => format!("{:#x}", raw & 0xFFFF_FFFF),
        _ => format!("{raw:#x}"),
    }
}

const fn cc(cc: Cc) -> &'static str {
    match cc {
        Cc::B => "b",
        Cc::Ae => "ae",
        Cc::E => "e",
        Cc::Ne => "ne",
        Cc::Be => "be",
        Cc::A => "a",
        Cc::L => "l",
        Cc::Ge => "ge",
        Cc::Le => "le",
        Cc::G => "g",
    }
}

/// `inst`'s mnemonic and operands; a jump's operand is its label, which the
/// caller resolves.
#[allow(clippy::too_many_lines)] // One exhaustive rendering match; splitting it would scatter the table.
pub(crate) fn text(inst: &Inst) -> (String, String) {
    let (name, operands): (String, String) = match *inst {
        Inst::Mov { wide, dst, src } => (
            "mov".into(),
            format!("{},{}", r(word(wide), dst), r(word(wide), src)),
        ),
        Inst::MovImm {
            wide,
            dst,
            imm: value,
        } => {
            if !wide || u32::try_from(value).is_ok() {
                (
                    "mov".into(),
                    format!("{},{:#x}", R32[usize::from(dst)], value & 0xFFFF_FFFF),
                )
            } else if i32::try_from(value.cast_signed()).is_ok() {
                (
                    "mov".into(),
                    format!("{},{value:#x}", R64[usize::from(dst)]),
                )
            } else {
                (
                    "movabs".into(),
                    format!("{},{value:#x}", R64[usize::from(dst)]),
                )
            }
        }
        Inst::Alu { op, wide, dst, src } => {
            let name = match op {
                Alu::Add => "add",
                Alu::Or => "or",
                Alu::And => "and",
                Alu::Sub => "sub",
                Alu::Xor => "xor",
                Alu::Cmp => "cmp",
            };
            let src = match src {
                Src::Reg(s) => String::from(r(word(wide), s)),
                Src::Imm(i) => imm(i64::from(i), word(wide)),
            };
            (name.into(), format!("{},{src}", r(word(wide), dst)))
        }
        Inst::Imul { wide, dst, src } => (
            "imul".into(),
            format!("{},{}", r(word(wide), dst), r(word(wide), src)),
        ),
        Inst::Shift {
            op,
            wide,
            dst,
            count,
        } => {
            let name = match op {
                Shift::Rol => "rol",
                Shift::Ror => "ror",
                Shift::Shl => "shl",
                Shift::Shr => "shr",
                Shift::Sar => "sar",
            };
            let count = match count {
                Count::Cl => String::from("cl"),
                Count::Imm(n) => format!("{n:#x}"),
            };
            (name.into(), format!("{},{count}", r(word(wide), dst)))
        }
        Inst::ShiftX {
            op,
            wide,
            dst,
            src,
            count,
        } => {
            let name = match op {
                ShiftX::Shl => "shlx",
                ShiftX::Shr => "shrx",
                ShiftX::Sar => "sarx",
            };
            let w = word(wide);
            (
                name.into(),
                format!("{},{},{}", r(w, dst), r(w, src), r(w, count)),
            )
        }
        Inst::Rorx {
            wide,
            dst,
            src,
            imm,
        } => (
            "rorx".into(),
            format!("{},{},{imm:#x}", r(word(wide), dst), r(word(wide), src)),
        ),
        Inst::Neg { wide, dst } => ("neg".into(), r(word(wide), dst).into()),
        Inst::Not { wide, dst } => ("not".into(), r(word(wide), dst).into()),
        Inst::Test { wide, a, b } => (
            "test".into(),
            format!("{},{}", r(word(wide), a), r(word(wide), b)),
        ),
        Inst::Movzx { from, dst, src } => (
            "movzx".into(),
            format!("{},{}", R32[usize::from(dst)], r(from, src)),
        ),
        Inst::Movsx {
            from: Bytes::B4 | Bytes::B8,
            dst,
            src,
            ..
        } => (
            "movsxd".into(),
            format!("{},{}", R64[usize::from(dst)], R32[usize::from(src)]),
        ),
        Inst::Movsx {
            from,
            wide,
            dst,
            src,
        } => (
            "movsx".into(),
            format!("{},{}", r(word(wide), dst), r(from, src)),
        ),
        Inst::Setcc { cc: c, dst } => (format!("set{}", cc(c)), R8[usize::from(dst)].into()),
        Inst::Cmov {
            cc: c,
            wide,
            dst,
            src,
        } => (
            format!("cmov{}", cc(c)),
            format!("{},{}", r(word(wide), dst), r(word(wide), src)),
        ),
        Inst::Div { signed, wide, src } => (
            (if signed { "idiv" } else { "div" }).into(),
            r(word(wide), src).into(),
        ),
        Inst::SignExtend { wide } => ((if wide { "cqo" } else { "cdq" }).into(), String::new()),
        Inst::Load {
            bytes,
            signed,
            wide,
            dst,
            base,
            disp,
        } => match (bytes, signed) {
            (Bytes::B1 | Bytes::B2, false) => (
                "movzx".into(),
                format!("{},{}", R32[usize::from(dst)], mem(bytes, base, disp)),
            ),
            (Bytes::B1 | Bytes::B2, true) => (
                "movsx".into(),
                format!("{},{}", r(word(wide), dst), mem(bytes, base, disp)),
            ),
            (Bytes::B4, true) if wide => (
                "movsxd".into(),
                format!("{},{}", R64[usize::from(dst)], mem(bytes, base, disp)),
            ),
            (Bytes::B4, _) => (
                "mov".into(),
                format!("{},{}", R32[usize::from(dst)], mem(bytes, base, disp)),
            ),
            (Bytes::B8, _) => (
                "mov".into(),
                format!("{},{}", R64[usize::from(dst)], mem(bytes, base, disp)),
            ),
        },
        Inst::Store {
            bytes,
            src,
            base,
            disp,
        } => (
            "mov".into(),
            format!("{},{}", mem(bytes, base, disp), r(bytes, src)),
        ),
        Inst::StoreImm {
            bytes,
            imm,
            base,
            disp,
        } => {
            let raw = i64::from(imm).cast_unsigned();
            let shown = match bytes {
                Bytes::B1 => raw & 0xFF,
                Bytes::B2 => raw & 0xFFFF,
                Bytes::B4 => raw & 0xFFFF_FFFF,
                Bytes::B8 => raw,
            };
            (
                "mov".into(),
                format!("{},{shown:#x}", mem(bytes, base, disp)),
            )
        }
        Inst::Push(s) => ("push".into(), R64[usize::from(s)].into()),
        Inst::Pop(d) => ("pop".into(), R64[usize::from(d)].into()),
        Inst::Call(s) => ("call".into(), R64[usize::from(s)].into()),
        Inst::Ret => ("ret".into(), String::new()),
        Inst::Jmp(_) => ("jmp".into(), String::new()),
        Inst::Jcc(c, _) => (format!("j{}", cc(c)), String::new()),
        Inst::Bind(_) => (String::new(), String::new()),
    };
    (name, operands)
}
