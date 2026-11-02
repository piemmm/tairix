use alloc::vec::Vec;

use tairix_geometry::Rect;

use super::{members_of, route, Route};
use crate::docapp::Request;

type Asked = Request<(), ()>;

#[test]
fn what_a_member_asks_of_its_document_is_its_primarys() {
    for request in [
        Asked::Save,
        Asked::SaveAs,
        Asked::SaveWhere { then_close: false },
        Asked::SaveWhere { then_close: true },
    ] {
        assert_eq!(route(&request), Route::Primary, "{request:?}");
    }
    assert_eq!(route(&Asked::SaveThenClose), Route::SaveThenClose);
}

#[test]
fn a_member_closes_without_being_asked() {
    assert_eq!(route(&Asked::Close), Route::Close);
    assert_eq!(route(&Asked::AskToClose), Route::Close);
}

#[test]
fn a_members_menus_opens_and_own_requests_are_its_own() {
    let menu = Asked::Menu {
        kind: (),
        anchor: Rect::new(0, 0, 1, 1),
    };
    for request in [Asked::Open, menu, Asked::Own(())] {
        assert_eq!(route(&request), Route::Member, "{request:?}");
    }
}

#[test]
fn only_a_windows_own_members_close_with_it() {
    let windows = [
        (1, None),
        (2, Some(1)),
        (3, Some(9)),
        (4, Some(1)),
        (5, None),
    ];
    let closing: Vec<u64> = members_of(1, windows).collect();
    assert_eq!(closing, [2, 4]);
    assert_eq!(members_of(5, windows).count(), 0);
}
