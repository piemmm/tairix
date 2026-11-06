//! The selection overlay: a veil over the screen while the user drags out an
//! area or, after Space, picks a window, the choice lit through it.
//!
//! It takes no pointer itself, so the window under the pointer is the one a
//! pick finds, and a change repaints only what it lit and unlit.

use tairix_input::PointerButton;
use tairix_wm::{
    Color, Compositor, InputEvent, Point, PointerCatch, Rect, Region, Surface, WindowId,
};

/// What the user chose.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Picked {
    /// The area dragged out, in desktop pixels.
    Area(Rect),
    /// The window clicked, with its shadow unless Alt was held.
    Window { id: WindowId, shadow: bool },
}

/// The veil's opacity over everything not being chosen.
const VEIL_ALPHA: u8 = 0x60;

/// The smallest area a drag takes, in pixels each way: less is a slip.
const AREA_MIN: u32 = 4;

/// What a press chooses.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Pick {
    Area,
    Window,
}

/// The overlay and what it shows lit.
pub(super) struct SelectOverlay {
    window: WindowId,
    screen: Rect,
    pick: Pick,
    at: Point,
    /// Where the drag began, while one is held.
    anchor: Option<Point>,
    /// The area dragged out, or the window under the pointer.
    lit: Option<Rect>,
    hovered: Option<WindowId>,
    edge: Color,
    border: u32,
    owed: Region,
}

impl SelectOverlay {
    /// Veil the screen, picking a window when `windows` and an area otherwise,
    /// the pointer at `at`; `None` where the heap refuses the veil.
    pub(super) fn open(windows: bool, at: Point, compositor: &mut Compositor) -> Option<Self> {
        let screen = compositor.screen_rect();
        let mut pixels = Surface::new(screen.width, screen.height)?;
        pixels.fill(veil());
        let window = compositor.add_window(screen.origin, pixels);
        compositor.set_pointer_catch(window, PointerCatch::None);
        compositor.raise(window);
        let mut overlay = Self {
            window,
            screen,
            pick: if windows { Pick::Window } else { Pick::Area },
            at,
            anchor: None,
            lit: None,
            hovered: None,
            edge: Color::from(compositor.theme().palette().accent),
            border: compositor.scale().scale_length(1).max(1),
            owed: Region::new(),
        };
        overlay.hover(compositor);
        overlay.settle(compositor);
        Some(overlay)
    }

    /// The overlay's window.
    pub(super) const fn window(&self) -> WindowId {
        self.window
    }

    /// Take `event`: what was chosen, once a drag ends or a window is clicked.
    /// `alt` is whether Alt is held, which leaves a window's shadow out.
    pub(super) fn pointer(
        &mut self,
        event: &InputEvent,
        alt: bool,
        compositor: &Compositor,
    ) -> Option<Picked> {
        match *event {
            InputEvent::PointerMoved { to } => {
                self.at = to;
                match (self.pick, self.anchor) {
                    (Pick::Area, Some(anchor)) => self.light(Some(spanned(anchor, to))),
                    (Pick::Window, _) => self.hover(compositor),
                    (Pick::Area, None) => {}
                }
                None
            }
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => match self.pick {
                Pick::Area => {
                    self.anchor = Some(self.at);
                    None
                }
                Pick::Window => self.hovered.map(|id| Picked::Window { id, shadow: !alt }),
            },
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => {
                let anchor = self.anchor.take()?;
                let area = spanned(anchor, self.at).intersection(&self.screen);
                if area.width >= AREA_MIN && area.height >= AREA_MIN {
                    return Some(Picked::Area(area));
                }
                self.light(None);
                None
            }
            InputEvent::PointerCancelled => {
                self.anchor = None;
                if self.pick == Pick::Area {
                    self.light(None);
                }
                None
            }
            _ => None,
        }
    }

    /// Swap between picking an area and picking a window: Space.
    pub(super) fn toggle(&mut self, compositor: &Compositor) {
        self.anchor = None;
        self.hovered = None;
        self.light(None);
        self.pick = match self.pick {
            Pick::Area => Pick::Window,
            Pick::Window => Pick::Area,
        };
        self.hover(compositor);
    }

    /// Repaint what the events since the last settle lit and unlit; what a
    /// repaint the heap refuses leaves owed is painted at the next settle.
    pub(super) fn settle(&mut self, compositor: &mut Compositor) {
        if self.owed.is_empty() {
            return;
        }
        let origin = self.screen.origin;
        let lit = self.lit.map(|lit| local(lit, origin));
        let (border, edge) = (self.border, self.edge);
        self.owed.translate(-origin.x, -origin.y);
        let size = (self.screen.width, self.screen.height);
        let painted = compositor.repaint_window(self.window, size, &self.owed, |surface, rects| {
            for rect in rects {
                paint(surface, *rect, lit, (border, edge));
            }
        });
        if painted {
            self.owed.clear();
        } else {
            self.owed.translate(origin.x, origin.y);
        }
    }

    /// Take the veil down.
    pub(super) fn close(self, compositor: &mut Compositor) {
        compositor.remove(self.window);
    }

    /// Light the window under the pointer, in window pick.
    fn hover(&mut self, compositor: &Compositor) {
        if self.pick != Pick::Window {
            return;
        }
        let hovered = compositor.window_at(self.at);
        if hovered == self.hovered {
            return;
        }
        self.hovered = hovered;
        let bounds = hovered
            .and_then(|id| compositor.window(id))
            .map(tairix_wm::Window::bounds);
        self.light(bounds);
    }

    /// Light `next` in place of what was lit, owing both with their borders.
    fn light(&mut self, next: Option<Rect>) {
        if next == self.lit {
            return;
        }
        for rect in self.lit.into_iter().chain(next) {
            self.owed
                .add(rect.outset(self.border).intersection(&self.screen));
        }
        self.lit = next;
    }
}

/// The veil's colour.
const fn veil() -> Color {
    Color::rgba(0, 0, 0, VEIL_ALPHA)
}

/// The rectangle `a` and `b` are opposite corners of.
fn spanned(a: Point, b: Point) -> Rect {
    Rect::new(
        a.x.min(b.x),
        a.y.min(b.y),
        a.x.abs_diff(b.x),
        a.y.abs_diff(b.y),
    )
}

/// `rect` relative to `origin`.
fn local(rect: Rect, origin: Point) -> Rect {
    Rect::new(
        rect.left().saturating_sub(origin.x),
        rect.top().saturating_sub(origin.y),
        rect.width,
        rect.height,
    )
}

/// Paint `rect` of the overlay: the veil, the lit rectangle clear through it,
/// and a border of `border` pixels just outside what is lit.
fn paint(surface: &mut Surface, rect: Rect, lit: Option<Rect>, (border, edge): (u32, Color)) {
    fill(surface, rect, veil());
    let Some(lit) = lit else {
        return;
    };
    fill(surface, rect.intersection(&lit), Color::TRANSPARENT);
    let thick = i32::try_from(border).unwrap_or(1);
    let span = lit.width.saturating_add(border.saturating_mul(2));
    let (left, top) = (
        lit.left().saturating_sub(thick),
        lit.top().saturating_sub(thick),
    );
    for line in [
        Rect::new(left, top, span, border),
        Rect::new(left, lit.bottom(), span, border),
        Rect::new(left, lit.top(), border, lit.height),
        Rect::new(lit.right(), lit.top(), border, lit.height),
    ] {
        fill(surface, rect.intersection(&line), edge);
    }
}

fn fill(surface: &mut Surface, rect: Rect, color: Color) {
    if let Some((x, y)) = rect.surface_origin() {
        surface.fill_rect(x, y, rect.width, rect.height, color);
    }
}

#[cfg(test)]
mod tests {
    use super::spanned;
    use tairix_wm::{Point, Rect};

    #[test]
    fn an_area_is_the_same_whichever_corner_the_drag_began_at() {
        let (a, b) = (Point::new(40, 10), Point::new(10, 70));
        assert_eq!(spanned(a, b), Rect::new(10, 10, 30, 60));
        assert_eq!(spanned(b, a), spanned(a, b));
    }
}
