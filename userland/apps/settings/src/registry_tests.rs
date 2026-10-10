//! Unit tests for the pane registry.
//!
//! These hold the registry's totality in both directions — every pane has a
//! row and every row belongs to a pane the enum names — and the properties
//! the shell relies on: one strip row per category, a disclosing category's
//! panes beneath it, a search that reaches every declared label, and a
//! statement on every pane so no category can be silently blank.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use tairix_controls::DisclosureSet;

use crate::form::{Composition, Setting};
use crate::registry::{
    strip_rows, Category, CategoryRow, Group, Location, Pane, PaneBacking, PaneContent, StripRow,
    CATEGORIES,
};

/// Every category the enum names, so the totality test iterates the closed
/// set rather than the table it is checking.
const EVERY_CATEGORY: &[Category] = &[
    Category::General,
    Category::Appearance,
    Category::Wallpaper,
    Category::Theme,
    Category::Displays,
    Category::LockScreen,
    Category::Screensaver,
    Category::Power,
    Category::Networking,
    Category::Bluetooth,
    Category::Sound,
    Category::Notifications,
    Category::Keyboard,
    Category::Mouse,
    Category::Trackpad,
    Category::Touchscreen,
    Category::Printers,
    Category::Accessibility,
    Category::Language,
    Category::Sharing,
    Category::Users,
    Category::Storage,
];

/// Every pane the enum names.
const EVERY_PANE: &[Pane] = &[
    Pane::About,
    Pane::LoginStartup,
    Pane::Caching,
    Pane::DateTime,
    Pane::Appearance,
    Pane::Wallpaper,
    Pane::Theme,
    Pane::Displays,
    Pane::LockScreen,
    Pane::Screensaver,
    Pane::Power,
    Pane::Ethernet,
    Pane::WiFi,
    Pane::Dns,
    Pane::TcpIp,
    Pane::Bluetooth,
    Pane::Sound,
    Pane::Notifications,
    Pane::Keyboard,
    Pane::Mouse,
    Pane::Trackpad,
    Pane::Touchscreen,
    Pane::Printers,
    Pane::Accessibility,
    Pane::Language,
    Pane::Sharing,
    Pane::Users,
    Pane::Storage,
];

#[test]
fn every_category_has_exactly_one_row() {
    for category in EVERY_CATEGORY {
        let listed = CATEGORIES
            .iter()
            .filter(|row| row.category == *category)
            .count();
        assert_eq!(listed, 1, "{category:?} has {listed} rows");
    }
    assert_eq!(CATEGORIES.len(), EVERY_CATEGORY.len());
}

#[test]
fn every_pane_has_exactly_one_row() {
    for pane in EVERY_PANE {
        let listed = CATEGORIES
            .iter()
            .flat_map(|row| row.panes)
            .filter(|row| row.pane == *pane)
            .count();
        assert_eq!(listed, 1, "{pane:?} has {listed} rows");
    }
    let rows: usize = CATEGORIES.iter().map(|row| row.panes.len()).sum();
    assert_eq!(rows, EVERY_PANE.len(), "a row names a pane twice, or none");
}

#[test]
fn no_category_is_empty_and_every_label_is_distinct() {
    let mut labels = BTreeSet::new();
    let mut titles = BTreeSet::new();
    for row in CATEGORIES {
        assert!(!row.panes.is_empty(), "{:?} holds no pane", row.category);
        assert!(!row.label.is_empty(), "{:?} has no label", row.category);
        assert!(
            labels.insert(row.label),
            "two categories read {}",
            row.label
        );
        assert!(row.first_pane().is_some());
        for pane in row.panes {
            assert!(!pane.title.is_empty(), "{:?} has no title", pane.pane);
            assert!(
                titles.insert((row.label, pane.title)),
                "{} lists {} twice",
                row.label,
                pane.title
            );
        }
        // A category holding one pane shares its name, so the trail does not
        // say the same word twice.
        if !row.discloses() {
            assert_eq!(row.panes[0].title, row.label, "{:?}", row.category);
        }
    }
}

/// The absence `pane` states, or a failure naming it.
fn statement_of(pane: Pane) -> (&'static str, &'static str) {
    let Some((_, row)) = pane.locate() else {
        panic!("{pane:?} is not listed");
    };
    let PaneBacking::None { missing, needs } = row.backing else {
        panic!("{pane:?} states no absence");
    };
    (missing, needs)
}

/// Displays names where the interface scale is set, which is true only while
/// that pane offers the row.
#[test]
fn the_displays_pane_points_to_where_the_interface_scale_is_set() {
    let (missing, _) = statement_of(Pane::Displays);
    let appearance = Category::Appearance.row().expect("appearance is listed");
    assert!(missing.contains(appearance.label), "{missing}");
    assert!(appearance
        .panes
        .iter()
        .any(|pane| pane.settings.contains(&Setting::Scale.label())));
}

/// Theme names where the two things a theme would gather are set today,
/// which is true only while those panes offer them.
#[test]
fn the_theme_pane_points_to_where_appearance_and_wallpaper_are_set() {
    let (missing, needs) = statement_of(Pane::Theme);
    for (category, setting) in [
        (Category::Appearance, Setting::Appearance.label()),
        (Category::Wallpaper, "Desktop picture"),
    ] {
        let row = category.row().expect("the category is listed");
        assert!(missing.contains(row.label), "{missing}");
        assert!(
            row.panes
                .iter()
                .any(|pane| pane.settings.contains(&setting)),
            "{category:?} no longer offers {setting}"
        );
    }
    assert!(needs.contains("accent palette"), "{needs}");
}

/// The audio service offers its device controls, so Sound composes them
/// rather than stating an absence the tree contradicts, and offers no Apply:
/// its effect is its feedback.
#[test]
fn the_sound_pane_composes_the_audio_services_controls() {
    let Some((_, row)) = Pane::Sound.locate() else {
        panic!("Sound is not listed");
    };
    assert_eq!(
        row.backing,
        PaneBacking::Composed(PaneContent::Form(Composition::Sound))
    );
    assert_eq!(row.action(), None);
    assert!(
        row.settings.contains(&"Volume"),
        "a reader searching for volume finds it"
    );
}

#[test]
fn every_pane_states_how_the_machine_stands() {
    // A pane with no controls has to say why, or the category is a blank the
    // reader cannot tell from a broken surface.
    for pane in CATEGORIES.iter().flat_map(|row| row.panes) {
        match pane.backing {
            PaneBacking::None { missing, needs } => {
                assert!(!missing.is_empty(), "{:?} states no absence", pane.pane);
                assert!(!needs.is_empty(), "{:?} names no prerequisite", pane.pane);
            }
            PaneBacking::Elsewhere { shows, elsewhere } => {
                assert!(!shows.is_empty(), "{:?} says nothing", pane.pane);
                assert!(
                    !elsewhere.is_empty(),
                    "{:?} names nowhere the setting is reached",
                    pane.pane
                );
            }
            // A composed pane makes no statement: what it draws is what it
            // says. That it draws *something* is the backing's own
            // declaration and so is a compile-time property; what still has
            // to be checked is that a search can reach its rows.
            PaneBacking::Composed(content) => {
                assert_eq!(pane.content(), Some(content), "{:?}", pane.pane);
                assert!(
                    !pane.settings.is_empty(),
                    "{:?} composes controls no search can reach",
                    pane.pane
                );
            }
        }
    }
}

#[test]
fn a_pane_only_declares_settings_it_could_show() {
    // The search index is derived from these labels, so a searchable setting
    // cannot exist without a row that shows it: a pane that composes no
    // controls declares none.
    for pane in CATEGORIES.iter().flat_map(|row| row.panes) {
        if !matches!(pane.backing, PaneBacking::Composed(_)) {
            assert!(
                pane.settings.is_empty(),
                "{:?} declares a setting it cannot show",
                pane.pane
            );
        }
    }
}

/// A launch target names a pane by this, so two panes sharing a name
/// would make the hand-over ambiguous.
#[test]
fn every_pane_has_a_unique_reachable_name() {
    let mut seen: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
    for pane in CATEGORIES.iter().flat_map(|row| row.panes) {
        assert!(!pane.name.is_empty(), "{:?} has no name", pane.pane);
        assert!(
            pane.name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "{:?}'s name is not a plain identifier",
            pane.pane
        );
        assert!(
            !seen.contains(&pane.name),
            "`{}` names more than one pane",
            pane.name
        );
        seen.push(pane.name);
        assert_eq!(Pane::named(pane.name), Some(pane.pane));
        assert_eq!(
            Location::named(pane.name).map(|at| at.pane),
            Some(pane.pane)
        );
    }
}

/// A launch target confers nothing and resolves against the closed set, so
/// a name the registry does not carry is simply not a pane.
#[test]
fn an_unknown_pane_name_resolves_to_nothing() {
    for name in ["", "Wallpaper", "../../etc", "wallpaper "] {
        assert_eq!(Pane::named(name), None, "`{name}` resolved to a pane");
        assert_eq!(Location::named(name), None, "`{name}` resolved to a place");
    }
}

/// The desktop's *Change Background…* hands this name over, so the one
/// spelling both sides share must reach the pane that shows the picture.
#[test]
fn the_shared_wallpaper_pane_name_reaches_the_wallpaper_pane() {
    assert_eq!(
        Pane::named(tairix_wallpaper::WALLPAPER_PANE),
        Some(Pane::Wallpaper)
    );
}

#[test]
fn the_registry_locates_every_category_and_pane() {
    for category in EVERY_CATEGORY {
        assert_eq!(
            category.row().map(|row| row.category),
            Some(*category),
            "{category:?}"
        );
    }
    for pane in EVERY_PANE {
        let located = pane.locate().expect("a located pane");
        assert_eq!(located.1.pane, *pane);
        assert!(
            located
                .0
                .row()
                .is_some_and(|row| row.panes.iter().any(|p| p.pane == *pane)),
            "{pane:?} is not in the category it located to"
        );
    }
}

#[test]
fn the_surface_opens_on_the_first_pane_of_the_first_category() {
    let opening = Location::opening().expect("an opening location");
    let first = CATEGORIES.first().expect("a first category");
    assert_eq!(opening.category, first.category);
    assert_eq!(opening.pane, first.panes[0].pane);
    assert!(opening.rows().is_some());
}

#[test]
fn a_location_naming_a_foreign_pane_resolves_to_nothing() {
    // Fail closed: a pane that does not belong to the category draws nothing
    // rather than whichever pane happened to be first.
    let stray = Location {
        category: Category::Sound,
        pane: Pane::About,
    };
    assert!(stray.rows().is_none());
}

/// A disclosure set with exactly `categories` open.
fn opened(categories: &[Category]) -> DisclosureSet<Category> {
    let mut open = DisclosureSet::closed();
    for category in categories {
        open.set(*category, true);
    }
    open
}

/// The categories `rows` lists, in order.
fn categories_in(rows: &[StripRow]) -> Vec<Category> {
    rows.iter()
        .filter_map(|row| match row {
            StripRow::Category(category) => Some(*category),
            StripRow::Pane(..) => None,
        })
        .collect()
}

#[test]
fn an_unsearched_strip_lists_every_category_and_discloses_the_open_one() {
    let rows = strip_rows(&opened(&[Category::General]), "");
    assert_eq!(categories_in(&rows).len(), CATEGORIES.len());

    // General discloses four panes; nothing else discloses any.
    let disclosed: Vec<StripRow> = rows
        .iter()
        .copied()
        .filter(|row| matches!(row, StripRow::Pane(..)))
        .collect();
    assert_eq!(disclosed.len(), 4);
    assert!(disclosed
        .iter()
        .all(|row| matches!(row, StripRow::Pane(Category::General, _))));

    // The disclosed rows follow their own category immediately.
    let at = rows
        .iter()
        .position(|row| matches!(row, StripRow::Category(Category::General)))
        .expect("the category row");
    assert!(rows[at + 1..at + 5]
        .iter()
        .all(|row| matches!(row, StripRow::Pane(Category::General, _))));
}

/// Opening a second section never closes the first: both lists show, each
/// beneath its own category.
#[test]
fn an_unsearched_strip_discloses_every_open_category_at_once() {
    let rows = strip_rows(&opened(&[Category::General, Category::Networking]), "");
    for category in [Category::General, Category::Networking] {
        let at = rows
            .iter()
            .position(|row| *row == StripRow::Category(category))
            .expect("the category row");
        let panes = category.row().expect("listed").panes;
        for (offset, pane) in panes.iter().enumerate() {
            assert_eq!(
                rows.get(at + 1 + offset),
                Some(&StripRow::Pane(category, pane.pane)),
                "{category:?}'s panes follow it"
            );
        }
    }
    assert_eq!(rows.len(), CATEGORIES.len() + 4 + 4);
}

#[test]
fn a_closed_strip_lists_the_categories_alone() {
    let rows = strip_rows(&DisclosureSet::closed(), "");
    assert_eq!(categories_in(&rows).len(), rows.len());
    let every: Vec<Category> = CATEGORIES.iter().map(|row| row.category).collect();
    assert_eq!(categories_in(&rows), every, "in the table's own order");
}

#[test]
fn a_single_pane_category_never_discloses_a_pane_row() {
    let rows = strip_rows(&opened(&[Category::Sound]), "");
    assert!(!rows.iter().any(|row| matches!(row, StripRow::Pane(..))));
}

#[test]
fn every_strip_row_resolves_to_a_location() {
    for open in EVERY_CATEGORY {
        for row in strip_rows(&opened(&[*open]), "") {
            let location = row.location().expect("a resolved location");
            assert!(location.rows().is_some(), "{row:?} names no pane");
        }
    }
}

/// A category that discloses its panes opens and closes their list, and so
/// shows no pane of its own; every other row shows the pane it stands for.
#[test]
fn only_a_disclosing_category_row_has_no_destination() {
    for row in strip_rows(&opened(&[Category::General, Category::Networking]), "") {
        let discloses = match row {
            StripRow::Category(category) => category.row().expect("listed").discloses(),
            StripRow::Pane(..) => false,
        };
        assert_eq!(row.destination().is_none(), discloses, "{row:?}");
        if !discloses {
            assert_eq!(row.destination(), row.location(), "{row:?}");
        }
    }
}

#[test]
fn a_search_reaches_a_category_by_its_own_label() {
    let rows = strip_rows(&opened(&[Category::General]), "sound");
    assert_eq!(rows, alloc::vec![StripRow::Category(Category::Sound)]);
}

#[test]
fn a_search_reaches_a_pane_by_its_title_and_discloses_only_that_pane() {
    // Whatever is open: a search lists what it matched, not what the reader
    // happened to leave open.
    for open in [DisclosureSet::closed(), opened(&[Category::General])] {
        let rows = strip_rows(&open, "caching");
        assert_eq!(
            rows,
            alloc::vec![
                StripRow::Category(Category::General),
                StripRow::Pane(Category::General, Pane::Caching),
            ],
            "the match is reachable in one press"
        );
    }
}

#[test]
fn a_search_ignores_letter_case() {
    let open = DisclosureSet::closed();
    assert_eq!(
        strip_rows(&open, "BLUETOOTH"),
        strip_rows(&open, "bluetooth")
    );
    assert_eq!(strip_rows(&open, "Wi-Fi"), strip_rows(&open, "wi-fi"));
}

#[test]
fn a_search_that_reaches_nothing_lists_nothing() {
    assert!(strip_rows(&DisclosureSet::closed(), "zzz-no-such-setting").is_empty());
}

#[test]
fn a_category_reached_by_its_own_label_offers_all_its_panes() {
    // The reader has not said which pane, so every one is offered.
    let rows = strip_rows(&DisclosureSet::closed(), "networking");
    assert_eq!(rows.len(), 1 + 4);
    assert_eq!(rows[0], StripRow::Category(Category::Networking));
    assert!(rows[1..]
        .iter()
        .all(|row| matches!(row, StripRow::Pane(Category::Networking, _))));
}

#[test]
fn the_search_index_reaches_every_declared_setting_label() {
    // Whatever labels a pane declares, the search must reach that pane by
    // each of them; a declared label the index missed would be a setting the
    // reader cannot find.
    for row in CATEGORIES {
        for pane in row.panes {
            for setting in pane.settings {
                let rows = strip_rows(&DisclosureSet::closed(), setting);
                assert!(
                    rows.contains(&StripRow::Category(row.category)),
                    "{setting} did not reach {:?}",
                    row.category
                );
                assert!(
                    !row.discloses() || rows.contains(&StripRow::Pane(row.category, pane.pane)),
                    "{setting} did not reach {:?}",
                    pane.pane
                );
            }
        }
    }
}

#[test]
fn an_empty_query_is_not_a_search() {
    // The empty needle matches everything, which must read as "no query" and
    // not as a filter that happens to pass: what is listed is what is open.
    assert!(!strip_rows(&DisclosureSet::closed(), "")
        .iter()
        .any(|row| matches!(row, StripRow::Pane(..))));
    assert!(strip_rows(&opened(&[Category::General]), "")
        .iter()
        .any(|row| matches!(row, StripRow::Pane(Category::General, _))));
}

/// A break falls where one run of categories ends, so each run must be one
/// unbroken stretch of the table: a run split in two would draw a break in
/// the middle of its own group.
#[test]
fn every_group_is_one_unbroken_run_of_the_table() {
    let mut finished: Vec<Group> = Vec::new();
    let mut current: Option<Group> = None;
    for row in CATEGORIES {
        if current != Some(row.group) {
            if let Some(ended) = current {
                finished.push(ended);
            }
            assert!(
                !finished.contains(&row.group),
                "{:?} returns to {:?} after it ended",
                row.category,
                row.group
            );
            current = Some(row.group);
        }
    }
    assert!(finished.len() >= 2, "a sidebar of one run has no breaks");
}

/// The rule both lists draw by: a category is set apart exactly when the one
/// above it belongs to another run, and the first is never set apart.
#[test]
fn a_category_breaks_from_the_one_above_only_across_runs() {
    let mut above = None;
    let mut breaks = 0;
    for row in CATEGORIES {
        let expected = above.is_some_and(|prior: &CategoryRow| prior.group != row.group);
        assert_eq!(row.breaks_from(above), expected, "{:?}", row.category);
        breaks += usize::from(expected);
        above = Some(row);
    }
    let runs = {
        let mut groups: Vec<Group> = CATEGORIES.iter().map(|row| row.group).collect();
        groups.dedup();
        groups.len()
    };
    assert_eq!(breaks, runs - 1, "one break between each pair of runs");
}

/// Theme sits with how the desktop looks, directly beneath Wallpaper.
#[test]
fn theme_follows_wallpaper_in_the_same_run() {
    let at = |category: Category| {
        CATEGORIES
            .iter()
            .position(|row| row.category == category)
            .expect("listed")
    };
    assert_eq!(at(Category::Theme), at(Category::Wallpaper) + 1);
    assert_eq!(
        Category::Theme.row().map(|row| row.group),
        Category::Wallpaper.row().map(|row| row.group)
    );
}

/// Every row the strip draws leads with a badge: a category's own, and a
/// disclosed pane's own — never one borrowed from its category, and never
/// one on a pane its category's row already stands for.
#[test]
fn every_disclosed_pane_carries_its_own_badge_and_no_other_pane_does() {
    let mut badges = BTreeSet::new();
    for row in CATEGORIES {
        assert!(row.icon.badge().is_some(), "{:?}", row.category);
        assert!(badges.insert(row.icon), "{:?} shares a badge", row.category);
    }
    for row in CATEGORIES {
        for pane in row.panes {
            assert_eq!(pane.icon.is_some(), row.discloses(), "{:?}", pane.pane);
            if let Some(icon) = pane.icon {
                assert!(icon.badge().is_some(), "{:?}", pane.pane);
                assert!(badges.insert(icon), "{:?} shares a badge", pane.pane);
            }
        }
    }
}

#[test]
fn the_bundle_presents_no_icon_bar_slot_and_runs_one_instance() {
    // Two properties the *manifest* carries and no Rust constant can: this
    // window is part of the desktop rather than an application the user
    // manages, so closing it ends the program and there is no slot to keep
    // a handle on it — and a second Settings would be a second view of one
    // machine's configuration, each able to overwrite the other's applies.
    //
    // A singleton is the signed header's default, so what this pins is that
    // no `multi-instance` key was ever added; the icon-bar exception has to
    // be declared, so that one is read directly.
    let manifest = include_str!("../AppInfo.toml");
    let declares = |key: &str| {
        manifest
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('#'))
            .any(|line| line.starts_with(key))
    };
    assert!(
        declares("icon-bar = false"),
        "the manifest must declare no icon-bar slot"
    );
    assert!(
        !declares("multi-instance"),
        "a second Settings could overwrite the first's applies"
    );
}

/// A launch names its pane as the vector's **first** operand, because the
/// runtime's argument reader has already dropped the program's own name.
/// Reading the second instead silently loses every launch target, which is
/// what sent *Change Background…* to the wrong pane.
#[test]
fn a_launch_names_its_pane_as_the_first_operand() {
    assert_eq!(Pane::launched(&["wallpaper"]), Some(Pane::Wallpaper));
    assert_eq!(Pane::launched(&["about"]), Some(Pane::About));
    // Not the second: there is never a pane there, and looking for one
    // finds nothing however the desktop spelled the launch.
    assert_eq!(Pane::launched(&["settings", "wallpaper"]), None);
    // And no operands names no pane, which opens the window where it
    // always does.
    assert_eq!(Pane::launched(&[]), None);
    // An operand outside the closed vocabulary confers nothing and reaches
    // nothing.
    assert_eq!(Pane::launched(&["../../etc/passwd"]), None);
}

/// The registry names the one command a pane offers *instead of* a working
/// copy: a reading whose subject another application owns, or the
/// authenticated read a pane's rows cannot exist without. A staged pane's
/// Revert and Apply are the band's own, so the registry names nothing for
/// it; an immediate pane has no band at all, because its effect is its
/// feedback and a stale Apply is a trap.
#[test]
fn the_registry_names_only_the_command_a_pane_has_instead_of_a_working_copy() {
    let action = |pane: Pane| pane.locate().and_then(|(_, row)| row.action());
    assert_eq!(action(Pane::DateTime), Some("Set Date & Time…"));
    assert_eq!(action(Pane::Ethernet), Some("Show Addressing…"));
    assert_eq!(action(Pane::Dns), Some("Show Addressing…"));
    assert_eq!(action(Pane::LoginStartup), None);
    assert_eq!(action(Pane::Caching), None);
    assert_eq!(action(Pane::Appearance), None);
    assert_eq!(action(Pane::Wallpaper), None);
    assert_eq!(action(Pane::About), None);
    assert_eq!(action(Pane::Storage), None);
    // A pane that states an absence offers nothing to press either.
    assert_eq!(action(Pane::Sound), None);
}
