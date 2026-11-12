//! The modelled processors and what distinguishes them.

/// A modelled processor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CpuModel {
    /// Intel 8088: the 8086 core on an 8-bit bus, four-byte queue.
    I8088,
    /// Intel 8086: 16-bit bus, six-byte queue.
    I8086,
    /// NEC V20: the 80186 instruction set plus NEC's extensions and 8080
    /// emulation, on the 8088's 8-bit bus.
    V20,
    /// NEC V30: the V20 on the 8086's 16-bit bus.
    V30,
    /// Intel 80188: the 80186 on an 8-bit bus.
    I80188,
    /// Intel 80186: the 8086 instruction set extended, with integrated
    /// peripherals, 16-bit bus.
    I80186,
}

/// The microarchitecture lineage a model belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    /// 8086 and 8088.
    Intel8086,
    /// 80186 and 80188.
    Intel80186,
    /// V20 and V30.
    Nec,
}

impl CpuModel {
    /// Every model, in presentation order.
    pub const ALL: [Self; 6] = [
        Self::I8088,
        Self::I8086,
        Self::V20,
        Self::V30,
        Self::I80188,
        Self::I80186,
    ];

    /// The model's lineage.
    #[must_use]
    pub const fn family(self) -> Family {
        match self {
            Self::I8088 | Self::I8086 => Family::Intel8086,
            Self::V20 | Self::V30 => Family::Nec,
            Self::I80188 | Self::I80186 => Family::Intel80186,
        }
    }

    /// Whether the external data bus is 8 bits wide, so each word transfer
    /// takes two bus cycles.
    #[must_use]
    pub const fn bus8(self) -> bool {
        matches!(self, Self::I8088 | Self::V20 | Self::I80188)
    }

    /// The prefetch queue's length in bytes.
    #[must_use]
    pub const fn queue_len(self) -> u8 {
        if self.bus8() {
            4
        } else {
            6
        }
    }

    /// Whether the 80186 instruction-set additions decode.
    #[must_use]
    pub const fn has_186_set(self) -> bool {
        !matches!(self.family(), Family::Intel8086)
    }

    /// Whether unused opcodes raise interrupt 6 rather than aliasing.
    #[must_use]
    pub const fn traps_unused(self) -> bool {
        matches!(self.family(), Family::Intel80186)
    }

    /// Whether shift and rotate counts are masked to five bits: the 80186
    /// masks them, the V-series does not.
    #[must_use]
    pub const fn masks_shift_count(self) -> bool {
        matches!(self.family(), Family::Intel80186)
    }

    /// The model's name as its manufacturer marked it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::I8088 => "Intel 8088",
            Self::I8086 => "Intel 8086",
            Self::V20 => "NEC V20",
            Self::V30 => "NEC V30",
            Self::I80188 => "Intel 80188",
            Self::I80186 => "Intel 80186",
        }
    }
}
