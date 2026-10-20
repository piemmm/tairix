//! This port's half of the console the kernel hosts (`tairix_fbcon::port`).
//!
//! The port lights no boot surface of its own: a display driver hosts the
//! console on a block of kernel memory it scans out, which the kernel reaches
//! through its direct map. The port issues no cache maintenance for written
//! pixels, so a device that does not snoop the caches is refused the console.

use tairix_fbcon::port::{CoherentPort, LinearDirectMap, LinearMap};

use crate::irqmask::PortIrqControl;

/// The direct map the console reaches its surface through.
pub struct DirectMap;

impl LinearDirectMap for DirectMap {
    type Irq = PortIrqControl;

    fn map() -> LinearMap {
        LinearMap {
            base: crate::paging::PHYSMAP_VMA_BASE,
            len: crate::paging::physmap_bytes(),
        }
    }
}

/// This port's half of the hosted console.
pub type FramebufferPort = CoherentPort<DirectMap>;
