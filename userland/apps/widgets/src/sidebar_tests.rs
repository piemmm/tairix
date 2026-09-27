//! Host tests for the sidebar demo: sections open and close independently
//! from the pointer and from the tree keys, and a chosen page stays selected
//! through its section closing and opening again.

use alloc::vec::Vec;

use tairix_controls::{damage, Tab};
use tairix_geometry::{Point, Rect, Scale};
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_theme::Theme;

use crate::sidebar::SidebarDemo;

const BOUNDS: Rect = Rect::new(0, 0, 240, 400);

fn labels(demo: &SidebarDemo) -> Vec<&str> {
    demo.strip().tabs().iter().map(Tab::label).collect()
}

fn click_entry(demo: &mut SidebarDemo, label: &str) {
    let theme = Theme::dark();
    let index = labels(demo)
        .iter()
        .position(|shown| *shown == label)
        .unwrap_or_else(|| panic!("{label} is listed"));
    let area = demo
        .strip()
        .tab_area(index, BOUNDS, Scale::ONE, &theme)
        .expect("seated");
    let at = Point::new(area.left() + 40, area.top() + 4);
    let mut sink = damage::sink();
    for event in [
        InputEvent::PointerMoved { to: at },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ] {
        demo.on_pointer(&event, BOUNDS, Scale::ONE, &theme, &mut sink);
    }
}

fn selected_label(demo: &SidebarDemo) -> Option<&str> {
    let index = demo.strip().selected()?;
    Some(demo.strip().tabs()[index].label())
}

#[test]
fn opening_a_second_section_leaves_the_first_open() {
    let mut demo = SidebarDemo::new();
    assert_eq!(
        labels(&demo),
        ["General", "About", "Caching", "Networking", "Storage"]
    );
    click_entry(&mut demo, "Networking");
    assert_eq!(
        labels(&demo),
        [
            "General",
            "About",
            "Caching",
            "Networking",
            "Ethernet",
            "DNS",
            "Storage"
        ]
    );
}

#[test]
fn a_closed_section_is_stood_for_by_its_entry_until_it_opens_again() {
    let mut demo = SidebarDemo::new();
    click_entry(&mut demo, "Caching");
    assert_eq!(selected_label(&demo), Some("Caching"));
    click_entry(&mut demo, "General");
    assert_eq!(labels(&demo), ["General", "Networking", "Storage"]);
    assert_eq!(selected_label(&demo), Some("General"));
    click_entry(&mut demo, "General");
    assert_eq!(selected_label(&demo), Some("Caching"));
}

#[test]
fn the_tree_keys_open_and_close_a_section_and_keep_the_cursor() {
    let theme = Theme::dark();
    let mut demo = SidebarDemo::new();
    let mut sink = damage::sink();
    demo.adopt_current(Some(3));
    assert!(demo.on_key(
        Key::Named(NamedKey::Right),
        BOUNDS,
        Scale::ONE,
        &theme,
        &mut sink
    ));
    assert_eq!(labels(&demo).len(), 7, "Networking opened");
    assert_eq!(demo.strip().current(), Some(3), "the cursor stays on it");
    assert!(demo.on_key(
        Key::Named(NamedKey::Left),
        BOUNDS,
        Scale::ONE,
        &theme,
        &mut sink
    ));
    assert_eq!(labels(&demo).len(), 5, "Networking closed");
    assert_eq!(demo.strip().current(), Some(3));
}

#[test]
fn the_plain_section_starts_its_own_group() {
    let demo = SidebarDemo::new();
    let storage = demo
        .strip()
        .tabs()
        .iter()
        .find(|tab| tab.label() == "Storage")
        .expect("listed");
    assert!(storage.is_group_break());
    assert_eq!(storage.disclosure(), None);
}
