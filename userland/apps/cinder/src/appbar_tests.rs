use super::*;

use alloc::vec::Vec;

use tairix_abi::window_ipc::AppMenuRowView;

/// The declaration's own row, the one between the information row and *Quit*.
fn own_row(loose: bool) -> (u16, &'static str, AppMenuSymbol) {
    let bar = declaration(7, loose).expect("the fixed rows fit");
    assert_eq!(bar.event_endpoint, 7);
    assert_eq!(bar.click, AppBarClick::RaiseOrOpen);
    let rows: Vec<AppMenuRowView<'_>> = bar.menu.rows().map(|(row, _)| row).collect();
    assert_eq!(rows.len(), 4, "{rows:?}");
    assert_eq!(rows[0], AppMenuRowView::Info);
    assert_eq!(rows[2], AppMenuRowView::Separator);
    assert!(matches!(rows[3], AppMenuRowView::Item(item) if item.id.get() == QUIT_ROW));
    match rows[1] {
        AppMenuRowView::Item(item) => {
            let label = [LET_OUT_LABEL, BRING_HOME_LABEL]
                .into_iter()
                .find(|known| *known == item.label)
                .expect("the row reads one of the pen button's own labels");
            (item.id.get(), label, item.symbol)
        }
        other => panic!("the declaration's own row is {other:?}"),
    }
}

/// The row offers what the pen's button offers, in its words, under the one
/// id the program matches a chosen row back to.
#[test]
fn the_row_says_what_the_pen_button_says_for_either_whereabouts() {
    assert_eq!(
        own_row(false),
        (ROW_OUT, LET_OUT_LABEL, AppMenuSymbol::LetOut)
    );
    assert_eq!(
        own_row(true),
        (ROW_OUT, BRING_HOME_LABEL, AppMenuSymbol::BringHome)
    );
    assert_ne!(ROW_OUT, QUIT_ROW);
}
