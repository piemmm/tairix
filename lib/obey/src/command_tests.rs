use alloc::vec::Vec;

use super::*;
use crate::vars::{Store, Value};

fn store() -> Store {
    Store::with_room(16, 256, 256).unwrap()
}

fn pick(tail: &[u8]) -> Result<Option<Vec<u8>>, Error> {
    let mut vars = store();
    vars.assign(b"Yes", Value::Number(1)).unwrap();
    choose(tail, &mut vars, &mut Budget::new(100)).map(|chosen| chosen.map(<[u8]>::to_vec))
}

#[test]
fn if_runs_the_command_its_condition_chooses() {
    assert_eq!(
        pick(b"1 Then Echo a Else Echo b"),
        Ok(Some(b" Echo a".to_vec()))
    );
    assert_eq!(
        pick(b"0 Then Echo a Else Echo b"),
        Ok(Some(b" Echo b".to_vec()))
    );
    assert_eq!(pick(b"0 THEN Echo a"), Ok(None));
    assert_eq!(
        pick(b"Yes then x eLsE y"),
        Ok(Some(b" x".to_vec())),
        "either case"
    );
    assert_eq!(
        pick(b"1 Then"),
        Ok(Some(Vec::new())),
        "Then at the end of the line"
    );
    assert_eq!(pick(b"1 Then a ElseX b"), Ok(Some(b" a ElseX b".to_vec())));
    assert_eq!(
        pick(b"1 Then a Else"),
        Ok(Some(b" a Else".to_vec())),
        "Else wants a space after it"
    );
}

#[test]
fn if_finds_then_whatever_the_quotes_say_and_wants_an_integer() {
    assert_eq!(
        pick(b"\"a Then b\" = \"\" Then x"),
        Err(Error::BadString),
        "the condition ends inside the quotes"
    );
    assert_eq!(pick(b"1 Thenx y"), Err(Error::Syntax));
    assert_eq!(pick(b"1"), Err(Error::Syntax));
    assert_eq!(pick(b"\"s\" Then x"), Err(Error::Syntax));
    assert_eq!(pick(b"1/0 Then x"), Err(Error::DivisionByZero));
}

/// Each `IconSprites` file, translated.
#[derive(Default)]
struct Sprites(Vec<Vec<u8>>);

impl Observer for Sprites {
    fn icon_sprites(&mut self, file: &[u8], _: &mut dyn Variables, _: &mut Budget) {
        self.0.push(file.to_vec());
    }
}

/// `file` read with `arguments`: the variables it left, the sprite files it
/// named, and how the reading ended.
fn read_with(file: &[u8], arguments: &[u8]) -> (Store, Vec<Vec<u8>>, Result<(), Error>) {
    let mut vars = store();
    let mut sprites = Sprites::default();
    let mut budget = Budget::new(200);
    let mut cx = Context {
        vars: &mut vars,
        longest: 256,
        observer: &mut sprites,
        budget: &mut budget,
    };
    let ended = read_only(file, b"$.Apps.!Edit", arguments, &mut cx);
    (vars, sprites.0, ended)
}

fn read(file: &[u8]) -> (Store, Vec<Vec<u8>>, Result<(), Error>) {
    read_with(file, b"\r")
}

fn value(vars: &mut Store, name: &[u8]) -> Option<Value> {
    vars.lookup(name).unwrap()
}

#[test]
fn set_setmacro_and_unset_change_the_variables() {
    let (mut vars, _, ended) = read(
        b"| A comment\n\
          *Set Edit$Dir <Obey$Dir>\n\
          SetMacro Edit$Res <Edit$Dir>.Resources\r\n\
          Set Gone 1\n\
          Set Gone2 2\n\
          Unset Gon*\n",
    );
    assert_eq!(ended, Ok(()));
    assert_eq!(
        value(&mut vars, b"Edit$Dir"),
        Some(Value::String(b"$.Apps.!Edit".to_vec()))
    );
    assert_eq!(
        value(&mut vars, b"Edit$Res"),
        Some(Value::Macro(b"<Edit$Dir>.Resources".to_vec()))
    );
    assert_eq!(value(&mut vars, b"Gon*"), None, "Unset takes every match");
}

#[test]
fn a_line_ends_at_a_cr_an_lf_or_a_nul() {
    let (mut vars, _, ended) = read(b"Set A 1\rSet B 2\0Set C 3\nSet D 4");
    assert_eq!(ended, Ok(()));
    for name in [&b"A"[..], b"B", b"C", b"D"] {
        assert!(value(&mut vars, name).is_some(), "{name:?}");
    }
}

#[test]
fn the_parameters_are_put_in_and_none_appended() {
    let (mut vars, _, ended) = read_with(b"Set P [%1][%0]\n", b"a \"b c\" d\r");
    assert_eq!(ended, Ok(()));
    assert_eq!(
        value(&mut vars, b"P"),
        Some(Value::String(b"[\"b c\"][a]".to_vec()))
    );
}

#[test]
fn icon_sprites_reports_its_file_and_if_nests() {
    let (_, sprites, ended) = read(
        b"IconSprites <Obey$Dir>.!Sprites\n\
          If \"<Obey$Dir>\" <> \"\" Then If 1 Then IconSprites <Obey$Dir>.Deep\n\
          If 0 Then IconSprites Never Else %IconSprites \"Quoted file\"\n",
    );
    assert_eq!(ended, Ok(()));
    assert_eq!(
        sprites,
        [
            b"$.Apps.!Edit.!Sprites".to_vec(),
            b"$.Apps.!Edit.Deep".to_vec(),
            b"Quoted file".to_vec(),
        ]
    );
}

#[test]
fn a_line_that_could_run_something_else_changes_nothing() {
    let (mut vars, sprites, ended) = read(
        b"Set Alias$IconSprites Run Evil\n\
          IconSprites <Obey$Dir>.Aliased\n\
          %IconSprites <Obey$Dir>.Unaliased\n\
          Set X 1 { > Out }\n\
          Set Y { 1\n\
          Ic. <Obey$Dir>.Abbreviated\n\
          Run <Obey$Dir>.!Run\n",
    );
    assert_eq!(ended, Ok(()));
    assert_eq!(sprites, [b"$.Apps.!Edit.Unaliased".to_vec()]);
    assert_eq!(
        value(&mut vars, b"X"),
        None,
        "a redirection is never opened"
    );
    assert_eq!(
        value(&mut vars, b"Y"),
        Some(Value::String(b"{ 1".to_vec())),
        "nor is a brace that opens none"
    );
}

#[test]
fn the_first_line_that_fails_ends_the_reading() {
    let (mut vars, _, ended) = read(b"Set A 1\nSet B |\nSet C 3\n");
    assert_eq!(ended, Err(Error::BadString));
    assert!(value(&mut vars, b"A").is_some());
    assert_eq!(value(&mut vars, b"C"), None);
    for (line, refused) in [
        (&b"Set Only\n"[..], Error::Syntax),
        (b"Unset A B\n", Error::Syntax),
        (b"IconSprites a b\n", Error::Syntax),
        (b"If 1\n", Error::Syntax),
    ] {
        let mut file = line.to_vec();
        file.extend_from_slice(b"Set After 1\n");
        let (mut vars, _, ended) = read(&file);
        assert_eq!(ended, Err(refused), "{line:?}");
        assert_eq!(value(&mut vars, b"After"), None);
    }
}

#[test]
fn a_line_longer_than_its_room_ends_the_reading() {
    let mut file = Vec::from(&b"Set A 1\n|"[..]);
    file.extend(core::iter::repeat_n(b'x', MOST_LINE - 3));
    file.extend_from_slice(b"\n|");
    file.extend(core::iter::repeat_n(b'x', MOST_LINE - 2));
    file.extend_from_slice(b"\nSet C 3\n");
    let (mut vars, _, ended) = read(&file);
    assert_eq!(
        ended,
        Err(Error::BufferOverflow),
        "a line and its terminator leave a byte of the room spare"
    );
    assert!(value(&mut vars, b"A").is_some());
    assert_eq!(value(&mut vars, b"C"), None);
}

#[test]
fn a_command_line_holds_its_terminator_within_its_longest() {
    let mut vars = store();
    let mut budget = Budget::new(10);
    let mut cx = Context {
        vars: &mut vars,
        longest: 2048,
        observer: &mut Sprites::default(),
        budget: &mut budget,
    };
    let mut line = Vec::from(&b"|"[..]);
    line.extend(core::iter::repeat_n(b'x', MOST_LINE - 2));
    line.push(b'\r');
    assert_eq!(read_line(&line, &mut cx), Ok(()));
    line.insert(0, b' ');
    assert_eq!(read_line(&line, &mut cx), Err(Error::LineTooLong));
}

#[test]
fn ifs_nest_no_deeper_than_the_bound() {
    let deep = |ifs: usize| {
        let mut line = b"If 1 Then ".repeat(ifs);
        line.extend_from_slice(b"Set Deep 1\r");
        line
    };
    let (mut vars, _, ended) = read(&deep(MOST_NESTED));
    assert_eq!(ended, Ok(()));
    assert!(value(&mut vars, b"Deep").is_some());
    let (_, _, ended) = read(&deep(MOST_NESTED + 1));
    assert_eq!(ended, Err(Error::ExpansionTooComplex));
}

#[test]
fn the_budget_bounds_the_whole_file_and_stops_it_where_it_runs_out() {
    let mut vars = store();
    let mut sprites = Sprites::default();
    let file =
        b"IconSprites <Obey$Dir>.A\nIconSprites <Obey$Dir>.B\nIconSprites <Obey$Dir>.C\nSet E 1\n";
    let mut budget = Budget::new(2);
    let mut cx = Context {
        vars: &mut vars,
        longest: 256,
        observer: &mut sprites,
        budget: &mut budget,
    };
    assert_eq!(
        read_only(file, b"D", b"", &mut cx),
        Err(Error::ExpansionTooComplex)
    );
    assert_eq!(
        sprites.0,
        [b"D.A".to_vec(), b"D.B".to_vec()],
        "the lines before"
    );
    assert_eq!(value(&mut vars, b"E"), None, "and none after");
}
