use super::*;

#[test]
fn the_default_copy_options_confirm_verbosely_and_copy_attributes() {
    let flags = apply_options(COPY_OPTIONS, COPY_LETTERS, 0);
    assert_eq!(flags, Some(1 << 3 | 1 << 4));
}

#[test]
fn an_option_is_set_by_its_letter_and_cleared_after_a_tilde() {
    let defaults = apply_options(COPY_OPTIONS, COPY_LETTERS, 0).unwrap_or_default();
    assert_eq!(apply_options(b"~C~V", COPY_LETTERS, defaults), Some(0));
    assert_eq!(
        apply_options(b"r F", COPY_LETTERS, 0),
        Some(1 << 0 | 1 << 1)
    );
    assert_eq!(apply_options(b"~A", COPY_LETTERS, 0), Some(1 << 9));
    assert_eq!(apply_options(b"A", COPY_LETTERS, 1 << 9), Some(0));
}

#[test]
fn a_letter_an_operation_does_not_take_is_refused() {
    assert_eq!(apply_options(b"X", COPY_LETTERS, 0), None);
    assert_eq!(apply_options(b"D", WIPE_LETTERS, 0), None);
    assert_eq!(apply_options(b"F", COUNT_LETTERS, 0), None);
}

#[test]
fn the_default_wipe_and_count_options_are_risc_os_5s() {
    assert_eq!(
        apply_options(WIPE_OPTIONS, WIPE_LETTERS, 0),
        Some(1 << 3 | 1 << 4)
    );
    assert_eq!(apply_options(COUNT_OPTIONS, COUNT_LETTERS, 0), Some(1 << 0));
}
