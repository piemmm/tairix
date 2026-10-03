//! The picture choice: a one-of-several setting offered as pictures
//! (`plans/GUI-CONTROLS-DESIGN.md` §11.43).
//!
//! Every choice is a rounded picture at one fixed [`Aspect`] with its name
//! beneath, wrapping into lines under optional section titles. The owner hands
//! each picture over already rendered at [`PictureChoice::picture_size`] — a
//! control never decodes an image — and a choice whose picture has not arrived
//! draws its built-in glyph on a quiet frame, so the choice is usable from its
//! first frame and never blank. A choice that is a flat colour is a
//! [`Swatch`], which the control draws itself.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_colour::Rgba;
use tairix_font::BitmapFont;
use tairix_geometry::{GridFill, GridRun, Point, Rect, Region, Scale};
use tairix_icon::IconKind;
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::{Color, Ring, RingInk, Surface};
use tairix_theme::{TextRole, Theme};

use crate::paint::{
    foreground, heavy_contrast, paint_bead, paint_icon_slot, paint_run, plate_border,
    rail_thickness, resolve_bead, role_font, surface_rect, to_i32, withheld, FULL_COLOUR,
};
use crate::state::{ControlDisposition, ControlState, RenderInvariant};

/// A box on the surface: its left, top, width and height.
type Area = (u32, u32, u32, u32);

/// The shape every picture of a [`PictureChoice`] is drawn at, width to height.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Aspect {
    width: u32,
    height: u32,
}

impl Aspect {
    /// Sixteen by nine: a screen's own shape, so a wallpaper or a screensaver
    /// is previewed the way it is seen.
    pub const WIDESCREEN: Self = Self {
        width: 16,
        height: 9,
    };

    /// The aspect `width`:`height`, or `None` when either is zero.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }
        Some(Self { width, height })
    }

    /// The height a picture `width` pixels wide takes, rounded to nearest and
    /// never nothing.
    fn height_for(self, width: u32) -> u32 {
        let scaled = u64::from(width)
            .saturating_mul(u64::from(self.height))
            .saturating_add(u64::from(self.width) / 2)
            / u64::from(self.width);
        u32::try_from(scaled).unwrap_or(u32::MAX).max(1)
    }
}

/// The colour a [`Swatch`] choice is drawn in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Swatch {
    /// This colour.
    Fixed(Rgba),
    /// The empty desktop's colour in the theme the choice is drawn with.
    Desktop,
}

impl Swatch {
    fn colour(self, theme: &Theme) -> Rgba {
        match self {
            Self::Fixed(colour) => colour,
            Self::Desktop => theme.palette().desktop,
        }
    }
}

/// What a choice shows in its frame.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Face {
    /// A picture the owner renders, its glyph shown until it arrives.
    Picture {
        glyph: IconKind,
        art: Option<Surface>,
    },
    Swatch(Swatch),
}

/// One choice: its name, and the picture or colour it shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PictureItem {
    label: String,
    face: Face,
}

impl PictureItem {
    /// A choice named `label` with no picture yet, showing `glyph` meanwhile.
    #[must_use]
    pub fn new(label: impl Into<String>, glyph: IconKind) -> Self {
        Self {
            label: label.into(),
            face: Face::Picture { glyph, art: None },
        }
    }

    /// A choice named `label` that is the flat colour `swatch`: it takes no
    /// picture, and is never waiting for one.
    #[must_use]
    pub fn swatch(label: impl Into<String>, swatch: Swatch) -> Self {
        Self {
            label: label.into(),
            face: Face::Swatch(swatch),
        }
    }

    /// The choice's name.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The choice's picture, if the owner has handed one over.
    #[must_use]
    pub const fn art(&self) -> Option<&Surface> {
        match &self.face {
            Face::Picture { art, .. } => art.as_ref(),
            Face::Swatch(_) => None,
        }
    }

    /// Whether the choice shows a picture its owner renders, rather than a
    /// colour.
    #[must_use]
    pub const fn takes_art(&self) -> bool {
        matches!(self.face, Face::Picture { .. })
    }
}

/// A run of choices under one title: a category of pictures. An empty title
/// is a run with no heading, which is how a choice that needs no sections is
/// built.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PictureSection {
    title: String,
    items: Vec<PictureItem>,
}

impl PictureSection {
    /// The choices `items`, headed `title`.
    #[must_use]
    pub fn new(title: impl Into<String>, items: Vec<PictureItem>) -> Self {
        Self {
            title: title.into(),
            items,
        }
    }

    /// The choices `items`, with no heading.
    #[must_use]
    pub fn untitled(items: Vec<PictureItem>) -> Self {
        Self::new(String::new(), items)
    }

    /// The section's heading, empty for none.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The section's choices.
    #[must_use]
    pub fn items(&self) -> &[PictureItem] {
        &self.items
    }
}

/// What routing one event to a [`PictureChoice`] asked of its owner.
///
/// Choices are named by their position across every section in order, so an
/// owner maps them back through the list it built the choice from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PictureAction {
    /// The reader chose the picture at `index`.
    Chose {
        /// The chosen picture.
        index: usize,
    },
    /// The keyboard cursor moved to the picture at `index` without choosing
    /// it; an owner showing the choice through a scrolled view reveals it.
    Moved {
        /// The picture the cursor rests on now.
        index: usize,
    },
}

/// A one-of-several setting chosen by its picture.
///
/// The choice owns its layout — tiles of one size wrapping into lines through
/// the shared [`GridRun`] arithmetic, spread across the width — its selection,
/// its keyboard cursor, and the tile the pointer is over, and reports a typed
/// [`PictureAction`]. It commits nothing: choosing a picture only asks.
///
/// Equal choices draw the same pixels, so a host may use `==` as its repaint
/// gate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PictureChoice {
    sections: Vec<PictureSection>,
    aspect: Aspect,
    len: usize,
    selected: Option<usize>,
    cursor: usize,
    focused: bool,
    state: ControlState,
    hovered: Option<usize>,
    armed: Option<usize>,
    /// Where the pointer last moved: a press and a release carry no position
    /// of their own.
    pointer: RenderInvariant<Point>,
}

impl PictureChoice {
    /// A choice among `sections`' pictures at `aspect`, none chosen yet.
    #[must_use]
    pub fn new(aspect: Aspect, sections: Vec<PictureSection>) -> Self {
        let len = sections
            .iter()
            .map(|section| section.items.len())
            .fold(0, usize::saturating_add);
        Self {
            sections,
            aspect,
            len,
            selected: None,
            cursor: 0,
            focused: false,
            state: ControlState::idle(),
            hovered: None,
            armed: None,
            pointer: RenderInvariant::new(Point::ORIGIN),
        }
    }

    /// This choice with the picture at `index` chosen; an index past the last
    /// chooses none.
    #[must_use]
    pub fn with_selected(mut self, index: Option<usize>) -> Self {
        self.set_selected(index);
        self
    }

    /// Choose the picture at `index`, or none; an index past the last chooses
    /// none rather than a picture the reader never saw.
    pub fn set_selected(&mut self, index: Option<usize>) {
        self.selected = index.filter(|&at| at < self.len);
    }

    /// Which picture is chosen.
    #[must_use]
    pub const fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// How many pictures the choice offers, across every section.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the choice offers nothing at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The sections, in order.
    #[must_use]
    pub fn sections(&self) -> &[PictureSection] {
        &self.sections
    }

    /// The picture at `index`, counted across every section.
    #[must_use]
    pub fn item(&self, index: usize) -> Option<&PictureItem> {
        let (section, local) = self.locate(index)?;
        self.sections.get(section)?.items.get(local)
    }

    fn item_mut(&mut self, index: usize) -> Option<&mut PictureItem> {
        let (section, local) = self.locate(index)?;
        self.sections.get_mut(section)?.items.get_mut(local)
    }

    /// Which section `index` falls in, and where within it.
    fn locate(&self, index: usize) -> Option<(usize, usize)> {
        let mut first = 0usize;
        for (at, section) in self.sections.iter().enumerate() {
            let end = first.saturating_add(section.items.len());
            if index < end {
                return Some((at, index - first));
            }
            first = end;
        }
        None
    }

    /// Hand over the picture for `index`, answering whether that choice takes
    /// one: a swatch does not.
    ///
    /// The picture is drawn only while it is exactly
    /// [`picture_size`](Self::picture_size) at the scale and theme it is drawn
    /// with: one of another size — rendered before the scale moved — shows the
    /// glyph instead rather than a stretched or cut picture.
    pub fn set_art(&mut self, index: usize, art: Surface) -> bool {
        match self.item_mut(index).map(|item| &mut item.face) {
            Some(Face::Picture { art: held, .. }) => {
                *held = Some(art);
                true
            }
            Some(Face::Swatch(_)) | None => false,
        }
    }

    /// Repaint the swatch at `index` in `swatch`, answering whether that
    /// choice is a swatch: a colour the owner's setting decides moves with
    /// it without rebuilding the choice.
    pub fn set_swatch(&mut self, index: usize, swatch: Swatch) -> bool {
        match self.item_mut(index).map(|item| &mut item.face) {
            Some(Face::Swatch(held)) => {
                *held = swatch;
                true
            }
            Some(Face::Picture { .. }) | None => false,
        }
    }

    /// Take the picture for `index` back out, leaving its glyph, so an owner
    /// rebuilding the choice can carry it across without a copy.
    pub fn take_art(&mut self, index: usize) -> Option<Surface> {
        match &mut self.item_mut(index)?.face {
            Face::Picture { art, .. } => art.take(),
            Face::Swatch(_) => None,
        }
    }

    /// The pixel size every picture is drawn at under `scale` and `theme`:
    /// what an owner renders a picture at before handing it over.
    #[must_use]
    pub fn picture_size(&self, scale: Scale, theme: &Theme) -> (u32, u32) {
        Tile::of(self.aspect, scale, theme).art
    }

    /// The choice's composed state: its enablement and authority, which a
    /// denied or disabled choice refuses the pointer and the keyboard by.
    #[must_use]
    pub const fn state(&self) -> ControlState {
        self.state
    }

    /// Replace the choice's composed state.
    pub fn set_state(&mut self, state: ControlState) {
        self.state = state;
    }

    /// Give the choice the keyboard, its cursor on the chosen picture, or take
    /// it away.
    pub fn set_focused(&mut self, focused: bool) {
        if focused && !self.focused {
            self.cursor = self.selected.unwrap_or(0).min(self.len.saturating_sub(1));
        }
        self.focused = focused;
    }

    /// Whether the choice holds the keyboard.
    #[must_use]
    pub const fn is_focused(&self) -> bool {
        self.focused
    }

    /// The picture the keyboard cursor rests on.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Put the keyboard cursor on `index`, where there is such a picture.
    pub fn set_cursor(&mut self, index: usize) {
        if index < self.len {
            self.cursor = index;
        }
    }

    /// The narrowest width that seats one whole picture.
    #[must_use]
    pub fn natural_width(&self, scale: Scale, theme: &Theme) -> u32 {
        Tile::of(self.aspect, scale, theme).width
    }

    /// The height the choice needs in a column `width` pixels wide: every
    /// section's heading and its lines of pictures.
    #[must_use]
    pub fn measured_height(&self, width: u32, scale: Scale, theme: &Theme) -> u32 {
        let layout = Layout::of(self.aspect, width, scale, theme);
        self.placed(Rect::new(0, 0, width, 0), &layout)
            .last()
            .map_or(0, |section| u32::try_from(section.bottom).unwrap_or(0))
    }

    /// The tile the picture at `index` is drawn in, the choice laid out in
    /// `bounds`, or `None` when there is no such picture or no line seats one.
    #[must_use]
    pub fn item_rect(
        &self,
        index: usize,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Option<Rect> {
        let layout = Layout::of(self.aspect, bounds.width, scale, theme);
        let section = self
            .placed(bounds, &layout)
            .find(|section| (section.first..section.first + section.len).contains(&index))?;
        layout.tile_rect(bounds, section, index - section.first)
    }

    /// Visit every picture's tile, the choice laid out once in `bounds`, in
    /// order: the walk an owner asking of every picture makes, where
    /// [`item_rect`](Self::item_rect) lays the choice out for each question.
    pub fn for_each_item_rect(
        &self,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        mut visit: impl FnMut(usize, Rect),
    ) {
        let layout = Layout::of(self.aspect, bounds.width, scale, theme);
        for section in self.placed(bounds, &layout) {
            for local in 0..section.len {
                if let Some(rect) = layout.tile_rect(bounds, section, local) {
                    visit(section.first + local, rect);
                }
            }
        }
    }

    /// The picture whose tile holds `point`, the choice laid out in `bounds`:
    /// `None` in a heading, a gap between tiles, or past the last picture.
    #[must_use]
    pub fn item_at(
        &self,
        point: Point,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
    ) -> Option<usize> {
        let layout = Layout::of(self.aspect, bounds.width, scale, theme);
        let section = self
            .placed(bounds, &layout)
            .find(|section| (section.tiles_top..section.bottom).contains(&point.y))?;
        let per_line = layout.across.count();
        let down = u32::try_from(point.y - section.tiles_top).ok()?;
        let line = GridRun::fixed(layout.lines(section.len), layout.tile.height, layout.gap)
            .cell_at(down)?;
        let across = u32::try_from(point.x.checked_sub(bounds.left())?).ok()?;
        let slot = layout.across.cell_at(across)?;
        let local = line.checked_mul(per_line)?.checked_add(slot)?;
        (local < section.len).then_some(section.first + local)
    }

    /// Where each section is drawn in `bounds`: its first picture's index,
    /// how many it holds, and the tops and bottom it spans. A section holding
    /// no picture takes no room.
    fn placed<'a>(&'a self, bounds: Rect, layout: &'a Layout) -> Placed<'a> {
        Placed {
            sections: self.sections.iter(),
            layout,
            top: bounds.top(),
            first: 0,
            laid: false,
        }
    }

    /// Paint the choice into `surface`, laid out in `bounds`.
    ///
    /// Nothing is drawn outside `bounds`, and a tile a scrolled owner shows
    /// none of costs nothing.
    pub fn render(&self, surface: &mut Surface, bounds: Rect, scale: Scale, theme: &Theme) {
        if withheld(surface, bounds) {
            return;
        }
        let Some((cx, cy, cw, ch)) = surface_rect(bounds) else {
            return;
        };
        let layout = Layout::of(self.aspect, bounds.width, scale, theme);
        let ink = foreground(theme, self.state.disposition());
        surface.with_clip(cx, cy, cw, ch, |surface| {
            for section in self.placed(bounds, &layout) {
                Self::render_title(surface, bounds, &layout, section, ink);
                for local in 0..section.len {
                    let (Some(rect), Some(item)) = (
                        layout.tile_rect(bounds, section, local),
                        self.item(section.first + local),
                    ) else {
                        continue;
                    };
                    self.render_tile(
                        surface,
                        rect,
                        section.first + local,
                        item,
                        &layout,
                        (scale, theme),
                    );
                }
            }
        });
    }

    /// Paint `section`'s heading over its first tile's picture, where it has
    /// one.
    fn render_title(
        surface: &mut Surface,
        bounds: Rect,
        layout: &Layout,
        section: PlacedSection<'_>,
        ink: Color,
    ) {
        if section.title.is_empty() || section.len == 0 {
            return;
        }
        let lead = layout.across.lead().saturating_add(layout.tile.pad);
        let left = bounds.left().saturating_add(to_i32(lead));
        let room = bounds.width.saturating_sub(lead.saturating_mul(2));
        let run = layout.title.elide_to_width(section.title, room);
        paint_run(
            surface,
            layout.title,
            run,
            (left, section.title_top),
            ink,
            None,
        );
    }

    /// Paint one tile: its ground, its picture in its frame, its name, and the
    /// marks laid over it.
    fn render_tile(
        &self,
        surface: &mut Surface,
        rect: Rect,
        index: usize,
        item: &PictureItem,
        layout: &Layout,
        (scale, theme): (Scale, &Theme),
    ) {
        if withheld(surface, rect) {
            return;
        }
        let Some(at) = surface_rect(rect) else {
            return;
        };
        let tile = &layout.tile;
        let panelled = self.selected == Some(index) && heavy_contrast(theme);
        self.paint_ground(surface, at, index, tile, theme);
        let frame = tile.frame(at.0, at.1);
        self.paint_face(surface, frame, item, tile, theme);
        self.paint_label(surface, at, &item.label, tile, (theme, panelled));
        self.paint_marks(surface, (at, frame), index, tile, (scale, theme));
    }

    /// The tile's ground: a heavier contrast's selection panel or the
    /// pointer's wash, and the chosen tile's ring round its picture and name.
    fn paint_ground(
        &self,
        surface: &mut Surface,
        (x, y, w, h): Area,
        index: usize,
        tile: &Tile,
        theme: &Theme,
    ) {
        let palette = theme.palette();
        let chosen = self.selected == Some(index);
        let wash = if chosen && heavy_contrast(theme) {
            Some(palette.accent)
        } else if self.armed == Some(index) {
            Some(palette.surface_pressed)
        } else if self.hovered == Some(index) {
            Some(palette.surface_hover)
        } else {
            None
        };
        if let Some(fill) = wash {
            surface.fill_round_rect(x, y, w, h, tile.panel_radius, Color::from(fill));
        }
        if self.rings_choice(index, theme) {
            // The keyboard's ring joins this one as added weight rather than
            // standing beside it as a second edge.
            let weight = if self.focused && self.cursor == index {
                tile.pad
            } else {
                tile.ring
            };
            surface.wash_ring(
                x,
                y,
                w,
                h,
                Ring::uniform(tile.panel_radius, weight),
                RingInk::Solid(Color::from(palette.accent)),
            );
        }
    }

    /// Whether tile `index` is chosen under a contrast that marks the choice
    /// with a ring round the tile rather than a panel across it.
    fn rings_choice(&self, index: usize, theme: &Theme) -> bool {
        self.selected == Some(index) && !heavy_contrast(theme)
    }

    /// The picture in its rim: the owner's picture at the one size it is
    /// drawn at, a swatch's colour, or the glyph on a quiet ground meanwhile —
    /// half veiled while the choice is disabled, so it still shows what it
    /// holds.
    fn paint_face(
        &self,
        surface: &mut Surface,
        (fx, fy, fw, fh): Area,
        item: &PictureItem,
        tile: &Tile,
        theme: &Theme,
    ) {
        let palette = theme.palette();
        surface.fill_round_rect(fx, fy, fw, fh, tile.radius, Color::from(palette.rim));
        let (ax, ay) = (
            fx.saturating_add(tile.border),
            fy.saturating_add(tile.border),
        );
        let (aw, ah) = tile.art;
        let radius = tile.radius.saturating_sub(tile.border);
        match &item.face {
            Face::Picture { art: Some(art), .. } if (art.width(), art.height()) == tile.art => {
                surface.blit_rounded(to_i32(ax), to_i32(ay), art, radius);
            }
            Face::Swatch(swatch) => {
                surface.fill_round_rect(ax, ay, aw, ah, radius, Color::from(swatch.colour(theme)));
            }
            Face::Picture { glyph, .. } => {
                let ground = Color::from(palette.surface_raised);
                surface.fill_round_rect(ax, ay, aw, ah, radius, ground);
                let side = ah.saturating_mul(2) / 5;
                let slot = (
                    ax.saturating_add(aw.saturating_sub(side) / 2),
                    ay.saturating_add(ah.saturating_sub(side) / 2),
                    side,
                );
                let ink = Color::from(palette.on_surface_muted);
                paint_icon_slot(surface, slot, *glyph, ink, None, FULL_COLOUR);
            }
        }
        if self.state.disposition() == ControlDisposition::DisabledByState {
            let veil = Color::from(palette.surface.with_alpha(128));
            surface.fill_round_rect(ax, ay, aw, ah, radius, veil);
        }
    }

    /// The choice's name, centred beneath its picture and elided to the tile,
    /// in the accent's own ink on a heavier contrast's selection panel.
    fn paint_label(
        &self,
        surface: &mut Surface,
        (x, y, w, _): Area,
        label: &str,
        tile: &Tile,
        (theme, panelled): (&Theme, bool),
    ) {
        let ink = if panelled {
            Color::from(theme.palette().on_accent)
        } else {
            foreground(theme, self.state.disposition())
        };
        let room = w.saturating_sub(tile.pad.saturating_mul(2));
        let run = tile.label.elide_to_width(label, room);
        let drawn = crate::paint::run_width(tile.label, run);
        let left = x.saturating_add(w.saturating_sub(drawn) / 2);
        let top = tile.label_top(y);
        paint_run(
            surface,
            tile.label,
            run,
            (to_i32(left), to_i32(top)),
            ink,
            None,
        );
    }

    /// The marks laid over a tile: the keyboard ring on the cursor's, unless
    /// the chosen tile's ring carries it, and an authority or recovery bead in
    /// the picture's corner.
    fn paint_marks(
        &self,
        surface: &mut Surface,
        ((x, y, w, h), (fx, fy, fw, fh)): (Area, Area),
        index: usize,
        tile: &Tile,
        (scale, theme): (Scale, &Theme),
    ) {
        if self.focused && self.cursor == index && !self.rings_choice(index, theme) {
            surface.wash_ring(
                x,
                y,
                w,
                h,
                Ring::uniform(tile.panel_radius, tile.border),
                RingInk::Solid(Color::from(theme.palette().rim_active)),
            );
        }
        if let Some((color, shape)) = resolve_bead(theme, self.state) {
            let size = scale
                .scale_length(theme.metrics().bead_size)
                .max(3)
                .min(fw)
                .min(fh);
            let inset = tile.pad.max(1);
            let left = fx
                .saturating_add(fw)
                .saturating_sub(size)
                .saturating_sub(inset);
            paint_bead(surface, left, fy.saturating_add(inset), size, color, shape);
        }
    }

    /// Route one pointer event over the choice laid out in `bounds`,
    /// reporting the tiles whose look it changed.
    ///
    /// A press chooses nothing: a release on the tile the press began on
    /// chooses it, as every shared control behaves, and a release elsewhere
    /// does nothing. A denied or disabled choice takes the hover and refuses
    /// the press.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<PictureAction> {
        if let InputEvent::PointerMoved { to } = *event {
            *self.pointer = to;
        }
        let over = bounds
            .contains(*self.pointer)
            .then(|| self.item_at(*self.pointer, bounds, scale, theme))
            .flatten();
        match *event {
            InputEvent::PointerMoved { .. } => {
                if over != self.hovered {
                    self.mark(&[self.hovered, over], bounds, scale, theme, damage);
                    self.hovered = over;
                }
                None
            }
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => {
                if over.is_none() || !self.state.is_actionable() {
                    return None;
                }
                self.armed = over;
                self.mark(&[over], bounds, scale, theme, damage);
                None
            }
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => {
                let armed = self.armed.take()?;
                self.mark(&[Some(armed)], bounds, scale, theme, damage);
                if over != Some(armed) || !self.state.is_actionable() {
                    return None;
                }
                self.cursor = armed;
                self.choose(armed, bounds, scale, theme, damage)
            }
            _ => None,
        }
    }

    /// Route one key to a choice holding the keyboard, laid out in `bounds`:
    /// the arrows walk the pictures a picture or a line at a time, crossing
    /// from one section into the next, Home and End go to the ends, and Enter
    /// or Space chooses the picture the cursor is on.
    ///
    /// A key that has nowhere to go here — Up from the first line, Down from
    /// the last — answers `None`, so the owner can carry the cursor on to
    /// whatever sits beside the choice.
    pub fn on_key(
        &mut self,
        key: Key,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<PictureAction> {
        if !self.focused || self.is_empty() || !self.state.is_actionable() {
            return None;
        }
        let per_line = Layout::of(self.aspect, bounds.width, scale, theme)
            .across
            .count()
            .max(1);
        let at = self.cursor;
        let last = self.len - 1;
        let to = match key {
            Key::Named(NamedKey::Left) => at.checked_sub(1)?,
            Key::Named(NamedKey::Right) => Some(at + 1).filter(|&next| next <= last)?,
            Key::Named(NamedKey::Up) => self.line_step(at, per_line, Step::Up)?,
            Key::Named(NamedKey::Down) => self.line_step(at, per_line, Step::Down)?,
            Key::Named(NamedKey::Home) => (at != 0).then_some(0)?,
            Key::Named(NamedKey::End) => (at != last).then_some(last)?,
            Key::Named(NamedKey::Enter) | Key::Char(' ') => {
                return self.choose(at, bounds, scale, theme, damage);
            }
            _ => return None,
        };
        self.mark(&[Some(at), Some(to)], bounds, scale, theme, damage);
        self.cursor = to;
        Some(PictureAction::Moved { index: to })
    }

    /// Choose `index`, answering nothing when it already is the choice: a
    /// setting asked to become what it already is has nothing to report.
    fn choose(
        &mut self,
        index: usize,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<PictureAction> {
        if self.selected == Some(index) {
            return None;
        }
        self.mark(&[self.selected, Some(index)], bounds, scale, theme, damage);
        self.selected = Some(index);
        Some(PictureAction::Chose { index })
    }

    /// The picture a line above or below `at` in the same slot, crossing into
    /// the neighbouring section's nearest line, or `None` past either end.
    fn line_step(&self, at: usize, per_line: usize, step: Step) -> Option<usize> {
        let (section, local) = self.locate(at)?;
        let first = at - local;
        let len = self.sections.get(section)?.items.len();
        let slot = local % per_line;
        match step {
            Step::Up if local >= per_line => Some(at - per_line),
            Step::Down if local / per_line < (len - 1) / per_line => {
                Some((at + per_line).min(first + len - 1))
            }
            Step::Up => {
                let (start, size) = self.neighbour(section, Step::Up)?;
                let last_line = (size - 1) / per_line * per_line;
                Some(start + (last_line + slot).min(size - 1))
            }
            Step::Down => {
                let (start, size) = self.neighbour(section, Step::Down)?;
                Some(start + slot.min(size - 1))
            }
        }
    }

    /// The first index and the size of the nearest section holding a picture
    /// before or after `section`.
    fn neighbour(&self, section: usize, step: Step) -> Option<(usize, usize)> {
        let mut start = 0usize;
        let mut before = None;
        for (at, held) in self.sections.iter().enumerate() {
            let size = held.items.len();
            if size > 0 && at < section {
                before = Some((start, size));
            }
            if size > 0 && at > section && step == Step::Down {
                return Some((start, size));
            }
            start = start.saturating_add(size);
        }
        match step {
            Step::Up => before,
            Step::Down => None,
        }
    }

    /// Report the tiles of `indices` that lay out somewhere.
    fn mark(
        &self,
        indices: &[Option<usize>],
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        for index in indices.iter().flatten() {
            if let Some(rect) = self.item_rect(*index, bounds, scale, theme) {
                damage.add(rect);
            }
        }
    }
}

/// Which way a line step moves the cursor.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Step {
    Up,
    Down,
}

/// One tile's anatomy at a scale and theme, in physical pixels.
#[derive(Copy, Clone, Debug)]
struct Tile {
    /// The whole tile, which the pointer's wash covers.
    width: u32,
    height: u32,
    /// The margin inside the tile's edge, round the picture's frame and its
    /// name, which the chosen tile's ring is drawn in.
    pad: u32,
    /// The picture's frame: its rim, and the picture inside it.
    frame: (u32, u32),
    /// The picture itself, inside the rim.
    art: (u32, u32),
    border: u32,
    ring: u32,
    radius: u32,
    panel_radius: u32,
    label: BitmapFont,
}

impl Tile {
    fn of(aspect: Aspect, scale: Scale, theme: &Theme) -> Self {
        let border = plate_border(theme, scale).max(1);
        let ring = rail_thickness(theme, scale);
        let pad = ring.saturating_add(border);
        // The picture is the aspect exactly; its rim is drawn outside it.
        let art_w = scale.scale_length(theme.metrics().picture_width).max(1);
        let art_h = aspect.height_for(art_w);
        let frame_w = art_w.saturating_add(border.saturating_mul(2));
        let frame_h = art_h.saturating_add(border.saturating_mul(2));
        let label = role_font(theme, scale, TextRole::Caption);
        let radius = scale.scale_length(theme.metrics().control_corner_radius);
        Self {
            width: frame_w.saturating_add(pad.saturating_mul(2)),
            height: pad
                .saturating_mul(3)
                .saturating_add(frame_h)
                .saturating_add(label.line_height()),
            pad,
            frame: (frame_w, frame_h),
            art: (art_w, art_h),
            border,
            ring,
            radius,
            panel_radius: radius.saturating_add(pad),
            label,
        }
    }

    /// The frame of a tile whose top-left is `(x, y)`.
    const fn frame(&self, x: u32, y: u32) -> Area {
        (
            x.saturating_add(self.pad),
            y.saturating_add(self.pad),
            self.frame.0,
            self.frame.1,
        )
    }

    /// The top of the name's line in a tile whose top is `y`.
    const fn label_top(&self, y: u32) -> u32 {
        y.saturating_add(self.pad.saturating_mul(2))
            .saturating_add(self.frame.1)
    }
}

/// The choice's layout across one width.
struct Layout {
    tile: Tile,
    /// The tiles one line holds, spread across the width.
    across: GridRun,
    gap: u32,
    title: BitmapFont,
}

impl Layout {
    fn of(aspect: Aspect, width: u32, scale: Scale, theme: &Theme) -> Self {
        let tile = Tile::of(aspect, scale, theme);
        let gap = scale.scale_length(theme.metrics().control_gap).max(1);
        Self {
            tile,
            across: GridRun::new(width, tile.width, gap, GridFill::Spread),
            gap,
            title: role_font(theme, scale, TextRole::ItemTitle),
        }
    }

    /// How many lines `len` pictures take.
    fn lines(&self, len: usize) -> usize {
        match self.across.count() {
            0 => 0,
            per_line => len.div_ceil(per_line),
        }
    }

    /// The band a section's heading takes above its pictures.
    fn title_band(&self, titled: bool) -> u32 {
        if titled {
            self.title.line_height().saturating_add(self.tile.pad)
        } else {
            0
        }
    }

    /// Where picture `local` of `section` is drawn in `bounds`.
    fn tile_rect(&self, bounds: Rect, section: PlacedSection<'_>, local: usize) -> Option<Rect> {
        let per_line = self.across.count();
        if per_line == 0 || local >= section.len {
            return None;
        }
        let across = self.across.offset(local % per_line)?;
        let down = GridRun::fixed(self.lines(section.len), self.tile.height, self.gap)
            .offset(local / per_line)?;
        Some(Rect::new(
            bounds.left().checked_add_unsigned(across)?,
            section.tiles_top.checked_add_unsigned(down)?,
            self.tile.width,
            self.tile.height,
        ))
    }
}

/// One section as it is laid out.
#[derive(Copy, Clone, Debug)]
struct PlacedSection<'a> {
    title: &'a str,
    first: usize,
    len: usize,
    title_top: i32,
    tiles_top: i32,
    bottom: i32,
}

/// The sections of a choice in the order they are laid out down its bounds,
/// each a gap below the last.
struct Placed<'a> {
    sections: core::slice::Iter<'a, PictureSection>,
    layout: &'a Layout,
    top: i32,
    first: usize,
    laid: bool,
}

impl<'a> Iterator for Placed<'a> {
    type Item = PlacedSection<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let section = self.sections.next()?;
            let first = self.first;
            let len = section.items.len();
            self.first = self.first.saturating_add(len);
            if len == 0 {
                continue;
            }
            if self.laid {
                self.top = self.top.saturating_add(to_i32(self.layout.gap));
            }
            self.laid = true;
            let title_top = self.top;
            let tiles_top =
                title_top.saturating_add(to_i32(self.layout.title_band(!section.title.is_empty())));
            let lines = self.layout.lines(len);
            let span = GridRun::fixed(lines, self.layout.tile.height, self.layout.gap).span();
            let bottom = tiles_top.saturating_add(i32::try_from(span).unwrap_or(i32::MAX));
            self.top = bottom;
            return Some(PlacedSection {
                title: &section.title,
                first,
                len,
                title_top,
                tiles_top,
                bottom,
            });
        }
    }
}
