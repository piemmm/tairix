//! The display adapters and the monitors they drive.

mod beam;
pub mod cga;
pub mod composite;
pub mod crtc6845;
pub mod font;
pub mod mda;
pub mod monitor;
pub mod raster;
pub mod vga;

/// A finished frame: opaque RGBA rows at the card's own dot and line
/// resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    /// Dots per line.
    pub width: u32,
    /// Lines.
    pub height: u32,
    /// `width * height` pixels of red, green, blue and alpha.
    pub pixels: &'a [u8],
    /// The picture inside the border, if the card displayed any.
    pub displayed: Option<raster::Area>,
}

use self::cga::Cga;
use self::mda::{Mda, Model};
use self::monitor::{Monitor, Phosphor};
use self::raster::RasterError;
use self::vga::crtc::Generation;
use self::vga::{Moved, Vga};
use crate::time::Nanos;
use alloc::vec::Vec;

/// The largest text screen read back: characters a row, and rows.
pub const MOST_TEXT: (u16, u16) = (256, 128);

/// The characters a card's text mode shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShownText {
    /// Characters a row.
    pub columns: u16,
    /// Rows.
    pub rows: u16,
    /// Each row's characters in code page 437, the top row first.
    pub codes: Vec<u8>,
}

/// Why a card's text was not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unread {
    /// It shows graphics, or nothing.
    NoText,
    /// Its text is wider or taller than [`MOST_TEXT`].
    TooLarge,
    /// The heap would not hold it.
    Memory,
}

/// A text screen of `columns` by `rows` whose codes `codes` yields a row at
/// a time.
fn read_text(
    columns: u16,
    rows: u16,
    codes: impl Iterator<Item = u8>,
) -> Result<ShownText, Unread> {
    if columns == 0 || rows == 0 {
        return Err(Unread::NoText);
    }
    if columns > MOST_TEXT.0 || rows > MOST_TEXT.1 {
        return Err(Unread::TooLarge);
    }
    let len = usize::from(columns) * usize::from(rows);
    let mut held = Vec::new();
    held.try_reserve_exact(len).map_err(|_| Unread::Memory)?;
    held.extend(codes.take(len));
    held.resize(len, 0);
    Ok(ShownText {
        columns,
        rows,
        codes: held,
    })
}

/// A display adapter a machine can have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adapter {
    /// The IBM Monochrome Display Adapter on a monitor of this phosphor.
    Mda(Phosphor),
    /// The Hercules Graphics Card.
    Hercules(Phosphor),
    /// The IBM Color Graphics Adapter.
    Cga(Monitor),
    /// The IBM Enhanced Graphics Adapter with its memory expansion fully
    /// populated: 256 KiB.
    Ega(Monitor),
    /// A VGA on an 8-bit card, with 256 KiB.
    Vga(Monitor),
}

/// The largest frame an EGA's or VGA's monitor shows: what a display of the
/// period syncs to at the scan rates those cards make.
const PLANAR_FRAME: (u32, u32) = (800, 600);

/// The largest frame any adapter's monitor shows.
pub const LARGEST_FRAME: (u32, u32) = PLANAR_FRAME;

const _: () = assert!(
    mda::FRAME.0 <= LARGEST_FRAME.0
        && mda::FRAME.1 <= LARGEST_FRAME.1
        && cga::FRAME.0 <= LARGEST_FRAME.0
        && cga::FRAME.1 <= LARGEST_FRAME.1
);

impl Adapter {
    /// The largest frame the adapter's monitor shows, in dots and lines. A
    /// frame the guest programs larger is out of the monitor's range.
    #[must_use]
    pub const fn largest_frame(self) -> (u32, u32) {
        match self {
            Self::Mda(_) | Self::Hercules(_) => mda::FRAME,
            Self::Cga(_) => cga::FRAME,
            Self::Ega(_) | Self::Vga(_) => PLANAR_FRAME,
        }
    }

    /// Whether the adapter is a colour one, as the configuration switches
    /// tell the BIOS.
    #[must_use]
    pub const fn colour(self) -> bool {
        matches!(self, Self::Cga(_))
    }

    /// Whether the adapter brings its own BIOS, as the switches report.
    #[must_use]
    pub const fn own_bios(self) -> bool {
        matches!(self, Self::Ega(..) | Self::Vga(_))
    }
}

/// An installed adapter.
#[derive(Clone, Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "a machine holds at most two for its life; boxing would only add an allocation"
)]
pub enum Card {
    /// An MDA or Hercules card.
    Mono(Mda),
    /// A CGA.
    Cga(Cga),
    /// An EGA or VGA.
    Planar(Vga),
}

impl Card {
    /// An adapter as power-up leaves it.
    pub fn new(adapter: Adapter) -> Result<Self, RasterError> {
        Ok(match adapter {
            Adapter::Mda(phosphor) => Self::Mono(Mda::new(Model::Mda, phosphor)?),
            Adapter::Hercules(phosphor) => Self::Mono(Mda::new(Model::Hercules, phosphor)?),
            Adapter::Cga(monitor) => Self::Cga(Cga::new(monitor)?),
            Adapter::Ega(monitor) => Self::Planar(Vga::new(Generation::Ega, monitor)?),
            Adapter::Vga(monitor) => Self::Planar(Vga::new(Generation::Vga, monitor)?),
        })
    }

    /// The emulated time the card has drawn to.
    #[must_use]
    pub fn drawn_to(&self) -> Nanos {
        match self {
            Self::Mono(card) => card.drawn_to(),
            Self::Cga(card) => card.drawn_to(),
            Self::Planar(card) => card.drawn_to(),
        }
    }

    /// The I/O ranges the card decodes now.
    #[must_use]
    pub fn ports(&self) -> &'static [(u16, u16)] {
        match self {
            Self::Mono(card) => card.ports(),
            Self::Cga(_) => Cga::PORTS,
            Self::Planar(card) => card.ports(),
        }
    }

    /// The memory window the card answers: its base and length.
    #[must_use]
    pub fn window(&self) -> (u32, u32) {
        match self {
            Self::Mono(card) => card.window(),
            Self::Cga(_) => Cga::window(),
            Self::Planar(card) => card.window(),
        }
    }

    /// Reads a port at `t`.
    pub fn input(&mut self, port: u16, t: Nanos) -> u8 {
        match self {
            Self::Mono(card) => card.input(port, t),
            Self::Cga(card) => card.input(port, t),
            Self::Planar(card) => card.input(port, t),
        }
    }

    /// Writes a port at `t`; returns what moved.
    pub fn output(&mut self, port: u16, value: u8, t: Nanos) -> Moved {
        match self {
            Self::Mono(card) => Moved {
                window: card.output(port, value, t),
                ports: false,
            },
            Self::Cga(card) => {
                card.output(port, value, t);
                Moved::default()
            }
            Self::Planar(card) => card.output(port, value, t),
        }
    }

    /// Reads a byte of the card's memory, as the CPU does.
    pub fn read(&mut self, addr: u32) -> u8 {
        match self {
            Self::Mono(card) => card.read(addr),
            Self::Cga(card) => card.read(addr),
            Self::Planar(card) => card.read(addr),
        }
    }

    /// What a read at `addr` would return, without its side effects.
    #[must_use]
    pub fn peek(&self, addr: u32) -> u8 {
        match self {
            Self::Mono(card) => card.read(addr),
            Self::Cga(card) => card.read(addr),
            Self::Planar(card) => card.peek(addr),
        }
    }

    /// Writes a byte of the card's memory at `t`.
    pub fn write(&mut self, addr: u32, value: u8, t: Nanos) {
        match self {
            Self::Mono(card) => card.write(addr, value, t),
            Self::Cga(card) => card.write(addr, value, t),
            Self::Planar(card) => card.write(addr, value, t),
        }
    }

    /// How long a CPU access to the card's memory made at `t` waits.
    #[must_use]
    pub fn access_wait(&self, t: Nanos) -> Nanos {
        match self {
            Self::Mono(_) | Self::Planar(_) => 0,
            Self::Cga(_) => Cga::access_wait(t),
        }
    }

    /// Whether the card asks for IRQ 2: the EGA's and VGA's vertical
    /// interrupt.
    #[must_use]
    pub fn irq(&self) -> bool {
        matches!(self, Self::Planar(card) if card.irq())
    }

    /// When the card next raises IRQ 2, if it will.
    #[must_use]
    pub fn next_interrupt(&self) -> Option<Nanos> {
        match self {
            Self::Planar(card) => card.next_interrupt(),
            _ => None,
        }
    }

    /// Renders everything the beam has drawn by `t`.
    pub fn catch_up(&mut self, t: Nanos) {
        match self {
            Self::Mono(card) => card.catch_up(t),
            Self::Cga(card) => card.catch_up(t),
            Self::Planar(card) => card.catch_up(t),
        }
    }

    /// The last finished frame, if one finished since the last call.
    pub fn take_frame(&mut self) -> Option<Frame<'_>> {
        match self {
            Self::Mono(card) => card.take_frame(),
            Self::Cga(card) => card.take_frame(),
            Self::Planar(card) => card.take_frame(),
        }
    }

    /// Whether the card is in a text mode, which [`Self::shown_text`]
    /// reads.
    #[must_use]
    pub fn shows_text(&self) -> bool {
        match self {
            Self::Mono(card) => card.shows_text(),
            Self::Cga(card) => card.shows_text(),
            Self::Planar(card) => card.shows_text(),
        }
    }

    /// The text the card shows, as its registers and memory stand.
    ///
    /// # Errors
    ///
    /// [`Unread`]: graphics or nothing shown, a screen larger than any read
    /// back, or no memory to read it into.
    pub fn shown_text(&self) -> Result<ShownText, Unread> {
        match self {
            Self::Mono(card) => card.shown_text(),
            Self::Cga(card) => card.shown_text(),
            Self::Planar(card) => card.shown_text(),
        }
    }
}

impl crate::snapshot::State for Card {
    fn save(&self, out: &mut crate::snapshot::Out) {
        match self {
            Self::Mono(card) => {
                out.u8(0);
                card.save(out);
            }
            Self::Cga(card) => {
                out.u8(1);
                card.save(out);
            }
            Self::Planar(card) => {
                out.u8(2);
                card.save(out);
            }
        }
    }

    fn restore(
        &mut self,
        input: &mut crate::snapshot::In<'_>,
    ) -> Result<(), crate::snapshot::Torn> {
        match (input.u8()?, self) {
            (0, Self::Mono(card)) => card.restore(input),
            (1, Self::Cga(card)) => card.restore(input),
            (2, Self::Planar(card)) => card.restore(input),
            (0..=2, _) => Err(crate::snapshot::Torn::Mismatch),
            _ => Err(crate::snapshot::Torn::Malformed),
        }
    }
}
