//! The colour picker: its readouts, its layouts, drags, keys and fields, what
//! settles and when, what each change repaints, and its denied, disabled and
//! focused looks.

use alloc::vec::Vec;

use tairix_colour::{Hsv, Rgba};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::Surface;
use tairix_theme::Theme;

use super::{ColourPicker, Layout, Part, PickerOutcome, ALPHA};
use crate::colour_model::{ColourModel, PickerView};
use crate::damage;
use crate::paint::authority_rgba;
use crate::state::{AuthorityState, ControlState};
use crate::testkit::{has_pixel, high_contrast, premul};

const WIDE: Rect = Rect::new(0, 0, 480, 200);
const NARROW: Rect = Rect::new(0, 0, 220, 420);

const SLATE: Rgba = Rgba::rgb(0x33, 0x66, 0x99);

const PRESS: InputEvent = InputEvent::PointerPressed {
    button: PointerButton::Primary,
};
const RELEASE: InputEvent = InputEvent::PointerReleased {
    button: PointerButton::Primary,
};

fn picker(colour: Rgba) -> ColourPicker {
    let mut picker = ColourPicker::new(colour);
    picker.set_focused(true);
    picker
}

fn layout(picker: &ColourPicker, bounds: Rect) -> Layout {
    picker.layout(bounds, Scale::ONE, &Theme::dark())
}

/// The point `across` and `down` thousandths into `part`'s drawing area.
fn into(layout: &Layout, part: Part, across: u32, down: u32) -> Point {
    let area = layout.inner(part);
    let offset = |extent: u32, permille: u32| {
        i32::try_from(extent.saturating_sub(1) * permille / 1000).expect("fits")
    };
    Point::new(
        area.left() + offset(area.width, across),
        area.top() + offset(area.height, down),
    )
}

fn pointer(picker: &mut ColourPicker, event: &InputEvent, bounds: Rect) -> PickerOutcome {
    picker.on_pointer(
        event,
        bounds,
        Scale::ONE,
        &Theme::dark(),
        &mut damage::sink(),
    )
}

fn move_to(picker: &mut ColourPicker, to: Point, bounds: Rect) -> PickerOutcome {
    pointer(picker, &InputEvent::PointerMoved { to }, bounds)
}

fn key_with(picker: &mut ColourPicker, key: Key, modifiers: Modifiers) -> PickerOutcome {
    let theme = Theme::dark();
    picker.on_key(
        key,
        modifiers,
        WIDE,
        (Scale::ONE, &theme),
        &mut damage::sink(),
    )
}

fn key(picker: &mut ColourPicker, key: Key) -> PickerOutcome {
    key_with(picker, key, Modifiers::default())
}

fn named(picker: &mut ColourPicker, named: NamedKey) -> PickerOutcome {
    key(picker, Key::Named(named))
}

fn shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..Modifiers::default()
    }
}

/// Tab from the plane to `part`.
fn tab_to(picker: &mut ColourPicker, part: Part) {
    picker.part = Part::Plane;
    picker.sync_children();
    while picker.part != part {
        assert_ne!(
            named(picker, NamedKey::Tab),
            PickerOutcome::Ignored,
            "reached the end before {part:?}"
        );
    }
}

/// Type `text` over whatever the focused field holds.
fn type_over(picker: &mut ColourPicker, text: &str) -> Vec<PickerOutcome> {
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    key_with(picker, Key::Char('a'), ctrl);
    text.chars().map(|c| key(picker, Key::Char(c))).collect()
}

fn render(picker: &ColourPicker, bounds: Rect, theme: &Theme) -> Surface {
    let mut surface = Surface::new(bounds.width, bounds.height).expect("surface");
    picker.render(&mut surface, bounds, Scale::ONE, theme);
    surface
}

fn readouts(picker: &ColourPicker) -> [i32; 5] {
    picker.numbers.each_ref().map(super::NumberField::value)
}

#[test]
fn every_readout_shows_the_colour() {
    let picker = picker(SLATE);
    let fields = readouts(&picker);
    assert_eq!(
        [fields[0], fields[1], fields[2], fields[ALPHA]],
        [0x33, 0x66, 0x99, 255]
    );
    let hsv = picker.clone().with_model(ColourModel::Hsv);
    assert_eq!(readouts(&hsv)[..3], [210, 67, 60]);
    assert_eq!(picker.hex.text(), "#336699");
    assert_eq!(picker.colour(), SLATE);
}

#[test]
fn a_picker_without_opacity_is_opaque_and_one_with_it_spells_its_alpha() {
    let translucent = Rgba::new(0x33, 0x66, 0x99, 0x80);
    assert_eq!(ColourPicker::new(translucent).colour(), SLATE);
    let picker = ColourPicker::new(SLATE).with_opacity(true);
    let mut picker = picker;
    picker.set_colour(translucent);
    assert_eq!(picker.colour(), translucent);
    assert_eq!(picker.hex.text(), "#33669980");
    assert_eq!(readouts(&picker)[ALPHA], 0x80);
}

#[test]
fn wide_bounds_put_the_fields_beside_the_plane_and_narrow_ones_beneath_it() {
    let picker = picker(SLATE);
    let wide = layout(&picker, WIDE);
    assert!(wide.hex.left() > wide.hue.right(), "beside");
    assert!(wide.numbers[0].top() < wide.plane.bottom());
    let narrow = layout(&picker, NARROW);
    assert!(narrow.hex.top() >= narrow.plane.bottom(), "beneath");
    assert!(narrow.numbers[0].top() > narrow.hex.top());
    for layout in [wide, narrow] {
        assert!(layout.alpha.is_empty() && layout.earlier.is_empty());
        assert!(layout.numbers[ALPHA].is_empty());
        for part in [Part::Plane, Part::Hue, Part::Hex] {
            assert!(!layout.rect_of(part).is_empty(), "{part:?}");
        }
    }
}

#[test]
fn short_bounds_give_up_the_fields_then_the_hex_row_but_never_the_plane() {
    let picker = picker(SLATE);
    let full = picker.measured_height(NARROW.width, Scale::ONE, &Theme::dark());
    let at = |height| layout(&picker, Rect::new(0, 0, NARROW.width, height));
    let whole = at(full);
    assert!(COMPONENTS_SHOWN
        .iter()
        .all(|&i| !whole.numbers[i].is_empty()));
    assert_eq!(
        u32::try_from(whole.numbers[2].bottom()).expect("fits"),
        full,
        "the measured height is exactly what every part needs"
    );
    let m = whole.measures;
    let without_grid = at(m.plane_min + m.gap + m.row);
    assert!(without_grid.numbers.iter().all(Rect::is_empty));
    assert!(!without_grid.hex.is_empty());
    let plane_only = at(m.plane_min);
    assert!(plane_only.hex.is_empty() && !plane_only.plane.is_empty());
}

/// The fields an RGB picker without opacity lays out.
const COMPONENTS_SHOWN: [usize; 3] = [0, 1, 2];

#[test]
fn a_drag_on_the_plane_is_live_and_settles_once_on_release() {
    let mut picker = picker(SLATE);
    let layout = layout(&picker, WIDE);
    move_to(&mut picker, into(&layout, Part::Plane, 1000, 0), WIDE);
    let pressed = pointer(&mut picker, &PRESS, WIDE);
    let hue = Hsv::from_rgb(SLATE.without_alpha(), Hsv::default()).hue;
    let full = Hsv::new(
        hue,
        tairix_colour::Fraction::ALL,
        tairix_colour::Fraction::ALL,
    );
    assert_eq!(pressed, PickerOutcome::Edited(full.to_rgb().opaque()));
    assert!(picker.is_dragging());
    let dragged = move_to(&mut picker, into(&layout, Part::Plane, 500, 500), WIDE);
    assert!(matches!(dragged, PickerOutcome::Edited(_)));
    let released = pointer(&mut picker, &RELEASE, WIDE);
    assert_eq!(released, PickerOutcome::Settled(picker.colour()));
    assert!(!picker.is_dragging());
    assert_eq!(pointer(&mut picker, &RELEASE, WIDE), PickerOutcome::Ignored);
}

#[test]
fn a_drag_that_comes_back_to_where_it_began_still_settles() {
    let mut picker = picker(SLATE);
    let layout = layout(&picker, WIDE);
    let (cx, cy) = super::marker_centre(layout.inner(Part::Plane), picker.hsv);
    move_to(&mut picker, into(&layout, Part::Plane, 0, 0), WIDE);
    pointer(&mut picker, &PRESS, WIDE);
    move_to(&mut picker, Point::new(cx, cy), WIDE);
    assert_eq!(
        pointer(&mut picker, &RELEASE, WIDE),
        PickerOutcome::Settled(picker.colour()),
        "the owner saw it move, so it hears where it stopped"
    );
}

#[test]
fn a_colour_taken_to_black_keeps_its_hue_and_saturation() {
    let mut picker = picker(SLATE).with_model(ColourModel::Hsv);
    let before = picker.hsv;
    tab_to(&mut picker, Part::Number(2));
    type_over(&mut picker, "0");
    assert_eq!(picker.colour(), Rgba::rgb(0, 0, 0));
    assert_eq!(
        (picker.hsv.hue, picker.hsv.saturation),
        (before.hue, before.saturation)
    );
    type_over(&mut picker, "60");
    assert_eq!(picker.colour(), SLATE, "back up, the colour returns");
    picker.set_colour(Rgba::rgb(0, 0, 0));
    assert_eq!(
        (picker.hsv.hue, picker.hsv.saturation),
        (before.hue, before.saturation),
        "an owner's black keeps them too"
    );
    picker.set_colour(Rgba::rgb(128, 128, 128));
    assert_eq!(picker.hsv.hue, before.hue, "and a grey its hue");
}

#[test]
fn the_hue_strip_sets_the_hue_alone() {
    let mut picker = picker(SLATE);
    let before = picker.hsv;
    let layout = layout(&picker, WIDE);
    move_to(&mut picker, into(&layout, Part::Hue, 500, 0), WIDE);
    assert!(matches!(
        pointer(&mut picker, &PRESS, WIDE),
        PickerOutcome::Edited(_)
    ));
    assert_eq!(picker.hsv.hue, tairix_colour::Hue::RED);
    assert_eq!(
        (picker.hsv.saturation, picker.hsv.value),
        (before.saturation, before.value)
    );
    move_to(&mut picker, into(&layout, Part::Hue, 500, 1000), WIDE);
    assert_eq!(picker.hsv.hue, tairix_colour::Hue::RED, "red at both ends");
    move_to(&mut picker, into(&layout, Part::Hue, 500, 500), WIDE);
    let pixel = 360 / (layout.inner(Part::Hue).height - 1) + 1;
    assert!(
        picker.hsv.hue.degrees().abs_diff(180) <= pixel,
        "half way down is cyan, to the pixel"
    );
    assert!(matches!(
        pointer(&mut picker, &RELEASE, WIDE),
        PickerOutcome::Settled(_)
    ));
}

#[test]
fn the_opacity_strip_sets_the_alpha_from_opaque_at_the_top_to_clear_at_the_bottom() {
    let mut picker = picker(SLATE).with_opacity(true);
    let layout = layout(&picker, WIDE);
    assert!(!layout.alpha.is_empty());
    move_to(&mut picker, into(&layout, Part::Alpha, 500, 1000), WIDE);
    pointer(&mut picker, &PRESS, WIDE);
    assert_eq!(picker.colour(), SLATE.with_alpha(0));
    move_to(&mut picker, into(&layout, Part::Alpha, 500, 0), WIDE);
    assert_eq!(picker.colour(), SLATE);
    pointer(&mut picker, &RELEASE, WIDE);
}

#[test]
fn escape_abandons_a_drag_where_it_began() {
    let mut picker = picker(SLATE);
    let layout = layout(&picker, WIDE);
    move_to(&mut picker, into(&layout, Part::Plane, 100, 900), WIDE);
    pointer(&mut picker, &PRESS, WIDE);
    assert_ne!(picker.colour(), SLATE);
    assert_eq!(
        named(&mut picker, NamedKey::Escape),
        PickerOutcome::Settled(SLATE)
    );
    assert!(!picker.is_dragging());
    assert_eq!(pointer(&mut picker, &RELEASE, WIDE), PickerOutcome::Ignored);
}

#[test]
fn finishing_a_drag_settles_it_where_it_stands() {
    let mut picker = picker(SLATE);
    let layout = layout(&picker, WIDE);
    move_to(&mut picker, into(&layout, Part::Plane, 300, 300), WIDE);
    pointer(&mut picker, &PRESS, WIDE);
    let stood = picker.colour();
    assert_eq!(picker.finish_drag(), PickerOutcome::Settled(stood));
    assert_eq!(picker.finish_drag(), PickerOutcome::Ignored);
}

#[test]
fn keys_step_the_plane_and_the_strips_each_step_settled() {
    let mut picker = picker(SLATE);
    let saturation = picker.hsv.saturation.percent();
    assert!(matches!(
        named(&mut picker, NamedKey::Right),
        PickerOutcome::Settled(_)
    ));
    assert_eq!(picker.hsv.saturation.percent(), saturation + 1);
    key_with(&mut picker, Key::Named(NamedKey::Left), shift());
    assert_eq!(picker.hsv.saturation.percent(), saturation - 9);
    named(&mut picker, NamedKey::End);
    assert_eq!(picker.hsv.saturation, tairix_colour::Fraction::ALL);
    assert_eq!(
        named(&mut picker, NamedKey::End),
        PickerOutcome::Taken,
        "already there"
    );

    tab_to(&mut picker, Part::Hue);
    named(&mut picker, NamedKey::Home);
    assert_eq!(picker.hsv.hue.degrees(), 0);
    named(&mut picker, NamedKey::Up);
    assert_eq!(picker.hsv.hue.degrees(), 359, "round the circle");
    assert_eq!(
        key(&mut picker, Key::Char('b')),
        PickerOutcome::Ignored,
        "the owner's shortcut"
    );
}

#[test]
fn the_opacity_strip_steps_by_one_and_sixteen() {
    let mut picker = picker(SLATE).with_opacity(true);
    tab_to(&mut picker, Part::Alpha);
    named(&mut picker, NamedKey::Down);
    assert_eq!(picker.colour().a, 254);
    key_with(&mut picker, Key::Named(NamedKey::Down), shift());
    assert_eq!(picker.colour().a, 238);
    named(&mut picker, NamedKey::End);
    assert_eq!(picker.colour().a, 0);
}

#[test]
fn hex_typing_is_live_and_enter_settles() {
    let mut picker = picker(SLATE);
    tab_to(&mut picker, Part::Hex);
    let typed = type_over(&mut picker, "#ff8000");
    assert_eq!(
        typed[3],
        PickerOutcome::Edited(Rgba::rgb(0xff, 0xff, 0x88)),
        "#ff8"
    );
    assert_eq!(
        typed[4],
        PickerOutcome::Taken,
        "#ff80 carries an alpha this picker lacks"
    );
    assert_eq!(typed[6], PickerOutcome::Edited(Rgba::rgb(0xff, 0x80, 0x00)));
    assert_eq!(readouts(&picker)[0], 0xff, "the fields follow");
    assert_eq!(
        named(&mut picker, NamedKey::Enter),
        PickerOutcome::Settled(Rgba::rgb(0xff, 0x80, 0x00))
    );
    assert_eq!(named(&mut picker, NamedKey::Enter), PickerOutcome::Taken);
}

#[test]
fn hex_spelling_the_picker_cannot_hold_shows_invalid_and_moves_nothing() {
    assert_eq!(super::read_hex("#11223344", false), None, "no opacity here");
    assert_eq!(
        super::read_hex(" 11223344 ", true),
        Some(Rgba::new(0x11, 0x22, 0x33, 0x44)),
        "spaces round it and no `#` are a person's typing"
    );
    let mut picker = picker(SLATE);
    tab_to(&mut picker, Part::Hex);
    let typed = type_over(&mut picker, "#11223344");
    assert_eq!(typed[8], PickerOutcome::Taken);
    assert_eq!(
        picker.hex.state().validation,
        crate::state::ValidationState::Invalid
    );
    assert_eq!(
        picker.colour(),
        Rgba::rgb(0x11, 0x22, 0x33),
        "the last spelling it could hold"
    );
    assert_eq!(
        named(&mut picker, NamedKey::Enter),
        PickerOutcome::Settled(Rgba::rgb(0x11, 0x22, 0x33))
    );
    assert_eq!(
        picker.hex.text(),
        "#112233",
        "committing shows the colour again"
    );
    assert_eq!(
        picker.hex.state().validation,
        crate::state::ValidationState::Valid
    );
}

#[test]
fn escape_takes_back_hex_typing() {
    let mut picker = picker(SLATE);
    tab_to(&mut picker, Part::Hex);
    type_over(&mut picker, "#000");
    assert_eq!(picker.colour(), Rgba::rgb(0, 0, 0));
    assert_eq!(
        named(&mut picker, NamedKey::Escape),
        PickerOutcome::Settled(SLATE)
    );
    assert_eq!(picker.hex.text(), "#336699");
    assert_eq!(
        named(&mut picker, NamedKey::Escape),
        PickerOutcome::Ignored,
        "the owner's now"
    );
}

#[test]
fn number_fields_edit_their_own_coordinate() {
    let mut picker = picker(SLATE);
    tab_to(&mut picker, Part::Number(0));
    assert_eq!(
        named(&mut picker, NamedKey::Up),
        PickerOutcome::Settled(Rgba::rgb(0x34, 0x66, 0x99))
    );
    assert_eq!(picker.hex.text(), "#346699");
    picker.set_model(ColourModel::Hsv);
    tab_to(&mut picker, Part::Number(2));
    let typed = type_over(&mut picker, "100");
    assert!(matches!(typed[2], PickerOutcome::Edited(_)));
    assert_eq!(picker.hsv.value, tairix_colour::Fraction::ALL);
    assert_eq!(readouts(&picker)[2], 100, "the typed field keeps its text");
    assert!(
        matches!(
            key_with(&mut picker, Key::Named(NamedKey::Tab), shift()),
            PickerOutcome::Settled(_)
        ),
        "leaving commits"
    );
}

#[test]
fn a_chord_a_field_has_no_use_for_is_the_owners() {
    let mut picker = picker(SLATE);
    tab_to(&mut picker, Part::Hex);
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    assert_eq!(
        key_with(&mut picker, Key::Char('s'), ctrl),
        PickerOutcome::Ignored,
        "Ctrl+S saves"
    );
    assert_eq!(
        key_with(&mut picker, Key::Char('a'), ctrl),
        PickerOutcome::Taken,
        "Ctrl+A selects"
    );
    assert_eq!(
        key(&mut picker, Key::Char('b')),
        PickerOutcome::Taken,
        "a field takes letters"
    );
}

#[test]
fn committing_settles_typing_and_keeps_the_keyboard() {
    let mut picker = picker(SLATE);
    tab_to(&mut picker, Part::Hex);
    type_over(&mut picker, "#000");
    let outcome = picker.commit(WIDE, Scale::ONE, &Theme::dark(), &mut damage::sink());
    assert_eq!(outcome, PickerOutcome::Settled(Rgba::rgb(0, 0, 0)));
    assert!(picker.state().focus.focused);
    assert_eq!(picker.part, Part::Hex);
}

#[test]
fn a_field_left_with_typing_settles_as_the_owner_blurs_the_picker() {
    let mut picker = picker(SLATE);
    tab_to(&mut picker, Part::Number(1));
    type_over(&mut picker, "7");
    let outcome = picker.blur(WIDE, Scale::ONE, &Theme::dark(), &mut damage::sink());
    assert_eq!(outcome, PickerOutcome::Settled(Rgba::rgb(0x33, 7, 0x99)));
    assert!(!picker.state().focus.focused);
}

#[test]
fn tab_walks_every_part_shown_and_hands_on_past_the_ends() {
    let mut picker = picker(SLATE).with_opacity(true);
    picker.set_earlier(Some(Rgba::rgb(1, 2, 3)));
    picker.enter_focus(true, WIDE, Scale::ONE, &Theme::dark());
    let mut walked = alloc::vec![picker.part];
    while named(&mut picker, NamedKey::Tab) != PickerOutcome::Ignored {
        walked.push(picker.part);
    }
    assert_eq!(walked, picker.parts().as_slice());
    while key_with(&mut picker, Key::Named(NamedKey::Tab), shift()) != PickerOutcome::Ignored {}
    assert_eq!(picker.part, Part::Plane);
    picker.enter_focus(false, WIDE, Scale::ONE, &Theme::dark());
    assert_eq!(picker.part, Part::Number(ALPHA));
}

#[test]
fn the_earlier_colour_is_taken_back_by_a_press_and_by_enter() {
    let earlier = Rgba::rgb(0xc0, 0x10, 0x10);
    let mut picker = picker(SLATE);
    picker.set_earlier(Some(earlier));
    let layout = layout(&picker, WIDE);
    move_to(&mut picker, into(&layout, Part::Earlier, 500, 500), WIDE);
    assert_eq!(pointer(&mut picker, &PRESS, WIDE), PickerOutcome::Taken);
    assert_eq!(
        pointer(&mut picker, &RELEASE, WIDE),
        PickerOutcome::Settled(earlier)
    );
    picker.set_colour(SLATE);
    tab_to(&mut picker, Part::Earlier);
    assert_eq!(
        named(&mut picker, NamedKey::Enter),
        PickerOutcome::Settled(earlier)
    );
}

#[test]
fn a_picker_that_may_not_act_takes_nothing() {
    for state in [
        ControlState::disabled(),
        ControlState::idle().with_authority(AuthorityState::Denied),
    ] {
        let mut picker = picker(SLATE);
        picker.set_state(state.with_focus(crate::state::FocusState {
            focused: true,
            in_focus_field: false,
        }));
        let layout = layout(&picker, WIDE);
        move_to(&mut picker, into(&layout, Part::Plane, 0, 0), WIDE);
        assert_eq!(
            pointer(&mut picker, &PRESS, WIDE),
            PickerOutcome::Ignored,
            "{state:?}"
        );
        assert_eq!(named(&mut picker, NamedKey::Right), PickerOutcome::Ignored);
        assert_eq!(picker.colour(), SLATE);
        assert!(
            !picker.hex.state().is_actionable(),
            "its fields may not act either"
        );
    }
}

#[test]
fn a_denied_picker_carries_one_authority_bead_and_a_disabled_one_is_veiled() {
    let theme = Theme::dark();
    let plain = render(&ColourPicker::new(SLATE), WIDE, &theme);
    let mut denied = ColourPicker::new(SLATE);
    denied.set_state(ControlState::idle().with_authority(AuthorityState::Denied));
    let bead = premul(authority_rgba(theme.palette(), AuthorityState::Denied));
    assert!(has_pixel(&render(&denied, WIDE, &theme), bead));
    assert!(!has_pixel(&plain, bead));
    let mut disabled = ColourPicker::new(SLATE);
    disabled.set_state(ControlState::disabled());
    assert_ne!(render(&disabled, WIDE, &theme).pixels(), plain.pixels());
}

#[test]
fn focus_rings_the_part_the_keyboard_rests_on() {
    let theme = Theme::dark();
    let ring = premul(theme.palette().rim_active);
    let mut picker = ColourPicker::new(SLATE);
    assert!(!has_pixel(&render(&picker, WIDE, &theme), ring));
    picker.set_focused(true);
    let focused = render(&picker, WIDE, &theme);
    let plane = layout(&picker, WIDE).plane;
    let ringed = (0..WIDE.height).any(|y| {
        (0..WIDE.width).any(|x| {
            let at = Point::new(
                i32::try_from(x).expect("fits"),
                i32::try_from(y).expect("fits"),
            );
            plane.contains(at) && focused.get(x, y) == Some(ring)
        })
    });
    assert!(ringed, "the ring is on the plane");
}

#[test]
fn a_marker_moving_on_the_plane_repaints_the_marker_not_the_plane() {
    let mut picker = picker(SLATE);
    let layout = layout(&picker, WIDE);
    move_to(&mut picker, into(&layout, Part::Plane, 400, 400), WIDE);
    pointer(&mut picker, &PRESS, WIDE);
    let mut damage = Region::new();
    let to = InputEvent::PointerMoved {
        to: into(&layout, Part::Plane, 420, 420),
    };
    picker.on_pointer(&to, WIDE, Scale::ONE, &Theme::dark(), &mut damage);
    let far = into(&layout, Part::Plane, 0, 1000);
    assert!(!damage.contains(far), "not the plane far from the marker");
    assert!(
        !damage.contains(into(&layout, Part::Hue, 500, 500)),
        "the hue held still"
    );
    assert!(damage.intersects(layout.hex), "the readouts moved");
    assert!(
        damage.contains(into(&layout, Part::Plane, 420, 420)),
        "the marker's new place"
    );
}

#[test]
fn a_hue_change_repaints_the_whole_plane() {
    let mut picker = picker(SLATE);
    tab_to(&mut picker, Part::Hue);
    let theme = Theme::dark();
    let mut damage = Region::new();
    picker.on_key(
        Key::Named(NamedKey::Down),
        Modifiers::default(),
        WIDE,
        (Scale::ONE, &theme),
        &mut damage,
    );
    let layout = layout(&picker, WIDE);
    for (across, down) in [(0, 0), (1000, 0), (500, 500), (0, 1000), (1000, 1000)] {
        assert!(
            damage.contains(into(&layout, Part::Plane, across, down)),
            "({across}, {down})"
        );
    }
}

#[test]
fn the_paint_shows_the_hue_the_colour_has() {
    let theme = Theme::dark();
    let red = render(&ColourPicker::new(Rgba::rgb(255, 0, 0)), WIDE, &theme);
    let blue = render(&ColourPicker::new(Rgba::rgb(0, 0, 255)), WIDE, &theme);
    let layout = layout(&ColourPicker::new(SLATE), WIDE);
    let corner = into(&layout, Part::Plane, 1000, 0);
    let at = |surface: &Surface| {
        surface.get(
            u32::try_from(corner.x - 1).expect("fits"),
            u32::try_from(corner.y + 1).expect("fits"),
        )
    };
    assert_eq!(at(&red).map(|pixel| (pixel.r, pixel.b)), Some((255, 0)));
    assert_eq!(at(&blue).map(|pixel| (pixel.r, pixel.b)), Some((0, 255)));
}

#[test]
fn every_theme_and_scale_lays_the_picker_out_and_paints_it() {
    for theme in [Theme::dark(), Theme::light(), high_contrast()] {
        let picker = ColourPicker::new(SLATE).with_opacity(true);
        let surface = render(&picker, WIDE, &theme);
        assert!(surface.pixels().iter().any(|pixel| pixel.a != 0));
    }
    let picker = ColourPicker::new(SLATE);
    let double = Scale::from_percent(200).expect("a scale");
    assert!(
        picker.min_width(double, &Theme::dark()) > picker.min_width(Scale::ONE, &Theme::dark())
    );
}

/// The pointer, the drag latch, the settled baseline and the remembered
/// measures are never drawn: pickers that differ only there compare equal and
/// draw the same pixels.
#[test]
fn bookkeeping_is_not_drawn() {
    let theme = Theme::dark();
    let earlier = Some(Rgba::rgb(9, 9, 9));
    let mut busy = picker(SLATE);
    busy.set_earlier(earlier);
    let layout = layout(&busy, WIDE);
    move_to(&mut busy, into(&layout, Part::Earlier, 0, 0), WIDE);
    pointer(&mut busy, &PRESS, WIDE);
    let mut resting = picker(SLATE);
    resting.set_earlier(earlier);
    // The press took the keyboard, which is drawn; the latch it armed is not.
    resting.part = Part::Earlier;
    resting.sync_children();
    assert_eq!(busy, resting);
    assert_eq!(
        render(&busy, WIDE, &theme).pixels(),
        render(&resting, WIDE, &theme).pixels()
    );
}

#[test]
fn the_wheel_steps_a_focused_number_field_under_the_pointer() {
    let mut picker = picker(SLATE);
    tab_to(&mut picker, Part::Number(2));
    let layout = layout(&picker, WIDE);
    let field = layout.numbers[2];
    move_to(
        &mut picker,
        Point::new(field.left() + 4, field.top() + 4),
        WIDE,
    );
    let detent = tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
    let wheel = InputEvent::PointerScrolled { dx: 0, dy: -detent };
    assert_eq!(
        pointer(&mut picker, &wheel, WIDE),
        PickerOutcome::Settled(Rgba::rgb(0x33, 0x66, 0x9a))
    );
    move_to(&mut picker, into(&layout, Part::Plane, 500, 500), WIDE);
    assert_eq!(
        pointer(&mut picker, &wheel, WIDE),
        PickerOutcome::Ignored,
        "the owner may scroll"
    );
}

/// The picker's lengths are measured once and kept, so a theme that changes
/// any length they are made of — here only the control height — measures
/// anew rather than laying out with the last theme's rows.
#[test]
fn a_theme_with_taller_controls_is_measured_anew() {
    let picker = picker(SLATE);
    let base = Theme::dark();
    let shipped = picker.measured_height(WIDE.width, Scale::ONE, &base);
    let mut metrics = *base.metrics();
    metrics.control_height *= 2;
    let tall = Theme::new(
        base.id(),
        base.name(),
        base.appearance(),
        *base.palette(),
        metrics,
        *base.fonts(),
        base.cursors().clone(),
        base.motion(),
        base.density(),
        base.contrast(),
    );
    assert!(picker.measured_height(WIDE.width, Scale::ONE, &tall) > shipped);
    assert_eq!(
        picker.measured_height(WIDE.width, Scale::ONE, &base),
        shipped
    );
}

/// Editing one channel of a model keeps the others as they were typed, even
/// where the colour they name rounds them away.
#[test]
fn a_models_values_are_held_as_typed() {
    let mut picker = picker(SLATE).with_model(ColourModel::Cmyk);
    tab_to(&mut picker, Part::Number(3));
    type_over(&mut picker, "100");
    assert_eq!(picker.colour(), Rgba::rgb(0, 0, 0), "full black");
    let held = readouts(&picker);
    assert_eq!(held[3], 100);
    assert!(
        held[0] > 0,
        "cyan kept as typed, though black holds no colour"
    );
    type_over(&mut picker, "0");
    assert_ne!(
        picker.colour(),
        Rgba::rgb(255, 255, 255),
        "the inks typed come back"
    );
}

#[test]
fn a_lab_value_past_srgb_is_clipped_and_marked() {
    let mut picker = picker(SLATE).with_model(ColourModel::Lab);
    tab_to(&mut picker, Part::Number(1));
    type_over(&mut picker, "127.0");
    assert!(picker.clipped());
    let theme = Theme::dark();
    let marked = render(&picker, WIDE, &theme);
    picker.set_colour(SLATE);
    assert!(!picker.clipped(), "an owner's colour is in sRGB");
    assert_ne!(marked.pixels(), render(&picker, WIDE, &theme).pixels());
}

#[test]
fn switching_the_model_shows_its_fields_and_keeps_the_colour() {
    let mut picker = picker(SLATE);
    picker.set_model(ColourModel::Lch);
    assert_eq!(picker.model(), ColourModel::Lch);
    assert_eq!(picker.colour(), SLATE);
    let wide = layout(&picker, WIDE);
    assert!(!wide.numbers[2].is_empty() && wide.numbers[3].is_empty());
    picker.set_model(ColourModel::Grey);
    let grey = layout(&picker, WIDE);
    assert!(grey.numbers[1].is_empty(), "one field");
    tab_to(&mut picker, Part::Number(0));
    type_over(&mut picker, "100");
    assert_eq!(picker.colour(), Rgba::rgb(0, 0, 0));
}

#[test]
fn the_wheel_sets_the_hue_on_its_ring_and_saturation_and_value_in_its_triangle() {
    let mut picker = picker(SLATE).with_view(PickerView::Wheel);
    let layout = layout(&picker, NARROW);
    let area = layout.inner(Part::Ring);
    let wheel = super::Wheel::in_area(area);
    let mid = wheel.outer.midpoint(wheel.inner);
    let on_ring = Point::new(
        tairix_util::mathf::round_i32(wheel.cx),
        tairix_util::mathf::round_i32(wheel.cy - mid),
    );
    assert_eq!(
        layout.part_at(on_ring, &picker.parts(), picker.hsv.hue),
        Some(Part::Ring)
    );
    move_to(&mut picker, on_ring, NARROW);
    let pressed = pointer(&mut picker, &PRESS, NARROW);
    assert!(matches!(pressed, PickerOutcome::Edited(_)));
    let degrees = picker.hsv.hue.degrees();
    assert!(
        (88..=92).contains(&degrees),
        "straight up is a quarter turn: {degrees}"
    );
    assert!(matches!(
        pointer(&mut picker, &RELEASE, NARROW),
        PickerOutcome::Settled(_)
    ));
    let [_, white, _] = wheel.corners(picker.hsv.hue);
    let near_white = Point::new(
        tairix_util::mathf::round_i32(white.0 * 0.9 + wheel.cx * 0.1),
        tairix_util::mathf::round_i32(white.1 * 0.9 + wheel.cy * 0.1),
    );
    move_to(&mut picker, near_white, NARROW);
    pointer(&mut picker, &PRESS, NARROW);
    pointer(&mut picker, &RELEASE, NARROW);
    let colour = picker.colour();
    assert!(
        colour.r > 200 && colour.g > 200 && colour.b > 200,
        "near white: {colour:?}"
    );
    let theme = Theme::dark();
    let pure_green = premul(tairix_colour::Rgba::rgb(0, 255, 0));
    assert!(
        has_pixel(&render(&picker, NARROW, &theme), pure_green),
        "the ring shows every hue"
    );
}

#[test]
fn a_slider_sets_its_channel_and_draws_it_swept() {
    let mut picker = picker(SLATE)
        .with_view(PickerView::Sliders)
        .with_opacity(true);
    let layout = layout(&picker, NARROW);
    assert!(layout.hue.is_empty(), "no hue strip");
    assert!(
        layout.alpha.width > layout.alpha.height,
        "the opacity is a slider too"
    );
    let red = layout.inner(Part::Track(0));
    move_to(
        &mut picker,
        Point::new(red.right() - 1, red.top() + 2),
        NARROW,
    );
    pointer(&mut picker, &PRESS, NARROW);
    assert_eq!(
        pointer(&mut picker, &RELEASE, NARROW),
        PickerOutcome::Settled(Rgba::rgb(255, 0x66, 0x99))
    );
    tab_to(&mut picker, Part::Track(1));
    assert_eq!(
        named(&mut picker, NamedKey::Home),
        PickerOutcome::Settled(Rgba::rgb(255, 0, 0x99))
    );
    let theme = Theme::dark();
    let surface = render(&picker, NARROW, &theme);
    let groove = layout.inner(Part::Track(2));
    let at = |x: i32| {
        surface
            .get(
                u32::try_from(x).expect("on"),
                u32::try_from(groove.top() + 2).expect("on"),
            )
            .expect("drawn")
    };
    assert_ne!(
        at(groove.left() + 2),
        at(groove.right() - 3),
        "swept from no blue to all"
    );
}
