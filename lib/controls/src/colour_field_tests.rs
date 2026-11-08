//! The colour field: its spelling, live typing, settling, what it refuses,
//! and the swatch it draws.

use alloc::vec::Vec;

use tairix_colour::Rgba;
use tairix_geometry::{Rect, Scale};
use tairix_input::{Key, Modifiers, NamedKey};
use tairix_raster::Surface;
use tairix_theme::Theme;

use crate::colour_field::{ColourAction, ColourField};
use crate::damage;
use crate::state::ValidationState;
use crate::testkit::premul;

const BOUNDS: Rect = Rect::new(0, 0, 140, 28);

const BLUE: Rgba = Rgba::new(0x4a, 0x8f, 0xe2, 0xff);

fn field(colour: Rgba, opacity: bool) -> ColourField {
    let mut field = ColourField::new(colour, opacity);
    field.set_focused(true);
    field
}

fn key(field: &mut ColourField, key: Key) -> Option<ColourAction> {
    field.on_key(key, Modifiers::default(), BOUNDS, &mut damage::sink())
}

fn named(field: &mut ColourField, named: NamedKey) -> Option<ColourAction> {
    key(field, Key::Named(named))
}

/// Type `text` over the whole of what the field holds, answering what each
/// character did.
fn retyped(field: &mut ColourField, text: &str) -> Vec<Option<ColourAction>> {
    named(field, NamedKey::End);
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    field.on_key(
        Key::Named(NamedKey::Home),
        shift,
        BOUNDS,
        &mut damage::sink(),
    );
    text.chars().map(|c| key(field, Key::Char(c))).collect()
}

fn render(field: &ColourField, theme: &Theme) -> Surface {
    let mut surface = Surface::new(BOUNDS.width, BOUNDS.height).expect("surface");
    field.render(&mut surface, BOUNDS, Scale::ONE, theme);
    surface
}

#[test]
fn a_field_spells_its_colour_and_drops_an_alpha_it_cannot_hold() {
    let translucent = BLUE.with_alpha(0x80);
    let opaque = ColourField::new(translucent, false);
    assert_eq!((opaque.colour(), opaque.text()), (BLUE, "#4a8fe2"));
    let held = ColourField::new(translucent, true);
    assert_eq!((held.colour(), held.text()), (translucent, "#4a8fe280"));
    assert!(held.has_opacity() && !opaque.has_opacity());
    let mut set = ColourField::new(BLUE, false);
    set.set_colour(translucent);
    assert_eq!(set.colour(), BLUE, "set as the field holds it");
}

#[test]
fn a_spelling_takes_effect_as_it_is_typed_and_settles_once() {
    let mut field = field(BLUE, false);
    let edits = retyped(&mut field, "#f00");
    let red = Rgba::new(0xff, 0, 0, 0xff);
    assert_eq!(
        edits.last().copied().flatten(),
        Some(ColourAction::Edited { colour: red }),
        "three digits spell a colour"
    );
    assert_eq!(field.colour(), red);
    assert_eq!(
        named(&mut field, NamedKey::Enter),
        Some(ColourAction::Settled { colour: red })
    );
    assert_eq!(field.text(), "#ff0000", "settled to its canonical spelling");
    assert_eq!(named(&mut field, NamedKey::Enter), None, "settled once");
}

#[test]
fn an_unreadable_spelling_moves_nothing_until_committed_back() {
    let mut field = field(BLUE, false);
    let edits = retyped(&mut field, "#12");
    assert!(edits.iter().all(Option::is_none), "{edits:?}");
    assert_eq!(field.colour(), BLUE);
    assert_eq!(field.state().validation, ValidationState::Invalid);
    assert_eq!(
        field.commit(BOUNDS, &mut damage::sink()),
        None,
        "nothing moved"
    );
    assert_eq!(field.text(), "#4a8fe2", "the held colour is shown again");
    assert_eq!(field.state().validation, ValidationState::Valid);
}

#[test]
fn escape_takes_typing_back_to_the_last_settle() {
    let mut field = field(BLUE, false);
    retyped(&mut field, "000000");
    assert_eq!(field.colour(), Rgba::new(0, 0, 0, 0xff), "a # is optional");
    assert_eq!(
        named(&mut field, NamedKey::Escape),
        Some(ColourAction::Settled { colour: BLUE })
    );
    assert_eq!(field.text(), "#4a8fe2");
    assert_eq!(
        named(&mut field, NamedKey::Escape),
        None,
        "nothing left to take back, so the owner may close"
    );
}

#[test]
fn a_field_refuses_what_cannot_spell_a_colour_and_an_alpha_it_does_not_hold() {
    let mut field = field(BLUE, false);
    assert_eq!(key(&mut field, Key::Char('g')), None);
    assert_eq!(field.text(), "#4a8fe2", "a non-digit is refused whole");
    let edits = retyped(&mut field, "#11223344");
    assert_eq!(
        edits.last().copied().flatten(),
        None,
        "no alpha in an opaque field"
    );
    assert_eq!(field.state().validation, ValidationState::Invalid);
    assert_eq!(
        field.colour(),
        Rgba::new(0x11, 0x22, 0x33, 0xff),
        "the last spelling that read as a colour"
    );
    let mut translucent = self::field(BLUE, true);
    let edits = retyped(&mut translucent, "#11223344");
    assert_eq!(
        edits.last().copied().flatten(),
        Some(ColourAction::Edited {
            colour: Rgba::new(0x11, 0x22, 0x33, 0x44)
        })
    );
    let pasted = translucent.insert_text("zz", BOUNDS, &mut damage::sink());
    assert_eq!(
        pasted, None,
        "a paste that cannot spell a colour is refused"
    );
    let unfocused = &mut ColourField::new(BLUE, false);
    assert_eq!(
        key(unfocused, Key::Char('1')),
        None,
        "no keyboard, no typing"
    );
}

#[test]
fn the_swatch_shows_the_colour_and_a_checker_beneath_a_translucent_one() {
    let theme = Theme::light();
    let opaque = render(&field(BLUE, false), &theme);
    let centre = BOUNDS.height / 2;
    assert_eq!(opaque.get(centre, centre), Some(premul(BLUE)));
    let clear = render(&ColourField::new(BLUE.with_alpha(0x40), true), &theme);
    let mut seen = alloc::collections::BTreeSet::new();
    for x in 3..BOUNDS.height - 3 {
        for y in 3..BOUNDS.height - 3 {
            if let Some(pixel) = clear.get(x, y) {
                seen.insert((pixel.r, pixel.g, pixel.b, pixel.a));
            }
        }
    }
    assert!(seen.len() >= 2, "the checker shows through: {seen:?}");
}
