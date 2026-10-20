//! Drawing the table.
//!
//! A frame is laid down in the order a real table stacks: the baize, the
//! empty places, every card at rest pile by pile, the hint and the landing
//! place, the cards in the air in the order they took off, and the cards
//! under the pointer last. The caller clips to the damage it presents, and
//! every card is skipped unless the clip admits part of it, so a two-card
//! repaint costs two cards.
//!
//! While the victory runs the frame is not cleared: each bouncing card is
//! drawn where it is now over whatever is already there, which is what
//! leaves its trail.

use tairix_font::BitmapFont;
use tairix_geometry::{to_i32, Point, Rect};
use tairix_raster::{Affine, Color, Ring, RingInk, Surface, SUBPIXEL};
use tairix_util::mathf;

use crate::art::{CardArt, Palette};
use crate::card::{CardId, Slot};
use crate::layout::Layout;
use crate::motion::{Facing, Motion};
use crate::table::Table;
use crate::variant::{PileId, Role, Variant};

/// Where the keyboard is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Cursor {
    /// The pile it is on.
    pub pile: PileId,
    /// The card it is on, counting from the bottom, or `None` for an empty
    /// place.
    pub depth: Option<usize>,
}

/// What the status line says.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Status<'a> {
    /// The game and the deal, on the left.
    pub game: &'a str,
    /// What is happening, in the middle.
    pub message: &'a str,
    /// The readings, on the right.
    pub readings: &'a str,
}

/// Everything one frame is drawn from.
#[derive(Copy, Clone, Debug)]
pub struct View<'a> {
    /// Where everything goes.
    pub layout: &'a Layout,
    /// The cards.
    pub table: &'a Table,
    /// Where each card is drawn.
    pub motion: &'a Motion,
    /// The cards under the pointer, bottom first, and where each is drawn.
    pub held: &'a [(CardId, Point)],
    /// The keyboard's place, unless it holds cards, which show it.
    pub cursor: Option<Cursor>,
    /// The pile a drop would land on now.
    pub target: Option<PileId>,
    /// The status line.
    pub status: Status<'a>,
    /// The face the status line is set in.
    pub font: BitmapFont,
    /// The instant drawn.
    pub now_ns: u64,
}

/// Draw the whole frame.
pub fn frame(surface: &mut Surface, view: &View<'_>, art: &mut CardArt) {
    let palette = *art.palette();
    band(surface, view.layout, view.status, view.font, &palette);
    if let Some(cascade) = view.motion.cascade().filter(|cascade| cascade.begun()) {
        // The trail is what the frame already holds; only the bouncing cards
        // are new.
        for (card, at) in cascade.bouncing() {
            card_tile(surface, view, art, card, at, Facing::Up, false);
        }
        return;
    }
    felt(surface, view.layout.felt, art);
    let table = view.table;
    for pile in table.shape().all() {
        place(surface, view, pile, &palette);
        resting(surface, view, art, pile);
    }
    if let Some((glow, strength)) = view.motion.lit(view.now_ns) {
        let accent = palette.accent;
        for &rect in &glow.rects {
            outline(
                surface,
                view.layout,
                rect,
                Color::rgba(accent.r, accent.g, accent.b, strength),
                3,
            );
        }
    }
    if let Some(target) = view.target {
        let rect = view.layout.landing(table, target);
        let accent = palette.accent;
        outline(
            surface,
            view.layout,
            rect,
            Color::rgba(accent.r, accent.g, accent.b, 220),
            2,
        );
    }
    for &card in view.motion.airborne() {
        let shown = view.motion.shown(card, view.now_ns);
        card_tile(surface, view, art, card, shown.at, shown.facing, true);
    }
    for &(card, at) in view.held {
        let up = table
            .find(card)
            .and_then(|(pile, depth)| table.pile(pile).get(depth))
            .is_none_or(|slot| slot.up());
        let facing = if up { Facing::Up } else { Facing::Down };
        card_tile(surface, view, art, card, at, facing, true);
    }
    if let Some(cursor) = view.cursor {
        focus(surface, view, cursor, &palette);
    }
}

/// The table while no deal is on it yet: the baize, its empty places and
/// the status line.
pub fn waiting(
    surface: &mut Surface,
    layout: &Layout,
    status: Status<'_>,
    font: BitmapFont,
    art: &mut CardArt,
) {
    let palette = *art.palette();
    felt(surface, layout.felt, art);
    band(surface, layout, status, font, &palette);
    let shape = layout.variant().shape();
    for pile in shape.all() {
        recess(
            surface,
            layout,
            layout.card_at(layout.origin(pile)),
            &palette,
        );
    }
}

/// The baize over `area`, copied from the one `art` keeps; shaded straight
/// onto the frame when there is no memory to keep one.
fn felt(surface: &mut Surface, area: Rect, art: &mut CardArt) {
    let palette = *art.palette();
    let kept = art.baize(area.width, area.height, |baize| {
        shade_felt(baize, Rect::new(0, 0, area.width, area.height), &palette);
    });
    match kept {
        Some(baize) => surface.overwrite(area.left(), area.top(), baize),
        None => shade_felt(surface, area, &palette),
    }
}

/// The baize's colour at its centre deepening towards the edges, with a fine
/// grain that keeps the shading from banding, over `area`.
fn shade_felt(surface: &mut Surface, area: Rect, palette: &Palette) {
    let Some((x, y)) = area.surface_origin() else {
        return;
    };
    let Some((columns, rows)) = surface.admitted(x, y, area.width, area.height) else {
        return;
    };
    let centre = (
        i64::from(x) + i64::from(area.width / 2),
        i64::from(y) + i64::from(area.height * 2 / 5),
    );
    let reach = (
        i64::from(area.width.max(2) / 2) * 13 / 10,
        i64::from(area.height.max(2) / 2) * 15 / 10,
    );
    let (inner, outer) = (palette.felt, palette.felt_edge);
    for row in rows {
        let Some((first, span)) =
            surface.row_span_mut(row, columns.start, columns.end - columns.start)
        else {
            continue;
        };
        let dy = (i64::from(row) - centre.1) * 1024 / reach.1;
        for (column, pixel) in (first..).zip(span.iter_mut()) {
            let dx = (i64::from(column) - centre.0) * 1024 / reach.0;
            let distance = ((dx * dx + dy * dy) / 1024).min(1024);
            // Darkening grows with the square of the distance, so the centre
            // stays open and the edges close in.
            let weight = distance * distance / 1024;
            let grain = i64::from(
                tairix_mix::mix32(column.wrapping_mul(0x9E37) ^ row.wrapping_mul(0x85EB)) & 3,
            ) - 1;
            let channel = |a: u8, b: u8| {
                let value = i64::from(a) + (i64::from(b) - i64::from(a)) * weight / 1024 + grain;
                u8::try_from(value.clamp(0, 255)).unwrap_or(a)
            };
            *pixel = Color::rgb(
                channel(inner.r, outer.r),
                channel(inner.g, outer.g),
                channel(inner.b, outer.b),
            )
            .premultiply();
        }
    }
}

/// The status line: a darkened band with the game on the left, what is
/// happening in the middle, and the readings on the right.
fn band(
    surface: &mut Surface,
    layout: &Layout,
    status: Status<'_>,
    font: BitmapFont,
    palette: &Palette,
) {
    let area = layout.status;
    if let Some((x, y)) = area.surface_origin() {
        surface.fill_round_rect(x, y, area.width, area.height, 0, palette.band);
    }
    let pad = to_i32(area.height / 2);
    let top = font.centred_top(area.top(), area.height);
    font.draw_text(surface, area.left() + pad, top, status.game, palette.muted);
    let readings = to_i32(font.text_width(status.readings));
    font.draw_text(
        surface,
        area.right() - pad - readings,
        top,
        status.readings,
        palette.text,
    );
    let message = to_i32(font.text_width(status.message));
    let centre = area.left() + to_i32(area.width / 2);
    font.draw_text(
        surface,
        centre - message / 2,
        top,
        status.message,
        palette.text,
    );
}

/// An empty place's recess in the baize.
fn recess(surface: &mut Surface, layout: &Layout, rect: Rect, palette: &Palette) {
    if let Some((x, y)) = rect.surface_origin() {
        surface.fill_round_rect(x, y, rect.width, rect.height, layout.radius, palette.slot);
        surface.wash_ring(
            x,
            y,
            rect.width,
            rect.height,
            Ring::uniform(layout.radius, layout.hairline),
            RingInk::Solid(palette.slot_rim),
        );
    }
}

/// A pile's empty place: a recess in the baize, with a faint mark where the
/// rules say what may start it.
fn place(surface: &mut Surface, view: &View<'_>, pile: PileId, palette: &Palette) {
    let layout = view.layout;
    let rect = layout.card_at(layout.origin(pile));
    if !admits(surface, rect) {
        return;
    }
    let table = view.table;
    // A pile whose bottom card rests on its place covers it completely.
    let covered = table
        .pile(pile)
        .first()
        .is_some_and(|slot| !view.motion.flying(slot.card()) && !held(view, slot.card()));
    if covered {
        return;
    }
    recess(surface, layout, rect, palette);
    let mark = match (table.rules().variant, table.role(pile)) {
        (Variant::Klondike(_) | Variant::FreeCell, Some(Role::Foundation)) => Some("A"),
        (Variant::Klondike(_), Some(Role::Tableau)) => Some("K"),
        _ => None,
    };
    if let Some(mark) = mark {
        let font = BitmapFont::new(view.font.family(), (rect.height * 30 / 100).max(8))
            .with_weight(tairix_abi::font_ipc::FontWeight::BOLD);
        let x = rect.left() + to_i32(rect.width.saturating_sub(font.text_width(mark)) / 2);
        let y = font.centred_top(rect.top(), rect.height);
        font.draw_text(surface, x, y, mark, palette.slot_rim);
    }
    if table.role(pile) == Some(Role::Stock) && table.allows(crate::table::Action::Recycle) {
        recycle_mark(surface, rect, layout.hairline, palette.slot_rim);
    }
}

/// The turn-back arrow an emptied stock shows while the waste may still be
/// turned over.
fn recycle_mark(surface: &mut Surface, rect: Rect, hairline: u32, ink: Color) {
    let centre = rect.center();
    let radius = f64::from(rect.width.min(rect.height)) * 0.22;
    let sub = |value: f64| mathf::round_i32(value * f64::from(SUBPIXEL));
    let arc: alloc::vec::Vec<(i32, i32)> = (0..=24)
        .map(|step| {
            let angle = core::f64::consts::PI * (0.15 + 1.55 * f64::from(step) / 24.0);
            (
                sub(f64::from(centre.x) + radius * mathf::cos(angle)),
                sub(f64::from(centre.y) - radius * mathf::sin(angle)),
            )
        })
        .collect();
    let weight = i32::try_from(hairline.max(1) * 3).unwrap_or(3) * SUBPIXEL;
    surface.stroke_polyline(&arc, weight, ink);
    if let Some(&(tip_x, tip_y)) = arc.last() {
        let head = mathf::round_i32(radius * 0.45) * SUBPIXEL;
        surface.fill_polygon_subpixel(
            &[
                (tip_x - head, tip_y - head / 3),
                (tip_x + head / 2, tip_y - head),
                (tip_x + head / 3, tip_y + head / 2),
            ],
            ink,
        );
    }
}

fn held(view: &View<'_>, card: CardId) -> bool {
    view.held.iter().any(|&(c, _)| c == card)
}

/// `pile`'s cards at rest, bottom first, leaving out any wholly hidden under
/// the card above it and any in the air or under the pointer.
fn resting(surface: &mut Surface, view: &View<'_>, art: &mut CardArt, pile: PileId) {
    let cards: &[Slot] = view.table.pile(pile);
    for (depth, slot) in cards.iter().enumerate() {
        let card = slot.card();
        if view.motion.flying(card) || held(view, card) {
            continue;
        }
        let shown = view.motion.shown(card, view.now_ns);
        let hidden = cards.get(depth + 1).is_some_and(|above| {
            let above = above.card();
            !view.motion.flying(above)
                && !held(view, above)
                && view.motion.shown(above, view.now_ns).at == shown.at
        });
        if hidden {
            continue;
        }
        card_tile(surface, view, art, card, shown.at, shown.facing, false);
    }
}

/// One card drawn at `at`, its shadow under it.
fn card_tile(
    surface: &mut Surface,
    view: &View<'_>,
    art: &mut CardArt,
    card: CardId,
    at: Point,
    facing: Facing,
    lifted: bool,
) {
    let layout = view.layout;
    let rect = layout.card_at(at);
    let reach = layout.card_height / 10;
    if !admits(surface, grown(rect, reach)) {
        return;
    }
    let (face_up, width) = match facing {
        Facing::Up => (true, 1000),
        Facing::Down => (false, 1000),
        Facing::Turning { face, width } => (face, width),
    };
    let tint = if lifted {
        art.palette().lifted
    } else {
        art.palette().shadow
    };
    if width == 1000 {
        if let Some(shadow) = art.shadow(lifted) {
            let (dx, dy) = shadow.offset;
            surface.blit_tinted(at.x + dx, at.y + dy, &shadow.mask, tint);
        }
    }
    let face = view.table.face(card);
    let tile = if face_up { art.face(face) } else { art.back() };
    let Some(tile) = tile else {
        return;
    };
    if width == 1000 {
        surface.blit(at.x, at.y, tile);
        return;
    }
    if width < 20 {
        return;
    }
    // Turning over: drawn narrowed about its own centre.
    let share = f64::from(width) / 1000.0;
    let narrowed = f64::from(layout.card_width) * share;
    let offset = (f64::from(layout.card_width) - narrowed) / 2.0;
    surface.blit_transformed(
        tile,
        Affine {
            a: share,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: f64::from(at.x) + offset,
            f: f64::from(at.y),
        },
    );
}

/// The keyboard's ring, round the card it is on and every card above it, or
/// round the empty place.
fn focus(surface: &mut Surface, view: &View<'_>, cursor: Cursor, palette: &Palette) {
    let layout = view.layout;
    let table = view.table;
    let fan = layout.fan(table, cursor.pile);
    let rect = match cursor.depth {
        Some(depth) => {
            let first = layout.card_at(layout.place(table, cursor.pile, depth, fan));
            let top = table.pile(cursor.pile).len().saturating_sub(1).max(depth);
            first.union(&layout.card_at(layout.place(table, cursor.pile, top, fan)))
        }
        None => layout.card_at(layout.origin(cursor.pile)),
    };
    outline(
        surface,
        layout,
        grown(rect, layout.hairline * 2),
        palette.text,
        2,
    );
}

/// A rounded ring `thickness` hairlines wide around `rect`.
fn outline(surface: &mut Surface, layout: &Layout, rect: Rect, ink: Color, thickness: u32) {
    let Some((x, y)) = rect.surface_origin() else {
        return;
    };
    surface.wash_ring(
        x,
        y,
        rect.width,
        rect.height,
        Ring::uniform(layout.radius + layout.hairline, layout.hairline * thickness),
        RingInk::Solid(ink),
    );
}

/// `rect` grown by `by` on every side.
fn grown(rect: Rect, by: u32) -> Rect {
    Rect::new(
        rect.left() - to_i32(by),
        rect.top() - to_i32(by),
        rect.width.saturating_add(by * 2),
        rect.height.saturating_add(by * 2),
    )
}

/// Whether any pixel of `rect` is one `surface` would write.
fn admits(surface: &Surface, rect: Rect) -> bool {
    let left = rect.left().max(0);
    let top = rect.top().max(0);
    surface.admits(
        u32::try_from(left).unwrap_or(u32::MAX),
        u32::try_from(top).unwrap_or(u32::MAX),
        rect.width.saturating_sub(left.abs_diff(rect.left())),
        rect.height.saturating_sub(top.abs_diff(rect.top())),
    )
}

#[cfg(test)]
#[path = "paint_tests.rs"]
mod tests;
