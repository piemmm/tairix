use alloc::vec::Vec;

use super::*;
use crate::vars::Value;

fn store() -> Store {
    Store::with_room(16, 256).unwrap()
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
    assert_eq!(pick(b"1 Thenx y"), Err(Error::NoThen));
    assert_eq!(pick(b"1"), Err(Error::NoThen));
    assert_eq!(pick(b"\"s\" Then x"), Err(Error::ExpressionIsAString));
    assert_eq!(pick(b"1/0 Then x"), Err(Error::DivisionByZero));
    assert_eq!(
        pick(b"1\x01 Then x"),
        Err(Error::ExpressionIsAString),
        "a failure expanding the condition reads as RISC OS reports it"
    );
}

/// Each `IconSprites` file, translated.
#[derive(Default)]
struct Sprites(Vec<Vec<u8>>);

impl Observer for Sprites {
    fn icon_sprites(&mut self, file: &[u8], _: &mut Store, _: &mut Budget) {
        self.0.push(file.to_vec());
    }
}

fn read(file: &[u8]) -> (Store, Vec<Vec<u8>>) {
    let mut vars = store();
    let mut sprites = Sprites::default();
    read_only(
        file,
        b"$.Apps.!Edit",
        &mut vars,
        &mut sprites,
        &mut Budget::new(200),
    )
    .unwrap();
    (vars, sprites.0)
}

fn value(vars: &mut Store, name: &[u8]) -> Option<Value> {
    vars.lookup(name).unwrap()
}

#[test]
fn set_setmacro_and_unset_change_the_variables() {
    let (mut vars, _) = read(
        b"| A comment\n\
          *Set Edit$Dir <Obey$Dir>\n\
          SetMacro Edit$Res <Edit$Dir>.Resources\r\n\
          Set Gone 1\n\
          Unset Gone\n\
          Set Only\n",
    );
    assert_eq!(
        value(&mut vars, b"Edit$Dir"),
        Some(Value::String(b"$.Apps.!Edit".to_vec()))
    );
    assert_eq!(
        value(&mut vars, b"Edit$Res"),
        Some(Value::Macro(b"<Edit$Dir>.Resources".to_vec()))
    );
    assert_eq!(value(&mut vars, b"Gone"), None);
    assert_eq!(value(&mut vars, b"Only"), None, "Set wants a value");
}

#[test]
fn icon_sprites_reports_its_file_and_if_nests() {
    let (_, sprites) = read(
        b"IconSprites <Obey$Dir>.!Sprites extra\n\
          If \"<Obey$Dir>\" <> \"\" Then If 1 Then IconSprites <Obey$Dir>.Deep\n\
          If 0 Then IconSprites Never Else %IconSprites \"Quoted file\"\n",
    );
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
    let (mut vars, sprites) = read(
        b"Set Alias$IconSprites Run Evil\n\
          IconSprites <Obey$Dir>.Aliased\n\
          %IconSprites <Obey$Dir>.Unaliased\n\
          Set X 1 { > Out }\n\
          Ic. <Obey$Dir>.Abbreviated\n\
          Run <Obey$Dir>.!Run\n\
          Set Long <Obey$Dir>%0%9\n",
    );
    assert_eq!(sprites, [b"$.Apps.!Edit.Unaliased".to_vec()]);
    assert_eq!(
        value(&mut vars, b"X"),
        None,
        "a redirection is never opened"
    );
    assert_eq!(
        value(&mut vars, b"Long"),
        Some(Value::String(b"$.Apps.!Edit".to_vec())),
        "the file's parameters are none"
    );
}

#[test]
fn a_line_too_long_or_failing_is_skipped_and_the_rest_read() {
    let mut file = Vec::from(&b"Set A "[..]);
    file.extend(core::iter::repeat_n(b'x', MOST_LINE));
    file.extend_from_slice(b"\nSet B |\nSet C 3\n");
    let (mut vars, _) = read(&file);
    assert_eq!(value(&mut vars, b"A"), None);
    assert_eq!(value(&mut vars, b"B"), None);
    assert_eq!(value(&mut vars, b"C"), Some(Value::String(b"3".to_vec())));
}

#[test]
fn the_budget_bounds_the_whole_file() {
    let mut vars = store();
    let mut sprites = Sprites::default();
    let file = b"IconSprites <Obey$Dir>.A\nIconSprites <Obey$Dir>.B\nIconSprites <Obey$Dir>.C\n";
    read_only(file, b"D", &mut vars, &mut sprites, &mut Budget::new(2)).unwrap();
    assert_eq!(sprites.0, [b"D.A".to_vec(), b"D.B".to_vec()]);
}
