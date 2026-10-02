//! Unit tests for the in-window settings sheet.
//!
//! Every geometric probe reads the sheet's *own* layout (`panel_bounds`,
//! `bands`, `resolve`, `split_row`, `footer_split`) rather than restating it,
//! so a test can never assert against a rectangle the sheet does not actually
//! draw or hit-test.

use alloc::vec::Vec;

use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
use tairix_controls::{damage, ScrollModel, WHEEL_STEP};
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::Surface;
use tairix_theme::Theme;

use crate::effects::{EffectKey, Effects, FULL, MIN_OPACITY};
use crate::profile::{Profile, MAX_FONT_SIZE_PX, MIN_FONT_SIZE_PX};
use crate::scheme::{Rgb, Scheme};

use super::{
    footer_split, panel_bounds, split_row, Focus, Layout, Settings, SheetOutcome, Style,
    EFFECTS_TAB,
};

const SCALE: Scale = Scale::ONE;

/// The client rectangle a 640x480 screen leaves once window furniture is
/// taken — the smallest screen the sheet must stay usable on.
const CLIENT: Rect = Rect::new(0, 0, 608, 435);

/// A viewport far too small for the sheet's rows.
const TINY: Rect = Rect::new(0, 0, 120, 80);

const PRESS: InputEvent = InputEvent::PointerPressed {
    button: PointerButton::Primary,
};

const RELEASE: InputEvent = InputEvent::PointerReleased {
    button: PointerButton::Primary,
};

fn theme() -> Theme {
    Theme::dark()
}

fn sheet() -> Settings {
    Settings::new(&Profile::default())
}

fn moved(at: Point) -> InputEvent {
    InputEvent::PointerMoved { to: at }
}

fn surface(viewport: Rect) -> Surface {
    Surface::new(viewport.width, viewport.height).expect("a test surface")
}

// --- Probes onto the sheet's own layout -----------------------------------

/// The sheet's four content bands for `viewport`.
fn bands(
    sheet: &Settings,
    viewport: Rect,
) -> (Option<Rect>, Option<Rect>, Option<Rect>, Option<Rect>) {
    let bounds = panel_bounds(viewport, SCALE);
    let content = sheet
        .panel
        .content_rect(bounds, SCALE, &theme())
        .expect("the panel has a content rectangle");
    sheet.bands(content, SCALE, &theme())
}

/// The scrollable body band for `viewport`.
fn body(sheet: &Settings, viewport: Rect) -> Rect {
    bands(sheet, viewport).1.expect("the body band is laid out")
}

/// The sheet's own resolution of `viewport`: where it draws every part, and
/// the scroll model it holds there.
fn resolved(sheet: &Settings, viewport: Rect) -> (Layout, ScrollModel) {
    let theme = theme();
    let font = Style::new(SCALE, &theme).font;
    let (layout, model) = sheet.resolve(viewport, SCALE, &theme, font);
    (layout.expect("the panel has a content rectangle"), model)
}

/// How far the body is scrolled.
fn offset(sheet: &Settings, viewport: Rect) -> u64 {
    resolved(sheet, viewport).1.offset()
}

/// Where `row` shows in the sheet — the part the body's edge leaves of it —
/// or `None` when it shows nowhere.
fn row_rect(sheet: &Settings, viewport: Rect, row: Focus) -> Option<Rect> {
    Some(resolved(sheet, viewport).0.rect_of(row)).filter(|rect| !rect.is_empty())
}

/// Where `row` shows, insisting some of it does.
fn visible_row(sheet: &Settings, viewport: Rect, row: Focus) -> Rect {
    row_rect(sheet, viewport, row).unwrap_or_else(|| panic!("{row:?} shows in the body"))
}

/// Where `row` lies in the rows' own unscrolled layout.
fn laid_out(sheet: &Settings, viewport: Rect, row: Focus) -> Rect {
    resolved(sheet, viewport)
        .0
        .laid_out(row)
        .unwrap_or_else(|| panic!("{row:?} is a row of the active tab"))
}

/// Turn the wheel by `units` scroll units with the pointer at `at`.
fn wheel_at(
    sheet: &mut Settings,
    viewport: Rect,
    at: Point,
    units: i32,
    damage: &mut Region,
) -> SheetOutcome {
    sheet.on_pointer(&moved(at), viewport, SCALE, &theme(), &mut damage::sink());
    sheet.on_pointer(
        &InputEvent::PointerScrolled { dx: 0, dy: units },
        viewport,
        SCALE,
        &theme(),
        damage,
    )
}

/// The pixels one wheel detent scrolls the body at [`SCALE`].
fn detent_px() -> u64 {
    u64::from(SCALE.scale_length(WHEEL_STEP))
}

/// Scroll the body to `to` pixels with the wheel, turned over the bar so no
/// row is left hovered.
fn wheel_to(sheet: &mut Settings, viewport: Rect, to: u64) {
    let bar = centre(
        bands(sheet, viewport)
            .2
            .expect("the scrollbar band is laid out"),
    );
    let from = offset(sheet, viewport);
    let pixels = i64::try_from(to).expect("a sane offset") - i64::try_from(from).expect("sane");
    let per_detent = i64::from(SCROLL_UNITS_PER_DETENT);
    let units = i32::try_from(pixels * per_detent / i64::try_from(detent_px()).expect("sane"))
        .expect("a sane turn");
    wheel_at(sheet, viewport, bar, units, &mut damage::sink());
    assert_eq!(
        offset(sheet, viewport),
        to,
        "the wheel scrolled exactly there"
    );
}

/// The *Restore defaults* and *Done* button rectangles.
fn footer_buttons(sheet: &Settings, viewport: Rect) -> (Rect, Rect) {
    let footer = bands(sheet, viewport)
        .3
        .expect("the footer band is laid out");
    let (restore, done) = footer_split(footer, SCALE);
    (
        restore.expect("the restore button is laid out"),
        done.expect("the done button is laid out"),
    )
}

/// A point `permille` of the way along a slider row's control column.
fn slider_point(row: Rect, permille: u32) -> Point {
    let (_, control) = split_row(row, SCALE);
    let along = to_i32(control.width.saturating_mul(permille.min(1000)) / 1000);
    let x = (control.left() + along).min(control.right() - 1);
    Point::new(x, row.top() + to_i32(row.height) / 2)
}

/// Whether every pixel of `rect` lies in one of the rectangles `reported`
/// holds — not merely inside their bounding box, which a report of two far
/// corners would span.
fn covers(reported: &Region, rect: Rect) -> bool {
    let mut uncovered = Region::new();
    uncovered.add(rect);
    for part in reported.rects() {
        uncovered.subtract(*part);
    }
    uncovered.is_empty()
}

/// How many of `outcomes` are `wanted`.
fn count(outcomes: &[SheetOutcome], wanted: SheetOutcome) -> usize {
    outcomes.iter().filter(|got| **got == wanted).count()
}

/// The centre of `rect`.
fn centre(rect: Rect) -> Point {
    Point::new(
        rect.left() + to_i32(rect.width) / 2,
        rect.top() + to_i32(rect.height) / 2,
    )
}

// --- Gestures --------------------------------------------------------------

/// Move the pointer to `at` and press, reporting the press outcome — the
/// gesture a slider commits on.
fn press_at(sheet: &mut Settings, viewport: Rect, at: Point) -> SheetOutcome {
    sheet.on_pointer(&moved(at), viewport, SCALE, &theme(), &mut damage::sink());
    sheet.on_pointer(&PRESS, viewport, SCALE, &theme(), &mut damage::sink())
}

/// A complete primary click at `at`, reporting the release outcome — the
/// gesture a radio or button commits on.
fn click_at(sheet: &mut Settings, viewport: Rect, at: Point) -> SheetOutcome {
    press_at(sheet, viewport, at);
    sheet.on_pointer(&RELEASE, viewport, SCALE, &theme(), &mut damage::sink())
}

/// One key press with no modifiers, reporting into `damage`.
fn key_into(sheet: &mut Settings, viewport: Rect, key: Key, damage: &mut Region) -> SheetOutcome {
    sheet.on_key(key, Modifiers::default(), viewport, SCALE, &theme(), damage)
}

/// One key press with no modifiers, for a test that does not read the report.
fn key(sheet: &mut Settings, viewport: Rect, key: Key) -> SheetOutcome {
    key_into(sheet, viewport, key, &mut damage::sink())
}

/// One Shift-modified key press.
fn shift_key(sheet: &mut Settings, viewport: Rect, key: Key) -> SheetOutcome {
    let modifiers = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    sheet.on_key(
        key,
        modifiers,
        viewport,
        SCALE,
        &theme(),
        &mut damage::sink(),
    )
}

/// Tab forward until `target` holds focus.
fn focus_on(sheet: &mut Settings, viewport: Rect, target: Focus) {
    for _ in 0..=sheet.focus_order().len() {
        if sheet.focus == target {
            return;
        }
        key(sheet, viewport, Key::Named(NamedKey::Tab));
    }
    panic!("Tab traversal never reached {target:?}");
}

/// Select the Effects tab from the keyboard alone (focus opens on the strip).
fn select_effects_tab(sheet: &mut Settings, viewport: Rect) {
    focus_on(sheet, viewport, Focus::Tabs);
    key(sheet, viewport, Key::Named(NamedKey::Right));
    key(sheet, viewport, Key::Named(NamedKey::Enter));
    assert_eq!(sheet.tabs.selected(), Some(EFFECTS_TAB));
}

/// Every effect value, in [`EffectKey::ALL`] order.
fn effect_values(effects: Effects) -> [u16; EffectKey::COUNT] {
    EffectKey::ALL.map(|key| key.of(effects))
}

// --- Rendering -------------------------------------------------------------

#[test]
fn renders_at_the_small_screen_client_budget() {
    let sheet = sheet();
    let mut surface = surface(CLIENT);
    sheet.render(&mut surface, CLIENT, SCALE, &theme());
    assert!(surface.pixels().iter().any(|pixel| pixel.a > 0));
}

#[test]
fn renders_at_a_tiny_viewport_without_panicking() {
    let sheet = sheet();
    let mut surface = surface(TINY);
    sheet.render(&mut surface, TINY, SCALE, &theme());
}

#[test]
fn renders_the_effects_tab_at_both_viewports() {
    for viewport in [CLIENT, TINY] {
        let mut sheet = sheet();
        select_effects_tab(&mut sheet, viewport);
        let mut surface = surface(viewport);
        sheet.render(&mut surface, viewport, SCALE, &theme());
    }
}

#[test]
fn renders_under_the_light_theme_too() {
    let sheet = sheet();
    let mut surface = surface(CLIENT);
    sheet.render(&mut surface, CLIENT, SCALE, &Theme::light());
    assert!(surface.pixels().iter().any(|pixel| pixel.a > 0));
}

#[test]
fn the_panel_is_inset_from_a_viewport_larger_than_it() {
    let wide = Rect::new(0, 0, 1280, 900);
    let bounds = panel_bounds(wide, SCALE);
    assert!(
        bounds.left() > wide.left(),
        "the panel leaves a margin to click out of"
    );
    assert!(bounds.right() < wide.right());
    assert!(bounds.top() > wide.top());
    assert!(bounds.bottom() < wide.bottom());
}

// --- Appearance: the scheme choice ----------------------------------------

#[test]
fn a_scheme_radio_puts_that_scheme_in_force() {
    let mut sheet = sheet();
    let wanted = Scheme::ALL[1];
    assert_ne!(
        sheet.profile().scheme,
        wanted,
        "the test must actually change it"
    );

    let row = visible_row(&sheet, CLIENT, Focus::Scheme(1));
    assert_eq!(
        click_at(&mut sheet, CLIENT, centre(row)),
        SheetOutcome::Settled,
        "a chosen radio is one whole interaction, so it settles"
    );
    assert_eq!(sheet.profile().scheme, wanted);
    assert!(sheet.scheme_radios[1].is_selected());
    assert!(!sheet.scheme_radios[0].is_selected());
}

#[test]
fn every_scheme_is_offered_as_its_own_radio() {
    let sheet = sheet();
    assert_eq!(sheet.scheme_radios.len(), Scheme::ALL.len());
    for (radio, scheme) in sheet.scheme_radios.iter().zip(Scheme::ALL) {
        assert_eq!(radio.label(), scheme.label());
    }
}

// --- Appearance: the text size ---------------------------------------------

#[test]
fn the_text_size_slider_edits_the_font_size_within_its_bounds() {
    let mut sheet = sheet();
    focus_on(&mut sheet, CLIENT, Focus::TextSize);

    assert_eq!(
        key(&mut sheet, CLIENT, Key::Named(NamedKey::End)),
        SheetOutcome::Settled
    );
    assert_eq!(sheet.profile().font_size_px, MAX_FONT_SIZE_PX);

    assert_eq!(
        key(&mut sheet, CLIENT, Key::Named(NamedKey::Home)),
        SheetOutcome::Settled
    );
    assert_eq!(sheet.profile().font_size_px, MIN_FONT_SIZE_PX);

    assert_eq!(
        key(&mut sheet, CLIENT, Key::Named(NamedKey::Right)),
        SheetOutcome::Settled
    );
    let stepped = sheet.profile().font_size_px;
    assert!(
        (MIN_FONT_SIZE_PX..=MAX_FONT_SIZE_PX).contains(&stepped) && stepped > MIN_FONT_SIZE_PX,
        "one line step moves the size up and stays in range, got {stepped}"
    );
}

/// The regression the live/settled split exists for: dragging a slider changes
/// the profile on every sample and asks to be **written** only when the drag
/// ends. Reporting a settled edit per sample cost one IPC round trip to the
/// configuration service and one disk commit per pointer motion, with the
/// window frozen for each of them.
#[test]
fn dragging_the_text_size_settles_once_however_many_samples_it_takes() {
    let mut sheet = sheet();
    let row = visible_row(&sheet, CLIENT, Focus::TextSize);
    let mut outcomes = alloc::vec::Vec::new();
    outcomes.push(press_at(&mut sheet, CLIENT, slider_point(row, 0)));
    for permille in [200, 400, 600, 800, 1000] {
        outcomes.push(sheet.on_pointer(
            &moved(slider_point(row, permille)),
            CLIENT,
            SCALE,
            &theme(),
            &mut damage::sink(),
        ));
    }
    assert!(
        count(&outcomes, SheetOutcome::Edited) > 1,
        "every sample of the drag is applied live"
    );
    assert_eq!(
        count(&outcomes, SheetOutcome::Settled),
        0,
        "nothing is written while the drag continues"
    );

    outcomes.push(sheet.on_pointer(&RELEASE, CLIENT, SCALE, &theme(), &mut damage::sink()));
    assert_eq!(
        count(&outcomes, SheetOutcome::Settled),
        1,
        "the release settles exactly once"
    );
    assert_eq!(sheet.profile().font_size_px, MAX_FONT_SIZE_PX);
}

/// A press and release with no motion between them is a track click: the value
/// is applied and then settled, so a single click still saves.
#[test]
fn clicking_the_text_size_track_settles_the_value_it_jumped_to() {
    let mut sheet = sheet();
    let row = visible_row(&sheet, CLIENT, Focus::TextSize);
    let point = slider_point(row, 1000);
    assert_eq!(
        press_at(&mut sheet, CLIENT, point),
        SheetOutcome::Edited,
        "the press applies the value it jumped to"
    );
    assert_eq!(
        sheet.on_pointer(&RELEASE, CLIENT, SCALE, &theme(), &mut damage::sink()),
        SheetOutcome::Settled
    );
    assert_eq!(sheet.profile().font_size_px, MAX_FONT_SIZE_PX);
}

// --- Appearance: the custom-scheme editor ----------------------------------

#[test]
fn a_channel_slider_edits_the_selected_well_of_the_custom_scheme() {
    let mut sheet = sheet();
    let before = sheet.profile().custom;
    assert_eq!(
        sheet.swatches.selected(),
        0,
        "the background well opens selected"
    );

    focus_on(&mut sheet, CLIENT, Focus::Channel(0));
    assert_eq!(
        key(&mut sheet, CLIENT, Key::Named(NamedKey::End)),
        SheetOutcome::Settled
    );

    assert_eq!(sheet.profile().custom.background.r, u8::MAX);
    assert_eq!(sheet.profile().custom.background.g, before.background.g);
    assert_eq!(sheet.profile().custom.background.b, before.background.b);
    assert_eq!(
        sheet.profile().custom.foreground,
        before.foreground,
        "only the selected well is edited"
    );
}

#[test]
fn selecting_another_well_repoints_the_channel_sliders() {
    let mut sheet = sheet();
    sheet.swatches.adopt_selected(1);
    sheet.sync_channel_sliders();

    focus_on(&mut sheet, CLIENT, Focus::Channel(2));
    assert_eq!(
        key(&mut sheet, CLIENT, Key::Named(NamedKey::Home)),
        SheetOutcome::Settled
    );

    assert_eq!(sheet.profile().custom.foreground.b, 0);
    assert_ne!(
        sheet.profile().custom.background,
        sheet.profile().custom.foreground,
        "the background well was left alone"
    );
}

// --- Effects ---------------------------------------------------------------

#[test]
fn every_effect_slider_edits_only_its_own_profile_field() {
    let defaults = effect_values(Effects::default());
    for index in 0..EffectKey::COUNT {
        let mut sheet = sheet();
        select_effects_tab(&mut sheet, CLIENT);
        let row = visible_row(&sheet, CLIENT, Focus::Effect(index));

        // The end of travel furthest from this effect's own default, so the
        // press is an edit for every slider — one whose default already sits
        // mid-travel included.
        let to = if defaults[index] >= 500 { 0 } else { 1000 };
        assert_eq!(
            press_at(&mut sheet, CLIENT, slider_point(row, to)),
            SheetOutcome::Edited,
            "effect {index} reports the edit"
        );

        let after = effect_values(sheet.profile().effects);
        for (other, value) in after.iter().enumerate() {
            if other == index {
                assert_ne!(*value, defaults[other], "effect {index} moved");
            } else {
                assert_eq!(*value, defaults[other], "effect {other} was left alone");
            }
        }
    }
}

#[test]
fn the_opacity_slider_spans_its_own_floor_to_full() {
    let mut sheet = sheet();
    select_effects_tab(&mut sheet, CLIENT);
    let row = visible_row(&sheet, CLIENT, Focus::Effect(0));

    assert_eq!(
        press_at(&mut sheet, CLIENT, slider_point(row, 0)),
        SheetOutcome::Edited
    );
    assert_eq!(
        sheet.profile().effects.opacity,
        MIN_OPACITY,
        "the low end of the travel is the readable floor, not a dead zone"
    );

    sheet.on_pointer(&RELEASE, CLIENT, SCALE, &theme(), &mut damage::sink());
    assert_eq!(
        press_at(&mut sheet, CLIENT, slider_point(row, 1000)),
        SheetOutcome::Edited
    );
    assert_eq!(sheet.profile().effects.opacity, FULL);
}

/// The other half of the reported slider freeze: what the transparency and
/// blur sliders *report* is a small part of the sheet, so the retained picture
/// ([`crate::sheet::SheetScreen`]) has something worth scoping a repaint to.
/// The sheet used to be re-rendered whole into a freshly allocated surface on
/// every sample of a drag, and the reports below were discarded.
#[test]
fn dragging_an_effect_slider_reports_a_small_part_of_the_sheet() {
    let mut sheet = sheet();
    select_effects_tab(&mut sheet, CLIENT);
    // Every effect slider, opacity and blur included: none of them may claim
    // the sheet.
    for index in 0..EffectKey::COUNT {
        let row = visible_row(&sheet, CLIENT, Focus::Effect(index));
        let mut damage = damage::sink();
        sheet.on_pointer(
            &moved(slider_point(row, 0)),
            CLIENT,
            SCALE,
            &theme(),
            &mut damage,
        );
        sheet.on_pointer(&PRESS, CLIENT, SCALE, &theme(), &mut damage);
        for permille in [200, 400, 600, 800, 1000] {
            sheet.on_pointer(
                &moved(slider_point(row, permille)),
                CLIENT,
                SCALE,
                &theme(),
                &mut damage,
            );
        }
        sheet.on_pointer(&RELEASE, CLIENT, SCALE, &theme(), &mut damage);

        let reported = damage.bounds();
        assert!(
            !reported.is_empty(),
            "effect slider {index} must report what it changed"
        );
        let area = u64::from(reported.width) * u64::from(reported.height);
        let sheet_area = u64::from(CLIENT.width) * u64::from(CLIENT.height);
        assert!(
            area * 4 < sheet_area,
            "a whole drag of effect slider {index} reported {reported:?}, \
             which is not a small part of {CLIENT:?}"
        );
    }
}

#[test]
fn an_effect_slider_reaches_full_at_the_end_of_its_travel() {
    let mut sheet = sheet();
    select_effects_tab(&mut sheet, CLIENT);
    let row = visible_row(&sheet, CLIENT, Focus::Effect(1));
    assert_eq!(
        press_at(&mut sheet, CLIENT, slider_point(row, 1000)),
        SheetOutcome::Edited
    );
    assert_eq!(sheet.profile().effects.blur, FULL);
}

#[test]
fn the_profile_stays_clamped_after_extreme_values() {
    let mut sheet = sheet();
    select_effects_tab(&mut sheet, CLIENT);
    for index in 0..EffectKey::COUNT {
        let row = visible_row(&sheet, CLIENT, Focus::Effect(index));
        press_at(&mut sheet, CLIENT, slider_point(row, 0));
        sheet.on_pointer(&RELEASE, CLIENT, SCALE, &theme(), &mut damage::sink());
        press_at(&mut sheet, CLIENT, slider_point(row, 1000));
        sheet.on_pointer(&RELEASE, CLIENT, SCALE, &theme(), &mut damage::sink());
        press_at(&mut sheet, CLIENT, slider_point(row, 0));
        sheet.on_pointer(&RELEASE, CLIENT, SCALE, &theme(), &mut damage::sink());
    }
    focus_on(&mut sheet, CLIENT, Focus::Tabs);
    key(&mut sheet, CLIENT, Key::Named(NamedKey::Left));
    key(&mut sheet, CLIENT, Key::Named(NamedKey::Enter));
    focus_on(&mut sheet, CLIENT, Focus::TextSize);
    key(&mut sheet, CLIENT, Key::Named(NamedKey::Home));

    let mut expected = *sheet.profile();
    expected.clamp();
    assert_eq!(
        *sheet.profile(),
        expected,
        "the sheet never leaves a profile unclamped"
    );
    assert!(sheet.profile().effects.opacity >= MIN_OPACITY);
    assert!(sheet.profile().font_size_px >= MIN_FONT_SIZE_PX);
}

// --- The footer ------------------------------------------------------------

#[test]
fn restore_defaults_asks_the_caller_rather_than_resetting_the_sheet() {
    // "Defaults" means the layers beneath the user's own document — the
    // machine's policy, the bundle's shipped defaults — and only the store
    // knows what those say. The sheet therefore reports the request and keeps
    // showing what it has until the caller hands back the profile that
    // actually applies.
    let mut sheet = sheet();
    let row = visible_row(&sheet, CLIENT, Focus::Scheme(1));
    click_at(&mut sheet, CLIENT, centre(row));
    focus_on(&mut sheet, CLIENT, Focus::Channel(0));
    key(&mut sheet, CLIENT, Key::Named(NamedKey::End));
    let edited = *sheet.profile();
    assert_ne!(edited, Profile::default(), "the profile really was edited");

    let (restore, _) = footer_buttons(&sheet, CLIENT);
    assert_eq!(
        click_at(&mut sheet, CLIENT, centre(restore)),
        SheetOutcome::Restore
    );
    assert_eq!(
        *sheet.profile(),
        edited,
        "the sheet does not guess at what the defaults are"
    );
}

/// Adopt `profile` into `sheet` as the caller does once the store answers.
fn adopt(sheet: &mut Settings, profile: Profile) {
    sheet.adopt(profile, CLIENT, SCALE, &theme(), &mut damage::sink());
}

#[test]
fn adopting_a_profile_rebuilds_every_control_to_match() {
    // What the caller does once the store has answered: the sheet is told the
    // profile that now applies, and its controls follow.
    let mut sheet = sheet();
    let row = visible_row(&sheet, CLIENT, Focus::Scheme(1));
    click_at(&mut sheet, CLIENT, centre(row));
    assert_ne!(*sheet.profile(), Profile::default());

    adopt(&mut sheet, Profile::default());
    assert_eq!(*sheet.profile(), Profile::default());
    assert!(
        sheet.scheme_radios[0].is_selected(),
        "the controls follow the adopted profile"
    );
}

/// An answer that lands while a slider is still under the pointer re-seeds
/// the sheet without taking the drag out of the user's hand: the rest of the
/// gesture keeps driving the same slider, and the edit it settles on carries
/// the adopted values of everything else.
#[test]
fn adopting_mid_drag_leaves_the_drag_in_hand() {
    let mut sheet = sheet();
    let row = visible_row(&sheet, CLIENT, Focus::TextSize);
    assert_eq!(
        press_at(&mut sheet, CLIENT, slider_point(row, 0)),
        SheetOutcome::Edited
    );
    let dragged = *sheet.profile();

    let answer = Profile {
        scheme: Scheme::Contrast,
        effects: Effects {
            opacity: FULL,
            ..dragged.effects
        },
        ..dragged
    };
    adopt(&mut sheet, answer);

    assert_eq!(
        sheet.on_pointer(
            &moved(slider_point(row, 1000)),
            CLIENT,
            SCALE,
            &theme(),
            &mut damage::sink()
        ),
        SheetOutcome::Edited,
        "the drag is still live after the answer"
    );
    assert_eq!(
        sheet.on_pointer(&RELEASE, CLIENT, SCALE, &theme(), &mut damage::sink()),
        SheetOutcome::Settled
    );
    let settled = *sheet.profile();
    assert_eq!(settled.font_size_px, MAX_FONT_SIZE_PX);
    assert_eq!(settled.scheme, Scheme::Contrast);
    assert_eq!(settled.effects.opacity, FULL);
}

/// The colours arriving from the store do not move the selected well, so a
/// channel slider being edited keeps editing the colour the user chose.
#[test]
fn adopting_keeps_the_well_the_channel_sliders_edit() {
    let mut sheet = sheet();
    sheet.swatches.adopt_selected(5);
    sheet.sync_channel_sliders();
    // The channel rows sit at the end of the body.
    focus_on(&mut sheet, CLIENT, Focus::Scroll);
    key(&mut sheet, CLIENT, Key::Named(NamedKey::End));
    let row = visible_row(&sheet, CLIENT, Focus::Channel(0));
    assert_eq!(
        press_at(&mut sheet, CLIENT, slider_point(row, 0)),
        SheetOutcome::Edited
    );

    let mut answer = *sheet.profile();
    answer.custom.background = Rgb::new(0x21, 0x43, 0x65);
    adopt(&mut sheet, answer);
    assert_eq!(sheet.swatches.selected(), 5);

    sheet.on_pointer(
        &moved(slider_point(row, 1000)),
        CLIENT,
        SCALE,
        &theme(),
        &mut damage::sink(),
    );
    sheet.on_pointer(&RELEASE, CLIENT, SCALE, &theme(), &mut damage::sink());
    let settled = *sheet.profile();
    assert_eq!(
        settled.custom.ansi[1].r,
        u8::MAX,
        "well 5 is the second ANSI colour"
    );
    assert_eq!(
        settled.custom.background, answer.custom.background,
        "the background well only took the adopted colour"
    );
}

#[test]
fn the_done_button_dismisses() {
    let mut sheet = sheet();
    let (_, done) = footer_buttons(&sheet, CLIENT);
    assert_eq!(
        click_at(&mut sheet, CLIENT, centre(done)),
        SheetOutcome::Dismissed
    );
}

// --- Dismissal -------------------------------------------------------------

#[test]
fn escape_dismisses() {
    let mut sheet = sheet();
    assert_eq!(
        key(&mut sheet, CLIENT, Key::Named(NamedKey::Escape)),
        SheetOutcome::Dismissed
    );
}

#[test]
fn escape_dismisses_even_when_the_viewport_cannot_draw_the_sheet() {
    let mut sheet = sheet();
    assert_eq!(
        key(
            &mut sheet,
            Rect::new(0, 0, 1, 1),
            Key::Named(NamedKey::Escape)
        ),
        SheetOutcome::Dismissed
    );
}

#[test]
fn a_press_outside_the_panel_dismisses() {
    let mut sheet = sheet();
    let bounds = panel_bounds(CLIENT, SCALE);
    let outside = Point::new(bounds.left() - 1, bounds.top() - 1);
    assert!(!bounds.contains(outside));
    assert_eq!(
        press_at(&mut sheet, CLIENT, outside),
        SheetOutcome::Dismissed
    );
}

#[test]
fn a_press_inside_the_panel_does_not_dismiss() {
    let mut sheet = sheet();
    let bounds = panel_bounds(CLIENT, SCALE);
    let outcome = press_at(&mut sheet, CLIENT, centre(bounds));
    assert_ne!(outcome, SheetOutcome::Dismissed);
}

// --- The keyboard-only path -------------------------------------------------

#[test]
fn the_keyboard_alone_reaches_and_changes_a_setting() {
    let mut sheet = sheet();
    let wanted = Scheme::ALL[2];
    assert_ne!(sheet.profile().scheme, wanted);

    focus_on(&mut sheet, CLIENT, Focus::Scheme(2));
    assert_eq!(
        key(&mut sheet, CLIENT, Key::Char(' ')),
        SheetOutcome::Settled
    );
    assert_eq!(sheet.profile().scheme, wanted);
}

#[test]
fn the_keyboard_reaches_every_row_of_the_active_tab() {
    let mut sheet = sheet();
    for row in sheet.content_rows() {
        focus_on(&mut sheet, CLIENT, row);
        assert_eq!(sheet.focus, row);
    }
    focus_on(&mut sheet, CLIENT, Focus::Restore);
    focus_on(&mut sheet, CLIENT, Focus::Done);
}

#[test]
fn shift_tab_walks_the_focus_order_backwards() {
    let mut sheet = sheet();
    let order = sheet.focus_order();
    let last = *order.last().expect("the sheet has focusable elements");
    assert_eq!(sheet.focus, Focus::Tabs, "focus opens on the tab strip");
    assert_eq!(
        shift_key(&mut sheet, CLIENT, Key::Named(NamedKey::Tab)),
        SheetOutcome::Changed
    );
    assert_eq!(sheet.focus, last);
}

#[test]
fn a_keyboard_only_session_reaches_a_row_the_body_cannot_show() {
    let mut sheet = sheet();
    let last = *sheet
        .content_rows()
        .last()
        .expect("the appearance tab has rows");
    focus_on(&mut sheet, TINY, last);
    assert_eq!(
        key(&mut sheet, TINY, Key::Named(NamedKey::End)),
        SheetOutcome::Settled,
        "a row unreachable by pointer on a tiny viewport is still editable"
    );
}

/// A pointer sample that redraws nothing must not ask the caller for a
/// repaint: the sheet is one plate in its own window, so a repaint re-renders
/// and re-publishes every pixel of it, and the pointer samples far faster than
/// the sheet changes.
#[test]
fn a_pointer_sample_that_redraws_nothing_asks_for_nothing() {
    let mut sheet = sheet();
    let row = row_rect(&sheet, CLIENT, Focus::Scheme(1)).expect("the first scheme row is shown");
    let at = Point::new(row.left() + to_i32(row.width / 2), row.top() + 1);

    let mut arriving = damage::sink();
    let first = sheet.on_pointer(&moved(at), CLIENT, SCALE, &theme(), &mut arriving);
    assert_eq!(
        first == SheetOutcome::Changed,
        !arriving.is_empty(),
        "a repaint was asked for exactly when something was reported"
    );

    // The same sample again, and one a pixel away inside the same row: the
    // sheet looks precisely as it did.
    for to in [at, Point::new(at.x + 1, at.y)] {
        let mut damage = damage::sink();
        assert_eq!(
            sheet.on_pointer(&moved(to), CLIENT, SCALE, &theme(), &mut damage),
            SheetOutcome::Ignored,
            "a sample that changed nothing asked for a repaint"
        );
        assert!(damage.is_empty(), "and it reported nothing either");
    }
}

/// The other half of the rule: a round that *did* report says so, and a tab
/// switch reports the body it replaced.
///
/// The strip reports only the two plates whose selection changed, so a sheet
/// that painted just that left every row of the tab it came from standing —
/// the Appearance controls stayed on screen under the Effects tab until an
/// unrelated hover happened to redraw them.
#[test]
fn switching_tabs_reports_the_body_it_replaced() {
    let mut sheet = sheet();
    let mut damage = damage::sink();
    let before = sheet.tabs.selected();
    let (tabs, body, scrollbar, _) = bands(&sheet, CLIENT);
    let strip = tabs.expect("the tab strip is laid out");
    let body = body.expect("the body band is laid out");
    let scrollbar = scrollbar.expect("the scrollbar band is laid out");
    let at = Point::new(
        strip.right() - to_i32(strip.width / 4),
        strip.top() + to_i32(strip.height / 2),
    );

    sheet.on_pointer(&moved(at), CLIENT, SCALE, &theme(), &mut damage);
    sheet.on_pointer(&PRESS, CLIENT, SCALE, &theme(), &mut damage);
    assert_eq!(
        sheet.on_pointer(&RELEASE, CLIENT, SCALE, &theme(), &mut damage),
        SheetOutcome::Changed
    );
    assert_ne!(sheet.tabs.selected(), before, "the tab really changed");
    assert!(
        covers(&damage, body),
        "every row the new tab draws must be repainted"
    );
    assert!(
        covers(&damage, scrollbar),
        "the bar is re-clamped against the new tab's extent"
    );
}

/// The same rule from the keyboard, which reaches the strip with no rectangle
/// of its own to hit-test against.
#[test]
fn switching_tabs_by_key_reports_the_body_it_replaced() {
    let mut sheet = sheet();
    let body = body(&sheet, CLIENT);
    focus_on(&mut sheet, CLIENT, Focus::Tabs);

    let mut damage = damage::sink();
    sheet.on_key(
        Key::Named(NamedKey::Right),
        Modifiers::default(),
        CLIENT,
        SCALE,
        &theme(),
        &mut damage,
    );
    sheet.on_key(
        Key::Named(NamedKey::Enter),
        Modifiers::default(),
        CLIENT,
        SCALE,
        &theme(),
        &mut damage,
    );
    assert_eq!(sheet.tabs.selected(), Some(EFFECTS_TAB));
    assert!(covers(&damage, body));
}

/// A press moves the drawn focus ring, not just the field the keyboard reads.
///
/// Nothing synced the ring on this path, so clicking a row left it drawn on
/// whatever held focus before while every key went to the row just clicked.
#[test]
fn a_pressed_row_takes_the_focus_ring() {
    let mut sheet = sheet();
    let row = row_rect(&sheet, CLIENT, Focus::Scheme(1)).expect("the second scheme row is seated");
    let at = Point::new(
        row.left() + to_i32(row.width / 4),
        row.top() + to_i32(row.height / 2),
    );

    let mut damage = damage::sink();
    sheet.on_pointer(&moved(at), CLIENT, SCALE, &theme(), &mut damage);
    sheet.on_pointer(&PRESS, CLIENT, SCALE, &theme(), &mut damage);
    sheet.on_pointer(&RELEASE, CLIENT, SCALE, &theme(), &mut damage);

    assert_eq!(sheet.focus, Focus::Scheme(1));
    assert!(
        sheet.scheme_radios[1].state().focus.focused,
        "the row the keyboard now edits is the row drawing the ring"
    );
    assert!(
        !sheet.scheme_radios[0].state().focus.focused,
        "and it is the only one"
    );
    assert!(covers(&damage, row), "the ring it arrived on is redrawn");
}

/// A value the sheet writes back into a control is drawn twice — as the
/// control's own state and as the label beside it — so the whole row is the
/// scope, not the control's rectangle.
///
/// Focus is moved before the report is measured, because a focus arrival
/// reports the row too and would mask the missing report.
#[test]
fn a_keyed_edit_reports_the_label_beside_the_control() {
    let mut sheet = sheet();
    focus_on(&mut sheet, CLIENT, Focus::TextSize);
    let row = row_rect(&sheet, CLIENT, Focus::TextSize).expect("the text-size row is seated");

    let mut damage = damage::sink();
    assert_eq!(
        key_into(&mut sheet, CLIENT, Key::Named(NamedKey::Home), &mut damage),
        SheetOutcome::Settled
    );
    assert_eq!(sheet.profile().font_size_px, MIN_FONT_SIZE_PX);
    assert!(
        covers(&damage, row),
        "the label spells the value out, so it is redrawn with the knob"
    );
}

/// The same rule for the effects tab, whose labels carry a percentage.
#[test]
fn a_keyed_effect_edit_reports_its_label() {
    let mut sheet = sheet();
    select_effects_tab(&mut sheet, CLIENT);
    focus_on(&mut sheet, CLIENT, Focus::Effect(0));
    let row = row_rect(&sheet, CLIENT, Focus::Effect(0)).expect("the first effect row is seated");

    let mut damage = damage::sink();
    key_into(&mut sheet, CLIENT, Key::Named(NamedKey::Home), &mut damage);
    assert!(covers(&damage, row));
}

/// Choosing another well re-points all three channel sliders, which is the
/// sheet's own write into controls it did not touch.
#[test]
fn selecting_a_well_reports_the_channel_rows_it_repoints() {
    let mut sheet = sheet();
    // The channel rows sit below the swatch grid, so once the grid has focus
    // the body is scrolled to its end to show them before anything is
    // asserted about their pixels.
    focus_on(&mut sheet, CLIENT, Focus::Swatches);
    let end = resolved(&sheet, CLIENT).1.range().max_offset();
    wheel_to(&mut sheet, CLIENT, end);
    let seated: Vec<Rect> = (0..3)
        .filter_map(|index| row_rect(&sheet, CLIENT, Focus::Channel(index)))
        .collect();
    assert!(!seated.is_empty(), "at least one channel row is on screen");

    let mut damage = damage::sink();
    key_into(&mut sheet, CLIENT, Key::Named(NamedKey::Right), &mut damage);
    for row in seated {
        assert!(
            covers(&damage, row),
            "a slider now showing another well's channel is redrawn"
        );
    }
}

/// Scrolling moves every row, and the bar reports only its own thumb.
#[test]
fn scrolling_reports_the_body_whose_rows_moved() {
    let mut sheet = sheet();
    let body = body(&sheet, CLIENT);
    // Reaching the bar walks focus over every row, which leaves the last one
    // revealed and the body at its end.
    focus_on(&mut sheet, CLIENT, Focus::Scroll);
    let before = offset(&sheet, CLIENT);

    let mut damage = damage::sink();
    key_into(&mut sheet, CLIENT, Key::Named(NamedKey::Home), &mut damage);
    assert_ne!(offset(&sheet, CLIENT), before, "the body really scrolled");
    assert!(covers(&damage, body));
}

/// Choosing a scheme from the keyboard moves the dot between two radios, and
/// neither the radio nor the key path has a rectangle of its own to report.
#[test]
fn a_keyed_scheme_choice_reports_both_dots() {
    let mut sheet = sheet();
    let (lit, custom) = (scheme_row(&sheet), custom_scheme_row());
    assert_ne!(lit, custom, "the custom scheme is not the one lit");

    focus_on(&mut sheet, CLIENT, Focus::Scheme(custom));
    let leaving = row_rect(&sheet, CLIENT, Focus::Scheme(lit)).expect("the lit row is seated");
    let arriving =
        row_rect(&sheet, CLIENT, Focus::Scheme(custom)).expect("the custom row is seated");

    let mut damage = damage::sink();
    assert_eq!(
        key_into(&mut sheet, CLIENT, Key::Char(' '), &mut damage),
        SheetOutcome::Settled
    );
    assert_eq!(sheet.profile().scheme, Scheme::Custom);

    assert!(
        covers(&damage, leaving),
        "the dot that emptied must be redrawn"
    );
    assert!(
        covers(&damage, arriving),
        "the dot that filled must be redrawn"
    );
}

/// The custom editor's caption reads off the same field the radios do, so a
/// scheme choice redraws it too.
#[test]
fn a_keyed_scheme_choice_reports_the_editor_caption() {
    let mut sheet = sheet();
    // The editor sits below the radios, so once the radio has focus the body
    // is scrolled to show the editor beside it.
    let radio = Focus::Scheme(custom_scheme_row());
    focus_on(&mut sheet, CLIENT, radio);
    let body = body(&sheet, CLIENT);
    let editor = laid_out(&sheet, CLIENT, Focus::Swatches);
    let above = laid_out(&sheet, CLIENT, radio).top() - body.top();
    wheel_to(
        &mut sheet,
        CLIENT,
        u64::try_from(above).expect("below the top"),
    );
    let caption = row_rect(&sheet, CLIENT, Focus::Swatches).expect("the editor row shows");
    assert!(caption.height > editor.height / 4, "enough of it to matter");
    assert!(
        row_rect(&sheet, CLIENT, radio).is_some(),
        "beside the radio"
    );

    let mut damage = damage::sink();
    key_into(&mut sheet, CLIENT, Key::Char(' '), &mut damage);
    assert_eq!(sheet.profile().scheme, Scheme::Custom);
    assert!(covers(&damage, caption));
}

/// The row index of the scheme the sheet's profile currently names.
fn scheme_row(sheet: &Settings) -> usize {
    Scheme::ALL
        .iter()
        .position(|scheme| *scheme == sheet.profile().scheme)
        .expect("some scheme is lit")
}

/// The row index of the custom scheme.
fn custom_scheme_row() -> usize {
    Scheme::ALL
        .iter()
        .position(|scheme| *scheme == Scheme::Custom)
        .expect("the custom scheme is offered")
}

// --- Tabs and scrolling ------------------------------------------------------

#[test]
fn switching_tabs_replaces_the_body_rows() {
    let mut sheet = sheet();
    assert!(sheet.content_rows().contains(&Focus::TextSize));
    select_effects_tab(&mut sheet, CLIENT);
    let rows = sheet.content_rows();
    assert!(!rows.contains(&Focus::TextSize));
    assert_eq!(rows.len(), EffectKey::COUNT);
}

#[test]
fn scrolling_to_the_end_brings_the_last_row_into_the_body() {
    let mut sheet = sheet();
    let last = *sheet
        .content_rows()
        .last()
        .expect("the appearance tab has rows");
    focus_on(&mut sheet, CLIENT, Focus::Scroll);
    key(&mut sheet, CLIENT, Key::Named(NamedKey::End));
    assert!(
        row_rect(&sheet, CLIENT, last).is_some(),
        "the end of the body is reachable"
    );
}

/// A row scrolled wholly out of the body shows nowhere, and a click where it
/// used to be reaches whatever is drawn there now instead.
#[test]
fn a_row_scrolled_out_of_the_body_takes_no_pointer() {
    let mut sheet = sheet();
    let row = Focus::Scheme(1);
    let was = centre(visible_row(&sheet, CLIENT, row));
    focus_on(&mut sheet, CLIENT, Focus::Scroll);
    key(&mut sheet, CLIENT, Key::Named(NamedKey::End));
    assert!(
        row_rect(&sheet, CLIENT, row).is_none(),
        "the row scrolled out of the body"
    );

    click_at(&mut sheet, CLIENT, was);
    assert_ne!(sheet.profile().scheme, Scheme::ALL[1]);
    assert!(!sheet.scheme_radios[1].is_selected());
}

/// The rows are laid out whole, one after another from the body's own top,
/// wherever the body is scrolled to: scrolling moves the window onto them, not
/// them.
#[test]
fn every_row_is_laid_out_whole_from_the_bodys_top() {
    let mut sheet = sheet();
    let body = body(&sheet, CLIENT);
    let unscrolled = resolved(&sheet, CLIENT).0.rows;
    focus_on(&mut sheet, CLIENT, Focus::Scroll);
    key(&mut sheet, CLIENT, Key::Named(NamedKey::End));
    assert!(offset(&sheet, CLIENT) > 0, "the body really scrolled");
    let scrolled = resolved(&sheet, CLIENT).0.rows;
    assert_eq!(unscrolled, scrolled);

    assert_eq!(
        unscrolled.iter().map(|(row, _)| *row).collect::<Vec<_>>(),
        sheet.content_rows(),
        "every row of the tab is laid out, in display order"
    );
    let mut top = body.top();
    for (row, rect) in unscrolled {
        assert_eq!(rect.top(), top, "{row:?} follows the row before it");
        assert_eq!(rect.left(), body.left());
        assert_eq!(rect.width, body.width);
        top = rect.bottom() + to_i32(SCALE.scale_length(theme().metrics().control_gap));
    }
}

/// The regression: a row the body's edge crossed used to be left out whole,
/// so rows popped in and out as the body scrolled. The body is a window onto
/// rows drawn whole, so scrolling it by `d` pixels moves every pixel it shows
/// up by exactly `d` — the row the edge cuts included.
#[test]
fn scrolling_moves_what_the_body_shows_and_nothing_pops() {
    let mut sheet = sheet();
    let body = body(&sheet, CLIENT);
    // Wheeled over the bar, so no row is hovered in either picture.
    let bar = bands(&sheet, CLIENT)
        .2
        .expect("the scrollbar band is laid out");
    let mut before = surface(CLIENT);
    wheel_at(&mut sheet, CLIENT, centre(bar), 0, &mut damage::sink());
    sheet.render(&mut before, CLIENT, SCALE, &theme());

    // Five units at a time is two pixels, so the rows land on every offset
    // parity and the edge cuts a different part of a row each time.
    for turn in 1..=12 {
        let was = offset(&sheet, CLIENT);
        wheel_at(&mut sheet, CLIENT, centre(bar), 5, &mut damage::sink());
        let moved = offset(&sheet, CLIENT) - was;
        assert!(moved > 0, "turn {turn} scrolled the body");
        let mut after = surface(CLIENT);
        sheet.render(&mut after, CLIENT, SCALE, &theme());

        let d = to_i32(u32::try_from(moved).expect("a small scroll"));
        for y in body.top()..body.bottom() - d {
            for x in body.left()..body.right() {
                assert_eq!(
                    pixel(&after, x, y),
                    pixel(&before, x, y + d),
                    "turn {turn}: ({x}, {y}) is not what showed {d} pixels lower"
                );
            }
        }
        before = after;
    }
}

/// A row the body's bottom edge cuts shows the part of it inside the body:
/// drawn, and reported and hit there.
#[test]
fn a_row_the_bodys_edge_cuts_shows_its_part_inside_the_body() {
    let sheet = sheet();
    let body = body(&sheet, CLIENT);
    let (layout, _) = resolved(&sheet, CLIENT);
    let (row, laid) = layout
        .rows
        .iter()
        .copied()
        .find(|(_, rect)| rect.top() < body.bottom() && rect.bottom() > body.bottom())
        .expect("the small-screen budget cuts a row at the body's bottom");
    let shown = layout.rect_of(row);
    assert_eq!(shown, laid.intersection(&body));
    assert!(shown.height > 0 && shown.height < laid.height);

    let mut drawn = surface(CLIENT);
    sheet.render(&mut drawn, CLIENT, SCALE, &theme());
    let ground = pixel(
        &drawn,
        body.left(),
        body.bottom() - 1 - to_i32(shown.height),
    );
    let inked = (shown.top()..shown.bottom())
        .any(|y| (shown.left()..shown.right()).any(|x| pixel(&drawn, x, y) != ground));
    assert!(inked, "{row:?} draws the part of itself the body shows");
}

/// The hidden part of a row the body's bottom edge cuts lies under the
/// footer, and pointing there reaches the footer, never the row.
#[test]
fn the_hidden_part_of_a_cut_row_takes_no_pointer() {
    let mut sheet = sheet();
    let body = body(&sheet, CLIENT);
    let row = Focus::Channel(0);
    // Scrolled so the body's bottom edge crosses the slider's middle.
    let laid = laid_out(&sheet, CLIENT, row);
    let middle = laid.top() + to_i32(laid.height) / 2;
    wheel_to(
        &mut sheet,
        CLIENT,
        u64::try_from(middle - body.bottom()).expect("the row lies below the body"),
    );
    let shown = visible_row(&sheet, CLIENT, row);
    assert!(shown.height < laid.height, "the body's edge cuts the row");
    let (restore, _) = footer_buttons(&sheet, CLIENT);
    let under = Point::new(slider_point(shown, 250).x, body.bottom() + 2);
    assert!(restore.contains(under), "the point is over the footer");
    let custom = sheet.profile().custom;

    sheet.on_pointer(&moved(under), CLIENT, SCALE, &theme(), &mut damage::sink());
    assert_ne!(
        sheet.channel_sliders[0].state().pointer,
        tairix_controls::PointerState::Hover,
        "the row is not hovered through its hidden part"
    );
    assert_eq!(
        click_at(&mut sheet, CLIENT, under),
        SheetOutcome::Restore,
        "the press is the footer's"
    );
    assert_eq!(
        sheet.profile().custom,
        custom,
        "and the slider moved nothing"
    );

    // The part that shows is the row's.
    assert_eq!(
        press_at(&mut sheet, CLIENT, slider_point(shown, 250)),
        SheetOutcome::Edited
    );
    assert_ne!(sheet.profile().custom, custom);
}

/// The pixel at `(x, y)`.
fn pixel(surface: &Surface, x: i32, y: i32) -> tairix_raster::Pixel {
    let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
        panic!("({x}, {y}) is on the surface");
    };
    surface.pixels()[usize::try_from(y * surface.width() + x).expect("an index")]
}

// --- The wheel ---------------------------------------------------------------

/// One detent scrolls the body one wheel step, and both the body whose rows
/// moved and the bar whose thumb moved are reported.
#[test]
fn a_wheel_detent_scrolls_the_body_one_wheel_step() {
    let mut sheet = sheet();
    let (_, body, bar, _) = bands(&sheet, CLIENT);
    let (body, bar) = (body.expect("body"), bar.expect("bar"));

    let mut damage = damage::sink();
    assert_eq!(
        wheel_at(
            &mut sheet,
            CLIENT,
            centre(body),
            SCROLL_UNITS_PER_DETENT,
            &mut damage
        ),
        SheetOutcome::Changed
    );
    assert_eq!(offset(&sheet, CLIENT), detent_px());
    assert!(covers(&damage, body), "every row moved");
    assert!(covers(&damage, bar), "the thumb moved");

    // Back up the other way, over the bar this time.
    wheel_at(
        &mut sheet,
        CLIENT,
        centre(bar),
        -SCROLL_UNITS_PER_DETENT,
        &mut damage::sink(),
    );
    assert_eq!(offset(&sheet, CLIENT), 0);
}

/// A turn short of a pixel is carried, so a detent delivered a unit at a time
/// by a fine wheel scrolls exactly as far as one delivered whole.
#[test]
fn the_wheel_carries_what_a_turn_leaves_short() {
    let mut sheet = sheet();
    let body = body(&sheet, CLIENT);
    for _ in 0..SCROLL_UNITS_PER_DETENT {
        wheel_at(&mut sheet, CLIENT, centre(body), 1, &mut damage::sink());
    }
    assert_eq!(offset(&sheet, CLIENT), detent_px());
}

/// Only the body scrolls: the wheel over the tab strip or the footer moves
/// nothing and asks for nothing.
#[test]
fn the_wheel_scrolls_nothing_but_the_body() {
    let mut sheet = sheet();
    let (tabs, _, _, footer) = bands(&sheet, CLIENT);
    for at in [centre(tabs.expect("tabs")), centre(footer.expect("footer"))] {
        let mut damage = damage::sink();
        assert_eq!(
            wheel_at(&mut sheet, CLIENT, at, SCROLL_UNITS_PER_DETENT, &mut damage),
            SheetOutcome::Ignored
        );
        assert_eq!(offset(&sheet, CLIENT), 0);
        assert!(damage.is_empty());
    }
}

/// A wheel is not a press, so keyboard focus stays where it was however the
/// body scrolls, over the rows or over the bar.
#[test]
fn the_wheel_leaves_keyboard_focus_where_it_was() {
    let mut sheet = sheet();
    let (_, body, bar, _) = bands(&sheet, CLIENT);
    assert_eq!(sheet.focus, Focus::Tabs);
    for at in [centre(body.expect("body")), centre(bar.expect("bar"))] {
        wheel_at(
            &mut sheet,
            CLIENT,
            at,
            SCROLL_UNITS_PER_DETENT,
            &mut damage::sink(),
        );
        assert_eq!(sheet.focus, Focus::Tabs);
    }
    assert!(offset(&sheet, CLIENT) > 0);
}

/// A press on the bar is what takes keyboard focus onto it.
#[test]
fn a_press_on_the_bar_takes_keyboard_focus() {
    let mut sheet = sheet();
    let bar = bands(&sheet, CLIENT)
        .2
        .expect("the scrollbar band is laid out");
    click_at(&mut sheet, CLIENT, centre(bar));
    assert_eq!(sheet.focus, Focus::Scroll);
}

/// The rows move under a pointer that does not: the row that scrolled away
/// loses its hover and the one scrolled beneath the pointer takes it.
#[test]
fn the_hover_follows_the_rows_the_wheel_moves() {
    let mut sheet = sheet();
    let first = visible_row(&sheet, CLIENT, Focus::Scheme(0));
    let at = Point::new(first.left() + 4, first.top() + 2);
    sheet.on_pointer(&moved(at), CLIENT, SCALE, &theme(), &mut damage::sink());
    let hover = tairix_controls::PointerState::Hover;
    assert_eq!(sheet.scheme_radios[0].state().pointer, hover);

    // One whole row pitch, so the next row is now where the first was.
    let dressing = theme();
    let metrics = dressing.metrics();
    let pitch = SCALE.scale_length(metrics.control_height + metrics.control_gap);
    let units = to_i32(pitch) * SCROLL_UNITS_PER_DETENT / to_i32(SCALE.scale_length(WHEEL_STEP));
    sheet.on_pointer(
        &InputEvent::PointerScrolled { dx: 0, dy: units },
        CLIENT,
        SCALE,
        &dressing,
        &mut damage::sink(),
    );
    assert_eq!(offset(&sheet, CLIENT), u64::from(pitch));
    assert_ne!(sheet.scheme_radios[0].state().pointer, hover);
    assert_eq!(sheet.scheme_radios[1].state().pointer, hover);
}

/// A slider held down follows the pointer past the body's edge, along the
/// slider's own axis, and out across the body's scrolling axis too.
#[test]
fn dragging_a_slider_past_the_bodys_edge_holds_it_at_the_end() {
    let mut sheet = sheet();
    select_effects_tab(&mut sheet, CLIENT);
    let blur = EffectKey::ALL
        .iter()
        .position(|key| *key == EffectKey::Blur)
        .expect("blur has a slider");
    let row = visible_row(&sheet, CLIENT, Focus::Effect(blur));
    let (_, body, _, footer) = bands(&sheet, CLIENT);
    let (body, footer) = (body.expect("body"), footer.expect("footer"));
    let y = row.top() + to_i32(row.height) / 2;
    let value = |sheet: &Settings| EffectKey::Blur.of(sheet.profile().effects);
    let drag = |sheet: &mut Settings, to: Point| {
        sheet.on_pointer(&moved(to), CLIENT, SCALE, &theme(), &mut damage::sink());
    };

    assert_eq!(
        press_at(&mut sheet, CLIENT, slider_point(row, 250)),
        SheetOutcome::Edited
    );
    for x in [body.right(), body.right() + 6, CLIENT.right() - 1] {
        drag(&mut sheet, Point::new(x, y));
        assert_eq!(value(&sheet), FULL, "past the trailing edge at x = {x}");
    }
    drag(&mut sheet, Point::new(CLIENT.right() - 1, footer.top() + 2));
    assert_eq!(value(&sheet), FULL, "out of the body into the footer");
    drag(&mut sheet, Point::new(body.left() - 2, footer.top() + 2));
    assert_eq!(value(&sheet), 0, "and past the leading edge");
    assert_eq!(
        sheet.on_pointer(&RELEASE, CLIENT, SCALE, &theme(), &mut damage::sink()),
        SheetOutcome::Settled
    );
}

// --- Keyboard reach ------------------------------------------------------------

/// Tab onto a row the body hides scrolls the least that shows it whole, and
/// reports the body and the bar that moved.
#[test]
fn tabbing_onto_a_hidden_row_scrolls_it_into_view() {
    let mut sheet = sheet();
    let last = *sheet
        .content_rows()
        .last()
        .expect("the appearance tab has rows");
    assert!(row_rect(&sheet, CLIENT, last).is_none(), "it starts hidden");
    let (_, body, bar, _) = bands(&sheet, CLIENT);
    let (body, bar) = (body.expect("body"), bar.expect("bar"));

    let mut damage = damage::sink();
    for _ in 0..=sheet.focus_order().len() {
        if sheet.focus == last {
            break;
        }
        key_into(&mut sheet, CLIENT, Key::Named(NamedKey::Tab), &mut damage);
    }
    assert_eq!(sheet.focus, last);
    let laid = laid_out(&sheet, CLIENT, last);
    let shown = visible_row(&sheet, CLIENT, last);
    assert_eq!(shown.height, laid.height, "the row shows whole");
    assert_eq!(
        shown.bottom(),
        body.bottom(),
        "and scrolled no further than that"
    );
    assert!(covers(&damage, body));
    assert!(covers(&damage, bar));
}
