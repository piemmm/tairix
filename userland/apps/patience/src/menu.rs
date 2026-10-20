//! Commands, the menus that offer them, and the words the game's alerts say.
//!
//! The icon-bar menu and the table's own context menu are built from one
//! description, so the two can never offer different games or disagree about
//! which is chosen; only the context menu carries the field a deal's number
//! is typed into, because only a menu the window opens has one.

use alloc::format;
use alloc::string::String;
use core::fmt::Write as _;

use tairix_abi::window_ipc::{
    AlertButton, AlertButtonRole, AlertSeverity, AlertSpec, AppMenu, AppMenuSymbol,
};
use tairix_window::menu::{MenuBuilder, Plate};
use tairix_window::{QUIT_LABEL, QUIT_ROW};

use crate::prefs::{Back, Baize, Preferences};
use crate::score::{Score, Scoring};
use crate::stats::{Record, Stats};
use crate::variant::Variant;

/// Something the player can ask for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Deal a new game of the game being played.
    NewGame,
    /// Deal this game again from the start.
    Restart,
    /// Take the last move back.
    Undo,
    /// Make the last move taken back again.
    Redo,
    /// Show a move that keeps the game winnable.
    Hint,
    /// Go back to the last position the game could still be won from.
    Rewind,
    /// Send every card home, the game being settled.
    Finish,
    /// Play this variant.
    Play(Variant),
    /// Score Klondike this way.
    Score(Scoring),
    /// Deal only winnable games, or any.
    Winnable,
    /// Send cards home unasked, or not.
    AutoHome,
    /// Four-colour suits, or two.
    FourColours,
    /// These card backs.
    Back(Back),
    /// This table.
    Baize(Baize),
    /// Show the statistics.
    Statistics,
}

/// Row ids, numbered from the shared *Quit* id so the two cannot collide.
const NEW_GAME: u16 = QUIT_ROW + 1;
const RESTART: u16 = NEW_GAME + 1;
const UNDO: u16 = RESTART + 1;
const REDO: u16 = UNDO + 1;
const HINT: u16 = REDO + 1;
const REWIND: u16 = HINT + 1;
const FINISH: u16 = REWIND + 1;
const WINNABLE: u16 = FINISH + 1;
const AUTO_HOME: u16 = WINNABLE + 1;
const FOUR_COLOURS: u16 = AUTO_HOME + 1;
const STATISTICS: u16 = FOUR_COLOURS + 1;
const FIRST_VARIANT: u16 = STATISTICS + 1;
const FIRST_SCORING: u16 = FIRST_VARIANT + 6;
const FIRST_BACK: u16 = FIRST_SCORING + 3;
const FIRST_BAIZE: u16 = FIRST_BACK + 4;
/// The context menu's deal-number field.
pub const DEAL_ENTRY: u16 = FIRST_BAIZE + 4;
/// The context menu's row that carries the field.
const DEAL_ROW: u16 = DEAL_ENTRY + 1;
// Each run of ids above is as long as the list it numbers.
const _: () = assert!(
    (FIRST_SCORING - FIRST_VARIANT) as usize == Variant::ALL.len()
        && (FIRST_BACK - FIRST_SCORING) as usize == Scoring::ALL.len()
        && (FIRST_BAIZE - FIRST_BACK) as usize == Back::ALL.len()
        && (DEAL_ENTRY - FIRST_BAIZE) as usize == Baize::ALL.len()
);

impl Command {
    /// The command a menu row's id names, or `None` for one no menu here
    /// carries.
    #[must_use]
    pub fn of_row(id: u16) -> Option<Self> {
        let offset = |first: u16| id.checked_sub(first).map(usize::from);
        Some(match id {
            NEW_GAME => Self::NewGame,
            RESTART => Self::Restart,
            UNDO => Self::Undo,
            REDO => Self::Redo,
            HINT => Self::Hint,
            REWIND => Self::Rewind,
            FINISH => Self::Finish,
            WINNABLE => Self::Winnable,
            AUTO_HOME => Self::AutoHome,
            FOUR_COLOURS => Self::FourColours,
            STATISTICS => Self::Statistics,
            _ if (FIRST_VARIANT..FIRST_SCORING).contains(&id) => {
                Self::Play(*Variant::ALL.get(offset(FIRST_VARIANT)?)?)
            }
            _ if (FIRST_SCORING..FIRST_BACK).contains(&id) => {
                Self::Score(*Scoring::ALL.get(offset(FIRST_SCORING)?)?)
            }
            _ if (FIRST_BACK..FIRST_BAIZE).contains(&id) => {
                Self::Back(*Back::ALL.get(offset(FIRST_BACK)?)?)
            }
            _ if (FIRST_BAIZE..DEAL_ENTRY).contains(&id) => {
                Self::Baize(*Baize::ALL.get(offset(FIRST_BAIZE)?)?)
            }
            _ => return None,
        })
    }
}

/// A row the menus offer only while the game allows it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Offer {
    /// A move to take back.
    Undo,
    /// A move to make again.
    Redo,
    /// The game proven lost, so there is a winnable position to go back to.
    Rewind,
    /// The game settled, every card free to go home.
    Finish,
}

/// Which [`Offer`] rows may be chosen now.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Offers(u8);

impl Offers {
    /// These offers with `offer` made, or withdrawn.
    #[must_use]
    pub const fn with(self, offer: Offer, made: bool) -> Self {
        let bit = 1 << offer as u8;
        Self(if made { self.0 | bit } else { self.0 & !bit })
    }

    /// Whether `offer` is made.
    #[must_use]
    pub const fn has(self, offer: Offer) -> bool {
        self.0 & (1 << offer as u8) != 0
    }
}

/// The game's menu: the icon bar's when `deal` is `None`, the table's own
/// when it names the deal in play, whose number its field starts at.
#[must_use]
pub fn menu(prefs: &Preferences, offers: Offers, deal: Option<u32>) -> AppMenu {
    let mut menu = MenuBuilder::new();
    let root = Plate::Root;
    if deal.is_none() {
        menu.info();
    }
    play_rows(&mut menu, offers, deal);
    menu.separator(root);
    if let Some(games) = menu.submenu("Game", AppMenuSymbol::NewGame, root) {
        game_rows(&mut menu, *prefs, games);
    }
    if let Some(options) = menu.submenu("Options", AppMenuSymbol::Settings, root) {
        option_rows(&mut menu, *prefs, options);
    }
    if let Some(cards) = menu.submenu("Cards", AppMenuSymbol::Colours, root) {
        look_rows(&mut menu, *prefs, cards);
    }
    menu.separator(root);
    menu.item(
        STATISTICS,
        AppMenuSymbol::SystemMonitor,
        "Statistics\u{2026}",
        "",
        true,
        root,
    );
    if deal.is_none() {
        menu.separator(root);
        menu.item(QUIT_ROW, AppMenuSymbol::Quit, QUIT_LABEL, "", true, root);
    }
    menu.finish()
}

/// The id of the row `index` places past `first`.
fn nth_row(first: u16, index: usize) -> u16 {
    first.saturating_add(u16::try_from(index).unwrap_or(u16::MAX))
}

/// The rows that play: dealing, the deal's number, and the moves.
fn play_rows(menu: &mut MenuBuilder, offers: Offers, deal: Option<u32>) {
    let root = Plate::Root;
    menu.item(
        NEW_GAME,
        AppMenuSymbol::NewGame,
        "New game",
        "N",
        true,
        root,
    );
    menu.item(
        RESTART,
        AppMenuSymbol::Refresh,
        "Restart this deal",
        "R",
        true,
        root,
    );
    if let Some(number) = deal {
        let text = format!("{number}");
        menu.entry(
            DEAL_ROW,
            DEAL_ENTRY,
            AppMenuSymbol::GoTo,
            "Play deal number",
            &text,
            root,
        );
    }
    menu.separator(root);
    let offered = [
        (UNDO, AppMenuSymbol::Undo, "Undo", "Ctrl+Z", Offer::Undo),
        (REDO, AppMenuSymbol::Redo, "Redo", "Ctrl+Y", Offer::Redo),
    ];
    for (id, symbol, label, shortcut, offer) in offered {
        menu.item(id, symbol, label, shortcut, offers.has(offer), root);
    }
    menu.item(HINT, AppMenuSymbol::Find, "Hint", "H", true, root);
    let offered = [
        (
            REWIND,
            AppMenuSymbol::Previous,
            "Back to last winnable",
            "Ctrl+B",
            Offer::Rewind,
        ),
        (FINISH, AppMenuSymbol::MoveUp, "Finish", "F", Offer::Finish),
    ];
    for (id, symbol, label, shortcut, offer) in offered {
        menu.item(id, symbol, label, shortcut, offers.has(offer), root);
    }
}

/// The games, one chosen.
fn game_rows(menu: &mut MenuBuilder, prefs: Preferences, plate: Plate) {
    for (index, variant) in Variant::ALL.into_iter().enumerate() {
        let key = [b'1' + u8::try_from(index).unwrap_or(0)];
        let shortcut = core::str::from_utf8(&key).unwrap_or("");
        menu.radio(
            nth_row(FIRST_VARIANT, index),
            level(variant),
            variant.label(),
            shortcut,
            prefs.variant == variant,
            plate,
        );
    }
}

/// How the game is dealt, played and scored.
fn option_rows(menu: &mut MenuBuilder, prefs: Preferences, plate: Plate) {
    let marks = [
        (
            WINNABLE,
            AppMenuSymbol::LevelEasy,
            "Winnable deals only",
            prefs.winnable,
        ),
        (
            AUTO_HOME,
            AppMenuSymbol::MoveUp,
            "Send cards home",
            prefs.auto_home,
        ),
        (
            FOUR_COLOURS,
            AppMenuSymbol::Colours,
            "Four-colour suits",
            prefs.four_colours,
        ),
    ];
    for (id, symbol, label, on) in marks {
        menu.mark(id, symbol, label, "", on, plate);
    }
    menu.separator(plate);
    for (index, scoring) in Scoring::ALL.into_iter().enumerate() {
        menu.radio(
            nth_row(FIRST_SCORING, index),
            AppMenuSymbol::Info,
            scoring.label(),
            "",
            prefs.scoring == scoring,
            plate,
        );
    }
}

/// The card backs and the table, one of each chosen.
fn look_rows(menu: &mut MenuBuilder, prefs: Preferences, plate: Plate) {
    for (index, back) in Back::ALL.into_iter().enumerate() {
        menu.radio(
            nth_row(FIRST_BACK, index),
            AppMenuSymbol::Colours,
            back.label(),
            "",
            prefs.back == back,
            plate,
        );
    }
    menu.separator(plate);
    for (index, baize) in Baize::ALL.into_iter().enumerate() {
        menu.radio(
            nth_row(FIRST_BAIZE, index),
            AppMenuSymbol::Wallpaper,
            baize.label(),
            "",
            prefs.baize == baize,
            plate,
        );
    }
}

/// The function a game's row shows: harder games climb.
const fn level(variant: Variant) -> AppMenuSymbol {
    use crate::card::SpiderSuits;
    use crate::variant::Draw;
    match variant {
        Variant::Klondike(Draw::One) | Variant::Spider(SpiderSuits::One) => {
            AppMenuSymbol::LevelEasy
        }
        Variant::Klondike(Draw::Three) | Variant::FreeCell | Variant::Spider(SpiderSuits::Two) => {
            AppMenuSymbol::LevelMedium
        }
        Variant::Spider(SpiderSuits::Four) => AppMenuSymbol::LevelHard,
    }
}

/// A duration as a clock shows it: `m:ss`, or `h:mm:ss` from an hour.
#[must_use]
pub fn clock(secs: u32) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// A score as the status line and the alerts show it.
#[must_use]
pub fn score_text(score: Score) -> String {
    match score {
        Score::Points(points) => format!("{points} points"),
        Score::Dollars(dollars) if dollars < 0 => format!("\u{2212}${}", dollars.unsigned_abs()),
        Score::Dollars(dollars) => format!("${dollars}"),
    }
}

/// What one variant's record says, a line at a time.
fn record_lines(variant: Variant, record: Record, bank: Option<i64>) -> String {
    let mut text = format!(
        "Played {} \u{b7} won {} ({}%)",
        record.played,
        record.won,
        record.percent()
    );
    let streak = match record.streak {
        0 => String::from("none"),
        1 => String::from("1 win"),
        -1 => String::from("1 loss"),
        n if n > 0 => format!("{n} wins"),
        n => format!("{} losses", n.unsigned_abs()),
    };
    let _ = write!(
        text,
        "\nCurrent run: {streak} \u{b7} longest run of wins: {}",
        record.best_streak
    );
    let mut bests = String::new();
    if let Some(fastest) = record.fastest {
        let _ = write!(bests, "Fastest win: {}", clock(fastest));
    }
    if let Some(fewest) = record.fewest {
        if !bests.is_empty() {
            bests.push_str(" \u{b7} ");
        }
        let _ = write!(bests, "fewest moves: {fewest}");
    }
    if !bests.is_empty() {
        text.push('\n');
        text.push_str(&bests);
    }
    if let Some(best) = record.best_score {
        let _ = write!(text, "\nBest score: {best}");
    }
    if let (Variant::Klondike(_), Some(bank)) = (variant, bank) {
        let _ = write!(
            text,
            "\nVegas winnings: {}",
            score_text(Score::Dollars(bank))
        );
    }
    text
}

fn button(label: &str, role: AlertButtonRole) -> Option<AlertButton> {
    AlertButton::new(label, role).ok()
}

/// The statistics alert for `variant`: its record, and the way to forget it.
#[must_use]
pub fn statistics(variant: Variant, stats: &Stats) -> Option<AlertSpec> {
    let detail = record_lines(variant, stats.record(variant), Some(stats.bank()));
    let buttons = [
        button("Close", AlertButtonRole::Default)?,
        button("Forget\u{2026}", AlertButtonRole::Plain)?,
    ];
    AlertSpec::new(
        AlertSeverity::Information,
        variant.label(),
        &detail,
        &buttons,
    )
    .ok()
}

/// The question asked before a record is forgotten.
#[must_use]
pub fn forget(variant: Variant) -> Option<AlertSpec> {
    let headline = format!("Forget the record of {}?", variant.label());
    let buttons = [
        button("Forget", AlertButtonRole::Destructive)?,
        button("Keep", AlertButtonRole::Cancel)?,
    ];
    AlertSpec::new(
        AlertSeverity::Question,
        &headline,
        "Its games, runs and best times are cleared for good.",
        &buttons,
    )
    .ok()
}

/// The alert a won game ends on.
#[must_use]
pub fn won(
    variant: Variant,
    secs: u32,
    moves: u32,
    score: Option<Score>,
    stats: &Stats,
) -> Option<AlertSpec> {
    let headline = format!("You won in {}", clock(secs));
    let mut detail = format!("{moves} moves");
    if let Some(score) = score {
        detail.push_str(" \u{b7} ");
        detail.push_str(&score_text(score));
    }
    detail.push('\n');
    detail.push_str(&record_lines(variant, stats.record(variant), None));
    let buttons = [
        button("Deal again", AlertButtonRole::Default)?,
        button("Close", AlertButtonRole::Cancel)?,
    ];
    AlertSpec::new(AlertSeverity::Information, &headline, &detail, &buttons).ok()
}

/// The deal a typed number names in `variant`, read leniently: spaces and
/// thousands separators are ignored, anything else refuses it.
#[must_use]
pub fn deal_number(variant: Variant, text: &str) -> Option<crate::deal::DealNumber> {
    let mut value: u32 = 0;
    let mut digits = 0;
    for ch in text.chars() {
        match ch {
            '0'..='9' => {
                value = value.checked_mul(10)?.checked_add(ch.to_digit(10)?)?;
                digits += 1;
            }
            ' ' | ',' | '.' | '\u{a0}' | '\u{2009}' | '#' => {}
            _ => return None,
        }
    }
    if digits == 0 {
        return None;
    }
    crate::deal::DealNumber::new(variant, value)
}

#[cfg(test)]
#[path = "menu_tests.rs"]
mod tests;
