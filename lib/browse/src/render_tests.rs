use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::sysinfo::MountAvailability;
use tairix_abi::time::Time64;
use tairix_abi::Errno;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_icon::IconKind;
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_theme::Theme;

use super::{
    clash_layout, drive_cells, entry_row, header_sort_at, list_header_rect, listing_top,
    progress_window_extent, replace_word, rest_label, ClashAnswer, ClashChoice, ClashPanel,
    ListColumns, Tiles,
};
use crate::browser::Browser;
use crate::chrome::ToolbarBand;
use crate::entry::{DriveMark, Entry, EntryKind};
use crate::layout::ViewMode;
use crate::places::VolumeFacts;
use crate::presentation::HiddenEndings;
use crate::progress::{Clash, ProgressModel, ProgressOp, Subject};
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
        title: String::new(),
        class: tairix_abi::sysinfo::VolumeClass::Local,
        free,
        availability,
        owner: None,
        volume_id: [0; 16],
        identity: [0; 16],
        read_only: false,
        titled: false,
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
    let row = entry_row(
        (&file, &[]),
        ListColumns::Files,
        (false, HiddenEndings::NONE),
        IconKind::Generic,
    );
    let cells = texts(&row);
    assert_eq!(cells[0], "notes.txt");
    assert_eq!(cells[1], "2.0 KiB");
    assert_eq!(cells[2], "1971-01-01 00:00");
    assert!(!cells[3].is_empty(), "the type is named");
    assert_eq!(row.cells()[0].icon(), Some(IconKind::Generic));

    let folder = Entry::directory("Docs");
    let row = entry_row(
        (&folder, &[]),
        ListColumns::Files,
        (true, HiddenEndings::NONE),
        IconKind::Folder,
    );
    assert!(texts(&row)[1].is_empty(), "a folder has no meaningful size");
}

/// A row names its entry as the window shows it, without the endings it
/// hides.
#[test]
fn a_row_names_its_entry_without_the_endings_hidden() {
    let bundle = Entry::new("Paint.app", EntryKind::Bundle, 0, Time64::UNIX_EPOCH);
    let hiding = HiddenEndings {
        applications: true,
        known_types: false,
    };
    let row = entry_row(
        (&bundle, &[]),
        ListColumns::Files,
        (false, hiding),
        IconKind::AppBundle,
    );
    assert_eq!(texts(&row)[0], "Paint");
    let exact = entry_row(
        (&bundle, &[]),
        ListColumns::Files,
        (false, HiddenEndings::NONE),
        IconKind::AppBundle,
    );
    assert_eq!(texts(&exact)[0], "Paint.app");
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
            ..SortMode::default_order()
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

/// The client of a progress window asking about a name taken, at its own
/// extent.
fn asking_client(theme: &Theme) -> Rect {
    let (width, height) = progress_window_extent(Scale::ONE, theme, true);
    Rect::new(0, 0, width, height)
}

/// The question about `one.jpg`, taken in `Photos`, by a folder where
/// `folders` says.
fn clash_panel(folders: bool) -> ClashPanel {
    ClashPanel::new(&Clash {
        name: String::from("one.jpg"),
        folder: String::from("Photos"),
        folders,
    })
}

/// A click — the pointer brought there, pressed and let go — at the centre of
/// `at`.
fn click(panel: &mut ClashPanel, frame: (Rect, Scale, &Theme), at: Rect) -> Option<ClashAnswer> {
    let centre = Point::new(
        at.left() + i32::try_from(at.width / 2).unwrap_or(0),
        at.top() + i32::try_from(at.height / 2).unwrap_or(0),
    );
    let mut damage = Region::new();
    let mut answer = None;
    for input in [
        InputEvent::PointerMoved { to: centre },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ] {
        if let Some(given) = panel.on_pointer(frame, &input, &mut damage) {
            answer = Some(given);
        }
    }
    answer
}

/// The question's first choice replaces a file and merges a folder.
#[test]
fn a_folder_meeting_a_folder_is_offered_a_merge() {
    assert_eq!(replace_word(false), "Replace");
    assert_eq!(replace_word(true), "Merge");
}

/// The box answering the rest names the kind it answers for: a merge given
/// for the rest never answers a question about a file.
#[test]
fn the_box_for_the_rest_names_the_kind_it_answers_for() {
    assert_eq!(
        rest_label(false),
        "Do the same for every file that already exists"
    );
    assert_eq!(
        rest_label(true),
        "Do the same for every folder that already exists"
    );
}

/// A click on a drawn choice answers with it once released over it, the box
/// ticked first answering for the rest; a press alone answers nothing.
#[test]
fn the_question_is_answered_by_a_click_on_what_it_draws() {
    let theme = Theme::dark();
    let frame = (asking_client(&theme), Scale::ONE, &theme);
    let mut panel = clash_panel(false);
    let layout = clash_layout(frame, &panel);
    let choices = [ClashChoice::Stop, ClashChoice::Replace, ClashChoice::Skip];
    for (at, choice) in layout.choices.iter().zip(choices) {
        assert_eq!(frame.0.intersection(at), *at, "{choice:?} is drawn inside");
        assert_eq!(
            click(&mut panel, frame, *at),
            Some(ClashAnswer {
                choice,
                for_the_rest: false
            })
        );
    }
    assert_eq!(
        click(&mut panel, frame, layout.rest),
        None,
        "the box answers nothing"
    );
    assert!(panel.for_the_rest());
    assert_eq!(
        click(&mut panel, frame, layout.choices[2]),
        Some(ClashAnswer {
            choice: ClashChoice::Skip,
            for_the_rest: true
        })
    );
    let press = InputEvent::PointerPressed {
        button: PointerButton::Primary,
    };
    let mut damage = Region::new();
    assert_eq!(
        panel.on_pointer(frame, &press, &mut damage),
        None,
        "a press alone answers nothing"
    );
}

/// From the keyboard the default is Skip: `Enter` takes it, `Escape` is Stop,
/// `Tab` moves between the box and the choices and `Shift+Tab` back, and
/// `Space` ticks the box.
#[test]
fn the_question_is_answered_from_the_keyboard() {
    let theme = Theme::dark();
    let frame = (asking_client(&theme), Scale::ONE, &theme);
    let mut damage = Region::new();
    let mut panel = clash_panel(true);
    let enter = Key::Named(NamedKey::Enter);
    let tab = Key::Named(NamedKey::Tab);
    let answer = |choice, for_the_rest| {
        Some(ClashAnswer {
            choice,
            for_the_rest,
        })
    };
    assert_eq!(
        panel.on_key(enter, false, frame, &mut damage),
        answer(ClashChoice::Skip, false)
    );
    let escape = Key::Named(NamedKey::Escape);
    assert_eq!(
        panel.on_key(escape, false, frame, &mut damage),
        answer(ClashChoice::Stop, false)
    );
    assert_eq!(
        panel.on_key(tab, false, frame, &mut damage),
        None,
        "onto the box"
    );
    assert!(!damage.is_empty(), "the ring's move is redrawn");
    assert_eq!(
        panel.on_key(Key::Char(' '), false, frame, &mut damage),
        None
    );
    assert!(panel.for_the_rest());
    assert_eq!(
        panel.on_key(enter, false, frame, &mut damage),
        answer(ClashChoice::Skip, true),
        "Enter on the box gives the default"
    );
    assert_eq!(
        panel.on_key(tab, true, frame, &mut damage),
        None,
        "onto Skip"
    );
    assert_eq!(
        panel.on_key(tab, true, frame, &mut damage),
        None,
        "onto Merge"
    );
    assert_eq!(
        panel.on_key(enter, false, frame, &mut damage),
        answer(ClashChoice::Replace, true)
    );
}

/// A window asking is taller than one working, and one fitted shorter keeps
/// its choices on show.
#[test]
fn a_window_asking_grows_to_its_question_and_keeps_its_choices_on_show() {
    let theme = Theme::dark();
    let working = progress_window_extent(Scale::ONE, &theme, false);
    let asking = progress_window_extent(Scale::ONE, &theme, true);
    assert_eq!(working.0, asking.0);
    assert!(asking.1 > working.1);
    let short = Rect::new(0, 0, asking.0, working.1);
    let layout = clash_layout((short, Scale::ONE, &theme), &clash_panel(false));
    for at in layout.choices {
        assert_eq!(
            short.intersection(&at),
            at,
            "drawn inside the shorter window"
        );
    }
}

/// A window asking paints its question and its controls, and a tiny one
/// paints without panicking.
#[test]
fn a_window_asking_paints_its_question() {
    use tairix_controls::value::Progress;
    use tairix_raster::Surface;

    let theme = Theme::dark();
    let client = asking_client(&theme);
    let mut model = ProgressModel::new(
        ProgressOp::Copy,
        Subject::One(String::from("Photos")),
        Some(String::from("Desktop")),
    );
    model.set_asking(Some(Clash {
        name: String::from("Photos"),
        folder: String::from("Desktop"),
        folders: true,
    }));
    let trace = Progress::new().uncaptioned();
    let panel = clash_panel(true);
    let mut surface = Surface::new(client.width, client.height).expect("surface");
    let before = surface.pixels().to_vec();
    super::draw_progress_window(
        &mut surface,
        (&model, &trace, Some(&panel)),
        Scale::ONE,
        &theme,
        client,
    );
    assert_ne!(surface.pixels().to_vec(), before);
    let mut tiny = Surface::new(2, 2).expect("tiny surface");
    super::draw_progress_window(
        &mut tiny,
        (&model, &trace, Some(&panel)),
        Scale::ONE,
        &theme,
        Rect::new(0, 0, 2, 2),
    );
}
