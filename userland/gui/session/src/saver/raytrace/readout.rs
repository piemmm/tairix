//! The readout over a reveal: what is under way and how far, small and
//! mid-grey in the screen's lower right, in a window of its own above the
//! picture's so the picture's buffer never holds a letter.

use alloc::format;
use alloc::string::String;

use tairix_font::BitmapFont;
use tairix_theme::{TextRole, Theme};
use tairix_wm::{Color, Compositor, Point, PointerCatch, Scale, Surface, WindowId};

/// How far in from the screen's corner the readout stands, in logical pixels.
const MARGIN_LOGICAL: u32 = 24;

/// The readout's ink: grey enough to stay out of the picture's way on black
/// or on a bright sky.
const INK: Color = Color::rgb(0x80, 0x80, 0x80);

/// What a reveal is doing.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Doing {
    /// Its scene is being prepared.
    Generating,
    /// Its picture is being traced.
    Rendering,
}

impl Doing {
    /// The readout's words for `self`, `percent` of the way.
    fn label(self, percent: u16) -> String {
        match self {
            Self::Generating => format!("Generating scene... {percent}%"),
            Self::Rendering => format!("Rendering... {percent}%"),
        }
    }
}

/// The readout of one screen's reveals.
pub(super) struct Readout {
    font: BitmapFont,
    /// Where its window stands, and its size: as wide as the widest it can
    /// read, so it never needs another size.
    origin: Point,
    size: (u32, u32),
    window: Option<WindowId>,
    shown: Option<(Doing, u16)>,
}

impl Readout {
    /// A readout for a `screen` at `scale`, in `theme`'s caption type.
    pub(super) fn new(theme: &Theme, scale: Scale, screen: (u32, u32)) -> Self {
        let font = BitmapFont::for_role(theme.fonts(), TextRole::Caption, scale);
        let width = [Doing::Generating, Doing::Rendering]
            .into_iter()
            .map(|doing| font.text_width(&doing.label(100)))
            .max()
            .unwrap_or(0)
            .max(1);
        let height = font.line_height().max(1);
        let margin = scale.scale_length(MARGIN_LOGICAL);
        let place = |extent: u32, own: u32| {
            i32::try_from(extent.saturating_sub(margin.saturating_add(own))).unwrap_or(0)
        };
        Self {
            font,
            origin: Point::new(place(screen.0, width), place(screen.1, height)),
            size: (width, height),
            window: None,
            shown: None,
        }
    }

    /// Read `doing`, `thousandths` of the way, over the picture in `owner`:
    /// redrawn only when the whole percentage changes.
    pub(super) fn show(
        &mut self,
        compositor: &mut Compositor,
        owner: WindowId,
        doing: Doing,
        thousandths: u16,
    ) {
        let percent = thousandths.min(1000) / 10;
        if self.shown == Some((doing, percent)) && self.window.is_some() {
            return;
        }
        let Some(surface) = self.lettered(&doing.label(percent)) else {
            return;
        };
        let placed = if let Some(window) = self.window {
            compositor.set_surface(window, surface)
        } else {
            self.window = compositor.add_transient_window(owner, self.origin, surface);
            if let Some(window) = self.window {
                compositor.set_pointer_catch(window, PointerCatch::None);
            }
            self.window.is_some()
        };
        if !placed {
            self.window = None;
        }
        self.shown = placed.then_some((doing, percent));
    }

    /// Take the readout off the screen.
    pub(super) fn take_down(&mut self, compositor: &mut Compositor) {
        if let Some(window) = self.window.take() {
            let _ = compositor.remove(window);
        }
        self.shown = None;
    }

    /// `text` right-aligned on a clear surface of the readout's size; `None`
    /// when the heap will not give one.
    fn lettered(&self, text: &str) -> Option<Surface> {
        let mut surface = Surface::new(self.size.0, self.size.1)?;
        let x = self.size.0.saturating_sub(self.font.text_width(text));
        let _ = self
            .font
            .draw_text(&mut surface, i32::try_from(x).unwrap_or(0), 0, text, INK);
        Some(surface)
    }

    /// The readout's window while it is up.
    #[cfg(test)]
    pub(super) const fn window(&self) -> Option<WindowId> {
        self.window
    }
}
