//! A register-level model of the clock manager, holding the driver to the
//! hardware's rules as assertions: every write carries the password, only a
//! served generator's two registers are ever written, a generator's source,
//! MASH and divisor change only while it is idle, and it is never enabled in
//! the write that changes them.

use std::cell::RefCell;
use std::vec::Vec;

use tairix_abi::{DriverError, RegisterBlock};

pub const WINDOW: usize = 0x2000;

const PASSWORD: u32 = 0x5A << 24;

pub const PCM_CTL: usize = 0x98;
pub const PCM_DIV: usize = 0x9C;
pub const PWM_CTL: usize = 0xA0;
pub const PWM_DIV: usize = 0xA4;

pub const CTL_ENABLE: u32 = 1 << 4;
pub const CTL_KILL: u32 = 1 << 5;
pub const CTL_BUSY: u32 = 1 << 7;
pub const MASH_FIRST_ORDER: u32 = 1 << 9;
/// The source and MASH fields.
const CTL_CONFIG: u32 = 0xF | 0b11 << 9;

pub const SOURCE_OSCILLATOR: u32 = 1;
pub const SOURCE_TEST: u32 = 2;
pub const SOURCE_PLLC: u32 = 5;
pub const SOURCE_PLLD: u32 = 6;

pub const PLLC_CTRL: usize = 0x1120;
pub const PLLC_FRAC: usize = 0x1220;
pub const PLLC_PER: usize = 0x1520;
pub const PLLD_CTRL: usize = 0x1140;
pub const PLLD_FRAC: usize = 0x1240;
pub const PLLD_ANA1: usize = 0x1054;
pub const PLLD_PER: usize = 0x1540;

pub const PLL_OUT_OF_RESET: u32 = 1 << 17;
pub const PLL_POWER_DOWN: u32 = 1 << 16;
pub const PLL_FEEDBACK_PREDIV: u32 = 1 << 14;
pub const CHANNEL_DISABLED: u32 = 1 << 8;

pub const OSCILLATOR: u64 = 54_000_000;

/// A running PLL's control word: its pre-divider and whole multiplier.
pub fn pll_control(pdiv: u32, ndiv: u32) -> u32 {
    PLL_OUT_OF_RESET | pdiv << 12 | ndiv
}

/// How a generator answers being stopped.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Stopping {
    /// It finishes its period after this many status reads.
    After(u32),
    /// Its source has stopped, so only a reset stops it.
    NeedsKill,
    /// Nothing stops it.
    Never,
}

#[derive(Copy, Clone)]
struct Generator {
    control: usize,
    divisor: usize,
    stopping: Stopping,
    busy: bool,
    /// Status reads left before a stopping generator goes idle.
    countdown: Option<u32>,
}

pub struct Model {
    regs: RefCell<Vec<u32>>,
    generators: RefCell<[Generator; 2]>,
    writes: RefCell<Vec<(usize, u32)>>,
}

impl Model {
    /// The manager with PLLD's peripheral channel at 675 MHz from the 54 MHz
    /// oscillator, every other PLL stopped, and both generators idle.
    pub fn new() -> Self {
        let idle = |control, divisor| Generator {
            control,
            divisor,
            stopping: Stopping::After(3),
            busy: false,
            countdown: None,
        };
        let model = Self {
            regs: RefCell::new(vec![0; WINDOW / 4]),
            generators: RefCell::new([idle(PCM_CTL, PCM_DIV), idle(PWM_CTL, PWM_DIV)]),
            writes: RefCell::new(Vec::new()),
        };
        model.set(PLLD_CTRL, pll_control(1, 50));
        model.set(PLLD_PER, 4);
        model
    }

    /// Set a register as the firmware left it, which is no write of the
    /// driver's.
    pub fn set(&self, offset: usize, value: u32) {
        self.regs.borrow_mut()[offset / 4] = value;
    }

    pub fn get(&self, offset: usize) -> u32 {
        self.regs.borrow()[offset / 4]
    }

    /// Leave the generator at `control` running as the firmware might.
    pub fn running(&self, control: usize, source: u32, divisor: u32) {
        let at = Self::index(control);
        let generator = self.generators.borrow()[at];
        self.set(generator.divisor, divisor);
        self.set(control, source | CTL_ENABLE);
        self.generators.borrow_mut()[at].busy = true;
    }

    pub fn stopping(&self, control: usize, stopping: Stopping) {
        self.generators.borrow_mut()[Self::index(control)].stopping = stopping;
    }

    pub fn writes(&self) -> Vec<(usize, u32)> {
        self.writes.borrow().clone()
    }

    pub fn clear_writes(&self) {
        self.writes.borrow_mut().clear();
    }

    pub fn enabled(&self, control: usize) -> bool {
        self.get(control) & CTL_ENABLE != 0
    }

    fn index(control: usize) -> usize {
        match control {
            PCM_CTL => 0,
            PWM_CTL => 1,
            other => panic!("no generator at {other:#x}"),
        }
    }

    fn generator_of(&self, offset: usize) -> Option<usize> {
        self.generators
            .borrow()
            .iter()
            .position(|generator| generator.control == offset || generator.divisor == offset)
    }

    fn write_control(&self, at: usize, value: u32) {
        let mut generators = self.generators.borrow_mut();
        let generator = &mut generators[at];
        let old = self.get(generator.control);
        if generator.busy {
            assert_eq!(
                value & CTL_CONFIG,
                old & CTL_CONFIG,
                "reconfigured while busy"
            );
        }
        if value & CTL_ENABLE != 0 && old & CTL_ENABLE == 0 {
            assert_eq!(
                value & CTL_CONFIG,
                old & CTL_CONFIG,
                "enabled in the write that configures it"
            );
        }
        if value & CTL_KILL != 0 {
            if generator.stopping != Stopping::Never {
                generator.busy = false;
                generator.countdown = None;
            }
        } else if value & CTL_ENABLE != 0 {
            generator.busy = true;
            generator.countdown = None;
        } else if old & CTL_ENABLE != 0 {
            generator.countdown = match generator.stopping {
                Stopping::After(reads) => Some(reads),
                Stopping::NeedsKill | Stopping::Never => None,
            };
        }
        let control = generator.control;
        drop(generators);
        self.set(control, value & !CTL_BUSY);
    }

    fn read_control(&self, at: usize) -> u32 {
        let mut generators = self.generators.borrow_mut();
        let generator = &mut generators[at];
        match generator.countdown {
            Some(0) => {
                generator.busy = false;
                generator.countdown = None;
            }
            Some(left) => generator.countdown = Some(left - 1),
            None => {}
        }
        let busy = if generator.busy { CTL_BUSY } else { 0 };
        let control = generator.control;
        drop(generators);
        self.get(control) | busy
    }
}

impl RegisterBlock for Model {
    fn read32(&self, offset: usize) -> Result<u32, DriverError> {
        assert!(
            offset.is_multiple_of(4) && offset < WINDOW,
            "read at {offset:#x}"
        );
        match self.generator_of(offset) {
            Some(at) if self.generators.borrow()[at].control == offset => Ok(self.read_control(at)),
            _ => Ok(self.get(offset)),
        }
    }

    fn write32(&self, offset: usize, value: u32) -> Result<(), DriverError> {
        assert_eq!(value & 0xFF00_0000, PASSWORD, "write without the password");
        let at = self
            .generator_of(offset)
            .unwrap_or_else(|| panic!("wrote {offset:#x}, which is no served generator's"));
        self.writes.borrow_mut().push((offset, value));
        let value = value & 0x00FF_FFFF;
        if self.generators.borrow()[at].control == offset {
            self.write_control(at, value);
        } else {
            assert!(
                !self.generators.borrow()[at].busy,
                "divisor changed while busy"
            );
            self.set(offset, value);
        }
        Ok(())
    }

    fn block_len(&self) -> usize {
        WINDOW
    }
}
