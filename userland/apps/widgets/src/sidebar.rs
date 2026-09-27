//! [`SidebarDemo`]: the sidebar list's two-level anatomy over a small model
//! the gallery owns, so its disclosure, its tree keys and its group break
//! visibly act.

use alloc::vec::Vec;

use tairix_controls::{DisclosureSet, Tab, Tabs, TabsAction, TabsOrientation};
use tairix_geometry::{Rect, Region, Scale};
use tairix_icon::{IconKind, NoArtwork};
use tairix_input::{InputEvent, Key};
use tairix_raster::Surface;
use tairix_theme::Theme;

/// One section of the demo list and the pages it discloses.
struct Section {
    label: &'static str,
    icon: IconKind,
    reading: Option<&'static str>,
    group_break: bool,
    pages: &'static [(&'static str, IconKind)],
}

/// Two sections that open in place and, set apart by a break, one that
/// discloses nothing and carries a reading instead.
const SECTIONS: &[Section] = &[
    Section {
        label: "General",
        icon: IconKind::Settings,
        reading: None,
        group_break: false,
        pages: &[("About", IconKind::About), ("Caching", IconKind::Caching)],
    },
    Section {
        label: "Networking",
        icon: IconKind::Networking,
        reading: None,
        group_break: false,
        pages: &[("Ethernet", IconKind::Ethernet), ("DNS", IconKind::Dns)],
    },
    Section {
        label: "Storage",
        icon: IconKind::Storage,
        reading: Some("2 volumes"),
        group_break: true,
        pages: &[],
    },
];

/// What one strip entry of the demo is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Row {
    /// A section's own entry, by its position in [`SECTIONS`].
    Section(usize),
    /// A disclosed page: its section, and its position among that section's
    /// pages.
    Page(usize, usize),
}

/// A two-level sidebar list whose sections open in place, each on its own.
#[derive(Clone, Debug)]
pub struct SidebarDemo {
    strip: Tabs,
    /// What each strip entry is, in the strip's own order.
    rows: Vec<Row>,
    open: DisclosureSet<usize>,
    /// The row on show; a page whose section is closed is stood for by the
    /// section's entry.
    selected: Row,
}

impl Default for SidebarDemo {
    fn default() -> Self {
        Self::new()
    }
}

impl SidebarDemo {
    /// The demo as the gallery opens it: General open on its first page,
    /// Networking closed.
    #[must_use]
    pub fn new() -> Self {
        let mut open = DisclosureSet::closed();
        open.set(0, true);
        let mut demo = Self {
            strip: Tabs::new(Vec::new()).with_orientation(TabsOrientation::Vertical),
            rows: Vec::new(),
            open,
            selected: Row::Page(0, 0),
        };
        demo.restate();
        demo
    }

    /// The strip the demo draws, for a test reading what it lists.
    #[cfg(test)]
    pub(crate) fn strip(&self) -> &Tabs {
        &self.strip
    }

    /// Put the strip's keyboard cursor on `index`, or take it off.
    pub fn adopt_current(&mut self, index: Option<usize>) {
        self.strip.adopt_current(index);
    }

    /// Paint the demo into `rect`, cut to it: a strip lays its whole list out
    /// and leaves showing it to its owner, and a slot too short for every
    /// section open at once keeps the rest of the gallery clear.
    pub fn render(&self, surface: &mut Surface, rect: Rect, scale: Scale, theme: &Theme) {
        let (Ok(x), Ok(y)) = (u32::try_from(rect.left()), u32::try_from(rect.top())) else {
            return;
        };
        surface.with_clip(x, y, rect.width, rect.height, |surface| {
            self.strip
                .render(surface, rect, scale, theme, &mut NoArtwork);
        });
    }

    /// Feed a pointer event, answering whether anything changed.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        rect: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        let acted = self.strip.on_pointer(event, rect, scale, theme, damage);
        self.act(acted, (rect, scale, theme), damage)
    }

    /// Feed a key, answering whether anything changed.
    pub fn on_key(
        &mut self,
        key: Key,
        rect: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        let acted = self.strip.on_key(key, rect, scale, theme, damage);
        self.act(acted, (rect, scale, theme), damage)
    }

    /// Apply what the strip reported: choosing a section that discloses
    /// pages opens or closes them, choosing any other entry shows it, and the
    /// tree keys open and close a section.
    fn act(
        &mut self,
        acted: Option<TabsAction>,
        (rect, scale, theme): (Rect, Scale, &Theme),
        damage: &mut Region,
    ) -> bool {
        let (section, open) = match acted {
            Some(TabsAction::Selected { index }) => match self.rows.get(index).copied() {
                Some(Row::Section(section)) if discloses(section) => {
                    (section, !self.open.is_open(&section))
                }
                Some(row) => {
                    self.selected = row;
                    self.strip.set_selected(index, rect, scale, theme, damage);
                    return true;
                }
                None => return false,
            },
            Some(TabsAction::Disclose { index, open }) => match self.rows.get(index).copied() {
                Some(Row::Section(section)) => (section, open),
                Some(Row::Page(..)) | None => return false,
            },
            None => return false,
        };
        if !self.open.set(section, open) {
            return false;
        }
        let cursor = self.strip.current().map(|_| Row::Section(section));
        self.restate();
        self.strip
            .adopt_current(cursor.and_then(|kept| self.rows.iter().position(|row| *row == kept)));
        // The list changed shape, so every entry below the section moved.
        damage.add(rect);
        true
    }

    /// Rebuild the strip for the sections open and the row on show.
    fn restate(&mut self) {
        let mut rows = Vec::new();
        let mut tabs = Vec::new();
        for (index, section) in SECTIONS.iter().enumerate() {
            let open = self.open.is_open(&index);
            let mut tab = Tab::new(section.label)
                .with_icon(section.icon)
                .with_group_break(section.group_break);
            if let Some(reading) = section.reading {
                tab = tab.with_reading(reading);
            }
            if discloses(index) {
                tab = tab.with_disclosure(open);
            }
            rows.push(Row::Section(index));
            tabs.push(tab);
            if open {
                for (page, &(label, icon)) in section.pages.iter().enumerate() {
                    rows.push(Row::Page(index, page));
                    tabs.push(Tab::new(label).with_icon(icon).nested());
                }
            }
        }
        let mut strip = Tabs::new(tabs).with_orientation(TabsOrientation::Vertical);
        let shown = rows
            .iter()
            .position(|row| *row == self.selected)
            .or_else(|| match self.selected {
                Row::Page(section, _) => rows.iter().position(|row| *row == Row::Section(section)),
                Row::Section(_) => None,
            });
        if let Some(index) = shown {
            strip.adopt_selected(index);
        }
        self.rows = rows;
        self.strip.restate(strip);
    }
}

/// Whether section `index` discloses pages of its own.
fn discloses(index: usize) -> bool {
    SECTIONS
        .get(index)
        .is_some_and(|section| !section.pages.is_empty())
}
