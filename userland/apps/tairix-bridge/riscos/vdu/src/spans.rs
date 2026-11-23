//! Runs of pixels plotted in the graphics foreground for a module that scan
//! converts its own shapes, as RISC OS's Draw module plots through the VDU
//! drivers' horizontal line.

use tairix_riscos_kernel::mouse::Bounds;
use tairix_riscos_kernel::os::{Fail, Os};

use crate::canvas::Block;
use crate::plot::{ink_block, Ink};
use crate::state::{read, with};

/// What a scan converter needs of where output goes: the screen, or the
/// sprite output is switched to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Surface {
    /// Each axis's eigen factor: an OS unit's share of a pixel, as a shift.
    pub eig: (u32, u32),
    /// The graphics origin, in OS units from the bottom left.
    pub origin: (i32, i32),
    /// The graphics window, in pixels, inclusive.
    pub window: Bounds,
}

/// Where output goes now; `None` where it cannot show graphics.
pub fn surface(os: &mut Os<'_>) -> Option<Surface> {
    read(os, |vdu, _| {
        let format = vdu.format();
        format.graphic().then(|| Surface {
            eig: (format.x_eig(), format.y_eig()),
            origin: vdu.graphics.origin,
            window: vdu.graphics.window,
        })
    })
    .flatten()
}

/// Plots `spans`, each a row and its first and last column in pixels, in
/// the graphics foreground, within the graphics window.
///
/// # Errors
///
/// The abort a write to the screen takes.
pub fn plot_foreground(os: &mut Os<'_>, spans: &[(i32, i32, i32)]) -> Result<(), Fail> {
    let block = ink_block(os, Ink::Foreground)?;
    plot(os, spans.iter().copied(), &block)
}

/// Plots `spans` with `block`, where output can show graphics.
pub(crate) fn plot(
    os: &mut Os<'_>,
    spans: impl IntoIterator<Item = (i32, i32, i32)>,
    block: &Block,
) -> Result<(), Fail> {
    with(os, |vdu, os| {
        if !vdu.format().graphic() {
            return Ok(());
        }
        vdu.draw_spans(os, spans, block)
    })
}
