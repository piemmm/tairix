//! Unit tests for the picture choice (spec §11.43): its layout across widths
//! and sections, the one picture size an owner renders at, choosing by the
//! pointer and the keyboard, the fail-closed refusals, the damage a gesture
//! reports, the chosen picture's mark under every contrast policy, and the
//! choice seated in a [`FieldGroup`] beneath its rows.

use alloc::vec;
use alloc::vec::Vec;

use tairix_geometry::{Point, Rect, Scale};
use tairix_icon::IconKind;
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;

use crate::damage::sink;
use crate::form::{FieldAction, FieldControl, FieldGroup, FieldGroupAction, FieldLayout, FieldRow};
use crate::picture::{Aspect, PictureAction, PictureChoice, PictureItem, PictureSection, Swatch};
use crate::selector::Toggle;
use crate::state::{AuthorityState, ControlState};
use crate::testkit::{high_contrast, keystroke, monochrome, premul};

const WIDE: u32 = 560;

fn items(labels: &[&str]) -> Vec<PictureItem> {
    labels
        .iter()
        .map(|label| PictureItem::new(*label, IconKind::Image))
        .collect()
}

/// Two titled sections of three and four pictures, then an untitled one of one.
fn choice() -> PictureChoice {
    PictureChoice::new(
        Aspect::WIDESCREEN,
        vec![
            PictureSection::new("Abstract", items(&["a0", "a1", "a2"])),
            PictureSection::new("Nature", items(&["n0", "n1", "n2", "n3"])),
            PictureSection::untitled(items(&["u0"])),
        ],
    )
}

fn bounds(choice: &PictureChoice, width: u32, theme: &Theme) -> Rect {
    Rect::new(
        10,
        20,
        width,
        choice.measured_height(width, Scale::ONE, theme),
    )
}

fn rect(choice: &PictureChoice, index: usize, at: Rect, theme: &Theme) -> Rect {
    choice
        .item_rect(index, at, Scale::ONE, theme)
        .expect("a laid-out picture")
}

fn press_release(
    choice: &mut PictureChoice,
    at: Rect,
    down: Point,
    up: Point,
) -> Option<PictureAction> {
    let theme = Theme::dark();
    let mut damage = sink();
    for event in [
        InputEvent::PointerMoved { to: down },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerMoved { to: up },
    ] {
        assert_eq!(
            choice.on_pointer(&event, at, Scale::ONE, &theme, &mut damage),
            None
        );
    }
    choice.on_pointer(
        &InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
        at,
        Scale::ONE,
        &theme,
        &mut damage,
    )
}

/// A coordinate the fixture lays out on the surface, as a surface column or row.
fn on(coordinate: i32) -> u32 {
    u32::try_from(coordinate).expect("on the surface")
}

fn key(choice: &mut PictureChoice, at: Rect, named: NamedKey) -> Option<PictureAction> {
    choice.on_key(
        Key::Named(named),
        at,
        Scale::ONE,
        &Theme::dark(),
        &mut sink(),
    )
}

#[test]
fn every_picture_is_the_aspect_the_choice_names() {
    let theme = Theme::dark();
    for scale in [Scale::ONE, Scale::from_percent(200).expect("a scale")] {
        let (width, height) = choice().picture_size(scale, &theme);
        assert_eq!(
            height,
            (width * 9 + 8) / 16,
            "{width}x{height} at {}%",
            scale.percent()
        );
    }
    let square = PictureChoice::new(Aspect::new(1, 1).expect("an aspect"), Vec::new());
    let (width, height) = square.picture_size(Scale::ONE, &theme);
    assert_eq!(width, height);
    assert_eq!(Aspect::new(0, 9), None);
}

#[test]
fn pictures_wrap_into_lines_under_their_sections_and_resolve_back() {
    let theme = Theme::dark();
    let choice = choice();
    let at = bounds(&choice, WIDE, &theme);
    let (first, second) = (rect(&choice, 0, at, &theme), rect(&choice, 1, at, &theme));
    assert_eq!(first.top(), second.top(), "one line");
    assert!(second.left() > first.right(), "apart");
    // A section starts a line of its own beneath the last.
    let nature = rect(&choice, 3, at, &theme);
    assert!(nature.top() > first.bottom());
    assert!(nature.left() <= first.left());
    // The untitled section's one picture is the last thing laid out.
    let last = rect(&choice, 7, at, &theme);
    assert_eq!(last.bottom(), at.bottom());
    assert_eq!(choice.item_rect(8, at, Scale::ONE, &theme), None);
    for index in 0..choice.len() {
        let tile = rect(&choice, index, at, &theme);
        assert_eq!(
            choice.item_at(tile.center(), at, Scale::ONE, &theme),
            Some(index)
        );
    }
    // A heading, and the gap between two pictures, choose nothing.
    assert_eq!(
        choice.item_at(
            Point::new(first.left() + 2, at.top() + 1),
            at,
            Scale::ONE,
            &theme
        ),
        None
    );
    assert_eq!(
        choice.item_at(
            Point::new(first.right() + 1, first.top() + 4),
            at,
            Scale::ONE,
            &theme
        ),
        None
    );
}

#[test]
fn a_narrower_column_takes_more_lines_and_so_more_height() {
    let theme = Theme::dark();
    let choice = choice();
    let tile = choice.natural_width(Scale::ONE, &theme);
    let narrow = choice.measured_height(tile, Scale::ONE, &theme);
    let wide = choice.measured_height(WIDE, Scale::ONE, &theme);
    assert!(narrow > wide);
    // A column too narrow for one picture seats none.
    let starved = bounds(&choice, tile - 1, &theme);
    assert_eq!(choice.item_rect(0, starved, Scale::ONE, &theme), None);
}

#[test]
fn a_press_released_on_its_picture_chooses_it_and_elsewhere_chooses_nothing() {
    let theme = Theme::dark();
    let mut choice = choice();
    let at = bounds(&choice, WIDE, &theme);
    let (one, two) = (rect(&choice, 1, at, &theme), rect(&choice, 2, at, &theme));
    assert_eq!(
        press_release(&mut choice, at, one.center(), two.center()),
        None
    );
    assert_eq!(choice.selected(), None);
    assert_eq!(
        press_release(&mut choice, at, one.center(), one.center()),
        Some(PictureAction::Chose { index: 1 })
    );
    assert_eq!(choice.selected(), Some(1));
    // Choosing what is already chosen asks for nothing.
    assert_eq!(
        press_release(&mut choice, at, one.center(), one.center()),
        None
    );
}

#[test]
fn a_denied_or_disabled_choice_refuses_the_pointer_and_the_keyboard() {
    let theme = Theme::dark();
    for state in [
        ControlState::disabled(),
        ControlState::idle().with_authority(AuthorityState::Denied),
    ] {
        let mut choice = choice();
        choice.set_state(state);
        let at = bounds(&choice, WIDE, &theme);
        let tile = rect(&choice, 2, at, &theme);
        assert_eq!(
            press_release(&mut choice, at, tile.center(), tile.center()),
            None
        );
        choice.set_focused(true);
        assert_eq!(key(&mut choice, at, NamedKey::Right), None);
        assert_eq!(key(&mut choice, at, NamedKey::Enter), None);
        assert_eq!(choice.selected(), None);
    }
}

#[test]
fn a_hover_reports_the_two_pictures_it_moves_between_and_nothing_else() {
    let theme = Theme::dark();
    let mut choice = choice();
    let at = bounds(&choice, WIDE, &theme);
    let (zero, one) = (rect(&choice, 0, at, &theme), rect(&choice, 1, at, &theme));
    let mut damage = sink();
    choice.on_pointer(
        &InputEvent::PointerMoved { to: zero.center() },
        at,
        Scale::ONE,
        &theme,
        &mut damage,
    );
    assert_eq!(damage.rects(), &[zero]);
    let mut damage = sink();
    choice.on_pointer(
        &InputEvent::PointerMoved {
            to: Point::new(zero.center().x + 1, zero.center().y),
        },
        at,
        Scale::ONE,
        &theme,
        &mut damage,
    );
    assert!(
        damage.is_empty(),
        "motion within one picture changes nothing"
    );
    let mut damage = sink();
    choice.on_pointer(
        &InputEvent::PointerMoved { to: one.center() },
        at,
        Scale::ONE,
        &theme,
        &mut damage,
    );
    assert!(damage.rects().iter().all(|r| *r == zero || *r == one));
    assert_eq!(damage.rects().len(), 2);
}

#[test]
fn the_arrows_walk_the_pictures_across_sections_and_stop_at_the_ends() {
    let theme = Theme::dark();
    let mut choice = choice().with_selected(Some(4));
    let at = bounds(&choice, WIDE, &theme);
    choice.set_focused(true);
    assert_eq!(
        choice.cursor(),
        4,
        "the cursor arrives on the chosen picture"
    );
    // Up from Nature's first line lands in the same slot of Abstract's last.
    assert_eq!(
        key(&mut choice, at, NamedKey::Up),
        Some(PictureAction::Moved { index: 1 })
    );
    // Up from the first line has nowhere to go and says so.
    assert_eq!(key(&mut choice, at, NamedKey::Up), None);
    assert_eq!(
        key(&mut choice, at, NamedKey::Left),
        Some(PictureAction::Moved { index: 0 })
    );
    assert_eq!(key(&mut choice, at, NamedKey::Left), None);
    assert_eq!(
        key(&mut choice, at, NamedKey::Down),
        Some(PictureAction::Moved { index: 3 })
    );
    assert_eq!(
        key(&mut choice, at, NamedKey::End),
        Some(PictureAction::Moved { index: 7 })
    );
    assert_eq!(key(&mut choice, at, NamedKey::Down), None);
    assert_eq!(key(&mut choice, at, NamedKey::Right), None);
    assert_eq!(
        key(&mut choice, at, NamedKey::Home),
        Some(PictureAction::Moved { index: 0 })
    );
    assert_eq!(
        key(&mut choice, at, NamedKey::Enter),
        Some(PictureAction::Chose { index: 0 })
    );
    assert_eq!(choice.selected(), Some(0));
    let mut unfocused = self::choice();
    assert_eq!(
        key(&mut unfocused, at, NamedKey::Right),
        None,
        "not holding the keyboard"
    );
}

#[test]
fn a_line_step_down_a_section_clamps_to_its_shorter_last_line() {
    let theme = Theme::dark();
    // One line holds three at this width, so Nature's four take two lines.
    let mut choice = choice();
    let at = bounds(&choice, WIDE, &theme);
    let per_line = (0..choice.len())
        .take_while(|&index| {
            rect(&choice, index, at, &theme).top() == rect(&choice, 0, at, &theme).top()
        })
        .count();
    assert_eq!(per_line, 3, "the fixture's width seats three a line");
    choice.set_selected(Some(5));
    choice.set_focused(true);
    assert_eq!(
        key(&mut choice, at, NamedKey::Down),
        Some(PictureAction::Moved { index: 6 })
    );
    // From the short last line, Down leaves the section for the next one.
    assert_eq!(
        key(&mut choice, at, NamedKey::Down),
        Some(PictureAction::Moved { index: 7 })
    );
}

#[test]
fn a_picture_is_drawn_only_at_the_size_it_was_asked_for_with_rounded_corners() {
    let theme = Theme::dark();
    let mut choice = choice();
    let (width, height) = choice.picture_size(Scale::ONE, &theme);
    let red = Color::rgb(0xE0, 0x10, 0x10);
    let mut art = Surface::new(width, height).expect("art");
    art.fill(red);
    assert!(choice.set_art(0, art));
    let mut stale = Surface::new(width + 4, height).expect("art");
    stale.fill(red);
    assert!(choice.set_art(1, stale));
    assert!(!choice.set_art(99, Surface::new(1, 1).expect("art")));
    let at = Rect::new(0, 0, WIDE, choice.measured_height(WIDE, Scale::ONE, &theme));
    let mut surface = Surface::new(WIDE, at.height).expect("surface");
    choice.render(&mut surface, at, Scale::ONE, &theme);
    let count = |tile: Rect| {
        let mut n = 0;
        for y in 0..tile.height {
            for x in 0..tile.width {
                let px = surface
                    .get(on(tile.left()) + x, on(tile.top()) + y)
                    .expect("in bounds");
                if px == red.premultiply() {
                    n += 1;
                }
            }
        }
        n
    };
    let (zero, one) = (rect(&choice, 0, at, &theme), rect(&choice, 1, at, &theme));
    // The whole picture shows but for its rounded corners.
    let shown = count(zero);
    assert!(
        shown > 0 && shown < width * height,
        "{shown} of {}",
        width * height
    );
    // A picture of the wrong size draws the glyph instead.
    assert_eq!(count(one), 0);
    assert_eq!(choice.take_art(0).map(|art| art.width()), Some(width));
    assert_eq!(choice.take_art(0), None);
}

#[test]
fn one_walk_lays_every_picture_where_each_is_asked_for() {
    let theme = Theme::dark();
    let choice = choice();
    for width in [WIDE, 200] {
        let at = bounds(&choice, width, &theme);
        let mut walked = Vec::new();
        choice.for_each_item_rect(at, Scale::ONE, &theme, |index, rect| {
            walked.push((index, rect));
        });
        let asked: Vec<_> = (0..choice.len())
            .map(|index| (index, rect(&choice, index, at, &theme)))
            .collect();
        assert_eq!(walked, asked, "{width} wide");
    }
}

/// A swatch is its own picture: it draws its colour where a picture would go,
/// takes no picture, and a desktop swatch is the desktop of the theme it is
/// drawn in.
#[test]
fn a_swatch_draws_its_colour_and_takes_no_picture() {
    let teal = tairix_colour::Rgba::rgb(0x10, 0x80, 0x80);
    let mut choice = PictureChoice::new(
        Aspect::WIDESCREEN,
        vec![PictureSection::untitled(vec![
            PictureItem::swatch("Teal", Swatch::Desktop),
            PictureItem::swatch("Desktop", Swatch::Desktop),
        ])],
    );
    let theme = Theme::dark();
    let (width, height) = choice.picture_size(Scale::ONE, &theme);
    assert!(!choice.set_art(0, Surface::new(width, height).expect("art")));
    assert!(
        choice.set_swatch(0, Swatch::Fixed(teal)),
        "a swatch repaints in place"
    );
    assert!(!choice.set_swatch(9, Swatch::Desktop), "no such choice");
    assert_eq!(choice.take_art(0), None);
    assert!(choice
        .item(0)
        .is_some_and(|item| !item.takes_art() && item.art().is_none()));
    for theme in [Theme::dark(), Theme::light()] {
        let at = Rect::new(0, 0, WIDE, choice.measured_height(WIDE, Scale::ONE, &theme));
        let mut surface = Surface::new(WIDE, at.height).expect("surface");
        choice.render(&mut surface, at, Scale::ONE, &theme);
        for (index, want) in [(0, teal), (1, theme.palette().desktop)] {
            let tile = rect(&choice, index, at, &theme);
            let centre = surface
                .get(
                    on(tile.left()) + tile.width / 2,
                    on(tile.top()) + height / 2,
                )
                .expect("in bounds");
            assert_eq!(centre, premul(want), "swatch {index}");
        }
    }
}

#[test]
fn the_chosen_picture_wears_the_accent_under_every_contrast_policy() {
    for theme in [Theme::dark(), Theme::light(), high_contrast(), monochrome()] {
        let chosen = choice().with_selected(Some(0));
        let at = Rect::new(0, 0, WIDE, chosen.measured_height(WIDE, Scale::ONE, &theme));
        let accent = premul(theme.palette().accent);
        let tile = rect(&chosen, 0, at, &theme);
        let lit = |choice: &PictureChoice| {
            let mut surface = Surface::new(WIDE, at.height).expect("surface");
            choice.render(&mut surface, at, Scale::ONE, &theme);
            (0..tile.height).any(|y| {
                (0..tile.width)
                    .any(|x| surface.get(on(tile.left()) + x, on(tile.top()) + y) == Some(accent))
            })
        };
        assert!(
            lit(&chosen),
            "{} draws no mark on the chosen picture",
            theme.name()
        );
        assert!(
            !lit(&choice()),
            "{} marks a picture nobody chose",
            theme.name()
        );
    }
}

/// The keyboard never goes unseen on the chosen tile, the one a chooser's
/// cursor arrives on, whichever contrast policy marks the choice.
#[test]
fn the_keyboard_shows_on_the_chosen_tile_under_every_contrast_policy() {
    for theme in [Theme::dark(), Theme::light(), high_contrast(), monochrome()] {
        let mut chosen = choice().with_selected(Some(0));
        let at = Rect::new(0, 0, WIDE, chosen.measured_height(WIDE, Scale::ONE, &theme));
        let draw = |choice: &PictureChoice| {
            let mut surface = Surface::new(WIDE, at.height).expect("surface");
            choice.render(&mut surface, at, Scale::ONE, &theme);
            surface
        };
        let resting = draw(&chosen);
        chosen.set_focused(true);
        let focused = draw(&chosen);
        let tile = rect(&chosen, 0, at, &theme);
        let shown = (0..tile.height).any(|y| {
            (0..tile.width).any(|x| {
                let (x, y) = (on(tile.left()) + x, on(tile.top()) + y);
                resting.get(x, y) != focused.get(x, y)
            })
        });
        assert!(
            shown,
            "{} hides the keyboard on the chosen tile",
            theme.name()
        );
    }
}

/// The chosen tile wears one edge, round its picture and its name alike; the
/// keyboard on it weighs that edge rather than adding a second, and a tile
/// that is not the choice wears the cursor's own ring.
#[test]
fn the_chosen_tile_wears_one_ring_round_its_picture_and_name() {
    let theme = Theme::dark();
    let palette = theme.palette();
    let (accent, focus) = (premul(palette.accent), premul(palette.rim_active));
    assert_ne!(accent, focus, "the fixture must tell the two rings apart");
    let mut chosen = choice().with_selected(Some(1));
    chosen.set_focused(true);
    let at = Rect::new(0, 0, WIDE, chosen.measured_height(WIDE, Scale::ONE, &theme));
    let draw = |choice: &PictureChoice| {
        let mut surface = Surface::new(WIDE, at.height).expect("surface");
        choice.render(&mut surface, at, Scale::ONE, &theme);
        surface
    };
    let tile = rect(&chosen, 1, at, &theme);
    let (left, top) = (on(tile.left()), on(tile.top()));
    let (right, bottom) = (left + tile.width - 1, top + tile.height - 1);
    let (middle_x, middle_y) = (left + tile.width / 2, top + tile.height / 2);
    let wears = |surface: &Surface, colour| {
        (top..=bottom).any(|y| (left..=right).any(|x| surface.get(x, y) == Some(colour)))
    };
    let weight = |surface: &Surface| {
        (top..=bottom)
            .take_while(|&y| surface.get(middle_x, y) == Some(accent))
            .count()
    };

    chosen.set_cursor(1);
    let surface = draw(&chosen);
    for (x, y) in [
        (middle_x, top),
        (middle_x, bottom),
        (left, middle_y),
        (right, middle_y),
    ] {
        assert_eq!(
            surface.get(x, y),
            Some(accent),
            "the ring runs along the tile's own edge at ({x}, {y}), below the name too"
        );
    }
    assert!(
        !wears(&surface, focus),
        "the chosen tile under the cursor takes a second ring"
    );
    let held = weight(&surface);

    chosen.set_cursor(0);
    let surface = draw(&chosen);
    assert!(
        !wears(&surface, focus),
        "the chosen tile takes the cursor's ring"
    );
    let resting = weight(&surface);
    let border = crate::paint::plate_border(&theme, Scale::ONE).max(1);
    assert_eq!(
        held,
        resting + border as usize,
        "the keyboard on the chosen tile weighs its one ring by the focus ring's own"
    );
    chosen.set_focused(false);
    assert_eq!(weight(&draw(&chosen)), resting);
    chosen.set_focused(true);
    chosen.set_cursor(0);
    let other = rect(&chosen, 0, at, &theme);
    assert_eq!(
        surface.get(on(other.left()) + other.width / 2, on(other.top())),
        Some(focus),
        "the tile the cursor rests on shows it"
    );
}

#[test]
fn a_group_seats_the_choice_beneath_its_rows_and_walks_into_and_out_of_it() {
    let theme = Theme::dark();
    let row = FieldRow::new("Start after", FieldControl::Toggle(Toggle::new("", true)));
    let bare = FieldGroup::new("SCREENSAVER", vec![row.clone()]);
    let mut group =
        FieldGroup::new("SCREENSAVER", vec![row]).with_pictures(choice().with_selected(Some(2)));
    assert_eq!(group.len(), 2);
    let tall = group.measured_height(WIDE, 0, Scale::ONE, &theme);
    assert!(tall > bare.measured_height(WIDE, 0, Scale::ONE, &theme));
    let layout = FieldLayout::new(
        Rect::new(0, 0, WIDE, tall),
        group.slot_column(WIDE, Scale::ONE, &theme),
    );
    group.adopt_focus(Some(0));
    let mut damage = sink();
    let walk = |group: &mut FieldGroup, named: NamedKey, damage: &mut _| {
        group.on_key(
            keystroke(Key::Named(named)),
            layout,
            Scale::ONE,
            &theme,
            damage,
        )
    };
    assert_eq!(walk(&mut group, NamedKey::Down, &mut damage), None);
    assert_eq!(
        group.focus(),
        Some(1),
        "Down from the last row reaches the pictures"
    );
    let pictures = group
        .row_rect(1, layout, Scale::ONE, &theme)
        .expect("the choice's rect");
    let cursor = group
        .focus_rect(layout, Scale::ONE, &theme)
        .expect("the cursor's picture");
    assert!(
        pictures.height > cursor.height,
        "the cursor is one picture of many"
    );
    assert_eq!(
        walk(&mut group, NamedKey::Right, &mut damage),
        Some(FieldGroupAction {
            row: 1,
            action: FieldAction::Browsed { index: 3 }
        })
    );
    assert_eq!(
        walk(&mut group, NamedKey::Enter, &mut damage),
        Some(FieldGroupAction {
            row: 1,
            action: FieldAction::Selected { index: 3 }
        })
    );
    // Up from the choice's first line steps back onto the row above it.
    for _ in 0..2 {
        let _ = walk(&mut group, NamedKey::Up, &mut damage);
    }
    assert_eq!(group.focus(), Some(0));
    // A press on a picture reaches the choice through the group.
    let tile = group
        .pictures()
        .expect("pictures")
        .item_rect(5, pictures, Scale::ONE, &theme)
        .expect("a picture");
    let mut fired = None;
    for event in [
        InputEvent::PointerMoved { to: tile.center() },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ] {
        fired = group
            .on_pointer(&event, layout, Scale::ONE, &theme, &mut damage)
            .or(fired);
    }
    assert_eq!(
        fired,
        Some(FieldGroupAction {
            row: 1,
            action: FieldAction::Selected { index: 5 }
        })
    );
    assert_eq!(
        group.row_at(layout, Scale::ONE, &theme, tile.center()),
        Some(1)
    );
}

#[test]
fn a_group_of_pictures_alone_hands_the_keyboard_on_at_either_end() {
    let theme = Theme::dark();
    let mut group = FieldGroup::new("DESKTOP PICTURE", Vec::new()).with_pictures(choice());
    let tall = group.measured_height(WIDE, 0, Scale::ONE, &theme);
    let layout = FieldLayout::new(Rect::new(0, 0, WIDE, tall), 0);
    group.adopt_focus(Some(0));
    assert_eq!(group.focus(), Some(0));
    let mut damage = sink();
    assert_eq!(
        group.on_key(
            keystroke(Key::Named(NamedKey::Up)),
            layout,
            Scale::ONE,
            &theme,
            &mut damage
        ),
        None
    );
    // Nothing moved, so the pane above carries the cursor on.
    assert_eq!(group.focus(), Some(0));
}
