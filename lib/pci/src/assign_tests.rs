extern crate std;

use core::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::vec;
use std::vec::Vec;

use super::*;
use crate::config::FIRST_BAR;
use tairix_abi::driver::pci::BUS_MASTER_ENABLE;

/// One emulated function: what each register holds, which of its bits take
/// writes, which read fixed, and, for a bridge, what sits behind it.
struct Device {
    slot: (u8, u8),
    held: RefCell<BTreeMap<u8, u32>>,
    writable: BTreeMap<u8, u32>,
    fixed: BTreeMap<u8, u32>,
    children: Vec<Device>,
    /// Writes its command register took.
    commanded: Cell<usize>,
}

impl Device {
    fn new(slot: (u8, u8), header: u8) -> Self {
        let mut device = Self {
            slot,
            held: RefCell::new(BTreeMap::new()),
            writable: BTreeMap::new(),
            fixed: BTreeMap::new(),
            children: Vec::new(),
            commanded: Cell::new(0),
        };
        device.fixed.insert(0, 0x1041_1AF4);
        device.fixed.insert(HEADER_TYPE, u32::from(header) << 16);
        device.writable.insert(COMMAND_STATUS, 0xFFFF);
        device
    }

    fn endpoint(slot: (u8, u8)) -> Self {
        Self::new(slot, HEADER_DEVICE)
    }

    /// A bridge with an I/O window decoding 32 bits where `io`, and a 64-bit
    /// prefetchable window.
    fn bridge(slot: (u8, u8), io: bool, children: Vec<Device>) -> Self {
        let mut bridge = Self::new(slot, HEADER_BRIDGE);
        bridge.writable.insert(BUS_NUMBERS, 0x00FF_FFFF);
        if io {
            bridge.writable.insert(IO_BASE_LIMIT, 0xF0F0);
            bridge.fixed.insert(IO_BASE_LIMIT, 0x0101);
            bridge.writable.insert(IO_UPPER, u32::MAX);
        }
        bridge.writable.insert(MEMORY_BASE_LIMIT, 0xFFF0_FFF0);
        bridge.writable.insert(PREFETCH_BASE_LIMIT, 0xFFF0_FFF0);
        bridge.fixed.insert(PREFETCH_BASE_LIMIT, 0x0001_0001);
        bridge.writable.insert(PREFETCH_BASE_UPPER, u32::MAX);
        bridge.writable.insert(PREFETCH_LIMIT_UPPER, u32::MAX);
        bridge.children = children;
        bridge
    }

    /// Give BAR slot `slot` a memory BAR of `size`, 64-bit and prefetchable
    /// as asked.
    fn memory(mut self, slot: u8, size: u64, wide: bool, prefetch: bool) -> Self {
        let reg = FIRST_BAR + slot;
        let mask = !(size - 1);
        let kind = if wide { 0b100 } else { 0 } | if prefetch { 0b1000 } else { 0 };
        self.writable.insert(reg, low32(mask) & 0xFFFF_FFF0);
        self.fixed.insert(reg, kind);
        if wide {
            self.writable.insert(reg + 1, high32(mask));
        }
        self
    }

    fn io(mut self, slot: u8, size: u32) -> Self {
        let reg = FIRST_BAR + slot;
        self.writable.insert(reg, !(size - 1) & 0x0000_FFFC);
        self.fixed.insert(reg, 1);
        self
    }

    fn read(&self, register: u8) -> u32 {
        let held = self.held.borrow().get(&register).copied().unwrap_or(0);
        let writable = self.writable.get(&register).copied().unwrap_or(0);
        self.fixed.get(&register).copied().unwrap_or(0) | (held & writable)
    }

    fn write(&self, register: u8, value: u32) {
        if register == COMMAND_STATUS {
            self.commanded.set(self.commanded.get() + 1);
        }
        self.held.borrow_mut().insert(register, value);
    }

    /// The function at `(device, function)` on `bus`, as this bridge and
    /// those below it forward: a loop, so a deep chain costs the stack
    /// nothing.
    fn find(&self, bus: u8, slot: (u8, u8)) -> Option<&Device> {
        let mut bridge = self;
        loop {
            let [_, secondary, subordinate, _] = bridge.read(BUS_NUMBERS).to_le_bytes();
            if secondary == 0 || bus < secondary || bus > subordinate {
                return None;
            }
            if bus == secondary {
                return bridge.children.iter().find(|child| child.slot == slot);
            }
            bridge = bridge.children.iter().find(|child| {
                let [_, secondary, subordinate, _] = child.read(BUS_NUMBERS).to_le_bytes();
                child.header() == HEADER_BRIDGE
                    && secondary != 0
                    && (secondary..=subordinate).contains(&bus)
            })?;
        }
    }

    fn header(&self) -> u8 {
        self.read(HEADER_TYPE).to_le_bytes()[2] & !MULTIFUNCTION
    }

    fn bar(&self, slot: u8) -> u64 {
        let low = u64::from(self.read(FIRST_BAR + slot));
        let wide = low & 0b110 == 0b100;
        let high = if wide {
            u64::from(self.read(FIRST_BAR + 1 + slot))
        } else {
            0
        };
        (high << 32) | (low & !0xF)
    }
}

struct Machine {
    root: Vec<Device>,
}

impl Machine {
    fn find(&self, addr: ConfigAddress) -> Option<&Device> {
        let slot = (addr.device, addr.function);
        if addr.bus == 0 {
            return self.root.iter().find(|device| device.slot == slot);
        }
        self.root
            .iter()
            .find_map(|device| device.find(addr.bus, slot))
    }
}

impl ConfigSpace for Machine {
    fn read32(&self, addr: ConfigAddress) -> u32 {
        let register = u8::try_from(addr.register).unwrap_or(u8::MAX);
        self.find(addr)
            .map_or(u32::MAX, |device| device.read(register))
    }

    fn write32(&self, addr: ConfigAddress, value: u32) {
        if let (Some(device), Ok(register)) = (self.find(addr), u8::try_from(addr.register)) {
            device.write(register, value);
        }
    }
}

fn windows() -> Windows {
    Windows {
        io: Some(0..0x1_0000),
        memory: Some(0x1000_0000..0x2000_0000),
        wide: Some(0x80_0000_0000..0x81_0000_0000),
    }
}

fn pci(root: Vec<Device>) -> Pci<Machine> {
    Pci::new(Machine { root }, None)
}

fn command(device: &Device) -> u32 {
    device.read(COMMAND_STATUS) & 0xFFFF
}

fn bus_numbers(device: &Device) -> [u8; 3] {
    let [primary, secondary, subordinate, _] = device.read(BUS_NUMBERS).to_le_bytes();
    [primary, secondary, subordinate]
}

#[test]
fn root_bus_bars_are_placed_naturally_aligned_in_their_spaces() {
    let pci = pci(vec![
        Device::endpoint((1, 0))
            .memory(0, 0x4000, false, false)
            .memory(1, 0x1_0000, true, true)
            .io(3, 0x20),
        Device::endpoint((2, 0)).memory(0, 0x10_0000, true, false),
    ]);
    let assigned = pci.assign(0..=255, &windows()).unwrap();
    assert_eq!(
        assigned,
        Assigned {
            last_bus: 0,
            unplaced: 0
        }
    );
    let [a, b] = &pci.config_space().root[..] else {
        panic!("two devices");
    };
    let small = a.bar(0);
    assert!(windows().memory.unwrap().contains(&small) && small % 0x4000 == 0);
    let wide = a.bar(1);
    assert!(
        windows().wide.unwrap().contains(&wide) && wide % 0x1_0000 == 0,
        "{wide:#x}"
    );
    let port = a.bar(3);
    assert!(
        (IO_FLOOR..0x1_0000).contains(&port) && port % 0x20 == 0,
        "{port:#x}"
    );
    let other = b.bar(0);
    assert!(
        windows().wide.unwrap().contains(&other),
        "a root 64-bit BAR goes high"
    );
    assert!(
        other + 0x10_0000 <= wide || wide + 0x1_0000 <= other,
        "no overlap"
    );
    assert_eq!(command(a), IO_SPACE_ENABLE | MEMORY_SPACE_ENABLE);
    assert_eq!(
        command(b),
        MEMORY_SPACE_ENABLE,
        "never a bus master of its own"
    );
}

#[test]
fn bridges_are_numbered_depth_first_and_their_windows_hold_their_subtrees() {
    let deep = Device::endpoint((0, 0)).memory(0, 0x1000, false, false);
    let nested = Device::bridge((1, 0), true, vec![deep]);
    let near = Device::endpoint((0, 0))
        .memory(0, 0x20_0000, true, true)
        .memory(2, 0x1000, false, false)
        .io(3, 0x100);
    let port = Device::bridge((1, 0), true, vec![near, nested]);
    let pci = pci(vec![port, Device::bridge((2, 0), true, vec![])]);
    let assigned = pci.assign(0..=255, &windows()).unwrap();
    assert_eq!(
        assigned,
        Assigned {
            last_bus: 3,
            unplaced: 0
        }
    );
    let machine = pci.config_space();
    let (port, empty) = (&machine.root[0], &machine.root[1]);
    assert_eq!(bus_numbers(port), [0, 1, 2]);
    assert_eq!(bus_numbers(&port.children[1]), [1, 2, 2]);
    assert_eq!(
        bus_numbers(empty),
        [0, 3, 3],
        "an empty port still gets its bus"
    );
    let near = &port.children[0];
    let prefetch = near.bar(0);
    assert!(windows().wide.unwrap().contains(&prefetch), "{prefetch:#x}");
    let upper = u64::from(port.read(PREFETCH_BASE_UPPER)) << 32;
    let prefetch_base = upper | (u64::from(port.read(PREFETCH_BASE_LIMIT) & 0xFFF0) << 16);
    assert!(
        prefetch_base <= prefetch,
        "the bridge forwards its prefetchable BAR"
    );
    let memory = port.read(MEMORY_BASE_LIMIT);
    let memory_window =
        (u64::from(memory & 0xFFF0) << 16)..((u64::from(memory) & 0xFFF0_0000) + MEMORY_GRANULE);
    assert!(memory_window.contains(&near.bar(2)));
    assert!(memory_window.contains(&port.children[1].children[0].bar(0)));
    let nested_memory = port.children[1].read(MEMORY_BASE_LIMIT);
    assert_eq!(
        u64::from(nested_memory & 0xFFF0) << 16,
        port.children[1].children[0].bar(0) & !(MEMORY_GRANULE - 1)
    );
    for bridge in [port, &port.children[1]] {
        assert_eq!(
            command(bridge) & (MEMORY_SPACE_ENABLE | BUS_MASTER_ENABLE),
            MEMORY_SPACE_ENABLE,
            "a bridge forwards DMA only once a function below it is granted it"
        );
    }
    assert_eq!(command(near), IO_SPACE_ENABLE | MEMORY_SPACE_ENABLE);
    assert_eq!(
        command(empty) & (IO_SPACE_ENABLE | MEMORY_SPACE_ENABLE),
        0,
        "nothing below it to decode for"
    );
    assert_eq!(empty.read(MEMORY_BASE_LIMIT), MEMORY_WINDOW_OFF);
}

#[test]
fn a_bar_with_no_room_leaves_its_function_decoding_nothing() {
    let pci = pci(vec![Device::endpoint((1, 0))
        .memory(0, 0x1000, false, false)
        .memory(1, 0x4000_0000, false, false)]);
    let assigned = pci.assign(0..=255, &windows()).unwrap();
    assert_eq!(assigned.unplaced, 1);
    let device = &pci.config_space().root[0];
    assert_eq!(device.bar(1), 0);
    assert_eq!(
        command(device) & MEMORY_SPACE_ENABLE,
        0,
        "BAR 1 would decode at zero"
    );
}

/// A bridge firmware left mastering forwards nothing until a grant opens
/// it, whatever firmware set.
#[test]
fn a_bridge_firmware_left_mastering_is_closed() {
    let behind = Device::endpoint((0, 0)).memory(0, 0x1000, false, false);
    let bridge = Device::bridge((1, 0), false, vec![behind]);
    bridge.write(COMMAND_STATUS, BUS_MASTER_ENABLE);
    let pci = pci(vec![bridge]);
    pci.assign(0..=255, &windows()).unwrap();
    assert_eq!(command(&pci.config_space().root[0]) & BUS_MASTER_ENABLE, 0);
}

#[test]
fn a_bridge_past_the_last_bus_forwards_nothing() {
    let behind = Device::endpoint((0, 0)).memory(0, 0x1000, false, false);
    let pci = pci(vec![Device::bridge((1, 0), true, vec![behind])]);
    let assigned = pci.assign(0..=0, &windows()).unwrap();
    assert_eq!(
        assigned,
        Assigned {
            last_bus: 0,
            unplaced: 0
        }
    );
    let bridge = &pci.config_space().root[0];
    assert_eq!(bus_numbers(bridge), [0, 0, 0]);
    assert_eq!(bridge.children[0].bar(0), 0, "unreachable, untouched");
}

#[test]
fn a_bridge_claiming_a_64_bit_bar_in_its_last_slot_keeps_its_bus_numbers() {
    let behind = Device::endpoint((0, 0)).memory(0, 0x1000, false, false);
    let mut bridge = Device::bridge((1, 0), true, vec![behind]);
    bridge.fixed.insert(FIRST_BAR + 1, 0b100);
    bridge.writable.insert(FIRST_BAR + 1, 0xFFFF_F000);
    let pci = pci(vec![bridge]);
    let assigned = pci.assign(0..=255, &windows()).unwrap();
    assert_eq!(assigned.last_bus, 1);
    let bridge = &pci.config_space().root[0];
    assert_eq!(
        bus_numbers(bridge),
        [0, 1, 1],
        "its upper half would be these"
    );
    assert_eq!(
        bridge.read(FIRST_BAR + 1) & !0xF,
        0,
        "never sized, never placed"
    );
    assert_ne!(
        bridge.children[0].bar(0),
        0,
        "the bus behind it still reached"
    );
}

#[test]
fn a_space_firmware_left_decoding_is_cleared_where_a_bar_found_no_room() {
    let device = Device::endpoint((1, 0))
        .memory(0, 0x1000, false, false)
        .memory(1, 0x4000_0000, false, false);
    device.write(COMMAND_STATUS, IO_SPACE_ENABLE | MEMORY_SPACE_ENABLE);
    let pci = pci(vec![device]);
    assert_eq!(pci.assign(0..=255, &windows()).unwrap().unplaced, 1);
    assert_eq!(
        command(&pci.config_space().root[0]),
        IO_SPACE_ENABLE,
        "BAR 1 would decode where firmware left it; no I/O BAR, so I/O is as found"
    );
}

#[test]
fn a_chain_of_bridges_as_deep_as_the_buses_is_walked_on_a_small_stack() {
    let mut chain = Device::endpoint((0, 0)).memory(0, 0x1000, false, false);
    for _ in 0..255 {
        chain = Device::bridge((0, 0), false, vec![chain]);
    }
    let pci = pci(vec![chain]);
    let (assigned, pci) = std::thread::Builder::new()
        .stack_size(64 << 10)
        .spawn(move || (pci.assign(0..=255, &windows()), pci))
        .unwrap()
        .join()
        .unwrap();
    let assigned = assigned.unwrap();
    assert_eq!(assigned.last_bus, 255);
    let mut bridge = &pci.config_space().root[0];
    for depth in 1..=255u8 {
        assert_eq!(bus_numbers(bridge), [depth - 1, depth, 255]);
        bridge = &bridge.children[0];
    }
    assert_ne!(bridge.bar(0), 0, "the function at the bottom is placed");
}

#[test]
fn io_behind_a_bridge_with_no_io_window_is_unplaced() {
    let behind = Device::endpoint((0, 0))
        .io(0, 0x20)
        .memory(1, 0x1000, false, false);
    let pci = pci(vec![Device::bridge((1, 0), false, vec![behind])]);
    let assigned = pci.assign(0..=255, &windows()).unwrap();
    assert_eq!(assigned.unplaced, 1);
    let device = &pci.config_space().root[0].children[0];
    assert_eq!(command(device), MEMORY_SPACE_ENABLE);
}

#[test]
fn a_host_without_a_wide_window_places_64_bit_bars_low() {
    let pci = pci(vec![
        Device::endpoint((1, 0)).memory(0, 0x1_0000, true, true)
    ]);
    let mut low = windows();
    low.wide = None;
    assert_eq!(pci.assign(0..=255, &low).unwrap().unplaced, 0);
    let bar = pci.config_space().root[0].bar(0);
    assert!(low.memory.unwrap().contains(&bar));
}

#[test]
fn a_window_is_the_aligned_sum_of_what_it_holds() {
    let request = |size| Request {
        space: Space::Memory,
        size,
        align: size,
        owner: 0,
        item: Item::Bar(0),
    };
    let requests = [request(0x10_0000), request(0x20_0000), request(0x1000)];
    assert_eq!(
        span_of(requests.iter(), MEMORY_GRANULE),
        Ok(Span::Of {
            size: 0x40_0000,
            align: 0x20_0000
        })
    );
    assert_eq!(
        span_of([request(0x1000)].iter(), MEMORY_GRANULE),
        Ok(Span::Of {
            size: MEMORY_GRANULE,
            align: MEMORY_GRANULE
        })
    );
    assert_eq!(span_of([].iter(), MEMORY_GRANULE), Ok(Span::Empty));
    assert_eq!(
        span_of([request(1 << 63), request(1 << 63)].iter(), MEMORY_GRANULE),
        Ok(Span::Overflowed),
        "more than an address names"
    );
    assert_eq!(align_up(0x1001, 0x1000), Some(0x2000));
    assert_eq!(align_up(u64::MAX, 0x1000), None);
}

/// QEMU `virt`'s root bus with no 64-bit window in reach: two virtio
/// functions, each a 32-bit BAR1 and a 64-bit prefetchable BAR4, share the
/// 32-bit window without overlapping.
#[test]
fn wide_bars_share_the_low_window_when_no_wide_window_is_reached() {
    let virtio = |device| {
        Device::endpoint((device, 0))
            .memory(1, 0x1000, false, false)
            .memory(4, 0x4000, true, true)
    };
    let pci = pci(vec![Device::endpoint((0, 0)), virtio(1), virtio(5)]);
    let low = Windows {
        io: Some(0..0x1_0000),
        memory: Some(0x1000_0000..0x3EFF_0000),
        wide: None,
    };
    let assigned = pci.assign(0..=255, &low).unwrap();
    assert_eq!(
        assigned,
        Assigned {
            last_bus: 0,
            unplaced: 0
        }
    );
    let [_, a, b] = &pci.config_space().root[..] else {
        panic!("three functions");
    };
    let mut spans: Vec<(u64, u64)> = Vec::new();
    for (device, slot, size) in [
        (a, 1, 0x1000),
        (a, 4, 0x4000),
        (b, 1, 0x1000),
        (b, 4, 0x4000),
    ] {
        let base = device.bar(slot);
        assert!(low.memory.clone().unwrap().contains(&base), "{base:#x}");
        assert_eq!(base % size, 0, "{base:#x} aligned");
        for &(other, other_size) in &spans {
            assert!(
                base + size <= other || other + other_size <= base,
                "{base:#x} overlaps {other:#x}"
            );
        }
        spans.push((base, size));
    }
    assert_eq!(command(a) & MEMORY_SPACE_ENABLE, MEMORY_SPACE_ENABLE);
    assert_eq!(command(b) & MEMORY_SPACE_ENABLE, MEMORY_SPACE_ENABLE);
}

/// A segment decodes its root functions' memory BARs, a root bridge's own
/// among them, and what its root bridges forward, below which every deeper
/// BAR lies; an I/O BAR and an empty bridge's closed windows decode no
/// memory.
#[test]
fn a_segment_decodes_its_root_bars_and_what_its_bridges_forward() {
    use crate::topology::{Confinement, PciTopology};
    let deep = Device::endpoint((0, 0)).memory(0, 0x1000, false, false);
    let port = Device::bridge((1, 0), true, vec![deep]).memory(1, 0x2000, false, false);
    let root = Device::endpoint((2, 0))
        .memory(0, 0x4000, false, false)
        .io(3, 0x20);
    let pci = pci(vec![port, root, Device::bridge((3, 0), true, vec![])]);
    pci.assign(0..=255, &windows()).unwrap();
    let topology = pci.topology(Confinement::Leave, &|_| false).unwrap();
    // The empty bridge decodes, so sizing it would turn its forwarding off.
    pci.config_space().root[2].write(COMMAND_STATUS, MEMORY_SPACE_ENABLE);
    let commanded = |at: usize| pci.config_space().root[at].commanded.get();
    let before = (commanded(0), commanded(2));
    let decoded = pci.decoded_windows(&topology).unwrap();
    assert!(commanded(0) > before.0, "the port's own BAR was sized");
    assert_eq!(
        commanded(2),
        before.1,
        "a bridge with no BAR placed never stops forwarding"
    );
    let machine = pci.config_space();
    let deep = machine.root[0].children[0].bar(0);
    let own = machine.root[0].bar(1);
    let bar = machine.root[1].bar(0);
    assert_eq!(decoded.len(), 3, "{decoded:#x?}");
    assert!(
        decoded.iter().any(|window| window.contains(&deep)),
        "below the port"
    );
    assert!(decoded.contains(&(own..own + 0x2000)), "the port's own BAR");
    assert!(decoded.contains(&(bar..bar + 0x4000)), "a root BAR");
}

/// A bridge's own BAR placed above 4 GiB, its base's low half zero, is still
/// found placed and its window decoded.
#[test]
fn a_bridge_bar_placed_above_four_gib_is_decoded() {
    use crate::topology::{Confinement, PciTopology};
    let port = Device::bridge((1, 0), true, vec![]).memory(0, 0x1_0000_0000, true, true);
    let pci = pci(vec![port]);
    pci.assign(0..=255, &windows()).unwrap();
    let topology = pci.topology(Confinement::Leave, &|_| false).unwrap();
    let bar = pci.config_space().root[0].bar(0);
    assert_eq!(bar & 0xFFFF_FFFF, 0, "{bar:#x} has no low half");
    let decoded = pci.decoded_windows(&topology).unwrap();
    assert!(
        decoded.contains(&(bar..bar + 0x1_0000_0000)),
        "{decoded:#x?}"
    );
}
