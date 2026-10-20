//! The table: every pile, the rules a move must satisfy, and the record each
//! move leaves so that it can be taken back exactly.
//!
//! Nothing here draws, keeps time, or performs I/O. An [`Action`] is what a
//! player asks for; [`Table::apply`] answers the [`Applied`] record of what it
//! did — including the consequences the rules impose, a card turned face up
//! and a completed Spider run sent home — and [`Table::undo`] reverses exactly
//! that record. The solver, the hints and the undo history all stand on that
//! one pair.

use alloc::vec::Vec;

use tairix_mix::mix64;

use crate::card::{CardId, Deck, Face, Rank, Slot, Suit};
use crate::deal::{shuffled, DealNumber};
use crate::variant::{Draw, PileId, Recycles, Role, Rules, Shape, Variant};

/// Cards in a completed Spider run.
const RUN: usize = Rank::COUNT as usize;

/// What a player asks the table to do.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Action {
    /// Move the top `count` cards of `from` onto `to`, keeping their order.
    Move {
        /// The pile the cards leave.
        from: PileId,
        /// The pile they land on.
        to: PileId,
        /// How many cards, from the top.
        count: u8,
    },
    /// Turn cards from the stock: Klondike turns them onto the waste, Spider
    /// deals one onto every column.
    Draw,
    /// Turn Klondike's waste back over to form the stock again.
    Recycle,
}

/// A completed Spider run the rules sent home.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Completion {
    /// The column it was completed on.
    pub column: PileId,
    /// The foundation it went to.
    pub foundation: PileId,
    /// Whether the card it uncovered was turned face up.
    pub turned: bool,
}

/// Most runs one action can complete: a Spider deal, one per column.
const MOST_COMPLETIONS: usize = 10;

/// What an action did, exactly enough to reverse it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Applied {
    /// What was asked for.
    pub action: Action,
    /// How many cards a [`Action::Draw`] moved.
    pub drawn: u8,
    /// Whether a move turned the card it uncovered face up.
    pub turned: bool,
    completions: [Option<Completion>; MOST_COMPLETIONS],
}

impl Applied {
    pub(crate) const fn of(action: Action) -> Self {
        Self {
            action,
            drawn: 0,
            turned: false,
            completions: [None; MOST_COMPLETIONS],
        }
    }

    /// The runs it completed, in the order they went home.
    #[must_use]
    pub fn completions(&self) -> impl DoubleEndedIterator<Item = Completion> + '_ {
        self.completions.iter().flatten().copied()
    }

    fn complete(&mut self, completion: Completion) {
        if let Some(slot) = self.completions.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(completion);
        }
    }
}

/// Every pile of one game and the rules it is played by.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Table {
    rules: Rules,
    shape: Shape,
    deck: Deck,
    piles: Vec<Vec<Slot>>,
    /// How often the waste has been turned back.
    recycled: u8,
}

impl Table {
    /// Deal `number` under `rules`, answering the table and the cards in the
    /// order they were dealt.
    #[must_use]
    pub fn deal(rules: Rules, number: DealNumber) -> (Self, Vec<CardId>) {
        let variant = rules.variant;
        let shape = variant.shape();
        let mut table = Self {
            rules,
            shape,
            deck: variant.deck(),
            piles: alloc::vec![Vec::new(); shape.piles()],
            recycled: 0,
        };
        let cards = shuffled(variant, number);
        let columns = shape.tableau;
        let mut order = Vec::with_capacity(cards.len());
        let mut next = cards.into_iter();
        match variant {
            Variant::Klondike(_) => {
                for row in 0..columns.len() {
                    for col in row..columns.len() {
                        if let (Some(card), Some(pile)) = (next.next(), columns.nth(col)) {
                            table.push(pile, Slot::new(card, col == row));
                            order.push(card);
                        }
                    }
                }
            }
            Variant::FreeCell => {
                for (index, card) in next.by_ref().enumerate() {
                    if let Some(pile) = columns.nth(index % columns.len()) {
                        table.push(pile, Slot::new(card, true));
                        order.push(card);
                    }
                }
            }
            Variant::Spider(_) => {
                /// Fifty-four cards go out: six to each of the first four
                /// columns and five to each of the rest.
                const DEALT: usize = 54;
                for index in 0..DEALT {
                    let (Some(card), Some(pile)) =
                        (next.next(), columns.nth(index % columns.len()))
                    else {
                        continue;
                    };
                    let last = index + columns.len() >= DEALT;
                    table.push(pile, Slot::new(card, last));
                    order.push(card);
                }
            }
        }
        if let Some(stock) = shape.stock {
            for card in next {
                table.push(stock, Slot::new(card, false));
            }
        }
        (table, order)
    }

    /// The rules in force.
    #[must_use]
    pub const fn rules(&self) -> Rules {
        self.rules
    }

    /// Where the piles lie.
    #[must_use]
    pub const fn shape(&self) -> Shape {
        self.shape
    }

    /// The deck the game was dealt from.
    #[must_use]
    pub const fn deck(&self) -> Deck {
        self.deck
    }

    /// What `card` shows.
    #[must_use]
    pub fn face(&self, card: CardId) -> Face {
        self.deck.face(card)
    }

    /// The cards of `pile`, bottom first.
    #[must_use]
    pub fn pile(&self, pile: PileId) -> &[Slot] {
        self.piles.get(pile.index()).map_or(&[], Vec::as_slice)
    }

    fn pile_mut(&mut self, pile: PileId) -> Option<&mut Vec<Slot>> {
        self.piles.get_mut(pile.index())
    }

    /// Lay `slot` on `pile`.
    fn push(&mut self, pile: PileId, slot: Slot) {
        if let Some(cards) = self.pile_mut(pile) {
            cards.push(slot);
        }
    }

    /// Take the top card off `pile`.
    fn pop(&mut self, pile: PileId) -> Option<Slot> {
        self.pile_mut(pile)?.pop()
    }

    /// Move the top `count` cards of `from` onto `to`, in order, without
    /// allocating: the solver does this millions of times.
    fn transfer(&mut self, from: PileId, to: PileId, count: usize) {
        let (f, t) = (from.index(), to.index());
        let (low, high) = (f.min(t), f.max(t));
        let Some((below, above)) = self.piles.split_at_mut_checked(high) else {
            return;
        };
        let (Some(first), Some(second)) = (below.get_mut(low), above.first_mut()) else {
            return;
        };
        let (source, target) = if f < t {
            (first, second)
        } else {
            (second, first)
        };
        let at = source.len().saturating_sub(count);
        target.extend_from_slice(&source[at..]);
        source.truncate(at);
    }

    /// The top card of `pile`.
    #[must_use]
    pub fn top(&self, pile: PileId) -> Option<Slot> {
        self.pile(pile).last().copied()
    }

    /// What the top card of `pile` shows, when it lies face up.
    #[must_use]
    pub fn top_face(&self, pile: PileId) -> Option<Face> {
        self.top(pile)
            .filter(|slot| slot.up())
            .map(|slot| self.face(slot.card()))
    }

    /// How often the waste has been turned back.
    #[must_use]
    pub const fn recycled(&self) -> u8 {
        self.recycled
    }

    /// Whether `pile` is one of this table's.
    #[must_use]
    pub fn holds(&self, pile: PileId) -> bool {
        pile.index() < self.piles.len()
    }

    /// What `pile` is for.
    #[must_use]
    pub fn role(&self, pile: PileId) -> Option<Role> {
        self.shape.role(pile)
    }

    /// Cards on the foundations.
    #[must_use]
    pub fn home(&self) -> usize {
        self.shape
            .foundations
            .iter()
            .map(|pile| self.pile(pile).len())
            .sum()
    }

    /// Whether every card is home.
    #[must_use]
    pub fn won(&self) -> bool {
        self.home() == self.deck.count()
    }

    /// Where `card` lies: its pile and its depth from the bottom.
    #[must_use]
    pub fn find(&self, card: CardId) -> Option<(PileId, usize)> {
        self.shape.all().find_map(|pile| {
            self.pile(pile)
                .iter()
                .position(|slot| slot.card() == card)
                .map(|depth| (pile, depth))
        })
    }

    // --- What may move --------------------------------------------------

    /// How many cards from the top of `pile` may be picked up together: one
    /// for a waste or a cell, the face-up run the variant lets move from a
    /// column, none from the stock.
    #[must_use]
    pub fn movable(&self, pile: PileId) -> usize {
        let cards = self.pile(pile);
        let Some(top) = cards.last().filter(|slot| slot.up()) else {
            return 0;
        };
        match self.role(pile) {
            Some(Role::Waste | Role::Cell) => 1,
            Some(Role::Foundation) => {
                usize::from(matches!(self.rules.variant, Variant::Klondike(_)))
            }
            Some(Role::Tableau) => {
                let mut run = 1;
                let mut above = self.face(top.card());
                for slot in cards.iter().rev().skip(1) {
                    if !slot.up() {
                        break;
                    }
                    let face = self.face(slot.card());
                    if !self.links(above, face) {
                        break;
                    }
                    above = face;
                    run += 1;
                }
                run
            }
            Some(Role::Stock) | None => 0,
        }
    }

    /// Whether `upper` may sit on `lower` within a run that moves as one.
    fn links(&self, upper: Face, lower: Face) -> bool {
        match self.rules.variant {
            Variant::Klondike(_) | Variant::FreeCell => upper.alternates_on(lower),
            Variant::Spider(_) => upper.follows_suit_on(lower),
        }
    }

    /// Whether a unit of `count` cards whose bottom card is `base` may land
    /// on `to`, wherever the unit comes from.
    #[must_use]
    pub fn accepts(&self, to: PileId, base: Face, count: usize) -> bool {
        match self.role(to) {
            Some(Role::Foundation) => {
                count == 1
                    && !matches!(self.rules.variant, Variant::Spider(_))
                    && base.builds_on(self.top_face(to))
            }
            Some(Role::Cell) => count == 1 && self.pile(to).is_empty(),
            Some(Role::Tableau) => match self.top(to) {
                None => match self.rules.variant {
                    Variant::Klondike(_) => base.rank == Rank::KING,
                    Variant::FreeCell | Variant::Spider(_) => true,
                },
                Some(top) if top.up() => {
                    let top = self.face(top.card());
                    match self.rules.variant {
                        Variant::Klondike(_) | Variant::FreeCell => base.alternates_on(top),
                        Variant::Spider(_) => base.descends_on(top),
                    }
                }
                Some(_) => false,
            },
            Some(Role::Stock | Role::Waste) | None => false,
        }
    }

    /// The most cards FreeCell may move onto `to` in one go: one through
    /// each free cell, doubled by each empty column it does not land in.
    #[must_use]
    pub fn capacity(&self, to: PileId) -> usize {
        if self.rules.variant != Variant::FreeCell {
            return usize::MAX;
        }
        let free_cells = self
            .shape
            .cells
            .iter()
            .filter(|&cell| self.pile(cell).is_empty())
            .count();
        let empty_columns = self
            .shape
            .tableau
            .iter()
            .filter(|&column| column != to && self.pile(column).is_empty())
            .count();
        (free_cells + 1)
            .checked_shl(u32::try_from(empty_columns).unwrap_or(u32::MAX))
            .unwrap_or(usize::MAX)
    }

    /// Whether `action` is one the rules allow now.
    #[must_use]
    pub fn allows(&self, action: Action) -> bool {
        match action {
            Action::Move { from, to, count } => self.allows_move(from, to, usize::from(count)),
            Action::Draw => self.may_draw(),
            Action::Recycle => self.may_recycle(),
        }
    }

    fn allows_move(&self, from: PileId, to: PileId, count: usize) -> bool {
        if from == to || count == 0 || !self.holds(from) || !self.holds(to) {
            return false;
        }
        if count > self.movable(from) || count > self.capacity(to) {
            return false;
        }
        self.base(from, count)
            .is_some_and(|base| self.accepts(to, base, count))
    }

    /// What the bottom card of the top `count` of `pile` shows.
    fn base(&self, pile: PileId, count: usize) -> Option<Face> {
        let cards = self.pile(pile);
        let at = cards.len().checked_sub(count)?;
        cards.get(at).map(|slot| self.face(slot.card()))
    }

    fn may_draw(&self) -> bool {
        let Some(stock) = self.shape.stock else {
            return false;
        };
        if self.pile(stock).is_empty() {
            return false;
        }
        match self.rules.variant {
            // A deal may not land on an empty column, which would bury it.
            Variant::Spider(_) => self
                .shape
                .tableau
                .iter()
                .all(|column| !self.pile(column).is_empty()),
            Variant::Klondike(_) | Variant::FreeCell => true,
        }
    }

    fn may_recycle(&self) -> bool {
        let (Some(stock), Some(waste)) = (self.shape.stock, self.shape.waste) else {
            return false;
        };
        self.pile(stock).is_empty()
            && !self.pile(waste).is_empty()
            && self.rules.recycles.allow(self.recycled)
    }

    // --- Doing it, and taking it back -----------------------------------

    /// Carry `action` out with every consequence the rules impose, answering
    /// what was done, or `None` — the table untouched — for an action the
    /// rules refuse.
    pub fn apply(&mut self, action: Action) -> Option<Applied> {
        if !self.allows(action) {
            return None;
        }
        let mut applied = Applied::of(action);
        match action {
            Action::Move { from, to, count } => {
                self.transfer(from, to, usize::from(count));
                applied.turned = self.turn_up(from);
                for column in [to, from] {
                    self.send_home_run(column, &mut applied);
                }
            }
            Action::Draw => self.draw(&mut applied),
            Action::Recycle => {
                let (Some(stock), Some(waste)) = (self.shape.stock, self.shape.waste) else {
                    return None;
                };
                while let Some(slot) = self.pop(waste) {
                    self.push(stock, slot.turned(false));
                }
                self.recycled = self.recycled.saturating_add(1);
            }
        }
        Some(applied)
    }

    fn draw(&mut self, applied: &mut Applied) {
        let Some(stock) = self.shape.stock else {
            return;
        };
        match self.rules.variant {
            Variant::Klondike(draw) => {
                let Some(waste) = self.shape.waste else {
                    return;
                };
                for _ in 0..draw.count() {
                    let Some(slot) = self.pop(stock) else {
                        break;
                    };
                    self.push(waste, slot.turned(true));
                    applied.drawn += 1;
                }
            }
            Variant::Spider(_) => {
                for column in self.shape.tableau.iter() {
                    let Some(slot) = self.pop(stock) else {
                        break;
                    };
                    self.push(column, slot.turned(true));
                    applied.drawn += 1;
                }
                for column in self.shape.tableau.iter() {
                    self.send_home_run(column, applied);
                }
            }
            Variant::FreeCell => {}
        }
    }

    /// Turn the top card of `pile` face up if it lies face down, answering
    /// whether it did.
    fn turn_up(&mut self, pile: PileId) -> bool {
        if self.role(pile) != Some(Role::Tableau) {
            return false;
        }
        match self.pile_mut(pile).and_then(|cards| cards.last_mut()) {
            Some(top) if !top.up() => {
                *top = top.turned(true);
                true
            }
            _ => false,
        }
    }

    /// Send a complete king-to-ace run of one suit on top of `column` home,
    /// recording it.
    fn send_home_run(&mut self, column: PileId, applied: &mut Applied) {
        if !matches!(self.rules.variant, Variant::Spider(_))
            || self.role(column) != Some(Role::Tableau)
        {
            return;
        }
        if self.movable(column) < RUN {
            return;
        }
        let Some(foundation) = self
            .shape
            .foundations
            .iter()
            .find(|&pile| self.pile(pile).is_empty())
        else {
            return;
        };
        self.transfer(column, foundation, RUN);
        let turned = self.turn_up(column);
        applied.complete(Completion {
            column,
            foundation,
            turned,
        });
    }

    /// Reverse `applied`, the record of the last action carried out on this
    /// table.
    pub fn undo(&mut self, applied: &Applied) {
        for completion in applied.completions().rev() {
            if completion.turned {
                self.turn_down(completion.column);
            }
            self.transfer(completion.foundation, completion.column, RUN);
        }
        match applied.action {
            Action::Move { from, to, count } => {
                if applied.turned {
                    self.turn_down(from);
                }
                self.transfer(to, from, usize::from(count));
            }
            Action::Draw => self.undraw(applied.drawn),
            Action::Recycle => {
                let (Some(stock), Some(waste)) = (self.shape.stock, self.shape.waste) else {
                    return;
                };
                while let Some(slot) = self.pop(stock) {
                    self.push(waste, slot.turned(true));
                }
                self.recycled = self.recycled.saturating_sub(1);
            }
        }
    }

    fn undraw(&mut self, drawn: u8) {
        let Some(stock) = self.shape.stock else {
            return;
        };
        match self.rules.variant {
            Variant::Klondike(_) => {
                let Some(waste) = self.shape.waste else {
                    return;
                };
                for _ in 0..drawn {
                    let Some(slot) = self.pop(waste) else {
                        break;
                    };
                    self.push(stock, slot.turned(false));
                }
            }
            Variant::Spider(_) => {
                let dealt = self.shape.tableau.iter().take(usize::from(drawn));
                for column in dealt.rev() {
                    if let Some(slot) = self.pop(column) {
                        self.push(stock, slot.turned(false));
                    }
                }
            }
            Variant::FreeCell => {}
        }
    }

    fn turn_down(&mut self, pile: PileId) {
        if let Some(top) = self.pile_mut(pile).and_then(|cards| cards.last_mut()) {
            *top = top.turned(false);
        }
    }

    // --- Every move there is --------------------------------------------

    /// Every action the rules allow now, into `out`, which is cleared first.
    pub fn actions(&self, out: &mut Vec<Action>) {
        out.clear();
        for from in self.shape.all() {
            let run = self.movable(from);
            if run == 0 {
                continue;
            }
            for to in self.shape.all() {
                if to == from {
                    continue;
                }
                self.moves_onto(from, to, run, out);
            }
        }
        if self.may_draw() {
            out.push(Action::Draw);
        }
        if self.may_recycle() {
            out.push(Action::Recycle);
        }
    }

    /// The moves of up to `run` cards from `from` that `to` takes.
    fn moves_onto(&self, from: PileId, to: PileId, run: usize, out: &mut Vec<Action>) {
        let fits = |count: usize| {
            u8::try_from(count).ok().filter(|_| {
                count <= self.capacity(to)
                    && self
                        .base(from, count)
                        .is_some_and(|base| self.accepts(to, base, count))
            })
        };
        match (self.role(to), self.top_face(to)) {
            // A non-empty column takes exactly the one unit whose base fits
            // its top, so only that length is tried.
            (Some(Role::Tableau), Some(top)) => {
                let wanted = top.rank.get().checked_sub(1);
                let length = (1..=run).find(|&count| {
                    self.base(from, count)
                        .is_some_and(|base| Some(base.rank.get()) == wanted)
                });
                if let Some(count) = length.and_then(fits) {
                    out.push(Action::Move { from, to, count });
                }
            }
            (Some(Role::Tableau), None) => {
                for count in (1..=run).filter_map(fits) {
                    out.push(Action::Move { from, to, count });
                }
            }
            (Some(Role::Foundation | Role::Cell), _) => {
                if let Some(count) = fits(1) {
                    out.push(Action::Move { from, to, count });
                }
            }
            _ => {}
        }
    }

    // --- Moves the player need not make ---------------------------------

    /// A move home no later play could want undone, if one is open: an ace or
    /// two, or a card both of whose opposite-coloured predecessors are already
    /// home, so nothing could ever need to be laid on it.
    ///
    /// `waste` says whether the waste's top counts. Drawing three, sending a
    /// waste card home changes how the waste's later threes fall, so a search
    /// that must not lose a line cannot treat that move as one it may make
    /// unasked.
    #[must_use]
    pub fn safe_home(&self, waste: bool) -> Option<Action> {
        if matches!(self.rules.variant, Variant::Spider(_)) {
            return None;
        }
        let sources = self
            .shape
            .waste
            .filter(|_| waste)
            .into_iter()
            .chain(self.shape.cells.iter())
            .chain(self.shape.tableau.iter());
        for from in sources {
            let Some(face) = self.top_face(from) else {
                continue;
            };
            if !self.safe(face) {
                continue;
            }
            if let Some(to) = self.foundation_for(face) {
                return Some(Action::Move { from, to, count: 1 });
            }
        }
        None
    }

    fn safe(&self, face: Face) -> bool {
        let rank = face.rank.get();
        if rank <= 2 {
            return true;
        }
        let height = |suit| {
            self.shape
                .foundations
                .iter()
                .filter_map(|pile| self.top_face(pile))
                .find(|top| top.suit == suit)
                .map_or(0, |top| top.rank.get())
        };
        Suit::ALL
            .into_iter()
            .filter(|suit| suit.colour() != face.colour())
            .all(|suit| height(suit) + 1 >= rank)
    }

    /// The foundation `face` builds on now: its suit's, or for an ace the
    /// first empty one.
    #[must_use]
    pub fn foundation_for(&self, face: Face) -> Option<PileId> {
        self.shape
            .foundations
            .iter()
            .find(|&pile| face.builds_on(self.top_face(pile)))
    }

    /// Whether the game is won however the remaining cards are played: nothing
    /// in the stock or waste, nothing face down, and every column descending,
    /// so its lowest card is always uncovered and can always go home.
    #[must_use]
    pub fn settled(&self) -> bool {
        if matches!(self.rules.variant, Variant::Spider(_)) || self.won() {
            return false;
        }
        let drawn_out = [self.shape.stock, self.shape.waste]
            .into_iter()
            .flatten()
            .all(|pile| self.pile(pile).is_empty());
        drawn_out
            && self.shape.tableau.iter().all(|column| {
                let cards = self.pile(column);
                cards.iter().all(|slot| slot.up())
                    && cards.windows(2).all(|pair| {
                        self.face(pair[1].card()).rank <= self.face(pair[0].card()).rank
                    })
            })
    }

    // --- Identity ------------------------------------------------------

    /// A fingerprint of the position that two tables share exactly when they
    /// play the same from here on: columns, cells and foundations are
    /// interchangeable within their role, so their order does not count.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        self.hashed(false)
    }

    /// [`fingerprint`](Self::fingerprint), but blind to where the stock's
    /// cycle stands when drawing one with no limit on the waste: there every
    /// card of the stock and waste can be reached whatever the next one is,
    /// so two such positions can be won exactly alike. For the solver's set of
    /// positions searched, never for telling the player's position apart.
    #[must_use]
    pub fn canonical(&self) -> u64 {
        self.hashed(self.cycles_freely())
    }

    /// Whether every stock and waste card can be reached whatever the next
    /// draw is: drawing one, with no limit on turning the waste back.
    #[must_use]
    pub fn cycles_freely(&self) -> bool {
        self.rules.variant == Variant::Klondike(Draw::One)
            && self.rules.recycles == Recycles::Unlimited
    }

    fn hashed(&self, cycle: bool) -> u64 {
        let mut total = if cycle {
            0x6379_636c_6573
        } else {
            mix64(u64::from(self.recycled) ^ 0x7265_6379_636c_6564)
        };
        if cycle {
            // The cycle's order: the waste as it was drawn, then the stock
            // in the order it will be.
            let (Some(stock), Some(waste)) = (self.shape.stock, self.shape.waste) else {
                return total;
            };
            let mut hash = 0x1;
            for slot in self.pile(waste).iter().chain(self.pile(stock).iter().rev()) {
                hash = mix64(hash ^ (u64::try_from(slot.card().index()).unwrap_or(0) << 16));
            }
            total = total.wrapping_add(mix64(hash ^ (0x1 << 40)));
        }
        for pile in self.shape.all() {
            let role = self.role(pile);
            if cycle && matches!(role, Some(Role::Stock | Role::Waste)) {
                continue;
            }
            let interchangeable =
                matches!(role, Some(Role::Tableau | Role::Cell | Role::Foundation));
            let salt = match role {
                Some(Role::Stock) => 0x1,
                Some(Role::Waste) => 0x2,
                Some(Role::Foundation) => 0x3,
                Some(Role::Cell) => 0x4,
                Some(Role::Tableau) => 0x5,
                None => 0x6,
            };
            let mut hash = salt
                | if interchangeable {
                    0
                } else {
                    u64::try_from(pile.index()).unwrap_or(0) << 8
                };
            for slot in self.pile(pile) {
                hash = mix64(hash ^ (u64::from(slot.byte()) << 16));
            }
            total = total.wrapping_add(mix64(hash ^ (salt << 40)));
        }
        total
    }

    /// Cards still face down.
    #[must_use]
    pub fn face_down(&self) -> usize {
        self.piles
            .iter()
            .flatten()
            .filter(|slot| !slot.up())
            .count()
    }
}

#[cfg(test)]
impl Table {
    /// A position laid out pile by pile, for another module's tests to play
    /// or search.
    pub(crate) fn laid(rules: Rules, piles: Vec<Vec<Slot>>) -> Self {
        let variant = rules.variant;
        Self {
            rules,
            shape: variant.shape(),
            deck: variant.deck(),
            piles,
            recycled: 0,
        }
    }
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod tests;
