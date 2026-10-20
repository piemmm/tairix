//! The console and seat layout every port installs, and the console device a
//! port's hosted framebuffer console is to the kernel.
//!
//! One decision covers all of it. With the port's framebuffer console
//! attached, that console is console 0 — the boot seat's text sink, the
//! console whose surface the seat hands over, the root-unlock prompt's — and
//! the serial line carries no session. Otherwise the port's serial console is
//! all of those. Deciding once is what keeps a seat's text sink, the console a
//! descriptor's index names, and the surface the seat hands over the same
//! device.

use alloc::vec::Vec;
use tairix_abi::driver::display::DisplayMode;
use tairix_abi::hwtree::DmaCoherence;
use tairix_abi::{Errno, TerminalSize};

use tairix_fbcon::hosted::{AdoptRefused, FramebufferConsole, GridSource};
use tairix_fbcon::port::{ConsolePort, ConsoleSurface};
use tairix_fbcon::{Cell, Surface};
use tairix_kernel_core::ConsoleWrite;
use tairix_log::{write_diag_line, Event, Level, Sink};

/// This port's half of the console.
#[cfg(kernel_isa = "aarch64")]
type Port = tairix_arch_aarch64::video::FramebufferPort;
/// This port's half of the console.
#[cfg(kernel_isa = "riscv64")]
type Port = tairix_arch_riscv64::video::FramebufferPort;
/// This port's half of the console.
#[cfg(kernel_isa = "x86_64")]
type Port = tairix_arch_x86_64::video::FramebufferPort;

/// The console's cell grids, on the kernel heap.
pub struct HeapGrids;

impl GridSource for HeapGrids {
    type Grid = Vec<Cell>;

    fn blank(cells: usize) -> Option<Vec<Cell>> {
        let mut grid = Vec::new();
        grid.try_reserve_exact(cells).ok()?;
        grid.resize(cells, Cell::BLANK);
        Some(grid)
    }
}

/// The framebuffer console the kernel hosts.
pub static CONSOLE: FramebufferConsole<Port, HeapGrids> = FramebufferConsole::new();

/// The hosted console as the kernel's console device: console 0 whenever it
/// is attached.
pub static FRAMEBUFFER: FramebufferConsoleDevice<Port, HeapGrids> =
    FramebufferConsoleDevice::new(&CONSOLE);

#[cfg(kernel_isa = "aarch64")]
use tairix_arch_aarch64::SERIAL_SINK;
#[cfg(kernel_isa = "riscv64")]
use tairix_arch_riscv64::SERIAL_SINK;
#[cfg(kernel_isa = "x86_64")]
use tairix_arch_x86_64::serial::SERIAL_SINK;

/// Milliseconds since boot, as the port's serial line stamps its records.
fn uptime_ms() -> Option<u64> {
    SERIAL_SINK.uptime_ms()
}

/// The log sink every port installs.
pub static LOG_SINK: LogSink = LogSink::new(&SERIAL_SINK, uptime_ms);

/// The port's serial line, and, in a shippable build, the framebuffer
/// console while it has a surface — the console a user sees. An error, and
/// anything worse, reaches the serial line however else it shows, and first,
/// so a renderer that cannot finish never costs the record.
pub struct LogSink {
    serial: &'static (dyn Sink + Sync),
    uptime_ms: fn() -> Option<u64>,
}

impl LogSink {
    /// A sink over `serial`, stamping console lines with `uptime_ms`.
    #[must_use]
    pub const fn new(serial: &'static (dyn Sink + Sync), uptime_ms: fn() -> Option<u64>) -> Self {
        Self { serial, uptime_ms }
    }
}

impl Sink for LogSink {
    fn write_event(&self, event: &Event<'_>) {
        let screen = !cfg!(debug_assertions) && CONSOLE.paints();
        if !screen || event.level >= Level::Error {
            self.serial.write_event(event);
        }
        if screen {
            let mut line = ConsoleLine::new();
            write_diag_line(&mut line, (self.uptime_ms)(), true, event);
            line.flush();
        }
    }
}

/// A record's text for the hosted console, painted a buffer at a time rather
/// than once per piece the formatter writes.
struct ConsoleLine {
    bytes: [u8; 128],
    len: usize,
}

impl ConsoleLine {
    const fn new() -> Self {
        Self {
            bytes: [0; 128],
            len: 0,
        }
    }

    fn flush(&mut self) {
        if self.len != 0 {
            CONSOLE.write_bytes(&self.bytes[..self.len]);
            self.len = 0;
        }
    }
}

impl core::fmt::Write for ConsoleLine {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        // Cut anywhere: the console decodes its bytes as a stream.
        let mut bytes = text.as_bytes();
        while !bytes.is_empty() {
            if self.len == self.bytes.len() {
                self.flush();
            }
            let take = bytes.len().min(self.bytes.len() - self.len);
            let (head, rest) = bytes.split_at(take);
            self.bytes[self.len..self.len + take].copy_from_slice(head);
            self.len += take;
            bytes = rest;
        }
        Ok(())
    }
}

/// A hosted framebuffer console as the kernel's console device.
pub struct FramebufferConsoleDevice<P: ConsolePort, S: GridSource> {
    console: &'static FramebufferConsole<P, S>,
}

impl<P: ConsolePort, S: GridSource> FramebufferConsoleDevice<P, S> {
    /// The device over `console`.
    #[must_use]
    pub const fn new(console: &'static FramebufferConsole<P, S>) -> Self {
        Self { console }
    }

    /// Whether the console is attached, and so is the layout's console 0.
    #[must_use]
    pub fn is_attached(&self) -> bool {
        self.console.is_active()
    }
}

impl<P: ConsolePort, S: GridSource> ConsoleWrite for FramebufferConsoleDevice<P, S> {
    fn write(&self, bytes: &[u8]) -> Result<usize, Errno> {
        // Listed only once attached, but a write reaching it otherwise fails
        // closed rather than dropping the bytes.
        if !self.console.is_active() {
            return Err(Errno::NotImplemented);
        }
        self.console.write_bytes(bytes);
        Ok(bytes.len())
    }

    fn write_output(&self, bytes: &[u8]) -> Result<usize, Errno> {
        if !self.console.is_active() {
            return Err(Errno::NotImplemented);
        }
        self.console.write_output_bytes(bytes);
        Ok(bytes.len())
    }

    fn geometry(&self) -> Option<TerminalSize> {
        self.console.text_grid()
    }

    fn set_surface(&self, surface: Surface) {
        self.console.set_surface(surface);
    }

    fn reclaim_surface(&self) {
        self.console.reclaim_surface();
    }

    fn enter_fatal(&self, every_cpu_stopped: bool) {
        self.console.fatal(every_cpu_stopped);
    }

    fn surface(&self) -> Option<ConsoleSurface> {
        self.console.placement()
    }

    fn can_host(&self) -> bool {
        self.console.is_active()
    }

    fn adopt(&self, phys: u64, mode: DisplayMode, coherence: DmaCoherence) -> Result<(), Errno> {
        let surface = ConsoleSurface {
            phys,
            mode,
            kernel_owned: true,
        };
        let base = P::direct_map(phys, surface.mode.len_bytes()).ok_or(Errno::OutOfRange)?;
        self.console
            .on_damage(tairix_kernel_core::console_damage_wake);
        self.console
            .adopt(base, surface, true, coherence == DmaCoherence::Snooped)
            .map_err(|refused| match refused {
                AdoptRefused::NoConsole => Errno::NotFound,
                AdoptRefused::Geometry => Errno::LengthOutOfRange,
                AdoptRefused::Unreachable => Errno::OutOfRange,
                AdoptRefused::Incoherent => Errno::NotSupported,
                AdoptRefused::OutOfMemory => Errno::OutOfMemory,
            })
    }

    fn owes_damage(&self) -> bool {
        self.console.owes_damage()
    }

    fn take_damage(&self) -> Option<(u32, u32)> {
        self.console.take_damage()
    }

    fn owe_damage(&self, start: u32, end: u32) {
        self.console.owe_damage((start, end));
    }

    fn vacate(&self, phys: u64) -> bool {
        self.console.vacate(phys)
    }

    fn boot_holds_screen(&self) -> bool {
        // Console 0 is handed to `login` by opening this gate, and the opener
        // wakes the console wait-queue.
        !crate::unlock_service::CONSOLE0_GATE.is_open()
    }

    fn purge(&self) {
        // A retained console owns its screen, so the discard is real: both
        // grids and every pixel, not the escape sequence the default sends a
        // terminal the kernel cannot reach.
        self.console.purge();
    }
}

#[cfg(freestanding)]
pub use layout::{console_layout, framebuffer_console0, FRAMEBUFFER_KEYBOARD};

/// The layout's statics, over the devices the port names.
#[cfg(freestanding)]
mod layout {
    use tairix_kernel_core::{ConsoleDevice, ConsoleInputQueue, SeatRegistry};

    use super::FRAMEBUFFER;
    #[cfg(kernel_isa = "aarch64")]
    use crate::aarch64::arch_wrapper::UART_ONLY_CONSOLES as SERIAL_CONSOLES;
    #[cfg(kernel_isa = "riscv64")]
    use crate::riscv64::boot::RISCV_UART_CONSOLES as SERIAL_CONSOLES;
    use crate::unlock_orchestrate::{Console0, KeySource};
    use crate::unlock_service::{GatedConsoleRead, CONSOLE0_GATE};
    #[cfg(kernel_isa = "x86_64")]
    use crate::x86_64::serial_sink::COM1_CONSOLES as SERIAL_CONSOLES;

    /// The framebuffer console's keyboard type-ahead queue: both its read
    /// half, drained by the login reading console 0, and the seat's text sink,
    /// into which an injected key press is encoded while the seat is unowned.
    /// A framebuffer login reads only its own keyboard, never the serial line.
    pub static FRAMEBUFFER_KEYBOARD: ConsoleInputQueue = ConsoleInputQueue::new();

    /// Console 0's read half while the framebuffer console is console 0,
    /// withheld from `login` until the root unlock has read its passphrase
    /// off the same queue.
    static GATED_FRAMEBUFFER_READ: GatedConsoleRead =
        GatedConsoleRead::new(&FRAMEBUFFER_KEYBOARD, &CONSOLE0_GATE);

    /// The console list with the framebuffer console attached: it is the only
    /// console, and the serial line is the debug log with no session.
    static FRAMEBUFFER_CONSOLES: [ConsoleDevice; 1] = [ConsoleDevice::with_input(
        &FRAMEBUFFER,
        &GATED_FRAMEBUFFER_READ,
        &FRAMEBUFFER_KEYBOARD,
    )];

    /// The boot seat over the framebuffer console.
    static FRAMEBUFFER_SEAT_REGISTRY: SeatRegistry =
        SeatRegistry::new(&FRAMEBUFFER_CONSOLES[0]).with_consoles(&FRAMEBUFFER_CONSOLES);

    /// The boot seat over the serial console: a directly attached keyboard's
    /// presses reach the login reading it, and the surface handover is inert,
    /// since the line shares no surface with a display client.
    static SERIAL_SEAT_REGISTRY: SeatRegistry =
        SeatRegistry::new(&SERIAL_CONSOLES[0]).with_consoles(&SERIAL_CONSOLES);

    /// The console list and the boot seat the port installs.
    #[must_use]
    pub fn console_layout() -> (&'static [ConsoleDevice], &'static SeatRegistry) {
        if FRAMEBUFFER.is_attached() {
            (&FRAMEBUFFER_CONSOLES, &FRAMEBUFFER_SEAT_REGISTRY)
        } else {
            (&SERIAL_CONSOLES, &SERIAL_SEAT_REGISTRY)
        }
    }

    /// The root-unlock prompt's console 0 when the framebuffer console is
    /// console 0; `None` leaves the port to arm its serial console.
    #[must_use]
    pub fn framebuffer_console0() -> Option<Console0> {
        FRAMEBUFFER.is_attached().then_some(Console0 {
            write: &FRAMEBUFFER,
            read: &FRAMEBUFFER_KEYBOARD,
            keys: KeySource::InputDevice,
        })
    }
}

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;

    use tairix_fbcon::port::LinearMap;
    use tairix_sync::NopInterruptControl;

    use super::*;

    /// A port whose memory is the host's own.
    struct HostPort;

    impl ConsolePort for HostPort {
        type Irq = NopInterruptControl;

        const CLEANS: bool = false;

        fn reachable(_base: NonNull<u32>, _bytes: usize) -> bool {
            true
        }

        fn direct_map(phys: u64, len: u64) -> Option<NonNull<u32>> {
            LinearMap { base: 0, len: 0 }.of(phys, len)
        }

        fn publish(_first: NonNull<u32>, _bytes: usize) {}
    }

    static DETACHED: FramebufferConsole<HostPort, HeapGrids> = FramebufferConsole::new();

    #[test]
    fn a_detached_console_is_not_console_0_and_refuses_writes() {
        let device = FramebufferConsoleDevice::new(&DETACHED);
        assert!(!device.is_attached());
        assert_eq!(device.write(b"x"), Err(Errno::NotImplemented));
        assert_eq!(device.write_output(b"x"), Err(Errno::NotImplemented));
        assert_eq!(device.geometry(), None);
        assert_eq!(device.surface(), None);
        assert!(!device.vacate(0));
    }

    static ATTACHED: FramebufferConsole<HostPort, HeapGrids> = FramebufferConsole::new();

    #[test]
    fn an_attached_console_reports_its_grid_and_takes_every_byte() {
        let geometry = tairix_fbcon::Geometry::for_display(640, 480, 640 * 4).expect("geometry");
        assert!(ATTACHED.attach(Some(geometry)));
        let device = FramebufferConsoleDevice::new(&ATTACHED);
        assert!(device.is_attached());
        assert_eq!(device.write(b"login: "), Ok(7));
        assert_eq!(device.write_output(b"\n"), Ok(1));
        assert_eq!(
            device.geometry().map(|size| (size.rows(), size.cols())),
            Some((30, 80))
        );
        assert_eq!(device.surface(), None, "no driver hosts it yet");
    }
}
