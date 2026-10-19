//! Host tests for the system authorisation sheet.

extern crate std;

use alloc::vec::Vec;

use tairix_abi::time::Duration64;
use tairix_controls::testkit::premul;
use tairix_controls::{damage, Keystroke};
use tairix_geometry::{Point, Rect, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey};
use tairix_raster::Surface;

use super::{AuthoriseAction, AuthoriseSheet, AUTHORISE_WIDTH, ROWS_IN_VIEW};
use crate::chooser::AccountTile;
use crate::surface::{CHECKING, HINT, REFUSED};
use crate::testkit::{centre, moved, still, theme, PRESS, RELEASE};

const SCALE: Scale = Scale::ONE;

fn ada() -> AccountTile {
    AccountTile::new("Ada Lovelace", "ada")
}

fn others(count: usize) -> Vec<AccountTile> {
    (0..count)
        .map(|index| {
            let name = std::format!("admin{index}");
            AccountTile::new(&name, &name)
        })
        .collect()
}

fn sheet() -> AuthoriseSheet {
    AuthoriseSheet::new(
        "Date & Time",
        "wants to change the date, time and time zone.",
    )
}

/// Where a sheet is drawn: its own extent at the origin.
fn bounds(sheet: &AuthoriseSheet) -> Rect {
    let (width, height) = sheet.client_extent(SCALE, &theme());
    Rect::new(0, 0, width, height)
}

fn press(sheet: &mut AuthoriseSheet, key: Key, modifiers: Modifiers) -> Option<AuthoriseAction> {
    let at = bounds(sheet);
    sheet.on_key(
        Keystroke {
            key,
            modifiers,
            at_ns: 0,
        },
        at,
        SCALE,
        &theme(),
        &mut damage::sink(),
    )
}

fn named(sheet: &mut AuthoriseSheet, name: NamedKey) -> Option<AuthoriseAction> {
    press(sheet, Key::Named(name), Modifiers::default())
}

fn type_in(sheet: &mut AuthoriseSheet, text: &str) {
    for ch in text.chars() {
        let _ = press(sheet, Key::Char(ch), Modifiers::default());
    }
}

fn pointer(sheet: &mut AuthoriseSheet, event: &InputEvent) -> Option<AuthoriseAction> {
    let at = bounds(sheet);
    sheet.on_pointer(event, at, SCALE, &theme(), &mut damage::sink())
}

/// A completed primary click at the centre of `rect`.
fn click(sheet: &mut AuthoriseSheet, rect: Rect) -> Option<AuthoriseAction> {
    let _ = pointer(sheet, &centre(rect));
    let _ = pointer(sheet, &PRESS);
    pointer(sheet, &RELEASE)
}

fn render(sheet: &AuthoriseSheet) -> Surface {
    let at = bounds(sheet);
    let mut surface = Surface::new(at.width, at.height).expect("a sheet-sized surface");
    sheet.render(&mut surface, at, SCALE, &theme());
    surface
}

#[test]
fn an_account_that_may_allow_is_asked_only_for_its_password() {
    let mut sheet = sheet();
    sheet.set_authorisers(Some(ada()), Vec::new());
    assert_eq!(sheet.account(), Some(&ada()));
    type_in(&mut sheet, "byron");
    assert_eq!(
        named(&mut sheet, NamedKey::Enter),
        Some(AuthoriseAction::Offered)
    );
    assert_eq!(sheet.secret(), Some("byron"));
}

#[test]
fn the_own_account_offers_the_others_that_may_and_steps_back_to_cancel() {
    let mut sheet = sheet();
    sheet.set_authorisers(Some(ada()), others(2));
    let at = bounds(&sheet);
    let field = sheet
        .field_rect(at, SCALE, &theme())
        .expect("a password is asked for");
    // Field, then the way to another account, then Cancel and Allow.
    let _ = named(&mut sheet, NamedKey::Tab);
    assert_eq!(
        named(&mut sheet, NamedKey::Enter),
        None,
        "the link opens the chooser"
    );
    assert_eq!(sheet.account(), None);
    assert_eq!(sheet.row_rects(bounds(&sheet), SCALE, &theme()).len(), 2);
    assert!(sheet.field_rect(bounds(&sheet), SCALE, &theme()).is_none());
    assert!(field.width > 0);
}

#[test]
fn an_account_that_may_not_chooses_from_those_that_may() {
    let mut sheet = sheet();
    sheet.set_authorisers(None, others(3));
    assert_eq!(
        sheet.account(),
        None,
        "nothing is asked until an account is chosen"
    );
    let _ = named(&mut sheet, NamedKey::Down);
    assert_eq!(named(&mut sheet, NamedKey::Enter), None);
    assert_eq!(sheet.account().map(AccountTile::login_name), Some("admin1"));
    type_in(&mut sheet, "secret");
    assert_eq!(
        named(&mut sheet, NamedKey::Enter),
        Some(AuthoriseAction::Offered)
    );

    // Escape steps back to the chooser, still on the account chosen, and
    // only then cancels.
    let mut sheet = AuthoriseSheet::new("Settings", "wants to change a machine setting.");
    sheet.set_authorisers(None, others(3));
    let _ = named(&mut sheet, NamedKey::Down);
    let _ = named(&mut sheet, NamedKey::Enter);
    assert_eq!(named(&mut sheet, NamedKey::Escape), None);
    assert_eq!(sheet.account(), None);
    let _ = named(&mut sheet, NamedKey::Enter);
    assert_eq!(sheet.account().map(AccountTile::login_name), Some("admin1"));
    let _ = named(&mut sheet, NamedKey::Escape);
    assert_eq!(
        named(&mut sheet, NamedKey::Escape),
        Some(AuthoriseAction::Cancelled)
    );
}

#[test]
fn when_no_account_may_the_sheet_says_so_and_asks_for_nothing() {
    let mut sheet = sheet();
    sheet.set_authorisers(None, Vec::new());
    assert_eq!(sheet.account(), None);
    assert!(sheet.field_rect(bounds(&sheet), SCALE, &theme()).is_none());
    type_in(&mut sheet, "anything");
    assert_eq!(
        named(&mut sheet, NamedKey::Enter),
        Some(AuthoriseAction::Cancelled)
    );

    let mut sheet = AuthoriseSheet::new("Settings", "wants to change a machine setting.");
    sheet.set_unavailable();
    assert_eq!(
        named(&mut sheet, NamedKey::Enter),
        Some(AuthoriseAction::Cancelled)
    );
}

#[test]
fn nothing_is_offered_until_a_password_is_typed_and_none_past_any_password() {
    let mut sheet = sheet();
    sheet.set_authorisers(Some(ada()), Vec::new());
    assert_eq!(
        named(&mut sheet, NamedKey::Enter),
        None,
        "an empty field offers nothing"
    );
    let long = "x".repeat(tairix_abi::account::MAX_PASSWORD_LEN + 1);
    type_in(&mut sheet, &long);
    assert_eq!(named(&mut sheet, NamedKey::Enter), None);
    assert_eq!(
        sheet.stated(),
        REFUSED,
        "refused in the authority's own words"
    );
    assert!(sheet.secret().is_some_and(str::is_empty), "and erased");
}

#[test]
fn while_checking_the_sheet_takes_nothing_and_a_refusal_asks_again() {
    let mut sheet = sheet();
    sheet.set_authorisers(Some(ada()), Vec::new());
    type_in(&mut sheet, "wrong");
    let _ = named(&mut sheet, NamedKey::Enter);
    sheet.check();
    assert!(sheet.is_checking());
    assert_eq!(sheet.stated(), CHECKING);
    assert_eq!(
        sheet.secret(),
        Some(""),
        "the offered password left the field"
    );
    assert_eq!(
        named(&mut sheet, NamedKey::Escape),
        None,
        "not even a cancel"
    );
    type_in(&mut sheet, "more");
    assert_eq!(sheet.secret(), Some(""));

    sheet.refuse(0, &theme());
    assert!(!sheet.is_checking());
    assert_eq!(sheet.stated(), REFUSED);
    assert!(sheet.next_frame_in(0).is_some(), "the question shakes");
    type_in(&mut sheet, "x");
    assert_eq!(sheet.stated(), HINT, "typing asks the question again");

    let mut calm = sheet_refused_in_still_motion();
    assert!(
        calm.next_frame_in(0).is_none(),
        "reduced motion shakes nothing"
    );
    assert_eq!(calm.stated(), REFUSED);
    let _ = named(&mut calm, NamedKey::Enter);
}

fn sheet_refused_in_still_motion() -> AuthoriseSheet {
    let mut sheet = sheet();
    sheet.set_authorisers(Some(ada()), Vec::new());
    sheet.check();
    sheet.refuse(0, &still());
    sheet
}

#[test]
fn a_cooling_account_is_told_how_long_and_offers_nothing_until_it_ends() {
    let mut sheet = sheet();
    sheet.set_authorisers(Some(ada()), Vec::new());
    assert!(sheet.set_cooldown(Duration64::from_nanos(4_200_000_000)));
    assert!(sheet.stated().contains("5s"), "{}", sheet.stated());
    assert!(
        !sheet.set_cooldown(Duration64::from_nanos(4_100_000_000)),
        "the same second"
    );
    type_in(&mut sheet, "byron");
    assert_eq!(named(&mut sheet, NamedKey::Enter), None);
    assert!(sheet.set_cooldown(Duration64::ZERO));
    assert_eq!(sheet.stated(), HINT);
    assert_eq!(
        named(&mut sheet, NamedKey::Enter),
        Some(AuthoriseAction::Offered)
    );
}

#[test]
fn the_secure_attention_key_answers_on_the_band_and_takes_the_keyboard_to_the_password() {
    let mut sheet = sheet();
    sheet.set_authorisers(Some(ada()), others(1));
    let _ = named(&mut sheet, NamedKey::Tab);
    sheet.attend(0);
    assert!(sheet.next_frame_in(0).is_some(), "the band answers");
    type_in(&mut sheet, "byron");
    assert_eq!(
        named(&mut sheet, NamedKey::Enter),
        Some(AuthoriseAction::Offered)
    );
    let at = bounds(&sheet);
    let mut owed = damage::sink();
    sheet.advance(1_000_000_000, at, SCALE, &theme(), &mut owed);
    assert!(sheet.next_frame_in(1_000_000_000).is_none(), "and settles");
}

#[test]
fn the_sheet_is_as_tall_as_what_it_asks_and_the_chooser_scrolls_past_a_few() {
    let theme = theme();
    let height = |sheet: &AuthoriseSheet| sheet.client_extent(SCALE, &theme).1;
    let finding = sheet();
    let (width, _) = finding.client_extent(SCALE, &theme);
    assert_eq!(width, AUTHORISE_WIDTH);
    let mut one = sheet();
    one.set_authorisers(None, others(1));
    let mut full = sheet();
    full.set_authorisers(None, others(ROWS_IN_VIEW));
    let mut many = sheet();
    many.set_authorisers(None, others(ROWS_IN_VIEW * 3));
    assert!(height(&one) < height(&full));
    assert_eq!(
        height(&full),
        height(&many),
        "past a view's worth, the list scrolls"
    );
    assert_eq!(
        many.row_rects(bounds(&many), SCALE, &theme).len(),
        ROWS_IN_VIEW
    );

    // Walking down past the view brings the focused account into it.
    for _ in 0..ROWS_IN_VIEW {
        let _ = named(&mut many, NamedKey::Down);
    }
    let shown: Vec<usize> = many
        .row_rects(bounds(&many), SCALE, &theme)
        .into_iter()
        .map(|(index, _)| index)
        .collect();
    assert_eq!(shown, (1..=ROWS_IN_VIEW).collect::<Vec<_>>());
}

#[test]
fn a_click_chooses_an_account_and_the_buttons_decide() {
    let mut sheet = sheet();
    sheet.set_authorisers(None, others(3));
    let rows = sheet.row_rects(bounds(&sheet), SCALE, &theme());
    let (index, rect) = rows[2];
    assert_eq!(click(&mut sheet, rect), None);
    assert_eq!(
        sheet.account().map(AccountTile::login_name),
        Some(std::format!("admin{index}").as_str())
    );
    let field = sheet
        .field_rect(bounds(&sheet), SCALE, &theme())
        .expect("its password is asked for");
    let _ = pointer(&mut sheet, &centre(field));
    let _ = pointer(&mut sheet, &PRESS);
    type_in(&mut sheet, "secret");
    // The Allow button sits at the right of the foot.
    let at = bounds(&sheet);
    let allow = Point::new(at.right() - 40, at.bottom() - 30);
    let _ = pointer(&mut sheet, &moved(allow.x, allow.y));
    let _ = pointer(&mut sheet, &PRESS);
    assert_eq!(
        pointer(&mut sheet, &RELEASE),
        Some(AuthoriseAction::Offered)
    );
}

#[test]
fn the_wheel_scrolls_the_accounts_in_view() {
    let mut sheet = sheet();
    sheet.set_authorisers(None, others(ROWS_IN_VIEW + 2));
    let _ = pointer(&mut sheet, &InputEvent::PointerScrolled { dx: 0, dy: 1 });
    let first = sheet.row_rects(bounds(&sheet), SCALE, &theme())[0].0;
    assert_eq!(first, 1);
    let _ = pointer(&mut sheet, &InputEvent::PointerScrolled { dx: 0, dy: 99 });
    let first = sheet.row_rects(bounds(&sheet), SCALE, &theme())[0].0;
    assert_eq!(first, 2, "never past the last view");
    let _ = pointer(&mut sheet, &InputEvent::PointerScrolled { dx: 0, dy: -99 });
    assert_eq!(sheet.row_rects(bounds(&sheet), SCALE, &theme())[0].0, 0);
}

#[test]
fn the_band_wears_the_accent_and_the_secret_is_never_drawn() {
    let theme = theme();
    let mut sheet = sheet();
    sheet.set_authorisers(Some(ada()), Vec::new());
    let frame = render(&sheet);
    let band = frame.get(frame.width() / 2, 4).expect("inside the sheet");
    assert_eq!(band, premul(theme.palette().accent));

    let mut typed = sheet;
    type_in(&mut typed, "hunter2");
    let debug = std::format!("{typed:?}");
    assert!(!debug.contains("hunter2"));
    assert!(!typed.stated().contains("hunter2"));
}

#[test]
fn the_secure_attention_notice_names_no_requester_and_only_closes() {
    let mut idle = AuthoriseSheet::nothing_waiting();
    assert_eq!(idle.account(), None);
    let mut asking = sheet();
    asking.set_authorisers(None, Vec::new());
    assert!(
        idle.client_extent(SCALE, &theme()).1 < asking.client_extent(SCALE, &theme()).1,
        "no requester block"
    );
    type_in(&mut idle, "hunter2");
    assert_eq!(
        named(&mut idle, NamedKey::Enter),
        Some(AuthoriseAction::Cancelled)
    );
}
