use super::*;
use alloc::vec;

#[test]
fn a_pipeline_splits_at_bars_outside_quotes() {
    assert_eq!(
        stages(b"dir | sort | more").unwrap(),
        vec![&b"dir "[..], b" sort ", b" more"]
    );
    assert_eq!(stages(b"echo \"a|b\"").unwrap(), vec![&b"echo \"a|b\""[..]]);
    assert_eq!(stages(b"dir |"), Err(ParseError::Syntax));
    assert_eq!(stages(b"| more"), Err(ParseError::Syntax));
}

#[test]
fn redirections_are_taken_out_the_last_of_each_counting() {
    let got = redirections(b"sort < in.txt > out.txt").unwrap();
    assert_eq!(got.command, b"sort  ");
    assert_eq!(got.input.as_deref(), Some(&b"in.txt"[..]));
    assert_eq!(got.output, Some((b"out.txt".to_vec(), false)));
    let appended = redirections(b"echo hi>>log>>log2").unwrap();
    assert_eq!(appended.command, b"echo hi");
    assert_eq!(appended.output, Some((b"log2".to_vec(), true)));
    assert_eq!(redirections(b"dir >"), Err(ParseError::Syntax));
    assert_eq!(redirections(b"echo \">\"").unwrap().output, None);
}

#[test]
fn the_command_word_ends_where_dos_ends_it() {
    assert_eq!(command_word(b"  dir/w"), (&b"dir"[..], &b"/w"[..]));
    assert_eq!(command_word(b"copy+a b"), (&b"copy"[..], &b"+a b"[..]));
    assert_eq!(command_word(b"cd.."), (&b"cd.."[..], &b""[..]));
    assert_eq!(command_word(b""), (&b""[..], &b""[..]));
}

#[test]
fn internal_commands_are_found_by_any_of_their_names() {
    assert_eq!(internal(b"dir"), Some((Internal::Dir, 3)));
    assert_eq!(internal(b"ERASE"), Some((Internal::Del, 5)));
    assert_eq!(internal(b"cd.."), Some((Internal::Cd, 2)));
    assert_eq!(internal(b"echo."), Some((Internal::Echo, 4)));
    assert_eq!(internal(b"cd\\dos"), Some((Internal::Cd, 2)));
    assert_eq!(internal(b"a:dir"), None, "a drive makes it a program");
    assert_eq!(internal(b"dirx"), None);
    assert_eq!(internal(b"format"), None);
}

#[test]
fn arguments_split_at_separators_and_before_switches() {
    assert_eq!(
        arguments(b" a.txt,b.txt /w/p"),
        vec![&b"a.txt"[..], b"b.txt", b"/w", b"/p"]
    );
    assert_eq!(arguments(b"\"a b\" c"), vec![&b"\"a b\""[..], b"c"]);
    assert_eq!(arguments(b"   "), Vec::<&[u8]>::new());
}

struct Batch {
    arguments: Vec<&'static [u8]>,
}

impl Source for Batch {
    fn argument(&self, n: usize) -> Option<&[u8]> {
        self.arguments.get(n).copied()
    }
    fn variable(&self, name: &[u8]) -> Option<Vec<u8>> {
        (name == b"PATH").then(|| b"A:\\DOS".to_vec())
    }
}

#[test]
fn a_batch_line_expands_its_arguments_and_variables() {
    let batch = Batch {
        arguments: vec![b"RUN", b"one", b"two"],
    };
    assert_eq!(expand(b"echo %0 %1 %2 %3.", &batch), b"echo RUN one two .");
    assert_eq!(expand(b"set p=%path%;C:\\", &batch), b"set p=A:\\DOS;C:\\");
    assert_eq!(expand(b"echo 100%% %NOPE%x", &batch), b"echo 100% x");
    assert_eq!(
        expand(b"for %%f in (*.*) do echo %%f", &batch),
        b"for %f in (*.*) do echo %f"
    );
    assert_eq!(
        expand(b"echo 50%", &batch),
        b"echo 50",
        "a lone percent is dropped"
    );
}

#[test]
fn the_prompt_makes_its_dollar_codes() {
    let mut now = Now {
        drive: b'C',
        directory: Some(b"C:\\DOS"),
        date: b"Tue 11-10-2026",
        time: b"12:34:56.78",
        version: b"TAIRiX DOS Version 5.00",
    };
    assert_eq!(prompt(b"$p$g", &now), b"C:\\DOS>");
    assert_eq!(prompt(b"$N$G", &now), b"C>");
    assert_eq!(prompt(b"[$t]$_$q$$$b$l$x", &now), b"[12:34:56.78]\r\n=$|<");
    assert_eq!(
        prompt(b"$e[1m$h", &now),
        b"\x1b[1m\x08 \x08",
        "a backspace rubs out"
    );
    now.directory = None;
    assert_eq!(prompt(b"$p$g", &now), b"Current drive is no longer valid>");
    assert_eq!(
        prompt(b"$n:", &now),
        b"C:",
        "the drive is DOS's, whatever its directory"
    );
}

#[test]
fn labels_are_read_and_compared_as_dos_does() {
    assert_eq!(label(b":start"), Some(&b"start"[..]));
    assert_eq!(label(b"  : end of file"), Some(&b"end"[..]));
    assert_eq!(label(b":averylonglabel"), Some(&b"averylon"[..]));
    assert_eq!(label(b"echo :x"), None);
    assert!(same_label(b"AVERYLONGLABEL", b"averylon"));
    assert!(!same_label(b"start", b"stop"));
}

#[test]
fn if_reads_its_three_conditions() {
    assert_eq!(
        condition(b" errorlevel 2 echo hi"),
        Ok((false, Condition::ErrorLevel(2), &b" echo hi"[..]))
    );
    assert_eq!(
        condition(b"not exist a.txt goto x"),
        Ok((true, Condition::Exist(b"a.txt"), &b" goto x"[..]))
    );
    assert_eq!(
        condition(b"\"%1\"==\"\" goto end"),
        Ok((
            false,
            Condition::Equal(b"\"%1\"", b"\"\""),
            &b" goto end"[..]
        ))
    );
    assert_eq!(
        condition(b"NOT a==b echo x"),
        Ok((true, Condition::Equal(b"a", b"b"), &b" echo x"[..]))
    );
    assert_eq!(
        condition(b"==x echo"),
        Err(ParseError::Syntax),
        "an empty side is DOS's syntax error"
    );
    assert_eq!(condition(b"errorlevel x"), Err(ParseError::Syntax));
}

#[test]
fn for_reads_its_variable_set_and_command() {
    let parsed = for_loop(b" %f in (*.txt a.bat) do type %f").unwrap();
    assert_eq!(
        parsed,
        For {
            variable: b'f',
            set: b"*.txt a.bat",
            command: b"type %f"
        }
    );
    assert_eq!(
        for_loop(b"%%f in (x) do y"),
        Err(ParseError::Syntax),
        "the expansion has made %% one"
    );
    assert_eq!(for_loop(b"%f (x) do y"), Err(ParseError::Syntax));
    assert_eq!(
        substitute(b"copy %f %f.bak", b'f', b"A.TXT"),
        b"copy A.TXT A.TXT.bak"
    );
}

#[test]
fn decimals_are_bytes_or_nothing() {
    assert_eq!(decimal(b"0"), Some(0));
    assert_eq!(decimal(b"255"), Some(255));
    assert_eq!(decimal(b"256"), None);
    assert_eq!(decimal(b""), None);
    assert_eq!(decimal(b"1a"), None);
}

#[test]
fn a_number_is_digits_alone_within_32_bits() {
    assert_eq!(number(b"0"), Some(0));
    assert_eq!(number(b"0004"), Some(4));
    assert_eq!(number(b"4294967295"), Some(u32::MAX));
    assert_eq!(number(b"4294967296"), None);
    assert_eq!(number(b"99999999999"), None);
    assert_eq!(number(b""), None);
    assert_eq!(number(b" 1"), None);
    assert_eq!(number(b"1/"), None);
}
