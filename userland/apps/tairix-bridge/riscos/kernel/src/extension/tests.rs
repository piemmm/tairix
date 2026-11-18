//! The session's typed state: held, found by type alone, replaced, and let
//! go of.

use crate::memory::layout::RISC_OS_5;
use crate::testkit::boot;

#[derive(Debug, PartialEq, Eq)]
struct Counter(u32);

#[derive(Debug, PartialEq, Eq)]
struct Name(&'static str);

#[test]
fn a_value_is_found_by_its_type_and_no_other() {
    let mut s = boot(&RISC_OS_5);
    let mut os = s.kernel.os();
    assert!(os.extension::<Counter>().is_none(), "none held at boot");
    os.hold_extension(Counter(1)).expect("held");
    os.hold_extension(Name("first")).expect("held");
    assert!(os.extension::<u32>().is_none(), "a type never held");
    if let Some(counter) = os.extension::<Counter>() {
        counter.0 += 1;
    }
    assert_eq!(os.extension::<Counter>(), Some(&mut Counter(2)));
    assert_eq!(os.extension::<Name>(), Some(&mut Name("first")));
}

#[test]
fn holding_a_type_again_replaces_its_value() {
    let mut s = boot(&RISC_OS_5);
    let mut os = s.kernel.os();
    os.hold_extension(Counter(1)).expect("held");
    os.hold_extension(Counter(7)).expect("held again");
    assert_eq!(os.release_extension::<Counter>(), Some(Counter(7)));
    assert!(
        os.release_extension::<Counter>().is_none(),
        "one value a type"
    );
}

#[test]
fn letting_go_of_one_leaves_the_others() {
    let mut s = boot(&RISC_OS_5);
    let mut os = s.kernel.os();
    os.hold_extension(Counter(3)).expect("held");
    os.hold_extension(Name("kept")).expect("held");
    assert_eq!(os.release_extension::<Counter>(), Some(Counter(3)));
    assert!(os.extension::<Counter>().is_none());
    assert_eq!(os.extension::<Name>(), Some(&mut Name("kept")));
}
