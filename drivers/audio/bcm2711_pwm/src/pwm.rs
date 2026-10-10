//! The PWM block's registers.
//!
//! Both channels run from the shared FIFO, which a DMA channel keeps fed, so
//! their words arrive interleaved: the first channel's, then the second's.
//! Each channel repeats its last word when the FIFO runs dry, which is what
//! holds the jack at silence between streams with no DMA running at all
//! (BCM2835 ARM Peripherals, chapter 9).

use tairix_abi::{DriverError, RegisterBlock};

const CTL: usize = 0x00;
const STA: usize = 0x04;
const DMAC: usize = 0x08;
const RNG1: usize = 0x10;
const RNG2: usize = 0x20;

/// The FIFO's offset in the block, which the DMA channel writes.
pub const FIFO: u64 = 0x18;

/// Bytes of window the registers lie within.
pub const WINDOW_LEN: usize = 0x28;

const CTL_PWEN1: u32 = 1 << 0;
const CTL_RPTL1: u32 = 1 << 2;
const CTL_USEF1: u32 = 1 << 5;
const CTL_CLRF1: u32 = 1 << 6;
const CTL_PWEN2: u32 = 1 << 8;
const CTL_RPTL2: u32 = 1 << 10;
const CTL_USEF2: u32 = 1 << 13;

/// Both channels on, fed from the FIFO, repeating their last word when it is
/// empty, in the balanced mode that spreads each duty's high cycles across
/// its period rather than as one pulse.
const CTL_RUN: u32 = CTL_PWEN1 | CTL_USEF1 | CTL_RPTL1 | CTL_PWEN2 | CTL_USEF2 | CTL_RPTL2;

/// Every latched error: the FIFO's write and read errors, the four gaps and
/// the bus error. Each clears when written one.
const STA_ERRORS: u32 = 0x1FC;

const DMAC_ENAB: u32 = 1 << 31;
/// The FIFO thresholds the DMA requests and the panic signal are raised at,
/// in words.
const DMAC_PANIC: u32 = 7;
const DMAC_DREQ: u32 = 3;

/// The PWM block, reached through its register window.
pub struct Pwm<'r, R: RegisterBlock + ?Sized> {
    regs: &'r R,
}

impl<'r, R: RegisterBlock + ?Sized> Pwm<'r, R> {
    /// The block behind `regs`.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] for a window short of the registers.
    pub fn new(regs: &'r R) -> Result<Self, DriverError> {
        if regs.block_len() < WINDOW_LEN {
            return Err(DriverError::LengthOutOfRange);
        }
        Ok(Self { regs })
    }

    /// Run both channels at `levels` clock cycles a period, from a cleared
    /// FIFO the DMA channel is asked to keep fed.
    ///
    /// # Errors
    ///
    /// A register write's failure.
    pub fn run(&self, levels: u32) -> Result<(), DriverError> {
        self.regs.write32(CTL, 0)?;
        self.regs.write32(STA, STA_ERRORS)?;
        self.regs.write32(RNG1, levels)?;
        self.regs.write32(RNG2, levels)?;
        self.regs.write32(CTL, CTL_CLRF1)?;
        self.regs
            .write32(DMAC, DMAC_ENAB | DMAC_PANIC << 8 | DMAC_DREQ)?;
        self.regs.write32(CTL, CTL_RUN)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::vec::Vec;

    use tairix_abi::{DriverError, RegisterBlock};

    use super::{Pwm, CTL, CTL_RUN, DMAC, RNG1, RNG2, STA, STA_ERRORS, WINDOW_LEN};

    struct Recorder {
        len: usize,
        writes: RefCell<Vec<(usize, u32)>>,
    }

    impl RegisterBlock for Recorder {
        fn read32(&self, _offset: usize) -> Result<u32, DriverError> {
            Ok(0)
        }
        fn write32(&self, offset: usize, value: u32) -> Result<(), DriverError> {
            self.writes.borrow_mut().push((offset, value));
            Ok(())
        }
        fn block_len(&self) -> usize {
            self.len
        }
    }

    #[test]
    fn the_block_is_stopped_cleared_and_set_up_before_both_channels_run() {
        let regs = Recorder {
            len: WINDOW_LEN,
            writes: RefCell::new(Vec::new()),
        };
        Pwm::new(&regs).expect("window").run(250).expect("runs");
        let writes = regs.writes.borrow();
        assert_eq!(writes.first(), Some(&(CTL, 0)), "stopped first");
        assert_eq!(writes.last(), Some(&(CTL, CTL_RUN)), "running last");
        for expected in [(STA, STA_ERRORS), (RNG1, 250), (RNG2, 250)] {
            assert!(writes.contains(&expected), "{expected:x?}");
        }
        let dmac = writes
            .iter()
            .find(|(offset, _)| *offset == DMAC)
            .expect("DMA requests enabled");
        assert_ne!(dmac.1 & 1 << 31, 0);
    }

    #[test]
    fn a_window_short_of_the_registers_is_refused() {
        let regs = Recorder {
            len: WINDOW_LEN - 4,
            writes: RefCell::new(Vec::new()),
        };
        assert!(matches!(
            Pwm::new(&regs),
            Err(DriverError::LengthOutOfRange)
        ));
    }
}
