//! What a carried drag shows under the pointer: one item as the tile its
//! listing draws it as, held where it was pressed, or several as a stack of
//! smaller pictures gathering from where they stood, the front card wearing
//! how many there are.
//!
//! Each card is a window of its own that nothing can be dropped on, painted
//! once and then only moved: a frame of the gather, or of the cards behind
//! replaying the pointer's path a little later each, moves windows and paints
//! nothing. A card is painted again only when what it is drawn from changes —
//! its artwork landing, a font, the theme or the scale.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver::display::RefreshRate;
use tairix_abi::time::NANOS_PER_MILLI;
use tairix_abi::window_ipc::DragOwnIcon;
use tairix_browse::render::{name_cut, Tiles};
use tairix_controls::{ControlState, IconTile};
use tairix_geometry::{to_i32, Point, Rect, Scale};
use tairix_icon::{IconArtwork, IconKind, IconRequest, Landed};
use tairix_inline::RingBuf;
use tairix_theme::motion::ease_out;
use tairix_theme::{MotionInteraction, Theme, Timeline};
use tairix_wm::{Compositor, PointerCatch, Surface, WindowId};

/// The side a stacked card's picture is drawn at, as a share of a listed
/// tile's: two thirds.
const STACK_SHARE: (u32, u32) = (2, 3);

/// How far each card behind the front is set up and to the left of the one in
/// front of it, in logical pixels.
const STACK_STEP: u32 = 7;

/// How much later in the gather each card behind the front sets out, in parts
/// of its whole span, so the stack arrives one card after another.
const STAGGER: u8 = 40;

/// How far behind the card in front of it each card follows the pointer, as a
/// share of the gather's span.
const TRAIL_SHARE: u64 = 6;

/// How many of the pointer's places the trail keeps, spread by time over the
/// back card's lag ([`Stack::keep`]) so they span it at any refresh rate.
const TRAIL_SAMPLES: usize = 32;

/// What one card is drawn as.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Look {
    /// The name it shows, for the one card of a drag that shows one.
    pub(crate) label: Option<String>,
    /// Its class picture.
    pub(crate) icon: IconKind,
    /// The item's own picture, where it carries one.
    pub(crate) own: Option<DragOwnIcon>,
    /// Its state, as its listing draws it.
    pub(crate) state: ControlState,
    /// How many items the front of a stack stands for.
    pub(crate) count: Option<u32>,
}

impl Look {
    /// The picture it is drawn by: the item's own where it carries one, else
    /// its class's.
    fn request(&self) -> IconRequest<'_> {
        IconRequest::from_carried(self.icon, self.own.as_ref())
    }
}

/// Where the pointer holds the front card's picture: from the picture's
/// centre, in pixels of a picture `side` across, so a picture drawn at another
/// size is held at the same place on it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PictureGrab {
    /// From the picture's centre, right and down.
    pub offset: (i32, i32),
    /// How wide the picture it was measured on is; `0` holds it at its centre.
    pub side: u32,
}

impl PictureGrab {
    /// Held at the picture's centre.
    pub const CENTRE: Self = Self {
        offset: (0, 0),
        side: 0,
    };

    /// The offset on a picture `side` across.
    fn on(self, side: u32) -> (i32, i32) {
        if self.side == 0 {
            return (0, 0);
        }
        let scaled = |value: i32| {
            let value = i64::from(value) * i64::from(side) / i64::from(self.side);
            i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
        };
        (scaled(self.offset.0), scaled(self.offset.1))
    }
}

/// One card: what it is drawn as, its window, and where it travels.
#[derive(Debug)]
struct Card {
    look: Look,
    window: Option<WindowId>,
    /// Its size, in physical pixels.
    size: (u32, u32),
    /// Where it rests: its origin, from the pointer.
    slot: (i32, i32),
    /// Where it set out from when the gather began.
    from: Point,
    /// Where it is drawn.
    at: Point,
}

impl Card {
    /// Where it rests with the pointer at `pointer`.
    const fn rest(&self, pointer: Point) -> Point {
        offset(pointer, self.slot)
    }
}

/// What a drag shows, and its cards' travel.
#[derive(Debug, Default)]
pub(crate) struct Stack {
    /// The front card first.
    cards: Vec<Card>,
    /// The gather from where the cards stood, once the first frame began it.
    gather: Option<Timeline>,
    /// Where frames found the pointer, oldest first, each with when: the path
    /// each card behind the front replays a little later.
    trail: RingBuf<(u64, Point), TRAIL_SAMPLES>,
    /// How far behind the card in front of it each card follows the pointer.
    lag_ns: u64,
    /// The pointer moved since a frame last took in where it is.
    owed: bool,
    /// Where the pointer was when a batch of input was last taken.
    seen: Point,
}

impl Stack {
    /// The cards `looks` show as — the front first, each with where its
    /// picture's centre stood on the screen, if it stood anywhere — with the
    /// pointer at `pointer`, holding the front picture at `grab`.
    pub(crate) fn new(
        looks: impl IntoIterator<Item = (Look, Option<Point>)>,
        pointer: Point,
        grab: PictureGrab,
        (scale, theme): (Scale, &Theme),
    ) -> Self {
        let layout = Tiles::Large.layout();
        let metrics = Tiles::Large.metrics(scale, theme);
        let cell = Rect::new(0, 0, metrics.cell_width, metrics.cell_height);
        let listed = layout.icon_side(cell, scale, theme);
        let small = listed.saturating_mul(STACK_SHARE.0) / STACK_SHARE.1;
        let step = to_i32(scale.scale_length(STACK_STEP));
        let mut cards: Vec<Card> = Vec::new();
        for (index, (look, stood)) in looks.into_iter().enumerate() {
            let (size, across) = if look.label.is_some() {
                ((metrics.cell_width, metrics.cell_height), listed)
            } else {
                let size = (
                    layout.width_for(small, scale, theme),
                    layout.height_for(small, scale, theme),
                );
                (size, small)
            };
            let label = look.label.as_deref().unwrap_or_default();
            let bounds = Rect::new(0, 0, size.0, size.1);
            let centre = layout
                .parts(bounds, scale, theme, label, name_cut(label))
                .and_then(|parts| parts.picture)
                .map_or((to_i32(size.0 / 2), to_i32(size.1 / 2)), |picture| {
                    (
                        picture.left() + to_i32(picture.width / 2),
                        picture.top() + to_i32(picture.height / 2),
                    )
                });
            let slot = match cards.first() {
                None => {
                    let (dx, dy) = grab.on(across);
                    (
                        held(centre.0.saturating_add(dx), size.0),
                        held(centre.1.saturating_add(dy), size.1),
                    )
                }
                Some(front) => {
                    let behind = to_i32(u32::try_from(index).unwrap_or(0)).saturating_mul(step);
                    (
                        front.slot.0.saturating_sub(behind),
                        front.slot.1.saturating_sub(behind),
                    )
                }
            };
            let at = stood.map_or_else(
                || offset(pointer, slot),
                |stood| Point::new(stood.x - centre.0, stood.y - centre.1),
            );
            if cards.try_reserve(1).is_err() {
                break;
            }
            cards.push(Card {
                look,
                window: None,
                size,
                slot,
                from: at,
                at,
            });
        }
        let lone = cards.len() < 2;
        // A card alone never travels, so it starts where it rests.
        if lone {
            for card in &mut cards {
                card.at = card.rest(pointer);
                card.from = card.at;
            }
        }
        Self {
            cards,
            gather: None,
            trail: RingBuf::new(),
            lag_ns: 0,
            owed: !lone,
            seen: pointer,
        }
    }

    /// Whether the stack has gathered under the pointer: its front card rests
    /// on it from then on.
    fn gathered(&self) -> bool {
        self.gather.is_some_and(|gather| !gather.running())
    }

    /// Paint every card afresh, `artwork` drawing their pictures, and open a
    /// window for each that has none: the back card first, so the front is
    /// above the rest.
    pub(crate) fn paint(
        &mut self,
        compositor: &mut Compositor,
        theme: &Theme,
        artwork: &mut dyn IconArtwork,
    ) {
        self.paint_where(compositor, theme, artwork, |_, _| true);
    }

    /// Paint afresh the cards whose picture `landed` moved.
    pub(crate) fn paint_landed(
        &mut self,
        compositor: &mut Compositor,
        theme: &Theme,
        (artwork, landed): (&mut dyn IconArtwork, &Landed),
    ) {
        self.paint_where(compositor, theme, artwork, |look, across| {
            landed.resolves(look.request(), across)
        });
    }

    /// Paint the cards whose look, its picture so many pixels across, `wanted`
    /// admits, as [`paint`](Self::paint) paints them.
    fn paint_where(
        &mut self,
        compositor: &mut Compositor,
        theme: &Theme,
        artwork: &mut dyn IconArtwork,
        wanted: impl Fn(&Look, u32) -> bool,
    ) {
        let scale = compositor.scale();
        for card in self.cards.iter_mut().rev() {
            if !wanted(&card.look, picture_side(card.size, (scale, theme))) {
                continue;
            }
            let Some(pixels) = paint_card(&card.look, card.size, (scale, theme), artwork) else {
                continue;
            };
            if let Some(window) = card.window {
                compositor.set_surface(window, pixels);
            } else {
                let window = compositor.add_window(card.at, pixels);
                compositor.set_pointer_catch(window, PointerCatch::None);
                compositor.raise(window);
                card.window = Some(window);
            }
        }
    }

    /// The pointer is at `pointer` once a batch of input has been taken: the
    /// card that rests on it moves with this batch, and the cards behind take
    /// it in at the next frame.
    pub(crate) fn follow(&mut self, pointer: Point, compositor: &mut Compositor) {
        if pointer == self.seen {
            return;
        }
        self.seen = pointer;
        let lone = self.cards.len() < 2;
        if lone || self.gathered() {
            if let Some(front) = self.cards.first_mut() {
                front.at = front.rest(pointer);
                if let Some(window) = front.window {
                    compositor.move_window(window, front.at);
                }
            }
        }
        self.owed |= !lone;
    }

    /// Move the cards of a stack to where they are at `now_ns`, the pointer at
    /// `pointer`: each follows where the pointer was so much later than the one
    /// in front of it, and while the gather runs each blends there from where
    /// it stood, one after another.
    pub(crate) fn advance(&mut self, now_ns: u64, pointer: Point, compositor: &mut Compositor) {
        if self.cards.len() < 2 {
            return;
        }
        if self.gather.is_none() {
            let span = compositor
                .theme()
                .motion()
                .duration(MotionInteraction::DragGather);
            self.lag_ns = u64::from(span).saturating_mul(NANOS_PER_MILLI) / TRAIL_SHARE;
            self.gather = Some(Timeline::start(now_ns, span));
        }
        if self.owed {
            self.owed = false;
            self.keep(now_ns, pointer);
        }
        let progress = self
            .gather
            .map_or(u8::MAX, |gather| gather.progress(now_ns));
        let (trail, lag_ns) = (&self.trail, self.lag_ns);
        for (index, card) in self.cards.iter_mut().enumerate() {
            let place = u64::try_from(index).unwrap_or(u64::MAX);
            let late = now_ns.saturating_sub(lag_ns.saturating_mul(place));
            let followed = offset(trail_at(trail, late, pointer), card.slot);
            let at = if progress == u8::MAX {
                followed
            } else {
                let lag = STAGGER.saturating_mul(u8::try_from(index).unwrap_or(u8::MAX));
                between(card.from, followed, ease_out(staged(progress, lag)))
            };
            if at != card.at {
                card.at = at;
                if let Some(window) = card.window {
                    compositor.move_window(window, at);
                }
            }
        }
        if let Some(gather) = self.gather.as_mut() {
            if gather.running() && gather.finished(now_ns) {
                gather.settle();
            }
        }
    }

    /// Take in that a frame found the pointer at `place` at `now_ns`.
    ///
    /// Places are kept by time rather than by frame: a newest place less than a
    /// share of the back card's lag past the one before it is replaced rather
    /// than kept, so the trail spans that lag at any refresh rate.
    fn keep(&mut self, now_ns: u64, place: Point) {
        let behind = u64::try_from(self.cards.len().saturating_sub(1)).unwrap_or(u64::MAX);
        let gaps = u64::try_from(TRAIL_SAMPLES - 2).unwrap_or(u64::MAX);
        let spacing = self.lag_ns.saturating_mul(behind) / gaps;
        let mut latest = self.trail.iter().rev().map(|&(at, _)| at);
        let standing_in = match (latest.next(), latest.next()) {
            (Some(newest), Some(kept)) => newest.saturating_sub(kept) < spacing,
            _ => false,
        };
        if standing_in {
            let _ = self.trail.pop_back();
        }
        let _ = self.trail.push_back_overwrite((now_ns, place));
    }

    /// When the next frame of the stack is due on a display refreshing at
    /// `refresh`: at once for a pointer not yet taken in, through the gather,
    /// and until the back card has replayed the pointer's last move; `None`
    /// once every card is at rest.
    pub(crate) fn next_frame_in(&self, now_ns: u64, refresh: RefreshRate) -> Option<u64> {
        if self.cards.len() < 2 {
            return None;
        }
        if self.owed {
            return Some(0);
        }
        if let Some(gather) = self.gather.filter(|gather| gather.running()) {
            return gather.next_frame_in(now_ns, refresh);
        }
        let behind = u64::try_from(self.cards.len() - 1).unwrap_or(u64::MAX);
        let settled = self
            .trail
            .back()
            .map(|&(at, _)| at.saturating_add(self.lag_ns.saturating_mul(behind)))?;
        (now_ns < settled).then(|| (settled - now_ns).min(refresh.frame_ns()))
    }

    /// Take every card's window down.
    pub(crate) fn close(&mut self, compositor: &mut Compositor) {
        for card in &mut self.cards {
            if let Some(window) = card.window.take() {
                compositor.remove(window);
            }
        }
    }

    /// Each card's window, the front first, as far as it has one.
    pub(crate) fn windows(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.cards.iter().filter_map(|card| card.window)
    }

    /// Hand `ask` the picture each card is drawn by and how many pixels across,
    /// the front first: what the stack still shows, for its producer to keep.
    pub(crate) fn each_picture(
        &self,
        (scale, theme): (Scale, &Theme),
        mut ask: impl FnMut(IconRequest<'_>, u32),
    ) {
        for card in &self.cards {
            ask(card.look.request(), picture_side(card.size, (scale, theme)));
        }
    }
}

/// Paint `look` into a surface `size` across at `scale` in `theme`: a listed
/// tile's picture through `artwork`, casting its soft shadow, under its name
/// where it shows one, and its count where it fronts a stack. `None` when the
/// heap will not give the surface.
fn paint_card(
    look: &Look,
    size: (u32, u32),
    (scale, theme): (Scale, &Theme),
    artwork: &mut dyn IconArtwork,
) -> Option<Surface> {
    let mut pixels = Surface::new(size.0, size.1)?;
    let bounds = Rect::new(0, 0, size.0, size.1);
    let label = look.label.clone().unwrap_or_default();
    let tile = Tiles::Large.named(label, look.state, look.icon);
    let tile = match look.count {
        Some(count) => tile.with_count(count),
        None => tile,
    };
    let across = picture_side(size, (scale, theme));
    let picture = artwork.shadowed(look.request(), across, IconTile::shadow_cast(scale, theme));
    tile.render(&mut pixels, bounds, scale, theme, picture);
    Some(pixels)
}

/// How many pixels across the picture of a card `size` across is drawn.
fn picture_side(size: (u32, u32), (scale, theme): (Scale, &Theme)) -> u32 {
    Tiles::Large
        .layout()
        .icon_side(Rect::new(0, 0, size.0, size.1), scale, theme)
}

/// The origin a card `extent` across takes so the point `held` into it lies
/// under the pointer, kept on the card: its slot along one axis.
fn held(point: i32, extent: u32) -> i32 {
    let last = to_i32(extent.saturating_sub(1));
    point.clamp(0, last).saturating_neg()
}

/// `point` moved by `by`.
const fn offset(point: Point, by: (i32, i32)) -> Point {
    Point::new(point.x.saturating_add(by.0), point.y.saturating_add(by.1))
}

/// Where the pointer was at `at` along `trail`: the last place a frame found it
/// by then, the earliest kept before any, or `pointer` with none kept.
fn trail_at(trail: &RingBuf<(u64, Point), TRAIL_SAMPLES>, at: u64, pointer: Point) -> Point {
    let mut found = None;
    for &(when, place) in trail {
        if when > at {
            break;
        }
        found = Some(place);
    }
    found
        .or_else(|| trail.front().map(|&(_, place)| place))
        .unwrap_or(pointer)
}

/// How far a card `lag` parts late is through a travel `progress` parts
/// through: nowhere until it sets out, and there when the travel ends.
fn staged(progress: u8, lag: u8) -> u8 {
    if progress == u8::MAX {
        return u8::MAX;
    }
    if progress <= lag {
        return 0;
    }
    let run = u32::from(u8::MAX - lag);
    let gone = u32::from(progress - lag);
    u8::try_from(gone * u32::from(u8::MAX) / run.max(1)).unwrap_or(u8::MAX)
}

/// The point `part` parts of the way from `from` to `to`.
fn between(from: Point, to: Point, part: u8) -> Point {
    let along = |from: i32, to: i32| {
        let span = i64::from(to) - i64::from(from);
        let moved = span * i64::from(part) / i64::from(u8::MAX);
        i32::try_from(i64::from(from) + moved).unwrap_or(to)
    };
    Point::new(along(from.x, to.x), along(from.y, to.y))
}

#[cfg(test)]
#[path = "stack_tests.rs"]
mod tests;
