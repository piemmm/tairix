//! An encoding's layout: its fixed bits, its should-be bits and where each
//! field lies, all read from one pattern string.
//!
//! A pattern spells the 32 bits from bit 31 down; spaces and underscores
//! are ignored. `0` and `1` are fixed bits, `-` and `+` bits that should be
//! zero and one, and every letter a bit of the field it names. A field's
//! value is its bits read from the most significant down, so a split
//! immediate such as `MOVW`'s `imm4:imm12` reads whole.

/// A field a pattern's letters name.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Field {
    /// `c`: the condition.
    Cond,
    /// `n`: Rn.
    Rn,
    /// `d`: Rd.
    Rd,
    /// `m`: Rm.
    Rm,
    /// `s`: Rs.
    Rs,
    /// `a`: Ra, an accumulator.
    Ra,
    /// `t`: Rt, a transferred register.
    Rt,
    /// `T`: Rt2, a second transferred register.
    Rt2,
    /// `h`: `RdHi`.
    Hi,
    /// `l`: `RdLo`.
    Lo,
    /// `i`: an immediate.
    Imm,
    /// `r`: an immediate's rotation.
    Rotate,
    /// `k`: a shift's amount.
    Amount,
    /// `y`: a shift's type.
    Type,
    /// `S`: set the flags.
    S,
    /// `P`: index before the transfer.
    P,
    /// `U`: add the offset.
    U,
    /// `B`: bit 22: a byte transfer, a long coprocessor transfer, or the
    /// user bank.
    B,
    /// `W`: write the address back.
    W,
    /// `L`: load.
    L,
    /// `R`: a register list.
    List,
    /// `p`: a coprocessor number.
    Cp,
    /// `o`: a coprocessor's first opcode.
    Opc1,
    /// `q`: a coprocessor's second opcode.
    Opc2,
    /// `N`: `CRn`.
    CRn,
    /// `M`: `CRm`.
    CRm,
    /// `D`: `CRd`.
    CRd,
    /// `f`: `MSR`'s field mask.
    Mask,
    /// `X`: a one-bit choice: a halfword, an exchange or a rounding.
    X,
    /// `Y`: a second halfword choice.
    Y,
    /// `H`: `BLX`'s halfword offset bit.
    H,
    /// `b`: a bitfield's least significant bit.
    Lsb,
    /// `e`: a bitfield's most significant bit, or its width less one.
    Msb,
    /// `x`: an extension's rotation in bytes.
    Rotation,
    /// `g`: a mode.
    Mode,
    /// `j`: `CPS`'s A, I and F.
    Interrupts,
    /// `I`: `CPS`'s enable or disable.
    Imod,
    /// `u`: `CPS` changes the mode.
    ChangeMode,
    /// `E`: big-endian.
    Endian,
    /// `Z`: the SPSR rather than the CPSR.
    Spsr,
    /// `J`: T32's first branch-offset bit beside the sign.
    J1,
    /// `K`: T32's second branch-offset bit beside the sign.
    J2,
}

/// How many fields there are.
pub(crate) const FIELDS: usize = Field::J2 as usize + 1;

impl Field {
    const fn of(letter: u8) -> Option<Self> {
        Some(match letter {
            b'c' => Self::Cond,
            b'n' => Self::Rn,
            b'd' => Self::Rd,
            b'm' => Self::Rm,
            b's' => Self::Rs,
            b'a' => Self::Ra,
            b't' => Self::Rt,
            b'T' => Self::Rt2,
            b'h' => Self::Hi,
            b'l' => Self::Lo,
            b'i' => Self::Imm,
            b'r' => Self::Rotate,
            b'k' => Self::Amount,
            b'y' => Self::Type,
            b'S' => Self::S,
            b'P' => Self::P,
            b'U' => Self::U,
            b'B' => Self::B,
            b'W' => Self::W,
            b'L' => Self::L,
            b'R' => Self::List,
            b'p' => Self::Cp,
            b'o' => Self::Opc1,
            b'q' => Self::Opc2,
            b'N' => Self::CRn,
            b'M' => Self::CRm,
            b'D' => Self::CRd,
            b'f' => Self::Mask,
            b'X' => Self::X,
            b'Y' => Self::Y,
            b'H' => Self::H,
            b'b' => Self::Lsb,
            b'e' => Self::Msb,
            b'x' => Self::Rotation,
            b'g' => Self::Mode,
            b'j' => Self::Interrupts,
            b'I' => Self::Imod,
            b'u' => Self::ChangeMode,
            b'E' => Self::Endian,
            b'Z' => Self::Spsr,
            b'J' => Self::J1,
            b'K' => Self::J2,
            _ => return None,
        })
    }
}

/// The bits a pattern fixes, the bits it wants zero or one, and its fields.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    /// The fixed bits.
    pub mask: u32,
    /// Their values.
    pub bits: u32,
    /// The should-be bits.
    pub should_mask: u32,
    /// Their required values.
    pub should_bits: u32,
    /// Each field's bits, by [`Field`].
    pub fields: [u32; FIELDS],
}

impl Layout {
    /// The layout a 32-bit `pattern` spells. Evaluated in a table's
    /// initialiser, so a malformed pattern fails the build.
    pub const fn of(pattern: &str) -> Self {
        Self::parse(pattern, 32)
    }

    /// The layout a 16-bit `pattern` spells, in the low half.
    pub const fn halfword(pattern: &str) -> Self {
        Self::parse(pattern, 16)
    }

    const fn parse(pattern: &str, width: u32) -> Self {
        let bytes = pattern.as_bytes();
        let mut layout = Self {
            mask: 0,
            bits: 0,
            should_mask: 0,
            should_bits: 0,
            fields: [0; FIELDS],
        };
        let mut seen = 0u32;
        let mut n = 0;
        while n < bytes.len() {
            let letter = bytes[n];
            n += 1;
            if letter == b' ' || letter == b'_' {
                continue;
            }
            assert!(seen < width, "a pattern spells its width's bits");
            let bit = 1u32 << (width - 1 - seen);
            seen += 1;
            match letter {
                b'0' => layout.mask |= bit,
                b'1' => {
                    layout.mask |= bit;
                    layout.bits |= bit;
                }
                b'-' => layout.should_mask |= bit,
                b'+' => {
                    layout.should_mask |= bit;
                    layout.should_bits |= bit;
                }
                _ => match Field::of(letter) {
                    Some(field) => layout.fields[field as usize] |= bit,
                    None => panic!("a pattern letter names no field"),
                },
            }
        }
        assert!(seen == width, "a pattern spells its width's bits");
        layout
    }

    /// The bits `field` occupies; zero when the pattern has no such field.
    pub const fn field(&self, field: Field) -> u32 {
        self.fields[field as usize]
    }

    /// Whether `word` has this layout's fixed bits.
    pub const fn matches(&self, word: u32) -> bool {
        word & self.mask == self.bits
    }

    /// The fields of `word`.
    pub fn extract(&self, word: u32) -> Fields {
        let mut values = [0; FIELDS];
        for (value, &mask) in values.iter_mut().zip(&self.fields) {
            *value = gather(word, mask);
        }
        Fields(values)
    }

    /// The word holding `fields`, the fixed bits and canonical should-be
    /// bits, or `None` when a field's value needs more bits than its field
    /// has.
    pub fn deposit(&self, fields: &Fields) -> Option<u32> {
        let mut word = self.bits | self.should_bits;
        for (&value, &mask) in fields.0.iter().zip(&self.fields) {
            if mask == 0 {
                continue;
            }
            if u64::from(value) >> mask.count_ones() != 0 {
                return None;
            }
            word |= scatter(value, mask);
        }
        Some(word)
    }
}

/// The bits of `word` under `mask`, packed from the most significant down.
fn gather(word: u32, mask: u32) -> u32 {
    if mask == 0 {
        return 0;
    }
    let low = mask.trailing_zeros();
    let run = mask >> low;
    if run & run.wrapping_add(1) == 0 {
        return (word >> low) & run;
    }
    let mut value = 0;
    let mut bits = mask;
    while bits != 0 {
        let top = bits.ilog2();
        value = (value << 1) | ((word >> top) & 1);
        bits &= !(1 << top);
    }
    value
}

/// `value`'s low bits spread over `mask`, the most significant first.
fn scatter(value: u32, mask: u32) -> u32 {
    let low = mask.trailing_zeros();
    let run = mask >> low;
    if run & run.wrapping_add(1) == 0 {
        return (value & run) << low;
    }
    let mut word = 0;
    let mut bits = mask;
    let mut rest = mask.count_ones();
    while bits != 0 {
        let top = bits.ilog2();
        rest -= 1;
        word |= ((value >> rest) & 1) << top;
        bits &= !(1 << top);
    }
    word
}

/// A value for each field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Fields(pub [u32; FIELDS]);

impl Default for Fields {
    fn default() -> Self {
        Self([0; FIELDS])
    }
}

impl Fields {
    /// `field`'s value.
    pub const fn get(&self, field: Field) -> u32 {
        self.0[field as usize]
    }

    /// Whether one-bit `field` is set.
    pub const fn bit(&self, field: Field) -> bool {
        self.0[field as usize] != 0
    }

    /// Sets `field` to `value`.
    pub fn set(&mut self, field: Field, value: u32) {
        self.0[field as usize] = value;
    }

    /// Sets one-bit `field`.
    pub fn flag(&mut self, field: Field, on: bool) {
        self.0[field as usize] = u32::from(on);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pattern_reads_its_fixed_bits_should_be_bits_and_fields() {
        let layout = Layout::of("cccc 0011 0100 iiii dddd iiii iiii iiii");
        assert_eq!(layout.mask, 0x0FF0_0000);
        assert_eq!(layout.bits, 0x0340_0000);
        assert_eq!(layout.field(Field::Cond), 0xF000_0000);
        assert_eq!(layout.field(Field::Imm), 0x000F_0FFF);
        assert_eq!(layout.field(Field::Rd), 0x0000_F000);
        let bx = Layout::of("cccc 0001 0010 ++++ ++++ ++++ 0001 mmmm");
        assert_eq!(bx.should_mask, 0x000F_FF00);
        assert_eq!(bx.should_bits, 0x000F_FF00);
    }

    #[test]
    fn a_halfword_pattern_reads_into_the_low_half() {
        let layout = Layout::halfword("1101 cccc iiii iiii");
        assert_eq!(layout.mask, 0xF000);
        assert_eq!(layout.bits, 0xD000);
        assert_eq!(layout.field(Field::Cond), 0x0F00);
        assert_eq!(layout.field(Field::Imm), 0x00FF);
    }

    #[test]
    fn a_split_field_reads_whole_and_writes_back_where_it_came_from() {
        let layout = Layout::of("cccc 0011 0100 iiii dddd iiii iiii iiii");
        let word = 0xE34A_5BCD;
        let fields = layout.extract(word);
        assert_eq!(fields.get(Field::Imm), 0xABCD);
        assert_eq!(fields.get(Field::Rd), 5);
        assert_eq!(layout.deposit(&fields), Some(word));
    }

    #[test]
    fn a_value_wider_than_its_field_is_refused() {
        let layout = Layout::of("cccc 1010 iiii iiii iiii iiii iiii iiii");
        let mut fields = Fields::default();
        fields.set(Field::Imm, 1 << 24);
        assert_eq!(layout.deposit(&fields), None);
        fields.set(Field::Imm, (1 << 24) - 1);
        assert_eq!(layout.deposit(&fields), Some(0x0AFF_FFFF));
    }

    #[test]
    fn gather_and_scatter_are_inverse_over_any_mask() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..2000 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let mask = u32::try_from(state >> 32).expect("the high half");
            let word = u32::try_from(state & u64::from(u32::MAX)).expect("the low half");
            let value = gather(word, mask);
            assert_eq!(scatter(value, mask), word & mask);
            assert_eq!(gather(scatter(value, mask), mask), value);
        }
    }
}
