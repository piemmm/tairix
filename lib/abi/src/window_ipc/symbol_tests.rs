use super::AppMenuSymbol;
use crate::Errno;

#[test]
fn every_symbol_round_trips_and_the_table_covers_the_vocabulary() {
    for (index, symbol) in AppMenuSymbol::ALL.iter().copied().enumerate() {
        assert_eq!(usize::from(symbol.wire()), index + 1, "{symbol:?}");
        assert_eq!(AppMenuSymbol::from_wire(symbol.wire()), Ok(symbol));
    }
    // Discriminants count up from 1 in declaration order, so the last
    // variant's value is the vocabulary's size: a variant missing from
    // the table would leave a value it cannot decode.
    assert_eq!(
        usize::from(AppMenuSymbol::BringHome.wire()),
        AppMenuSymbol::ALL.len()
    );
}

#[test]
fn zero_and_a_value_past_the_vocabulary_name_nothing() {
    assert_eq!(AppMenuSymbol::from_wire(0), Err(Errno::OutOfRange));
    let past = u16::try_from(AppMenuSymbol::ALL.len() + 1).expect("fits");
    assert_eq!(AppMenuSymbol::from_wire(past), Err(Errno::OutOfRange));
    assert_eq!(AppMenuSymbol::from_wire(u16::MAX), Err(Errno::OutOfRange));
}
