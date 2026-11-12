//! Video memory as the CPU reaches it: four planes, the latches, and the
//! sequencer's and graphics controller's routing of every access.
//!
//! A CPU read loads all four latches from one plane address and returns one
//! plane's byte, or the colour comparison of all four. A CPU write goes to
//! the planes the map mask enables, through the write mode's data path: the
//! rotate, the set/reset, the logical function against the latches, and the
//! bit mask. Odd/even addressing gives even addresses to planes 0 and 2 and
//! odd ones to 1 and 3; chain-4 gives each address's low two bits a plane.

use alloc::vec::Vec;

/// The sequencer's memory mode bits.
mod memory_mode {
    pub const SEQUENTIAL: u8 = 0x04;
    pub const CHAIN_4: u8 = 0x08;
}

/// The graphics controller's mode register bits.
mod gc_mode {
    pub const WRITE_MODE: u8 = 0x03;
    pub const READ_COMPARE: u8 = 0x08;
    pub const HOST_ODD_EVEN: u8 = 0x10;
}

/// The registers that route a CPU access.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Routing {
    /// Sequencer register 2: the planes a write reaches.
    pub map_mask: u8,
    /// Sequencer register 4.
    pub memory_mode: u8,
    /// Graphics controller registers 0–8.
    pub gc: [u8; 9],
}

impl Routing {
    const fn chain4(&self) -> bool {
        self.memory_mode & memory_mode::CHAIN_4 != 0
    }
}

/// The four planes and their latches.
#[derive(Clone, Debug)]
pub struct Planes {
    memory: Vec<u8>,
    plane: usize,
    latches: [u8; 4],
}

/// Why the planes could not be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutOfMemory;

impl Planes {
    /// Four planes of `plane` bytes each, a power of two up to 64 KiB.
    pub fn new(plane: usize) -> Result<Self, OutOfMemory> {
        let plane = plane.next_power_of_two().clamp(0x4000, 0x1_0000);
        let mut memory = Vec::new();
        memory
            .try_reserve_exact(plane * 4)
            .map_err(|_| OutOfMemory)?;
        memory.resize(plane * 4, 0);
        Ok(Self {
            memory,
            plane,
            latches: [0; 4],
        })
    }

    fn at(&self, plane: usize, addr: usize) -> usize {
        (plane & 3) * self.plane + (addr & (self.plane - 1))
    }

    /// The four planes' bytes at plane address `addr`, as the display
    /// fetches them.
    #[must_use]
    pub fn fetch(&self, addr: u16) -> [u8; 4] {
        let addr = usize::from(addr);
        [0, 1, 2, 3].map(|p| self.memory[self.at(p, addr)])
    }

    /// The byte of plane `plane` at `addr`.
    #[must_use]
    pub fn byte(&self, plane: usize, addr: usize) -> u8 {
        self.memory[self.at(plane, addr)]
    }

    /// Plane address for a CPU offset in the window.
    fn plane_addr(routing: &Routing, offset: u32, odd_even: bool) -> usize {
        let offset = offset as usize;
        if routing.chain4() {
            offset & !3
        } else if odd_even {
            offset & !1
        } else {
            offset
        }
    }

    /// The plane bytes a read at `offset` reaches, and the read mode's
    /// answer from them.
    fn reached(&self, routing: &Routing, offset: u32) -> ([u8; 4], u8) {
        let gc = &routing.gc;
        let odd_even = gc[5] & gc_mode::HOST_ODD_EVEN != 0;
        let addr = Self::plane_addr(routing, offset, odd_even);
        let latches = [0, 1, 2, 3].map(|p| self.memory[self.at(p, addr)]);
        if gc[5] & gc_mode::READ_COMPARE != 0 {
            let mut same = 0xFF;
            for (p, latch) in latches.iter().enumerate() {
                if gc[7] & (1 << p) != 0 {
                    let colour = if gc[2] & (1 << p) != 0 { 0xFF } else { 0 };
                    same &= !(latch ^ colour);
                }
            }
            return (latches, same);
        }
        let plane = if routing.chain4() {
            offset as usize & 3
        } else if odd_even {
            usize::from(gc[4] & 2) | (offset as usize & 1)
        } else {
            usize::from(gc[4] & 3)
        };
        (latches, latches[plane])
    }

    /// A CPU read at `offset` into the window: the latches load, and the
    /// read mode answers.
    pub fn read(&mut self, routing: &Routing, offset: u32) -> u8 {
        let (latches, value) = self.reached(routing, offset);
        self.latches = latches;
        value
    }

    /// What a read at `offset` would return, leaving the latches alone.
    #[must_use]
    pub fn peek(&self, routing: &Routing, offset: u32) -> u8 {
        self.reached(routing, offset).1
    }

    /// A CPU write of `data` at `offset` into the window.
    pub fn write(&mut self, routing: &Routing, offset: u32, data: u8) {
        let gc = &routing.gc;
        let odd_even = routing.memory_mode & memory_mode::SEQUENTIAL == 0;
        let addr = Self::plane_addr(routing, offset, odd_even);
        let reached = if routing.chain4() {
            1 << (offset & 3)
        } else if odd_even {
            0b0101 << (offset & 1)
        } else {
            0b1111
        };
        let planes = reached & routing.map_mask;
        let rotated = data.rotate_right(u32::from(gc[3] & 7));
        let function = (gc[3] >> 3) & 3;
        let set_reset = |p: usize| if gc[0] & (1 << p) != 0 { 0xFF } else { 0 };
        for p in 0..4 {
            if planes & (1 << p) == 0 {
                continue;
            }
            let latch = self.latches[p];
            let (value, mask) = match gc[5] & gc_mode::WRITE_MODE {
                0 => (
                    if gc[1] & (1 << p) != 0 {
                        set_reset(p)
                    } else {
                        rotated
                    },
                    gc[8],
                ),
                1 => (latch, 0xFF),
                2 => (if data & (1 << p) != 0 { 0xFF } else { 0 }, gc[8]),
                _ => (set_reset(p), rotated & gc[8]),
            };
            let value = if gc[5] & gc_mode::WRITE_MODE == 1 {
                value
            } else {
                match function {
                    0 => value,
                    1 => value & latch,
                    2 => value | latch,
                    _ => value ^ latch,
                }
            };
            let at = self.at(p, addr);
            self.memory[at] = (value & mask) | (latch & !mask);
        }
    }

    /// Writes `bytes` straight into plane `plane` from `addr`, as firmware
    /// loading a character set does through the planar path.
    pub fn load(&mut self, plane: usize, addr: usize, bytes: &[u8]) {
        for (i, &byte) in bytes.iter().enumerate() {
            let at = self.at(plane, addr + i);
            self.memory[at] = byte;
        }
    }
}

crate::snapshot::state!(Planes { memory, latches } as built { plane });

#[cfg(test)]
mod tests {
    use super::*;

    fn planar() -> Routing {
        let mut gc = [0u8; 9];
        gc[8] = 0xFF;
        Routing {
            map_mask: 0x0F,
            memory_mode: memory_mode::SEQUENTIAL,
            gc,
        }
    }

    #[test]
    fn write_mode_zero_spreads_set_reset_and_keeps_masked_bits() {
        let mut planes = Planes::new(0x1_0000).unwrap();
        let mut routing = planar();
        routing.gc[0] = 0b0101;
        routing.gc[1] = 0b0111;
        routing.gc[8] = 0xF0;
        planes.read(&routing, 0);
        planes.write(&routing, 0, 0x3C);
        assert_eq!(
            planes.fetch(0),
            [0xF0, 0x00, 0xF0, 0x30],
            "planes 0-2 take set/reset, 3 the data"
        );
    }

    #[test]
    fn write_mode_one_copies_the_latches() {
        let mut planes = Planes::new(0x1_0000).unwrap();
        let mut routing = planar();
        planes.load(0, 10, &[0x12]);
        planes.load(3, 10, &[0x34]);
        planes.read(&routing, 10);
        routing.gc[5] = 1;
        planes.write(&routing, 20, 0xFF);
        assert_eq!(planes.fetch(20), [0x12, 0, 0, 0x34]);
    }

    #[test]
    fn write_mode_two_paints_a_colour_through_the_bit_mask() {
        let mut planes = Planes::new(0x1_0000).unwrap();
        let mut routing = planar();
        routing.gc[5] = 2;
        routing.gc[8] = 0x81;
        planes.read(&routing, 0);
        planes.write(&routing, 0, 0x0A);
        assert_eq!(planes.fetch(0), [0x00, 0x81, 0x00, 0x81]);
    }

    #[test]
    fn write_mode_three_masks_by_the_rotated_data() {
        let mut planes = Planes::new(0x1_0000).unwrap();
        let mut routing = planar();
        routing.gc[0] = 0x0F;
        routing.gc[3] = 0x01;
        routing.gc[5] = 3;
        planes.read(&routing, 0);
        planes.write(&routing, 0, 0x02);
        assert_eq!(
            planes.fetch(0),
            [0x01; 4],
            "the data rotated right by one is the mask"
        );
    }

    #[test]
    fn logical_functions_combine_with_the_latches() {
        let mut planes = Planes::new(0x1_0000).unwrap();
        let mut routing = planar();
        planes.load(0, 0, &[0b1100]);
        for (function, expected) in [(1u8, 0b1000u8), (2, 0b1110), (3, 0b0110)] {
            planes.load(0, 0, &[0b1100]);
            planes.read(&routing, 0);
            routing.gc[3] = function << 3;
            planes.write(&routing, 0, 0b1010);
            assert_eq!(planes.fetch(0)[0], expected, "function {function}");
        }
    }

    #[test]
    fn colour_compare_finds_matching_pixels() {
        let mut planes = Planes::new(0x1_0000).unwrap();
        let mut routing = planar();
        planes.load(0, 0, &[0b1111_0000]);
        planes.load(1, 0, &[0b1100_1100]);
        routing.gc[2] = 0b0011;
        routing.gc[7] = 0b0011;
        routing.gc[5] = gc_mode::READ_COMPARE;
        assert_eq!(
            planes.read(&routing, 0),
            0b1100_0000,
            "colour 3 in planes 0 and 1"
        );
    }

    #[test]
    fn odd_even_and_chain_4_route_by_address() {
        let mut planes = Planes::new(0x1_0000).unwrap();
        let mut gc = [0u8; 9];
        gc[5] = gc_mode::HOST_ODD_EVEN;
        gc[8] = 0xFF;
        let text = Routing {
            map_mask: 0x03,
            memory_mode: 0,
            gc,
        };
        planes.write(&text, 0x10, b'A');
        planes.write(&text, 0x11, 0x1F);
        assert_eq!(
            planes.fetch(0x10)[..2],
            [b'A', 0x1F],
            "char and attribute share a plane address"
        );
        assert_eq!(planes.read(&text, 0x11), 0x1F);
        let mut gc = [0u8; 9];
        gc[8] = 0xFF;
        let chained = Routing {
            map_mask: 0x0F,
            memory_mode: memory_mode::CHAIN_4 | memory_mode::SEQUENTIAL,
            gc,
        };
        for pixel in 0..8u32 {
            planes.write(&chained, 0x100 + pixel, u8::try_from(pixel).unwrap() + 1);
        }
        assert_eq!(planes.fetch(0x100), [1, 2, 3, 4]);
        assert_eq!(planes.fetch(0x104), [5, 6, 7, 8]);
        assert_eq!(planes.read(&chained, 0x106), 7);
    }

    #[test]
    fn small_planes_repeat() {
        let mut planes = Planes::new(0x4000).unwrap();
        let routing = planar();
        planes.write(&routing, 0x4005, 0x77);
        assert_eq!(
            planes.fetch(5),
            [0x77; 4],
            "16 KiB planes wrap their address"
        );
    }
}
