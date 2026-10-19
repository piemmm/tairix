//! The sheet the desktop states an application's identity in, hung where a
//! submenu of its icon-bar menu would: an *About* band, the bundle's picture
//! beside its name and version, its purpose as prose, and the remaining facts
//! below a rule.
//!
//! The words are the bundle's signed manifest's, read by the host before the
//! sheet is built, so an application cannot state an identity that is not its
//! own. The sheet draws on a plate its host has laid, exactly as every plate
//! of the chain it hangs from does.

use alloc::format;
use alloc::string::String;

use tairix_geometry::{Rect, Scale};
use tairix_icon::{IconKind, IconPicture};
use tairix_raster::{Color, Surface};
use tairix_theme::{TextRole, Theme};

use crate::menu::RowIcon;
use crate::paint::{paint_icon_slot, role_font, surface_rect, to_i32, withheld, TextBlock};
use crate::record::FactList;
use crate::window::TitleBar;

/// The least logical width the sheet is laid out in, at the reference density.
pub const ABOUT_MIN_WIDTH: u32 = 280;

/// The most logical width the sheet is laid out in: past it the purpose wraps
/// rather than the sheet growing.
pub const ABOUT_MAX_WIDTH: u32 = 400;

/// The logical side of the bundle's picture beside its name.
pub const ABOUT_ICON_SIDE: u32 = 48;

/// The most lines the purpose may take before it is cut, marked as cut.
const PURPOSE_LINES: usize = 8;

/// The identity sheet of one application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AboutSheet {
    band: TitleBar,
    name: String,
    version: String,
    purpose: Option<String>,
    facts: FactList,
    icon: IconKind,
    source: Option<RowIcon>,
    artwork: Option<Surface>,
}

/// Where each part of the sheet sits, the one definition measure and paint
/// both read.
struct Layout {
    band: Rect,
    icon: (u32, u32, u32),
    name: (u32, u32),
    version: (u32, u32),
    purpose: (u32, u32),
    rule: Option<Rect>,
    facts: Rect,
    height: u32,
}

impl AboutSheet {
    /// The sheet of the application `name` at `version`, stating `purpose`
    /// when its manifest does and `facts` below it, drawn with `icon`'s
    /// built-in picture until [`set_artwork`](Self::set_artwork) gives it
    /// the bundle's own.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        purpose: Option<String>,
        facts: FactList,
        icon: IconKind,
    ) -> Self {
        let name = name.into();
        let mut band = TitleBar::plate();
        band.set_title(&format!("About {name}"));
        Self {
            band,
            name,
            version: version.into(),
            purpose: purpose.filter(|purpose| !purpose.is_empty()),
            facts,
            icon,
            source: None,
            artwork: None,
        }
    }

    /// Resolve the sheet's picture from `source`, the host's to decode; the
    /// built-in glyph stands until [`set_artwork`](Self::set_artwork) gives
    /// it the picture.
    pub fn set_source(&mut self, source: RowIcon) {
        self.source = Some(source);
    }

    /// Where the sheet's picture is resolved from, when its host named it.
    #[must_use]
    pub const fn source(&self) -> Option<&RowIcon> {
        self.source.as_ref()
    }

    /// Show `artwork`, the bundle's own picture rasterised at
    /// [`ABOUT_ICON_SIDE`] scaled, in place of the built-in glyph.
    pub fn set_artwork(&mut self, artwork: Surface) {
        self.artwork = Some(artwork);
    }

    /// The bundle's picture, once its host gave it one.
    #[must_use]
    pub const fn artwork(&self) -> Option<&Surface> {
        self.artwork.as_ref()
    }

    /// The application's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The application's version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// What the application is for, when its manifest says.
    #[must_use]
    pub fn purpose(&self) -> Option<&str> {
        self.purpose.as_deref()
    }

    /// The facts stated below the rule.
    #[must_use]
    pub const fn facts(&self) -> &FactList {
        &self.facts
    }

    /// The extent the sheet draws at, band included: as wide as its widest
    /// line between [`ABOUT_MIN_WIDTH`] and [`ABOUT_MAX_WIDTH`], and as tall
    /// as what it states at that width.
    #[must_use]
    pub fn measured_size(&self, scale: Scale, theme: &Theme) -> (u32, u32) {
        let width = self.width(scale, theme);
        (width, self.layout(width, scale, theme).height)
    }

    /// Paint the sheet into `bounds` of `surface`, over the plate its host
    /// laid there.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        if withheld(surface, bounds) {
            return;
        }
        let Some((x, y, w, _)) = surface_rect(bounds) else {
            return;
        };
        let layout = self.layout(w, scale, theme);
        let at = |(px, py): (u32, u32)| (x.saturating_add(px), y.saturating_add(py));
        let shift = |rect: Rect| {
            Rect::new(
                rect.left().saturating_add(to_i32(x)),
                rect.top().saturating_add(to_i32(y)),
                rect.width,
                rect.height,
            )
        };
        self.band
            .render(surface, shift(layout.band), scale, theme, None);
        let palette = theme.palette();
        let (ix, iy, side) = layout.icon;
        let picture = self.artwork.as_ref().map(IconPicture::coloured);
        paint_icon_slot(
            surface,
            (x.saturating_add(ix), y.saturating_add(iy), side),
            self.icon,
            Color::from(palette.on_surface),
            picture,
            crate::ArtworkTone::FULL,
        );
        let text_w = Self::text_width(w, scale, theme);
        Self::name_block(text_w, scale, theme).paint(surface, &self.name, at(layout.name));
        Self::version_block(text_w, scale, theme).paint(
            surface,
            &self.version_line(),
            at(layout.version),
        );
        if let Some(purpose) = &self.purpose {
            Self::purpose_block(Self::inner_width(w, scale, theme), scale, theme).paint(
                surface,
                purpose,
                at(layout.purpose),
            );
        }
        if let Some(rule) = layout.rule {
            let rule = shift(rule);
            if let Some((rx, ry, rw, rh)) = surface_rect(rule) {
                surface.fill_rect(rx, ry, rw, rh, Color::from(palette.border));
            }
        }
        self.facts
            .render(surface, shift(layout.facts), scale, theme);
    }

    /// "Version 1.2.3", the line beneath the name.
    fn version_line(&self) -> String {
        format!("Version {}", self.version)
    }

    fn pad(scale: Scale, theme: &Theme) -> u32 {
        scale
            .scale_length(theme.metrics().control_inset.saturating_mul(2))
            .max(1)
    }

    fn gap(scale: Scale, theme: &Theme) -> u32 {
        scale.scale_length(theme.metrics().control_gap).max(1)
    }

    fn icon_side(scale: Scale) -> u32 {
        scale.scale_length(ABOUT_ICON_SIDE).max(1)
    }

    /// The width the sheet is laid out in: what its name, version line and
    /// facts need side by side, held to its floor and ceiling.
    fn width(&self, scale: Scale, theme: &Theme) -> u32 {
        let floor = scale.scale_length(ABOUT_MIN_WIDTH);
        let ceiling = scale.scale_length(ABOUT_MAX_WIDTH).max(floor);
        let pad = Self::pad(scale, theme);
        let beside = Self::icon_side(scale).saturating_add(Self::gap(scale, theme));
        let head = Self::name_block(ceiling, scale, theme)
            .measured_width(&self.name)
            .max(Self::version_block(ceiling, scale, theme).measured_width(&self.version_line()))
            .saturating_add(beside);
        let wanted = head
            .max(self.facts.measured_width(scale, theme))
            .saturating_add(pad.saturating_mul(2));
        wanted.clamp(floor, ceiling)
    }

    /// The width inside the sheet's padding.
    fn inner_width(width: u32, scale: Scale, theme: &Theme) -> u32 {
        width.saturating_sub(Self::pad(scale, theme).saturating_mul(2))
    }

    /// The width the name and version have beside the picture.
    fn text_width(width: u32, scale: Scale, theme: &Theme) -> u32 {
        Self::inner_width(width, scale, theme)
            .saturating_sub(Self::icon_side(scale))
            .saturating_sub(Self::gap(scale, theme))
    }

    fn name_block(width: u32, scale: Scale, theme: &Theme) -> TextBlock {
        TextBlock::prose(
            role_font(theme, scale, TextRole::ItemTitle),
            width,
            2,
            Color::from(theme.palette().on_surface),
        )
    }

    fn version_block(width: u32, scale: Scale, theme: &Theme) -> TextBlock {
        TextBlock::prose(
            role_font(theme, scale, TextRole::Caption),
            width,
            1,
            Color::from(theme.palette().on_surface_muted),
        )
    }

    fn purpose_block(width: u32, scale: Scale, theme: &Theme) -> TextBlock {
        TextBlock::prose(
            role_font(theme, scale, TextRole::Body),
            width,
            PURPOSE_LINES,
            Color::from(theme.palette().on_surface),
        )
    }

    /// Where every part sits in a sheet `width` pixels wide.
    fn layout(&self, width: u32, scale: Scale, theme: &Theme) -> Layout {
        let pad = Self::pad(scale, theme);
        let gap = Self::gap(scale, theme);
        let side = Self::icon_side(scale);
        let band_h = TitleBar::band_height(scale, theme);
        let text_w = Self::text_width(width, scale, theme);
        let top = band_h.saturating_add(pad);
        let name_h = Self::name_block(text_w, scale, theme).height(&self.name);
        let version_h = Self::version_block(text_w, scale, theme).height(&self.version_line());
        let words = name_h.saturating_add(version_h);
        // The name and version sit centred beside the picture, or the picture
        // beside them when they take more room.
        let head_h = side.max(words);
        let text_x = pad.saturating_add(side).saturating_add(gap);
        let words_top = top.saturating_add(head_h.saturating_sub(words) / 2);
        let icon_top = top.saturating_add(head_h.saturating_sub(side) / 2);
        let mut y = top.saturating_add(head_h);
        let purpose_top = y.saturating_add(gap);
        if let Some(purpose) = &self.purpose {
            let inner = Self::inner_width(width, scale, theme);
            y = purpose_top
                .saturating_add(Self::purpose_block(inner, scale, theme).height(purpose));
        }
        let rule = (!self.facts.is_empty()).then(|| {
            let thickness = scale.scale_length(1).max(1);
            let at = y.saturating_add(gap);
            y = at.saturating_add(thickness);
            Rect::new(
                to_i32(pad),
                to_i32(at),
                Self::inner_width(width, scale, theme),
                thickness,
            )
        });
        let facts_top = if rule.is_some() {
            y.saturating_add(gap)
        } else {
            y
        };
        let facts_h = self.facts.measured_height(scale, theme);
        let facts = Rect::new(
            to_i32(pad),
            to_i32(facts_top),
            Self::inner_width(width, scale, theme),
            facts_h,
        );
        Layout {
            band: Rect::new(0, 0, width, band_h),
            icon: (pad, icon_top, side),
            name: (text_x, words_top),
            version: (text_x, words_top.saturating_add(name_h)),
            purpose: (pad, purpose_top),
            rule,
            facts,
            height: facts_top.saturating_add(facts_h).saturating_add(pad),
        }
    }
}

#[cfg(test)]
#[path = "about_tests.rs"]
mod tests;
