//! The `Run` binary of the BCM2711 clock manager driver, autoloaded into user
//! space by `devmgr` for a discovered `brcm,bcm2711-cprman` node.
//!
//! It reads the oscillator's rate from the node's first `clocks` entry, maps
//! the register window and serves the node's endpoint. It touches no clock
//! until a consumer asks. On the host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_clock::{bringup_failed, fixed_rate, serve};
    use tairix_drv_clock_bcm2711_cprman::clocks::Clocks;
    use tairix_drv_clock_bcm2711_cprman::cprman::Cprman;
    use tairix_drv_clock_bcm2711_cprman::REQUIRED_CAPABILITIES;

    /// The binding's position of the oscillator among the node's `clocks`.
    const OSCILLATOR_ENTRY: u8 = 0;

    fn run() -> Result<i32, i32> {
        let node = tairix_clock::open(REQUIRED_CAPABILITIES.iter().copied().collect())?;
        let oscillator = fixed_rate(&node, OSCILLATOR_ENTRY)?;
        let regs = node.map()?;
        let cprman = Cprman::new(&regs, oscillator)
            .map_err(|_| bringup_failed("clock: the register window does not reach the PLLs"))?;
        Ok(serve(&node, Clocks(cprman)))
    }

    fn main() -> i32 {
        match run() {
            Ok(code) | Err(code) => code,
        }
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
