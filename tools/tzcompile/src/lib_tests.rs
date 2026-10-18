//! Host tests of the compiler's own refusals: each input `zic` would refuse,
//! or the store could not hold, stops the build with its reason.

use tairix_tz::write::{build, StoreInput, TransitionInput, ZoneInput};

use super::{
    aliases, check_continuations, compile_zone, plans, rule_book, text, CompileError, Pinned,
};
use crate::source::parse;

/// The first zone of `source`, compiled.
fn compiled(source: &str) -> Result<ZoneInput, CompileError> {
    let source = parse(source).expect("the source parses");
    let book = rule_book(&source.rules);
    compile_zone(&source.zones[0], &book)
}

#[test]
fn an_input_that_does_not_hash_to_its_pin_is_refused() {
    let pinned = |sha256| Pinned {
        path: "data/x",
        bytes: b"data",
        sha256,
    };
    // SHA-256("data").
    let right = "3a6eb0790f39ac87c94f3856b2dd2c5d110e6811602261a9a923d3bb23adc8b7";
    assert_eq!(text(&pinned(right)), Ok("data"));
    let wrong = "3a6eb0790f39ac87c94f3856b2dd2c5d110e6811602261a9a923d3bb23adc8b6";
    assert_eq!(text(&pinned(wrong)), Err(CompileError::Pin("data/x")));
    assert_eq!(text(&pinned("not hex")), Err(CompileError::Pin("data/x")));
}

#[test]
fn an_era_naming_neither_rules_nor_a_saving_is_refused() {
    let source = parse("Z T/Nameless 0 Nope XST\n").expect("the source parses");
    let book = rule_book(&source.rules);
    assert_eq!(
        plans(&source.zones[0], &book).map(|_| ()),
        Err(CompileError::Saving {
            zone: String::from("T/Nameless"),
            saving: String::from("Nope"),
        })
    );
}

#[test]
fn letters_in_an_era_without_rules_are_refused() {
    assert_eq!(
        compiled("Z T/Lettered 0 - X%sT\n"),
        Err(CompileError::RulelessLetters(String::from("T/Lettered")))
    );
}

#[test]
fn an_offset_the_store_cannot_hold_is_refused() {
    assert_eq!(
        compiled("Z T/Far 1000000 - XST\n"),
        Err(CompileError::Range(String::from("T/Far")))
    );
}

#[test]
fn links_are_followed_to_their_zone_and_a_cycle_is_refused() {
    let chained = parse("Z A/Zone 0 - XST\nL A/Zone B/Link\nL B/Link C/Link\n").expect("parses");
    assert_eq!(
        aliases(&chained),
        Ok(vec![
            (String::from("B/Link"), String::from("A/Zone")),
            (String::from("C/Link"), String::from("A/Zone")),
        ])
    );
    // A link line names its target first.
    let cycle = parse("L A/Link B/Link\nL B/Link A/Link\n").expect("parses");
    assert_eq!(
        aliases(&cycle),
        Err(CompileError::LinkCycle(String::from("B/Link")))
    );
}

/// A zone whose rules would read otherwise a second after its last transition
/// than that transition does is refused once the store is built.
#[test]
fn a_last_transition_its_rules_contradict_is_refused() {
    let mut input: StoreInput = tairix_tz::testkit::store_input();
    input.zones.retain(|zone| zone.name == "Europe/London");
    input.aliases.clear();
    input.places.retain(|place| place.zone == "Europe/London");
    let consistent = build(&input).expect("London alone builds");
    assert_eq!(check_continuations(&input, &consistent), Ok(()));
    let london = input
        .zones
        .iter_mut()
        .find(|zone| zone.name == "Europe/London")
        .expect("London");
    let last: &mut TransitionInput = london.transitions.last_mut().expect("a transition");
    last.offset = 3600;
    last.abbreviation = String::from("BST");
    last.daylight = true;
    let contradicted = build(&input).expect("the store holds it");
    assert_eq!(
        check_continuations(&input, &contradicted),
        Err(CompileError::Continues(String::from("Europe/London")))
    );
}
