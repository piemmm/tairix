use super::*;
use crate::time::{NANOS_PER_MILLISECOND as MS, NANOS_PER_SECOND};

/// No sound card.
const QUIET: SoundConfig = SoundConfig {
    stream_rate: crate::machine::STREAM_RATE,
    card: None,
};

fn board(cpu: CpuModel) -> Board {
    let clock = if cpu.family() == Family::Intel80186 {
        CpuClock::I80186_8
    } else {
        CpuClock::PC
    };
    Board::new(&BoardConfig {
        cpu,
        clock,
        wiring: Wiring::Xt,
        memory_kib: 640,
        video: [None, None],
        floppy: None,
        fixed: None,
        sound: QUIET,
        ports: PortsConfig::default(),
        ems: None,
        coprocessor: false,
    })
    .unwrap()
}

#[test]
fn memory_outside_the_modelled_range_is_refused() {
    for kib in [0, 32, 100, 704] {
        let config = BoardConfig {
            cpu: CpuModel::I8088,
            clock: CpuClock::PC,
            wiring: Wiring::Pc,
            memory_kib: kib,
            video: [None, None],
            floppy: None,
            fixed: None,
            sound: QUIET,
            ports: PortsConfig::default(),
            ems: None,
            coprocessor: false,
        };
        assert_eq!(
            Board::new(&config).err(),
            Some(ConfigError::Memory),
            "{kib} KiB"
        );
    }
}

#[test]
fn memory_above_conventional_floats_and_rom_holds() {
    let mut b = board(CpuModel::I8088);
    b.write(0x9_FFFF, 0x12, 0);
    assert_eq!(b.read(0x9_FFFF, 0), 0x12);
    b.write(0xA_0000, 0x12, 0);
    assert_eq!(b.read(0xA_0000, 0), OPEN_BUS);
    b.map_rom(0xF_0000, &[0xCC], false).unwrap();
    b.write(0xF_0000, 0, 0);
    assert_eq!(b.read(0xF_0000, 0), 0xCC);
    assert_eq!(b.peek(0xF_0000), 0xCC);
    assert_eq!(b.take_wait_states(0), 0, "planar memory has no wait states");
}

#[test]
fn ten_bit_decode_repeats_every_kilobyte() {
    let mut b = board(CpuModel::I8088);
    b.output(0x20, 0x13, 0);
    b.output(0x421, 0x08, 0);
    b.output(0x821, 0x09, 0);
    b.output(0xC21, 0x5A, 0);
    assert_eq!(
        b.input(0x21, 0),
        0x5A,
        "the mask written through three aliases"
    );
    assert_eq!(
        b.input(0x3F, 0),
        0x5A,
        "and within the planar's 32-port block"
    );
    assert_eq!(b.take_wait_states(0), 6, "one wait state per I/O cycle");
}

#[test]
fn the_page_registers_are_write_only() {
    let mut b = board(CpuModel::I8088);
    b.output(0x81, 0x05, 0);
    assert_eq!(b.input(0x81, 0), OPEN_BUS);
    assert_eq!(b.input(0xA0, 0), OPEN_BUS, "nor is the NMI mask readable");
}

#[test]
fn the_timer_interrupt_reaches_the_cpu_pin_at_its_edge() {
    let mut b = board(CpuModel::I8088);
    // The timer first: its output rises with the mode word, an edge the
    // 8259A's initialisation then forgets.
    for (port, value) in [
        (0x43, 0x34),
        (0x40, 100),
        (0x40, 0),
        (0x20, 0x13),
        (0x21, 0x08),
        (0x21, 0x09),
        (0x21, 0xFE),
    ] {
        b.output(port, value, 0);
    }
    assert!(!b.intr());
    let edge = b.next_event().unwrap();
    assert_eq!(
        edge,
        Hertz::PIT.time_of(1 + 100),
        "mode 2's pulse ends a period after the load"
    );
    b.advance_to(edge - 1);
    assert!(!b.intr());
    b.advance_to(edge);
    assert!(b.intr());
    assert_eq!(b.acknowledge(), 0x08);
    assert!(!b.intr());
}

#[test]
fn refresh_takes_the_bus_once_per_timer_period() {
    let mut b = board(CpuModel::I8088);
    for (port, value) in [
        (0x43, 0x54),
        (0x41, 18),
        (0x0B, 0x58),
        (0x01, 0xFF),
        (0x01, 0xFF),
        (0x0A, 0x00),
    ] {
        b.output(port, value, 0);
    }
    b.take_wait_states(0);
    b.advance_to(NANOS_PER_SECOND);
    let refreshes = (Hertz::PIT.ticks_at(NANOS_PER_SECOND) - 1) / 18;
    assert_eq!(u64::from(b.take_wait_states(0)), refreshes * DMA_CYCLES);
    b.output(0x0A, 0x04, 0);
    b.take_wait_states(0);
    b.advance_to(2 * NANOS_PER_SECOND);
    assert_eq!(
        b.take_wait_states(0),
        0,
        "a masked channel does not refresh"
    );
}

#[test]
fn port_b_clears_the_keyboard_and_its_interrupt() {
    let mut b = board(CpuModel::I8088);
    for (port, value) in [
        (0x63, 0x99),
        (0x61, 0x40),
        (0x20, 0x13),
        (0x21, 0x08),
        (0x21, 0x09),
        (0x21, 0xFD),
    ] {
        b.output(port, value, 0);
    }
    b.key(Scan::plain(0x1E), true);
    let arrival = b.next_event().unwrap();
    b.advance_to(arrival);
    assert!(b.intr());
    assert_eq!(b.input(0x60, b.cycles_at(arrival)), 0x1E);
    assert_eq!(b.acknowledge(), 0x09);
    let at = b.cycles_at(arrival);
    b.output(0x61, 0xC0, at);
    b.output(0x61, 0x40, at);
    assert_eq!(b.input(0x60, at), 0, "the shift register is clear");
}

#[test]
fn the_80186_block_answers_word_io_inside_the_cpu() {
    let mut b = board(CpuModel::I80186);
    b.output_word(0xFF52, 0x1234, 0);
    assert_eq!(b.input_word(0xFF52, 0), 0x1234);
    assert_eq!(b.input(0xFF53, 0), 0x12);
    assert_eq!(b.take_wait_states(0), 0, "no external bus cycle");
    assert_eq!(
        b.input(0x352, 0),
        OPEN_BUS,
        "the block does not alias onto the I/O channel"
    );
}

#[test]
fn the_upper_chip_select_slows_the_top_kilobyte_from_reset() {
    let mut b = board(CpuModel::I80186);
    b.read(0xF_FBFF, 0);
    assert_eq!(b.take_wait_states(0), 0);
    b.read(0xF_FC00, 0);
    assert_eq!(b.take_wait_states(0), 3);
    b.output_word(0xFFA0, 0xF03C, 0);
    b.read(0xF_FC00, 0);
    assert_eq!(
        b.take_wait_states(0),
        0,
        "firmware's first act on the 80186"
    );
}

#[test]
fn relocated_into_memory_the_block_overlays_its_page() {
    let mut b = board(CpuModel::I80186);
    b.output_word(0xFFFE, 0x1100, 0);
    b.write(0x1_0052, 0x34, 0);
    b.write(0x1_0053, 0x12, 0);
    assert_eq!((b.read(0x1_0052, 0), b.read(0x1_0053, 0)), (0x34, 0x12));
    assert_eq!(b.peek(0x1_0053), 0x12);
    assert_eq!(b.read(0x1_0100, 0), 0, "the rest of the page is RAM");
    assert_eq!(
        b.input_word(0xFF52, 0),
        0xFFFF,
        "and the I/O window is gone"
    );
}

#[test]
fn the_8259a_interrupts_through_int0() {
    let mut b = board(CpuModel::I80186);
    for (port, value) in [
        (0x43, 0x34),
        (0x40, 10),
        (0x40, 0),
        (0x20, 0x13),
        (0x21, 0x08),
        (0x21, 0x09),
        (0x21, 0xFE),
    ] {
        b.output(port, value, 0);
    }
    let edge = b.next_event().unwrap();
    b.advance_to(edge);
    assert!(!b.intr(), "INT0 is masked until firmware sets it up");
    b.output_word(0xFF38, 0x0077, b.cycles_at(edge));
    assert!(b.intr());
    assert_eq!(
        b.acknowledge(),
        0x08,
        "the 8259A's vector, through cascade mode"
    );
}

#[test]
fn the_80186_timers_run_on_their_own_clock() {
    let mut b = board(CpuModel::I80186);
    b.output_word(0xFF32, 0x0000, 0);
    b.output_word(0xFF52, 2000, 0);
    b.output_word(0xFF56, 0xE001, 0);
    let event = b.next_event().unwrap();
    assert_eq!(
        event,
        CpuClock::I80186_8.divided::<4>().time_of(2000),
        "a millisecond at 2 MHz"
    );
    let sixteen = Hertz::whole(16_000_000).and_then(CpuClock::new).unwrap();
    b.set_cpu_rate(sixteen, 0);
    assert_eq!(
        b.next_event(),
        Some(event),
        "turbo leaves the peripherals' clock alone"
    );
    b.advance_to(event);
    assert_eq!(b.acknowledge(), 8);
}

/// A board with an XT's disk adapters, driven port by port as a CPU would.
struct Driver {
    board: Board,
    at: u64,
}

impl Driver {
    fn new(video: Option<Adapter>) -> Self {
        Self::with(&BoardConfig {
            cpu: CpuModel::I8088,
            clock: CpuClock::PC,
            wiring: Wiring::Xt,
            memory_kib: 640,
            video: [video, None],
            floppy: Some(FloppyAdapter {
                interface: crate::storage::fdc::Interface::Standard,
                drives: [
                    Some(crate::storage::fdc::DriveKind::Dd525),
                    None,
                    None,
                    None,
                ],
            }),
            fixed: Some(FixedAdapter {
                recording: Recording::Mfm,
                jumpers: 0x0C,
            }),
            sound: QUIET,
            ports: PortsConfig::default(),
            ems: None,
            coprocessor: false,
        })
    }

    fn with(config: &BoardConfig) -> Self {
        let board = Board::new(config).unwrap();
        let mut driver = Self { board, at: 0 };
        // The 8259A, edge-triggered, with only `irq` unmasked by each test.
        for (port, value) in [(0x20, 0x13), (0x21, 0x08), (0x21, 0x09), (0x21, 0xFF)] {
            driver.out(port, value);
        }
        driver
    }

    fn out(&mut self, port: u16, value: u8) {
        self.board.output(port, value, self.at);
    }

    fn input(&mut self, port: u16) -> u8 {
        self.board.input(port, self.at)
    }

    /// Runs the board's events until the CPU's interrupt pin rises, and
    /// takes the interrupt.
    fn interrupt(&mut self) -> u8 {
        while !self.board.intr() {
            let t = self.board.next_event().expect("an interrupt to come");
            self.board.advance_to(t);
            self.at = self.board.cycles_at(t);
        }
        let vector = self.board.acknowledge();
        self.out(0x20, 0x20);
        vector
    }

    fn after(&mut self, t: Nanos) {
        self.board.advance_to(t);
        self.at = self.board.cycles_at(t);
    }

    /// Channel `n` set to move `bytes` at physical `address` in `mode`.
    fn dma(&mut self, n: u8, mode: u8, address: u32, bytes: u16) {
        let page = [0x87, 0x83, 0x81, 0x82][usize::from(n)];
        let [_, page_bits, high, low] = address.to_be_bytes();
        let [count_high, count_low] = (bytes - 1).to_be_bytes();
        for (port, value) in [
            (0x0A, 0x04 | n),
            (0x0B, mode | n),
            (0x0C, 0),
            (u16::from(n) * 2, low),
            (u16::from(n) * 2, high),
            (u16::from(n) * 2 + 1, count_low),
            (u16::from(n) * 2 + 1, count_high),
            (page, page_bits),
            (0x0A, n),
        ] {
            self.out(port, value);
        }
    }
}

fn marked(sectors: usize) -> crate::storage::MemoryDisk {
    let mut bytes = alloc::vec![0u8; sectors * crate::storage::SECTOR];
    for (lba, sector) in bytes.chunks_mut(crate::storage::SECTOR).enumerate() {
        sector[..2].copy_from_slice(&u16::try_from(lba).unwrap().to_le_bytes());
    }
    crate::storage::MemoryDisk::new(bytes, false).unwrap()
}

#[test]
fn a_floppy_sector_reaches_a_cards_memory_over_dma() {
    let mut d = Driver::new(Some(Adapter::Cga(crate::video::monitor::Monitor::Colour)));
    let floppy = crate::storage::floppy::Floppy::raw(Box::new(marked(720))).unwrap();
    assert!(d.board.insert_floppy(0, floppy).unwrap().is_none());
    d.out(0x21, 0xBF);
    assert_eq!(d.board.lights(), Lights::default(), "every motor off");
    d.out(0x3F2, 0x1C);
    assert_eq!(d.board.lights().floppy, 0b01, "drive A's motor runs");
    assert_eq!(d.interrupt(), 0x0E, "the reset's poll on IRQ 6");
    for _ in 0..4 {
        d.out(0x3F5, 0x08);
        d.after(d.board.time_of(d.at) + MS);
        d.input(0x3F5);
        d.input(0x3F5);
    }
    for byte in [0x03, 0xDF, 0x02] {
        d.out(0x3F5, byte);
    }
    d.after(500 * MS);
    d.dma(2, 0x46, 0xB_8000, 512);
    for byte in [0x46, 0x00, 0, 0, 6, 2, 9, 0x2A, 0xFF] {
        d.out(0x3F5, byte);
    }
    d.board.take_wait_states(d.at);
    assert_eq!(d.interrupt(), 0x0E, "the result on IRQ 6");
    assert_eq!(
        d.board.take_wait_states(d.at),
        512 * 5 + 1,
        "each DMA cycle held the CPU off the bus, as did the EOI's wait state"
    );
    assert_eq!(d.input(0x3F4) & 0xD0, 0xD0);
    assert_eq!(d.input(0x3F5), 0x00);
    assert_eq!(d.board.peek(0xB_8000), 5, "the CGA took the sector's bytes");
}

#[test]
fn a_fixed_disk_sector_arrives_with_irq_5() {
    let mut d = Driver::new(None);
    let kind = crate::storage::hdc::DiskType {
        cylinders: 64,
        heads: 2,
        track_to_track: 3 * MS,
        average_seek: 85 * MS,
    };
    let format = crate::storage::hdc::LowLevel {
        interleave: 3,
        bad_tracks: alloc::vec::Vec::new(),
    };
    d.board
        .attach_fixed(0, kind, Box::new(marked(64 * 2 * 17)), format)
        .unwrap();
    assert_eq!(d.input(0x322), 0x0C, "the drive-type jumpers");
    d.out(0x21, 0xDF);
    d.out(0x323, 0x03);
    d.dma(3, 0x44, 0x1_2000, 512);
    d.out(0x322, 0);
    for byte in [0x08, 0x00, 0x05, 0x00, 0x01, 0x05] {
        assert_eq!(d.input(0x321) & 0x0F, 0x0D);
        d.out(0x320, byte);
    }
    assert_eq!(
        d.board.lights().fixed,
        0b01,
        "the read is under way on drive 0"
    );
    assert_eq!(d.interrupt(), 0x0D);
    assert_eq!(d.board.lights().fixed, 0, "and done");
    assert_eq!(d.input(0x320), 0x00, "a clean completion");
    assert_eq!(d.board.peek(0x1_2000), 5, "sector 5 of the first track");
    assert_eq!(d.board.fixed_format(0).map(|f| f.interleave), Some(3));
}

#[test]
fn the_adapters_answer_only_where_fitted() {
    let mut b = board(CpuModel::I8088);
    assert_eq!(b.input(0x3F4, 0), OPEN_BUS);
    assert_eq!(b.input(0x321, 0), OPEN_BUS);
    let floppy = crate::storage::floppy::Floppy::raw(Box::new(marked(720))).unwrap();
    assert!(
        b.insert_floppy(0, floppy).is_err(),
        "no adapter takes the disk"
    );
    let mut d = Driver::new(None);
    assert_eq!(
        d.input(0x3F4),
        0x00,
        "the floppy controller powers up in reset"
    );
    assert_eq!(
        d.input(0x3F3),
        OPEN_BUS,
        "the adapter decodes three of its eight ports"
    );
}

#[test]
fn the_switches_count_the_floppy_drives() {
    let mut config = crate::machine::Template::IbmPc.config();
    config.video = [None, None];
    let mut b = Board::new(&config).unwrap();
    b.output(0x61, 0x80, 0);
    assert_eq!(b.input(0x60, 0) >> 6, 1, "two drives");
}

/// The XT with `card`, and `parallel` in place of its printer.
fn sounding(card: Option<SoundCard>, parallel: Option<ParallelPort>) -> Driver {
    let mut config = crate::machine::Template::IbmXt.config();
    config.sound = SoundConfig { card, ..QUIET };
    config.ports.parallel = [parallel, None, None];
    Driver::with(&config)
}

/// The board's audio over the last span, its left channel.
fn heard(driver: &mut Driver) -> Vec<f32> {
    driver.board.render_audio();
    let mut out = alloc::vec![0.0f32; 2 * 60_000];
    let frames = driver.board.take_audio(&mut out);
    out[..2 * frames]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|frame| frame[0])
        .collect()
}

const US: Nanos = 1_000;

#[test]
fn the_adlib_passes_its_detection_at_388h() {
    let mut d = sounding(Some(SoundCard::AdLib), None);
    let opl = |d: &mut Driver, register: u8, value: u8| {
        d.out(0x388, register);
        d.out(0x389, value);
    };
    opl(&mut d, 0x04, 0x60);
    opl(&mut d, 0x04, 0x80);
    assert_eq!(d.input(0x388) & 0xE0, 0);
    opl(&mut d, 0x02, 0xFF);
    opl(&mut d, 0x04, 0x21);
    d.after(200 * US);
    assert_eq!(d.input(0x388) & 0xE0, 0xC0);
    assert_eq!(d.input(0x228), OPEN_BUS, "no Sound Blaster");
}

#[test]
fn the_sound_blaster_resets_and_interrupts_on_its_line() {
    let mut d = sounding(
        Some(SoundCard::Blaster {
            base: 0x220,
            irq: 7,
        }),
        None,
    );
    d.out(0x21, 0x7F);
    d.out(0x226, 1);
    d.after(5 * US);
    d.out(0x226, 0);
    d.after(200 * US);
    assert_ne!(d.input(0x22E) & 0x80, 0, "a byte to read");
    assert_eq!(d.input(0x22A), 0xAA);
    let mut t = 300 * US;
    for byte in [0x40, 0xD3, 0x80, 9, 0] {
        d.out(0x22C, byte);
        t += 20 * US;
        d.after(t);
    }
    assert_eq!(d.interrupt(), 0x0F, "IRQ 7 after its ten ticks of silence");
    assert_eq!(
        d.input(0x388) & 0x1F,
        0x06,
        "its YM3812 at the AdLib's port too"
    );
}

#[test]
fn the_speaker_sounds_timer_2_through_port_61h() {
    let mut d = sounding(None, None);
    // The 8255 as the POST sets it: port B an output.
    d.out(0x63, 0x99);
    d.out(0x61, 0x00);
    d.out(0x43, 0xB6);
    d.out(0x42, 0xA9);
    d.out(0x42, 0x04);
    let b = d.input(0x61);
    d.out(0x61, b | 0x03);
    d.after(NANOS_PER_SECOND);
    let tone = heard(&mut d);
    let rate = crate::sound::testing::cycles(&tone[4_800..]) * 48_000 / (tone.len() - 4_800);
    assert!((995..=1_005).contains(&rate), "{rate} Hz");
    d.out(0x61, b & !0x03);
    d.after(2 * NANOS_PER_SECOND);
    let quiet = heard(&mut d);
    assert!(
        quiet[24_000..].iter().all(|x| x.abs() < 1e-3),
        "off with port 61h"
    );
}

#[test]
fn a_sound_source_on_lpt1_plays_what_it_latched() {
    let port = ParallelPort {
        base: 0x378,
        device: Device::SoundSource,
    };
    let mut d = sounding(None, Some(port));
    for _ in 0..16 {
        d.out(0x378, 0xF0);
        d.out(0x37A, 0x0C);
        d.out(0x37A, 0x04);
    }
    assert_ne!(d.input(0x379) & 0x40, 0, "its FIFO full");
    d.after(10_000 * US);
    let played = heard(&mut d);
    assert!(played.iter().any(|x| x.abs() > 0.1));
    assert_eq!(d.input(0x379) & 0x40, 0, "and drained");
}

#[test]
fn sound_settings_the_card_cannot_take_are_refused() {
    let refused = |card, parallel| {
        let mut config = crate::machine::Template::IbmXt.config();
        config.sound = SoundConfig { card, ..QUIET };
        config.ports.parallel = [parallel, None, None];
        Board::new(&config).err()
    };
    let blaster = |base, irq| Some(SoundCard::Blaster { base, irq });
    assert_eq!(
        refused(blaster(0x220, 5), None),
        Some(ConfigError::SoundConflict),
        "the XT disk's IRQ 5"
    );
    assert_eq!(
        refused(blaster(0x230, 7), None),
        Some(ConfigError::SoundConflict),
        "no such jumper"
    );
    assert_eq!(
        refused(blaster(0x220, 4), None),
        Some(ConfigError::SoundConflict)
    );
    let lpt = |base| {
        Some(ParallelPort {
            base,
            device: Device::Covox,
        })
    };
    assert_eq!(refused(None, lpt(0x3C0)), Some(ConfigError::PortConflict));
    assert_eq!(refused(blaster(0x240, 3), lpt(0x278)), None);
    assert_eq!(
        refused(None, lpt(0x3BC)),
        None,
        "as a monochrome adapter's port"
    );
    let mut vga = crate::machine::Template::TurboXt.config();
    vga.sound.card = blaster(0x220, 2);
    assert_eq!(
        Board::new(&vga).err(),
        Some(ConfigError::SoundConflict),
        "the VGA's IRQ 2"
    );
    vga.sound = SoundConfig {
        stream_rate: 0,
        ..QUIET
    };
    assert_eq!(Board::new(&vga).err(), Some(ConfigError::StreamRate));
}

#[test]
fn the_ports_answer_at_their_bases_and_overlaps_are_refused() {
    let mut d = Driver::with(&crate::machine::Template::TurboXt.config());
    assert_eq!(d.input(0x3FA), 0x01, "COM1 has no interrupt pending");
    assert_eq!(d.input(0x2FA), 0x01, "COM2");
    assert_eq!(d.input(0x3EA), OPEN_BUS, "no COM3");
    assert_eq!(d.input(0x379), 0xDF, "a ready printer on LPT1");
    assert_eq!(d.input(0x201), 0xF0, "the game port, its one-shots idle");
    assert_eq!(
        d.input(0x2CF) & 0x0F,
        0x04,
        "the clock card in 24-hour time"
    );
    let mut twice = crate::machine::Template::IbmXt.config();
    twice.ports.parallel[1] = twice.ports.parallel[0];
    assert_eq!(Board::new(&twice).err(), Some(ConfigError::PortConflict));
}

#[test]
fn a_serial_mouse_interrupts_on_irq_4() {
    let mut d = Driver::with(&crate::machine::Template::IbmXt.config());
    for (port, value) in [
        (0x3FB, 0x80),
        (0x3F8, 96),
        (0x3F9, 0),
        (0x3FB, 0x02),
        (0x3F9, 0x01),
    ] {
        d.out(port, value);
    }
    d.out(0x3FC, 0x0B);
    d.out(0x21, 0xEF);
    assert_eq!(d.interrupt(), 0x0C);
    assert_eq!(d.input(0x3F8), b'M');
    assert!(d.board.mouse(2, 0, 1));
    assert_eq!(d.interrupt(), 0x0C);
    assert_eq!(
        d.input(0x3F8),
        0x60,
        "the left button, and the packet's first byte"
    );
}

#[test]
fn com1_and_com3_share_irq_4_while_only_one_drives_it() {
    let mut config = crate::machine::Template::IbmXt.config();
    config.ports.serial[2] = Some(SerialPort {
        model: Model::Ns16450,
        device: Plug::Nothing,
    });
    let mut d = Driver::with(&config);
    d.out(0x21, 0xEF);
    d.out(0x3F9, 0x02);
    assert!(
        !d.board.intr(),
        "COM1 asks, but its OUT2 keeps it off the line"
    );
    d.out(0x3E9, 0x02);
    d.out(0x3EC, 0x08);
    assert_eq!(d.interrupt(), 0x0C, "COM3's transmitter, through its OUT2");
    assert_eq!(
        d.input(0x3EA),
        0x02,
        "identified, so COM3 lets the line fall"
    );
    d.out(0x3FC, 0x08);
    assert_eq!(d.interrupt(), 0x0C, "now COM1's");
}

#[test]
fn the_printer_on_lpt1_interrupts_on_irq_7_when_enabled() {
    let mut d = Driver::with(&crate::machine::Template::IbmXt.config());
    d.out(0x21, 0x7F);
    d.out(0x37A, 0x1C);
    d.out(0x378, b'X');
    d.out(0x37A, 0x1D);
    d.out(0x37A, 0x1C);
    assert_eq!(d.interrupt(), 0x0F, "its Ack");
    let mut out = [0; 2];
    assert_eq!(d.board.take_printed(0, &mut out), 1);
    assert_eq!(out[0], b'X');
    assert_eq!(d.board.take_printed(1, &mut out), 0, "no second adapter");
}

#[test]
fn the_clock_card_keeps_the_time_the_front_end_sets() {
    let mut d = Driver::with(&crate::machine::Template::IbmXt.config());
    let now = CivilTime {
        year: 2026,
        month: 11,
        day: 10,
        hour: 9,
        minute: 30,
        second: 0,
    };
    assert!(d.board.set_clock(&now));
    d.after(61 * NANOS_PER_SECOND);
    assert_eq!(
        d.board.clock(),
        Some(CivilTime {
            minute: 31,
            second: 1,
            ..now
        })
    );
    let mut bare = crate::machine::Template::IbmXt.config();
    bare.ports.clock = false;
    let mut b = Board::new(&bare).unwrap();
    assert!(!b.set_clock(&now), "no card");
    assert_eq!(b.clock(), None);
}

#[test]
fn the_game_port_reads_the_sticks_the_front_end_sets() {
    let mut d = Driver::with(&crate::machine::Template::IbmXt.config());
    assert!(d.board.set_sticks(Sticks {
        axes: [Some(0), Some(0), None, None],
        buttons: 0b0001,
    }));
    d.out(0x201, 0);
    assert_eq!(
        d.input(0x201),
        0xEF,
        "every axis firing, A's first button held"
    );
    d.after(100_000);
    assert_eq!(
        d.input(0x201) & 0x0F,
        0x0C,
        "A's axes fell; B's are unplugged"
    );
}

#[test]
fn the_expanded_memory_frame_shows_the_page_each_register_selects() {
    let mut d = Driver::with(&crate::machine::Template::TurboXt.config());
    assert_eq!(
        d.board.read(0xD_0000, d.at),
        OPEN_BUS,
        "nothing shown at power-up"
    );
    d.out(0x258, 5);
    d.out(0x259, 0x80);
    d.out(0x25E, 5);
    d.out(0x25F, 0x80);
    d.board.write(0xD_0010, 0x42, d.at);
    assert_eq!(
        d.board.read(0xD_C010, d.at),
        0x42,
        "one page in two windows"
    );
    let store = d.board.expanded_store().unwrap();
    assert_eq!(store.len(), 129 * 0x4000, "128 pages and the manager's");
    assert_eq!(
        store[5 * 0x4000 + 0x10],
        0x42,
        "page 5, written through the frame"
    );
    d.out(0x259, 0x00);
    assert_eq!(d.board.read(0xD_0010, d.at), OPEN_BUS);
    assert_eq!(d.input(0x25E), 5);
}

#[test]
fn expanded_memory_the_board_cannot_have_is_refused() {
    let refused = |port, frame, pages| {
        let mut config = crate::machine::Template::TurboXt.config();
        config.ems = Some(EmsConfig { port, frame, pages });
        Board::new(&config).err()
    };
    assert_eq!(refused(0x258, 0xD_0000, 0), Some(ConfigError::Expanded));
    assert_eq!(refused(0x258, 0xD_0000, 513), Some(ConfigError::Expanded));
    assert_eq!(refused(0x258, 0xC_0000, 4), Some(ConfigError::Expanded));
    let mut com4 = crate::machine::Template::TurboXt.config();
    com4.ports.serial[3] = com4.ports.serial[1];
    com4.ems = Some(EmsConfig {
        port: 0x2E8,
        frame: 0xD_0000,
        pages: 4,
    });
    assert_eq!(
        Board::new(&com4).err(),
        Some(ConfigError::PortConflict),
        "COM4's ports"
    );
    assert_eq!(
        refused(0x2F0, 0xE_0000, 4),
        Some(ConfigError::PortConflict),
        "no such jumper"
    );
    assert_eq!(refused(0x2E8, 0xE_0000, 512), None, "free without a COM4");
}
