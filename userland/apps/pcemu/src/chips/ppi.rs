//! The Intel 8255A programmable peripheral interface, as the IBM PC and
//! PC/XT wire it: port A reads the keyboard, port B drives the speaker gate
//! and the keyboard's clock and clear lines, port C reads configuration
//! switches and status lines.
//!
//! The two boards differ in where the switches appear. The PC (5150) shows
//! switch block SW1 on port A while port B's bit 7 is set, and the I/O
//! channel memory switches SW2 on port C. The XT (5160) has no SW2 and shows
//! SW1 on port C, a nibble at a time.

/// Port B's bits.
pub mod port_b {
    /// Gates PIT channel 2.
    pub const TIMER2_GATE: u8 = 0x01;
    /// Enables the speaker from channel 2's output.
    pub const SPEAKER_DATA: u8 = 0x02;
    /// The PC: port C shows SW2's fifth switch rather than its first four.
    pub const PC_SW2_HIGH: u8 = 0x04;
    /// The XT: port C shows SW1's high nibble rather than its low one.
    pub const XT_SW1_HIGH: u8 = 0x08;
    /// Lets the keyboard clock run; low holds the keyboard in reset.
    pub const KEYBOARD_CLOCK: u8 = 0x40;
    /// Clears the keyboard's byte and its interrupt; on the PC, also shows
    /// SW1 on port A.
    pub const KEYBOARD_CLEAR: u8 = 0x80;
}

/// Which board the 8255 sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wiring {
    /// The IBM PC (5150).
    Pc,
    /// The IBM PC/XT (5160).
    Xt,
}

/// The display adapter the switches tell the BIOS to start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootDisplay {
    /// An adapter with its own BIOS (EGA, VGA).
    OwnBios,
    /// CGA in 40-column mode.
    Cga40,
    /// CGA in 80-column mode.
    Cga80,
    /// MDA or Hercules.
    Monochrome,
}

/// The configuration switches, as the BIOS reads them: a set bit is a
/// switch the equipment word reports present.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Switches {
    sw1: u8,
    sw2: u8,
}

impl Switches {
    /// The switches for a board: `floppies` drives (0–4), a coprocessor, the
    /// boot display, and `memory_kib` of conventional memory (64–640 in
    /// 64 KiB steps).
    #[must_use]
    pub fn new(
        wiring: Wiring,
        floppies: u8,
        coprocessor: bool,
        display: BootDisplay,
        memory_kib: u16,
    ) -> Self {
        let video = match display {
            BootDisplay::OwnBios => 0,
            BootDisplay::Cga40 => 1,
            BootDisplay::Cga80 => 2,
            BootDisplay::Monochrome => 3,
        };
        let drives = floppies.clamp(1, 4) - 1;
        let common = (u8::from(coprocessor) << 1) | (video << 4) | (drives << 6);
        match wiring {
            // SW1-1 says diskette drives are present; SW1-3 and -4 are the
            // planar banks in 64 KiB; SW2 counts I/O channel memory in 32 KiB.
            Wiring::Pc => {
                let planar = memory_kib.clamp(64, 256);
                let banks = u8::try_from(planar / 64 - 1).unwrap_or(3);
                let channel = u8::try_from(memory_kib.saturating_sub(planar) / 32).unwrap_or(0x1F);
                Self {
                    sw1: common | u8::from(floppies > 0) | (banks << 2),
                    sw2: channel & 0x1F,
                }
            }
            // SW1-1 off runs POST normally; all four planar banks are filled.
            Wiring::Xt => Self {
                sw1: common | 0x01 | 0x0C,
                sw2: 0,
            },
        }
    }
}

/// The 8255.
#[derive(Clone, Debug)]
pub struct Ppi {
    wiring: Wiring,
    control: u8,
    a: u8,
    b: u8,
    c: u8,
    switches: Switches,
}

/// The mode word RESET leaves: every port an input.
const RESET_MODE: u8 = 0x9B;

impl Ppi {
    /// A PPI on `wiring` with the board's switches, as RESET leaves it.
    #[must_use]
    pub const fn new(wiring: Wiring, switches: Switches) -> Self {
        Self {
            wiring,
            control: RESET_MODE,
            a: 0,
            b: 0,
            c: 0,
            switches,
        }
    }

    const fn a_is_input(&self) -> bool {
        self.control & 0x10 != 0
    }

    const fn b_is_input(&self) -> bool {
        self.control & 0x02 != 0
    }

    /// Port A: the keyboard's byte, or on the PC SW1 while port B's bit 7 is
    /// set.
    #[must_use]
    pub const fn read_a(&self, keyboard: u8) -> u8 {
        if !self.a_is_input() {
            self.a
        } else if matches!(self.wiring, Wiring::Pc) && self.b & port_b::KEYBOARD_CLEAR != 0 {
            self.switches.sw1
        } else {
            keyboard
        }
    }

    /// Port B's lines: its latch while it is an output, else the board's
    /// pull-ups.
    #[must_use]
    pub const fn read_b(&self) -> u8 {
        if self.b_is_input() {
            0xFF
        } else {
            self.b
        }
    }

    /// Port C: switches, channel 2's output, and the parity and channel
    /// check lines, which never fire.
    #[must_use]
    pub const fn read_c(&self, timer2_out: bool) -> u8 {
        let switches = match self.wiring {
            Wiring::Pc if self.b & port_b::PC_SW2_HIGH != 0 => self.switches.sw2 >> 4,
            Wiring::Pc => self.switches.sw2 & 0x0F,
            Wiring::Xt if self.b & port_b::XT_SW1_HIGH != 0 => self.switches.sw1 >> 4,
            Wiring::Xt => self.switches.sw1 & 0x0F,
        };
        let lower = if self.control & 0x01 != 0 {
            switches
        } else {
            self.c & 0x0F
        };
        let upper = if self.control & 0x08 != 0 {
            if timer2_out {
                0x20
            } else {
                0
            }
        } else {
            self.c & 0xF0
        };
        lower | upper
    }

    /// Writes port 60h–63h (`port` 0–3), returning port B's lines when
    /// they changed so the board can drive what they are wired to. Setting
    /// a mode clears every output latch.
    pub fn write(&mut self, port: u8, value: u8) -> Option<u8> {
        let before = self.read_b();
        match port & 3 {
            0 => self.a = value,
            1 => self.b = value,
            2 => self.c = value,
            _ if value & 0x80 != 0 => {
                self.control = value;
                self.a = 0;
                self.b = 0;
                self.c = 0;
            }
            _ => {
                let bit = 1u8 << ((value >> 1) & 7);
                if value & 1 != 0 {
                    self.c |= bit;
                } else {
                    self.c &= !bit;
                }
            }
        }
        let after = self.read_b();
        (after != before).then_some(after)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_xt_reads_sw1_a_nibble_at_a_time() {
        let switches = Switches::new(Wiring::Xt, 2, false, BootDisplay::Cga80, 640);
        let mut ppi = Ppi::new(Wiring::Xt, switches);
        ppi.write(3, 0x99);
        assert_eq!(ppi.read_c(false) & 0x0F, 0x0D);
        ppi.write(1, port_b::XT_SW1_HIGH);
        assert_eq!(ppi.read_c(true), 0x06 | 0x20);
        assert_eq!(
            ppi.read_a(0x1C),
            0x1C,
            "the XT's port A is always the keyboard"
        );
    }

    #[test]
    fn the_pc_shows_sw1_on_port_a_and_its_memory_switches_on_port_c() {
        let switches = Switches::new(Wiring::Pc, 1, true, BootDisplay::Monochrome, 640);
        let mut ppi = Ppi::new(Wiring::Pc, switches);
        ppi.write(3, 0x99);
        assert_eq!(ppi.read_a(0x1C), 0x1C);
        ppi.write(1, port_b::KEYBOARD_CLEAR);
        assert_eq!(
            ppi.read_a(0x1C),
            0x01 | 0x02 | 0x0C | 0x30,
            "drives, 8087, 256 KiB planar, mono"
        );
        assert_eq!(
            ppi.read_c(false) & 0x0F,
            12,
            "384 KiB on the I/O channel in 32 KiB units"
        );
        ppi.write(1, port_b::PC_SW2_HIGH);
        assert_eq!(ppi.read_c(false) & 0x0F, 0);
    }

    #[test]
    fn a_small_pc_has_fewer_planar_banks_and_no_channel_memory() {
        let switches = Switches::new(Wiring::Pc, 0, false, BootDisplay::Cga40, 128);
        let mut ppi = Ppi::new(Wiring::Pc, switches);
        ppi.write(3, 0x99);
        ppi.write(1, port_b::KEYBOARD_CLEAR);
        assert_eq!(ppi.read_a(0), 0x04 | 0x10, "no drives, two banks, CGA 40");
        assert_eq!(ppi.read_c(false) & 0x0F, 0);
    }

    fn bios_mode(wiring: Wiring) -> Ppi {
        let mut ppi = Ppi::new(
            wiring,
            Switches::new(wiring, 1, false, BootDisplay::Cga80, 640),
        );
        ppi.write(3, 0x99);
        ppi
    }

    #[test]
    fn port_b_reports_its_changes_once() {
        let mut ppi = bios_mode(Wiring::Xt);
        assert_eq!(ppi.write(1, 0x4D), Some(0x4D));
        assert_eq!(ppi.write(1, 0x4D), None);
        assert_eq!(ppi.read_b(), 0x4D);
    }

    #[test]
    fn reset_leaves_every_port_an_input() {
        let mut ppi = Ppi::new(
            Wiring::Xt,
            Switches::new(Wiring::Xt, 1, false, BootDisplay::Cga80, 640),
        );
        assert_eq!(ppi.read_b(), 0xFF, "port B's lines float to the pull-ups");
        assert_eq!(ppi.write(1, 0x4D), None, "an input port only latches");
        assert_eq!(
            ppi.write(3, 0x99),
            Some(0x00),
            "setting the mode clears the latch it now drives"
        );
    }
}

crate::snapshot::state!(Ppi { control, a, b, c } as built { wiring, switches });
