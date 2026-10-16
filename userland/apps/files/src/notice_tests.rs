use super::*;

use tairix_browse::render::build_refusal_dialog;

fn shown(notices: &Notices) -> Option<(&str, Option<&str>)> {
    notices
        .showing()
        .map(|notice| (notice.title(), notice.message()))
}

#[test]
fn a_refusal_while_another_shows_waits_its_turn() {
    let mut notices = Notices::default();
    notices.state(
        NoticeSource::Listing,
        build_refusal_dialog("Not listed", "first"),
    );
    notices.state(
        NoticeSource::Operation,
        build_refusal_dialog("Copy stopped", "second"),
    );
    assert_eq!(
        shown(&notices),
        Some(("Not listed", Some("first"))),
        "the later one neither replaces it nor is lost"
    );
    notices.dismiss();
    assert_eq!(shown(&notices), Some(("Copy stopped", Some("second"))));
    notices.dismiss();
    assert!(!notices.is_showing());
}

#[test]
fn a_source_keeps_only_its_latest_refusal_waiting() {
    let mut notices = Notices::default();
    notices.state(
        NoticeSource::Write,
        build_refusal_dialog("Not made", "shown"),
    );
    for reason in ["stale", "newer", "newest"] {
        notices.state(
            NoticeSource::Listing,
            build_refusal_dialog("Not listed", reason),
        );
    }
    notices.state(
        NoticeSource::Paste,
        build_refusal_dialog("Not moved", "after"),
    );
    notices.dismiss();
    assert_eq!(
        shown(&notices),
        Some(("Not listed", Some("newest"))),
        "it keeps its place in line with what it says now"
    );
    notices.dismiss();
    assert_eq!(shown(&notices), Some(("Not moved", Some("after"))));
    notices.dismiss();
    assert!(!notices.is_showing(), "nothing else was kept");
}

#[test]
fn the_showing_notice_is_never_replaced_by_its_own_source() {
    let mut notices = Notices::default();
    notices.state(
        NoticeSource::Listing,
        build_refusal_dialog("Not listed", "read"),
    );
    notices.state(
        NoticeSource::Listing,
        build_refusal_dialog("Not listed", "since"),
    );
    assert_eq!(
        shown(&notices),
        Some(("Not listed", Some("read"))),
        "what the user is reading stays put"
    );
    notices.dismiss();
    assert_eq!(shown(&notices), Some(("Not listed", Some("since"))));
}
