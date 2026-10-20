//! Input: the pointer, the keyboard and the menus, turned into moves.
//!
//! A press only takes hold of cards; dragging them past the desktop's drag
//! slop lifts them, and the release lands them on the place they overlap
//! most that takes them, or sends them back. A press and release with no drag
//! between is a click, which sends the cards to the best place they can go.
//!
//! The keyboard works the same table with a cursor. `Space` picks the cards
//! from the cursor up into a hand that follows the cursor from pile to pile
//! and puts them down where it is; `Enter` sends them to their best place.

use alloc::vec::Vec;

use tairix_geometry::{to_i32, Point, Rect, Region};
use tairix_input::{past_drag_slop, InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_rng::RandU64;

use crate::card::CardId;
use crate::game::{shift, Drag, Game, Note, Pace, Press};
use crate::layout::Layout;
use crate::menu::{deal_number, Command};
use crate::motion::Glide;
use crate::paint::Cursor;
use crate::play::Play;
use crate::prefs::Preferences;
use crate::table::{Action, Table};
use crate::variant::{PileId, Role, Variant};

/// What one key asks for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Intent {
    /// Move the cursor along its row.
    Along(i32),
    /// Move the cursor up or down the table.
    Across(i32),
    /// Pick the cards at the cursor up, or put the hand down.
    Take,
    /// Send the cards at the cursor to their best place, or put the hand
    /// down.
    Send,
    /// Put the hand back, or take the cursor away.
    Escape,
    /// Turn the stock.
    Draw,
    /// A command, as a menu gives one.
    Command(Command),
}

impl Intent {
    fn of(key: Key, modifiers: Modifiers) -> Option<Self> {
        Some(match key {
            Key::Named(NamedKey::Left) => Self::Along(-1),
            Key::Named(NamedKey::Right) => Self::Along(1),
            Key::Named(NamedKey::Up) => Self::Across(-1),
            Key::Named(NamedKey::Down) => Self::Across(1),
            Key::Named(NamedKey::Enter) => Self::Send,
            Key::Named(NamedKey::Escape) => Self::Escape,
            Key::Named(NamedKey::Backspace) => Self::Command(Command::Undo),
            Key::Named(NamedKey::Function { number: 2 }) => Self::Command(Command::NewGame),
            Key::Char('z' | 'Z') if modifiers.ctrl && modifiers.shift => {
                Self::Command(Command::Redo)
            }
            Key::Char('z' | 'Z') if modifiers.ctrl => Self::Command(Command::Undo),
            Key::Char('y' | 'Y') if modifiers.ctrl => Self::Command(Command::Redo),
            Key::Char('b' | 'B') if modifiers.ctrl => Self::Command(Command::Rewind),
            _ if modifiers.ctrl || modifiers.alt || modifiers.meta => return None,
            Key::Char(' ') => Self::Take,
            Key::Char('n' | 'N') => Self::Command(Command::NewGame),
            Key::Char('r' | 'R') => Self::Command(Command::Restart),
            Key::Char('u' | 'U') => Self::Command(Command::Undo),
            Key::Char('h' | 'H') => Self::Command(Command::Hint),
            Key::Char('f' | 'F') => Self::Command(Command::Finish),
            Key::Char('s' | 'S') => Self::Command(Command::Statistics),
            Key::Char('d' | 'D') => Self::Draw,
            Key::Char(digit) => {
                let index = usize::try_from(digit.to_digit(10)?).ok()?.checked_sub(1)?;
                Self::Command(Command::Play(*Variant::ALL.get(index)?))
            }
            Key::Named(_) => return None,
        })
    }
}

impl Game {
    /// Route one pointer event, answering whether the view changed.
    pub fn on_pointer(&mut self, event: &InputEvent, now_ns: u64, damage: &mut Region) -> bool {
        match *event {
            InputEvent::PointerMoved { to } => self.moved(to, now_ns, damage),
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } => self.pressed(now_ns, damage),
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => self.released(now_ns, damage),
            InputEvent::PointerPressed {
                button: PointerButton::Secondary,
            } => self.pressed_secondary(now_ns, damage),
            InputEvent::PointerCancelled => self.put_back(now_ns, damage),
            _ => false,
        }
    }

    fn moved(&mut self, to: Point, now_ns: u64, damage: &mut Region) -> bool {
        self.pointer = to;
        if self.pointer_holds() {
            self.drag_to(to, damage);
            return true;
        }
        match self.press {
            Some(press) if self.drag.is_none() && past_drag_slop(press.at, to, self.scale) => {
                self.lift(press.pile, press.depth, None, now_ns, damage);
                self.drag_to(to, damage);
                true
            }
            _ => false,
        }
    }

    fn pressed(&mut self, now_ns: u64, damage: &mut Region) -> bool {
        if self.skip_victory(damage) {
            return true;
        }
        let mut changed = self.put_back(now_ns, damage);
        changed |= self.hide_cursor(damage);
        let Some((pile, depth)) = self
            .table()
            .and_then(|table| self.layout.hit(table, self.pointer))
        else {
            return changed;
        };
        if self.table().and_then(|table| table.role(pile)) == Some(Role::Stock) {
            return self.turn_stock(now_ns, damage) || changed;
        }
        let Some(depth) = depth else {
            return changed;
        };
        if self
            .table()
            .is_some_and(|table| movable_from(table, pile, depth))
        {
            self.press = Some(Press {
                pile,
                depth,
                at: self.pointer,
            });
            changed
        } else {
            self.refuse(pile, depth, now_ns, damage) || changed
        }
    }

    fn released(&mut self, now_ns: u64, damage: &mut Region) -> bool {
        if self.pointer_holds() {
            return self.put_down(now_ns, damage);
        }
        let Some(press) = self.press.take() else {
            return false;
        };
        self.send(press.pile, press.depth, now_ns, damage)
    }

    fn pressed_secondary(&mut self, now_ns: u64, damage: &mut Region) -> bool {
        if self.skip_victory(damage) {
            return true;
        }
        let changed = self.put_back(now_ns, damage) | self.hide_cursor(damage);
        self.want_context(self.pointer);
        changed
    }

    /// Whether the cards in hand are the pointer's.
    fn pointer_holds(&self) -> bool {
        self.drag.as_ref().is_some_and(|drag| drag.glide.is_none())
    }

    /// Take the cards from `depth` up `pile` into the hand, carried by the
    /// keyboard when `glide` says how.
    fn lift(
        &mut self,
        pile: PileId,
        depth: usize,
        glide: Option<Glide>,
        now_ns: u64,
        damage: &mut Region,
    ) {
        let Some(table) = self.play.as_ref().map(Play::table) else {
            return;
        };
        let cards: Vec<CardId> = table
            .pile(pile)
            .iter()
            .skip(depth)
            .map(|slot| slot.card())
            .collect();
        let held: Vec<(CardId, Point)> = cards
            .iter()
            .map(|&card| (card, self.motion.shown(card, now_ns).at))
            .collect();
        self.motion.ground(&cards);
        let Some(&(_, base)) = held.first() else {
            return;
        };
        let size = self.layout.card_size();
        for &(_, at) in &held {
            damage.add(crate::motion::card_rect(at, size));
        }
        self.press = None;
        self.drag = Some(Drag {
            pile,
            depth,
            grab: Point::new(self.pointer.x - base.x, self.pointer.y - base.y),
            held,
            target: None,
            glide,
        });
    }

    /// Carry a hand the pointer holds to `to`, and light where it would land.
    fn drag_to(&mut self, to: Point, damage: &mut Region) {
        let size = self.layout.card_size();
        let Some(drag) = &mut self.drag else {
            return;
        };
        let corner = Point::new(to.x - drag.grab.x, to.y - drag.grab.y);
        shift(&mut drag.held, corner, size, damage);
        let (pile, count) = (drag.pile, drag.held.len());
        let base = self.layout.card_at(corner);
        let target = self
            .play
            .as_ref()
            .and_then(|play| landing(&self.layout, play.table(), pile, count, base));
        self.retarget_hand(target, damage);
    }

    /// Light `target` as where the hand would land, unlighting the last.
    fn retarget_hand(&mut self, target: Option<PileId>, damage: &mut Region) {
        let Some(drag) = &mut self.drag else {
            return;
        };
        if drag.target == target {
            return;
        }
        let was = core::mem::replace(&mut drag.target, target);
        if let Some(table) = self.play.as_ref().map(Play::table) {
            for pile in [was, target].into_iter().flatten() {
                damage.add(lit(&self.layout, table, pile));
            }
        }
    }

    /// Land the hand where it would land now, or send it back if it has
    /// nowhere to land.
    fn put_down(&mut self, now_ns: u64, damage: &mut Region) -> bool {
        let Some(drag) = self.drag.take() else {
            return false;
        };
        self.press = None;
        if let (Some(target), Some(table)) = (drag.target, self.table()) {
            damage.add(lit(&self.layout, table, target));
        }
        if let Some(to) = drag.target {
            let count = u8::try_from(drag.held.len()).unwrap_or(u8::MAX);
            let action = Action::Move {
                from: drag.pile,
                to,
                count,
            };
            if self.act(action, Pace::Slide, &drag.held, now_ns, damage) {
                return true;
            }
        }
        self.animate(now_ns, Pace::Return, &drag.held, damage);
        true
    }

    /// Send any hand back where it came from, answering whether there was
    /// one.
    fn put_back(&mut self, now_ns: u64, damage: &mut Region) -> bool {
        let target = self.drag.as_ref().and_then(|drag| drag.target);
        let held = self.release();
        if let (Some(target), Some(table)) = (target, self.table()) {
            damage.add(lit(&self.layout, table, target));
        }
        if held.is_empty() {
            return false;
        }
        self.animate(now_ns, Pace::Return, &held, damage);
        true
    }

    /// Send the cards from `depth` up `pile` to the best place they can go,
    /// or show that there is none.
    fn send(&mut self, pile: PileId, depth: usize, now_ns: u64, damage: &mut Region) -> bool {
        let Some(table) = self.table() else {
            return false;
        };
        let count = table.pile(pile).len().saturating_sub(depth);
        match best_target(table, pile, count).zip(u8::try_from(count).ok()) {
            Some((to, count)) => self.act(
                Action::Move {
                    from: pile,
                    to,
                    count,
                },
                Pace::Steps,
                &[],
                now_ns,
                damage,
            ),
            None => self.refuse(pile, depth, now_ns, damage),
        }
    }

    /// Hop the cards from `depth` up `pile`: they have nowhere to go.
    fn refuse(&mut self, pile: PileId, depth: usize, now_ns: u64, damage: &mut Region) -> bool {
        let Some(table) = self.play.as_ref().map(Play::table) else {
            return false;
        };
        let cards: Vec<CardId> = table
            .pile(pile)
            .iter()
            .skip(depth)
            .map(|slot| slot.card())
            .collect();
        let rise = self.layout.card_height / 12;
        self.motion.hop(&cards, rise, now_ns, damage);
        !cards.is_empty()
    }

    /// Turn the stock onto the waste, deal a row, or turn the waste back,
    /// whichever the table allows; or say why it allows none.
    fn turn_stock(&mut self, now_ns: u64, damage: &mut Region) -> bool {
        let Some(table) = self.table() else {
            return false;
        };
        let shape = table.shape();
        let action = [Action::Draw, Action::Recycle]
            .into_iter()
            .find(|&action| table.allows(action));
        let blocked = match table.rules().variant {
            Variant::Spider(_) => Note::DealBlocked,
            Variant::Klondike(_) | Variant::FreeCell => Note::NoRedeal,
        };
        let cards_left = [shape.stock, shape.waste]
            .into_iter()
            .flatten()
            .any(|pile| !table.pile(pile).is_empty());
        match action {
            Some(action) => self.act(action, Pace::Steps, &[], now_ns, damage),
            None if cards_left => {
                self.say(blocked, now_ns, false, damage);
                true
            }
            None => false,
        }
    }

    /// End the victory early, if one is running.
    fn skip_victory(&mut self, damage: &mut Region) -> bool {
        if self.motion.cascade().is_none() {
            return false;
        }
        self.end_cascade(damage);
        true
    }

    // --- The keyboard ---------------------------------------------------

    /// Route one key press, answering whether the view changed.
    pub fn on_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        now_ns: u64,
        rng: &mut dyn RandU64,
        damage: &mut Region,
    ) -> bool {
        if self.skip_victory(damage) {
            return true;
        }
        let Some(intent) = Intent::of(key, modifiers) else {
            return false;
        };
        // The pointer's hand is the pointer's: a key only puts it back.
        if self.pointer_holds() {
            return intent == Intent::Escape && self.put_back(now_ns, damage);
        }
        let holding = self.drag.is_some();
        match intent {
            Intent::Along(by) => self.along(by, now_ns, damage),
            Intent::Across(by) => self.across(by, now_ns, damage),
            Intent::Take | Intent::Send if holding => self.put_down_here(now_ns, damage),
            Intent::Take => self.take(now_ns, damage),
            Intent::Send => self.send_cursor(now_ns, damage),
            Intent::Escape if holding => self.put_back(now_ns, damage),
            Intent::Escape => self.hide_cursor(damage),
            Intent::Draw => {
                let returned = self.put_back(now_ns, damage);
                self.turn_stock(now_ns, damage) || returned
            }
            Intent::Command(command) => self.command(command, now_ns, rng, damage),
        }
    }

    /// The piles in the order the keyboard walks them: the top row left to
    /// right, and the columns.
    fn rows(&self) -> (Vec<PileId>, Vec<PileId>) {
        let Some(table) = self.table() else {
            return (Vec::new(), Vec::new());
        };
        let shape = table.shape();
        let mut top: Vec<PileId> = shape
            .all()
            .filter(|&pile| !shape.tableau.contains(pile))
            .collect();
        top.sort_by_key(|&pile| self.layout.origin(pile).x);
        (top, shape.tableau.iter().collect())
    }

    /// The cursor, put on the first column's top card if there was none.
    fn cursor_or_start(&mut self, damage: &mut Region) -> Option<Cursor> {
        if self.cursor.is_none() {
            let (_, columns) = self.rows();
            let cursor = self.cursor_on(*columns.first()?);
            self.cursor = Some(cursor);
            self.light_cursor(Some(cursor), damage);
        }
        self.cursor
    }

    /// The cursor on `pile`'s top card, or on its empty place.
    fn cursor_on(&self, pile: PileId) -> Cursor {
        let len = self.table().map_or(0, |table| table.pile(pile).len());
        Cursor {
            pile,
            depth: len.checked_sub(1),
        }
    }

    fn light_cursor(&self, cursor: Option<Cursor>, damage: &mut Region) {
        if let (Some(cursor), Some(table)) = (cursor, self.table()) {
            let area = grown(
                self.layout.pile_area(table, cursor.pile),
                self.layout.hairline * 4,
            );
            damage.add(area.union(&lit(&self.layout, table, cursor.pile)));
        }
    }

    fn hide_cursor(&mut self, damage: &mut Region) -> bool {
        let was = self.cursor.take();
        self.light_cursor(was, damage);
        was.is_some()
    }

    /// Put the cursor on `next`, the hand following it.
    fn move_cursor(&mut self, next: Cursor, now_ns: u64, damage: &mut Region) -> bool {
        let was = self.cursor.replace(next);
        if was == Some(next) {
            return false;
        }
        self.light_cursor(was, damage);
        self.light_cursor(Some(next), damage);
        self.carry_to(next.pile, now_ns, damage);
        true
    }

    fn along(&mut self, by: i32, now_ns: u64, damage: &mut Region) -> bool {
        let fresh = self.cursor.is_none();
        let Some(cursor) = self.cursor_or_start(damage) else {
            return false;
        };
        if fresh {
            return true;
        }
        let (top, columns) = self.rows();
        let row = if columns.contains(&cursor.pile) {
            columns
        } else {
            top
        };
        let next = row
            .iter()
            .position(|&pile| pile == cursor.pile)
            .and_then(|at| at.checked_add_signed(isize::try_from(by).ok()?))
            .and_then(|at| row.get(at).copied());
        next.is_some_and(|pile| self.move_cursor(self.cursor_on(pile), now_ns, damage))
    }

    fn across(&mut self, by: i32, now_ns: u64, damage: &mut Region) -> bool {
        let fresh = self.cursor.is_none();
        let Some(cursor) = self.cursor_or_start(damage) else {
            return false;
        };
        if fresh {
            return true;
        }
        let Some(table) = self.table() else {
            return false;
        };
        let (top, columns) = self.rows();
        let on_column = columns.contains(&cursor.pile);
        let len = table.pile(cursor.pile).len();
        // A column's cards are climbed only with the hand empty, and only
        // over the run that may be picked up.
        let first = len.saturating_sub(table.movable(cursor.pile).max(1));
        let climbing = on_column && self.drag.is_none();
        let nearest = |row: &[PileId]| {
            let x = self.layout.origin(cursor.pile).x;
            row.iter()
                .copied()
                .min_by_key(|&pile| self.layout.origin(pile).x.abs_diff(x))
        };
        let next = match (cursor.depth, by < 0) {
            (Some(depth), true) if climbing && depth > first => Some(Cursor {
                depth: Some(depth - 1),
                ..cursor
            }),
            (Some(depth), false) if climbing && depth + 1 < len => Some(Cursor {
                depth: Some(depth + 1),
                ..cursor
            }),
            (_, true) if on_column => nearest(&top).map(|pile| self.cursor_on(pile)),
            (_, false) if !on_column => nearest(&columns).map(|pile| self.cursor_on(pile)),
            _ => None,
        };
        next.is_some_and(|next| self.move_cursor(next, now_ns, damage))
    }

    /// Pick the cards from the cursor up into the hand.
    fn take(&mut self, now_ns: u64, damage: &mut Region) -> bool {
        let Some(cursor) = self.cursor_or_start(damage) else {
            return false;
        };
        let Some(table) = self.table() else {
            return false;
        };
        if table.role(cursor.pile) == Some(Role::Stock) {
            return self.turn_stock(now_ns, damage);
        }
        let Some(depth) = cursor.depth else {
            return true;
        };
        if !movable_from(table, cursor.pile, depth) {
            return self.refuse(cursor.pile, depth, now_ns, damage);
        }
        let Some(card) = Layout::card(table, cursor.pile, depth) else {
            return false;
        };
        let from = self.motion.shown(card, now_ns).at;
        let glide = Glide::new(from, from, now_ns, self.motion.reduced_motion());
        self.lift(cursor.pile, depth, Some(glide), now_ns, damage);
        self.carry_to(cursor.pile, now_ns, damage);
        self.light_cursor(Some(cursor), damage);
        true
    }

    /// Glide a hand the keyboard holds to hover over `pile`, lighting the
    /// pile if the hand may land there.
    fn carry_to(&mut self, pile: PileId, now_ns: u64, damage: &mut Region) {
        let Some(table) = self.play.as_ref().map(Play::table) else {
            return;
        };
        let Some(drag) = &self.drag else {
            return;
        };
        let (Some(glide), Some(&(_, base))) = (drag.glide, drag.held.first()) else {
            return;
        };
        let rest = if pile == drag.pile {
            Layout::card(table, pile, drag.depth)
                .and_then(|card| self.placements.get(card.index()))
                .map_or(base, |placement| placement.at)
        } else {
            let landing = self.layout.landing(table, pile);
            Point::new(landing.left(), landing.top())
        };
        let lift = to_i32(self.layout.card_height / 9);
        let hover = Point::new(rest.x + lift / 3, rest.y - lift);
        let count = u8::try_from(drag.held.len()).unwrap_or(u8::MAX);
        let legal = table.allows(Action::Move {
            from: drag.pile,
            to: pile,
            count,
        });
        let from = glide.at(now_ns);
        let reduced = self.motion.reduced_motion();
        if let Some(drag) = &mut self.drag {
            drag.glide = Some(Glide::new(from, hover, now_ns, reduced));
        }
        self.retarget_hand(legal.then_some(pile), damage);
        self.carry(now_ns, damage);
    }

    /// Put the hand down on the pile the cursor is on.
    fn put_down_here(&mut self, now_ns: u64, damage: &mut Region) -> bool {
        let landed = self.put_down(now_ns, damage);
        if let Some(cursor) = self.cursor {
            self.light_cursor(Some(cursor), damage);
            let next = self.cursor_on(cursor.pile);
            self.cursor = Some(next);
            self.light_cursor(Some(next), damage);
        }
        landed
    }

    /// Send the cards from the cursor up to their best place.
    fn send_cursor(&mut self, now_ns: u64, damage: &mut Region) -> bool {
        let Some(cursor) = self.cursor_or_start(damage) else {
            return false;
        };
        if self.table().and_then(|table| table.role(cursor.pile)) == Some(Role::Stock) {
            return self.turn_stock(now_ns, damage);
        }
        let Some(depth) = cursor.depth else {
            return true;
        };
        let movable = self
            .table()
            .is_some_and(|table| movable_from(table, cursor.pile, depth));
        let sent = if movable {
            self.send(cursor.pile, depth, now_ns, damage)
        } else {
            self.refuse(cursor.pile, depth, now_ns, damage)
        };
        self.light_cursor(Some(cursor), damage);
        let next = self.cursor_on(cursor.pile);
        self.cursor = Some(next);
        self.light_cursor(Some(next), damage);
        sent
    }

    // --- Commands -------------------------------------------------------

    /// Carry out `command`, as the menus and the keyboard give it, answering
    /// whether the view changed.
    pub fn command(
        &mut self,
        command: Command,
        now_ns: u64,
        rng: &mut dyn RandU64,
        damage: &mut Region,
    ) -> bool {
        self.end_cascade(damage);
        self.put_back(now_ns, damage);
        let prefs = *self.prefs();
        let variant = self.play().map_or(prefs.variant, Play::variant);
        match command {
            Command::NewGame => self.new_game(variant, None, now_ns, rng, damage),
            Command::Restart => {
                let number = self.play().map(Play::number);
                self.new_game(variant, number, now_ns, rng, damage);
            }
            Command::Undo => return self.undo(now_ns, damage),
            Command::Redo => return self.redo(now_ns, damage),
            Command::Hint => self.hint(now_ns, damage),
            Command::Rewind => self.rewind(now_ns, damage),
            Command::Finish => return self.finish(now_ns, damage),
            Command::Statistics => self.show_statistics(),
            Command::Play(chosen) => {
                self.choose(
                    Preferences {
                        variant: chosen,
                        ..prefs
                    },
                    now_ns,
                    damage,
                );
                self.new_game(chosen, None, now_ns, rng, damage);
            }
            Command::Score(scoring) => {
                self.choose(Preferences { scoring, ..prefs }, now_ns, damage);
            }
            Command::Winnable => {
                self.choose(
                    Preferences {
                        winnable: !prefs.winnable,
                        ..prefs
                    },
                    now_ns,
                    damage,
                );
            }
            Command::AutoHome => {
                self.choose(
                    Preferences {
                        auto_home: !prefs.auto_home,
                        ..prefs
                    },
                    now_ns,
                    damage,
                );
            }
            Command::FourColours => {
                self.choose(
                    Preferences {
                        four_colours: !prefs.four_colours,
                        ..prefs
                    },
                    now_ns,
                    damage,
                );
            }
            Command::Back(back) => self.choose(Preferences { back, ..prefs }, now_ns, damage),
            Command::Baize(baize) => self.choose(Preferences { baize, ..prefs }, now_ns, damage),
        }
        true
    }

    /// A deal's number was typed into the table's menu: deal it, answering
    /// whether `text` named one.
    pub fn deal_entered(
        &mut self,
        text: &str,
        now_ns: u64,
        rng: &mut dyn RandU64,
        damage: &mut Region,
    ) -> bool {
        let variant = self.play().map_or(self.prefs().variant, Play::variant);
        let Some(number) = deal_number(variant, text) else {
            return false;
        };
        self.end_cascade(damage);
        self.put_back(now_ns, damage);
        self.new_game(variant, Some(number), now_ns, rng, damage);
        true
    }
}

/// Whether the cards from `depth` up `pile` may be picked up together.
pub(crate) fn movable_from(table: &Table, pile: PileId, depth: usize) -> bool {
    let len = table.pile(pile).len();
    depth < len && len - depth <= table.movable(pile)
}

/// The best place the top `count` cards of `from` can go: home for a lone
/// card; then onto a column, one of the same suit first in Spider; then an
/// empty column, unless they would leave one behind; then a free cell. A card
/// already home stays there: taking one back is a drag, never a click.
pub(crate) fn best_target(table: &Table, from: PileId, count: usize) -> Option<PileId> {
    let shape = table.shape();
    if table.role(from) == Some(Role::Foundation) {
        return None;
    }
    let units = u8::try_from(count).ok()?;
    let allows = |to: PileId| {
        table.allows(Action::Move {
            from,
            to,
            count: units,
        })
    };
    if count == 1 {
        if let Some(to) = shape.foundations.iter().find(|&to| allows(to)) {
            return Some(to);
        }
    }
    let cards = table.pile(from);
    let base = cards
        .len()
        .checked_sub(count)
        .and_then(|at| cards.get(at))
        .map(|slot| table.face(slot.card()));
    let spider = matches!(table.rules().variant, Variant::Spider(_));
    let same_suit = |to: PileId| {
        spider
            && base
                .zip(table.top_face(to))
                .is_some_and(|(base, top)| base.suit == top.suit)
    };
    let onto = || {
        shape
            .tableau
            .iter()
            .filter(|&to| !table.pile(to).is_empty() && allows(to))
    };
    if let Some(to) = onto().find(|&to| same_suit(to)).or_else(|| onto().next()) {
        return Some(to);
    }
    let empties = table.role(from) == Some(Role::Tableau) && cards.len() == count;
    if !empties {
        if let Some(to) = shape
            .tableau
            .iter()
            .find(|&to| table.pile(to).is_empty() && allows(to))
        {
            return Some(to);
        }
    }
    if count == 1 && table.role(from) == Some(Role::Tableau) {
        return shape.cells.iter().find(|&to| allows(to));
    }
    None
}

/// The pile a hand whose bottom card is drawn at `base` overlaps most, of
/// those that would take it.
fn landing(
    layout: &Layout,
    table: &Table,
    from: PileId,
    count: usize,
    base: Rect,
) -> Option<PileId> {
    let units = u8::try_from(count).ok()?;
    table
        .shape()
        .all()
        .filter(|&to| {
            table.allows(Action::Move {
                from,
                to,
                count: units,
            })
        })
        .map(|to| {
            (
                overlap(
                    layout
                        .pile_area(table, to)
                        .union(&layout.landing(table, to)),
                    base,
                ),
                to,
            )
        })
        .filter(|&(area, _)| area > 0)
        .max_by_key(|&(area, _)| area)
        .map(|(_, to)| to)
}

/// The area two rectangles share.
fn overlap(a: Rect, b: Rect) -> u64 {
    let width = a
        .right()
        .min(b.right())
        .saturating_sub(a.left().max(b.left()))
        .max(0);
    let height = a
        .bottom()
        .min(b.bottom())
        .saturating_sub(a.top().max(b.top()))
        .max(0);
    u64::try_from(width).unwrap_or(0) * u64::try_from(height).unwrap_or(0)
}

/// What lighting `pile` as the hand's landing place repaints.
fn lit(layout: &Layout, table: &Table, pile: PileId) -> Rect {
    grown(layout.landing(table, pile), layout.hairline * 4)
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

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
