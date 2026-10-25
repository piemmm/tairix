use super::{
    Change, Mode, Op, Perms, PERM_COND_EXEC, PERM_EXEC, PERM_READ, PERM_SETID, PERM_STICKY,
    PERM_WRITE, WHO_ALL, WHO_GROUP, WHO_OTHER, WHO_USER,
};
use alloc::vec;
use tairix_abi::fs::FS_NEW_DIR_MODE;

fn mode(spec: &str) -> Mode {
    Mode::parse(spec).unwrap_or_else(|| panic!("{spec:?} is a mode"))
}

/// `spec` applied to a file holding `current`, under `umask`.
fn file(spec: &str, current: u32, umask: u32) -> u32 {
    mode(spec).resolve(current, false, umask)
}

/// `spec` applied to a directory holding `current`, under `umask`.
fn dir(spec: &str, current: u32, umask: u32) -> u32 {
    mode(spec).resolve(current, true, umask)
}

fn letters(who: u8, op: Op, letters: u8) -> Change {
    Change {
        who,
        op,
        perms: Perms::Letters(letters),
    }
}

// ----- reading a mode ---------------------------------------------------

#[test]
fn octal_of_any_length_within_the_mode_bits_parses() {
    assert_eq!(
        Mode::parse("644"),
        Some(Mode::Octal {
            bits: 0o644,
            names_set_id: false
        })
    );
    assert_eq!(
        Mode::parse("0755"),
        Some(Mode::Octal {
            bits: 0o755,
            names_set_id: false
        })
    );
    // GNU 9.4 takes any number of leading zeros, and five digits or more name
    // a directory's set-ID bits.
    assert_eq!(
        Mode::parse("000000007777"),
        Some(Mode::Octal {
            bits: 0o7777,
            names_set_id: true
        })
    );
    assert_eq!(
        Mode::parse("00755"),
        Some(Mode::Octal {
            bits: 0o755,
            names_set_id: true
        })
    );
}

#[test]
fn a_symbolic_clause_reads_into_its_changes() {
    assert_eq!(
        Mode::parse("g+w"),
        Some(Mode::Symbolic(vec![letters(
            WHO_GROUP,
            Op::Add,
            PERM_WRITE
        )]))
    );
    // `u+x-w` is two changes to the owner's field.
    assert_eq!(
        Mode::parse("u+x-w"),
        Some(Mode::Symbolic(vec![
            letters(WHO_USER, Op::Add, PERM_EXEC),
            letters(WHO_USER, Op::Remove, PERM_WRITE),
        ]))
    );
    assert_eq!(
        Mode::parse("u=rwx,go=rx"),
        Some(Mode::Symbolic(vec![
            letters(WHO_USER, Op::Set, PERM_READ | PERM_WRITE | PERM_EXEC),
            letters(WHO_GROUP | WHO_OTHER, Op::Set, PERM_READ | PERM_EXEC),
        ]))
    );
    assert_eq!(
        Mode::parse("a+X,+t,u+s"),
        Some(Mode::Symbolic(vec![
            letters(WHO_ALL, Op::Add, PERM_COND_EXEC),
            letters(0, Op::Add, PERM_STICKY),
            letters(WHO_USER, Op::Add, PERM_SETID),
        ]))
    );
}

#[test]
fn a_copy_and_an_octal_operand_read_as_gnu_reads_them() {
    assert_eq!(
        Mode::parse("g=u+r"),
        Some(Mode::Symbolic(vec![
            Change {
                who: WHO_GROUP,
                op: Op::Set,
                perms: Perms::CopyOf(WHO_USER),
            },
            letters(WHO_GROUP, Op::Add, PERM_READ),
        ]))
    );
    assert_eq!(
        Mode::parse("+x=644"),
        Some(Mode::Symbolic(vec![
            letters(0, Op::Add, PERM_EXEC),
            Change {
                who: 0,
                op: Op::Set,
                perms: Perms::Octal(0o644),
            },
        ]))
    );
}

/// GNU 9.4 refuses each of these as an invalid mode.
#[test]
fn a_malformed_mode_is_refused() {
    for spec in [
        "", "zzz", "8", "010000", "u=755", "=644+x", "+17777", "=8", "g=uo", "g=ur", "ug", "a",
        "g", ",", "u+x,", "a+rw,", "a+q", "+x7",
    ] {
        assert_eq!(Mode::parse(spec), None, "{spec:?}");
    }
}

#[test]
fn an_operator_with_nothing_after_it_is_a_valid_no_op() {
    assert_eq!(file("u+", 0o640, 0), 0o640);
    assert_eq!(file("a-", 0o640, 0), 0o640);
    assert_eq!(file("+", 0o750, 0o022), 0o750);
}

// ----- resolving it -------------------------------------------------------

#[test]
fn octal_replaces_a_files_bits() {
    assert_eq!(file("644", 0o777, 0o022), 0o644);
    assert_eq!(file("4755", 0, 0o022), 0o4755);
    assert_eq!(file("755", 0o6755, 0), 0o755);
}

#[test]
fn add_remove_and_set_change_the_named_fields() {
    assert_eq!(file("g+w", 0o644, 0), 0o664);
    assert_eq!(file("a+x", 0o644, 0o022), 0o755);
    assert_eq!(file("o-r", 0o644, 0), 0o640);
    assert_eq!(file("a-w", 0o777, 0o022), 0o555);
    assert_eq!(file("u=rw", 0o777, 0), 0o677);
    assert_eq!(file("go=", 0o777, 0), 0o700);
    assert_eq!(file("a=rx", 0o777, 0o022), 0o555);
    assert_eq!(file("a=rwx,go-w", 0, 0), 0o755);
}

/// GNU 9.4 under umask `022` and `077`: a clause naming no field changes no
/// bit the umask holds, though `=` still clears them all first.
#[test]
fn a_clause_naming_no_field_spares_the_umask() {
    let cases = [
        ("+w", 0o000, 0o022, 0o200),
        ("+w", 0o600, 0o022, 0o600),
        ("+rwx", 0o600, 0o022, 0o755),
        ("+rwx", 0o000, 0o077, 0o700),
        ("-w", 0o777, 0o022, 0o577),
        ("-rwx", 0o777, 0o022, 0o022),
        ("-rwx", 0o777, 0o077, 0o077),
        ("-x", 0o777, 0o022, 0o666),
        ("=r", 0o600, 0o022, 0o444),
        ("=r", 0o4777, 0o022, 0o444),
        ("=rw", 0o777, 0o022, 0o644),
        ("=rwx", 0o000, 0o077, 0o700),
        ("=", 0o755, 0o022, 0o000),
        ("+s", 0o000, 0o022, 0o6000),
        ("+t", 0o000, 0o022, 0o1000),
        ("+X", 0o750, 0o022, 0o751),
    ];
    for (spec, current, umask, want) in cases {
        assert_eq!(file(spec, current, umask), want, "{spec} on {current:o}");
    }
    // Naming every field takes no notice of the umask.
    assert_eq!(file("a+w", 0o000, 0o022), 0o222);
    assert_eq!(file("a-w", 0o777, 0o022), 0o555);
}

#[test]
fn conditional_execute_needs_a_directory_or_an_execute_bit() {
    assert_eq!(dir("a+X", 0o644, 0), 0o755);
    assert_eq!(file("a+X", 0o644, 0), 0o644);
    assert_eq!(file("a+X", 0o744, 0), 0o755);
    assert_eq!(file("g=X", 0o750, 0), 0o710);
    // `X` reads the bits the earlier changes left.
    assert_eq!(file("a-x,a+X", 0o755, 0), 0o644);
}

/// GNU 9.4: `s` sets the owner's and the group's set-ID bits and `t` the
/// sticky bit, each only for the field that carries it.
#[test]
fn special_bits_belong_to_their_own_fields() {
    let cases = [
        ("u+s", 0o755, 0o4755),
        ("g+s", 0o755, 0o2755),
        ("+s", 0o755, 0o6755),
        ("o+s", 0o755, 0o755),
        ("uo+s", 0o755, 0o4755),
        ("u+t", 0o755, 0o755),
        ("ug+t", 0o755, 0o755),
        ("o+t", 0o755, 0o1755),
        ("a+t", 0o755, 0o1755),
        ("u=t", 0o755, 0o055),
        ("o=t", 0o755, 0o1750),
        ("o=s", 0o755, 0o750),
        ("u-t", 0o1777, 0o1777),
        ("o-t", 0o1777, 0o777),
        ("u=r", 0o1777, 0o1477),
        ("o=r", 0o1777, 0o774),
        ("u-s", 0o6777, 0o2777),
        ("g-s", 0o6777, 0o4777),
        ("u=r", 0o6777, 0o2477),
        ("g=r", 0o6777, 0o4747),
    ];
    for (spec, current, want) in cases {
        assert_eq!(file(spec, current, 0o022), want, "{spec} on {current:o}");
    }
}

/// GNU 9.4: a field's bits copied to others, as they stand when the change
/// is made.
#[test]
fn a_copy_takes_the_fields_bits_as_they_stand() {
    let cases = [
        ("g=u", 0o750, 0o770),
        ("o=g", 0o750, 0o755),
        ("u=o", 0o750, 0o050),
        ("g+u", 0o750, 0o770),
        ("g-u", 0o750, 0o700),
        ("=u", 0o750, 0o755),
        ("go=u-w", 0o750, 0o755),
        ("u=rx,g=u", 0o750, 0o550),
        ("a=r,u+w,g=u", 0o750, 0o664),
    ];
    for (spec, current, want) in cases {
        assert_eq!(file(spec, current, 0o022), want, "{spec}");
    }
}

/// GNU 9.4: octal after an operator is taken as written, the umask aside.
#[test]
fn octal_after_an_operator_ignores_the_umask() {
    let cases = [
        ("+777", 0o750, 0o777),
        ("=777", 0o750, 0o777),
        ("-777", 0o750, 0o000),
        ("+022", 0o750, 0o772),
        ("=0", 0o750, 0o000),
        ("+4000", 0o750, 0o4750),
        ("=2755", 0o750, 0o2755),
        ("u+x,=644", 0o750, 0o644),
        ("+x=644", 0o750, 0o644),
    ];
    for (spec, current, want) in cases {
        assert_eq!(file(spec, current, 0o022), want, "{spec}");
    }
}

/// GNU 9.4 on a directory holding `6755` or `2755`: a mode leaves its set-ID
/// bits as they are unless it names them — with `s`, an octal operator, or an
/// octal number of five digits or more.
#[test]
fn a_directorys_set_id_bits_stay_unless_named() {
    let cases = [
        ("755", 0o6755, 0o6755),
        ("0755", 0o6755, 0o6755),
        ("00755", 0o6755, 0o755),
        ("000755", 0o6755, 0o755),
        ("2755", 0o6755, 0o6755),
        ("4755", 0o6755, 0o6755),
        ("0644", 0o6755, 0o6644),
        ("07755", 0o6755, 0o7755),
        ("=rwx", 0o6755, 0o6755),
        ("g=rx", 0o6755, 0o6755),
        ("a=rx", 0o6755, 0o6555),
        ("a=", 0o6755, 0o6000),
        ("u=rwx,go=rx", 0o6755, 0o6755),
        ("g-s", 0o6755, 0o4755),
        ("u-s", 0o6755, 0o2755),
        ("+s", 0o6755, 0o6755),
        ("=755", 0o6755, 0o755),
        ("-6000", 0o6755, 0o755),
        ("+2000", 0o6755, 0o6755),
        ("+777", 0o2755, 0o2777),
        ("=777", 0o2755, 0o777),
        ("-777", 0o2755, 0o2000),
        ("+4000", 0o2755, 0o6755),
        ("g=u", 0o2755, 0o2775),
        ("u=o", 0o2755, 0o2555),
        ("a=r,u+w,g=u", 0o2755, 0o2664),
    ];
    for (spec, current, want) in cases {
        assert_eq!(dir(spec, current, 0o022), want, "{spec} on {current:o}");
    }
    // A sticky bit is no set-ID bit: an octal mode replaces it.
    assert_eq!(dir("755", 0o1777, 0o022), 0o755);
}

/// `mkdir -m`'s reading: a mode resolved from a fresh directory's bits.
#[test]
fn a_mode_resolved_from_a_fresh_directory_is_the_one_mkdir_gives() {
    assert_eq!(dir("g-w", FS_NEW_DIR_MODE, 0o022), 0o757);
    assert_eq!(dir("-w", FS_NEW_DIR_MODE, 0o022), 0o577);
    assert_eq!(dir("=r", FS_NEW_DIR_MODE, 0o022), 0o444);
    assert_eq!(dir("u=rwx,g=rx,o=", FS_NEW_DIR_MODE, 0o022), 0o750);
    assert_eq!(dir("u=rwx,g=u", FS_NEW_DIR_MODE, 0o022), 0o777);
    assert_eq!(dir("777", FS_NEW_DIR_MODE, 0o022), 0o777);
    assert_eq!(dir("2755", FS_NEW_DIR_MODE, 0o022), 0o2755);
}
