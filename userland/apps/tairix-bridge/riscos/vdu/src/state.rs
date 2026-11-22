//! The VDU drivers' state for the session's screen, held as a kernel
//! extension, and the one way every part reaches it.
//!
//! The state is set aside while a part works on it, so a part that calls
//! out — a service, a vector, a SWI — does so between two uses of it,
//! never inside one: whatever the call reaches may come back to the VDU
//! drivers and find them whole.

use tairix_riscos_kernel::os::{Ended, Fail, Os, Unwind};

use crate::colour::Colours;
use crate::font::Font;
use crate::frames::Frames;
use crate::output::Output;
use crate::palette::Palette;
use crate::plot::Graphics;
use crate::pointer::Pointer;
use crate::screen::Screen;
use crate::sprite::Sprites;
use crate::stream::Stream;
use crate::teletext::Teletext;
use crate::text::Text;

/// Everything the VDU drivers keep.
pub(crate) struct Vdu {
    pub(crate) screen: Screen,
    pub(crate) text: Text,
    pub(crate) graphics: Graphics,
    pub(crate) colours: Colours,
    pub(crate) palette: Palette,
    pub(crate) font: Font,
    pub(crate) stream: Stream,
    pub(crate) pointer: Pointer,
    pub(crate) frames: Frames,
    pub(crate) teletext: Teletext,
    pub(crate) sprites: Sprites,
    /// Where the drivers draw, and what switches output back there.
    pub(crate) output: Output,
    pub(crate) context: crate::output::Context,
    /// The drivers' own save area, `OS_SpriteOp` 60's save area 1.
    pub(crate) kernel_save: Option<crate::saved::Saved>,
    /// The current monitor type.
    pub(crate) monitor: u32,
    /// `OS_Byte` 114: the modes selected from now on take the second bank.
    pub(crate) shadow: bool,
    /// The depth of `OS_RemoveCursors` calls not yet restored.
    pub(crate) cursors_removed: u32,
}

/// `work` on the drivers' state and the session together.
///
/// # Errors
///
/// What `work` gives; a session whose drivers never started ends as the
/// kernel's own fault would end it.
pub(crate) fn with<R>(
    os: &mut Os<'_>,
    work: impl FnOnce(&mut Vdu, &mut Os<'_>) -> Result<R, Fail>,
) -> Result<R, Fail> {
    os.with_extension::<Vdu, _>(|vdu, os| work(vdu, os))
        .unwrap_or(Err(Fail::Unwind(Unwind::Ended(Ended::Fault))))
}

/// As [`with`], for work that cannot fail.
pub(crate) fn read<R>(os: &mut Os<'_>, work: impl FnOnce(&mut Vdu, &mut Os<'_>) -> R) -> Option<R> {
    os.with_extension::<Vdu, _>(|vdu, os| work(vdu, os))
}
