//! The composed game: the model the `Run` binary drives.
//!
//! It owns the game in play, its layout and motion, the clock, and the
//! player's preferences and statistics, and turns one delivered input into
//! moves, movement and a reported damage rectangle. Nothing here performs
//! I/O. The solver's searches, the deal search and the store's writes are
//! asked for through the [`Outbox`] and answered back, because this runs on
//! the loop that owes the window a frame.
//!
//! # What a move reports
//!
//! A move repaints the cards it moved, where they were and where they go, and
//! the status line; the motion model adds each card's path frame by frame.
//! Only a new deal, a new layout or a new look repaints the whole window.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_abi::driver::display::RefreshRate;
use tairix_appdata::Refusal;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_raster::Surface;
use tairix_rng::RandU64;
use tairix_theme::Theme;

use crate::analysis::{DealFound, DealSearch, Finding, Question, HINT_BUDGET, MOVE_BUDGET};
use crate::art::{CardArt, Palette};
use crate::card::CardId;
use crate::deal::DealNumber;
use crate::layout::{Layout, Placement};
use crate::menu::{self, Offer, Offers};
use crate::motion::{Cascade, Glide, Motion, Timing};
use crate::paint::{self, Cursor, Status, View};
use crate::play::Play;
use crate::prefs::Preferences;
use crate::saved::Saved;
use crate::score::Score;
use crate::solver::Verdict;
use crate::stats::{Outcome, Stats, Tally};
use crate::store::{Answer, Job, Ledger};
use crate::table::{Action, Table};
use crate::variant::{PileId, Role, Rules, Variant};

/// A second, in nanoseconds.
const SECOND_NS: u64 = 1_000_000_000;
/// How long after a move the game in play is kept, so a burst of moves costs
/// one write.
const AUTOSAVE_NS: u64 = 4 * SECOND_NS;
/// How long a passing note stays on the status line.
const NOTE_NS: u64 = 5 * SECOND_NS;
/// How long each move the game makes itself waits behind the one before.
const STEP_NS: u64 = 85_000_000;
/// How long each card of a deal waits behind the one before.
const DEAL_STAGGER_NS: u64 = 28_000_000;
/// How long each card of a run sent home waits behind the one above it.
const RUN_STAGGER_NS: u64 = 26_000_000;
/// How long a won table stands before the victory throws its first card.
const VICTORY_PAUSE_NS: u64 = 400_000_000;
/// A card the last change did not move.
const UNMOVED: u16 = u16::MAX;
/// Places a step's cards may stagger across: a Spider run's thirteen and a
/// deal's ten fit.
const WITHIN: u16 = 32;

/// What the game starts from.
#[derive(Debug)]
pub struct Setup {
    /// The player's preferences, as the store held them.
    pub prefs: Preferences,
    /// The statistics, as the store held them.
    pub stats: Stats,
    /// The game kept from last time, and how long it had been played.
    pub resume: Option<(Play, u32)>,
    /// The window's client area.
    pub client: Rect,
    /// The desktop's density.
    pub scale: Scale,
    /// Whether the theme asks for motion to be suppressed.
    pub reduced: bool,
    /// The rate the desktop composes at.
    pub refresh: RefreshRate,
}

/// Which alert an answer belongs to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Alerting {
    /// The game won.
    Won,
    /// The statistics shown.
    Statistics,
    /// Whether to forget a record.
    Forget,
}

/// What the game asks the program around it to do.
#[derive(Debug, Default)]
pub struct Outbox {
    /// A question for the analysis worker; a newer one replaces an older.
    pub question: Option<Question>,
    /// A winnable deal to find.
    pub deal: Option<DealSearch>,
    /// An alert to put up.
    pub alert: Option<(Alerting, tairix_abi::window_ipc::AlertSpec)>,
    /// The table's menu to open at this point.
    pub context: Option<Point>,
    /// Whether the icon bar's menu no longer says what is true.
    pub menu_moved: bool,
}

/// A passing word on the status line.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Note {
    Shuffling,
    Unproven,
    Lost,
    NoMove,
    Thinking,
    NoRewind,
    TooLong,
    DealBlocked,
    NoRedeal,
}

impl Note {
    const fn text(self) -> &'static str {
        match self {
            Self::Shuffling => "Finding a winnable deal\u{2026}",
            Self::Unproven => "No winnable deal was found in time: this one is unchecked",
            Self::Lost => "No winning line remains \u{b7} Ctrl+B goes back to the last",
            Self::NoMove => "No moves left \u{b7} undo, or deal again",
            Self::Thinking => "Looking for a move\u{2026}",
            Self::NoRewind => "No winnable position could be found to go back to",
            Self::TooLong => "This game is too long to keep for next time",
            Self::DealBlocked => "Every column needs a card before the next deal",
            Self::NoRedeal => "The stock has been turned as often as the scoring allows",
        }
    }
}

/// The clock: it runs from the first move while the window is shown, and
/// stops when the game is won.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct Clock {
    banked_ns: u64,
    since: Option<u64>,
}

impl Clock {
    fn secs(self, now_ns: u64) -> u32 {
        let running = self.since.map_or(0, |since| now_ns.saturating_sub(since));
        let secs = self.banked_ns.saturating_add(running) / SECOND_NS;
        u32::try_from(secs)
            .unwrap_or(u32::MAX)
            .min(crate::stats::MOST_SECS)
    }

    fn run(&mut self, now_ns: u64) {
        if self.since.is_none() {
            self.since = Some(now_ns);
        }
    }

    fn stop(&mut self, now_ns: u64) {
        if let Some(since) = self.since.take() {
            self.banked_ns = self.banked_ns.saturating_add(now_ns.saturating_sub(since));
        }
    }
}

/// A press that may become a drag or a click.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Press {
    pub(crate) pile: PileId,
    pub(crate) depth: usize,
    pub(crate) at: Point,
}

/// Cards under the pointer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Drag {
    pub(crate) pile: PileId,
    pub(crate) depth: usize,
    /// From the pointer to the bottom card's corner.
    pub(crate) grab: Point,
    /// Each card and where it is drawn.
    pub(crate) held: Vec<(CardId, Point)>,
    /// Where it would land now.
    pub(crate) target: Option<PileId>,
    /// How a hand the keyboard holds is carried from place to place; a hand
    /// the pointer holds goes where the pointer does.
    pub(crate) glide: Option<Glide>,
}

/// A winning line known from the position in play.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Known {
    fingerprint: u64,
    line: Vec<Action>,
}

/// A won game's numbers, for the alert.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Won {
    secs: u32,
    moves: u32,
    score: Option<Score>,
}

/// How the cards a change moved set off.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Pace {
    /// Together, a short slide.
    Slide,
    /// Each step the game took one after another, as `steps` says.
    Steps,
    /// One by one in the order dealt.
    Deal,
    /// Back where they came from.
    Return,
}

/// What the status line's words were last set from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Said {
    secs: u32,
    moves: usize,
    score: Option<Score>,
    note: Option<Note>,
    number: Option<u32>,
    proven: bool,
}

/// The status line's words, kept until what they say changes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct StatusText {
    said: Option<Said>,
    game: String,
    message: String,
    readings: String,
}

/// A game, its window, and everything in flight over it.
#[derive(Debug)]
pub struct Game {
    pub(crate) ledger: Ledger,
    pub(crate) play: Option<Play>,
    waiting: Option<Rules>,
    prefetched: Option<DealFound>,
    proven: bool,
    clock: Clock,
    won: Option<Won>,
    pub(crate) layout: Layout,
    pub(crate) scale: Scale,
    pub(crate) placements: Vec<Placement>,
    pub(crate) motion: Motion,
    art: Option<CardArt>,
    pub(crate) pointer: Point,
    pub(crate) press: Option<Press>,
    pub(crate) drag: Option<Drag>,
    pub(crate) cursor: Option<Cursor>,
    known: Option<Known>,
    lost: Option<u64>,
    hint_wanted: bool,
    pub(crate) note: Option<(Note, Option<u64>)>,
    outbox: Outbox,
    autosave_ns: Option<u64>,
    visible: bool,
    shown_secs: u32,
    status: StatusText,
    /// The step each card of the last change first moved at, by card.
    steps: Vec<u16>,
}

impl Game {
    /// A game from `setup`: the kept game resumed, or a new deal.
    #[must_use]
    pub fn new(setup: Setup, now_ns: u64, rng: &mut dyn RandU64) -> Self {
        let variant = setup
            .resume
            .as_ref()
            .map_or(setup.prefs.variant, |(play, _)| play.variant());
        let layout = Layout::resolve(setup.client, variant, setup.scale);
        let size = layout.card_size();
        let mut game = Self {
            ledger: Ledger::new(setup.prefs, setup.stats),
            play: None,
            waiting: None,
            prefetched: None,
            proven: false,
            clock: Clock::default(),
            won: None,
            layout,
            scale: setup.scale,
            placements: Vec::new(),
            motion: Motion::new(variant.deck().count(), size, setup.reduced, setup.refresh),
            art: None,
            pointer: Point::ORIGIN,
            press: None,
            drag: None,
            cursor: None,
            known: None,
            lost: None,
            hint_wanted: false,
            note: None,
            outbox: Outbox::default(),
            autosave_ns: None,
            visible: true,
            shown_secs: 0,
            status: StatusText::default(),
            steps: Vec::new(),
        };
        let mut sink = Region::new();
        if let Some((play, secs)) = setup.resume {
            game.clock.banked_ns = u64::from(secs) * SECOND_NS;
            if play.started() && !play.table().won() {
                game.clock.run(now_ns);
            }
            game.adopt(play, now_ns, &mut sink);
            game.snap();
        } else {
            game.new_game(game.ledger.prefs().variant, None, now_ns, rng, &mut sink);
        }
        game
    }

    // --- Reading it -----------------------------------------------------

    /// The table in play, if a deal is.
    #[must_use]
    pub fn table(&self) -> Option<&Table> {
        self.play.as_ref().map(Play::table)
    }

    /// The game in play.
    #[must_use]
    pub const fn play(&self) -> Option<&Play> {
        self.play.as_ref()
    }

    /// The preferences in force.
    #[must_use]
    pub fn prefs(&self) -> &Preferences {
        self.ledger.prefs()
    }

    /// The statistics, every game finished this session included.
    #[must_use]
    pub fn stats(&self) -> Stats {
        self.ledger.stats()
    }

    /// The clock's reading.
    #[must_use]
    pub fn secs(&self, now_ns: u64) -> u32 {
        self.clock.secs(now_ns)
    }

    /// What the program around the game is asked to do.
    pub fn outbox(&mut self) -> &mut Outbox {
        &mut self.outbox
    }

    /// The write owed to the store now, if one is and none is in flight.
    pub fn store_job(&mut self) -> Option<Job> {
        self.ledger.job()
    }

    /// The write owed as the program ends, tried once more however the
    /// last went: the game in play kept first.
    pub fn final_job(&mut self, now_ns: u64) -> Option<Job> {
        self.keep(now_ns);
        self.ledger.final_job()
    }

    /// Adopt the store's answer to the write in flight, answering the next.
    pub fn store_landed(&mut self, answer: Answer, refusals: &mut Vec<Refusal>) -> Option<Job> {
        let before = *self.ledger.prefs();
        let next = self.ledger.landed(answer, refusals);
        if *self.ledger.prefs() != before {
            self.outbox.menu_moved = true;
        }
        next
    }

    /// What the menus offer now.
    #[must_use]
    pub fn offers(&self) -> Offers {
        let play = self.play.as_ref();
        Offers::default()
            .with(Offer::Undo, play.is_some_and(Play::can_undo))
            .with(Offer::Redo, play.is_some_and(Play::can_redo))
            .with(
                Offer::Rewind,
                self.lost.is_some() && self.lost == self.fingerprint(),
            )
            .with(
                Offer::Finish,
                play.is_some_and(|play| play.table().settled()),
            )
    }

    /// The icon bar's menu.
    #[must_use]
    pub fn app_menu(&self) -> tairix_abi::window_ipc::AppMenu {
        menu::menu(self.prefs(), self.offers(), None)
    }

    /// The table's own menu.
    #[must_use]
    pub fn context_menu(&self) -> tairix_abi::window_ipc::AppMenu {
        let number = self.play.as_ref().map_or(1, |play| play.number().get());
        menu::menu(self.prefs(), self.offers(), Some(number))
    }

    fn fingerprint(&self) -> Option<u64> {
        self.table().map(Table::fingerprint)
    }

    // --- Time -----------------------------------------------------------

    /// When the next wake is due, or `None` when the game owes none: the
    /// next frame of anything moving, the clock's next second, a note's end,
    /// or the kept game's write.
    #[must_use]
    pub fn deadline_ns(&self, now_ns: u64) -> Option<u64> {
        let clock = self.clock.since.map(|since| {
            let total = self
                .clock
                .banked_ns
                .saturating_add(now_ns.saturating_sub(since));
            now_ns.saturating_add(SECOND_NS - total % SECOND_NS)
        });
        let note = self.note.and_then(|(_, until)| until);
        let hand = self
            .drag
            .as_ref()
            .and_then(|drag| drag.glide)
            .filter(|glide| glide.moving(now_ns))
            .map(|_| now_ns.saturating_add(self.motion.frame_ns()));
        [
            self.motion.deadline_ns(now_ns),
            clock,
            note,
            self.autosave_ns,
            hand,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Run everything on to `now_ns`, reporting what must be repainted.
    pub fn tick(&mut self, now_ns: u64, damage: &mut Region) -> bool {
        let mut changed = false;
        let cascading = self.motion.cascade().is_some();
        self.motion.advance(now_ns, damage);
        self.carry(now_ns, damage);
        if !damage.is_empty() {
            changed = true;
        }
        if cascading && self.motion.cascade().is_some_and(Cascade::done) {
            self.end_cascade(damage);
            changed = true;
        }
        let secs = self.clock.secs(now_ns);
        if secs != self.shown_secs {
            self.shown_secs = secs;
            damage.add(self.layout.status);
            changed = true;
        }
        if let Some((_, Some(until))) = self.note {
            if now_ns >= until {
                self.note = None;
                damage.add(self.layout.status);
                changed = true;
            }
        }
        if self.autosave_ns.is_some_and(|due| now_ns >= due) {
            self.autosave_ns = None;
            self.keep(now_ns);
        }
        changed
    }

    /// Draw a hand the keyboard holds where its glide has reached.
    pub(crate) fn carry(&mut self, now_ns: u64, damage: &mut Region) {
        let size = self.layout.card_size();
        let Some(drag) = &mut self.drag else {
            return;
        };
        let (Some(glide), Some(&(_, base))) = (drag.glide, drag.held.first()) else {
            return;
        };
        let to = glide.at(now_ns);
        if to != base {
            shift(&mut drag.held, to, size, damage);
        }
    }

    /// The window was hidden or shown again: the clock stops while nobody
    /// can see the game, and the game is kept as it goes out of view.
    pub fn set_visible(&mut self, visible: bool, now_ns: u64) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            if self
                .play
                .as_ref()
                .is_some_and(|play| play.started() && !play.table().won())
            {
                self.clock.run(now_ns);
            }
        } else {
            self.clock.stop(now_ns);
            self.keep(now_ns);
        }
    }

    // --- The window -----------------------------------------------------

    /// Give back the card tiles and the baize, as memory pressure asks: each
    /// is drawn again the next time it is shown.
    pub fn trim(&mut self) {
        if let Some(art) = &mut self.art {
            art.trim();
        }
    }

    /// Lay the game out in a new client area or at a new density.
    pub fn relayout(&mut self, client: Rect, scale: Scale, damage: &mut Region) {
        if self.layout.client == client && self.scale == scale {
            return;
        }
        self.scale = scale;
        self.layout = Layout::resolve(client, self.layout.variant(), scale);
        self.end_drag();
        self.end_cascade(damage);
        self.snap();
        damage.add(client);
    }

    /// Pace every frame by a desktop now refreshing at `refresh`.
    pub fn set_refresh(&mut self, refresh: RefreshRate) {
        self.motion.set_refresh(refresh);
    }

    /// Adopt the theme's reduced-motion policy.
    pub fn set_reduced_motion(&mut self, reduced: bool, damage: &mut Region) {
        if self.motion.reduced_motion() != reduced {
            if reduced {
                self.end_cascade(damage);
            }
            self.motion.set_reduced_motion(reduced);
            damage.add(self.layout.client);
        }
    }

    /// Draw the frame `damage` covers.
    pub fn render(
        &mut self,
        surface: &mut Surface,
        theme: &Theme,
        font: tairix_font::BitmapFont,
        now_ns: u64,
    ) {
        self.refresh_status(now_ns);
        let palette = Palette::resolve(theme, self.prefs());
        let layout = &self.layout;
        let art = self.art.get_or_insert_with(|| {
            CardArt::new(
                layout.card_width,
                layout.card_height,
                layout.radius,
                layout.hairline,
                palette,
            )
        });
        art.retune(
            layout.card_width,
            layout.card_height,
            layout.radius,
            layout.hairline,
            palette,
        );
        let status = Status {
            game: &self.status.game,
            message: &self.status.message,
            readings: &self.status.readings,
        };
        let Some(play) = &self.play else {
            paint::waiting(surface, &self.layout, status, font, art);
            return;
        };
        let held = self
            .drag
            .as_ref()
            .map_or(&[][..], |drag| drag.held.as_slice());
        let by_keys = self.drag.as_ref().is_some_and(|drag| drag.glide.is_some());
        let view = View {
            layout: &self.layout,
            table: play.table(),
            motion: &self.motion,
            held,
            cursor: self.cursor.filter(|_| !by_keys),
            target: self.drag.as_ref().and_then(|drag| drag.target),
            status,
            font,
            now_ns,
        };
        paint::frame(surface, &view, art);
    }

    /// Bring the status line's words up to what is true at `now_ns`.
    fn refresh_status(&mut self, now_ns: u64) {
        let secs = self.clock.secs(now_ns);
        let play = self.play.as_ref();
        let moves = play.map_or(0, Play::moves);
        let bank = self.ledger.stats().bank();
        let score = play.and_then(|play| play.score(u64::from(secs), bank));
        let note = self.note.map(|(note, _)| note);
        let number = play.map(|play| play.number().get());
        let said = Some(Said {
            secs,
            moves,
            score,
            note,
            number,
            proven: self.proven,
        });
        if self.status.said == said {
            return;
        }
        self.status.said = said;
        let variant = play.map_or(self.layout.variant(), Play::variant);
        self.status.game.clear();
        let _ = write!(self.status.game, "{}", variant.label());
        if let Some(number) = number {
            let _ = write!(self.status.game, " \u{b7} deal {number}");
            if self.proven {
                self.status.game.push_str(" \u{2713}");
            }
        }
        self.status.message.clear();
        if let Some(note) = note {
            self.status.message.push_str(note.text());
        }
        self.status.readings.clear();
        if let Some(score) = score {
            self.status.readings.push_str(&menu::score_text(score));
            self.status.readings.push_str("   ");
        }
        let _ = write!(
            self.status.readings,
            "{moves} moves   {}",
            menu::clock(secs)
        );
    }

    pub(crate) fn say(&mut self, note: Note, now_ns: u64, lasting: bool, damage: &mut Region) {
        let until = (!lasting).then(|| now_ns.saturating_add(NOTE_NS));
        self.note = Some((note, until));
        damage.add(self.layout.status);
    }

    fn quiet(&mut self, note: Note, damage: &mut Region) {
        if self.note.is_some_and(|(said, _)| said == note) {
            self.note = None;
            damage.add(self.layout.status);
        }
    }

    // --- Deals ----------------------------------------------------------

    /// Deal a new game of `variant`: deal `number` if one is named, else a
    /// winnable one when the player asks for those, else any.
    pub fn new_game(
        &mut self,
        variant: Variant,
        number: Option<DealNumber>,
        now_ns: u64,
        rng: &mut dyn RandU64,
        damage: &mut Region,
    ) {
        self.abandon(now_ns);
        let rules = self.prefs().scoring.rules(variant);
        if let Some(number) = number {
            self.deal(rules, number, false, now_ns, damage);
            return;
        }
        if !self.prefs().winnable {
            self.deal(
                rules,
                DealNumber::random(variant, rng),
                false,
                now_ns,
                damage,
            );
            return;
        }
        if let Some(found) = self.prefetched.take().filter(|found| found.rules == rules) {
            self.deal(rules, found.number, found.proven, now_ns, damage);
            if !found.proven {
                self.say(Note::Unproven, now_ns, false, damage);
            }
            self.prefetch(rng);
            return;
        }
        // Nothing found ahead of time: the table waits, empty, for one.
        self.play = None;
        self.waiting = Some(rules);
        self.relayout_for(variant, damage);
        self.outbox.deal = Some(DealSearch {
            rules,
            first: DealNumber::random(variant, rng),
            waited: true,
        });
        self.say(Note::Shuffling, now_ns, true, damage);
        damage.add(self.layout.client);
    }

    /// Ask for the next winnable deal ahead of time, so the next new game
    /// deals at once.
    fn prefetch(&mut self, rng: &mut dyn RandU64) {
        if !self.prefs().winnable {
            return;
        }
        let variant = self.prefs().variant;
        self.outbox.deal = Some(DealSearch {
            rules: self.prefs().scoring.rules(variant),
            first: DealNumber::random(variant, rng),
            waited: false,
        });
    }

    /// The dealing worker settled on `found`.
    pub fn dealt(
        &mut self,
        found: DealFound,
        now_ns: u64,
        rng: &mut dyn RandU64,
        damage: &mut Region,
    ) {
        if self.waiting == Some(found.rules) {
            self.waiting = None;
            self.quiet(Note::Shuffling, damage);
            self.deal(found.rules, found.number, found.proven, now_ns, damage);
            if !found.proven {
                self.say(Note::Unproven, now_ns, false, damage);
            }
            self.prefetch(rng);
        } else {
            self.prefetched = Some(found);
        }
    }

    fn deal(
        &mut self,
        rules: Rules,
        number: DealNumber,
        proven: bool,
        now_ns: u64,
        damage: &mut Region,
    ) {
        let scoring = self.prefs().scoring;
        let (play, order) = Play::deal(rules, scoring, number);
        self.proven = proven;
        self.clock = Clock::default();
        self.won = None;
        self.adopt(play, now_ns, damage);
        // Every card dealt sets off from the stock, or the top of the table
        // where there is none, face down.
        let Some(table) = self.table() else {
            return;
        };
        let shape = table.shape();
        let deck = shape
            .stock
            .or_else(|| shape.cells.nth(0))
            .map_or(self.layout.felt.center(), |pile| self.layout.origin(pile));
        let mut from = self.placements.clone();
        for &card in &order {
            if let Some(placement) = from.get_mut(card.index()) {
                *placement = Placement {
                    at: deck,
                    up: false,
                };
            }
        }
        self.motion.snap(&from, self.layout.card_size());
        self.steps.clear();
        self.steps.resize(from.len(), UNMOVED);
        for (index, &card) in order.iter().enumerate() {
            if let Some(slot) = self.steps.get_mut(card.index()) {
                *slot = u16::try_from(index).unwrap_or(UNMOVED);
            }
        }
        self.animate(now_ns, Pace::Deal, &[], damage);
        damage.add(self.layout.client);
        self.ask(now_ns);
    }

    /// Make `play` the game in play, laid out for its variant.
    fn adopt(&mut self, play: Play, now_ns: u64, damage: &mut Region) {
        let variant = play.variant();
        self.relayout_for(variant, damage);
        self.play = Some(play);
        self.known = None;
        self.lost = None;
        self.end_drag();
        self.cursor = None;
        self.motion.end_cascade();
        self.place_all();
        self.outbox.menu_moved = true;
        self.ask(now_ns);
    }

    fn relayout_for(&mut self, variant: Variant, damage: &mut Region) {
        if self.layout.variant() != variant {
            self.layout = Layout::resolve(self.layout.client, variant, self.scale);
            self.motion = Motion::new(
                variant.deck().count(),
                self.layout.card_size(),
                self.motion.reduced_motion(),
                self.motion.refresh(),
            );
            damage.add(self.layout.client);
        }
    }

    /// Count the game in play as lost if it was started and not won, as a
    /// player who deals again concedes it.
    fn abandon(&mut self, now_ns: u64) {
        let Some(play) = &self.play else {
            return;
        };
        if !play.started() || play.table().won() {
            return;
        }
        let secs = self.clock.secs(now_ns);
        let outcome = self.outcome(play, false, secs);
        self.ledger.tally(Tally::Add(outcome));
        self.ledger.keep(Saved::default());
        self.autosave_ns = None;
    }

    fn outcome(&self, play: &Play, won: bool, secs: u32) -> Outcome {
        let bank = self.ledger.stats().bank();
        Outcome {
            variant: play.variant(),
            won,
            secs,
            moves: u32::try_from(play.moves()).unwrap_or(u32::MAX),
            score: play.score(u64::from(secs), bank).map(Score::value),
            banked: (play.scoring() == crate::score::Scoring::Cumulative
                && matches!(play.variant(), Variant::Klondike(_)))
            .then(|| play.vegas()),
        }
    }

    /// Keep the game in play for next time, or forget the one kept if it
    /// has not begun or is over.
    fn keep(&mut self, now_ns: u64) {
        let Some(play) = &self.play else {
            return;
        };
        if !play.started() || play.table().won() {
            return;
        }
        if let Some(saved) = Saved::of(play, self.clock.secs(now_ns)) {
            self.ledger.keep(saved);
        } else {
            self.ledger.keep(Saved::default());
            self.note = Some((Note::TooLong, Some(now_ns.saturating_add(NOTE_NS))));
        }
    }

    // --- Moves ----------------------------------------------------------

    /// Make the player's `action`, everything that follows from it, and the
    /// movement it all makes, answering whether the rules allowed it.
    pub(crate) fn act(
        &mut self,
        action: Action,
        pace: Pace,
        held: &[(CardId, Point)],
        now_ns: u64,
        damage: &mut Region,
    ) -> bool {
        let Some(play) = &mut self.play else {
            return false;
        };
        let Some(applied) = play.act(action) else {
            return false;
        };
        self.steps.clear();
        self.steps.resize(self.placements.len(), UNMOVED);
        self.mark(&applied, 0);
        if self.visible {
            self.clock.run(now_ns);
        }
        let followed = self.follow_home();
        let pace = if followed > 0 && pace == Pace::Slide {
            Pace::Steps
        } else {
            pace
        };
        self.animate(now_ns, pace, held, damage);
        self.after_move(now_ns, damage);
        true
    }

    /// Make every move home the game makes for the player, answering how
    /// many: the safe ones while the player asks for them, and every card
    /// once nothing could stop it going home.
    fn follow_home(&mut self) -> u16 {
        let auto = self.prefs().auto_home;
        let mut made = 0_u16;
        while let Some(play) = &mut self.play {
            let table = play.table();
            let next = if auto {
                table.safe_home(true).or_else(|| finishing(table))
            } else {
                None
            };
            let Some(action) = next else {
                break;
            };
            let Some(applied) = play.follow(action) else {
                break;
            };
            made = made.saturating_add(1);
            self.mark(&applied, made);
        }
        made
    }

    /// Record the cards `applied`, just made, moved as setting off at `step`,
    /// each run or row of them one after another within it.
    fn mark(&mut self, applied: &crate::table::Applied, step: u16) {
        let Some(table) = self.table() else {
            return;
        };
        let shape = table.shape();
        let mut moved: Vec<(CardId, u16)> = Vec::new();
        let top = |pile: PileId, count: usize, moved: &mut Vec<(CardId, u16)>| {
            let cards = table.pile(pile);
            for (within, slot) in cards.iter().rev().take(count).rev().enumerate() {
                moved.push((slot.card(), u16::try_from(within).unwrap_or(0)));
            }
        };
        match applied.action {
            Action::Move { from, to, count } => {
                top(to, usize::from(count), &mut moved);
                if applied.turned {
                    top(from, 1, &mut moved);
                }
            }
            Action::Draw => match table.rules().variant {
                Variant::Spider(_) => {
                    for (within, column) in shape.tableau.iter().enumerate() {
                        if let Some(slot) = table.top(column) {
                            moved.push((slot.card(), u16::try_from(within).unwrap_or(0)));
                        }
                    }
                }
                Variant::Klondike(_) | Variant::FreeCell => {
                    if let Some(waste) = shape.waste {
                        top(waste, usize::from(applied.drawn), &mut moved);
                    }
                }
            },
            Action::Recycle => {
                if let Some(stock) = shape.stock {
                    moved.extend(table.pile(stock).iter().map(|slot| (slot.card(), 0)));
                }
            }
        }
        for completion in applied.completions() {
            // A completed run flies home ace first, as it is picked up.
            let run = table.pile(completion.foundation);
            for (within, slot) in run.iter().rev().enumerate() {
                moved.push((slot.card(), u16::try_from(within).unwrap_or(0)));
            }
            if completion.turned {
                top(completion.column, 1, &mut moved);
            }
        }
        for (card, within) in moved {
            if let Some(slot) = self.steps.get_mut(card.index()) {
                if *slot == UNMOVED {
                    *slot = step
                        .saturating_mul(WITHIN)
                        .saturating_add(within.min(WITHIN - 1));
                }
            }
        }
    }

    /// Lay every card out where the table now puts it, without movement.
    fn place_all(&mut self) {
        let Some(table) = self.play.as_ref().map(Play::table) else {
            return;
        };
        self.layout.placements(table, &mut self.placements);
    }

    /// Lay every card out and put each there at once.
    fn snap(&mut self) {
        self.place_all();
        self.motion.snap(&self.placements, self.layout.card_size());
    }

    /// Move every card to where the table now puts it, paced by `pace`.
    pub(crate) fn animate(
        &mut self,
        now_ns: u64,
        pace: Pace,
        held: &[(CardId, Point)],
        damage: &mut Region,
    ) {
        self.place_all();
        let steps = &self.steps;
        let timing = |card: CardId| -> Timing {
            let step = steps.get(card.index()).copied().unwrap_or(UNMOVED);
            match pace {
                Pace::Slide => Timing::SLIDE,
                Pace::Return => Timing {
                    delay_ns: 0,
                    span_ns: 240_000_000,
                    arc: 0,
                },
                Pace::Deal => Timing {
                    delay_ns: if step == UNMOVED {
                        0
                    } else {
                        u64::from(step) * DEAL_STAGGER_NS
                    },
                    span_ns: 300_000_000,
                    arc: 90,
                },
                Pace::Steps => Timing {
                    delay_ns: if step == UNMOVED {
                        0
                    } else {
                        u64::from(step / WITHIN) * STEP_NS
                            + u64::from(step % WITHIN) * RUN_STAGGER_NS
                    },
                    span_ns: 220_000_000,
                    arc: 120,
                },
            }
        };
        self.motion
            .retarget(now_ns, &self.placements, held, timing, damage);
        damage.add(self.layout.status);
    }

    /// What a move leaves owed: a fresh look at the position, a kept game,
    /// and the end of a won one.
    fn after_move(&mut self, now_ns: u64, damage: &mut Region) {
        self.outbox.menu_moved = true;
        self.hint_wanted = false;
        self.quiet(Note::NoMove, damage);
        self.quiet(Note::Lost, damage);
        let Some(table) = self.table() else {
            return;
        };
        if table.won() {
            self.win(now_ns, damage);
            return;
        }
        let mut actions = Vec::new();
        table.actions(&mut actions);
        self.autosave_ns = Some(now_ns.saturating_add(AUTOSAVE_NS));
        if actions.is_empty() {
            self.say(Note::NoMove, now_ns, true, damage);
        }
        self.ask(now_ns);
    }

    /// Ask the solver about the position in play, unless a winning line from
    /// it is already known.
    fn ask(&mut self, _now_ns: u64) {
        let Some(table) = self.table() else {
            return;
        };
        let fingerprint = table.fingerprint();
        if self
            .known
            .as_ref()
            .is_some_and(|known| known.fingerprint == fingerprint)
        {
            return;
        }
        if table.won() {
            return;
        }
        self.outbox.question = Some(Question::Position {
            table: table.clone(),
            budget: if self.hint_wanted {
                HINT_BUDGET
            } else {
                MOVE_BUDGET
            },
        });
    }

    /// Take the player's last move back.
    pub fn undo(&mut self, now_ns: u64, damage: &mut Region) -> bool {
        let Some(play) = &mut self.play else {
            return false;
        };
        if play.table().won() || play.undo().is_empty() {
            return false;
        }
        self.end_drag();
        self.animate(now_ns, Pace::Slide, &[], damage);
        self.known = None;
        self.after_move(now_ns, damage);
        true
    }

    /// Make the last move taken back again.
    pub fn redo(&mut self, now_ns: u64, damage: &mut Region) -> bool {
        let Some(play) = &mut self.play else {
            return false;
        };
        if play.redo().is_empty() {
            return false;
        }
        self.end_drag();
        self.animate(now_ns, Pace::Slide, &[], damage);
        self.known = None;
        self.after_move(now_ns, damage);
        true
    }

    /// Send every card home, the game being settled.
    pub fn finish(&mut self, now_ns: u64, damage: &mut Region) -> bool {
        let Some(play) = &mut self.play else {
            return false;
        };
        if !play.table().settled() {
            return false;
        }
        self.steps.clear();
        self.steps.resize(self.placements.len(), UNMOVED);
        let mut made = 0_u16;
        while let Some(play) = &mut self.play {
            let Some(action) = finishing(play.table()) else {
                break;
            };
            let done = if made == 0 {
                play.act(action)
            } else {
                play.follow(action)
            };
            let Some(applied) = done else {
                break;
            };
            self.mark(&applied, made);
            made = made.saturating_add(1);
        }
        self.animate(now_ns, Pace::Steps, &[], damage);
        self.after_move(now_ns, damage);
        made > 0
    }

    fn win(&mut self, now_ns: u64, damage: &mut Region) {
        let Some(play) = &self.play else {
            return;
        };
        self.clock.stop(now_ns);
        let secs = self.clock.secs(now_ns);
        let outcome = self.outcome(play, true, secs);
        self.won = Some(Won {
            secs,
            moves: outcome.moves,
            score: play.score(u64::from(secs), self.ledger.stats().bank()),
        });
        self.ledger.tally(Tally::Add(outcome));
        self.ledger.keep(Saved::default());
        self.autosave_ns = None;
        self.known = None;
        let table = play.table();
        let shape = table.shape();
        let mut home: Vec<(CardId, Point)> = Vec::new();
        let tallest = shape
            .foundations
            .iter()
            .map(|pile| table.pile(pile).len())
            .max()
            .unwrap_or(0);
        for layer in 0..tallest {
            for pile in shape.foundations.iter() {
                let cards = table.pile(pile);
                if let Some(slot) = cards
                    .len()
                    .checked_sub(layer + 1)
                    .and_then(|at| cards.get(at))
                {
                    let at = self
                        .placements
                        .get(slot.card().index())
                        .map_or(Point::ORIGIN, |p| p.at);
                    home.push((slot.card(), at));
                }
            }
        }
        home.reverse();
        let felt = self.layout.felt;
        let cascade = Cascade::new(
            home,
            (felt.left(), felt.right(), felt.bottom()),
            self.layout.card_size(),
            now_ns.saturating_add(VICTORY_PAUSE_NS),
            u64::from(play.number().get()),
        );
        self.motion.celebrate(cascade);
        if self.motion.cascade().is_none() {
            self.show_won();
        }
        damage.add(self.layout.status);
    }

    /// End the victory, if one is running, and say the game is won.
    pub(crate) fn end_cascade(&mut self, damage: &mut Region) {
        if self.motion.end_cascade() {
            damage.add(self.layout.client);
            self.show_won();
        }
    }

    fn show_won(&mut self) {
        let (Some(won), Some(play)) = (self.won, &self.play) else {
            return;
        };
        if let Some(spec) = menu::won(
            play.variant(),
            won.secs,
            won.moves,
            won.score,
            &self.ledger.stats(),
        ) {
            self.outbox.alert = Some((Alerting::Won, spec));
        }
    }

    // --- The solver's answers -------------------------------------------

    /// The analysis worker found `finding`.
    pub fn answered(&mut self, finding: Finding, now_ns: u64, damage: &mut Region) {
        let Some(current) = self.fingerprint() else {
            return;
        };
        match finding {
            Finding::Position {
                fingerprint,
                verdict,
            } if fingerprint == current => match verdict {
                Verdict::Won(line) => {
                    self.lost = None;
                    self.known = Some(Known { fingerprint, line });
                    self.quiet(Note::Thinking, damage);
                    if core::mem::take(&mut self.hint_wanted) {
                        self.show_hint(now_ns, damage);
                    }
                }
                Verdict::Lost => {
                    self.lost = Some(fingerprint);
                    self.known = None;
                    self.hint_wanted = false;
                    self.quiet(Note::Thinking, damage);
                    self.say(Note::Lost, now_ns, true, damage);
                    self.outbox.menu_moved = true;
                }
                Verdict::Unknown => {
                    self.quiet(Note::Thinking, damage);
                    if core::mem::take(&mut self.hint_wanted) {
                        self.show_fallback(now_ns, damage);
                    }
                }
            },
            Finding::Rewind { fingerprint, keep } if fingerprint == current => match keep {
                Some(keep) => self.rewind_to(keep, now_ns, damage),
                None => self.say(Note::NoRewind, now_ns, false, damage),
            },
            // An answer about a position the player has already left.
            Finding::Position { .. } | Finding::Rewind { .. } => {}
        }
    }

    /// Show a move that keeps the game winnable, or say why there is none.
    pub fn hint(&mut self, now_ns: u64, damage: &mut Region) {
        let Some(current) = self.fingerprint() else {
            return;
        };
        if self
            .known
            .as_ref()
            .is_some_and(|known| known.fingerprint == current)
        {
            self.show_hint(now_ns, damage);
            return;
        }
        if self.lost == Some(current) {
            self.say(Note::Lost, now_ns, true, damage);
            self.show_fallback(now_ns, damage);
            return;
        }
        self.hint_wanted = true;
        self.say(Note::Thinking, now_ns, true, damage);
        self.ask(now_ns);
    }

    fn show_hint(&mut self, now_ns: u64, damage: &mut Region) {
        let Some(action) = self
            .known
            .as_ref()
            .and_then(|known| known.line.first().copied())
        else {
            return;
        };
        self.light(action, now_ns, damage);
    }

    /// With no line known, the move most likely to help.
    fn show_fallback(&mut self, now_ns: u64, damage: &mut Region) {
        let Some(table) = self.table() else {
            return;
        };
        match likely(table) {
            Some(action) => self.light(action, now_ns, damage),
            None => self.say(Note::NoMove, now_ns, true, damage),
        }
    }

    /// Light the cards `action` moves and where they would go.
    fn light(&mut self, action: Action, now_ns: u64, damage: &mut Region) {
        let Some(table) = self.table() else {
            return;
        };
        let layout = &self.layout;
        let mut rects = Vec::new();
        let rect_of = |card: CardId| -> Rect {
            layout.card_at(
                self.placements
                    .get(card.index())
                    .map_or(Point::ORIGIN, |p| p.at),
            )
        };
        match action {
            Action::Move { from, to, count } => {
                let cards = table.pile(from);
                let first = cards.len().saturating_sub(usize::from(count));
                let span = cards[first..]
                    .iter()
                    .map(|slot| rect_of(slot.card()))
                    .reduce(|a, b| a.union(&b));
                rects.extend(span);
                rects.push(layout.landing(table, to));
            }
            Action::Draw | Action::Recycle => {
                if let Some(stock) = table.shape().stock {
                    rects.push(layout.pile_area(table, stock));
                }
            }
        }
        self.motion.glow(rects, now_ns, damage);
    }

    /// Ask for the last position along the line the game could still be won
    /// from.
    pub fn rewind(&mut self, now_ns: u64, damage: &mut Region) {
        let Some(play) = &self.play else {
            return;
        };
        if self.lost != Some(play.table().fingerprint()) {
            return;
        }
        let (start, _) = Table::deal(play.rules(), play.number());
        let line = play.line().iter().map(|step| step.applied.action).collect();
        self.outbox.question = Some(Question::Rewind {
            start,
            line,
            winnable: self.proven,
        });
        self.say(Note::Thinking, now_ns, true, damage);
    }

    fn rewind_to(&mut self, keep: usize, now_ns: u64, damage: &mut Region) {
        let held = self.release();
        let Some(play) = &mut self.play else {
            return;
        };
        while play.moves() > keep && !play.undo().is_empty() {}
        self.quiet(Note::Thinking, damage);
        self.animate(now_ns, Pace::Slide, &held, damage);
        self.known = None;
        self.lost = None;
        self.after_move(now_ns, damage);
    }

    // --- Preferences ----------------------------------------------------

    /// Make `now` the preferences in force, owing their write, and act on
    /// what changed.
    pub(crate) fn choose(&mut self, now: Preferences, now_ns: u64, damage: &mut Region) {
        let was = *self.prefs();
        if was == now {
            return;
        }
        self.ledger.choose(now);
        self.outbox.menu_moved = true;
        if (was.back, was.baize, was.four_colours) != (now.back, now.baize, now.four_colours) {
            damage.add(self.layout.client);
        }
        if now.auto_home && !was.auto_home {
            self.steps.clear();
            self.steps.resize(self.placements.len(), UNMOVED);
            if self.follow_home() > 0 {
                self.animate(now_ns, Pace::Steps, &[], damage);
                self.after_move(now_ns, damage);
            }
        }
        if now.winnable && !was.winnable {
            self.prefetched = None;
        }
    }

    /// The statistics alert, for the game being played.
    pub fn show_statistics(&mut self) {
        let variant = self
            .play
            .as_ref()
            .map_or(self.prefs().variant, Play::variant);
        if let Some(spec) = menu::statistics(variant, &self.ledger.stats()) {
            self.outbox.alert = Some((Alerting::Statistics, spec));
        }
    }

    /// An alert was answered with the button at `chosen`, or dismissed.
    pub fn alert_answered(
        &mut self,
        alert: Alerting,
        chosen: Option<u8>,
        now_ns: u64,
        rng: &mut dyn RandU64,
        damage: &mut Region,
    ) {
        let variant = self
            .play
            .as_ref()
            .map_or(self.prefs().variant, Play::variant);
        match (alert, chosen) {
            (Alerting::Won, Some(0)) => self.new_game(variant, None, now_ns, rng, damage),
            (Alerting::Statistics, Some(1)) => {
                if let Some(spec) = menu::forget(variant) {
                    self.outbox.alert = Some((Alerting::Forget, spec));
                }
            }
            (Alerting::Forget, Some(0)) => self.ledger.tally(Tally::Reset(variant)),
            _ => {}
        }
    }

    pub(crate) fn end_drag(&mut self) {
        self.press = None;
        self.drag = None;
    }

    /// Let go of any cards in hand, answering where each is drawn, for the
    /// movement that follows to start from there.
    pub(crate) fn release(&mut self) -> Vec<(CardId, Point)> {
        self.press = None;
        self.drag.take().map(|drag| drag.held).unwrap_or_default()
    }

    /// The table's menu was asked for at `at`.
    pub(crate) fn want_context(&mut self, at: Point) {
        self.outbox.context = Some(at);
    }
}

/// Move every card of `held` by as much as takes the first to `to`,
/// repainting each where it was and where it is.
pub(crate) fn shift(
    held: &mut [(CardId, Point)],
    to: Point,
    size: (u32, u32),
    damage: &mut Region,
) {
    let Some(&(_, base)) = held.first() else {
        return;
    };
    let (dx, dy) = (to.x - base.x, to.y - base.y);
    for (_, at) in held {
        damage.add(crate::motion::card_rect(*at, size));
        *at = Point::new(at.x + dx, at.y + dy);
        damage.add(crate::motion::card_rect(*at, size));
    }
}

/// The next card a settled game sends home, if the game is settled.
fn finishing(table: &Table) -> Option<Action> {
    if !table.settled() {
        return None;
    }
    let shape = table.shape();
    shape
        .waste
        .into_iter()
        .chain(shape.cells.iter())
        .chain(shape.tableau.iter())
        .find_map(|from| {
            let face = table.top_face(from)?;
            let to = table.foundation_for(face)?;
            Some(Action::Move { from, to, count: 1 })
        })
}

/// The move most likely to help when no winning line is known: one home,
/// then one that turns a card over, then any onto a column, then a draw.
fn likely(table: &Table) -> Option<Action> {
    let mut actions = Vec::new();
    table.actions(&mut actions);
    let rank = |action: &Action| -> u8 {
        match *action {
            Action::Move { from, to, count } => {
                let cards = table.pile(from);
                let uncovers = cards
                    .len()
                    .checked_sub(usize::from(count) + 1)
                    .and_then(|at| cards.get(at))
                    .is_some_and(|slot| !slot.up());
                match table.role(to) {
                    Some(Role::Foundation) => 0,
                    _ if uncovers => 1,
                    Some(Role::Tableau) if !table.pile(to).is_empty() => 2,
                    _ => 4,
                }
            }
            Action::Draw => 3,
            Action::Recycle => 5,
        }
    };
    actions.into_iter().min_by_key(rank)
}

#[cfg(test)]
#[path = "game_tests.rs"]
mod tests;
