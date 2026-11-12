//! The Intel 8259A programmable interrupt controller.

/// Where an initialisation sequence has got to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Init {
    /// Initialised: odd-port writes set the mask.
    Ready,
    /// ICW1 written; ICW2 (the vector base) is next.
    Icw2,
    /// ICW3 (cascade wiring) is next.
    Icw3,
    /// ICW4 (mode) is next.
    Icw4,
}

/// What an even-port read returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadRegister {
    Irr,
    Isr,
}

/// ICW1's bits.
mod icw1 {
    pub const IC4: u8 = 0x01;
    pub const SINGLE: u8 = 0x02;
    /// Call-address interval four rather than eight, in MCS-80 mode.
    pub const ADI: u8 = 0x04;
    pub const LEVEL: u8 = 0x08;
}

/// ICW4's bits.
mod icw4 {
    pub const X86: u8 = 0x01;
    pub const AUTO_EOI: u8 = 0x02;
    pub const SPECIAL_FULLY_NESTED: u8 = 0x10;
}

/// An 8259A.
#[derive(Clone, Debug)]
pub struct Pic {
    irr: u8,
    isr: u8,
    imr: u8,
    lines: u8,
    icw1: u8,
    icw2: u8,
    icw4: u8,
    init: Init,
    rotate_on_auto_eoi: bool,
    special_mask: bool,
    poll: bool,
    read: ReadRegister,
    /// The IRQ with the lowest priority; the next one round has the highest.
    lowest: u8,
}

impl Default for Pic {
    fn default() -> Self {
        Self::new()
    }
}

impl Pic {
    /// A controller as it powers up: uninitialised, everything masked.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            irr: 0,
            isr: 0,
            imr: 0xFF,
            lines: 0,
            icw1: icw1::SINGLE,
            icw2: 0,
            icw4: 0,
            init: Init::Ready,
            rotate_on_auto_eoi: false,
            special_mask: false,
            poll: false,
            read: ReadRegister::Irr,
            lowest: 7,
        }
    }

    const fn level_triggered(&self) -> bool {
        self.icw1 & icw1::LEVEL != 0
    }

    /// Drives IRQ input `irq` to `high`.
    pub fn set_line(&mut self, irq: u8, high: bool) {
        let bit = 1u8 << (irq & 7);
        let was = self.lines & bit != 0;
        if high {
            self.lines |= bit;
            if self.level_triggered() || !was {
                self.irr |= bit;
            }
        } else {
            self.lines &= !bit;
            if self.level_triggered() {
                self.irr &= !bit;
            }
        }
    }

    /// A low-to-high transition on IRQ `irq`, whatever level it was at: an
    /// edge the board saw happen between two of its visits.
    pub fn rising_edge(&mut self, irq: u8) {
        self.set_line(irq, false);
        self.set_line(irq, true);
    }

    /// The IRQs in priority order, highest first.
    fn priorities(&self) -> impl Iterator<Item = u8> {
        let first = (self.lowest + 1) & 7;
        (0..8u8).map(move |i| (first + i) & 7)
    }

    /// The highest-priority request that may interrupt now. A level in
    /// service holds off itself and every level below it, except that
    /// special mask mode releases the levels below and special fully nested
    /// mode lets the level itself in again.
    fn pending(&self) -> Option<u8> {
        let requests = self.irr & !self.imr;
        let nested = self.icw4 & icw4::SPECIAL_FULLY_NESTED != 0;
        for irq in self.priorities() {
            let bit = 1u8 << irq;
            let in_service = self.isr & bit != 0;
            if self.special_mask {
                if requests & bit != 0 && !in_service {
                    return Some(irq);
                }
            } else if in_service {
                return (nested && requests & bit != 0).then_some(irq);
            } else if requests & bit != 0 {
                return Some(irq);
            }
        }
        None
    }

    /// Whether the INT output is asserted.
    #[must_use]
    pub fn intr(&self) -> bool {
        self.init == Init::Ready && self.pending().is_some()
    }

    /// The byte the second acknowledge cycle drives for `irq`: the vector in
    /// 8086 mode, and in MCS-80 mode the low byte of the CALL address, which
    /// an 8086-family CPU takes as its vector all the same.
    fn vector(&self, irq: u8) -> u8 {
        if self.icw4 & icw4::X86 != 0 {
            (self.icw2 & 0xF8) | irq
        } else if self.icw1 & icw1::ADI != 0 {
            (self.icw1 & 0xE0) | (irq << 2)
        } else {
            (self.icw1 & 0xC0) | (irq << 3)
        }
    }

    /// The interrupt-acknowledge cycles: the vector of the request taken, or
    /// IRQ 7's for a request that vanished (a spurious interrupt).
    pub fn acknowledge(&mut self) -> u8 {
        let Some(irq) = self.pending() else {
            return self.vector(7);
        };
        self.take(irq);
        self.vector(irq)
    }

    fn take(&mut self, irq: u8) {
        let bit = 1u8 << irq;
        if !self.level_triggered() {
            self.irr &= !bit;
        }
        if self.icw4 & icw4::AUTO_EOI == 0 {
            self.isr |= bit;
        } else if self.rotate_on_auto_eoi {
            self.lowest = irq;
        }
    }

    /// A write to the even (`a0` clear) or odd port.
    pub fn write(&mut self, a0: bool, value: u8) {
        if !a0 && value & 0x10 != 0 {
            self.icw1(value);
            return;
        }
        if a0 {
            match self.init {
                Init::Icw2 => {
                    self.icw2 = value;
                    self.init = if self.icw1 & icw1::SINGLE == 0 {
                        Init::Icw3
                    } else {
                        self.after_icw3()
                    };
                }
                Init::Icw3 => self.init = self.after_icw3(),
                Init::Icw4 => {
                    self.icw4 = value;
                    self.init = Init::Ready;
                }
                Init::Ready => self.imr = value,
            }
        } else if value & 0x08 == 0 {
            self.ocw2(value);
        } else {
            self.ocw3(value);
        }
    }

    const fn after_icw3(&self) -> Init {
        if self.icw1 & icw1::IC4 != 0 {
            Init::Icw4
        } else {
            Init::Ready
        }
    }

    fn icw1(&mut self, value: u8) {
        self.init = Init::Icw2;
        self.icw1 = value;
        self.icw4 = 0;
        self.imr = 0;
        self.isr = 0;
        self.irr = if self.level_triggered() {
            self.lines
        } else {
            0
        };
        self.special_mask = false;
        self.poll = false;
        self.read = ReadRegister::Irr;
        self.lowest = 7;
        self.rotate_on_auto_eoi = false;
    }

    fn ocw2(&mut self, value: u8) {
        let level = value & 7;
        let highest_in_service = self.priorities().find(|&irq| self.isr & (1 << irq) != 0);
        match value >> 5 {
            0b001 => {
                if let Some(irq) = highest_in_service {
                    self.isr &= !(1 << irq);
                }
            }
            0b011 => self.isr &= !(1 << level),
            0b101 => {
                if let Some(irq) = highest_in_service {
                    self.isr &= !(1 << irq);
                    self.lowest = irq;
                }
            }
            0b100 => self.rotate_on_auto_eoi = true,
            0b000 => self.rotate_on_auto_eoi = false,
            0b111 => {
                self.isr &= !(1 << level);
                self.lowest = level;
            }
            0b110 => self.lowest = level,
            _ => {}
        }
    }

    fn ocw3(&mut self, value: u8) {
        if value & 0x40 != 0 {
            self.special_mask = value & 0x20 != 0;
        }
        self.poll = value & 0x04 != 0;
        if value & 0x02 != 0 {
            self.read = if value & 0x01 != 0 {
                ReadRegister::Isr
            } else {
                ReadRegister::Irr
            };
        }
    }

    /// A read of the even (`a0` clear) or odd port. After a poll command
    /// either port answers the poll, which acknowledges what it reports.
    pub fn read(&mut self, a0: bool) -> u8 {
        if self.poll {
            self.poll = false;
            return match self.pending() {
                Some(irq) => {
                    self.take(irq);
                    0x80 | irq
                }
                None => 0,
            };
        }
        if a0 {
            self.imr
        } else {
            match self.read {
                ReadRegister::Irr => self.irr,
                ReadRegister::Isr => self.isr,
            }
        }
    }

    /// Whether a rising edge on IRQ `irq` could raise INT now, which is all
    /// the board needs to know to decide whether the edge's timing matters.
    #[must_use]
    pub const fn listens_to(&self, irq: u8) -> bool {
        self.imr & (1 << (irq & 7)) == 0
    }
}

crate::snapshot::state_enum!(Init {
    0 => Ready,
    1 => Icw2,
    2 => Icw3,
    3 => Icw4,
});

crate::snapshot::state_enum!(ReadRegister {
    0 => Irr,
    1 => Isr,
});

crate::snapshot::state!(
    Pic {
        irr,
        isr,
        imr,
        lines,
        icw1,
        icw2,
        icw4,
        init,
        rotate_on_auto_eoi,
        special_mask,
        poll,
        read,
        lowest,
    }
    check |pic: &Pic| pic.lowest < 8
);

#[cfg(test)]
#[path = "pic_tests.rs"]
mod tests;
