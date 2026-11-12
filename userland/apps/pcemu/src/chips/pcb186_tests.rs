use super::*;
use alloc::vec;
use alloc::vec::Vec;
use tairix_fuzzseed::Prng;

const I0CON: u8 = 0x38;
const TCUCON: u8 = 0x32;
const DMA0CON: u8 = 0x34;
const EOI: u8 = 0x22;
const INTSTS: u8 = 0x30;

/// INT0 as a PC-compatible board's firmware sets it: the 8259A cascaded,
/// level-triggered, special fully nested, unmasked at the lowest priority.
fn pc_cascade(pcb: &mut Pcb) {
    pcb.write(I0CON, con::CASCADE | con::LEVEL | con::NESTED | 7);
}

struct Ram {
    memory: Vec<u8>,
    ports: Vec<(u16, u8)>,
}

impl Ram {
    fn new() -> Self {
        Self {
            memory: vec![0; 1 << 20],
            ports: Vec::new(),
        }
    }
}

impl DmaBus for Ram {
    fn memory_read(&mut self, addr: u32) -> u8 {
        self.memory[addr as usize]
    }
    fn memory_write(&mut self, addr: u32, value: u8) {
        self.memory[addr as usize] = value;
    }
    fn io_read(&mut self, port: u16) -> u8 {
        crate::bits::lo(port)
    }
    fn io_write(&mut self, port: u16, value: u8) {
        self.ports.push((port, value));
    }
}

fn dma_channel(pcb: &mut Pcb, base: u8, source: u32, destination: u32, count: u16, control: u16) {
    pcb.write(base, low16(source));
    pcb.write(base + 2, high16(source));
    pcb.write(base + 4, low16(destination));
    pcb.write(base + 6, high16(destination));
    pcb.write(base + 8, count);
    pcb.write(base + 10, control | dcon::CHANGE);
}

#[test]
fn reset_puts_the_block_at_ff00h_in_io_space() {
    let mut pcb = Pcb::new(false);
    assert_eq!(pcb.space(), Space::Io(0xFF00));
    assert_eq!(pcb.read(0xFE), 0x20FF);
    assert_eq!(pcb.read(0xA0), 0xFFFB);
    assert!(!pcb.escape_traps());
    assert_eq!(pcb.read(0x28), 0x00FD, "every source masked");
    assert!(!pcb.intr());
}

#[test]
fn relocation_moves_the_block_and_sets_the_esc_trap() {
    let mut pcb = Pcb::new(false);
    assert_eq!(pcb.write(0xFE, 0x9123), Effect::Relocated);
    assert_eq!(pcb.space(), Space::Memory(0x1_2300));
    assert!(pcb.escape_traps());
    assert_eq!(pcb.read(0xFE), 0xB123, "bit 13 reads as one");
}

#[test]
fn the_cascaded_8259a_supplies_the_vector() {
    let mut pcb = Pcb::new(false);
    pc_cascade(&mut pcb);
    pcb.set_int_pin(0, true);
    assert!(pcb.intr());
    assert_eq!(pcb.acknowledge(), Some(Ack::Cascade(0)));
    pcb.set_int_pin(0, false);
    assert!(!pcb.intr(), "a level request follows the pin");
    pcb.set_int_pin(0, true);
    assert!(
        pcb.intr(),
        "special fully nested: INT0 interrupts its own service"
    );
    assert_eq!(
        pcb.read(0x2C),
        Source::Int0.bit(),
        "never ended at the 186, yet nothing is lost"
    );
}

#[test]
fn a_level_in_service_holds_off_lower_levels() {
    let mut pcb = Pcb::new(false);
    pcb.write(I0CON, 2);
    pcb.write(TCUCON, 5);
    pcb.write(
        0x56,
        tcon::ENABLE | tcon::INHIBIT | tcon::INTERRUPT | tcon::CONTINUOUS,
    );
    pcb.write(0x52, 10);
    pcb.set_int_pin(0, true);
    assert_eq!(pcb.acknowledge(), Some(Ack::Vector(12)));
    pcb.advance(10);
    assert!(!pcb.intr(), "timer at level 5 waits behind INT0 at level 2");
    pcb.write(EOI, 12);
    assert_eq!(pcb.acknowledge(), Some(Ack::Vector(8)));
}

#[test]
fn timers_vector_separately_through_one_source() {
    let mut pcb = Pcb::new(false);
    pcb.write(TCUCON, 0);
    for (control, compare) in [(0x56, 100), (0x5E, 150)] {
        pcb.write(control - 4, compare);
        pcb.write(
            control,
            tcon::ENABLE | tcon::INHIBIT | tcon::INTERRUPT | tcon::CONTINUOUS,
        );
    }
    pcb.advance(160);
    assert_eq!(pcb.read(INTSTS) & 7, 3);
    assert_eq!(pcb.acknowledge(), Some(Ack::Vector(8)));
    assert!(!pcb.intr(), "timer 1 shares the timer source's service");
    pcb.write(EOI, 8);
    assert_eq!(pcb.acknowledge(), Some(Ack::Vector(18)));
    pcb.write(EOI, 0x8000);
    assert_eq!(pcb.read(0x2C), 0);
}

#[test]
fn a_count_past_its_maximum_wraps_through_ffffh() {
    let mut pcb = Pcb::new(false);
    pcb.write(0x52, 100);
    pcb.write(0x50, 150);
    pcb.write(0x56, tcon::ENABLE | tcon::INHIBIT);
    pcb.advance(0x1_0000 - 150 + 99);
    assert_eq!(pcb.read(0x56) & tcon::MAX_COUNT, 0);
    pcb.advance(0x1_0000 - 150 + 100);
    assert_ne!(pcb.read(0x56) & tcon::MAX_COUNT, 0);
    assert_eq!(
        pcb.read(0x56) & tcon::ENABLE,
        0,
        "a single-shot timer stops at its maximum"
    );
}

#[test]
fn dual_mode_alternates_its_registers() {
    let mut pcb = Pcb::new(false);
    pcb.write(0x52, 3);
    pcb.write(0x54, 5);
    pcb.write(
        0x56,
        tcon::ENABLE | tcon::INHIBIT | tcon::ALTERNATE | tcon::CONTINUOUS,
    );
    pcb.advance(3);
    assert_ne!(
        pcb.read(0x56) & tcon::REGISTER_B,
        0,
        "register B in use after A's maximum"
    );
    pcb.advance(8);
    assert_eq!(pcb.read(0x56) & tcon::REGISTER_B, 0);
    pcb.advance(8 + 8 * 1_000_000 + 4);
    assert_eq!(pcb.read(0x50), 1, "a million whole cycles, then one into B");
}

#[test]
fn timer_two_prescales_timers_zero_and_one() {
    let mut pcb = Pcb::new(false);
    pcb.write(TCUCON, 0);
    pcb.write(0x62, 10);
    pcb.write(0x66, tcon::ENABLE | tcon::INHIBIT | tcon::CONTINUOUS);
    pcb.write(0x52, 3);
    pcb.write(
        0x56,
        tcon::ENABLE | tcon::INHIBIT | tcon::INTERRUPT | tcon::PRESCALE | tcon::CONTINUOUS,
    );
    assert_eq!(pcb.next_event(), Some(30));
    pcb.advance(29);
    assert!(!pcb.intr());
    pcb.advance(30);
    assert_eq!(pcb.acknowledge(), Some(Ack::Vector(8)));
}

#[test]
fn the_predicted_event_is_the_first_interrupt() {
    let mut rng = Prng::new(0x186);
    for _ in 0..400 {
        let mut pcb = Pcb::new(false);
        pcb.write(TCUCON, 0);
        let options = [
            0,
            tcon::CONTINUOUS,
            tcon::ALTERNATE,
            tcon::ALTERNATE | tcon::CONTINUOUS,
        ];
        for base in [0x50u8, 0x58, 0x60] {
            pcb.write(base, u16::from(rng.next_u8()));
            pcb.write(base + 2, u16::from(rng.next_u8() % 60));
            pcb.write(base + 4, u16::from(rng.next_u8() % 60));
            let prescale = if base < 0x60 && rng.next_u8() & 1 != 0 {
                tcon::PRESCALE
            } else {
                0
            };
            let control =
                tcon::ENABLE | tcon::INHIBIT | tcon::INTERRUPT | prescale | *rng.pick(&options);
            pcb.write(base + 6, if rng.below(4) == 0 { 0 } else { control });
        }
        // Timer requests stay set, so the first clock that has one is found
        // by bisection over single jumps.
        let horizon = 5_000_000u64;
        let fired = |t: u64| {
            let mut probe = pcb.clone();
            probe.advance(t);
            probe.read(INTSTS) & 7 != 0
        };
        let first = fired(horizon).then(|| {
            let (mut quiet, mut loud) = (0, horizon);
            while loud - quiet > 1 {
                let mid = quiet + (loud - quiet) / 2;
                if fired(mid) {
                    loud = mid;
                } else {
                    quiet = mid;
                }
            }
            loud
        });
        assert_eq!(pcb.next_event().filter(|&p| p <= horizon), first);
    }
}

#[test]
fn unsynchronised_dma_copies_and_ends_at_terminal_count() {
    let mut pcb = Pcb::new(false);
    let mut ram = Ram::new();
    ram.memory[0x1_0000..0x1_0004].copy_from_slice(&[1, 2, 3, 4]);
    pcb.write(DMA0CON, 0);
    let control =
        dcon::SRC_MEMORY | dcon::SRC_INC | dcon::DST_MEMORY | dcon::DST_INC | dcon::STOP_AT_COUNT;
    dma_channel(
        &mut pcb,
        0xC0,
        0x1_0000,
        0x2_0000,
        4,
        control | dcon::INTERRUPT | dcon::START,
    );
    pcb.run_dma(&mut ram);
    assert_eq!(&ram.memory[0x2_0000..0x2_0005], &[1, 2, 3, 4, 0]);
    assert_eq!(pcb.read(0xCA) & dcon::START, 0);
    assert_eq!(pcb.take_stolen(), 4 * 2 * BUS_CYCLE);
    assert_eq!(pcb.acknowledge(), Some(Ack::Vector(10)));
}

#[test]
fn a_pointer_both_incremented_and_decremented_holds_still() {
    let mut pcb = Pcb::new(true);
    let mut ram = Ram::new();
    let control =
        dcon::SRC_MEMORY | dcon::SRC_INC | dcon::SRC_DEC | dcon::STOP_AT_COUNT | dcon::WORD;
    dma_channel(&mut pcb, 0xD0, 0x0_0100, 0x0_0300, 3, control | dcon::START);
    ram.memory[0x100] = 0xAB;
    ram.memory[0x101] = 0xCD;
    pcb.run_dma(&mut ram);
    assert_eq!(
        ram.ports,
        [
            (0x300, 0xAB),
            (0x301, 0xCD),
            (0x300, 0xAB),
            (0x301, 0xCD),
            (0x300, 0xAB),
            (0x301, 0xCD)
        ]
    );
    assert_eq!(
        pcb.take_stolen(),
        3 * 4 * BUS_CYCLE,
        "an 80188 moves a word in two cycles each way"
    );
}

#[test]
fn the_dma_halt_stops_transfers_until_an_iret() {
    let mut pcb = Pcb::new(false);
    let mut ram = Ram::new();
    pcb.write(INTSTS, DMA_HALT);
    let control = dcon::SRC_MEMORY | dcon::DST_MEMORY | dcon::DST_INC | dcon::STOP_AT_COUNT;
    dma_channel(&mut pcb, 0xC0, 0, 0x100, 2, control | dcon::START);
    pcb.run_dma(&mut ram);
    assert_eq!(pcb.read(0xC8), 2);
    pcb.interrupt_returned();
    pcb.run_dma(&mut ram);
    assert_eq!(pcb.read(0xC8), 0);
}

#[test]
fn timer_two_paces_its_dma_one_transfer_per_maximum() {
    let mut pcb = Pcb::new(false);
    let mut ram = Ram::new();
    pcb.write(0x62, 25);
    pcb.write(0x66, tcon::ENABLE | tcon::INHIBIT | tcon::CONTINUOUS);
    let control =
        dcon::SRC_MEMORY | dcon::SRC_INC | dcon::DST_MEMORY | dcon::DST_INC | dcon::TIMER_REQUEST;
    dma_channel(&mut pcb, 0xC0, 0, 0x8000, 0, control | dcon::START);
    assert_eq!(pcb.next_event(), Some(25));
    pcb.advance(100);
    assert!(pcb.dma_pending());
    pcb.run_dma(&mut ram);
    assert_eq!(
        pcb.read(0xC8),
        0u16.wrapping_sub(4),
        "four maxima, four transfers"
    );
    assert!(!pcb.dma_pending());
}

#[test]
fn equal_channels_take_turns_and_priority_wins() {
    let mut pcb = Pcb::new(false);
    let mut ram = Ram::new();
    let control = dcon::DST_INC | dcon::STOP_AT_COUNT | dcon::START;
    dma_channel(&mut pcb, 0xC0, 0x10, 0x10, 2, control);
    dma_channel(&mut pcb, 0xD0, 0x20, 0x20, 2, control);
    pcb.run_dma(&mut ram);
    assert_eq!(
        ram.ports,
        [(0x10, 0x10), (0x20, 0x20), (0x11, 0x10), (0x21, 0x20)]
    );
    ram.ports.clear();
    dma_channel(&mut pcb, 0xC0, 0x10, 0x10, 2, control);
    dma_channel(&mut pcb, 0xD0, 0x20, 0x20, 2, control | dcon::PRIORITY);
    pcb.run_dma(&mut ram);
    assert_eq!(
        ram.ports,
        [(0x20, 0x20), (0x21, 0x20), (0x10, 0x10), (0x11, 0x10)]
    );
}

#[test]
fn chip_selects_add_their_wait_states() {
    let mut pcb = Pcb::new(false);
    assert_eq!(
        pcb.memory_waits(0xF_FC00).map(|w| w.with_board(0)),
        Some(3),
        "the top kilobyte from reset"
    );
    assert_eq!(pcb.memory_waits(0xF_FBFF), None);
    assert_eq!(pcb.write(0xA0, 0xF03C), Effect::ChipSelects);
    assert_eq!(
        pcb.memory_waits(0xF_0000).map(|w| w.with_board(2)),
        Some(0),
        "R2 set ignores READY"
    );
    pcb.write(0xA2, 0x1FF9);
    assert_eq!(
        pcb.memory_waits(0x1_FFFF).map(|w| w.with_board(2)),
        Some(2),
        "READY still counts"
    );
    assert_eq!(pcb.memory_waits(0x2_0000), None);
    pcb.write(0xA4, 0x007A);
    assert_eq!(pcb.io_waits(0x0400), None, "PCS needs MPCS too");
    pcb.write(0xA8, 0x81BB);
    assert_eq!(pcb.io_waits(0x0400).map(|w| w.with_board(1)), Some(2));
    assert_eq!(
        pcb.io_waits(0x0600).map(|w| w.with_board(1)),
        Some(3),
        "PCS4 takes MPCS's waits"
    );
    assert_eq!(
        pcb.io_waits(0x0780).map(|w| w.with_board(0)),
        None,
        "PCS7 does not exist"
    );
    pcb.write(0xA6, 0x4001);
    assert_eq!(
        pcb.memory_waits(0x4_1FFF).map(|w| w.with_board(0)),
        Some(1),
        "the mid-range block"
    );
    assert_eq!(pcb.memory_waits(0x4_4000), None);
}

#[test]
fn byte_writes_land_in_their_half() {
    let mut pcb = Pcb::new(true);
    pcb.write(0x52, 0x1234);
    pcb.write_byte(0x53, 0xAB);
    assert_eq!(pcb.read(0x52), 0xAB34);
    assert_eq!(pcb.read_byte(0x52), 0x34);
    pcb.write_byte(0x52, 0xCD);
    assert_eq!(pcb.read(0x52), 0xABCD);
}

#[test]
fn polling_acknowledges_and_poll_status_does_not() {
    let mut pcb = Pcb::new(false);
    pcb.write(0x3C, 3);
    pcb.set_int_pin(2, true);
    assert_eq!(pcb.read(0x26), 0x8000 | 0x0E);
    assert_eq!(pcb.read(0x2C), 0);
    assert_eq!(pcb.read(0x24), 0x8000 | 0x0E);
    assert_eq!(pcb.read(0x2C), Source::Int2.bit());
    assert_eq!(pcb.read(0x24), 0);
}

#[test]
fn only_the_dma_requests_are_writable() {
    let mut pcb = Pcb::new(false);
    pcb.set_int_pin(1, true);
    pcb.write(0x2E, 0x00FF);
    assert_eq!(
        pcb.read(0x2E),
        Source::Int1.bit() | Source::Dma0.bit() | Source::Dma1.bit()
    );
    pcb.write(0x2E, 0);
    assert_eq!(pcb.read(0x2E), Source::Int1.bit());
}

#[test]
fn slave_mode_hands_interrupts_to_the_8259a() {
    let mut pcb = Pcb::new(false);
    pc_cascade(&mut pcb);
    pcb.set_int_pin(0, true);
    pcb.write(0xFE, 0x60FF);
    assert!(pcb.slave_mode());
    assert!(!pcb.intr());
    assert_eq!(pcb.acknowledge(), None);
    pcb.write(0x20, 0x0040);
    assert_eq!(pcb.read(0x20), 0x0040);
    pcb.write(0xFE, 0x20FF);
    assert!(pcb.intr(), "the master registers kept their state");
}
