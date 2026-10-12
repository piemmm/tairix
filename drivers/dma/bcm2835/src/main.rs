//! The `Run` binary of the Broadcom DMA driver, autoloaded into user space by
//! `devmgr` for a discovered `brcm,bcm2835-dma` or `brcm,bcm2711-dma` node.
//!
//! It learns which engines its node holds from the bind key the node matched,
//! maps the register window, builds those engines over it, and hands them to
//! the shared serve loop. On the host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    use tairix_abi::driver::dma::DmaReach;
    use tairix_dmaengine::{bringup_failed, serve, Node};
    use tairix_drv_dma_bcm2835::dma4::{self, Bcm2711Dma};
    use tairix_drv_dma_bcm2835::legacy::{self, Bcm2835Dma};
    use tairix_drv_dma_bcm2835::{Engine, BIND_KEYS};

    const WINDOW_REFUSED: &str = "dma: the register window is not whole channels";

    /// The engines the node holds, by the rule the device manager matched it.
    fn engine() -> Option<Engine> {
        let record = tairix_rt::hw_self_node().ok()?;
        tairix_devmatch::matched_bind_key(record.match_keys(), BIND_KEYS).and_then(Engine::of)
    }

    fn main() -> i32 {
        let Some(engine) = engine() else {
            return bringup_failed("dma: the node matches neither engine's binding");
        };
        let caps = tairix_drv_dma_bcm2835::REQUIRED_CAPABILITIES
            .iter()
            .copied()
            .collect();
        let node = match Node::open(caps) {
            Ok(node) => node,
            Err(code) => return code,
        };
        let regs = match node.map() {
            Ok(regs) => regs,
            Err(code) => return code,
        };
        let reach: DmaReach = match engine {
            Engine::Legacy => legacy::REACH,
            Engine::Dma4 => dma4::REACH,
        };
        if let Err(code) = node.narrow(reach) {
            return code;
        }
        let store = node.store();
        match engine {
            Engine::Legacy => match Bcm2835Dma::new(&regs, &store) {
                Ok(engine) => serve(&node, engine, reach),
                Err(_) => bringup_failed(WINDOW_REFUSED),
            },
            Engine::Dma4 => match Bcm2711Dma::new(&regs, &store) {
                Ok(engine) => serve(&node, engine, reach),
                Err(_) => bringup_failed(WINDOW_REFUSED),
            },
        }
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}
