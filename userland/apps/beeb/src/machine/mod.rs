//! The BBC Micro Model B: its memory map, its devices, and the clock that
//! keeps them in step with the CPU.
//!
//! Time is a count of 2 MHz cycles. Every CPU bus cycle advances it, and an
//! access to the 1 MHz bus stretches to the next coincident edge of the two
//! clocks. A device is brought up to the current cycle before the CPU
//! touches it, and a device whose outputs change of their own accord states
//! the next cycle they will, so the interrupt lines are exact at every cycle
//! the CPU samples them.

pub mod acia;
pub mod adc;
pub mod cassette;
pub mod crtc;
pub mod fdc;
pub mod keyboard;
pub mod serial;
pub mod sound;
pub mod teletext;
pub mod ula;
pub mod via;
pub mod video;

#[cfg(test)]
mod acia_tests;
#[cfg(test)]
mod crtc_tests;
#[cfg(test)]
mod snapshot_tests;
#[cfg(test)]
mod teletext_tests;
#[cfg(test)]
mod tests;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_crypto::{Fingerprint, FINGERPRINT_LEN};

use crate::cpu::{Bus, Cpu, CpuState};
use crate::media::disc::Disc;
use crate::media::tape::Tape;
use crate::snapshot::{self, Reader, SnapshotError, Writer};
use acia::Acia;
use adc::Adc;
use cassette::{Cassette, Event as CassetteEvent};
use fdc::{Fdc, PhysicalDrive};
use keyboard::{Key, Keyboard};
use serial::SerialUla;
use sound::{Sn76489, CYCLES_PER_UPDATE};
use via::Via;
use video::{Signal, Video};

/// Cycles a second: the 2 MHz the CPU runs at and the machine's time counts.
pub const CLOCK: u64 = 2_000_000;

/// The 1 MHz tick a cycle falls in: the clock the 1 MHz bus's devices count.
#[must_use]
pub const fn tick(cycle: u64) -> u64 {
    cycle >> 1
}

/// The cycle 1 MHz tick `tick` begins in.
#[must_use]
pub const fn tick_start(tick: u64) -> u64 {
    tick << 1
}

/// Where the sideways window starts: RAM fills the addresses below it.
pub const SIDEWAYS_BASE: u16 = 0x8000;

/// Where the OS ROM starts, and the sideways window ends.
pub const OS_BASE: u16 = 0xC000;

/// Bytes of RAM.
pub const RAM_SIZE: usize = SIDEWAYS_BASE as usize;

/// Bytes in a ROM: the OS, or a sideways bank, each filling its window.
pub const ROM_SIZE: usize = (OS_BASE - SIDEWAYS_BASE) as usize;

/// The bits of ROM select that choose a slot.
const ROMSEL_MASK: u8 = 0x0F;

/// Sideways slots ROM select can page in.
pub const SLOTS: usize = ROMSEL_MASK as usize + 1;

/// The memory-mapped I/O pages: FRED, JIM and SHEILA.
pub const IO_BASE: u16 = 0xFC00;
/// The first byte past the I/O pages.
pub const IO_END: u16 = 0xFF00;

/// The I/O pages, and in SHEILA each device's block: its registers repeat
/// through the block, which runs from its first address to its `_END`.
pub mod sheila {
    /// FRED, the first 1 MHz expansion page.
    pub const FRED: u16 = super::IO_BASE;
    /// JIM, the second, through which expansion memory is paged.
    pub const JIM: u16 = FRED + 0x100;
    /// SHEILA, the page the machine's own devices answer in.
    pub const SHEILA: u16 = JIM + 0x100;
    /// 6845 address register.
    pub const CRTC_ADDRESS: u16 = SHEILA;
    /// 6845 data register.
    pub const CRTC_DATA: u16 = CRTC_ADDRESS + 1;
    /// The 6845's block.
    pub const CRTC_END: u16 = CRTC_ADDRESS + 0x07;
    /// 6850 control (write) and status (read).
    pub const ACIA_CONTROL: u16 = 0xFE08;
    /// 6850 data.
    pub const ACIA_DATA: u16 = ACIA_CONTROL + 1;
    /// The 6850's block.
    pub const ACIA_END: u16 = ACIA_CONTROL + 0x07;
    /// Serial ULA control.
    pub const SERIAL_ULA: u16 = 0xFE10;
    /// The serial ULA's block.
    pub const SERIAL_ULA_END: u16 = SERIAL_ULA + 0x0F;
    /// Video ULA control.
    pub const VIDEO_CONTROL: u16 = 0xFE20;
    /// Video ULA palette.
    pub const VIDEO_PALETTE: u16 = VIDEO_CONTROL + 1;
    /// The video ULA's block.
    pub const VIDEO_ULA_END: u16 = VIDEO_CONTROL + 0x0F;
    /// Sideways ROM select.
    pub const ROMSEL: u16 = 0xFE30;
    /// ROM select's block.
    pub const ROMSEL_END: u16 = ROMSEL + 0x0F;
    /// System VIA.
    pub const SYSTEM_VIA: u16 = 0xFE40;
    /// The System VIA's block.
    pub const SYSTEM_VIA_END: u16 = SYSTEM_VIA + 0x1F;
    /// User VIA.
    pub const USER_VIA: u16 = 0xFE60;
    /// The User VIA's block.
    pub const USER_VIA_END: u16 = USER_VIA + 0x1F;
    /// 8271 command/status.
    pub const FDC: u16 = 0xFE80;
    /// The 8271's block.
    pub const FDC_END: u16 = FDC + 0x1F;
    /// `µPD7002` status/control.
    pub const ADC: u16 = 0xFEC0;
    /// The `µPD7002`'s block.
    pub const ADC_END: u16 = ADC + 0x1F;
    /// The Tube's block, at 2 MHz with nothing fitted.
    pub const TUBE: u16 = 0xFEE0;
}

/// System VIA addressable latch bits (IC32).
pub mod latch {
    /// Sound generator write enable, active low.
    pub const SOUND_WE: u8 = 0x01;
    /// Speech processor read select.
    pub const SPEECH_READ: u8 = 0x02;
    /// Speech processor write select.
    pub const SPEECH_WRITE: u8 = 0x04;
    /// Keyboard: low lets the CPU scan it, high lets it autoscan.
    pub const KEYBOARD_AUTOSCAN: u8 = 0x08;
    /// The two hardware-scroll screen-size bits.
    pub const SCREEN_SIZE: u8 = 0x30;
    /// CAPS LOCK LED, lit when low.
    pub const CAPS_LOCK_LED: u8 = 0x40;
    /// SHIFT LOCK LED, lit when low.
    pub const SHIFT_LOCK_LED: u8 = 0x80;
}

/// System VIA port B: the two joystick fire buttons, active low.
const FIRE_BUTTONS: u8 = 0x30;
/// System VIA port B: the speech processor's lines, high with none fitted.
const SPEECH_LINES: u8 = 0xC0;

/// A ROM image, shared rather than copied, and the fingerprint a snapshot
/// records it by, taken once.
#[derive(Clone, Debug)]
struct Rom {
    image: Rc<[u8]>,
    fingerprint: [u8; FINGERPRINT_LEN],
}

impl Rom {
    fn new(image: Box<[u8]>) -> Self {
        let fingerprint = rom_fingerprint(&image);
        Self {
            image: image.into(),
            fingerprint,
        }
    }
}

/// What a sideways slot holds.
#[derive(Clone, Debug, Default)]
enum Slot {
    /// An empty socket: reads see the floating bus.
    #[default]
    Empty,
    /// A ROM image.
    Rom(Rom),
    /// 16 KiB of RAM.
    Ram(Box<[u8]>),
}

/// Why a ROM image was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RomError {
    /// Not 16 KiB or a power-of-two fraction of it.
    BadSize,
    /// No such slot.
    BadSlot,
}

/// A ROM image of 2, 4, 8 or 16 KiB, mirrored to fill 16 KiB as the
/// unused address lines leave it.
fn rom_image(bytes: &[u8]) -> Result<Box<[u8]>, RomError> {
    if !matches!(bytes.len(), 0x800 | 0x1000 | 0x2000 | 0x4000) {
        return Err(RomError::BadSize);
    }
    let mut image = vec![0; ROM_SIZE];
    for chunk in image.chunks_exact_mut(bytes.len()) {
        chunk.copy_from_slice(bytes);
    }
    Ok(image.into_boxed_slice())
}

/// The machine's state other than the CPU's: memory, devices, and time.
pub struct Board {
    now: u64,
    ram: Box<[u8]>,
    os: Rom,
    slots: [Slot; SLOTS],
    romsel: u8,
    /// The byte last carried by the data bus, which an undriven read sees.
    bus: u8,
    system_via: Via,
    user_via: Via,
    latch: u8,
    keyboard: Keyboard,
    /// Joystick fire buttons held, bit 0 for joystick 1.
    fire: u8,
    sound: Sn76489,
    /// The sound generator's output since it was last taken, one sample per
    /// update.
    audio: Vec<f32>,
    audio_enabled: bool,
    video: Video,
    printer: Printer,
    acia: Acia,
    serial_ula: SerialUla,
    cassette: Cassette,
    adc: Adc,
    fdc: Fdc,
    /// What has been sent out of the RS423 port.
    rs423_output: Vec<u8>,
    /// The I/O accesses a bus monitor is recording, oldest first.
    io_trace: Option<alloc::collections::VecDeque<IoAccess>>,
    irq: bool,
    nmi: bool,
    next_event: u64,
}

/// RS423's clear-to-send input: high, not clear, with nothing connected.
const RS423_CTS: bool = true;

/// The printer on the user port: it takes the byte on port A at each falling
/// edge of the strobe (CA2) and acknowledges it on CA1.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Printer {
    output: Vec<u8>,
    /// The strobe's level when last seen.
    strobe: bool,
    /// The cycles the acknowledge pulse starts and ends.
    ack: Option<(u64, u64)>,
}

impl Default for Printer {
    fn default() -> Self {
        Self {
            output: Vec::new(),
            strobe: true,
            ack: None,
        }
    }
}

/// One access to the memory-mapped I/O pages, as a bus monitor records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoAccess {
    /// The cycle the device saw it in.
    pub cycle: u64,
    /// The address.
    pub addr: u16,
    /// The byte transferred.
    pub value: u8,
    /// Whether the CPU wrote it.
    pub write: bool,
}

/// The most accesses a bus monitor keeps before it drops the oldest.
pub const IO_TRACE_CAPACITY: usize = 4096;

/// Cycles from a strobe to the printer's acknowledge, and its width.
const PRINTER_ACK_DELAY: u64 = 20;
const PRINTER_ACK_WIDTH: u64 = 10;

// What was printed is the front end's, taken or lost; an acknowledge under
// way ends within its delay and width of the machine's time.
snapshot::fields!(
    Printer { strobe, ack } transient { output },
    check | printer,
    r | printer.ack.is_none_or(|(start, end)| {
        r.pending(end)
            && end - r.clock() <= PRINTER_ACK_DELAY + PRINTER_ACK_WIDTH
            && end.checked_sub(PRINTER_ACK_WIDTH) == Some(start)
    })
);

/// The domain a ROM image's fingerprint is taken in.
const ROM_DOMAIN: &[u8] = b"tairix.beeb.rom";

/// The fingerprint a snapshot records a ROM image by.
fn rom_fingerprint(image: &[u8]) -> [u8; FINGERPRINT_LEN] {
    let mut fingerprint = Fingerprint::new(ROM_DOMAIN);
    fingerprint.field(image);
    fingerprint.finish()
}

/// What a snapshot records each sideways slot as holding.
mod fitting {
    pub const EMPTY: u8 = 0;
    pub const ROM: u8 = 1;
    pub const RAM: u8 = 2;
}

/// Samples of sound kept for the front end to take: a second's worth. The
/// chip keeps running exactly past it; the samples are dropped.
pub const AUDIO_CAPACITY: usize = sound::UPDATE_RATE as usize;

/// Bytes the printer and RS423 ports keep for the front end to take; more
/// are dropped.
pub const PORT_CAPACITY: usize = 1 << 16;

/// Keep what fits of `bytes` beneath `PORT_CAPACITY`.
fn collect_port(output: &mut Vec<u8>, bytes: impl IntoIterator<Item = u8>) {
    let room = PORT_CAPACITY.saturating_sub(output.len());
    output.extend(bytes.into_iter().take(room));
}

/// Which bus devices run at 1 MHz: FRED, JIM, and SHEILA but for the video
/// ULA, ROM select and the Tube.
const fn one_mhz(addr: u16) -> bool {
    use sheila::{FRED, SYSTEM_VIA, TUBE, VIDEO_CONTROL};
    matches!(addr, FRED..VIDEO_CONTROL | SYSTEM_VIA..TUBE)
}

impl Board {
    fn new(os: Rom) -> Self {
        let mut board = Self {
            now: 0,
            ram: vec![0; RAM_SIZE].into_boxed_slice(),
            os,
            slots: Default::default(),
            romsel: 0,
            bus: 0,
            system_via: Via::new(),
            user_via: Via::new(),
            latch: 0,
            keyboard: Keyboard::new(),
            fire: 0,
            sound: Sn76489::new(),
            audio: Vec::new(),
            audio_enabled: true,
            video: Video::new(),
            printer: Printer::default(),
            acia: Acia::new(),
            serial_ula: SerialUla::new(),
            cassette: Cassette::new(),
            adc: Adc::new(),
            fdc: Fdc::new(),
            rs423_output: Vec::new(),
            io_trace: None,
            irq: false,
            nmi: false,
            next_event: 0,
        };
        board.drive_control_lines();
        // RES is held while power comes up, so the levels the lines settle at
        // raise no flag.
        board.system_via.reset(0);
        board
    }

    /// Present each VIA control input at the level its source drives at
    /// power-on, so that the first change either way is a real edge.
    fn drive_control_lines(&mut self) {
        self.system_via.set_ca1(0, self.video.vsync());
        let keyboard = self.keyboard.column_interrupts(self.selected_column(0));
        self.system_via.set_ca2(0, keyboard);
        self.system_via.set_cb1(0, self.adc.eoc());
        // No light pen strobes.
        self.system_via.set_cb2(0, false);
        self.cassette.set_rts(0, self.acia.rts());
        // The printer's acknowledge and the user port idle high.
        self.user_via.set_ca1(0, true);
        self.user_via.set_cb1(0, true);
        self.user_via.set_cb2(0, true);
    }

    /// End the current cycle: run what falls due in it, then count it.
    fn end_cycle(&mut self) {
        if self.now >= self.next_event {
            self.run_events();
        }
        self.now += 1;
    }

    /// Stretch a 1 MHz access to the cycle its device sees it in.
    fn stretch(&mut self) {
        let extra = if self.now & 1 == 0 { 1 } else { 2 };
        for _ in 0..extra {
            self.end_cycle();
        }
    }

    fn run_events(&mut self) {
        let now = self.now;
        self.catch_up_video(now + 1);
        self.advance_devices(now);
        self.update_lines();
        self.schedule();
    }

    /// Bring every device but the picture up to `now`.
    fn advance_devices(&mut self, now: u64) {
        self.system_via.advance(now);
        self.update_keyboard_interrupt(now);
        self.run_printer(now);
        self.run_serial(now);
        self.run_adc(now);
        self.fdc.advance(now);
    }

    /// Complete a conversion due by `now`, following EOC into CB1.
    fn run_adc(&mut self, now: u64) {
        let was = self.adc.eoc();
        self.adc.advance(now);
        if was != self.adc.eoc() {
            self.system_via.set_cb1(now, self.adc.eoc());
        }
    }

    fn update_lines(&mut self) {
        self.irq = self.system_via.irq() || self.user_via.irq() || self.acia.irq();
        self.nmi = self.fdc.int();
    }

    /// Follow the serial ULA's choice of line into the 6850's clocks and
    /// inputs, and its motor bit into the cassette relay.
    fn route_serial(&mut self, now: u64) {
        let ula = self.serial_ula;
        self.acia
            .set_clocks(now, ula.transmit_clock(), ula.receive_clock());
        self.cassette.set_motor(now, ula.motor());
        if ula.rs423() {
            self.acia.set_dcd(false);
            self.acia.set_cts(RS423_CTS);
        } else {
            // DCD is high when the cassette carries no carrier.
            self.acia.set_dcd(!self.cassette.carrier());
            self.acia.set_cts(false);
        }
    }

    /// Run the 6850 and the cassette to `now`: what the 6850 sent goes where
    /// the ULA routes it, and what the tape plays reaches the 6850 while the
    /// ULA gives it the cassette.
    fn run_serial(&mut self, now: u64) {
        let (rate, format) = self.acia.receive_rate();
        let listening = !self.serial_ula.rs423();
        let acia = &mut self.acia;
        self.cassette
            .advance(now, rate, format, listening, |event| match event {
                CassetteEvent::Frame {
                    at,
                    byte,
                    framing_error,
                    parity_error,
                } => acia.receive(at, byte, framing_error, parity_error),
                CassetteEvent::Carrier { at, present } if listening => {
                    acia.advance(at);
                    acia.set_dcd(!present);
                }
                CassetteEvent::Carrier { .. } => {}
            });
        self.acia.advance(now);
        if self.serial_ula.rs423() {
            let sent = self.acia.take_transmitted().map(|(_, byte)| byte);
            collect_port(&mut self.rs423_output, sent);
        } else {
            let (format, frame) = (self.acia.format(), self.acia.transmit_frame());
            for (done, byte) in self.acia.take_transmitted() {
                self.cassette.transmitted(done, byte, format, frame);
            }
        }
    }

    /// The next cycle each device acts of its own accord, where it will.
    fn device_events(&self) -> impl Iterator<Item = u64> {
        let (rate, format) = self.acia.receive_rate();
        [
            Some(self.video.next_event()),
            self.system_via.next_event(),
            self.user_via.next_event(),
            self.acia.next_event(),
            self.cassette.next_event(rate, format),
            self.adc.next_event(),
            self.fdc.next_event(),
            self.next_keyboard_edge(),
            self.printer
                .ack
                .map(|(start, end)| if start > self.now { start } else { end }),
        ]
        .into_iter()
        .flatten()
    }

    fn schedule(&mut self) {
        let next = self.device_events().min().unwrap_or(u64::MAX);
        self.next_event = next.max(self.now + 1);
    }

    /// Whether a restored board is one a machine could leave: no device's
    /// next event already past, and the 6850 and the cassette wired as the
    /// serial ULA routes them.
    fn restorable(&self) -> bool {
        let ula = self.serial_ula;
        let (dcd, cts) = if ula.rs423() {
            (false, RS423_CTS)
        } else {
            (!self.cassette.carrier(), false)
        };
        self.device_events().all(|at| at >= self.now)
            && self.acia.clocks() == (ula.transmit_clock(), ula.receive_clock())
            && self.acia.lines() == (dcd, cts)
            && self.cassette.motor() == ula.motor()
            && self.cassette.rts() == self.acia.rts()
    }

    /// A device changed what it may do next: reconsider the lines and the
    /// next event.
    fn device_changed(&mut self) {
        self.update_lines();
        self.schedule();
    }

    /// Draw the picture up to, not including, `until`, delivering VSYNC to
    /// the System VIA's CA1 and the light pen's strobe to its CB2.
    fn catch_up_video(&mut self, until: u64) {
        let wrap = screen_wrap(self.latch);
        let via = &mut self.system_via;
        self.video
            .catch_up(until, &self.ram, wrap, |cycle, signal| match signal {
                Signal::Vsync(level) => via.set_ca1(cycle, level),
                Signal::LightPen(level) => via.set_cb2(cycle, level),
            });
    }

    /// The image paged into the sideways window, if the socket holds one.
    fn sideways(&self) -> Option<&[u8]> {
        match self.slots.get(usize::from(self.romsel))? {
            Slot::Rom(rom) => Some(&rom.image),
            Slot::Ram(bank) => Some(bank),
            Slot::Empty => None,
        }
    }

    fn record_io(&mut self, addr: u16, value: u8, write: bool) {
        if let Some(trace) = &mut self.io_trace {
            if trace.len() == IO_TRACE_CAPACITY {
                trace.pop_front();
            }
            trace.push_back(IoAccess {
                cycle: self.now,
                addr,
                value,
                write,
            });
        }
    }

    fn io_read(&mut self, addr: u16) -> u8 {
        use sheila::{
            ACIA_CONTROL, ACIA_END, ADC, ADC_END, CRTC_ADDRESS, CRTC_END, FDC, FDC_END, SYSTEM_VIA,
            SYSTEM_VIA_END, USER_VIA, USER_VIA_END,
        };
        if one_mhz(addr) {
            self.stretch();
        }
        let now = self.now;
        let value = match addr {
            CRTC_ADDRESS..=CRTC_END if addr & 1 == 1 => self.video.read_crtc(),
            ACIA_CONTROL..=ACIA_END => {
                // A frame the tape delivers in this cycle is there to read.
                self.run_serial(now);
                let value = self.acia.read(now, addr & 1 == 1);
                self.device_changed();
                value
            }
            SYSTEM_VIA..=SYSTEM_VIA_END => {
                self.sync_system_via(now);
                let value = self.system_via.read(now, register(addr));
                self.device_changed();
                value
            }
            ADC..=ADC_END => {
                self.run_adc(now);
                let value = self.adc.read(now, register(addr));
                self.device_changed();
                value
            }
            FDC..=FDC_END => {
                let value = self.fdc.read(now, register(addr)).unwrap_or(self.bus);
                self.device_changed();
                value
            }
            USER_VIA..=USER_VIA_END => {
                self.advance_user_via(now);
                let value = self.user_via.read(now, register(addr));
                self.after_user_via(now);
                value
            }
            _ => self.bus,
        };
        self.record_io(addr, value, false);
        self.bus = value;
        self.end_cycle();
        value
    }

    fn io_write(&mut self, addr: u16, value: u8) {
        use sheila::{
            ACIA_CONTROL, ACIA_END, ADC, ADC_END, CRTC_ADDRESS, CRTC_END, FDC, FDC_END, ROMSEL,
            ROMSEL_END, SERIAL_ULA, SERIAL_ULA_END, SYSTEM_VIA, SYSTEM_VIA_END, USER_VIA,
            USER_VIA_END, VIDEO_CONTROL, VIDEO_ULA_END,
        };
        if one_mhz(addr) {
            self.stretch();
        }
        let now = self.now;
        self.record_io(addr, value, true);
        match addr {
            CRTC_ADDRESS..=CRTC_END => {
                self.catch_up_video(now + 1);
                self.video.write_crtc(addr & 1 == 1, value);
                self.device_changed();
            }
            ACIA_CONTROL..=ACIA_END => {
                self.run_serial(now);
                self.acia.write(now, addr & 1 == 1, value);
                self.cassette.set_rts(now, self.acia.rts());
                self.run_serial(now);
                self.device_changed();
            }
            SERIAL_ULA..=SERIAL_ULA_END => {
                self.run_serial(now);
                self.serial_ula.write(value);
                self.route_serial(now);
                self.device_changed();
            }
            VIDEO_CONTROL..=VIDEO_ULA_END => {
                self.catch_up_video(now + 1);
                self.video.write_ula(addr & 1 == 1, value);
                self.device_changed();
            }
            ROMSEL..=ROMSEL_END => self.romsel = value & ROMSEL_MASK,
            SYSTEM_VIA..=SYSTEM_VIA_END => {
                self.sync_system_via(now);
                self.system_via.write(now, register(addr), value);
                self.after_system_via(now);
            }
            USER_VIA..=USER_VIA_END => {
                self.advance_user_via(now);
                self.user_via.write(now, register(addr), value);
                self.after_user_via(now);
            }
            FDC..=FDC_END => {
                self.fdc.write(now, register(addr), value);
                self.device_changed();
            }
            ADC..=ADC_END => {
                if register(addr) & 0x03 == adc::register::STATUS {
                    self.run_adc(now);
                    self.adc.write(now, value);
                    self.system_via.set_cb1(now, self.adc.eoc());
                }
                self.device_changed();
            }
            _ => {}
        }
        self.bus = value;
        self.end_cycle();
    }

    /// Bring everything the System VIA's inputs depend on up to `now`.
    fn sync_system_via(&mut self, now: u64) {
        self.catch_up_video(now + 1);
        self.update_keyboard_interrupt(now);
        self.system_via.advance(now);
        self.refresh_slow_bus(now);
    }

    /// The keyboard column its counter selects: free-running while the
    /// keyboard autoscans, loaded from PA0–3 while the CPU scans it.
    fn selected_column(&self, now: u64) -> u8 {
        if self.latch & latch::KEYBOARD_AUTOSCAN != 0 {
            Keyboard::scanned_column(tick(now))
        } else {
            self.system_via.port_a() & 0x0F
        }
    }

    /// What the slow data bus and port B present to the System VIA's pins:
    /// PA7 is the switch in the selected column at the row PA4–6 selects.
    fn refresh_slow_bus(&mut self, now: u64) {
        let row = self.system_via.port_a() & 0x70;
        let mut pins = 0xFF;
        if !self.keyboard.scan(row | self.selected_column(now)) {
            pins &= 0x7F;
        }
        self.system_via.set_port_a_pins(pins);
        let buttons = !(self.fire << 4) & FIRE_BUTTONS;
        self.system_via
            .set_port_b_pins(now, 0x0F | buttons | SPEECH_LINES);
    }

    /// The System VIA's outputs may have moved: follow them into the latch,
    /// and from the latch into the sound generator and the keyboard.
    fn after_system_via(&mut self, now: u64) {
        let port_b = self.system_via.port_b();
        let bit = 1 << (port_b & 0x07);
        let was = self.latch;
        if port_b & 0x08 != 0 {
            self.latch |= bit;
        } else {
            self.latch &= !bit;
        }
        if was & latch::SOUND_WE != 0 && self.latch & latch::SOUND_WE == 0 {
            self.render_sound(now);
            self.sound.write(self.system_via.port_a());
        }
        self.refresh_slow_bus(now);
        self.follow_keyboard(now);
        self.device_changed();
    }

    fn after_user_via(&mut self, now: u64) {
        let strobe = self.user_via.ca2();
        if self.printer.strobe && !strobe {
            collect_port(&mut self.printer.output, [self.user_via.port_a()]);
            let start = now + PRINTER_ACK_DELAY;
            self.printer.ack = Some((start, start + PRINTER_ACK_WIDTH));
        }
        self.printer.strobe = strobe;
        self.device_changed();
    }

    /// Bring the User VIA up to `now`. A strobe pulse that ended meanwhile
    /// raised the line the printer watches, so a strobe the next access
    /// starts is a falling edge it sees.
    fn advance_user_via(&mut self, now: u64) {
        self.user_via.advance(now);
        self.printer.strobe = self.user_via.ca2();
    }

    /// The printer's acknowledge on CA1, which in handshake mode raises CA2,
    /// so the strobe is seen after it.
    fn run_printer(&mut self, now: u64) {
        self.user_via.advance(now);
        if let Some((start, end)) = self.printer.ack {
            if now >= end {
                self.user_via.set_ca1(end, true);
                self.printer.ack = None;
            } else if now >= start {
                self.user_via.set_ca1(start, false);
            }
        }
        self.printer.strobe = self.user_via.ca2();
    }

    /// CA2's level at `now`: high whenever the selected column holds a key
    /// in rows 1–7, which is how the MOS learns a key went down while the
    /// keyboard autoscans, and finds its column while scanning.
    fn column_interrupt(&self, now: u64) -> bool {
        self.keyboard.column_interrupts(self.selected_column(now))
    }

    /// Bring CA2 up to `now`. The autoscan moves it every tick a held key's
    /// column passes; an edge the System VIA awaits is delivered at its own
    /// tick, which an event is set for, and any other move is taken quietly,
    /// since nothing can see it. In a manual scan it moves only as the CPU
    /// selects a column.
    fn update_keyboard_interrupt(&mut self, now: u64) {
        let level = self.column_interrupt(now);
        if self.latch & latch::KEYBOARD_AUTOSCAN == 0 {
            if level != self.system_via.ca2_input() {
                self.system_via.set_ca2(now, level);
            }
            return;
        }
        let moved = tick(now) > 0 && self.column_interrupt(tick_start(tick(now) - 1)) != level;
        if moved && self.system_via.ca2_edge_awaited() == Some(level) {
            self.system_via.sync_ca2(!level);
            self.system_via.set_ca2(now, level);
        } else {
            self.system_via.sync_ca2(level);
        }
    }

    /// Follow CA2 through a change to what drives it, the keys or the column
    /// selected, made at `now` with the line already brought up to it.
    fn follow_keyboard(&mut self, now: u64) {
        let level = self.column_interrupt(now);
        if level != self.system_via.ca2_input() {
            self.system_via.set_ca2(now, level);
        }
    }

    /// Change the keys with `change`, delivering what it does to CA2.
    fn change_keyboard(&mut self, change: impl FnOnce(&mut Keyboard)) {
        let now = self.now;
        self.update_keyboard_interrupt(now);
        change(&mut self.keyboard);
        self.follow_keyboard(now);
        self.device_changed();
    }

    /// The next cycle the autoscan moves CA2 onto the edge the System VIA
    /// awaits: none in a manual scan, or while no edge on CA2 changes
    /// anything.
    fn next_keyboard_edge(&self) -> Option<u64> {
        if self.latch & latch::KEYBOARD_AUTOSCAN == 0 {
            return None;
        }
        let awaited = self.system_via.ca2_edge_awaited()?;
        let level = |t: u64| self.keyboard.column_interrupts(Keyboard::scanned_column(t));
        let from = tick(self.now);
        (from + 1..=from + keyboard::SCAN_COLUMNS)
            .find(|&t| level(t) == awaited && level(t - 1) != awaited)
            .map(tick_start)
    }

    /// Record the ROMs by fingerprint and the sideways RAM by its contents.
    fn save_fitting(&self, w: &mut Writer) {
        w.put(&self.os.fingerprint);
        for slot in &self.slots {
            match slot {
                Slot::Empty => w.put(&fitting::EMPTY),
                Slot::Rom(rom) => {
                    w.put(&fitting::ROM);
                    w.put(&rom.fingerprint);
                }
                Slot::Ram(bank) => {
                    w.put(&fitting::RAM);
                    w.bytes(bank);
                }
            }
        }
    }

    /// The sideways slots a snapshot restores: this board's ROMs where it
    /// recorded the same ones, its own sideways RAM's contents, and a refusal
    /// for any socket fitted otherwise.
    fn load_fitting(&self, r: &mut Reader<'_>) -> Result<[Slot; SLOTS], SnapshotError> {
        let fitted = |holds: bool| {
            if holds {
                Ok(())
            } else {
                Err(SnapshotError::Fitting)
            }
        };
        fitted(r.get::<[u8; FINGERPRINT_LEN]>()? == self.os.fingerprint)?;
        let mut slots: [Slot; SLOTS] = Default::default();
        for (slot, fitted_now) in slots.iter_mut().zip(&self.slots) {
            *slot = match (r.get::<u8>()?, fitted_now) {
                (fitting::EMPTY, Slot::Empty) => Slot::Empty,
                (fitting::ROM, Slot::Rom(rom)) => {
                    fitted(r.get::<[u8; FINGERPRINT_LEN]>()? == rom.fingerprint)?;
                    Slot::Rom(rom.clone())
                }
                (fitting::RAM, Slot::Ram(_)) => {
                    let bank = r.bytes_at_most(ROM_SIZE)?;
                    Reader::check(bank.len() == ROM_SIZE)?;
                    Slot::Ram(bank.into())
                }
                (fitting::EMPTY | fitting::ROM | fitting::RAM, _) => {
                    return Err(SnapshotError::Fitting)
                }
                _ => return Err(SnapshotError::Malformed),
            };
        }
        Ok(slots)
    }

    fn render_sound(&mut self, now: u64) {
        let until = now / CYCLES_PER_UPDATE;
        if self.audio_enabled {
            let room = AUDIO_CAPACITY.saturating_sub(self.audio.len());
            let room = u64::try_from(room).unwrap_or(u64::MAX);
            let kept = until.min(self.sound.updates().saturating_add(room));
            self.sound.render(kept, &mut self.audio);
        }
        self.sound.skip(until);
    }
}

/// The register of a 16-register device at `addr`.
const fn register(addr: u16) -> u8 {
    (addr & 0x0F) as u8
}

/// The amount the hardware adds to an address past the top of memory, from
/// the latch's screen-size bits.
const fn screen_wrap(latch: u8) -> u16 {
    match (latch & latch::SCREEN_SIZE) >> 4 {
        0b11 => 0x3000,
        0b00 => 0x4000,
        0b10 => 0x5800,
        _ => 0x6000,
    }
}

impl Bus for Board {
    fn read(&mut self, addr: u16) -> u8 {
        let value = match addr {
            ..SIDEWAYS_BASE => self.ram[usize::from(addr)],
            SIDEWAYS_BASE..OS_BASE => match self.sideways() {
                Some(image) => image[usize::from(addr - SIDEWAYS_BASE)],
                None => self.bus,
            },
            IO_BASE..IO_END => return self.io_read(addr),
            _ => self.os.image[usize::from(addr - OS_BASE)],
        };
        self.bus = value;
        self.end_cycle();
        value
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            ..SIDEWAYS_BASE => {
                // The 6845 fetches in the half-cycle before the CPU writes.
                self.catch_up_video(self.now + 1);
                self.ram[usize::from(addr)] = value;
            }
            SIDEWAYS_BASE..OS_BASE => {
                if let Some(Slot::Ram(bank)) = self.slots.get_mut(usize::from(self.romsel)) {
                    bank[usize::from(addr - SIDEWAYS_BASE)] = value;
                }
            }
            IO_BASE..IO_END => return self.io_write(addr, value),
            _ => {}
        }
        self.bus = value;
        self.end_cycle();
    }

    fn irq(&self) -> bool {
        self.irq
    }

    fn nmi(&self) -> bool {
        self.nmi
    }
}

/// The whole machine.
pub struct Machine {
    cpu: Cpu,
    board: Board,
    /// BREAK is held, keeping the CPU in reset.
    break_held: bool,
}

impl Machine {
    /// A machine with `os` as its operating system ROM and every sideways
    /// slot empty, powered on.
    pub fn new(os: &[u8]) -> Result<Self, RomError> {
        let mut machine = Self {
            cpu: Cpu::new(),
            board: Board::new(Rom::new(rom_image(os)?)),
            break_held: false,
        };
        machine.board.route_serial(0);
        machine.board.schedule();
        Ok(machine)
    }

    /// Fit a ROM image in sideways slot `slot`.
    pub fn insert_rom(&mut self, slot: usize, image: &[u8]) -> Result<(), RomError> {
        let rom = Rom::new(rom_image(image)?);
        *self.board.slots.get_mut(slot).ok_or(RomError::BadSlot)? = Slot::Rom(rom);
        Ok(())
    }

    /// Fit 16 KiB of sideways RAM in slot `slot`.
    pub fn insert_ram(&mut self, slot: usize) -> Result<(), RomError> {
        *self.board.slots.get_mut(slot).ok_or(RomError::BadSlot)? =
            Slot::Ram(vec![0; ROM_SIZE].into_boxed_slice());
        Ok(())
    }

    /// The current cycle.
    #[must_use]
    pub const fn cycles(&self) -> u64 {
        self.board.now
    }

    /// Run until at least `until` cycles have elapsed since power-on, ending
    /// on an instruction boundary.
    pub fn run_until(&mut self, until: u64) {
        while self.board.now < until {
            if !self.break_held {
                self.cpu.step(&mut self.board);
            } else if self.board.now < self.board.next_event {
                // With the CPU held, nothing happens before the next event.
                self.board.now = self.board.next_event.min(until);
            } else {
                self.board.end_cycle();
            }
        }
        self.board.catch_up_video(self.board.now);
        self.board.render_sound(self.board.now);
    }

    /// Press or release a key.
    pub fn set_key(&mut self, key: Key, down: bool) {
        self.board
            .change_keyboard(|keyboard| keyboard.set(key, down));
    }

    /// Release every key: what the keyboard is left with when the window
    /// loses focus mid-press.
    pub fn release_keys(&mut self) {
        self.board.change_keyboard(Keyboard::release_all);
    }

    /// The startup options the keyboard links set; the OS reads them at a
    /// hard BREAK.
    #[must_use]
    pub fn startup_options(&self) -> u8 {
        self.board.keyboard.startup_options()
    }

    /// Fit the keyboard links for `options`.
    pub fn set_startup_options(&mut self, options: u8) {
        self.board
            .change_keyboard(|keyboard| keyboard.set_startup_options(options));
    }

    /// Press or release BREAK. While it is down the CPU and the chips sharing
    /// its reset line, the User VIA and the 8271, are held in reset, and the
    /// CPU restarts as it is released. The System VIA is reset only at power
    /// on, which is how the MOS tells the two apart.
    pub fn set_break(&mut self, down: bool) {
        if down == self.break_held {
            return;
        }
        let now = self.board.now;
        self.board.user_via.reset(now);
        self.board.after_user_via(now);
        self.board.fdc.advance(now);
        self.board.fdc.reset();
        if !down {
            self.cpu.reset();
        }
        self.board.device_changed();
        self.break_held = down;
    }

    /// The CPU.
    #[must_use]
    pub const fn cpu(&self) -> &Cpu {
        &self.cpu
    }

    /// A byte of main memory, read without touching the bus.
    #[must_use]
    pub fn peek(&self, addr: u16) -> u8 {
        self.board.ram.get(usize::from(addr)).copied().unwrap_or(0)
    }

    /// The video system.
    #[must_use]
    pub const fn video(&self) -> &Video {
        &self.board.video
    }

    /// The System VIA.
    #[must_use]
    pub const fn system_via(&self) -> &Via {
        &self.board.system_via
    }

    /// The User VIA.
    #[must_use]
    pub const fn user_via(&self) -> &Via {
        &self.board.user_via
    }

    /// The rows of the finished picture that changed since last asked.
    pub fn take_dirty_rows(&mut self) -> video::DirtyRows {
        self.board.video.take_dirty()
    }

    /// The sound generator's output since last asked, one sample per 250 kHz
    /// update.
    pub fn take_audio(&mut self) -> alloc::vec::Drain<'_, f32> {
        self.board.audio.drain(..)
    }

    /// Keep the sound generator's output for `take_audio`, or let the chip
    /// run on silently, as a machine running unpaced does.
    pub fn set_sound_output(&mut self, on: bool) {
        let now = self.board.now;
        self.board.render_sound(now);
        self.board.audio_enabled = on;
    }

    /// Put a disc in `drive`.
    pub fn insert_disc(&mut self, drive: PhysicalDrive, disc: Disc) {
        let now = self.board.now;
        self.board.fdc.set_disc(now, drive, Some(disc));
        self.board.device_changed();
    }

    /// Take the disc out of `drive`.
    pub fn eject_disc(&mut self, drive: PhysicalDrive) -> Option<Disc> {
        let now = self.board.now;
        let disc = self.board.fdc.set_disc(now, drive, None);
        self.board.device_changed();
        disc
    }

    /// The disc in `drive`.
    #[must_use]
    pub const fn disc(&self, drive: PhysicalDrive) -> Option<&Disc> {
        self.board.fdc.drive(drive).disc()
    }

    /// Note that the disc in `drive` has been saved.
    pub fn mark_disc_saved(&mut self, drive: PhysicalDrive) {
        if let Some(disc) = self.board.fdc.disc_mut(drive) {
            disc.mark_saved();
        }
    }

    /// Put `tape` in the cassette deck, wound to its start, returning the
    /// tape it replaces with what was recorded on it.
    pub fn insert_tape(&mut self, tape: Tape) -> Option<Tape> {
        let now = self.board.now;
        let old = self.board.cassette.insert(now, tape);
        self.board.device_changed();
        old
    }

    /// Take the tape out of the deck, with what was recorded on it.
    pub fn eject_tape(&mut self) -> Option<Tape> {
        let now = self.board.now;
        let tape = self.board.cassette.eject(now);
        self.board.device_changed();
        tape
    }

    /// Whether a tape is in the deck.
    #[must_use]
    pub const fn holds_tape(&self) -> bool {
        self.board.cassette.holds_tape()
    }

    /// The tape in the deck, with what has been recorded on it so far.
    pub fn tape(&mut self) -> Option<&Tape> {
        let now = self.board.now;
        self.board.cassette.tape(now)
    }

    /// Where the tape stands, in cycles from its start.
    #[must_use]
    pub fn tape_position(&self) -> u64 {
        self.board.cassette.position(self.board.now)
    }

    /// Wind the tape to `position` cycles from its start.
    pub fn wind_tape(&mut self, position: u64) {
        let now = self.board.now;
        self.board.cassette.wind(now, position);
        self.board.device_changed();
    }

    /// Press or release the deck's record key.
    pub fn set_tape_record(&mut self, record: bool) {
        let now = self.board.now;
        self.board.cassette.set_record(now, record);
        self.board.device_changed();
    }

    /// Whether the deck's record key is down, and whether the last recording
    /// ran into the tape's end.
    #[must_use]
    pub fn tape_record(&self) -> (bool, bool) {
        (self.board.cassette.recording(), self.board.cassette.full())
    }

    /// Whether the cassette motor runs: the motor light.
    #[must_use]
    pub const fn tape_motor(&self) -> bool {
        self.board.cassette.motor()
    }

    /// Whether `drive`'s head is loaded: its activity light.
    #[must_use]
    pub fn drive_active(&self, drive: PhysicalDrive) -> bool {
        self.board.fdc.head_loaded(drive)
    }

    /// Hold the light pen over canvas slot `column` of row `row` of the
    /// picture, or take it away.
    pub fn set_light_pen(&mut self, at: Option<(usize, usize)>) {
        let now = self.board.now;
        self.board.catch_up_video(now);
        self.board.video.set_light_pen(at);
    }

    /// Set an analogue input, full scale at `0xFFFF`: channels 0 and 1 are
    /// the first joystick's horizontal and vertical axes, 2 and 3 the
    /// second's, each reading low at right and down.
    pub fn set_analogue(&mut self, channel: usize, level: u16) {
        self.board.adc.set_input(channel, level);
    }

    /// Hold or release joystick fire buttons, bit 0 for the first joystick.
    pub fn set_fire_buttons(&mut self, held: u8) {
        self.board.fire = held & 0x03;
        let now = self.board.now;
        self.board.refresh_slow_bus(now);
        self.board.device_changed();
    }

    /// Whether the keyboard's CAPS LOCK and SHIFT LOCK LEDs are lit.
    #[must_use]
    pub const fn lock_leds(&self) -> (bool, bool) {
        (
            self.board.latch & latch::CAPS_LOCK_LED == 0,
            self.board.latch & latch::SHIFT_LOCK_LED == 0,
        )
    }

    /// Start or stop a bus monitor recording accesses to the I/O pages.
    pub fn monitor_io(&mut self, on: bool) {
        self.board.io_trace = on.then(alloc::collections::VecDeque::new);
    }

    /// The accesses the bus monitor has recorded since last asked.
    pub fn take_io_trace(&mut self) -> impl Iterator<Item = IoAccess> + '_ {
        self.board
            .io_trace
            .iter_mut()
            .flat_map(|trace| trace.drain(..))
    }

    /// What the printer has been sent since last asked.
    pub fn take_printer_output(&mut self) -> alloc::vec::Drain<'_, u8> {
        self.board.printer.output.drain(..)
    }

    /// What has been sent out of the RS423 port since last asked.
    pub fn take_rs423_output(&mut self) -> alloc::vec::Drain<'_, u8> {
        self.board.rs423_output.drain(..)
    }

    /// A snapshot of the machine as it stands (`crate::snapshot`), every
    /// device, the picture and the sound generator brought up to the current
    /// cycle first, as a restore requires.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::TooLarge`] for a machine whose state outgrows a
    /// snapshot.
    pub fn snapshot(&mut self) -> Result<Vec<u8>, SnapshotError> {
        let now = self.board.now;
        self.board.catch_up_video(now);
        self.board.advance_devices(now);
        self.board.device_changed();
        self.board.render_sound(now);
        let board = &self.board;
        let mut w = Writer::new();
        w.section(*b"ROMS", |w| board.save_fitting(w));
        w.section(*b"TIME", |w| w.put(&board.now));
        w.section(*b"CPU ", |w| {
            w.put(&self.cpu.state());
            w.put(&self.break_held);
        });
        w.section(*b"BORD", |w| {
            w.bytes(&board.ram);
            w.put(&board.romsel);
            w.put(&board.bus);
            w.put(&board.latch);
            w.put(&board.fire);
        });
        w.section(*b"SVIA", |w| w.put(&board.system_via));
        w.section(*b"UVIA", |w| w.put(&board.user_via));
        w.section(*b"KEYS", |w| w.put(&board.keyboard));
        w.section(*b"SND ", |w| w.put(&board.sound));
        w.section(*b"VID ", |w| w.put(&board.video));
        w.section(*b"PRN ", |w| w.put(&board.printer));
        w.section(*b"ACIA", |w| w.put(&board.acia));
        w.section(*b"SULA", |w| w.put(&board.serial_ula));
        w.section(*b"CASS", |w| w.put(&board.cassette));
        w.section(*b"ADC ", |w| w.put(&board.adc));
        w.section(*b"FDC ", |w| w.put(&board.fdc));
        w.finish()
    }

    /// Restore `snapshot`, taken on a machine fitted with these ROMs and
    /// sideways RAM. The whole snapshot is checked before anything changes,
    /// so a refused one leaves the machine as it was. Sound and output not
    /// yet taken belong to the machine replaced and are dropped; the sound
    /// setting and any bus monitor carry on.
    ///
    /// # Errors
    ///
    /// [`SnapshotError`]: the snapshot is damaged, holds what no machine can,
    /// or was taken on one fitted otherwise.
    pub fn restore(&mut self, snapshot: &[u8]) -> Result<(), SnapshotError> {
        let body = snapshot::open(snapshot)?;
        let mut r = Reader::new(&body);
        let slots = r.section(*b"ROMS", |r| self.board.load_fitting(r))?;
        let now = r.section(*b"TIME", Reader::get::<u64>)?;
        r.set_clock(now)?;
        let (cpu, break_held) =
            r.section(*b"CPU ", |r| Ok((r.get::<CpuState>()?, r.get::<bool>()?)))?;
        let (ram, romsel, bus, latch, fire) = r.section(*b"BORD", |r| {
            let ram = r.bytes_at_most(RAM_SIZE)?;
            let scalars = (r.get::<u8>()?, r.get()?, r.get()?, r.get::<u8>()?);
            Reader::check(
                ram.len() == RAM_SIZE && scalars.0 & !ROMSEL_MASK == 0 && scalars.3 <= 0x03,
            )?;
            Ok((ram, scalars.0, scalars.1, scalars.2, scalars.3))
        })?;
        let mut board = Board {
            now,
            ram: ram.into(),
            os: self.board.os.clone(),
            slots,
            romsel,
            bus,
            system_via: r.section(*b"SVIA", Reader::get)?,
            user_via: r.section(*b"UVIA", Reader::get)?,
            latch,
            keyboard: r.section(*b"KEYS", Reader::get)?,
            fire,
            sound: r.section(*b"SND ", Reader::get)?,
            audio: Vec::new(),
            audio_enabled: self.board.audio_enabled,
            video: r.section(*b"VID ", Reader::get)?,
            printer: r.section(*b"PRN ", Reader::get)?,
            acia: r.section(*b"ACIA", Reader::get)?,
            serial_ula: r.section(*b"SULA", Reader::get)?,
            cassette: r.section(*b"CASS", Reader::get)?,
            adc: r.section(*b"ADC ", Reader::get)?,
            fdc: r.section(*b"FDC ", Reader::get)?,
            rs423_output: Vec::new(),
            io_trace: self
                .board
                .io_trace
                .as_ref()
                .map(|_| alloc::collections::VecDeque::new()),
            irq: false,
            nmi: false,
            next_event: 0,
        };
        Reader::check(r.left() == 0 && board.restorable())?;
        board.device_changed();
        self.cpu.restore(&cpu);
        self.board = board;
        self.break_held = break_held;
        Ok(())
    }
}
