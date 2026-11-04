use tairix_abi::fs::Filetype;
use tairix_abi::window_ipc::{SaveFormat, SaveFormats};
use tairix_browse::media::MediaType;
use tairix_geometry::{Point, Rect, Scale};
use tairix_theme::Theme;

use super::{format_label, PanelFocus, PanelHit, SavePanel};

/// The formats `offered` describe — each a file type and its extensions —
/// the one at `current` the document's.
fn formats(offered: &[(Option<u16>, &[&str])], current: usize) -> SaveFormats {
    let mut formats = SaveFormats::ANY;
    for (at, (filetype, extensions)) in offered.iter().enumerate() {
        let mut format = SaveFormat::new(filetype.and_then(Filetype::new));
        for extension in *extensions {
            format.push_extension(extension).expect("an extension");
        }
        formats.push(format, at == current).expect("a format");
    }
    formats
}

/// A picture offered as a PNG or a JPEG, the PNG current.
fn png_or_jpeg() -> SaveFormats {
    formats(&[(Some(0xB60), &[".png"]), (Some(0xC85), &[".jpg"])], 0)
}

/// A document offered as text alone.
fn text() -> SaveFormats {
    formats(&[(Some(0xFFF), &[".txt"])], 0)
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
/// keeps the ending; a name ending as another type is offered without that
/// ending, the whole name selected.
#[test]
fn the_name_opens_with_its_stem_selected() {
    let panel = SavePanel::new("Holiday.png", &png_or_jpeg());
    assert_eq!(panel.name.selected_text(), Some("Holiday"));
    let riscos = SavePanel::new("Sprites,ff9", &formats(&[(Some(0xFF9), &[])], 0));
    assert_eq!(riscos.name.selected_text(), Some("Sprites"));
    let other = SavePanel::new("notes.txt", &formats(&[(None, &[".md"])], 0));
    assert_eq!(
        other.name.text(),
        "notes",
        "a text file's ending is left off"
    );
    assert_eq!(other.name.selected_text(), Some("notes"));
}

/// The panel opens on the format the document is in, whatever its name ends
/// in or does not, names it by its type and draws it with its type's picture.
#[test]
fn the_panel_opens_on_the_documents_format() {
    let sprite_or_png = formats(&[(Some(0xB60), &[".png"]), (Some(0xFF9), &[])], 1);
    let bare = SavePanel::new("Sprites", &sprite_or_png);
    assert_eq!(bare.format(), Some(1));
    assert_eq!(bare.name.text(), "Sprites", "a sprite file needs no ending");
    assert_eq!(bare.icon(), MediaType::ImageSprite.icon());
    let converted = SavePanel::new("photo.png", &sprite_or_png);
    assert_eq!(
        converted.format(),
        Some(1),
        "the document's format, not the name's"
    );
    assert_eq!(converted.name.text(), "photo");
    let menu = converted.format_menu();
    assert_eq!(menu.rows().len(), 2, "one row a format");
}

/// Choosing a format puts the name in it and keeps the stem; typing an
/// ending of another format offered chooses that one.
#[test]
fn a_format_chosen_replaces_the_ending_and_keeps_the_stem() {
    let mut panel = SavePanel::new("Holiday.PNG", &png_or_jpeg());
    assert_eq!(panel.format(), Some(0));
    assert!(panel.set_format(1));
    assert_eq!(panel.name.text(), "Holiday.jpg");
    assert_eq!(panel.format(), Some(1));
    assert!(!panel.set_format(2), "only a format the requester writes");
    assert_eq!(panel.name.text(), "Holiday.jpg");
    panel.name.set_text("Holiday.png");
    panel.restate_format();
    assert_eq!(panel.format(), Some(0), "typed into the other format");
}

/// One format is stated, not offered as a choice; none is no row at all.
#[test]
fn one_format_is_stated_and_none_offers_no_row() {
    let theme = Theme::light();
    let one = SavePanel::new("notes.txt", &text());
    assert!(one.layout(0, 480, Scale::ONE, &theme).format_box.is_some());
    let any = SavePanel::new("notes", &SaveFormats::ANY);
    assert!(any.layout(0, 480, Scale::ONE, &theme).format_box.is_none());
}

/// Every part sits inside the panel, none on another, and a press on each
/// lands on it, wherever the panel's top edge is; *New Folder* is there only
/// while the folders show.
#[test]
fn every_part_is_its_own_and_a_press_lands_on_it() {
    let theme = Theme::light();
    let mut panel = SavePanel::new("Holiday.png", &png_or_jpeg());
    assert_eq!(panel.layout(0, 480, Scale::ONE, &theme).new_folder, None);
    panel.set_expanded(true);
    for top in [0, 320] {
        let layout = panel.layout(top, 480, Scale::ONE, &theme);
        let parts = [
            (layout.icon, PanelHit::Icon),
            (layout.name, PanelHit::Name),
            (layout.where_box, PanelHit::Where),
            (layout.format_box.expect("two formats"), PanelHit::Format),
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
/// rather than an ending, and one the registry does not know by its ending.
#[test]
fn a_format_is_named_by_its_type() {
    let png = formats(&[(Some(0xB60), &[".png"])], 0);
    assert_eq!(
        format_label(png.get(0).expect("a format")),
        MediaType::ImagePng.name()
    );
    let unknown = formats(&[(None, &[".zzq"])], 0);
    assert_eq!(format_label(unknown.get(0).expect("a format")), ".zzq");
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
    let mut one = SavePanel::new("notes.txt", &text());
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
    let mut two = SavePanel::new("Holiday.png", &png_or_jpeg());
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
    let mut panel = SavePanel::new("Holiday.png", &png_or_jpeg());
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
