//! What the PC emulator's fuzz harnesses share.

use tairix_pcemu::chips::dma::DmaMemory;

/// The 1 MiB address space as one array, which a device's DMA reaches.
pub struct Flat(pub Vec<u8>);

impl DmaMemory for Flat {
    fn dma_read(&mut self, addr: u32) -> u8 {
        self.0[addr as usize & 0xF_FFFF]
    }
    fn dma_write(&mut self, addr: u32, value: u8) {
        self.0[addr as usize & 0xF_FFFF] = value;
    }
}
