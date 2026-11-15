//! The arithmetic the instructions share: the barrel shifter, addition with
//! its flags, saturation and the parallel lanes.

use super::execute::wrap;
use crate::a32::{Lanes, Prefix, Shift, ShiftType};

/// A shifter's output and its carry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Shifted {
    pub value: u32,
    pub carry: bool,
}

const fn bit(value: u32, n: u32) -> bool {
    (value >> n) & 1 != 0
}

/// `value` shifted by an immediate, with `carry` the C flag going in.
pub(super) fn shift_immediate(value: u32, shift: Shift, carry: bool) -> Shifted {
    match shift {
        Shift::Lsl(n) => shift_register(value, ShiftType::Lsl, u32::from(n), carry),
        Shift::Lsr(n) => shift_register(value, ShiftType::Lsr, u32::from(n), carry),
        Shift::Asr(n) => shift_register(value, ShiftType::Asr, u32::from(n), carry),
        Shift::Ror(n) => shift_register(value, ShiftType::Ror, u32::from(n), carry),
        Shift::Rrx => Shifted {
            value: (value >> 1) | (u32::from(carry) << 31),
            carry: bit(value, 0),
        },
    }
}

/// `value` shifted by `amount`, the bottom byte of a register.
pub(super) const fn shift_register(
    value: u32,
    kind: ShiftType,
    amount: u32,
    carry: bool,
) -> Shifted {
    if amount == 0 {
        return Shifted { value, carry };
    }
    match kind {
        ShiftType::Lsl => match amount {
            1..=31 => Shifted {
                value: value << amount,
                carry: bit(value, 32 - amount),
            },
            32 => Shifted {
                value: 0,
                carry: bit(value, 0),
            },
            _ => Shifted {
                value: 0,
                carry: false,
            },
        },
        ShiftType::Lsr => match amount {
            1..=31 => Shifted {
                value: value >> amount,
                carry: bit(value, amount - 1),
            },
            32 => Shifted {
                value: 0,
                carry: bit(value, 31),
            },
            _ => Shifted {
                value: 0,
                carry: false,
            },
        },
        ShiftType::Asr => match amount {
            1..=31 => Shifted {
                value: (value.cast_signed() >> amount).cast_unsigned(),
                carry: bit(value, amount - 1),
            },
            _ => Shifted {
                value: (value.cast_signed() >> 31).cast_unsigned(),
                carry: bit(value, 31),
            },
        },
        ShiftType::Ror => {
            let n = amount & 31;
            if n == 0 {
                Shifted {
                    value,
                    carry: bit(value, 31),
                }
            } else {
                Shifted {
                    value: value.rotate_right(n),
                    carry: bit(value, n - 1),
                }
            }
        }
    }
}

/// `a + b + carry`, with the carry and overflow out.
pub(super) fn add_with_carry(a: u32, b: u32, carry: bool) -> (u32, bool, bool) {
    let wide = u64::from(a) + u64::from(b) + u64::from(carry);
    let result = super::execute::low(wide);
    let overflow = ((a ^ result) & (b ^ result)) >> 31 != 0;
    (result, wide >> 32 != 0, overflow)
}

/// `value` saturated to a signed field of `bits`, and whether it was.
pub(super) fn signed_saturate(value: i64, bits: u32) -> (u32, bool) {
    let top = (1i64 << (bits - 1)) - 1;
    let bottom = -(1i64 << (bits - 1));
    let clamped = value.clamp(bottom, top);
    (wrap(clamped), clamped != value)
}

/// `value` saturated to an unsigned field of `bits`, and whether it was.
pub(super) fn unsigned_saturate(value: i64, bits: u32) -> (u32, bool) {
    let top = (1i64 << bits) - 1;
    let clamped = value.clamp(0, top);
    (wrap(clamped), clamped != value)
}

/// A parallel operation's result, and the GE flags it sets when its prefix
/// sets them.
pub(super) fn parallel(prefix: Prefix, lanes: Lanes, a: u32, b: u32) -> (u32, Option<u32>) {
    let (width, count) = match lanes {
        Lanes::Add8 | Lanes::Sub8 => (8, 4),
        _ => (16, 2),
    };
    let mask = (1u32 << width) - 1;
    let signed = matches!(
        prefix,
        Prefix::Signed | Prefix::SignedSaturating | Prefix::SignedHalving
    );
    let lane = |value: u32, n: u32| -> i64 {
        let raw = (value >> (n * width)) & mask;
        if signed && raw >> (width - 1) != 0 {
            i64::from(raw) - (1 << width)
        } else {
            i64::from(raw)
        }
    };
    let mut result = 0;
    let mut ge = 0;
    for n in 0..count {
        let (x, y, subtract) = match lanes {
            Lanes::Add16 | Lanes::Add8 => (lane(a, n), lane(b, n), false),
            Lanes::Sub16 | Lanes::Sub8 => (lane(a, n), lane(b, n), true),
            Lanes::Asx => (lane(a, n), lane(b, 1 - n), n == 0),
            Lanes::Sax => (lane(a, n), lane(b, 1 - n), n == 1),
        };
        let sum = if subtract { x - y } else { x + y };
        let value = match prefix {
            Prefix::Signed | Prefix::Unsigned => wrap(sum),
            Prefix::SignedSaturating => signed_saturate(sum, width).0,
            Prefix::UnsignedSaturating => unsigned_saturate(sum, width).0,
            Prefix::SignedHalving | Prefix::UnsignedHalving => wrap(sum >> 1),
        };
        result |= (value & mask) << (n * width);
        let lane_ge = match prefix {
            Prefix::Signed => sum >= 0,
            Prefix::Unsigned if subtract => sum >= 0,
            Prefix::Unsigned => sum >= 1 << width,
            _ => false,
        };
        if lane_ge {
            let flags = if width == 8 { 1 } else { 0b11 };
            ge |= flags << (n * (4 / count));
        }
    }
    let sets_ge = matches!(prefix, Prefix::Signed | Prefix::Unsigned);
    (result, sets_ge.then_some(ge))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shifter_s_carry_follows_the_architecture() {
        let value = 0x8000_0001;
        assert_eq!(
            shift_immediate(value, Shift::Lsl(1), false),
            Shifted {
                value: 2,
                carry: true
            }
        );
        assert_eq!(
            shift_immediate(value, Shift::Lsr(32), false),
            Shifted {
                value: 0,
                carry: true
            }
        );
        assert_eq!(
            shift_immediate(value, Shift::Asr(32), false),
            Shifted {
                value: u32::MAX,
                carry: true
            }
        );
        assert_eq!(
            shift_immediate(value, Shift::Rrx, true),
            Shifted {
                value: 0xC000_0000,
                carry: true
            }
        );
        assert_eq!(
            shift_register(value, ShiftType::Lsl, 32, false),
            Shifted {
                value: 0,
                carry: true
            }
        );
        assert_eq!(
            shift_register(value, ShiftType::Lsl, 33, true),
            Shifted {
                value: 0,
                carry: false
            }
        );
        assert_eq!(
            shift_register(value, ShiftType::Lsr, 0, true),
            Shifted { value, carry: true }
        );
        assert_eq!(
            shift_register(value, ShiftType::Ror, 32, false),
            Shifted { value, carry: true }
        );
        assert_eq!(
            shift_register(value, ShiftType::Ror, 33, false),
            Shifted {
                value: 0xC000_0000,
                carry: true
            }
        );
        assert_eq!(
            shift_register(value, ShiftType::Asr, 200, false),
            Shifted {
                value: u32::MAX,
                carry: true
            }
        );
    }

    #[test]
    fn addition_reports_carry_and_overflow() {
        assert_eq!(add_with_carry(u32::MAX, 1, false), (0, true, false));
        assert_eq!(
            add_with_carry(0x7FFF_FFFF, 1, false),
            (0x8000_0000, false, true)
        );
        assert_eq!(add_with_carry(5, !3, true), (2, true, false), "5 - 3");
        assert_eq!(
            add_with_carry(3, !5, true),
            (u32::MAX - 1, false, false),
            "3 - 5"
        );
    }

    #[test]
    fn saturation_clamps_and_reports() {
        assert_eq!(signed_saturate(1 << 40, 32), (0x7FFF_FFFF, true));
        assert_eq!(signed_saturate(-200, 8), (0xFFFF_FF80, true));
        assert_eq!(signed_saturate(-100, 8), ((-100i32).cast_unsigned(), false));
        assert_eq!(unsigned_saturate(-1, 8), (0, true));
        assert_eq!(unsigned_saturate(300, 8), (255, true));
    }

    #[test]
    fn the_parallel_lanes_set_their_own_ge_flags() {
        let (sum, ge) = parallel(Prefix::Unsigned, Lanes::Add8, 0xFF01_80FF, 0x0101_8001);
        assert_eq!(sum, 0x0002_0000);
        assert_eq!(ge, Some(0b1011));
        let (difference, ge) = parallel(Prefix::Signed, Lanes::Sub16, 0x0001_0005, 0x0002_0003);
        assert_eq!(difference, 0xFFFF_0002);
        assert_eq!(ge, Some(0b0011));
        let (asx, _) = parallel(Prefix::Signed, Lanes::Asx, 0x0010_0020, 0x0003_0004);
        assert_eq!(asx, 0x0014_001D, "high = 0x10 + 4, low = 0x20 - 3");
        let (saturated, ge) = parallel(
            Prefix::SignedSaturating,
            Lanes::Add16,
            0x7FFF_8000,
            0x0001_FFFF,
        );
        assert_eq!(saturated, 0x7FFF_8000);
        assert_eq!(ge, None);
        let (halved, _) = parallel(
            Prefix::UnsignedHalving,
            Lanes::Add8,
            0xFF00_0000,
            0xFF00_0000,
        );
        assert_eq!(halved, 0xFF00_0000);
    }
}
