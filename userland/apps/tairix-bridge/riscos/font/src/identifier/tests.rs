use super::*;

#[test]
fn fields_are_found_as_risc_os_5_30_found_them() {
    let text = b"\\FHomerton.Medium\\ELatin1\\M1 2 3 4 5 6";
    let at = |kind| find(text, kind).map(|field| field.at);
    assert_eq!(at(b'E'), Some(19));
    assert_eq!(at(b'F'), Some(2));
    assert_eq!(at(b'M'), Some(27));
    assert_eq!(at(b'Q'), None);
    assert_eq!(at(b'f'), None);
    assert_eq!(
        find(b"Homerton.Medium\\ELatin1", b'F').map(|f| f.at),
        Some(0)
    );
    assert_eq!(
        find(b"Homerton.Medium\\ELatin1", b'E').map(|f| f.at),
        Some(17)
    );
    assert_eq!(
        find(b"\\Fa b\\Ec d", b'F'),
        Some(Field { at: 2, value: b"a" })
    );
    assert_eq!(find(b"\\Ea\\Eb", b'E').map(|f| f.at), Some(2));
}

#[test]
fn the_first_of_each_field_counts_and_unknown_letters_are_passed_over() {
    let asked = Asked::of(b"\\FHomerton.Medium\\FTrinity.Medium\\Xfoo\\ELatin1\\ELatin2");
    assert_eq!(asked.font, Some(&b"Homerton.Medium"[..]));
    assert_eq!(asked.encoding, Some(&b"Latin1"[..]));
    assert_eq!(
        Asked::of(b"Trinity.Medium\\Fx").font,
        Some(&b"Trinity.Medium"[..])
    );
    assert_eq!(
        Asked::of(b"Trinity.Medium \x0d").font,
        Some(&b"Trinity.Medium"[..])
    );
    assert_eq!(
        Asked::of(b"\\EUTF8\\FHomerton.Medium").font,
        Some(&b"Homerton.Medium"[..])
    );
}

#[test]
fn an_error_names_the_font_by_its_territory_name_where_that_territory_is_current() {
    assert_eq!(Asked::of(b"\\f1 Big O").shown(1), b"Big O");
    assert_eq!(Asked::of(b"\\FOmega\\f1 Big O").shown(1), b"Big O");
    assert_eq!(Asked::of(b"\\FOmega\\f4 Big O").shown(1), b"Omega");
    assert_eq!(Asked::of(b"").shown(1), b"");
}

#[test]
fn a_matrix_is_six_numbers_compared_by_value() {
    let identity = Ok(Matrix([65536, 0, 0, 65536, 100, -200]));
    for text in [
        &b" 65536 0 0 65536 100 -200"[..],
        b"65536 0 0 65536 100 -200",
        b"65536 0 0 65536 100 -200 ",
        b"65536  0 0 65536 100 -200",
        b"&10000 0 0 &10000 100 -200",
    ] {
        assert_eq!(Matrix::parse(text), identity, "{text:?}");
    }
    assert_eq!(
        Matrix::parse(b"2147483648 0 0 1 0 0"),
        Ok(Matrix([i32::MIN, 0, 0, 1, 0, 0]))
    );
    for bad in [
        &b"1 2 3 4 5 "[..],
        b"1 2 3 4 5",
        b"+1 -2 x 4 5 6 ",
        b"1 2 3 4 5 6 7 ",
        b"",
        b"4294967296 0 0 1 0 0",
        b"99999999999 0 0 1 0 0",
        b"1 2 3 4 5 6x",
    ] {
        assert_eq!(Matrix::parse(bad), Err(BadMatrix), "{bad:?}");
    }
    assert_eq!(
        Matrix([65536, 0, 0, 65536, 100, -200]).spelled(),
        b" 65536 0 0 65536 100 -200"
    );
}

#[test]
fn an_encoding_is_kept_in_lower_case_and_cut_to_twelve_bytes() {
    assert_eq!(encoding_key(b"Fred"), b"fred");
    assert_eq!(encoding_key(b"Trinity.Medium"), b"trinity.medi");
}

#[test]
fn applied_fields_replace_their_kind_and_add_the_rest_after() {
    assert_eq!(
        apply(
            b"\\FHomerton.Medium\\ELatin1\\M1 2 3 4 5 6",
            b"\\FTrinity.Bold\\ELatin2"
        ),
        b"\\FTrinity.Bold\\ELatin2\\M1 2 3 4 5 6"
    );
    assert_eq!(
        apply(b"Homerton.Medium", b"\\ELatin2"),
        b"Homerton.Medium\\ELatin2",
        "a plain identifier stays plain"
    );
    assert_eq!(
        apply(b"\\FHomerton.Medium\\ELatin1\\M1 2 3 4 5 6", b"\\E\\M\\Q9"),
        b"\\FHomerton.Medium\\Q9",
        "an empty field deletes its kind"
    );
}
