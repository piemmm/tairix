//! The `Run` binary of the BCM2711 DVP clock-gate driver, autoloaded into user
//! space by `devmgr` for a discovered `brcm,brcm2711-dvp` node.
//!
//! It reads the gated clock's rate from the node's `clocks` entry, maps the
//! register window and serves the node's endpoint. It touches no gate until a
//! consumer asks. On the host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_clock::{fixed_rate, serve};
    use tairix_drv_clock_bcm2711_dvp::{Gates, REQUIRED_CAPABILITIES};
    use tairix_drvrt::exit::{end, step, DriverExit};

    /// The binding's position of the gated clock among the node's `clocks`.
    const PARENT_ENTRY: u8 = 0;

    fn run() -> Result<i32, i32> {
        let node = tairix_clock::open(REQUIRED_CAPABILITIES.iter().copied().collect())?;
        let parent = fixed_rate(&node, PARENT_ENTRY)?;
        let regs = node.map()?;
        let gates = Gates::new(&regs, parent).map_err(|_| {
            end(
                DriverExit::BringupFailed,
                &[step("clock: the register window does not reach the gates")],
            )
        })?;
        Ok(serve(&node, gates))
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
