extern crate alloc;

use super::*;

fn word(text: &str) -> OptionKey {
    OptionKey::new(text).expect("a word")
}

fn label<const MIN: usize, const MAX: usize>(text: &str) -> BoundedText<MIN, MAX> {
    BoundedText::new(text).expect("a label")
}

fn choice(value: &str, text: &str) -> Choice {
    Choice {
        value: ChoiceValue::new(value).expect("a value"),
        label: label(text),
    }
}

fn detail() -> SaverOption {
    let choices = Choices::new(&[choice("simple", "Simple"), choice("maximum", "Maximum")])
        .expect("two choices");
    SaverOption::new(
        word("detail"),
        (label("Detail"), label("How much each scene sets out.")),
        SaverOptionKind::Choice {
            choices,
            default: 1,
        },
    )
    .expect("an option")
}

fn interval() -> SaverOption {
    SaverOption::new(
        word("interval_s"),
        (label("Change every"), BoundedText::EMPTY),
        SaverOptionKind::Range {
            range: Range::new(5, 300, 5).expect("a range"),
            default: 30,
            unit: label("seconds"),
        },
    )
    .expect("an option")
}

fn keep() -> SaverOption {
    SaverOption::new(
        word("keep"),
        (
            label("Keep finished pictures"),
            label("Kept as a PNG in your Pictures."),
        ),
        SaverOptionKind::Switch { default: true },
    )
    .expect("an option")
}

#[test]
fn every_kind_of_option_round_trips_its_entry() {
    for option in [detail(), interval(), keep()] {
        let entry = option.to_entry();
        assert_eq!(SaverOption::from_entry(&entry), Ok(option));
    }
}

#[test]
fn a_word_is_lower_case_ascii_led_by_a_letter() {
    for good in ["a", "interval_s", "tasks", "x9_y"] {
        assert!(OptionKey::new(good).is_ok(), "{good}");
    }
    for bad in [
        "",
        "Detail",
        "9lives",
        "_lead",
        "two words",
        "dash-ed",
        "dot.ted",
        "é",
    ] {
        assert!(OptionKey::new(bad).is_err(), "{bad:?}");
    }
    let long = "a".repeat(SAVER_OPTION_KEY_MAX + 1);
    assert_eq!(OptionKey::new(&long), Err(Errno::LengthOutOfRange));
}

#[test]
fn a_choice_offers_two_to_twelve_distinct_labelled_values() {
    assert_eq!(
        Choices::new(&[choice("one", "One")]),
        Err(Errno::LengthOutOfRange)
    );
    let past: alloc::vec::Vec<Choice> = (0..=SAVER_MAX_CHOICES)
        .map(|n| choice(&alloc::format!("v{n}"), "V"))
        .collect();
    assert_eq!(Choices::new(&past), Err(Errno::LengthOutOfRange));
    assert_eq!(
        Choices::new(&[choice("same", "A"), choice("same", "B")]),
        Err(Errno::OutOfRange)
    );
    let unlabelled = Choice {
        value: ChoiceValue::new("bare").expect("a value"),
        label: BoundedText::EMPTY,
    };
    assert_eq!(
        Choices::new(&[choice("one", "One"), unlabelled]),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn a_range_reaches_both_its_ends() {
    assert!(Range::new(0, 10, 5).is_ok());
    assert_eq!(Range::new(0, 10, 3), Err(Errno::OutOfRange));
    assert_eq!(Range::new(5, 5, 1), Err(Errno::OutOfRange));
    assert_eq!(Range::new(10, 0, 1), Err(Errno::OutOfRange));
    assert_eq!(Range::new(0, 10, 0), Err(Errno::OutOfRange));
    // The span of the whole i32 line, in steps few enough for a slider.
    let whole = Range::new(i32::MIN, i32::MAX, 16_843_009).expect("the widest range");
    assert!(whole.holds(i32::MIN) && whole.holds(i32::MAX));
    assert_eq!(whole.values(), 256);
    assert_eq!(whole.value_at(255), Some(i32::MAX));
    let stepped = Range::new(-10, 10, 5).expect("a stepped range");
    assert!(stepped.holds(-5) && !stepped.holds(-4) && !stepped.holds(15));
}

/// A range holds no more values than a slider sets one at a time, and each
/// value is reached by its place and back.
#[test]
fn a_range_holds_few_enough_values_to_set_each() {
    let max = i32::from(super::SAVER_RANGE_VALUES_MAX);
    assert!(Range::new(1, max, 1).is_ok(), "the most values");
    assert_eq!(
        Range::new(0, max, 1),
        Err(Errno::OutOfRange),
        "one too many"
    );
    assert_eq!(Range::new(i32::MIN, i32::MAX, 1), Err(Errno::OutOfRange));
    let stepped = Range::new(-10, 10, 5).expect("a stepped range");
    assert_eq!(stepped.values(), 5);
    let values: alloc::vec::Vec<i32> = (0..=5).filter_map(|at| stepped.value_at(at)).collect();
    assert_eq!(values, [-10, -5, 0, 5, 10]);
    assert_eq!(stepped.index_of(5), Some(3));
    assert_eq!(stepped.index_of(4), None);
    assert_eq!(stepped.index_of(15), None);
}

#[test]
fn a_default_outside_the_values_is_refused() {
    let choices = Choices::new(&[choice("a", "A"), choice("b", "B")]).expect("two");
    assert_eq!(
        SaverOption::new(
            word("pick"),
            (label("Pick"), BoundedText::EMPTY),
            SaverOptionKind::Choice {
                choices,
                default: 2
            }
        ),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        SaverOption::new(
            word("count"),
            (label("Count"), BoundedText::EMPTY),
            SaverOptionKind::Range {
                range: Range::new(0, 10, 2).expect("a range"),
                default: 3,
                unit: BoundedText::EMPTY,
            }
        ),
        Err(Errno::OutOfRange)
    );
    assert_eq!(OptionLabel::new(""), Err(Errno::LengthOutOfRange));
}

#[test]
fn an_option_accepts_only_its_own_kind_of_value() {
    assert!(!keep().accepts(&SaverValue::Range(1)));
    assert!(!interval().accepts(&SaverValue::Switch(true)));
    assert!(detail().accepts(&detail().default_value()));
    assert_eq!(keep().default_value(), SaverValue::Switch(true));
    assert_eq!(interval().default_value(), SaverValue::Range(30));
}

#[test]
fn an_entry_with_a_stray_byte_or_an_unknown_kind_is_refused() {
    for option in [detail(), interval(), keep()] {
        let entry = option.to_entry();
        for at in 0..SAVER_OPTION_ENTRY_LEN {
            let mut flipped = entry;
            flipped[at] ^= 0x80;
            assert_ne!(
                SaverOption::from_entry(&flipped),
                Ok(option),
                "byte {at} of {:?} is not covered",
                option.key()
            );
        }
    }
    let mut unknown = keep().to_entry();
    unknown[0] = 9;
    assert_eq!(SaverOption::from_entry(&unknown), Err(Errno::BadMagic));
    assert_eq!(
        SaverOption::from_entry(&keep().to_entry()[..SAVER_OPTION_ENTRY_LEN - 1]),
        Err(Errno::BufferTooSmall)
    );
}

#[test]
fn a_switch_default_is_zero_or_one() {
    let mut entry = keep().to_entry();
    entry[4] = 2;
    assert_eq!(SaverOption::from_entry(&entry), Err(Errno::OutOfRange));
}
