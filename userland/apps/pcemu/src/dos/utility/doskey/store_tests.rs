//! The store: macros and commands kept in one buffer, the oldest commands
//! given up for room, and a macro's text expanded.

use super::*;

/// A buffer of `size` bytes holding nothing.
fn empty(size: usize) -> Store {
    Store::read(size, &[], 0)
}

#[test]
fn commands_are_kept_oldest_first_and_given_up_for_room() {
    let mut store = empty(16);
    store.remember(b"dir");
    store.remember(b"");
    store.remember(b"type a.txt");
    assert_eq!(
        store.history(),
        [b"dir".to_vec(), b"type a.txt".to_vec()],
        "an empty line is not kept"
    );
    store.remember(b"cls");
    assert_eq!(
        store.history(),
        [b"type a.txt".to_vec(), b"cls".to_vec()],
        "the oldest given up: 4 + 11 + 4 bytes"
    );
    store.remember(&[b'x'; 20]);
    assert_eq!(
        store.history().len(),
        2,
        "a command longer than the buffer is not kept, and costs none"
    );
}

#[test]
fn a_macro_takes_room_from_the_commands_never_from_another_macro() {
    let mut store = empty(20);
    store.remember(b"first");
    store.remember(b"second");
    assert_eq!(store.define(b"ls", b"dir /w"), Ok(()));
    assert_eq!(
        store.macros(),
        [Macro {
            name: b"LS".to_vec(),
            text: b"dir /w".to_vec()
        }],
        "names in capitals"
    );
    assert_eq!(
        store.history(),
        [b"second".to_vec()],
        "10 bytes of macro: the oldest command went"
    );
    assert_eq!(store.define(b"big", &[b'x'; 30]), Err(NoRoom));
    assert_eq!(
        store.history(),
        [b"second".to_vec()],
        "a macro that could never fit costs no command"
    );
    assert_eq!(store.macros().len(), 1, "the macros kept");
    assert_eq!(store.define(b"LS", b""), Ok(()), "no text deletes it");
    assert!(store.macros().is_empty());
}

#[test]
fn a_store_reads_back_from_its_bytes() {
    let mut store = empty(64);
    store.define(b"go", b"cd \\games$tdir").unwrap();
    store.remember(b"ver");
    store.remember(b"vol");
    let (bytes, macros) = store.bytes();
    assert_eq!(bytes, b"GO=cd \\games$tdir\0ver\0vol\0");
    assert_eq!(macros, 18);
    assert_eq!(Store::read(64, &bytes, macros), store);
    assert_eq!(
        Store::read(8, &bytes, macros).history(),
        [] as [Vec<u8>; 0],
        "a smaller buffer gives up the commands"
    );
}

#[test]
fn a_macros_text_takes_the_lines_words_and_splits_at_t() {
    let mut store = empty(512);
    store.define(b"mc", b"md $1$tcd $1").unwrap();
    store
        .define(b"each", b"for %%f in ($*) do type %%f $g$g all.txt")
        .unwrap();
    store.define(b"odd", b"echo $$ $l $b $x $").unwrap();
    assert_eq!(
        store.expand(b"MC games"),
        Some(vec![b"md games".to_vec(), b"cd games".to_vec()])
    );
    assert_eq!(
        store.expand(b"mc"),
        Some(vec![b"md ".to_vec(), b"cd ".to_vec()]),
        "a parameter not given is nothing"
    );
    assert_eq!(
        store.expand(b"each a.txt  b.txt"),
        Some(vec![
            b"for %%f in (a.txt  b.txt) do type %%f >> all.txt".to_vec()
        ])
    );
    assert_eq!(
        store.expand(b"odd"),
        Some(vec![b"echo $ < | $x $".to_vec()])
    );
    assert_eq!(
        store.expand(b" mc games"),
        None,
        "a blank first runs the command, not the macro"
    );
    assert_eq!(store.expand(b"mcx games"), None);
    assert_eq!(store.expand(b""), None);
}
