//! Unit tests for the text-entry family (spec §20 checklist).
//!
//! These cover the editor (insert/backspace/delete, caret navigation and
//! selection, character limit, Ctrl+A), the pointer caret placement and
//! selection drag, the read-only / denied / disabled distinction, the
//! validation rim and inline message, dark/light and high-contrast coverage,
//! scale, and the search field's magnifier chrome, query-active tint, and
//! Escape-clear behaviour.
//!
//! The masked field has its own section: that it draws the shared
//! secret-entry marker and nothing of what it holds, not even how much; that
//! its dots move on the text-mode cadence only while the owner keeps time and
//! never under reduced motion; that it appends and erases at the end and
//! nothing else; and the credential hygiene it promises — a buffer that never
//! reallocates while it fills, an erase that leaves no plaintext behind, and a
//! debug dump that reports a length instead of a password.

use alloc::format;
use alloc::string::String;

use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::{Pixel, Surface};
use tairix_theme::Theme;
use tairix_vt::secret::SECRET_TICK_NS;

use crate::damage::sink;
use crate::state::{AuthorityState, ControlState, ValidationState};
use crate::testkit::{control_font, has_pixel, high_contrast, marks_elision, premul};
use crate::text::{
    close_gap, debug_buffer_identity, debug_bytes, debug_zeroize, zeroize_range, Keystroke,
    SearchField, SecretField, TextAction, TextField,
};

const W: u32 = 200;
const H: u32 = 28;

fn font() -> BitmapFont {
    control_font(&Theme::dark(), Scale::ONE)
}

fn bounds() -> Rect {
    Rect::new(0, 0, W, H)
}

fn moved(x: i32, y: i32) -> InputEvent {
    InputEvent::PointerMoved {
        to: Point::new(x, y),
    }
}

const PRESS: InputEvent = InputEvent::PointerPressed {
    button: PointerButton::Primary,
};
const RELEASE: InputEvent = InputEvent::PointerReleased {
    button: PointerButton::Primary,
};

const NONE_MODS: Modifiers = Modifiers {
    shift: false,
    ctrl: false,
    alt: false,
    meta: false,
};
const SHIFT: Modifiers = Modifiers {
    shift: true,
    ctrl: false,
    alt: false,
    meta: false,
};
const CTRL: Modifiers = Modifiers {
    shift: false,
    ctrl: true,
    alt: false,
    meta: false,
};

fn field_surface(field: &TextField, theme: &Theme) -> Surface {
    let mut surface = Surface::new(W, H).expect("surface");
    field.render(&mut surface, bounds(), Scale::ONE, theme);
    surface
}

fn search_surface(field: &SearchField, theme: &Theme) -> Surface {
    let mut surface = Surface::new(W, H).expect("surface");
    field.render(&mut surface, bounds(), Scale::ONE, theme);
    surface
}

/// Type a string into a focused, editable field a character at a time.
fn type_str(field: &mut TextField, text: &str) {
    for ch in text.chars() {
        field.on_key(Key::Char(ch), NONE_MODS, bounds(), &mut sink());
    }
}

// --- Editing -----------------------------------------------------------------

/// Only a key press is a keystroke, and it keeps the time it was taken at.
#[test]
fn a_keystroke_is_a_key_press_taken_at_a_time() {
    let press = InputEvent::KeyPressed {
        key: Key::Char('a'),
        modifiers: Modifiers::default(),
    };
    assert_eq!(
        Keystroke::pressed(press, 42),
        Some(Keystroke {
            key: Key::Char('a'),
            modifiers: Modifiers::default(),
            at_ns: 42,
        })
    );
    assert_eq!(
        Keystroke::pressed(
            InputEvent::KeyReleased {
                key: Key::Char('a'),
                modifiers: Modifiers::default(),
            },
            42
        ),
        None
    );
}

#[test]
fn typing_inserts_and_reports_edits() {
    let mut field = TextField::new();
    field.set_focused(true);
    assert_eq!(
        field.on_key(Key::Char('h'), NONE_MODS, bounds(), &mut sink()),
        Some(TextAction::Edited)
    );
    type_str(&mut field, "i!");
    assert_eq!(field.text(), "hi!");
}

#[test]
fn backspace_and_delete_remove_characters() {
    let mut field = TextField::new().with_text("hello");
    field.set_focused(true);
    // Caret is at the end after with_text; backspace removes 'o'.
    assert_eq!(
        field.on_key(
            Key::Named(NamedKey::Backspace),
            NONE_MODS,
            bounds(),
            &mut sink()
        ),
        Some(TextAction::Edited)
    );
    assert_eq!(field.text(), "hell");
    // Home then forward-delete removes 'h'.
    field.on_key(Key::Named(NamedKey::Home), NONE_MODS, bounds(), &mut sink());
    assert_eq!(
        field.on_key(
            Key::Named(NamedKey::Delete),
            NONE_MODS,
            bounds(),
            &mut sink()
        ),
        Some(TextAction::Edited)
    );
    assert_eq!(field.text(), "ell");
}

#[test]
fn backspace_at_start_and_delete_at_end_do_nothing() {
    let mut field = TextField::new().with_text("x");
    field.set_focused(true);
    field.on_key(Key::Named(NamedKey::Home), NONE_MODS, bounds(), &mut sink());
    assert_eq!(
        field.on_key(
            Key::Named(NamedKey::Backspace),
            NONE_MODS,
            bounds(),
            &mut sink()
        ),
        None
    );
    field.on_key(Key::Named(NamedKey::End), NONE_MODS, bounds(), &mut sink());
    assert_eq!(
        field.on_key(
            Key::Named(NamedKey::Delete),
            NONE_MODS,
            bounds(),
            &mut sink()
        ),
        None
    );
    assert_eq!(field.text(), "x");
}

#[test]
fn caret_moves_and_inserts_in_the_middle() {
    let mut field = TextField::new().with_text("ac");
    field.set_focused(true);
    field.on_key(Key::Named(NamedKey::Left), NONE_MODS, bounds(), &mut sink());
    type_str(&mut field, "b");
    assert_eq!(field.text(), "abc");
}

#[test]
fn selection_then_typing_replaces() {
    let mut field = TextField::new().with_text("abc");
    field.set_focused(true);
    // Select the whole buffer, then type replaces it.
    assert_eq!(
        field.on_key(Key::Char('a'), CTRL, bounds(), &mut sink()),
        None
    );
    type_str(&mut field, "Z");
    assert_eq!(field.text(), "Z");
}

#[test]
fn shift_arrow_selects_and_backspace_deletes_selection() {
    let mut field = TextField::new().with_text("abcd");
    field.set_focused(true);
    // Select the last two characters with Shift+Left twice.
    field.on_key(Key::Named(NamedKey::Left), SHIFT, bounds(), &mut sink());
    field.on_key(Key::Named(NamedKey::Left), SHIFT, bounds(), &mut sink());
    assert_eq!(
        field.on_key(
            Key::Named(NamedKey::Backspace),
            NONE_MODS,
            bounds(),
            &mut sink()
        ),
        Some(TextAction::Edited)
    );
    assert_eq!(field.text(), "ab");
}

#[test]
fn character_limit_is_enforced() {
    let mut field = TextField::new().with_max_len(3);
    field.set_focused(true);
    type_str(&mut field, "abcdef");
    assert_eq!(field.text(), "abc");
}

#[test]
fn with_text_truncates_to_limit() {
    let field = TextField::new().with_max_len(2).with_text("abcd");
    assert_eq!(field.text(), "ab");
}

#[test]
fn multibyte_editing_stays_on_boundaries() {
    let mut field = TextField::new().with_text("café");
    field.set_focused(true);
    // Backspace removes the 'é' (a two-byte scalar) cleanly.
    assert_eq!(
        field.on_key(
            Key::Named(NamedKey::Backspace),
            NONE_MODS,
            bounds(),
            &mut sink()
        ),
        Some(TextAction::Edited)
    );
    assert_eq!(field.text(), "caf");
}

#[test]
fn enter_submits_and_escape_cancels() {
    let mut field = TextField::new().with_text("hi");
    field.set_focused(true);
    assert_eq!(
        field.on_key(
            Key::Named(NamedKey::Enter),
            NONE_MODS,
            bounds(),
            &mut sink()
        ),
        Some(TextAction::Submitted)
    );
    assert_eq!(
        field.on_key(
            Key::Named(NamedKey::Escape),
            NONE_MODS,
            bounds(),
            &mut sink()
        ),
        Some(TextAction::Cancelled)
    );
    // A plain text field's Escape does not clear its text.
    assert_eq!(field.text(), "hi");
}

// --- Authority / read-only / disabled ---------------------------------------

#[test]
fn disabled_field_ignores_input() {
    let mut field = TextField::new();
    field.set_state(ControlState::disabled());
    field.set_focused(true);
    assert_eq!(
        field.on_key(Key::Char('x'), NONE_MODS, bounds(), &mut sink()),
        None
    );
    assert_eq!(field.text(), "");
}

#[test]
fn denied_field_keeps_value_and_ignores_edits() {
    let mut field = TextField::new().with_text("secret");
    field.set_state(ControlState::idle().with_authority(AuthorityState::Denied));
    field.set_focused(true);
    assert_eq!(
        field.on_key(Key::Char('x'), NONE_MODS, bounds(), &mut sink()),
        None
    );
    assert_eq!(
        field.on_key(
            Key::Named(NamedKey::Backspace),
            NONE_MODS,
            bounds(),
            &mut sink()
        ),
        None
    );
    assert_eq!(field.text(), "secret");
}

#[test]
fn read_only_field_navigates_but_refuses_edits() {
    let mut field = TextField::new().with_text("value").read_only(true);
    field.set_focused(true);
    assert_eq!(
        field.on_key(Key::Char('x'), NONE_MODS, bounds(), &mut sink()),
        None
    );
    assert_eq!(
        field.on_key(
            Key::Named(NamedKey::Backspace),
            NONE_MODS,
            bounds(),
            &mut sink()
        ),
        None
    );
    // Navigation still works (no action, but no panic and no change).
    assert_eq!(
        field.on_key(Key::Named(NamedKey::Home), NONE_MODS, bounds(), &mut sink()),
        None
    );
    assert_eq!(field.text(), "value");
    assert!(field.is_read_only());
}

#[test]
fn denied_field_draws_lock_bead() {
    let theme = Theme::dark();
    let mut field = TextField::new().with_text("x");
    field.set_state(ControlState::idle().with_authority(AuthorityState::Denied));
    let surface = field_surface(&field, &theme);
    assert!(
        has_pixel(&surface, premul(theme.palette().denied)),
        "a denied field shows the denied Authority Mark"
    );
}

#[test]
fn read_only_reads_differently_from_disabled() {
    let theme = Theme::dark();
    let ro = TextField::new().with_text("x").read_only(true);
    let mut disabled = TextField::new().with_text("x");
    disabled.set_state(ControlState::disabled());
    // The read-only plate is the recessed surface; the disabled plate is too,
    // but the read-only text stays full-contrast while the disabled text is
    // muted, so the two are distinguishable.
    let ro_surface = field_surface(&ro, &theme);
    let dis_surface = field_surface(&disabled, &theme);
    assert!(has_pixel(&ro_surface, premul(theme.palette().on_surface)));
    assert!(has_pixel(
        &dis_surface,
        premul(theme.palette().on_surface_muted)
    ));
}

// --- Validation --------------------------------------------------------------

#[test]
fn invalid_field_shows_danger_rim() {
    let theme = Theme::dark();
    let mut field = TextField::new().with_text("bad");
    field.set_state(ControlState::idle().with_validation(ValidationState::Invalid));
    let surface = field_surface(&field, &theme);
    assert!(
        has_pixel(&surface, premul(theme.palette().danger)),
        "an invalid field shows a danger rim segment"
    );
}

#[test]
fn warning_field_shows_warning_rim() {
    let theme = Theme::dark();
    let mut field = TextField::new().with_text("meh");
    field.set_state(ControlState::idle().with_validation(ValidationState::Warning));
    let surface = field_surface(&field, &theme);
    assert!(has_pixel(&surface, premul(theme.palette().warning)));
}

#[test]
fn inline_message_is_drawn_below_when_there_is_room() {
    let theme = Theme::dark();
    // A tall bounds leaves room for the message row under the field row.
    let mut surface = Surface::new(W, 80).expect("surface");
    let mut field = TextField::new().with_text("x").with_message("required");
    field.set_state(ControlState::idle().with_validation(ValidationState::Invalid));
    field.render(&mut surface, Rect::new(0, 0, W, 80), Scale::ONE, &theme);
    // The message row (below the standard control height) is painted danger.
    let control_h = Scale::ONE.scale_length(theme.metrics().control_height);
    let mut found = false;
    for y in control_h..80 {
        for x in 0..W {
            if surface.get(x, y) == Some(premul(theme.palette().danger)) {
                found = true;
            }
        }
    }
    assert!(
        found,
        "the inline validation message is drawn below the field"
    );
}

// --- Pointer -----------------------------------------------------------------

#[test]
fn click_focuses_caret_and_typing_inserts_there() {
    let theme = Theme::dark();
    let mut field = TextField::new().with_text("aaaa");
    field.set_focused(true);
    // Click near the far left to place the caret at the start.
    field.on_pointer(&moved(1, 14), bounds(), Scale::ONE, &theme, &mut sink());
    field.on_pointer(&PRESS, bounds(), Scale::ONE, &theme, &mut sink());
    field.on_pointer(&RELEASE, bounds(), Scale::ONE, &theme, &mut sink());
    type_str(&mut field, "Z");
    assert!(
        field.text().starts_with('Z'),
        "clicking at the start places the caret there: {}",
        field.text()
    );
}

#[test]
fn drag_selects_a_range_then_typing_replaces_it() {
    let theme = Theme::dark();
    let mut field = TextField::new().with_text("abcdef");
    field.set_focused(true);
    let advance = font().cell_width();
    // Press at the start, drag several cells right, release: selects a run.
    field.on_pointer(&moved(1, 14), bounds(), Scale::ONE, &theme, &mut sink());
    field.on_pointer(&PRESS, bounds(), Scale::ONE, &theme, &mut sink());
    let far = 3 * i32::try_from(advance).unwrap() + 2;
    field.on_pointer(&moved(far, 14), bounds(), Scale::ONE, &theme, &mut sink());
    field.on_pointer(&RELEASE, bounds(), Scale::ONE, &theme, &mut sink());
    type_str(&mut field, "Z");
    assert!(
        field.text().starts_with('Z') && field.text().ends_with("def"),
        "a drag-selection is replaced by typing: {}",
        field.text()
    );
}

// --- Theme / scale -----------------------------------------------------------

#[test]
fn renders_in_dark_and_light_without_panic() {
    for theme in [Theme::dark(), Theme::light()] {
        let mut field = TextField::new().with_text("hello");
        field.set_focused(true);
        let surface = field_surface(&field, &theme);
        assert!(has_pixel(&surface, premul(theme.palette().on_surface)));
    }
}

#[test]
fn focused_field_draws_focus_ring() {
    let theme = Theme::dark();
    let mut focused = TextField::new();
    focused.set_focused(true);
    let surface = field_surface(&focused, &theme);
    assert!(
        has_pixel(&surface, premul(theme.palette().rim_active)),
        "a focused field draws the active focus ring"
    );
}

#[test]
fn high_contrast_thickens_the_rim() {
    let normal = Theme::dark();
    let heavy = high_contrast();
    let field = TextField::new().with_text("x");
    let normal_rim = count_color(
        &field_surface(&field, &normal),
        premul(normal.palette().rim),
    );
    let heavy_rim = count_color(&field_surface(&field, &heavy), premul(heavy.palette().rim));
    assert!(
        heavy_rim > normal_rim,
        "high contrast draws a thicker rim ({heavy_rim} vs {normal_rim})"
    );
}

fn count_color(surface: &Surface, want: Pixel) -> usize {
    surface.pixels().iter().filter(|&&p| p == want).count()
}

#[test]
fn renders_at_double_scale_without_panic() {
    let theme = Theme::dark();
    let mut surface = Surface::new(W * 2, H * 2).expect("surface");
    let mut field = TextField::new().with_text("scaled");
    field.set_focused(true);
    field.render(
        &mut surface,
        Rect::new(0, 0, W * 2, H * 2),
        Scale::from_percent(200).expect("scale"),
        &theme,
    );
    assert!(has_pixel(&surface, premul(theme.palette().on_surface)));
}

#[test]
fn degenerate_bounds_do_not_panic() {
    let theme = Theme::dark();
    let mut surface = Surface::new(4, 4).expect("surface");
    let field = TextField::new().with_text("too big for me");
    field.render(&mut surface, Rect::new(0, 0, 4, 4), Scale::ONE, &theme);
    // No assertion beyond "did not panic".
}

// --- SearchField -------------------------------------------------------------

#[test]
fn search_escape_clears_query_then_cancels() {
    let mut search = SearchField::new().with_text("query");
    search.set_focused(true);
    // First Escape clears the non-empty query and reports an edit.
    assert_eq!(
        search.on_key(
            Key::Named(NamedKey::Escape),
            NONE_MODS,
            bounds(),
            &mut sink()
        ),
        Some(TextAction::Edited)
    );
    assert_eq!(search.text(), "");
    assert!(!search.has_query());
    // A second Escape (now empty) cancels.
    assert_eq!(
        search.on_key(
            Key::Named(NamedKey::Escape),
            NONE_MODS,
            bounds(),
            &mut sink()
        ),
        Some(TextAction::Cancelled)
    );
}

#[test]
fn search_typing_builds_a_query() {
    let mut search = SearchField::new();
    search.set_focused(true);
    for ch in "abc".chars() {
        search.on_key(Key::Char(ch), NONE_MODS, bounds(), &mut sink());
    }
    assert_eq!(search.text(), "abc");
    assert!(search.has_query());
}

#[test]
fn search_magnifier_is_accent_when_query_present() {
    let theme = Theme::dark();
    let empty = SearchField::new();
    let active = SearchField::new().with_text("q");
    let empty_surface = search_surface(&empty, &theme);
    let active_surface = search_surface(&active, &theme);
    // The active search glyph is drawn in the accent colour; the empty one is
    // not (it is drawn muted).
    let accent = premul(theme.palette().accent);
    assert!(
        !has_pixel(&empty_surface, accent),
        "an empty search field's magnifier is quiet"
    );
    assert!(
        has_pixel(&active_surface, accent),
        "an active search field's magnifier reads as accent"
    );
}

#[test]
fn search_click_places_caret_after_the_magnifier() {
    let theme = Theme::dark();
    let mut search = SearchField::new().with_text("aaaa");
    search.set_focused(true);
    // A click well to the right lands somewhere in the text without panic.
    search.on_pointer(&moved(80, 14), bounds(), Scale::ONE, &theme, &mut sink());
    search.on_pointer(&PRESS, bounds(), Scale::ONE, &theme, &mut sink());
    search.on_pointer(&RELEASE, bounds(), Scale::ONE, &theme, &mut sink());
    // Typing at that caret keeps the buffer well-formed.
    search.on_key(Key::Char('Z'), NONE_MODS, bounds(), &mut sink());
    assert!(search.text().contains('Z'));
}

#[test]
fn a_focused_search_field_shows_one_accent_line_not_two() {
    for theme in [Theme::dark(), Theme::light()] {
        let accent = premul(theme.palette().rim_active);
        let mut search = SearchField::new();
        search.set_focused(true);
        let focused = search_surface(&search, &theme);
        // Down the middle of the row, so the count is the vertical runs of
        // the field's edge and not a corner's coverage blend: one ring per
        // side, never a ring with a lifted rim outside it.
        let lines = (0..W)
            .filter(|x| focused.get(*x, H / 2) == Some(accent))
            .count();
        assert_eq!(lines, 2, "{}: doubled focus border", theme.name());

        // The pointer resting on the focused field cannot restore the second
        // line either. It states nothing on the plate: the page a field is
        // written on is not a button's wash, and what reports the pointer is
        // the seat's own text cursor.
        let mid = moved(i32::try_from(W / 2).expect("in range"), 1);
        search.on_pointer(&mid, bounds(), Scale::ONE, &theme, &mut sink());
        let hovered = search_surface(&search, &theme);
        let lines = (0..W)
            .filter(|x| hovered.get(*x, H / 2) == Some(accent))
            .count();
        assert_eq!(lines, 2, "{}: hover doubled the focus border", theme.name());
        assert!(
            !has_pixel(&hovered, premul(theme.palette().surface_hover)),
            "{}: a hover washed the page instead of leaving it alone",
            theme.name()
        );
    }
}

#[test]
fn an_editable_field_is_written_on_the_page_and_a_read_only_one_recesses() {
    // The ground a field draws is what it *is*: a field the user may type in
    // is a page (paper on a light appearance), while a read-only one recesses
    // onto the window ground so it reads as a value shown rather than
    // entered. Both keep full-contrast text, so neither is a muted disabled
    // field.
    for theme in [Theme::dark(), Theme::light()] {
        let palette = theme.palette();
        let editable = field_surface(&TextField::new(), &theme);
        assert!(
            has_pixel(&editable, premul(palette.document)),
            "{}: an editable field is not on the page",
            theme.name()
        );
        assert!(
            !has_pixel(&editable, premul(palette.surface)),
            "{}: an editable field still shows the window ground",
            theme.name()
        );

        let read_only = field_surface(&TextField::new().read_only(true), &theme);
        assert!(
            has_pixel(&read_only, premul(palette.surface)),
            "{}: a read-only field is not recessed",
            theme.name()
        );
        assert!(
            !has_pixel(&read_only, premul(palette.document)),
            "{}: a read-only field is on the page an editable one owns",
            theme.name()
        );
    }
}

#[test]
fn a_disposed_field_keeps_the_plate_that_states_it() {
    // A page ground would erase what a disabled, denied, or failed-closed
    // field is saying, so the shared recipe's plate outranks it. Checked by
    // the one property that distinguishes them: none of the three is on the
    // page.
    let theme = Theme::light();
    let palette = theme.palette();
    let mut disabled = TextField::new();
    disabled.set_state(ControlState::disabled());
    let mut denied = TextField::new();
    denied.set_state(ControlState::idle().with_authority(AuthorityState::Denied));
    let mut failed = TextField::new();
    failed.set_state(ControlState::idle().with_authority(AuthorityState::FailedClosed));
    for (name, field) in [
        ("disabled", &disabled),
        ("denied", &denied),
        ("failed-closed", &failed),
    ] {
        let surface = field_surface(field, &theme);
        assert!(
            !has_pixel(&surface, premul(palette.document)),
            "a {name} field took the page and lost what it was stating"
        );
    }
}

#[test]
fn a_field_on_floating_chrome_lets_the_backdrop_through_whatever_its_ground() {
    // Both grounds go through the shared chrome-alpha path, so neither an
    // editable field's page nor a read-only field's recess lands as an opaque
    // patch on a frosted popup.
    let theme = Theme::light().floating();
    let palette = theme.palette();
    for (name, field) in [
        ("editable", TextField::new()),
        ("read-only", TextField::new().read_only(true)),
    ] {
        let ground = if field.is_read_only() {
            palette.surface
        } else {
            palette.document
        };
        let surface = field_surface(&field, &theme);
        assert!(
            has_pixel(
                &surface,
                premul(ground.with_alpha(palette.chrome_plate_alpha))
            ),
            "a {name} field on floating chrome is a plate on glass"
        );
        assert!(
            !has_pixel(&surface, premul(ground)),
            "a {name} field on floating chrome laid an opaque patch"
        );
    }
}

#[test]
fn search_renders_in_light_without_panic() {
    let theme = Theme::light();
    let search = SearchField::new().with_text("find");
    let surface = search_surface(&search, &theme);
    assert!(has_pixel(&surface, premul(theme.palette().on_surface)));
}

/// A theme identical to [`Theme::dark`] but with reduced motion requested.
fn reduced_motion() -> Theme {
    let base = Theme::dark();
    Theme::new(
        base.id(),
        "Test Reduced Motion",
        base.appearance(),
        *base.palette(),
        *base.metrics(),
        *base.fonts(),
        base.cursors().clone(),
        base.motion().with_reduced_motion(true),
        base.density(),
        base.contrast(),
    )
}

/// `key` pressed at `at_ns` with no modifier held.
fn stroke(key: Key, at_ns: u64) -> Keystroke {
    Keystroke {
        key,
        modifiers: NONE_MODS,
        at_ns,
    }
}

/// A focused masked field holding at most `max` characters.
fn masked(max: usize) -> SecretField {
    let mut field = SecretField::new(max);
    field.set_focused(true);
    field
}

/// Type `text` into `field`, every key at `at_ns`.
fn type_secret(field: &mut SecretField, text: &str, at_ns: u64) {
    for ch in text.chars() {
        field.on_key(
            stroke(Key::Char(ch), at_ns),
            bounds(),
            &Theme::dark(),
            &mut sink(),
        );
    }
}

/// Press `key` in `field` at `at_ns`, answering what it reported.
fn press_secret(field: &mut SecretField, key: Key, at_ns: u64) -> Option<TextAction> {
    field.on_key(stroke(key, at_ns), bounds(), &Theme::dark(), &mut sink())
}

fn masked_surface(field: &SecretField, theme: &Theme) -> Surface {
    let mut surface = Surface::new(W, H).expect("surface");
    field.render(&mut surface, bounds(), Scale::ONE, theme);
    surface
}

/// An unfocused masked field holding `typed`, drawn in `theme`.
fn drawn_holding(typed: &str, theme: &Theme) -> Surface {
    let mut field = masked(16);
    type_secret(&mut field, typed, 0);
    field.set_focused(false);
    masked_surface(&field, theme)
}

/// What a plain, unfocused field showing `text` draws: the reference a masked
/// field's marker is compared against.
fn plain_showing(text: &str, theme: &Theme) -> Surface {
    field_surface(&TextField::new().with_text(text), theme)
}

/// Keys past the bound were dropped silently and the prefix offered; a
/// credential that long can never be valid, so the entry is refused whole
/// until what was typed past the bound is erased again.
#[test]
fn typing_past_the_bound_refuses_the_whole_entry_until_it_is_erased() {
    let mut field = masked(4);
    type_secret(&mut field, "abcdef", 0);
    assert_eq!(field.secret(), None, "a prefix is never offered");
    assert!(!field.is_empty());
    press_secret(&mut field, Key::Named(NamedKey::Backspace), 1);
    assert_eq!(
        field.secret(),
        None,
        "one character is still past the bound"
    );
    press_secret(&mut field, Key::Named(NamedKey::Backspace), 2);
    assert_eq!(field.secret(), Some("abcd"));
    press_secret(&mut field, Key::Named(NamedKey::Backspace), 3);
    assert_eq!(field.secret(), Some("abc"), "then the buffer's own last");
    field.clear();
    assert_eq!(field.secret(), Some(""));
}

/// The bound is in bytes, the unit every wire and the verifier count, so a
/// wide character cannot carry an entry past what they hold.
#[test]
fn the_bound_counts_bytes_not_characters() {
    let mut field = masked(8);
    type_secret(&mut field, "😀😀", 0);
    assert_eq!(field.secret(), Some("😀😀"));
    type_secret(&mut field, "a", 1);
    assert_eq!(field.secret(), None, "nine bytes do not fit eight");
}

#[test]
fn filling_a_masked_field_to_its_limit_never_reallocates() {
    const LIMIT: usize = 64;
    let mut field = masked(LIMIT);
    let (before_ptr, before_cap) = debug_buffer_identity(&field);
    assert!(before_cap >= LIMIT, "the bound is reserved: {before_cap}");
    // Filled with the widest scalar UTF-8 encodes, then pushed past the
    // bound: a growth here would leave a copy of everything typed so far in
    // the block it moved out of.
    type_secret(&mut field, &"😀".repeat(LIMIT / 4), 0);
    assert_eq!(field.secret().map(str::len), Some(LIMIT));
    type_secret(&mut field, "😀x", 1);
    let (after_ptr, after_cap) = debug_buffer_identity(&field);
    assert_eq!(before_ptr, after_ptr, "the buffer never moved");
    assert_eq!(before_cap, after_cap, "…and never grew");
}

/// A derived clone copied the secret into a buffer only as long as it, so the
/// clone's next keystroke reallocated and freed an unerased copy.
#[test]
fn a_cloned_masked_field_keeps_its_reservation() {
    const LIMIT: usize = 32;
    let mut field = masked(LIMIT);
    type_secret(&mut field, "pw", 0);
    let mut copy = field.clone();
    let (before_ptr, before_cap) = debug_buffer_identity(&copy);
    assert!(before_cap >= LIMIT, "{before_cap}");
    type_secret(&mut copy, &"😀".repeat((LIMIT - 2) / 4), 1);
    assert_eq!(
        debug_buffer_identity(&copy),
        (before_ptr, before_cap),
        "filling the copy never moved its buffer"
    );
    assert_eq!(field.secret(), Some("pw"), "the original is untouched");
}

/// Backspace shifted nothing out of the buffer's bytes: a removal leaves a
/// copy of the tail past the new end, where no later erase reaches.
#[test]
fn closing_a_gap_erases_every_position_the_tail_vacates() {
    let mut bytes = *b"hunter2";
    assert_eq!(close_gap(&mut bytes, 4..7), 4, "a removal at the end");
    assert_eq!(&bytes, b"hunt\0\0\0");
    let mut bytes = *b"abcdef";
    assert_eq!(close_gap(&mut bytes, 1..3), 4, "a removal mid-buffer");
    assert_eq!(&bytes, b"adef\0\0", "the moved tail's old copy is gone");
    let mut bytes = *b"abc";
    assert_eq!(
        close_gap(&mut bytes, 2..2),
        3,
        "an empty gap removes nothing"
    );
    assert_eq!(close_gap(&mut bytes, 2..9), 3, "an impossible one neither");
    assert_eq!(&bytes, b"abc");
}

#[test]
fn a_paste_replaces_the_selection_as_typing_it_would() {
    let mut field = TextField::new().with_text("hello world").with_max_len(14);
    field.set_focused(true);
    for _ in 0..5 {
        field.on_key(Key::Named(NamedKey::Left), SHIFT, bounds(), &mut sink());
    }
    assert_eq!(field.selected_text(), Some("world"));
    let mut damage = sink();
    assert_eq!(
        field.insert_text("the\nwide\tsea!", bounds(), &mut damage),
        Some(TextAction::Edited)
    );
    assert_eq!(
        field.text(),
        "hello thewides",
        "control characters dropped, cut at the limit"
    );
    assert!(!damage.is_empty());
    assert_eq!(
        field.selected_text(),
        None,
        "the caret follows what went in"
    );
    field.on_key(Key::Char('a'), CTRL, bounds(), &mut sink());
    assert_eq!(
        field.delete_selection(bounds(), &mut sink()),
        Some(TextAction::Edited)
    );
    assert_eq!(field.text(), "");
    assert_eq!(
        field.insert_text("\n\r", bounds(), &mut sink()),
        None,
        "nothing to insert"
    );
}

#[test]
fn a_read_only_field_copies_but_takes_nothing() {
    let mut field = TextField::new().with_text("fixed").read_only(true);
    field.set_focused(true);
    field.on_key(Key::Char('a'), CTRL, bounds(), &mut sink());
    assert_eq!(field.selected_text(), Some("fixed"));
    assert_eq!(field.insert_text("x", bounds(), &mut sink()), None);
    assert_eq!(field.delete_selection(bounds(), &mut sink()), None);
    assert_eq!(field.text(), "fixed");
}

#[test]
fn a_search_field_takes_a_paste_too() {
    let mut field = SearchField::new().with_text("ab");
    field.set_focused(true);
    field.on_key(Key::Char('a'), CTRL, bounds(), &mut sink());
    assert_eq!(field.selected_text(), Some("ab"));
    assert_eq!(
        field.insert_text("cd", bounds(), &mut sink()),
        Some(TextAction::Edited)
    );
    assert_eq!(field.text(), "cd");
}

#[test]
fn zeroize_range_overwrites_its_bytes_without_changing_the_length() {
    let mut text = String::from("abcdef");
    zeroize_range(&mut text, 2..4);
    assert_eq!(text.as_bytes(), &b"ab\0\0ef"[..]);
    assert_eq!(text.len(), 6, "the erase writes in place");
}

#[test]
fn dropping_a_filled_masked_field_erases_its_buffer() {
    let mut field = masked(12);
    type_secret(&mut field, "hunter2", 0);
    assert_eq!(debug_bytes(&field).as_slice(), &b"hunter2"[..]);
    // Dropping the field runs exactly this erase on its way out. A released
    // allocation cannot be read back in a crate that forbids `unsafe`, so the
    // erase is asserted here on the live buffer, through the very method the
    // drop calls, and the field is then dropped for real.
    debug_zeroize(&mut field);
    let erased = debug_bytes(&field);
    assert_eq!(erased.len(), 7, "the erase leaves the length alone");
    assert!(erased.iter().all(|&b| b == 0), "…and no byte survives it");
    drop(field);
}

#[test]
fn the_first_edit_after_submission_begins_a_new_secret() {
    let mut field = masked(12);
    type_secret(&mut field, "hunter2", 0);
    assert_eq!(
        press_secret(&mut field, Key::Named(NamedKey::Enter), 1),
        Some(TextAction::Submitted)
    );
    assert_eq!(
        field.secret(),
        Some("hunter2"),
        "the owner reads what was submitted"
    );
    type_secret(&mut field, "pw", 2);
    assert_eq!(debug_bytes(&field).as_slice(), &b"pw"[..]);
    assert_eq!(
        field.secret(),
        Some("pw"),
        "the submitted credential is gone, not merely hidden behind a shorter one"
    );
    field.set_focused(false);
    let theme = Theme::dark();
    assert_eq!(
        masked_surface(&field, &theme).pixels(),
        plain_showing("[input active.]", &theme).pixels(),
        "the new secret wears a fresh marker"
    );
}

#[test]
fn backspace_after_submission_discards_the_secret() {
    let mut field = masked(12).with_placeholder("Password");
    type_secret(&mut field, "pw", 0);
    press_secret(&mut field, Key::Named(NamedKey::Enter), 1);
    assert_eq!(
        press_secret(&mut field, Key::Named(NamedKey::Backspace), 2),
        Some(TextAction::Edited)
    );
    assert!(field.is_empty());
    field.set_focused(false);
    let theme = Theme::dark();
    assert_eq!(
        masked_surface(&field, &theme).pixels(),
        field_surface(&TextField::new().with_placeholder("Password"), &theme).pixels(),
        "an empty field shows its placeholder and no marker"
    );
}

/// A dump of the length, or of a caret sitting at the end, said as much as
/// the marker hides.
#[test]
fn a_masked_fields_debug_output_redacts_its_buffer_and_its_length() {
    let mut field = masked(16);
    type_secret(&mut field, "hunter2", 0);
    let dump = format!("{field:?}");
    assert!(
        !dump.contains("hunter2"),
        "a debug dump must not carry the credential: {dump}"
    );
    assert!(dump.contains("<redacted>"), "{dump}");
    let mut other = masked(16);
    type_secret(&mut other, "x", 0);
    assert_eq!(
        format!("{other:?}"),
        dump,
        "a one-character secret dumps exactly as a seven-character one"
    );
}

/// Equality derived over the plaintext compared what is never drawn, so two
/// fields showing the same marker compared unequal.
#[test]
fn masked_fields_compare_by_what_they_draw_not_by_what_they_hold() {
    let mut short = masked(16);
    type_secret(&mut short, "a", 0);
    let mut long = masked(16);
    type_secret(&mut long, "abc", 0);
    assert_eq!(short, long);
    let empty = masked(16);
    assert_ne!(short, empty, "the marker is drawn, and differs");
}

/// Under reduced motion the marker kept its armed deadline, so turning motion
/// back on replayed every tick since in one burst.
#[test]
fn a_keystroke_under_reduced_motion_leaves_no_deadline_to_replay() {
    let mut field = masked(16);
    field.on_key(
        stroke(Key::Char('a'), 0),
        bounds(),
        &reduced_motion(),
        &mut sink(),
    );
    assert_eq!(field.deadline_ns(), None);
    let later = 3_600 * SECRET_TICK_NS;
    field.on_key(
        stroke(Key::Char('b'), later),
        bounds(),
        &Theme::dark(),
        &mut sink(),
    );
    assert_eq!(field.deadline_ns(), Some(later + SECRET_TICK_NS));
    let shown = masked_surface(&field, &Theme::dark());
    assert!(!field.advance(later + SECRET_TICK_NS - 1));
    assert_eq!(
        masked_surface(&field, &Theme::dark()).pixels(),
        shown.pixels()
    );
}

#[test]
fn a_plain_fields_debug_output_still_shows_its_text() {
    let field = TextField::new().with_text("hunter2");
    assert!(format!("{field:?}").contains("hunter2"));
}

#[test]
fn a_masked_field_draws_the_shared_marker_and_nothing_typed() {
    let theme = Theme::dark();
    assert_eq!(
        drawn_holding("WWWW", &theme).pixels(),
        plain_showing("[input active.]", &theme).pixels(),
        "the first keystroke puts up the text-mode prompt's own marker"
    );
    assert_ne!(
        drawn_holding("WWWW", &theme).pixels(),
        plain_showing("WWWW", &theme).pixels()
    );
}

#[test]
fn a_masked_fields_render_depends_on_neither_what_nor_how_much_it_holds() {
    let theme = Theme::dark();
    let reference = drawn_holding("i", &theme);
    for typed in ["W", "iiii", "WWWWWWWWWWWWWWWW", "😀😀😀"] {
        assert_eq!(
            drawn_holding(typed, &theme).pixels(),
            reference.pixels(),
            "{} characters drew differently from one",
            typed.chars().count()
        );
    }
}

#[test]
fn an_empty_masked_field_still_shows_its_placeholder() {
    let theme = Theme::dark();
    let muted = premul(theme.palette().on_surface_muted);
    let mut field = SecretField::new(8).with_placeholder("Password");
    assert!(
        has_pixel(&masked_surface(&field, &theme), muted),
        "a placeholder is not a secret"
    );
    field.set_focused(true);
    type_secret(&mut field, "pw", 0);
    assert!(
        !has_pixel(&masked_surface(&field, &theme), muted),
        "…and it gives way once there is something to hide"
    );
}

#[test]
fn a_plain_fields_caret_stands_at_the_measured_width_of_the_text_before_it() {
    let theme = Theme::dark();
    let font = font();
    let caret = premul(theme.palette().on_surface);
    let mut field = TextField::new().with_text("iMxW");
    field.set_focused(true);
    field.on_key(Key::Named(NamedKey::Home), NONE_MODS, bounds(), &mut sink());
    // The text origin is where an empty field's caret stands, so the two
    // together pin the caret against the face's own measurement rather than
    // against a figure this test picked.
    let surface = field_surface(&field, &theme);
    let origin = (0..W)
        .find(|&x| surface.get(x, 0) == Some(caret))
        .expect("a focused field draws its caret");
    for before in ["i", "iM", "iMx", "iMxW"] {
        field.on_key(
            Key::Named(NamedKey::Right),
            NONE_MODS,
            bounds(),
            &mut sink(),
        );
        assert_eq!(
            field_surface(&field, &theme).get(origin + font.text_width(before), 0),
            Some(caret),
            "the caret after {before:?} stands at that text's own width"
        );
    }
}

#[test]
fn a_masked_fields_caret_stands_after_the_marker() {
    let theme = Theme::dark();
    let caret = premul(theme.palette().on_surface);
    let mut field = masked(8);
    // The caret spans the whole row while a glyph only covers its middle, so
    // the field's top row shows the caret alone.
    let origin = (0..W)
        .find(|&x| masked_surface(&field, &theme).get(x, 0) == Some(caret))
        .expect("a focused empty field draws its caret at the text origin");
    type_secret(&mut field, "abc", 0);
    assert_eq!(
        masked_surface(&field, &theme).get(origin + font().text_width("[input active.]"), 0),
        Some(caret)
    );
}

#[test]
fn a_masked_field_takes_no_caret_or_selection_key() {
    let mut field = masked(8);
    type_secret(&mut field, "abc", 0);
    for (key, modifiers) in [
        (Key::Named(NamedKey::Left), NONE_MODS),
        (Key::Named(NamedKey::Home), NONE_MODS),
        (Key::Named(NamedKey::Left), SHIFT),
        (Key::Named(NamedKey::Delete), NONE_MODS),
        (Key::Char('a'), CTRL),
        (Key::Named(NamedKey::Right), NONE_MODS),
        (Key::Named(NamedKey::End), NONE_MODS),
    ] {
        let at = Keystroke {
            key,
            modifiers,
            at_ns: 1,
        };
        assert_eq!(
            field.on_key(at, bounds(), &Theme::dark(), &mut sink()),
            None,
            "{key:?}"
        );
        assert_eq!(field.secret(), Some("abc"), "{key:?} changed nothing");
    }
    type_secret(&mut field, "d", 2);
    assert_eq!(field.secret(), Some("abcd"), "the caret never left the end");
    assert_eq!(
        press_secret(&mut field, Key::Named(NamedKey::Backspace), 3),
        Some(TextAction::Edited)
    );
    assert_eq!(
        field.secret(),
        Some("abc"),
        "Backspace erases the last character"
    );
    assert_eq!(
        press_secret(&mut field, Key::Named(NamedKey::Escape), 4),
        Some(TextAction::Cancelled)
    );
}

#[test]
fn a_press_in_a_masked_field_places_no_caret() {
    let theme = Theme::dark();
    let mut field = masked(8);
    type_secret(&mut field, "abcde", 0);
    field.on_pointer(&moved(12, 14), bounds(), Scale::ONE, &theme, &mut sink());
    field.on_pointer(&PRESS, bounds(), Scale::ONE, &theme, &mut sink());
    field.on_pointer(&moved(120, 14), bounds(), Scale::ONE, &theme, &mut sink());
    field.on_pointer(&RELEASE, bounds(), Scale::ONE, &theme, &mut sink());
    type_secret(&mut field, "Z", 1);
    assert_eq!(
        field.secret(),
        Some("abcdeZ"),
        "a press and a drag moved and selected nothing"
    );
}

#[test]
fn the_marker_moves_on_the_text_mode_cadence_and_then_stands_still() {
    let theme = Theme::dark();
    let mut field = masked(8);
    type_secret(&mut field, "p", 5);
    assert_eq!(field.deadline_ns(), Some(5 + SECRET_TICK_NS));
    assert!(
        !field.advance(4 + SECRET_TICK_NS),
        "nothing is due before the frame"
    );
    for (frame, shown) in [(1, "[input active..]"), (2, "[input active...]")] {
        assert!(
            field.advance(5 + frame * SECRET_TICK_NS),
            "frame {frame} redraws the field"
        );
        let mut drawn = field.clone();
        drawn.set_focused(false);
        assert_eq!(
            masked_surface(&drawn, &theme).pixels(),
            plain_showing(shown, &theme).pixels(),
            "frame {frame}"
        );
    }
    assert!(
        !field.advance(5 + 3 * SECRET_TICK_NS),
        "the freeze changes no pixel"
    );
    assert_eq!(field.deadline_ns(), None, "and arms nothing further");
}

#[test]
fn a_late_advance_catches_the_marker_up_to_the_frame_it_owes() {
    let mut field = masked(8);
    type_secret(&mut field, "p", 0);
    assert!(field.advance(60 * SECRET_TICK_NS));
    assert_eq!(field.deadline_ns(), None, "a window long past has frozen");
}

#[test]
fn a_keystroke_extends_the_window_the_dots_move_in() {
    let mut field = masked(8);
    type_secret(&mut field, "p", 0);
    type_secret(&mut field, "w", 2 * SECRET_TICK_NS);
    field.advance(3 * SECRET_TICK_NS);
    assert!(
        field.deadline_ns().is_some(),
        "the frame that would have frozen the first window still moves"
    );
}

#[test]
fn reduced_motion_keeps_the_marker_still_and_arms_no_frame() {
    let theme = reduced_motion();
    let mut field = masked(8);
    field.on_key(stroke(Key::Char('p'), 0), bounds(), &theme, &mut sink());
    assert_eq!(field.deadline_ns(), None);
    assert!(!field.advance(10 * SECRET_TICK_NS));
    field.set_focused(false);
    assert_eq!(
        masked_surface(&field, &theme).pixels(),
        plain_showing("[input active.]", &theme).pixels()
    );
}

#[test]
fn enter_submits_and_the_marker_says_so() {
    let theme = Theme::dark();
    let mut field = masked(8);
    type_secret(&mut field, "pw", 0);
    let mut damage = sink();
    assert_eq!(
        field.on_key(
            stroke(Key::Named(NamedKey::Enter), 1),
            bounds(),
            &theme,
            &mut damage
        ),
        Some(TextAction::Submitted)
    );
    assert_eq!(damage.bounds(), bounds());
    assert_eq!(field.deadline_ns(), None);
    field.set_focused(false);
    assert_eq!(
        masked_surface(&field, &theme).pixels(),
        plain_showing("[input complete]", &theme).pixels()
    );
}

#[test]
fn erasing_back_to_empty_takes_the_marker_down() {
    let theme = Theme::dark();
    let mut field = masked(8);
    type_secret(&mut field, "ab", 0);
    press_secret(&mut field, Key::Named(NamedKey::Backspace), 1);
    press_secret(&mut field, Key::Named(NamedKey::Backspace), 2);
    assert!(field.is_empty());
    assert_eq!(field.deadline_ns(), None);
    field.set_focused(false);
    assert_eq!(
        masked_surface(&field, &theme).pixels(),
        field_surface(&TextField::new(), &theme).pixels()
    );
}

#[test]
fn clearing_a_masked_field_erases_it_and_takes_the_marker_down() {
    let mut field = masked(8);
    type_secret(&mut field, "hunter2", 0);
    press_secret(&mut field, Key::Named(NamedKey::Enter), 1);
    field.clear();
    assert!(field.is_empty());
    assert!(debug_bytes(&field).is_empty());
    field.set_focused(false);
    let theme = Theme::dark();
    assert_eq!(
        masked_surface(&field, &theme).pixels(),
        field_surface(&TextField::new(), &theme).pixels(),
        "a cleared field shows neither marker, not even the completed one"
    );
}

#[test]
fn a_masked_field_draws_its_marker_in_dark_light_and_high_contrast() {
    for theme in [Theme::dark(), Theme::light(), high_contrast()] {
        assert_eq!(
            drawn_holding("pw", &theme).pixels(),
            plain_showing("[input active.]", &theme).pixels(),
            "every theme draws the marker in its own foreground over the same plate"
        );
    }
}

// --- Render-equivalence equality (the host's repaint gate) ----------------

/// Two samples clear of the field, so only the recorded coordinate differs.
const OFF_A: (i32, i32) = (400, 60);
const OFF_B: (i32, i32) = (460, 70);

#[test]
fn pointer_position_alone_never_changes_a_text_field_render() {
    let theme = Theme::dark();
    let mut a = TextField::new().with_text("hello");
    let mut b = a.clone();
    a.on_pointer(
        &moved(OFF_A.0, OFF_A.1),
        bounds(),
        Scale::ONE,
        &theme,
        &mut sink(),
    );
    b.on_pointer(
        &moved(OFF_B.0, OFF_B.1),
        bounds(),
        Scale::ONE,
        &theme,
        &mut sink(),
    );

    assert_eq!(
        a, b,
        "where the pointer last was is hit-testing state; the caret it \
         places lives in the editor and is still compared"
    );
    let sa = field_surface(&a, &theme);
    let sb = field_surface(&b, &theme);
    assert_eq!(
        sa.pixels(),
        sb.pixels(),
        "…and the two must therefore paint identically"
    );
}

#[test]
fn selection_drag_latch_alone_never_changes_a_text_field_render() {
    let theme = Theme::dark();
    // An empty field maps every press to byte zero, so the press moves no
    // caret and creates no selection: the drag latch is the only difference.
    let mut dragging = TextField::new();
    dragging.on_pointer(&moved(4, 14), bounds(), Scale::ONE, &theme, &mut sink());
    dragging.on_pointer(&PRESS, bounds(), Scale::ONE, &theme, &mut sink());

    let mut shown = TextField::new();
    shown.on_pointer(&moved(4, 14), bounds(), Scale::ONE, &theme, &mut sink());
    let mut pressed = ControlState::idle();
    pressed.pointer = crate::state::PointerState::Pressed;
    shown.set_state(pressed);

    assert_eq!(
        dragging, shown,
        "whether a press is still extending a selection is bookkeeping"
    );
    let sa = field_surface(&dragging, &theme);
    let sb = field_surface(&shown, &theme);
    assert_eq!(
        sa.pixels(),
        sb.pixels(),
        "…and the two must therefore paint identically"
    );
}

#[test]
fn pointer_position_alone_never_changes_a_search_field_render() {
    let theme = Theme::dark();
    let mut a = SearchField::new();
    let mut b = a.clone();
    a.on_pointer(
        &moved(OFF_A.0, OFF_A.1),
        bounds(),
        Scale::ONE,
        &theme,
        &mut sink(),
    );
    b.on_pointer(
        &moved(OFF_B.0, OFF_B.1),
        bounds(),
        Scale::ONE,
        &theme,
        &mut sink(),
    );

    assert_eq!(a, b);
    let sa = search_surface(&a, &theme);
    let sb = search_surface(&b, &theme);
    assert_eq!(sa.pixels(), sb.pixels());
}

#[test]
fn hover_and_typing_each_change_a_text_field_render() {
    let theme = Theme::dark();
    let resting = TextField::new();

    let mut hovered = resting.clone();
    hovered.on_pointer(&moved(4, 14), bounds(), Scale::ONE, &theme, &mut sink());
    assert_ne!(resting, hovered, "a hover highlight is visible");

    let typed = TextField::new().with_text("a");
    assert_ne!(resting, typed, "the text is visible");
}

/// A caret move repaints the field; a submit draws nothing of its own.
#[test]
fn a_caret_move_reports_and_a_submit_does_not() {
    let mut field = TextField::new().with_text("ab");
    field.set_focused(true);
    let mut moved_caret = sink();
    field.on_key(
        Key::Named(NamedKey::Left),
        NONE_MODS,
        bounds(),
        &mut moved_caret,
    );
    assert_eq!(moved_caret.bounds(), bounds(), "the caret is drawn");

    let mut submitted = sink();
    field.on_key(
        Key::Named(NamedKey::Enter),
        NONE_MODS,
        bounds(),
        &mut submitted,
    );
    assert!(submitted.is_empty(), "submitting changes no pixel here");

    let mut at_start = sink();
    field.on_key(
        Key::Named(NamedKey::Home),
        NONE_MODS,
        bounds(),
        &mut at_start,
    );
    let mut again = sink();
    field.on_key(Key::Named(NamedKey::Home), NONE_MODS, bounds(), &mut again);
    assert!(!at_start.is_empty(), "the first Home moved the caret");
    assert!(again.is_empty(), "the second had nowhere to move it");
}

/// A masked field reports an edit only where the marker changed. Reporting
/// every keystroke made the presents it caused count the characters the
/// marker hides.
#[test]
fn a_masked_field_reports_only_the_edits_that_change_its_marker() {
    let theme = Theme::dark();
    let mut field = masked(16);
    let mut first = sink();
    field.on_key(stroke(Key::Char('p'), 0), bounds(), &theme, &mut first);
    assert_eq!(first.bounds(), bounds(), "the marker went up");
    for (at, ch) in (1..).zip("wxyz".chars()) {
        let mut again = sink();
        field.on_key(stroke(Key::Char(ch), at), bounds(), &theme, &mut again);
        assert!(again.is_empty(), "{ch:?} changed nothing drawn");
    }
    let mut enter = sink();
    field.on_key(
        stroke(Key::Named(NamedKey::Enter), 9),
        bounds(),
        &theme,
        &mut enter,
    );
    assert_eq!(enter.bounds(), bounds(), "[input complete] is drawn");
}

/// A placeholder too long for the field is elided with the shared mark rather
/// than cut where the field ran out.
#[test]
fn a_placeholder_too_long_for_the_field_is_elided_with_the_mark() {
    let theme = Theme::dark();
    assert!(
        marks_elision(|text| field_surface(&TextField::new().with_placeholder(text), &theme)),
        "a text field"
    );
    assert!(
        marks_elision(|text| search_surface(&SearchField::new().with_placeholder(text), &theme)),
        "a search field"
    );
}

#[test]
fn a_paste_the_allocator_cannot_hold_is_refused_rather_than_aborting() {
    let mut field = TextField::new();
    assert!(!crate::text::debug_fits(&mut field, usize::MAX));
    assert!(crate::text::debug_fits(&mut field, 16));
}
