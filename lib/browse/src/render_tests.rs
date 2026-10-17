use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::sysinfo::MountAvailability;
use tairix_abi::time::Time64;
use tairix_abi::Errno;
use tairix_geometry::{Point, Rect, Scale};
use tairix_icon::IconKind;
use tairix_theme::Theme;

use super::{
    drive_cells, entry_row, header_sort_at, list_header_rect, listing_top, ListColumns, Tiles,
};
use crate::browser::Browser;
use crate::chrome::ToolbarBand;
use crate::entry::{DriveMark, Entry, EntryKind};
use crate::layout::ViewMode;
use crate::places::VolumeFacts;
use crate::sort::{SortDirection, SortKey, SortMode};
use crate::source::{DirectorySource, Listing};

const BAND: ToolbarBand = ToolbarBand::Shown;

/// A source every folder of which lists `entries`.
struct Fixed(Vec<Entry>);

impl DirectorySource for Fixed {
    fn list(&mut self, _components: &[String]) -> Result<Listing, Errno> {
        Ok(Listing::Ready(self.0.clone()))
    }
}

fn facts(free: Option<u64>, availability: MountAvailability) -> VolumeFacts {
    VolumeFacts {
        filesystem: String::from("arxfs"),
        free,
        availability,
    }
}

fn drive(name: &str, free: Option<u64>, availability: MountAvailability) -> Entry {
    let mut root = Entry::directory(name);
    root.set_drive(Some(&DriveMark {
        icon: IconKind::DiskSolidState,
        facts: facts(free, availability),
    }));
    root
}

fn texts(row: &tairix_controls::TableRow) -> Vec<&str> {
    row.cells()
        .iter()
        .map(tairix_controls::TableCell::text)
        .collect()
}

#[test]
fn only_a_listing_of_drives_alone_takes_the_drive_columns() {
    assert_eq!(ListColumns::of(&[]), ListColumns::Files);
    let drives = [
        drive("Backup", Some(1 << 30), MountAvailability::Available),
        drive("Stick", None, MountAvailability::Degraded),
    ];
    assert_eq!(ListColumns::of(&drives), ListColumns::Drives);
    let mixed = [drives[0].clone(), Entry::directory("Stray")];
    assert_eq!(ListColumns::of(&mixed), ListColumns::Files);
}

#[test]
fn a_drive_states_its_free_space_filesystem_and_health_in_either_set_of_columns() {
    let healthy = facts(Some(3 << 30), MountAvailability::Available);
    assert_eq!(
        drive_cells(&healthy, ListColumns::Drives),
        [
            "3.0 GiB".to_string(),
            "arxfs".to_string(),
            "Healthy".to_string()
        ]
    );
    assert_eq!(
        drive_cells(&healthy, ListColumns::Files)[0],
        "3.0 GiB free",
        "under a Size title the space is said to be free"
    );
    let unaccounted = facts(None, MountAvailability::Recovering);
    let cells = drive_cells(&unaccounted, ListColumns::Drives);
    assert!(
        cells[0].is_empty(),
        "a volume keeping no accounting states none"
    );
    assert!(cells[2].starts_with("Degraded"), "{}", cells[2]);
}

#[test]
fn a_file_row_states_its_size_modification_and_type_and_a_folder_no_size() {
    let file = Entry::new(
        "notes.txt",
        EntryKind::File,
        2048,
        Time64::from_secs(86_400 * 365),
    );
    let row = entry_row((&file, &[]), ListColumns::Files, false, IconKind::Generic);
    let cells = texts(&row);
    assert_eq!(cells[0], "notes.txt");
    assert_eq!(cells[1], "2.0 KiB");
    assert_eq!(cells[2], "1971-01-01 00:00");
    assert!(!cells[3].is_empty(), "the type is named");
    assert_eq!(row.cells()[0].icon(), Some(IconKind::Generic));

    let folder = Entry::directory("Docs");
    let row = entry_row((&folder, &[]), ListColumns::Files, true, IconKind::Folder);
    assert!(texts(&row)[1].is_empty(), "a folder has no meaningful size");
}

/// A browser at the root of `entries`, in `view`.
fn browser(entries: Vec<Entry>, view: ViewMode) -> Browser<Fixed> {
    let mut browser = Browser::open_root(Fixed(entries)).expect("the root lists");
    browser.set_view_mode(view);
    browser
}

/// The middle of the list header's `column`th title cell. The columns share
/// the header's width out in proportion to their declared widths.
fn header_cell(browser: &Browser<Fixed>, window: Rect, column: usize) -> Point {
    let theme = Theme::dark();
    let header =
        list_header_rect(browser, Scale::ONE, &theme, window, BAND).expect("a list draws one");
    let total: u32 = super::COLUMNS.iter().sum();
    let before: u32 = super::COLUMNS.iter().take(column).sum();
    let middle =
        u64::from(before + super::COLUMNS[column] / 2) * u64::from(header.width) / u64::from(total);
    Point::new(
        header.left() + i32::try_from(middle).expect("small"),
        header.top() + i32::try_from(header.height / 2).expect("small"),
    )
}

#[test]
fn a_press_on_a_column_title_orders_by_it_and_a_second_reverses_it() {
    let theme = Theme::dark();
    let window = Rect::new(0, 0, 900, 400);
    let mut browser = browser(
        alloc::vec![Entry::file("b"), Entry::file("a")],
        ViewMode::List,
    );
    let on_size = header_cell(&browser, window, 1);
    let asked = header_sort_at(&browser, Scale::ONE, &theme, window, BAND, on_size);
    assert_eq!(
        asked,
        Some(SortMode {
            key: SortKey::Size,
            direction: SortDirection::Ascending,
        })
    );
    browser.set_sort_mode(asked.expect("an order"));
    let again = header_sort_at(&browser, Scale::ONE, &theme, window, BAND, on_size);
    assert_eq!(
        again.map(|mode| mode.direction),
        Some(SortDirection::Descending)
    );

    let below = Point::new(
        on_size.x,
        i32::try_from(listing_top(ViewMode::List, Scale::ONE, &theme, BAND)).expect("small") + 2,
    );
    assert_eq!(
        header_sort_at(&browser, Scale::ONE, &theme, window, BAND, below),
        None,
        "a row is no title"
    );
}

#[test]
fn a_drive_listing_titles_and_orders_its_columns_by_what_a_volume_states() {
    let theme = Theme::dark();
    let window = Rect::new(0, 0, 900, 400);
    let browser = browser(
        alloc::vec![drive("Backup", Some(1), MountAvailability::Available)],
        ViewMode::List,
    );
    for (column, key) in [
        (1, SortKey::Free),
        (2, SortKey::Filesystem),
        (3, SortKey::Health),
    ] {
        let at = header_cell(&browser, window, column);
        let asked = header_sort_at(&browser, Scale::ONE, &theme, window, BAND, at);
        assert_eq!(asked.map(|mode| mode.key), Some(key), "column {column}");
    }
}

#[test]
fn the_tile_displays_draw_no_header_and_begin_under_the_toolbar() {
    let theme = Theme::dark();
    let window = Rect::new(0, 0, 900, 400);
    for view in [ViewMode::LargeIcons, ViewMode::SmallIcons] {
        let browser = browser(alloc::vec![Entry::file("a")], view);
        assert_eq!(
            list_header_rect(&browser, Scale::ONE, &theme, window, BAND),
            None
        );
        assert_eq!(
            header_sort_at(
                &browser,
                Scale::ONE,
                &theme,
                window,
                BAND,
                Point::new(10, 40)
            ),
            None
        );
        assert_eq!(
            listing_top(view, Scale::ONE, &theme, BAND),
            super::chrome_height(Scale::ONE, &theme, BAND)
        );
    }
    assert!(
        listing_top(ViewMode::List, Scale::ONE, &theme, BAND)
            > super::chrome_height(Scale::ONE, &theme, BAND)
    );
}

/// A small tile is a list row's picture beside one line of name, so many more
/// fit a window than large ones.
#[test]
fn a_small_tile_sets_one_line_of_name_beside_a_row_sized_picture() {
    let theme = Theme::dark();
    for percent in [100, 200] {
        let scale = Scale::from_percent(percent).expect("a valid scale");
        let small = Tiles::Small.metrics(scale, &theme);
        let large = Tiles::Large.metrics(scale, &theme);
        assert!(small.cell_height < large.cell_height, "at {percent}%");
        let cell = Rect::new(0, 0, small.cell_width, small.cell_height);
        let layout = Tiles::Small.layout();
        assert_eq!(layout.label_lines(cell, scale, &theme), 1, "at {percent}%");
        let side = layout.icon_side(cell, scale, &theme);
        assert!(
            side > 0 && side < small.cell_width / 2,
            "at {percent}%: {side}"
        );
    }
    assert_eq!(Tiles::of(ViewMode::SmallIcons), Some(Tiles::Small));
    assert_eq!(Tiles::of(ViewMode::LargeIcons), Some(Tiles::Large));
    assert_eq!(Tiles::of(ViewMode::List), None);
}
