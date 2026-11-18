//! Core registers and register lists.

use core::fmt;

/// A core register, `R0` to `R15`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Reg(u8);

impl Reg {
    /// The stack pointer by convention.
    pub const SP: Self = Self(13);
    /// The link register.
    pub const LR: Self = Self(14);
    /// The program counter.
    pub const PC: Self = Self(15);

    /// Register `number`, if it is one.
    #[must_use]
    pub const fn new(number: u32) -> Option<Self> {
        if number < 16 {
            Some(Self(number.to_le_bytes()[0]))
        } else {
            None
        }
    }

    /// The register a four-bit field names; bits above the field are ignored.
    #[must_use]
    pub const fn field(bits: u32) -> Self {
        Self((bits & 15).to_le_bytes()[0])
    }

    /// The register's number.
    #[must_use]
    pub const fn number(self) -> u32 {
        self.0 as u32
    }

    /// The register's number as an index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// The register after this one, wrapping from `R15` to `R0`.
    #[must_use]
    pub const fn next(self) -> Self {
        Self((self.0 + 1) & 15)
    }
}

impl fmt::Display for Reg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::PC => f.write_str("PC"),
            Self(n) => write!(f, "R{n}"),
        }
    }
}

/// A set of core registers, as `LDM` and `STM` name them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct RegList(pub u16);

impl RegList {
    /// Whether `reg` is in the list.
    #[must_use]
    pub const fn contains(self, reg: Reg) -> bool {
        self.0 & (1 << reg.0) != 0
    }

    /// How many registers the list names.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.0.count_ones()
    }

    /// Whether the list names no register.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The lowest-numbered register in the list.
    #[must_use]
    pub const fn lowest(self) -> Option<Reg> {
        if self.0 == 0 {
            None
        } else {
            Some(Reg(self.0.trailing_zeros().to_le_bytes()[0]))
        }
    }

    /// The registers in ascending order.
    pub fn iter(self) -> impl Iterator<Item = Reg> {
        (0..16u8).filter(move |&n| self.0 & (1 << n) != 0).map(Reg)
    }
}

impl fmt::Display for RegList {
    /// Ranges of three or more run together, as in `{R0-R3,R14}`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{")?;
        let mut first = true;
        let mut n = 0u8;
        while n < 16 {
            if self.0 & (1 << n) == 0 {
                n += 1;
                continue;
            }
            let start = n;
            while n < 15 && self.0 & (1 << (n + 1)) != 0 {
                n += 1;
            }
            if !first {
                f.write_str(",")?;
            }
            first = false;
            match n - start {
                0 => write!(f, "{}", Reg(start))?,
                1 => write!(f, "{},{}", Reg(start), Reg(n))?,
                _ => write!(f, "{}-{}", Reg(start), Reg(n))?,
            }
            n += 1;
        }
        f.write_str("}")
    }
}
