//! `Draw_ProcessPath`: a path read, passed through the stages its flags
//! ask for, and handed where R7 says, held to RISC OS 5.30's recorded
//! behaviour.
//!
//! R7 nought rewrites the path in place, which RISC OS refuses for any stage
//! that could change its length; one fills the path, two fills it a subpath
//! at a time; three counts the bytes the output would take; with bit 31 set
//! it is a box to write the bounding box to, and otherwise the buffer to
//! write the path to. Flag bit 26 makes R7 a full word's address, with bit
//! 25 its box's.

use alloc::vec::Vec;

use crate::dash::{Dasher, Pattern};
use crate::fill::{Fill, Surface, Use};
use crate::flatten::Threshold;
use crate::geometry::{Matrix, Point};
use tairix_drawing::riscos::format::TAG;
use tairix_riscos_kernel::os::Fail;

use crate::path::Element;
use crate::read::Reader;
use crate::sink::{write_element, Bounds, Count, Output};
use crate::stage::{Closer, Flattener, Guest, Refusal, Sink, Span, Steps, Stop, Transformer};
use crate::thicken::Thickener;

/// The flag bits of R1 above the fill style.
pub(crate) mod flag {
    /// The pixels a fill would plot become the clip.
    pub(crate) const MAKE_CLIP: u32 = 1 << 6;
    /// A fill plots only the pixels the clip holds.
    pub(crate) const WITHIN_CLIP: u32 = 1 << 7;
    /// R7 is a box for the bounding box: with [`WIDE`] only.
    pub(crate) const BOUNDS: u32 = 1 << 25;
    /// R7 is a full word's address.
    pub(crate) const WIDE: u32 = 1 << 26;
    pub(crate) const CLOSE: u32 = 1 << 27;
    pub(crate) const FLATTEN: u32 = 1 << 28;
    pub(crate) const THICKEN: u32 = 1 << 29;
    pub(crate) const REFLATTEN: u32 = 1 << 30;
    /// Floating-point output, which RISC OS has never had.
    pub(crate) const FLOAT: u32 = 1 << 31;
    /// The bits a call may set but [`FLOAT`].
    pub(crate) const KNOWN: u32 = 0xFF | BOUNDS | WIDE | CLOSE | FLATTEN | THICKEN | REFLATTEN;
}

/// R6 for a call that dashes nothing.
pub(crate) const NO_DASH: u32 = 0;

/// R7's values that are not addresses.
pub(crate) mod special {
    pub(crate) const IN_PLACE: u32 = 0;
    pub(crate) const FILL: u32 = 1;
    pub(crate) const FILL_EACH: u32 = 2;
    pub(crate) const COUNT: u32 = 3;
    /// Set in an address for the bounding box.
    pub(crate) const BOUNDS: u32 = 1 << 31;
}

/// R0 after a plot: RISC OS leaves one of two internal values, the second
/// where exterior pixels were asked for.
pub(crate) const PLOTTED: u32 = 0x7FFF_FFFF;
pub(crate) const PLOTTED_EXTERIOR: u32 = 0x8000_0000;

/// What a `Draw_ProcessPath` asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Call {
    pub(crate) path: u32,
    pub(crate) flags: u32,
    pub(crate) matrix: u32,
    pub(crate) flatness: u32,
    pub(crate) thickness: i32,
    pub(crate) style: u32,
    pub(crate) dash: u32,
    pub(crate) output: u32,
}

/// Where the processed path goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Destination {
    InPlace,
    Fill { each: bool },
    Count,
    Bounds(u32),
    Path(u32),
}

impl Call {
    /// The call R0 to R7 make.
    pub(crate) const fn of(r: &[u32; 8]) -> Self {
        Self {
            path: r[0],
            flags: r[1],
            matrix: r[2],
            flatness: r[3],
            thickness: r[4].cast_signed(),
            style: r[5],
            dash: r[6],
            output: r[7],
        }
    }

    /// Where it asks for the path to go.
    pub(crate) const fn destination(&self) -> Destination {
        if self.flags & flag::WIDE != 0 {
            return if self.flags & flag::BOUNDS != 0 {
                Destination::Bounds(self.output)
            } else {
                Destination::Path(self.output)
            };
        }
        match self.output {
            special::IN_PLACE => Destination::InPlace,
            special::FILL => Destination::Fill { each: false },
            special::FILL_EACH => Destination::Fill { each: true },
            special::COUNT => Destination::Count,
            at if at & special::BOUNDS != 0 => Destination::Bounds(at & !special::BOUNDS),
            at => Destination::Path(at),
        }
    }

    /// Refuses flags RISC OS refuses: reserved bits, a bounding box's flag
    /// without the full-word one, floating point; and in place, any stage
    /// that could change the path's length.
    pub(crate) const fn checked(&self) -> Result<(), Refusal> {
        let reserved = self.flags & !(flag::KNOWN | flag::FLOAT) != 0
            || (self.flags & flag::BOUNDS != 0 && self.flags & flag::WIDE == 0);
        if reserved {
            return Err(Refusal::Reserved);
        }
        if self.flags & flag::FLOAT != 0 {
            return Err(Refusal::Unsupported);
        }
        let lengthens = self.flags & (flag::CLOSE | flag::FLATTEN | flag::REFLATTEN) != 0
            || (self.flags & flag::THICKEN != 0 && self.thickness != 0)
            || self.dash != NO_DASH;
        if matches!(self.destination(), Destination::InPlace) && lengthens {
            return Err(Refusal::MayChangeLength);
        }
        Ok(())
    }

    fn threshold(&self) -> Threshold {
        Threshold::of(self.flatness)
    }
}

/// The matrix the call names, the identity for none.
pub(crate) fn matrix_at(guest: &mut dyn Guest, at: u32) -> Result<Matrix, Fail> {
    if at == 0 {
        return Ok(Matrix::IDENTITY);
    }
    let mut words = [0; 6];
    guest.read_words(at, &mut words)?;
    Ok(Matrix::from_words(words))
}

/// The clip fills are held within, which one may make anew.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Clip {
    pub(crate) spans: Vec<Span>,
}

/// Performs `call`, `surface` the screen a fill plots on and `room` the
/// bytes a fill's segments may take, answering with R0: the end element's
/// address for a path, the bytes for a count, and otherwise the value RISC
/// OS leaves.
///
/// # Errors
///
/// Where RISC OS would stop: what was output before the error stays.
pub(crate) fn process(
    guest: &mut dyn Guest,
    call: &Call,
    surface: Surface,
    clip: &mut Clip,
    room: u64,
) -> Result<u32, Stop> {
    call.checked()?;
    let matrix = matrix_at(guest, call.matrix)?;
    let destination = call.destination();
    let mut output: Option<Output> = None;
    let mut count: Option<Count> = None;
    let mut bounds: Option<Bounds> = None;
    let mut fill: Option<Fill> = None;
    let using = Use {
        each: matches!(destination, Destination::Fill { each: true }),
        make_clip: call.flags & flag::MAKE_CLIP != 0,
        within_clip: call.flags & flag::WITHIN_CLIP != 0,
    };
    let sink: &mut dyn Sink = match destination {
        Destination::Path(at) => output.insert(Output::new(guest, at)?),
        Destination::Count => count.insert(Count::default()),
        Destination::Bounds(at) => bounds.insert(Bounds::new(at)),
        Destination::Fill { .. } => {
            let held = core::mem::take(&mut clip.spans);
            fill.insert(Fill::new(
                surface,
                call.flags,
                using,
                held,
                crate::fill::most_segments(room),
            ))
        }
        Destination::InPlace => return in_place(guest, call, matrix),
    };
    let mut steps = Steps::new(sink);
    let sink: &mut dyn Sink = &mut steps;
    let mut transform: Option<Transformer<'_>> = None;
    let sink: &mut dyn Sink = if matrix.is_identity() {
        sink
    } else {
        transform.insert(Transformer::new(sink, matrix))
    };
    let mut reflatten: Option<Flattener<'_>> = None;
    let sink: &mut dyn Sink = if call.flags & flag::REFLATTEN == 0 {
        sink
    } else {
        reflatten.insert(Flattener::new(sink, call.threshold()))
    };
    let mut thicken: Option<Thickener<'_>> = None;
    let sink: &mut dyn Sink = if call.flags & flag::THICKEN == 0 {
        sink
    } else {
        thicken.insert(Thickener::new(sink, call.thickness, call.style))
    };
    let mut dash: Option<Dasher<'_>> = None;
    let sink: &mut dyn Sink = if call.dash == NO_DASH {
        sink
    } else {
        dash.insert(Dasher::new(sink, Pattern::at(call.dash)))
    };
    let mut flatten: Option<Flattener<'_>> = None;
    let sink: &mut dyn Sink = if call.flags & flag::FLATTEN == 0 {
        sink
    } else {
        flatten.insert(Flattener::new(sink, call.threshold()))
    };
    let mut close: Option<Closer<'_>> = None;
    let head: &mut dyn Sink = if call.flags & flag::CLOSE == 0 {
        sink
    } else {
        close.insert(Closer::new(sink))
    };
    let ran = run(guest, head, call.path);
    let closed_start = close.and_then(|closer| closer.last_start);
    if let Some(mut fill) = fill {
        let made = core::mem::take(&mut fill.made);
        clip.spans = if using.make_clip {
            crate::fill::normalised(made)
        } else {
            fill.into_clip()
        };
        ran?;
        return Ok(if call.flags & crate::raster::class::EXTERIOR != 0 {
            PLOTTED_EXTERIOR
        } else {
            PLOTTED
        });
    }
    ran?;
    if let Some(output) = output {
        return Ok(output.end());
    }
    if let Some(count) = count {
        return Ok(count.bytes);
    }
    let last_x = match (closed_start, bounds) {
        (Some(start), Some(_)) => start.x,
        (None, Some(bounds)) => bounds.last_x,
        (_, None) => 0,
    };
    Ok(last_x.cast_unsigned())
}

fn run(guest: &mut dyn Guest, head: &mut dyn Sink, path: u32) -> Result<(), Stop> {
    let mut reader = Reader::new(path);
    while let Some(read) = reader.next(guest)? {
        head.element(guest, read.element)?;
    }
    head.finish(guest)
}

/// The path rewritten over itself, each point transformed and, for the
/// thinnest line, each subpath made to add nothing to the winding number;
/// the client bits and the end element kept. RISC OS leaves R0 the x of
/// the last point written, a close's being its subpath's start.
fn in_place(guest: &mut dyn Guest, call: &Call, matrix: Matrix) -> Result<u32, Stop> {
    let thin = call.flags & flag::THICKEN != 0;
    let mut reader = Reader::new(call.path);
    let mut start = Point::default();
    let mut last_x = 0;
    while let Some(read) = reader.next(guest)? {
        let element = match read.element {
            Element::Move { to, .. } if thin => Element::Move { to, winds: false },
            element => element,
        };
        let element =
            element.try_map(|p| matrix.apply(p).ok_or(Stop::Refused(Refusal::Overflow)))?;
        let word = (read.word & !TAG) | element.code();
        write_element(guest, read.at, word, element)?;
        last_x = match element {
            Element::Move { to, .. } => {
                start = to;
                to.x
            }
            Element::Close { .. } => start.x,
            Element::Curve { to, .. } | Element::Line { to } | Element::Gap { to } => to.x,
        };
    }
    Ok(last_x.cast_unsigned())
}

#[cfg(test)]
mod tests;
