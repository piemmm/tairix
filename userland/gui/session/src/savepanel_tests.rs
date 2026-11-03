use tairix_abi::window_ipc::SaveEndings;
use tairix_geometry::{Point, Rect, Scale};
use tairix_theme::Theme;

use super::{format_label, PanelFocus, PanelHit, SavePanel};

/// The endings `codes` name, each a `.` extension.
fn endings(codes: &[&str]) -> SaveEndings {
    let mut endings = SaveEndings::ANY;
    for code in codes {
        endings.push('.', code).expect("a valid ending");
    }
    endings
}

fn centre(rect: Rect) -> Point {
    Point::new(
        rect.left()
            .saturating_add(i32::try_from(rect.width / 2).unwrap_or(0)),
        rect.top()
            .saturating_add(i32::try_from(rect.height / 2).unwrap_or(0)),
    )
}

/// The name opens with its stem selected, so typing replaces the name and
/// keeps the ending; a name in none of the endings selects what comes before
/// its own.
#[test]
fn the_name_opens_with_its_stem_selected() {
    let panel = SavePanel::new("Holiday.png", endings(&["png", "jpg"]));
    assert_eq!(panel.name.selected_text(), Some("Holiday"));
    let riscos = {
        let mut ending = SaveEndings::ANY;
        ending.push(',', "ff9").expect("a filetype");
        SavePanel::new("Sprites,ff9", ending)
    };
    assert_eq!(riscos.name.selected_text(), Some("Sprites"));
    let other = SavePanel::new("notes.txt", endings(&["md"]));
    assert_eq!(other.name.selected_text(), Some("notes"));
}

/// Choosing a format puts the name in it and keeps the stem; the format
/// the name is in is read back from it, whatever its case.
#[test]
fn a_format_chosen_replaces_the_ending_and_keeps_the_stem() {
    let mut panel = SavePanel::new("Holiday.PNG", endings(&["png", "jpg"]));
    assert_eq!(
        panel.format(),
        Some(0),
        "an ending matches without regard to case"
    );
    assert!(panel.set_format(1));
    assert_eq!(panel.name.text(), "Holiday.jpg");
    assert_eq!(panel.format(), Some(1));
    assert!(!panel.set_format(2), "only an ending the requester writes");
    assert_eq!(panel.name.text(), "Holiday.jpg");
    let menu = panel.format_menu();
    assert_eq!(menu.rows().len(), 2);
}

/// One ending is no choice: the panel offers no format row.
#[test]
fn one_ending_offers_no_format() {
    let theme = Theme::light();
    let one = SavePanel::new("notes.txt", endings(&["txt"]));
    assert!(one.layout(0, 480, Scale::ONE, &theme).format_box.is_none());
    let two = SavePanel::new("notes.txt", endings(&["txt", "md"]));
    assert!(two.layout(0, 480, Scale::ONE, &theme).format_box.is_some());
    let any = SavePanel::new("notes", SaveEndings::ANY);
    assert!(any.layout(0, 480, Scale::ONE, &theme).format_box.is_none());
}

/// Every part sits inside the panel, none on another, and a press on each
/// lands on it, wherever the panel's top edge is; *New Folder* is there only
/// while the folders show.
#[test]
fn every_part_is_its_own_and_a_press_lands_on_it() {
    let theme = Theme::light();
    let mut panel = SavePanel::new("Holiday.png", endings(&["png", "jpg"]));
    assert_eq!(panel.layout(0, 480, Scale::ONE, &theme).new_folder, None);
    panel.set_expanded(true);
    for top in [0, 320] {
        let layout = panel.layout(top, 480, Scale::ONE, &theme);
        let parts = [
            (layout.icon, PanelHit::Icon),
            (layout.name, PanelHit::Name),
            (layout.where_box, PanelHit::Where),
            (layout.format_box.expect("two endings"), PanelHit::Format),
            (layout.browse, PanelHit::Browse),
            (
                layout.new_folder.expect("the folders show"),
                PanelHit::NewFolder,
            ),
            (layout.cancel, PanelHit::Cancel),
            (layout.save, PanelHit::Save),
        ];
        for (rect, hit) in parts {
            assert!(rect.width > 0 && rect.height > 0, "{hit:?}");
            assert!(layout.panel.contains(rect.origin), "{hit:?} inside");
            assert!(
                layout.panel.right() >= rect.right() && layout.panel.bottom() >= rect.bottom(),
                "{hit:?} inside"
            );
            assert_eq!(layout.hit(centre(rect)), Some(hit));
            for (other, other_hit) in parts {
                if other_hit != hit {
                    assert!(
                        rect.intersection(&other).is_empty(),
                        "{hit:?} on {other_hit:?}"
                    );
                }
            }
        }
        assert_eq!(layout.panel.top(), top);
    }
}

/// A format is named by its type, so the user chooses a kind of document
/// rather than an ending.
#[test]
fn a_format_is_named_by_its_type_and_its_ending() {
    let label = format_label(".png");
    assert!(label.ends_with("(.png)"), "{label}");
    assert!(label.len() > "(.png)".len(), "{label} names the type");
}

/// `Tab` walks every control that can take the keyboard and wraps, and
/// `Shift+Tab` walks back; the format is reached only when there is a choice,
/// and the listing and *New Folder* only while the folders show.
#[test]
fn tab_walks_the_controls_that_can_take_the_keyboard() {
    let walk = |panel: &mut SavePanel, back: bool| {
        let mut seen = alloc::vec![panel.focus()];
        loop {
            panel.move_focus(back);
            if panel.focus() == PanelFocus::Name {
                return seen;
            }
            seen.push(panel.focus());
        }
    };
    let mut one = SavePanel::new("notes.txt", endings(&["txt"]));
    assert_eq!(
        walk(&mut one, false),
        [
            PanelFocus::Name,
            PanelFocus::Where,
            PanelFocus::Browse,
            PanelFocus::Cancel,
            PanelFocus::Save
        ]
    );
    let mut two = SavePanel::new("Holiday.png", endings(&["png", "jpg"]));
    two.set_expanded(true);
    assert_eq!(
        walk(&mut two, false),
        [
            PanelFocus::Name,
            PanelFocus::Where,
            PanelFocus::Format,
            PanelFocus::Browse,
            PanelFocus::Listing,
            PanelFocus::NewFolder,
            PanelFocus::Cancel,
            PanelFocus::Save
        ]
    );
    two.move_focus(true);
    assert_eq!(
        two.focus(),
        PanelFocus::Save,
        "back from the name wraps to Save"
    );
    two.set_focus(PanelFocus::NewFolder);
    two.set_expanded(false);
    assert_eq!(
        two.focus(),
        PanelFocus::Name,
        "a control folded away gives the keyboard back"
    );
}

/// Naming a folder offers its name selected and keeps the document's to
/// come back to; the format is no choice meanwhile.
#[test]
fn a_folder_named_keeps_the_documents_name_to_come_back_to() {
    let mut panel = SavePanel::new("Holiday.png", endings(&["png", "jpg"]));
    panel.set_expanded(true);
    panel.set_focus(PanelFocus::Cancel);
    panel.begin_naming("New Folder");
    assert!(panel.naming());
    assert_eq!(panel.name.selected_text(), Some("New Folder"));
    assert_eq!(panel.focus(), PanelFocus::Name);
    panel.move_focus(false);
    panel.move_focus(false);
    assert_eq!(
        panel.focus(),
        PanelFocus::Browse,
        "past Where, the format is skipped"
    );
    panel.end_naming();
    assert!(!panel.naming());
    assert_eq!(panel.name.text(), "Holiday.png");
    assert_eq!(panel.name.selected_text(), Some("Holiday"));
    assert_eq!(panel.focus(), PanelFocus::Name);
}
