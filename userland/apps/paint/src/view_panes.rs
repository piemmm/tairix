//! The panes of the window's chrome: each one's band, rolling it up, closing
//! it, dragging it by its band from one place in a dock to another or out of
//! the window into a tool window of its own, and docking it again.

use tairix_controls::{TitleBar, TitleBarEvent, WindowControlKind};
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_input::InputEvent;
use tairix_theme::Theme;
use tairix_window::docapp::{ToolGone, ToolMove, ToolOpening};

use super::{pane_header, tool_pane, Landing, Outcome, PaneDrag, View};
use crate::layout::Layout;
use crate::pane::{Arrangement, PaneKind, Side};

/// How far in from the window's edge a dragged pane is taken to an empty
/// dock there, in logical pixels.
const EDGE_REACH: u32 = 24;

impl View {
    /// Whether pane `kind` is shown.
    #[must_use]
    pub fn shows(&self, kind: PaneKind) -> bool {
        self.panes.shows(kind)
    }

    /// Where the panes are.
    #[must_use]
    pub const fn arrangement(&self) -> &Arrangement {
        &self.panes
    }

    /// The band heading pane `kind`, for the painter.
    #[must_use]
    pub(crate) fn header(&self, kind: PaneKind) -> &TitleBar {
        &self.headers[kind.index()]
    }

    /// Where a pane being dragged by its band, or moved in its tool window,
    /// would land, for the painter.
    #[must_use]
    pub(crate) fn landing(&self) -> Option<Landing> {
        self.pane_drag
            .and_then(|drag| drag.landing)
            .or(self.tool_landing)
    }

    /// Show pane `kind`, or hide it; hiding the Adjustment pane closes the
    /// adjustment open in it.
    pub(super) fn toggle_pane(
        &mut self,
        kind: PaneKind,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        if self.panes.shows(kind) {
            self.panes.hide(kind);
            if kind == PaneKind::Adjustment {
                self.close_adjustment(layout, damage);
            }
        } else {
            self.panes.show(kind);
        }
        Outcome::relaid()
    }

    /// Put every pane back where a new window has it, which closes an open
    /// adjustment with its pane.
    pub(super) fn reset_panes(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        self.pane_drag = None;
        self.tool_landing = None;
        self.panes.clone_from(&self.home_panes);
        if !self.panes.shows(PaneKind::Adjustment) {
            self.close_adjustment(layout, damage);
        }
        Outcome::relaid()
    }

    /// The pointer on the panes' bands. Every band sees every event, so a
    /// hover leaves and a press held on one ends wherever it is let go; while
    /// a pane is dragged nothing else in the chrome takes the pointer.
    pub(super) fn panes_pointer(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        if matches!(event, InputEvent::PointerPressed { .. }) {
            let at = self.pointer;
            self.band_grab = layout
                .panes()
                .find(|slot| slot.header.contains(at))
                .map(|slot| Point::new(at.x - slot.header.left(), at.y - slot.header.top()));
        }
        let mut outcome = None;
        for slot in layout.panes() {
            let header = &mut self.headers[slot.kind.index()];
            let Some(happened) = header.on_pointer(event, slot.header, scale, theme, damage) else {
                continue;
            };
            outcome = match happened {
                TitleBarEvent::Control(WindowControlKind::Close) => {
                    Some(self.toggle_pane(slot.kind, layout, damage))
                }
                TitleBarEvent::Control(WindowControlKind::Minimize) => {
                    self.panes.toggle_collapsed(slot.kind);
                    Some(Outcome::relaid())
                }
                TitleBarEvent::DragBegin => {
                    let middle = Point::new(
                        to_i32(slot.header.width / 2),
                        to_i32(slot.header.height / 2),
                    );
                    self.pane_drag = Some(PaneDrag {
                        kind: slot.kind,
                        landing: None,
                        grab: self.band_grab.unwrap_or(middle),
                    });
                    // The sample that crosses the threshold may already be
                    // over a dock: a flick let go at once still lands.
                    match event {
                        InputEvent::PointerMoved { to } => {
                            Some(self.drag_pane_to(*to, layout, scale, theme, damage))
                        }
                        _ => Some(Outcome::none()),
                    }
                }
                TitleBarEvent::DragMoved { to } => {
                    Some(self.drag_pane_to(to, layout, scale, theme, damage))
                }
                TitleBarEvent::DragEnd => Some(self.drop_pane(layout, scale, theme, damage)),
                TitleBarEvent::Activate
                | TitleBarEvent::Control(_)
                | TitleBarEvent::AlternateControl(_) => outcome,
            };
        }
        if outcome.is_none() && self.pane_drag.is_some() {
            return Some(Outcome::none());
        }
        outcome
    }

    /// The dragged pane is over `to`: mark where it would land, or, carried
    /// to the window's edge — as far as the pointer is told while a press is
    /// held — tear it out into a tool window that goes on following the
    /// press.
    fn drag_pane_to(
        &mut self,
        to: Point,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        let Some(drag) = self.pane_drag else {
            return Outcome::none();
        };
        if on_edge(layout.window(), to) {
            return self.tear_out(drag, to, true, layout);
        }
        let landing = landing_at(layout, to, scale);
        if landing == drag.landing {
            return Outcome::none();
        }
        for marked in [drag.landing, landing].into_iter().flatten() {
            damage.add(landing_mark(layout, marked, scale, theme));
        }
        self.pane_drag = Some(PaneDrag { landing, ..drag });
        Outcome::none()
    }

    /// The dragged pane was let go: move it to where it was marked to land,
    /// or, let go anywhere else, float it in a tool window there.
    fn drop_pane(
        &mut self,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        let Some(drag) = self.pane_drag.take() else {
            return Outcome::none();
        };
        let Some(landing) = drag.landing else {
            return self.tear_out(drag, self.pointer, false, layout);
        };
        damage.add(landing_mark(layout, landing, scale, theme));
        self.panes.move_to(drag.kind, landing.side, landing.before);
        Outcome::relaid()
    }

    /// Float the pane `drag` holds in a tool window whose band lies where the
    /// pane's would with the pointer at `to`, carried on by the press still
    /// held when `carried`. The window is laid out again whole, which takes
    /// any landing mark with it.
    fn tear_out(&mut self, drag: PaneDrag, to: Point, carried: bool, layout: &Layout) -> Outcome {
        self.pane_drag = None;
        let band = layout.pane(drag.kind).map_or(0, |slot| slot.header.height);
        let offset = (to.x - drag.grab.x, to.y - drag.grab.y + to_i32(band));
        let carry = carried.then(|| u32::try_from(drag.grab.x).unwrap_or(0));
        self.openings[drag.kind.index()] = Some(ToolOpening { offset, carry });
        // The band the press began on hears no more of it, so it starts
        // afresh rather than believing a drag is still under way.
        let furniture = self.headers[drag.kind.index()].furniture();
        let header = &mut self.headers[drag.kind.index()];
        *header = pane_header(drag.kind);
        header.set_furniture(furniture);
        self.panes.float(drag.kind);
        Outcome::relaid()
    }

    /// Where the tool window floating pane `id` opens: where it was torn out
    /// to, carried by the press that tore it out, else at its home side.
    pub(super) fn tool_opening(&mut self, id: u32) -> ToolOpening {
        let Some(kind) = tool_pane(id) else {
            return ToolOpening::default();
        };
        self.openings[kind.index()].take().unwrap_or(ToolOpening {
            offset: self.float_homes[kind.index()],
            carry: None,
        })
    }

    /// A floating pane's tool window is being moved: over a dock, mark where
    /// the pane would land; let go there, dock it, which closes its tool
    /// window.
    pub(super) fn tool_moved(
        &mut self,
        moved: ToolMove,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        let Some(kind) = tool_pane(moved.id).filter(|&kind| self.panes.floats(kind)) else {
            return Outcome::none();
        };
        let landing = moved.over.and_then(|at| landing_at(layout, at, scale));
        let shown = if moved.ended { None } else { landing };
        if shown != self.tool_landing {
            for marked in [self.tool_landing, shown].into_iter().flatten() {
                damage.add(landing_mark(layout, marked, scale, theme));
            }
            self.tool_landing = shown;
        }
        match landing.filter(|_| moved.ended) {
            Some(landing) => {
                self.panes.move_to(kind, landing.side, landing.before);
                Outcome::relaid()
            }
            None => Outcome::none(),
        }
    }

    /// A floating pane's tool window went: closed, the pane is hidden as a
    /// docked pane's close mark hides it; refused, it docks at the foot of its
    /// home side.
    pub(super) fn tool_gone(
        &mut self,
        id: u32,
        why: ToolGone,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let Some(kind) = tool_pane(id).filter(|&kind| self.panes.floats(kind)) else {
            return Outcome::none();
        };
        self.openings[kind.index()] = None;
        match why {
            ToolGone::Closed => self.toggle_pane(kind, layout, damage),
            ToolGone::Refused => {
                self.panes.move_to(kind, self.panes.home(kind), usize::MAX);
                Outcome::relaid()
            }
        }
    }

    /// Note where each floating pane opens when nothing tore it out: just
    /// under the top band, against the edge of its home side.
    pub(super) fn settle_float_homes(&mut self, layout: &Layout) {
        let below = layout.top().bottom();
        for slot in layout.floating() {
            let left = match self.panes.home(slot.kind) {
                Side::Left => layout.window().left(),
                Side::Right => layout.window().right() - to_i32(slot.frame.width),
            };
            self.float_homes[slot.kind.index()] = (left, below);
        }
    }

    /// Turn a pane drag down, as Escape or the window losing the keyboard do;
    /// the band's own release then lands nothing and floats nothing.
    pub(super) fn turn_down_pane_drag(
        &mut self,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> bool {
        let Some(drag) = self.pane_drag.take() else {
            return false;
        };
        if let Some(landing) = drag.landing {
            damage.add(landing_mark(layout, landing, scale, theme));
        }
        true
    }

    /// Draw every band as the window does: lit while it has the keyboard.
    pub(super) fn headers_follow_focus(
        &mut self,
        focused: bool,
        layout: &Layout,
        damage: &mut Region,
    ) {
        let activation = if focused {
            tairix_controls::WindowActivationState::Active
        } else {
            tairix_controls::WindowActivationState::Inactive
        };
        for (kind, header) in PaneKind::ALL.iter().zip(self.headers.iter_mut()) {
            let mut furniture = header.furniture();
            if furniture.activation == activation {
                continue;
            }
            furniture.activation = activation;
            header.set_furniture(furniture);
            if let Some(slot) = layout.pane(*kind) {
                damage.add(slot.header);
            }
        }
    }
}

/// Whether `at` lies on `window`'s outermost pixels: where a held press's
/// pointer is pinned once it has left the window.
fn on_edge(window: Rect, at: Point) -> bool {
    at.x <= window.left()
        || at.y <= window.top()
        || at.x >= window.right() - 1
        || at.y >= window.bottom() - 1
}

/// Where a pane dragged to `at` lands: in the gap down a dock nearest the
/// pointer, or at the head of an empty dock when the pointer is near its edge.
fn landing_at(layout: &Layout, at: Point, scale: Scale) -> Option<Landing> {
    Side::BOTH.into_iter().find_map(|side| {
        let zone = zone(layout, side, scale);
        if !zone.contains(at) {
            return None;
        }
        let panes = &layout.dock_on(side).panes;
        let before = panes
            .iter()
            .position(|slot| {
                at.y < slot.frame.top() + tairix_geometry::to_i32(slot.frame.height / 2)
            })
            .unwrap_or(panes.len());
        Some(Landing { side, before })
    })
}

/// Where a drop lands on `side`: its dock, or a strip along the window's edge
/// between the bands where the dock is empty.
fn zone(layout: &Layout, side: Side, scale: Scale) -> Rect {
    let dock = layout.dock_on(side);
    if !dock.panes.is_empty() {
        return dock.rect;
    }
    let reach = scale.scale_length(EDGE_REACH);
    let below = layout.top().bottom();
    let band = Rect::new(
        0,
        below,
        layout.window().width,
        u32::try_from(layout.status().top() - below).unwrap_or(0),
    );
    match side {
        Side::Left => Rect::new(band.left(), band.top(), reach.min(band.width), band.height),
        Side::Right => Rect::new(
            band.right() - tairix_geometry::to_i32(reach.min(band.width)),
            band.top(),
            reach.min(band.width),
            band.height,
        ),
    }
}

/// The mark showing where a dragged pane lands: a bar across its dock in the
/// gap it would take, or down the edge of an empty dock's zone.
#[must_use]
pub(crate) fn landing_mark(layout: &Layout, landing: Landing, scale: Scale, theme: &Theme) -> Rect {
    let thickness = scale
        .scale_length(theme.metrics().seam_thickness)
        .max(1)
        .saturating_mul(2);
    let zone = zone(layout, landing.side, scale);
    let panes = &layout.dock_on(landing.side).panes;
    if panes.is_empty() {
        let left = match landing.side {
            Side::Left => zone.left(),
            Side::Right => zone.right() - tairix_geometry::to_i32(thickness),
        };
        return Rect::new(left, zone.top(), thickness, zone.height);
    }
    let half = tairix_geometry::to_i32(thickness / 2);
    let y = match panes.get(landing.before) {
        Some(slot) => slot.frame.top() - half - 1,
        None => panes
            .last()
            .map_or(zone.top(), |slot| slot.frame.bottom() + 1),
    };
    Rect::new(zone.left(), y.max(zone.top()), zone.width, thickness)
}
