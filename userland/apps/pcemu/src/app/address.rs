//! An address as the debugger's user types one: `segment:offset`, or an
//! offset alone in the segment a view reads from, each part a hexadecimal
//! number (an `h` after it allowed) or a register's name — `DS:SI`, `0F000:0E05B`,
//! `SP`. A bare number past `FFFF` names a physical address. Spelling is
//! the user's, so case is not.

use crate::bits::low16;
use crate::cpu::{gpr, Regs, Seg};

/// An address the user named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Address {
    /// A segment and an offset.
    Far(u16, u16),
    /// An offset in the segment the view reads from.
    Near(u16),
    /// A physical address.
    Physical(u32),
}

impl Address {
    /// The segment and offset it names, the view reading from `segment`: a
    /// physical address as the paragraph holding it and the offset there.
    #[must_use]
    pub const fn resolve(self, segment: u16) -> (u16, u16) {
        match self {
            Self::Far(segment, offset) => (segment, offset),
            Self::Near(offset) => (segment, offset),
            Self::Physical(addr) => (low16(addr >> 4), low16(addr & 0xF)),
        }
    }
}

/// The 16-bit registers by name, and where each lives.
const WORDS: [(&str, Register); 14] = [
    ("AX", Register::General(gpr::AX)),
    ("BX", Register::General(gpr::BX)),
    ("CX", Register::General(gpr::CX)),
    ("DX", Register::General(gpr::DX)),
    ("SI", Register::General(gpr::SI)),
    ("DI", Register::General(gpr::DI)),
    ("BP", Register::General(gpr::BP)),
    ("SP", Register::General(gpr::SP)),
    ("CS", Register::Segment(Seg::Cs)),
    ("DS", Register::Segment(Seg::Ds)),
    ("ES", Register::Segment(Seg::Es)),
    ("SS", Register::Segment(Seg::Ss)),
    ("IP", Register::Ip),
    ("FLAGS", Register::Flags),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Register {
    General(usize),
    Segment(Seg),
    Ip,
    Flags,
}

impl Register {
    fn read(self, regs: &Regs) -> u16 {
        match self {
            Self::General(n) => regs.gpr[n],
            Self::Segment(seg) => regs.sreg(seg),
            Self::Ip => regs.ip,
            Self::Flags => regs.flags,
        }
    }
}

/// The most characters an address is typed in.
pub const MOST_TYPED: usize = 24;

/// One part's value: a register `regs` holds, or a hexadecimal number no
/// wider than `limit`.
fn part(text: &str, regs: &Regs, limit: u32) -> Option<u32> {
    let text = text.trim();
    if let Some((_, register)) = WORDS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(text))
    {
        return Some(u32::from(register.read(regs)));
    }
    let digits = text.strip_suffix(['h', 'H']).unwrap_or(text);
    if digits.is_empty() || digits.len() > 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16)
        .ok()
        .filter(|&value| value <= limit)
}

/// The address `text` names, the registers standing as `regs`; none for
/// what names none.
#[must_use]
pub fn parse(text: &str, regs: &Regs) -> Option<Address> {
    if text.len() > MOST_TYPED {
        return None;
    }
    if let Some((segment, offset)) = text.split_once(':') {
        let segment = u16::try_from(part(segment, regs, 0xFFFF)?).ok()?;
        let offset = u16::try_from(part(offset, regs, 0xFFFF)?).ok()?;
        return Some(Address::Far(segment, offset));
    }
    let value = part(text, regs, 0xF_FFFF)?;
    Some(match u16::try_from(value) {
        Ok(offset) => Address::Near(offset),
        Err(_) => Address::Physical(value),
    })
}

#[cfg(test)]
#[path = "address_tests.rs"]
mod tests;
