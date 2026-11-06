//! The window's panes and where each one is: docked down the left or the
//! right edge, in order, rolled up to its band or open, or the palette in its
//! strip beneath the canvas; floating in a tool window of its own; or hidden.
//! Each counts when it was last opened in a dock, which its dock gives it room
//! by (`layout`).

use alloc::string::String;

use tairix_abi::window_ipc::AppMenuSymbol;
use tairix_inline::ArrayVec;

/// The palette's place in the stored arrangement: its strip.
const STRIP: &str = "strip";

/// One pane of the window's chrome.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum PaneKind {
    /// The tool box.
    Tools,
    /// The two inks and the colour picker.
    Colour,
    /// The picture's layers, the one painted on chosen among them.
    Layers,
    /// The adjustments, the one chosen in its selector set beneath it.
    Adjustments,
    /// The effects, the one chosen in its selector set beneath it.
    Effects,
    /// The palette's wells, which dock only in their strip beneath the
    /// canvas.
    Palette,
}

impl PaneKind {
    /// Every pane, in the order a list of them reads.
    pub const ALL: [Self; 6] = [
        Self::Tools,
        Self::Colour,
        Self::Layers,
        Self::Adjustments,
        Self::Effects,
        Self::Palette,
    ];

    /// The panes an adjustment or an effect is set in, in that order.
    pub const OPERATIONS: [Self; 2] = [Self::Adjustments, Self::Effects];

    /// What the pane's band names it.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Tools => "Tools",
            Self::Colour => "Colour",
            Self::Layers => "Layers",
            Self::Adjustments => "Adjustments",
            Self::Effects => "Effects",
            Self::Palette => "Palette",
        }
    }

    /// The function the Panes menu names it by.
    #[must_use]
    pub const fn symbol(self) -> AppMenuSymbol {
        match self {
            Self::Tools => AppMenuSymbol::Tools,
            Self::Colour | Self::Palette => AppMenuSymbol::Colours,
            Self::Layers => AppMenuSymbol::Layers,
            Self::Adjustments => AppMenuSymbol::Adjust,
            Self::Effects => AppMenuSymbol::Blur,
        }
    }

    /// Its spelling in the stored arrangement.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::Colour => "colour",
            Self::Layers => "layers",
            Self::Adjustments => "adjustments",
            Self::Effects => "effects",
            Self::Palette => "palette",
        }
    }

    /// Where the pane sits in [`ALL`](Self::ALL).
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Tools => 0,
            Self::Colour => 1,
            Self::Layers => 2,
            Self::Adjustments => 3,
            Self::Effects => 4,
            Self::Palette => 5,
        }
    }

    /// Where the pane sits in [`OPERATIONS`](Self::OPERATIONS); `None` for
    /// any pane but the Adjustments and the Effects panes.
    #[must_use]
    pub const fn operation(self) -> Option<usize> {
        match self {
            Self::Adjustments => Some(0),
            Self::Effects => Some(1),
            Self::Tools | Self::Colour | Self::Layers | Self::Palette => None,
        }
    }
}

/// An edge of the window a dock runs down.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Side {
    /// The left edge.
    Left,
    /// The right edge.
    Right,
}

impl Side {
    /// Both edges, left first.
    pub const BOTH: [Self; 2] = [Self::Left, Self::Right];

    /// Its spelling in the stored arrangement.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    /// Where the side's dock sits in a pair of docks.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
        }
    }
}

/// A pane in a dock.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Docked {
    /// Which pane.
    pub kind: PaneKind,
    /// Whether it is rolled up to its band.
    pub collapsed: bool,
}

/// The panes down one dock: at most every pane but the palette, held inline
/// so an arrangement copies without an allocation.
type Dock = ArrayVec<Docked, { PaneKind::ALL.len() - 1 }>;

/// Where every pane is, and when each was last opened in a dock.
///
/// A pane is in one dock or in none, never in two, the palette's one dock its
/// strip. One not docked floats or is hidden, and remembers the side it last
/// stood on, which showing a hidden one puts it back on. Two arrangements are
/// equal where every pane stands alike: the order the panes were opened in is
/// the window's own, neither stored nor compared.
#[derive(Clone, Debug)]
pub struct Arrangement {
    docks: [Dock; 2],
    /// The side each pane last stood on: none for the palette.
    homes: [Option<Side>; PaneKind::ALL.len()],
    floating: [bool; PaneKind::ALL.len()],
    /// Whether the palette lies in its strip.
    strip: bool,
    /// The opening each pane was last opened in a dock by, counted from one;
    /// none for a pane not opened since the arrangement began.
    opened: [u64; PaneKind::ALL.len()],
    openings: u64,
}

impl PartialEq for Arrangement {
    fn eq(&self, other: &Self) -> bool {
        (&self.docks, &self.homes, &self.floating, self.strip)
            == (&other.docks, &other.homes, &other.floating, other.strip)
    }
}

impl Eq for Arrangement {}

impl Default for Arrangement {
    /// The tool box down the left; the colour pane down the right, where the
    /// Layers, Adjustments and Effects panes join it beneath once shown; the
    /// palette in its strip. No pane counts as opened yet.
    fn default() -> Self {
        let mut arrangement = Self {
            docks: [Dock::new(), Dock::new()],
            homes: [
                Some(Side::Left),
                Some(Side::Right),
                Some(Side::Right),
                Some(Side::Right),
                Some(Side::Right),
                None,
            ],
            floating: [false; PaneKind::ALL.len()],
            strip: false,
            opened: [0; PaneKind::ALL.len()],
            openings: 0,
        };
        for kind in [PaneKind::Tools, PaneKind::Colour, PaneKind::Palette] {
            arrangement.open(kind);
        }
        arrangement
    }
}

impl Arrangement {
    /// The panes docked down `side`, top first.
    #[must_use]
    pub fn docked(&self, side: Side) -> &[Docked] {
        &self.docks[side.index()]
    }

    /// Where `kind` is docked down a side — its side and its place down it —
    /// or `None` where it floats, is hidden, or is the palette.
    #[must_use]
    pub fn place(&self, kind: PaneKind) -> Option<(Side, usize)> {
        Side::BOTH.into_iter().find_map(|side| {
            self.docked(side)
                .iter()
                .position(|docked| docked.kind == kind)
                .map(|at| (side, at))
        })
    }

    /// Whether the palette lies in its strip beneath the canvas.
    #[must_use]
    pub const fn in_strip(&self) -> bool {
        self.strip
    }

    /// Whether `kind` is shown, docked or floating.
    #[must_use]
    pub fn shows(&self, kind: PaneKind) -> bool {
        self.floats(kind) || self.is_docked(kind)
    }

    /// Whether `kind` floats in a tool window of its own.
    #[must_use]
    pub const fn floats(&self, kind: PaneKind) -> bool {
        self.floating[kind.index()]
    }

    /// The panes floating, in the order a list of them reads.
    pub fn floating(&self) -> impl Iterator<Item = PaneKind> + '_ {
        PaneKind::ALL.into_iter().filter(|&kind| self.floats(kind))
    }

    /// The side `kind` last stood on, which docking it again without a place
    /// puts it on; none for the palette, whose one dock is its strip.
    #[must_use]
    pub const fn home(&self, kind: PaneKind) -> Option<Side> {
        self.homes[kind.index()]
    }

    /// The opening `kind` was last opened in a dock by — shown, unrolled or
    /// moved there — counted from one since the arrangement began; `0` for a
    /// pane not opened since.
    #[must_use]
    pub const fn opened(&self, kind: PaneKind) -> u64 {
        self.opened[kind.index()]
    }

    /// Whether `kind` is shown open rather than rolled up to its band: a
    /// floating pane, and the palette, always are.
    #[must_use]
    pub fn is_open(&self, kind: PaneKind) -> bool {
        self.floats(kind)
            || (self.is_docked(kind) && self.find(kind).is_none_or(|docked| !docked.collapsed))
    }

    /// Show `kind` at the foot of the side it last stood on, or the palette in
    /// its strip, open; one already shown is unrolled where it is, and one
    /// floating floats on. A pane shown or unrolled in a dock counts as the one
    /// opened last.
    pub fn show(&mut self, kind: PaneKind) {
        if self.open(kind) {
            self.stamp(kind);
        }
    }

    /// Stand `kind` where showing it puts it, answering whether it now lies
    /// open in a dock where it did not.
    fn open(&mut self, kind: PaneKind) -> bool {
        if self.floats(kind) {
            return false;
        }
        if let Some(docked) = self.find_mut(kind) {
            return core::mem::replace(&mut docked.collapsed, false);
        }
        let Some(side) = self.home(kind) else {
            self.strip = true;
            return false;
        };
        // A dock holds every pane but the palette, and this one is in neither.
        self.docks[side.index()]
            .try_push(Docked {
                kind,
                collapsed: false,
            })
            .is_ok()
    }

    /// Hide `kind`, keeping the side it stood on to show it there again.
    pub fn hide(&mut self, kind: PaneKind) {
        self.undock(kind);
        self.floating[kind.index()] = false;
    }

    /// Float `kind` in a tool window of its own, keeping the side it stood on.
    pub fn float(&mut self, kind: PaneKind) {
        self.undock(kind);
        self.floating[kind.index()] = true;
    }

    /// Take `kind` out of the dock it is in, keeping that side as its home,
    /// or the palette out of its strip.
    fn undock(&mut self, kind: PaneKind) {
        if let Some((side, at)) = self.place(kind) {
            self.docks[side.index()].remove(at);
            self.homes[kind.index()] = Some(side);
        } else if self.home(kind).is_none() {
            self.strip = false;
        }
    }

    /// Roll `kind` up to its band, or open it again, which counts it as the
    /// one opened last.
    pub fn toggle_collapsed(&mut self, kind: PaneKind) {
        let Some(docked) = self.find_mut(kind) else {
            return;
        };
        docked.collapsed = !docked.collapsed;
        if !docked.collapsed {
            self.stamp(kind);
        }
    }

    /// Move `kind` down `side`, landing before the pane that stands at
    /// `before` there now — the dock's length to land at its foot — so a drop
    /// lands in the gap the pointer showed whichever dock, or tool window, it
    /// came from. The palette docks in its strip, whatever side it is given.
    /// A pane moved into a dock counts as the one opened last.
    pub fn move_to(&mut self, kind: PaneKind, side: Side, before: usize) {
        if self.home(kind).is_none() {
            self.redock(kind);
            return;
        }
        self.floating[kind.index()] = false;
        let mut before = before.min(self.docked(side).len());
        let removed = self.place(kind).and_then(|(from, at)| {
            if from == side && at < before {
                before -= 1;
            }
            self.docks[from.index()].remove(at)
        });
        let docked = removed.unwrap_or(Docked {
            kind,
            collapsed: false,
        });
        // A dock holds every pane but the palette, and this one is now in
        // neither.
        let _ = self.docks[side.index()].try_insert(before, docked);
        self.homes[kind.index()] = Some(side);
        self.stamp(kind);
    }

    /// Dock `kind` again: at the foot of the side it last stood on, or the
    /// palette in its strip.
    pub fn redock(&mut self, kind: PaneKind) {
        if let Some(side) = self.home(kind) {
            self.move_to(kind, side, usize::MAX);
        } else {
            self.floating[kind.index()] = false;
            self.strip = true;
        }
    }

    /// Spell the arrangement into `out` as it is stored: each pane once, as
    /// `pane:side`, down each dock in turn, then the palette as
    /// `palette:strip` where it lies in its strip, and then the rest, a pane
    /// rolled up marked `:collapsed`, one floating `:floating` and one hidden
    /// `:hidden`.
    pub fn spell(&self, out: &mut String) {
        let mut first = true;
        let mut word = |out: &mut String, kind: PaneKind, home: &str, mark: Option<&str>| {
            if !first {
                out.push(' ');
            }
            first = false;
            out.push_str(kind.token());
            out.push(':');
            out.push_str(home);
            if let Some(mark) = mark {
                out.push(':');
                out.push_str(mark);
            }
        };
        for side in Side::BOTH {
            for docked in self.docked(side) {
                word(
                    out,
                    docked.kind,
                    side.token(),
                    docked.collapsed.then_some("collapsed"),
                );
            }
        }
        if self.strip {
            word(out, PaneKind::Palette, STRIP, None);
        }
        for kind in PaneKind::ALL {
            if !self.is_docked(kind) {
                let mark = if self.floats(kind) {
                    "floating"
                } else {
                    "hidden"
                };
                word(
                    out,
                    kind,
                    self.home(kind).map_or(STRIP, Side::token),
                    Some(mark),
                );
            }
        }
    }

    /// The arrangement `text` spells as [`spell`](Self::spell) does: every
    /// pane exactly once, the palette only ever in its strip, or none at all.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut arrangement = Self {
            docks: [Dock::new(), Dock::new()],
            homes: [None; PaneKind::ALL.len()],
            floating: [false; PaneKind::ALL.len()],
            strip: false,
            opened: [0; PaneKind::ALL.len()],
            openings: 0,
        };
        let mut seen = [false; PaneKind::ALL.len()];
        for word in text.split_whitespace() {
            let mut parts = word.split(':');
            let (pane, home) = (parts.next()?, parts.next()?);
            let kind = PaneKind::ALL
                .into_iter()
                .find(|kind| kind.token() == pane)?;
            let side = match (kind, home) {
                (PaneKind::Palette, STRIP) => None,
                (PaneKind::Palette, _) => return None,
                (_, home) => Some(Side::BOTH.into_iter().find(|at| at.token() == home)?),
            };
            let (collapsed, docked) = match parts.next() {
                None => (false, true),
                Some("collapsed") if side.is_some() => (true, true),
                Some("floating") => {
                    arrangement.floating[kind.index()] = true;
                    (false, false)
                }
                Some("hidden") => (false, false),
                Some(_) => return None,
            };
            if parts.next().is_some() || core::mem::replace(&mut seen[kind.index()], true) {
                return None;
            }
            arrangement.homes[kind.index()] = side;
            match side {
                Some(side) if docked => {
                    arrangement.docks[side.index()]
                        .try_push(Docked { kind, collapsed })
                        .ok()?;
                }
                Some(_) => {}
                None => arrangement.strip = docked,
            }
        }
        seen.iter().all(|&seen| seen).then_some(arrangement)
    }

    /// Whether `kind` stands in a dock: down a side, or the palette in its
    /// strip.
    fn is_docked(&self, kind: PaneKind) -> bool {
        self.place(kind).is_some() || (self.home(kind).is_none() && self.strip)
    }

    fn find(&self, kind: PaneKind) -> Option<&Docked> {
        self.docks
            .iter()
            .flatten()
            .find(|docked| docked.kind == kind)
    }

    fn find_mut(&mut self, kind: PaneKind) -> Option<&mut Docked> {
        self.docks
            .iter_mut()
            .flatten()
            .find(|docked| docked.kind == kind)
    }

    /// Count `kind` as the pane opened last.
    fn stamp(&mut self, kind: PaneKind) {
        self.openings += 1;
        self.opened[kind.index()] = self.openings;
    }
}

#[cfg(test)]
#[path = "pane_tests.rs"]
mod tests;
