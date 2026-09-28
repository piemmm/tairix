//! The settables a pane offers as pictures, and the pictures themselves.
//!
//! Settings may neither read the shipped stores nor decode what is in them:
//! the desktop renders each picture into a region this application granted,
//! one at a time, and this is the bookkeeping over those answers. It performs
//! no I/O. What is asked for is decided by what the reader can see: the
//! pictures on screen first, then those a screen's height either side, and
//! nothing beyond.
//!
//! What is kept is decided by memory. A picture is a thumbnail — tens of
//! kilobytes at the reference density — and the catalog is the desktop's own
//! bounded store, so while memory is plentiful every picture handed over stays
//! for the life of the pane and scrolling back to one costs the desktop
//! nothing. Once memory is short only the pictures on screen are kept.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::window_ipc::{PreviewSubject, WINDOW_PREVIEW_MAX_SIDE};
use tairix_controls::{
    Aspect, FieldGroup, FieldLayout, PictureChoice, PictureItem, PictureSection, Swatch,
};
use tairix_geometry::{Rect, Scale};
use tairix_icon::IconKind;
use tairix_raster::Surface;
use tairix_theme::{Rgba, Theme};
use tairix_wallpaper::{
    Backdrop, CatalogItem, DesktopSettings, ScreensaverKind, SettingsKey, WallpaperChoice,
    WallpaperPath,
};

/// The label of the choice that shows no picture, only the backdrop colour.
pub const NONE_LABEL: &str = "No picture";

/// A settable chosen by its picture, so the reader sees what they choose.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Chooser {
    /// The picture on the desktop, or none.
    Wallpaper,
    /// What covers the screen once the desktop has sat idle.
    Screensaver,
}

impl Chooser {
    /// The registry key this chooser writes.
    #[must_use]
    pub const fn key(self) -> SettingsKey {
        match self {
            Self::Wallpaper => SettingsKey::Wallpaper,
            Self::Screensaver => SettingsKey::ScreensaverKind,
        }
    }

    /// What the chooser is called, which is also its search term.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Wallpaper => "Desktop picture",
            Self::Screensaver => "Screensaver",
        }
    }

    /// The pictures this chooser offers a desktop showing `settings`, the
    /// shipped ones being `catalog`, with the one in effect chosen, and what
    /// each of them sets and what renders it.
    pub(crate) fn offer(
        self,
        settings: &DesktopSettings,
        catalog: &[CatalogItem],
    ) -> (PictureChoice, Vec<Offered>) {
        match self {
            Self::Wallpaper => {
                let ladder = wallpaper_ladder(&settings.wallpaper, catalog);
                let selected = ladder
                    .iter()
                    .position(|offer| offer.is(&settings.wallpaper));
                let mut sections: Vec<PictureSection> = Vec::new();
                let mut run: Vec<PictureItem> = Vec::new();
                let mut title: Option<&str> = None;
                for offer in &ladder {
                    let section = offer.section();
                    if section != title && !run.is_empty() {
                        sections.push(section_of(title, core::mem::take(&mut run)));
                    }
                    title = section;
                    run.push(match offer {
                        WallpaperOffer::None => {
                            PictureItem::swatch(NONE_LABEL, backdrop_swatch(settings.backdrop))
                        }
                        WallpaperOffer::Shipped { item, .. } => {
                            PictureItem::new(item.file.as_str(), IconKind::Image)
                        }
                        WallpaperOffer::InEffect(path) => {
                            PictureItem::new(leaf(path.as_str()), IconKind::Image)
                        }
                    });
                }
                if !run.is_empty() {
                    sections.push(section_of(title, run));
                }
                let offered = ladder
                    .iter()
                    .map(|offer| Offered {
                        offer: Offer::Wallpaper(offer.choice()),
                        subject: offer.subject(),
                    })
                    .collect();
                (
                    PictureChoice::new(Aspect::WIDESCREEN, sections).with_selected(selected),
                    offered,
                )
            }
            Self::Screensaver => {
                let items = ScreensaverKind::ALL
                    .iter()
                    .map(|kind| PictureItem::new(screensaver_label(*kind), IconKind::Screensaver))
                    .collect();
                let selected = ScreensaverKind::ALL
                    .iter()
                    .position(|kind| *kind == settings.screensaver);
                let offered = ScreensaverKind::ALL
                    .iter()
                    .map(|kind| Offered {
                        offer: Offer::Screensaver(*kind),
                        subject: Some(PreviewSubject::Screensaver(*kind)),
                    })
                    .collect();
                (
                    PictureChoice::new(
                        Aspect::WIDESCREEN,
                        [PictureSection::untitled(items)].into(),
                    )
                    .with_selected(selected),
                    offered,
                )
            }
        }
    }
}

/// The swatch a desktop's `backdrop` is drawn in.
pub(crate) const fn backdrop_swatch(backdrop: Backdrop) -> Swatch {
    match backdrop {
        Backdrop::Theme => Swatch::Desktop,
        Backdrop::Colour(rgb) => Swatch::Fixed(Rgba::rgb(rgb.r, rgb.g, rgb.b)),
    }
}

/// What choosing one of a chooser's pictures sets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Offer {
    /// The desktop picture, or none.
    Wallpaper(WallpaperChoice),
    /// The screensaver.
    Screensaver(ScreensaverKind),
}

impl Offer {
    /// Write this choice onto `settings`.
    pub(crate) fn apply(&self, settings: &mut DesktopSettings) {
        match self {
            Self::Wallpaper(choice) => settings.wallpaper = choice.clone(),
            Self::Screensaver(kind) => settings.screensaver = *kind,
        }
    }
}

/// One picture a chooser offers: what choosing it sets, and what renders it —
/// `None` for one the desktop cannot render.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Offered {
    pub(crate) offer: Offer,
    pub(crate) subject: Option<PreviewSubject>,
}

/// The display label of a screensaver.
pub(crate) const fn screensaver_label(kind: ScreensaverKind) -> &'static str {
    match kind {
        ScreensaverKind::Blank => "Black",
        ScreensaverKind::Dim => "Dimmed desktop",
        ScreensaverKind::Slideshow => "Slideshow",
        ScreensaverKind::Clock => "Clock",
        ScreensaverKind::Starfield => "Starfield",
        ScreensaverKind::Life => "Game of Life",
    }
}

/// A run of pictures under `title`, or under none.
fn section_of(title: Option<&str>, items: Vec<PictureItem>) -> PictureSection {
    match title {
        Some(title) => PictureSection::new(title, items),
        None => PictureSection::untitled(items),
    }
}

/// One picture the wallpaper chooser offers.
enum WallpaperOffer<'a> {
    /// No picture: the backdrop colour alone.
    None,
    /// A shipped picture, at its position in the desktop's catalog.
    Shipped {
        at: u16,
        item: &'a CatalogItem,
        path: WallpaperPath,
    },
    /// The picture in effect, which the catalog does not hold: one an update
    /// has since removed, or one set from outside the shipped store.
    InEffect(&'a WallpaperPath),
}

impl WallpaperOffer<'_> {
    /// The section this picture is listed under: its category, the directory
    /// it sits in for one outside the catalog, and none for no picture.
    fn section(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Shipped { item, .. } => Some(&item.category),
            Self::InEffect(path) => Some(parent(path.as_str())),
        }
    }

    /// Whether choosing this sets `choice`.
    fn is(&self, choice: &WallpaperChoice) -> bool {
        match (self, choice) {
            (Self::None, WallpaperChoice::None) => true,
            (Self::Shipped { path, .. }, WallpaperChoice::Image(held)) => path == held,
            (Self::InEffect(path), WallpaperChoice::Image(held)) => *path == held,
            _ => false,
        }
    }

    fn choice(&self) -> WallpaperChoice {
        match self {
            Self::None => WallpaperChoice::None,
            Self::Shipped { path, .. } => WallpaperChoice::Image(path.clone()),
            Self::InEffect(path) => WallpaperChoice::Image((*path).clone()),
        }
    }

    /// What renders this picture: a render names a catalog position and
    /// nothing else, so neither of the others has one.
    fn subject(&self) -> Option<PreviewSubject> {
        match self {
            Self::Shipped { at, .. } => Some(PreviewSubject::Wallpaper(*at)),
            Self::None | Self::InEffect(_) => None,
        }
    }
}

/// The pictures the wallpaper chooser offers a desktop showing `current`, in
/// the order it lists them: no picture first, then the catalog category by
/// category.
///
/// A picture in effect that the catalog does not hold is listed rather than
/// dropped, beside the others of its category where the catalog has that
/// category, so opening the pane never hides the choice actually in force.
fn wallpaper_ladder<'a>(
    current: &'a WallpaperChoice,
    catalog: &'a [CatalogItem],
) -> Vec<WallpaperOffer<'a>> {
    let mut ladder = Vec::with_capacity(catalog.len().saturating_add(2));
    ladder.push(WallpaperOffer::None);
    for (at, item) in catalog.iter().enumerate() {
        let (Ok(path), Ok(at)) = (WallpaperPath::new(&item.path()), u16::try_from(at)) else {
            continue;
        };
        ladder.push(WallpaperOffer::Shipped { at, item, path });
    }
    if let WallpaperChoice::Image(path) = current {
        let listed = ladder.iter().any(|offer| {
            matches!(offer, WallpaperOffer::Shipped { path: shipped, .. } if shipped == path)
        });
        if !listed {
            let category = Some(parent(path.as_str()));
            let after = ladder
                .iter()
                .rposition(|offer| offer.section() == category)
                .map_or(ladder.len(), |last| last + 1);
            ladder.insert(after, WallpaperOffer::InEffect(path));
        }
    }
    ladder
}

/// The last segment of `path`.
fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The name of the directory `path` sits in.
fn parent(path: &str) -> &str {
    path.rsplit('/').nth(1).unwrap_or("")
}

/// One picture the caller must ask the desktop to render.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PictureWanted {
    /// What to render.
    pub subject: PreviewSubject,
    /// The width to render it at, in physical pixels.
    pub width: u16,
    /// The height to render it at, in physical pixels.
    pub height: u16,
}

impl PictureWanted {
    /// The bytes an answer to this fills: four per pixel.
    #[must_use]
    pub fn bytes(&self) -> usize {
        usize::from(self.width)
            .saturating_mul(usize::from(self.height))
            .saturating_mul(4)
    }
}

/// One chooser a form draws: which it is, the group it sits in, and what
/// each of its pictures sets and what renders it, in the chooser's own order.
///
/// Kept as the chooser was built, so a choice always sets what was drawn
/// where it was made, whatever the document has come to say since.
pub(crate) struct Pictured {
    pub(crate) chooser: Chooser,
    pub(crate) group: usize,
    pub(crate) pictures: Vec<Offered>,
}

/// The pictures a form's choosers show: what renders each, and which the
/// desktop would not.
#[derive(Default)]
pub(crate) struct Pictures {
    choosers: Vec<Pictured>,
    /// Kept across rebuilds and never asked for again: a picture the desktop
    /// could not render once it cannot render at another size either.
    refused: Vec<PreviewSubject>,
}

impl Pictures {
    /// Whether the form draws any chooser at all.
    pub(crate) fn is_empty(&self) -> bool {
        self.choosers.is_empty()
    }

    /// Take every rendered picture out of `groups`, keyed by what renders it,
    /// so a rebuild can put each back on whichever picture it now belongs to
    /// rather than asking the desktop again.
    pub(crate) fn take(&self, groups: &mut [FieldGroup]) -> BTreeMap<PreviewSubject, Surface> {
        let mut taken = BTreeMap::new();
        self.each(groups, |choice, index, subject| {
            if let Some(art) = choice.take_art(index) {
                taken.insert(subject, art);
            }
        });
        taken
    }

    /// Adopt the choosers a rebuild drew, handing each picture `carried`
    /// holds back to the one it renders.
    pub(crate) fn adopt(
        &mut self,
        choosers: Vec<Pictured>,
        groups: &mut [FieldGroup],
        mut carried: BTreeMap<PreviewSubject, Surface>,
    ) {
        self.choosers = choosers;
        if carried.is_empty() {
            return;
        }
        self.each(groups, |choice, index, subject| {
            if let Some(art) = carried.remove(&subject) {
                choice.set_art(index, art);
            }
        });
    }

    /// Visit every picture a chooser in `groups` shows that something
    /// renders.
    fn each(
        &self,
        groups: &mut [FieldGroup],
        mut visit: impl FnMut(&mut PictureChoice, usize, PreviewSubject),
    ) {
        for chooser in &self.choosers {
            let Some(choice) = groups
                .get_mut(chooser.group)
                .and_then(FieldGroup::pictures_mut)
            else {
                continue;
            };
            for (index, picture) in chooser.pictures.iter().enumerate() {
                if let Some(subject) = picture.subject {
                    visit(choice, index, subject);
                }
            }
        }
    }

    /// The next picture to ask the desktop for, the choosers laid out in
    /// `layouts` and seen through `seen`: the nearest one to what is seen
    /// that lacks its picture — on screen, or while memory is `roomy` up to a
    /// screen's height above or below it.
    ///
    /// Let go first are any pictures rendered at a size the choosers no
    /// longer draw and, once memory is short, those off screen; both are
    /// asked for again should they come back. `None` when every picture asked
    /// for has its answer, or is one the desktop cannot render at this size.
    pub(crate) fn round(
        &self,
        groups: &mut [FieldGroup],
        layouts: &[(usize, FieldLayout)],
        (seen, roomy): (Rect, bool),
        (scale, theme): (Scale, &Theme),
    ) -> Option<PictureWanted> {
        let reach = if roomy {
            tairix_geometry::to_i32(seen.height)
        } else {
            0
        };
        let top = seen.top().saturating_sub(reach);
        let bottom = seen.bottom().saturating_add(reach);
        let mut nearest: Option<(u32, PreviewSubject, (u32, u32))> = None;
        for chooser in &self.choosers {
            let Some(layout) = layouts
                .iter()
                .find_map(|(index, layout)| (*index == chooser.group).then_some(*layout))
            else {
                continue;
            };
            let Some(group) = groups.get_mut(chooser.group) else {
                continue;
            };
            let Some(bounds) = group.row_rect(group.rows().len(), layout, scale, theme) else {
                continue;
            };
            let Some(choice) = group.pictures_mut() else {
                continue;
            };
            let size = choice.picture_size(scale, theme);
            let mut let_go = Vec::new();
            choice.for_each_item_rect(bounds, scale, theme, |index, tile| {
                let Some(subject) = chooser
                    .pictures
                    .get(index)
                    .and_then(|held| held.subject.as_ref())
                else {
                    return;
                };
                let in_reach = tile.bottom() > top && tile.top() < bottom;
                let held = choice
                    .item(index)
                    .and_then(PictureItem::art)
                    .map(|art| (art.width(), art.height()));
                if held.is_some() && (held != Some(size) || !(roomy || in_reach)) {
                    let_go.push(index);
                }
                if !in_reach || held == Some(size) || self.refused.contains(subject) {
                    return;
                }
                let distance = u32::try_from(if tile.bottom() <= seen.top() {
                    seen.top().saturating_sub(tile.bottom())
                } else {
                    tile.top().saturating_sub(seen.bottom()).max(0)
                })
                .unwrap_or(u32::MAX);
                if nearest.is_none_or(|(best, ..)| distance < best) {
                    nearest = Some((distance, *subject, size));
                }
            });
            for index in let_go {
                drop(choice.take_art(index));
            }
        }
        let (_, subject, (width, height)) = nearest?;
        let side = |length: u32| {
            u16::try_from(length)
                .ok()
                .filter(|length| (1..=WINDOW_PREVIEW_MAX_SIDE).contains(length))
        };
        Some(PictureWanted {
            subject,
            width: side(width)?,
            height: side(height)?,
        })
    }

    /// Adopt the pixels the desktop rendered for `wanted`, answering which
    /// group's chooser, and which of its pictures, they fill.
    ///
    /// An answer for a subject no chooser shows is dropped, and one whose
    /// pixels are not the size asked for is refused: a picture keeps its
    /// glyph rather than drawing something of the wrong shape.
    pub(crate) fn land(
        &mut self,
        groups: &mut [FieldGroup],
        wanted: PictureWanted,
        pixels: &[u8],
    ) -> Option<(usize, usize)> {
        let art = pixels.get(..wanted.bytes()).and_then(|pixels| {
            Surface::from_rgba8(u32::from(wanted.width), u32::from(wanted.height), pixels)
        });
        let Some(art) = art else {
            self.refuse(wanted.subject);
            return None;
        };
        let (group, index) = self.choosers.iter().find_map(|chooser| {
            chooser
                .pictures
                .iter()
                .position(|held| held.subject == Some(wanted.subject))
                .map(|index| (chooser.group, index))
        })?;
        let choice = groups.get_mut(group).and_then(FieldGroup::pictures_mut)?;
        choice.set_art(index, art).then_some((group, index))
    }

    /// What choosing `chooser`'s picture `index` sets, as the chooser was
    /// built.
    pub(crate) fn offer(&self, chooser: Chooser, index: usize) -> Option<&Offer> {
        self.choosers
            .iter()
            .find(|held| held.chooser == chooser)?
            .pictures
            .get(index)
            .map(|held| &held.offer)
    }

    /// Repaint the wallpaper chooser's *No picture* in `swatch`, answering its
    /// group and position when there is one to repaint.
    pub(crate) fn restate_swatch(
        &self,
        groups: &mut [FieldGroup],
        swatch: Swatch,
    ) -> Option<(usize, usize)> {
        let chooser = self
            .choosers
            .iter()
            .find(|held| held.chooser == Chooser::Wallpaper)?;
        let index = chooser
            .pictures
            .iter()
            .position(|held| held.offer == Offer::Wallpaper(WallpaperChoice::None))?;
        let choice = groups.get_mut(chooser.group)?.pictures_mut()?;
        choice
            .set_swatch(index, swatch)
            .then_some((chooser.group, index))
    }

    /// Record that the desktop would not render `subject`, so it is never
    /// asked for again, answering whether it was news.
    pub(crate) fn refuse(&mut self, subject: PreviewSubject) -> bool {
        if self.refused.contains(&subject) {
            return false;
        }
        self.refused.push(subject);
        true
    }
}

/// The caption of the group a screensaver's own options sit in: its name.
pub(crate) fn options_caption(kind: ScreensaverKind) -> String {
    screensaver_label(kind).to_uppercase()
}

#[cfg(test)]
#[path = "pictures_tests.rs"]
mod tests;
