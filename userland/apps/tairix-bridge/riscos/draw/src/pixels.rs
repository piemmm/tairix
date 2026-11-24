//! Draw's fill for a module with a path of its own to fill, as RISC OS 5's
//! Font Manager has Draw fill a glyph too big to anti-alias: a path in
//! Draw's user units, its curves flattened as Draw flattens them, and the
//! pixels its interior holds handed back as spans rather than plotted in the
//! graphics foreground.

use tairix_riscos_kernel::os::{Fail, Os};
use tairix_riscos_vdu::render;

pub use crate::geometry::Point;
pub use crate::path::Element;

use crate::fill::{most_segments, Fill, Surface, Use};
use crate::flatten::Threshold;
use crate::module::FILL_STYLE;
use crate::stage::{Closer, Flattener, Guest, Sink, Span};

/// The default flatness, as a nought asks for it.
const DEFAULT_FLATNESS: u32 = 0;

/// What takes the spans a fill makes: each a row and its first and last
/// columns.
pub type Spans<'s> = dyn FnMut(&mut Os<'_>, &[Span]) -> Result<(), Fail> + 's;

/// The pixels `Draw_Fill` fills for `path` on `surface` with its default
/// style and flatness, each subpath closed, handed to `spans` as rows and
/// their first and last columns.
///
/// # Errors
///
/// What `spans` gives, the host's refusal of memory, or a stop it asks
/// for.
pub fn fill_interior(
    os: &mut Os<'_>,
    path: &[Element],
    surface: &render::Surface,
    spans: &mut Spans<'_>,
) -> Result<(), Fail> {
    let room = most_segments(os.ceiling_bytes());
    let using = Use {
        each: false,
        make_clip: false,
        within_clip: false,
    };
    let mut fill = Fill::new(
        Surface::of(surface),
        FILL_STYLE,
        using,
        alloc::vec::Vec::new(),
        room,
    );
    let mut flatten = Flattener::new(&mut fill, Threshold::of(DEFAULT_FLATNESS));
    let mut close = Closer::new(&mut flatten);
    let mut guest = Handed { os, spans };
    let walked = path
        .iter()
        .try_for_each(|&element| close.element(&mut guest, element))
        .and_then(|()| close.finish(&mut guest));
    walked.map_err(|stop| crate::module::raise(guest.os, stop))
}

/// The session, with the spans a fill plots handed on.
struct Handed<'o, 'a, 's> {
    os: &'o mut Os<'a>,
    spans: &'s mut Spans<'s>,
}

impl Guest for Handed<'_, '_, '_> {
    fn read_word(&mut self, at: u32) -> Result<u32, Fail> {
        self.os.read_word(at)
    }

    fn write_word(&mut self, at: u32, value: u32) -> Result<(), Fail> {
        self.os.write_word(at, value)
    }

    fn plot(&mut self, spans: &[Span]) -> Result<(), Fail> {
        (self.spans)(self.os, spans)
    }

    fn yield_point(&mut self) -> Result<(), Fail> {
        self.os.yield_point().map_err(Fail::Unwind)
    }
}

#[cfg(test)]
mod tests;
