//! Where the VDU drivers draw (`plans/RISCOS-EMULATOR.md` §8): the screen,
//! or the image or mask of a sprite output has been switched to by
//! `OS_SpriteOp` 60 or 61. Every part that draws, or reports what it draws
//! on, asks here; only the display, the frames and mode changes work on the
//! screen itself.

use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::service::SWITCHING_OUTPUT_TO_SPRITE;

use crate::canvas::Canvas;
use crate::mode::Format;
use crate::state::Vdu;

/// The registers `OS_SpriteOp` takes to switch output to a destination, as
/// it hands them back when output leaves it: the reason and area value, the
/// area, the sprite or nought for the screen, and the save area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Context(pub(crate) [u32; 4]);

/// A save area of nought: none, the state reset on each switch.
pub(crate) const NO_SAVE_AREA: u32 = 0;
/// A save area of one: the drivers' own.
pub(crate) const KERNEL_SAVE_AREA: u32 = 1;

impl Context {
    /// The screen's as the drivers start: by pointer, its state kept in the
    /// drivers' own save area.
    pub(crate) const SCREEN: Self = Self([
        crate::sprite::Naming::Pointer.value() | crate::sprite::reason::SWITCH_TO_SPRITE,
        0,
        0,
        KERNEL_SAVE_AREA,
    ]);

    /// The save area its state goes to as output leaves it.
    pub(crate) const fn save_area(&self) -> u32 {
        self.0[3]
    }
}

/// `Service_SwitchingOutputToSprite`, R2 to R5 the registers the switch was
/// made with.
pub(crate) fn announce(os: &mut Os<'_>, switched_with: [u32; 4]) -> Result<(), Fail> {
    let [r2, r3, r4, r5] = switched_with;
    let mut service = Regs::new([
        0,
        SWITCHING_OUTPUT_TO_SPRITE,
        r2,
        r3,
        r4,
        r5,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);
    os.service(&mut service).map_err(Fail::Unwind)
}

/// The destination drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Output {
    Screen,
    /// A sprite's image or mask: its pixels as a screen of `format` at
    /// `canvas`, as a sprite of mode word `word` holds them.
    Sprite {
        format: Format,
        canvas: Canvas,
        word: u32,
    },
}

impl Vdu {
    /// The destination's geometry and pixels.
    pub(crate) const fn format(&self) -> &Format {
        match &self.output {
            Output::Screen => &self.screen.format,
            Output::Sprite { format, .. } => format,
        }
    }

    /// Its memory, as the drivers write it.
    pub(crate) const fn canvas(&self) -> Canvas {
        match self.output {
            Output::Screen => self.screen.canvas(),
            Output::Sprite { canvas, .. } => canvas,
        }
    }

    /// Whether whole-screen text scrolls by moving the screen: never in a
    /// sprite.
    pub(crate) const fn hardware_scrolls(&self) -> bool {
        matches!(self.output, Output::Screen) && self.screen.hardware_scrolls()
    }
}
