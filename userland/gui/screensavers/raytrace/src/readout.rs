//! The readout over a reveal: what is under way and how far, small and
//! mid-grey in the screen's lower right. It is laid over the picture with what
//! it covers kept aside, and lifted off again before the picture changes, so
//! the picture itself never holds a letter.

use alloc::format;
use alloc::string::String;

use tairix_font::BitmapFont;
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_raster::{Color, Surface};
use tairix_theme::{TextRole, Theme};

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
    /// Where it stands: as wide as the widest it can read, so it never needs
    /// another size.
    place: Rect,
    /// What it covers of the picture while it is laid on.
    under: Surface,
    /// What it reads while it is laid on.
    shown: Option<(Doing, u16)>,
    /// The words it reads and where they start, made when what it reads
    /// changes rather than each time it is laid on.
    text: String,
    left: i32,
    /// What it is to read when next laid on; `None` to come off.
    wanted: Option<(Doing, u16)>,
}

impl Readout {
    /// A readout for a `screen` at `scale`, in `theme`'s caption type; `None`
    /// when the heap will not hold what it covers.
    pub(super) fn new(theme: &Theme, scale: Scale, screen: (u32, u32)) -> Option<Self> {
        let font = BitmapFont::for_role(theme.fonts(), TextRole::Caption, scale);
        let width = [Doing::Generating, Doing::Rendering]
            .into_iter()
            .map(|doing| font.text_width(&doing.label(100)))
            .max()
            .unwrap_or(0)
            .clamp(1, screen.0.max(1));
        let height = font.line_height().clamp(1, screen.1.max(1));
        let margin = scale.scale_length(MARGIN_LOGICAL);
        let place = |extent: u32, own: u32| {
            i32::try_from(extent.saturating_sub(margin.saturating_add(own))).unwrap_or(0)
        };
        let origin = Point::new(place(screen.0, width), place(screen.1, height));
        Some(Self {
            font,
            place: Rect::new(origin.x, origin.y, width, height),
            under: Surface::new(width, height)?,
            shown: None,
            text: String::new(),
            left: origin.x,
            wanted: None,
        })
    }

    /// Read `doing`, `thousandths` of the way, when next laid on: changed on
    /// screen only when the whole percentage changes.
    pub(super) fn show(&mut self, doing: Doing, thousandths: u16) {
        self.wanted = Some((doing, thousandths.min(1000) / 10));
    }

    /// Take the readout off when next laid on.
    pub(super) fn take_down(&mut self) {
        self.wanted = None;
    }

    /// Lift the readout off `surface`, laying back what it covered, before
    /// the picture beneath it changes.
    pub(super) fn lift(&self, surface: &mut Surface) {
        if self.shown.is_some() {
            surface.overwrite(self.place.left(), self.place.top(), &self.under);
        }
    }

    /// Lay the readout on `surface` as it is wanted, keeping aside what it
    /// covers, and add its place to `damage` where what it shows changed. The
    /// picture changing beneath it is that change's own damage.
    pub(super) fn lay(&mut self, surface: &mut Surface, damage: &mut Region) {
        let Some((doing, percent)) = self.wanted else {
            if self.shown.take().is_some() {
                damage.add(self.place);
            }
            return;
        };
        let Some((x, y)) = self.place.surface_origin() else {
            return;
        };
        self.under.overwrite(-to_i32(x), -to_i32(y), surface);
        if self.shown != self.wanted {
            self.text = doing.label(percent);
            let width = to_i32(self.font.text_width(&self.text));
            self.left = (self.place.right() - width).max(self.place.left());
            damage.add(self.place);
        }
        let _ = self
            .font
            .draw_text(surface, self.left, self.place.top(), &self.text, INK);
        self.shown = self.wanted;
    }

    /// Where the readout stands.
    #[cfg(test)]
    pub(super) const fn place(&self) -> Rect {
        self.place
    }
}

#[cfg(test)]
mod tests {
    use tairix_geometry::{Region, Scale};
    use tairix_raster::Surface;
    use tairix_theme::Theme;

    use super::{Doing, Readout};

    /// The words are made when the reading changes, not each time the
    /// readout is laid back on after the picture beneath it moved.
    #[test]
    fn the_words_are_remade_only_when_the_reading_changes() {
        let screen = (640, 360);
        let mut readout = Readout::new(&Theme::dark(), Scale::ONE, screen).expect("a readout");
        let mut surface = Surface::new(screen.0, screen.1).expect("a screen");
        let mut damage = Region::new();
        readout.show(Doing::Rendering, 105);
        readout.lay(&mut surface, &mut damage);
        assert_eq!(readout.text, "Rendering... 10%");
        assert!(!damage.is_empty(), "a new reading is drawn");
        let left = readout.left;
        damage.clear();
        readout.lift(&mut surface);
        readout.show(Doing::Rendering, 109);
        readout.lay(&mut surface, &mut damage);
        assert!(damage.is_empty(), "the same whole percentage is no change");
        assert_eq!(
            (readout.text.as_str(), readout.left),
            ("Rendering... 10%", left)
        );
        readout.lift(&mut surface);
        readout.show(Doing::Generating, 999);
        readout.lay(&mut surface, &mut damage);
        assert_eq!(readout.text, "Generating scene... 99%");
        assert!(readout.left < left, "longer words start further left");
    }
}
