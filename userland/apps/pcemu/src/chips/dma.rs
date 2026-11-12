//! The Intel 8237A DMA controller and the PC's page registers.
//!
//! Devices move their data through [`Dma::to_memory`] and
//! [`Dma::from_memory`], which tell them at once when a transfer reaches
//! terminal count, as the end-of-process line does. The 8237's address
//! counter is 16 bits and the page register supplies the rest, so a transfer
//! wraps within its 64 KiB page exactly as software that crosses one
//! discovers on the hardware.
//!
//! The page registers are the PC and XT's 4 × 4-bit register file, written at
//! ports 80h–83h (aliased through 9Fh) and never read back. Each channel's
//! acknowledge line picks the register its transfer uses: channel 2 reads
//! 81h, channel 3 reads 82h, and channels 0 and 1 share 83h.

/// What a channel's mode register asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    /// A verify transfer: addresses and counts advance, no data moves.
    Verify,
    /// Device to memory.
    Write,
    /// Memory to device.
    Read,
}

#[derive(Clone, Copy, Debug, Default)]
struct Channel {
    base_address: u16,
    base_count: u16,
    address: u16,
    count: u16,
    mode: u8,
}

impl Channel {
    fn direction(&self) -> Direction {
        match (self.mode >> 2) & 3 {
            1 => Direction::Write,
            2 => Direction::Read,
            _ => Direction::Verify,
        }
    }

    fn auto_init(&self) -> bool {
        self.mode & 0x10 != 0
    }

    fn decrements(&self) -> bool {
        self.mode & 0x20 != 0
    }

    /// The address `steps` transfers further on, wrapping in the page.
    fn stepped(&self, from: u16, steps: u64) -> u16 {
        let steps = u16::try_from(steps % 0x1_0000).unwrap_or(0);
        if self.decrements() {
            from.wrapping_sub(steps)
        } else {
            from.wrapping_add(steps)
        }
    }
}

/// The page register each channel's acknowledge line selects.
const PAGE_OF_CHANNEL: [u8; 4] = [3, 3, 1, 2];
/// The page registers' width: A19–A16 of a 1 MiB bus.
const PAGE_BITS: u8 = 0x0F;
/// Where the page register file begins among the system's ports.
pub const PAGE_PORTS: u16 = 0x80;

/// The port writes that set channel `n` to move `count` + 1 bytes in `mode`
/// at physical `address`, the channel masked while they do, as firmware
/// sets one.
#[must_use]
pub fn channel_setup(n: usize, mode: u8, address: u32, count: u16) -> [(u16, u8); 9] {
    let [n, ..] = (n & 3).to_le_bytes();
    let [low, high, page, _] = address.to_le_bytes();
    let [count_low, count_high] = count.to_le_bytes();
    let address_port = u16::from(n) * 2;
    let page_port = PAGE_PORTS + u16::from(PAGE_OF_CHANNEL[usize::from(n)]);
    [
        (0x0A, 0x04 | n),
        (0x0C, 0),
        (0x0B, (mode & !3) | n),
        (address_port, low),
        (address_port, high),
        (page_port, page & PAGE_BITS),
        (address_port + 1, count_low),
        (address_port + 1, count_high),
        (0x0A, n),
    ]
}

/// What a device's transfer over a channel did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Moved {
    /// Bytes that moved.
    pub bytes: usize,
    /// The last of them reached terminal count.
    pub terminal: bool,
}

/// Memory as a DMA transfer reaches it.
pub trait DmaMemory {
    /// Reads a byte at a physical address.
    fn dma_read(&mut self, addr: u32) -> u8;
    /// Writes a byte at a physical address.
    fn dma_write(&mut self, addr: u32, value: u8);
}

/// The DMA controller.
#[derive(Clone, Debug)]
pub struct Dma {
    channels: [Channel; 4],
    flip_flop: bool,
    command: u8,
    status: u8,
    mask: u8,
    request: u8,
    temporary: u8,
    pages: [u8; 4],
    /// Device transfers since last asked, each a bus cycle the processor
    /// lost.
    transfers: u64,
}

impl Default for Dma {
    fn default() -> Self {
        Self::new()
    }
}

impl Dma {
    /// A controller as reset: every channel masked.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            channels: [Channel {
                base_address: 0,
                base_count: 0,
                address: 0,
                count: 0,
                mode: 0,
            }; 4],
            flip_flop: false,
            command: 0,
            status: 0,
            mask: 0x0F,
            request: 0,
            temporary: 0,
            pages: [0; 4],
            transfers: 0,
        }
    }

    fn physical(&self, n: usize) -> u32 {
        let page = self.pages[usize::from(PAGE_OF_CHANNEL[n])];
        (u32::from(page) << 16) | u32::from(self.channels[n].address)
    }

    /// A write at system port `port`: the controller's own from 00h, or
    /// the page register file's from 80h.
    pub fn write_port(&mut self, port: u16, value: u8) {
        let [low, _] = port.to_le_bytes();
        if port & PAGE_PORTS != 0 {
            self.write_page(low, value);
        } else {
            self.write(low, value);
        }
    }

    /// A write to the controller's ports 00h–0Fh (`port` 0–15).
    fn write(&mut self, port: u8, value: u8) {
        let port = port & 0x0F;
        if port < 8 {
            let channel = &mut self.channels[usize::from(port >> 1)];
            let high = self.flip_flop;
            self.flip_flop = !self.flip_flop;
            let set = |reg: &mut u16| {
                *reg = if high {
                    (*reg & 0x00FF) | (u16::from(value) << 8)
                } else {
                    (*reg & 0xFF00) | u16::from(value)
                };
            };
            if port & 1 == 0 {
                set(&mut channel.base_address);
                set(&mut channel.address);
            } else {
                set(&mut channel.base_count);
                set(&mut channel.count);
            }
            return;
        }
        match port {
            0x08 => self.command = value,
            0x09 => {
                let bit = 1 << (value & 3);
                if value & 0x04 != 0 {
                    self.request |= bit;
                } else {
                    self.request &= !bit;
                }
            }
            0x0A => {
                let bit = 1 << (value & 3);
                if value & 0x04 != 0 {
                    self.mask |= bit;
                } else {
                    self.mask &= !bit;
                }
            }
            0x0B => self.channels[usize::from(value & 3)].mode = value,
            0x0C => self.flip_flop = false,
            0x0D => {
                self.command = 0;
                self.status = 0;
                self.request = 0;
                self.temporary = 0;
                self.flip_flop = false;
                self.mask = 0x0F;
            }
            0x0E => self.mask = 0,
            _ => self.mask = value & 0x0F,
        }
    }

    /// A read of the controller's ports 00h–0Fh.
    pub fn read(&mut self, port: u8) -> u8 {
        let port = port & 0x0F;
        if port < 8 {
            let channel = &self.channels[usize::from(port >> 1)];
            let value = if port & 1 == 0 {
                channel.address
            } else {
                channel.count
            };
            let high = self.flip_flop;
            self.flip_flop = !self.flip_flop;
            return if high {
                crate::bits::hi(value)
            } else {
                crate::bits::lo(value)
            };
        }
        match port {
            0x08 => {
                let status = self.status | (self.request << 4);
                self.status &= 0xF0;
                status
            }
            0x0D => self.temporary,
            _ => 0xFF,
        }
    }

    /// A write to the page register file at ports 80h–9Fh (`port` 0–31).
    fn write_page(&mut self, port: u8, value: u8) {
        self.pages[usize::from(port & 3)] = value & PAGE_BITS;
    }

    /// Whether channel `n` would answer a request: the controller enabled
    /// and the channel unmasked.
    #[must_use]
    pub fn enabled(&self, n: usize) -> bool {
        n < 4 && self.command & 0x04 == 0 && self.mask & (1 << n) == 0
    }

    /// The device transfers made since last asked.
    pub fn take_transfers(&mut self) -> u64 {
        core::mem::take(&mut self.transfers)
    }

    /// Advances channel `n` by up to `transfers` transfers, as many as come
    /// before a terminal count that masks the channel. Returns the transfers
    /// made and whether a terminal count was reached.
    fn advance(&mut self, n: usize, transfers: u64) -> (u64, bool) {
        let mut channel = self.channels[n];
        let to_terminal = u64::from(channel.count) + 1;
        let made = if transfers < to_terminal {
            channel.address = channel.stepped(channel.address, transfers);
            channel.count -= u16::try_from(transfers).unwrap_or(0);
            transfers
        } else if channel.auto_init() {
            let beyond = (transfers - to_terminal) % (u64::from(channel.base_count) + 1);
            channel.address = channel.stepped(channel.base_address, beyond);
            channel.count = channel.base_count - u16::try_from(beyond).unwrap_or(0);
            transfers
        } else {
            channel.address = channel.stepped(channel.address, to_terminal);
            channel.count = 0xFFFF;
            self.mask |= 1 << n;
            to_terminal
        };
        self.channels[n] = channel;
        let ended = transfers >= to_terminal;
        if ended {
            self.status |= 1 << n;
            self.request &= !(1 << n);
        }
        (made, ended)
    }

    /// Moves device bytes into memory over channel `n`, until terminal count
    /// or the bytes run out. A verify transfer advances the channel without
    /// writing.
    pub fn to_memory<M: DmaMemory>(&mut self, n: usize, memory: &mut M, bytes: &[u8]) -> Moved {
        let mut moved = Moved {
            bytes: 0,
            terminal: false,
        };
        let direction = self.channels[n & 3].direction();
        if !self.enabled(n) || direction == Direction::Read {
            return moved;
        }
        for &byte in bytes {
            if direction == Direction::Write {
                memory.dma_write(self.physical(n), byte);
            }
            moved.bytes += 1;
            moved.terminal = self.advance(n, 1).1;
            if moved.terminal {
                break;
            }
        }
        self.transfers += moved.bytes as u64;
        moved
    }

    /// Moves memory bytes to a device over channel `n`, filling `out` until
    /// terminal count.
    pub fn from_memory<M: DmaMemory>(&mut self, n: usize, memory: &mut M, out: &mut [u8]) -> Moved {
        let mut moved = Moved {
            bytes: 0,
            terminal: false,
        };
        if !self.enabled(n) || self.channels[n & 3].direction() != Direction::Read {
            return moved;
        }
        for slot in out.iter_mut() {
            *slot = memory.dma_read(self.physical(n));
            moved.bytes += 1;
            moved.terminal = self.advance(n, 1).1;
            if moved.terminal {
                break;
            }
        }
        self.transfers += moved.bytes as u64;
        moved
    }

    /// Runs `transfers` cycles on channel `n` that no device takes data
    /// from, as DRAM refresh's reads are: returns how many the channel ran.
    pub fn idle_transfers(&mut self, n: usize, transfers: u64) -> u64 {
        if transfers == 0 || !self.enabled(n) {
            return 0;
        }
        self.advance(n, transfers).0
    }
}

/// Memory and channel set-up for devices' DMA in tests.
#[cfg(test)]
pub(crate) mod testing {
    use super::{channel_setup, Dma, DmaMemory};
    use alloc::vec;
    use alloc::vec::Vec;

    /// The whole 1 MiB address space as one array.
    pub struct Flat(pub Vec<u8>);

    impl Flat {
        pub fn new() -> Self {
            Self(vec![0; 1 << 20])
        }
    }

    impl DmaMemory for Flat {
        fn dma_read(&mut self, addr: u32) -> u8 {
            self.0[addr as usize]
        }
        fn dma_write(&mut self, addr: u32, value: u8) {
            self.0[addr as usize] = value;
        }
    }

    /// Channel `n` set through its ports to move `count` + 1 bytes in
    /// `mode` from physical `address`, and unmasked.
    pub fn program(dma: &mut Dma, n: usize, mode: u8, address: u32, count: u16) {
        for (port, value) in channel_setup(n, mode, address, count) {
            dma.write_port(port, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{program, Flat};
    use super::*;
    use alloc::vec;

    fn address(dma: &mut Dma, n: u8) -> u16 {
        dma.write(0x0C, 0);
        let low = dma.read(n * 2);
        crate::bits::word(low, dma.read(n * 2))
    }

    #[test]
    fn a_floppy_read_fills_memory_and_reaches_terminal_count() {
        let mut dma = Dma::new();
        let mut memory = Flat(vec![0; 1 << 20]);
        program(&mut dma, 2, 0x46, 0x12000, 3);
        let moved = dma.to_memory(2, &mut memory, &[1, 2, 3, 4, 5, 6]);
        assert_eq!(
            moved,
            Moved {
                bytes: 4,
                terminal: true
            },
            "count 3 moves four bytes"
        );
        assert_eq!(&memory.0[0x1_2000..0x1_2005], &[1, 2, 3, 4, 0]);
        assert_eq!(dma.read(0x08) & 0x04, 0x04, "the status register shows it");
        assert!(!dma.enabled(2), "a single-shot channel masks itself");
        assert_eq!(dma.take_transfers(), 4);
        assert_eq!(dma.to_memory(2, &mut memory, &[7]).bytes, 0);
        assert_eq!(dma.take_transfers(), 0);
    }

    #[test]
    fn the_address_wraps_inside_its_64k_page() {
        let mut dma = Dma::new();
        let mut memory = Flat(vec![0; 1 << 20]);
        program(&mut dma, 2, 0x46, 0x3FFFF, 1);
        dma.to_memory(2, &mut memory, &[0xAA, 0xBB]);
        assert_eq!(memory.0[0x3_FFFF], 0xAA);
        assert_eq!(memory.0[0x3_0000], 0xBB, "no carry into the page register");
        assert_eq!(memory.0[0x4_0000], 0);
    }

    #[test]
    fn channels_zero_and_one_share_a_page_register() {
        let mut dma = Dma::new();
        let mut memory = Flat(vec![0; 1 << 20]);
        program(&mut dma, 1, 0x44, 0x50010, 0);
        dma.write_page(0x07, 0x02);
        dma.to_memory(1, &mut memory, &[0x5A]);
        assert_eq!(memory.0[0x2_0010], 0x5A, "87h aliases 83h");
        dma.write_page(0x00, 0x09);
        program(&mut dma, 2, 0x44, 0x60020, 0);
        dma.to_memory(2, &mut memory, &[0xA5]);
        assert_eq!(
            memory.0[0x6_0020], 0xA5,
            "80h is a register no channel reads"
        );
    }

    #[test]
    fn auto_init_reloads_for_the_next_pass() {
        let mut dma = Dma::new();
        let mut memory = Flat((0..=255u8).cycle().take(1 << 20).collect());
        program(&mut dma, 1, 0x58, 0x10, 1);
        let mut out = [0u8; 4];
        let moved = dma.from_memory(1, &mut memory, &mut out[..2]);
        assert_eq!(
            moved,
            Moved {
                bytes: 2,
                terminal: true
            }
        );
        assert_eq!(&out[..2], &[0x10, 0x11]);
        assert!(dma.enabled(1), "auto-init stays unmasked");
        assert_eq!(dma.from_memory(1, &mut memory, &mut out[..2]).bytes, 2);
        assert_eq!(&out[..2], &[0x10, 0x11]);
    }

    #[test]
    fn refresh_cycles_advance_the_channel_in_one_step() {
        let mut dma = Dma::new();
        program(&mut dma, 0, 0x58, 0x0, 0xFFFF);
        assert_eq!(dma.idle_transfers(0, 70_000), 70_000);
        assert_eq!(dma.read(0x08) & 0x01, 0x01, "one pass of 65 536 completed");
        assert_eq!(dma.take_transfers(), 0, "refresh is no device's transfer");
        assert_eq!(u32::from(address(&mut dma, 0)), 70_000 - 65_536);
        dma.write(0x0A, 0x04);
        assert_eq!(
            dma.idle_transfers(0, 10),
            0,
            "a masked channel does not refresh"
        );
    }

    #[test]
    fn a_single_shot_idle_run_stops_at_terminal_count() {
        let mut dma = Dma::new();
        program(&mut dma, 3, 0x68, 0x100, 9);
        assert_eq!(dma.idle_transfers(3, 50), 10);
        assert_eq!(
            address(&mut dma, 3),
            0x0100 - 10,
            "a decrementing channel counts down"
        );
        assert_eq!(dma.idle_transfers(3, 50), 0);
    }

    #[test]
    fn registers_read_back_through_the_flip_flop() {
        let mut dma = Dma::new();
        program(&mut dma, 3, 0x44, 0xA1234, 0x0200);
        dma.write(0x0C, 0);
        assert_eq!((dma.read(6), dma.read(6)), (0x34, 0x12));
        assert_eq!((dma.read(7), dma.read(7)), (0x00, 0x02));
    }
}

crate::snapshot::state!(Channel {
    base_address,
    base_count,
    address,
    count,
    mode,
});

crate::snapshot::state!(Dma {
    channels,
    flip_flop,
    command,
    status,
    mask,
    request,
    temporary,
    pages,
    transfers,
});
