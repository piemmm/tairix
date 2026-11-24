use super::*;

#[path = "recorded.rs"]
mod recorded;

#[test]
fn every_recorded_file_is_checked_as_genuine_risc_os_checked_it() {
    for &(name, bytes, refusal) in recorded::FILES {
        let checked = check(bytes).err().map(|stop| match stop {
            Stop::Refused(refusal) => refusal,
            Stop::NoRoom | Stop::Overflowed => panic!("{name}: {stop:?}"),
        });
        assert_eq!(checked, refusal, "{name}");
    }
}

#[test]
fn a_font_table_names_its_numbers_where_they_are_in_the_file() {
    let table = crate::fixture::font_table(&[(1, b"Ab"), (2, b"Cd"), (1, b"Ef")]);
    let file = crate::fixture::file([0; 4], &[&table]);
    let fonts = check(&file).expect("sound");
    assert_eq!(fonts.table, Some(40));
    assert_eq!(
        fonts.named(1),
        Some(40 + 8 + 9),
        "a number named twice takes the later name"
    );
    assert_eq!(fonts.named(2), Some(40 + 8 + 5));
    assert_eq!(fonts.named(3), None);
}
