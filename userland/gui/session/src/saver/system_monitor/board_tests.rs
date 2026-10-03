//! The board's layout and drawing: slots that tile the screen at every shape
//! and orbit step, cells that seat every core, and each part drawn inside its
//! own slot alone — which is what lets a reading repaint its part and nothing
//! else.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::switchboard_ipc::{
    MachineComposition, MachineDevice, MachineDeviceName, MachineInterface, MachineInterfaceName,
    MachineTasks,
};
use tairix_abi::sysinfo::{LoadAverage, MountAvailability};
use tairix_abi::Duration64;
use tairix_colour::Rgba;
use tairix_controls::{CompositionBar, MetricTile, PressureKind};
use tairix_theme::{TextRole, Theme};
use tairix_wm::{Color, Point, Rect, Scale, Surface};

use super::super::fixtures::{report, share};
use super::super::verdict::Verdict;
use super::{
    cell_fit, census_line, column_width, columns, composition, detail_line, detailed, device_facts,
    heat, interface_facts, paint, seats, share_text, split, Board, Look, Part, Readings, ORBIT,
    REFERENCE,
};

const MARK: Color = Color::rgb(1, 2, 3);

fn screens() -> [(u32, u32); 7] {
    [
        (1920, 1080),
        (3840, 2160),
        (1280, 1024),
        (1080, 1920),
        (800, 600),
        (2560, 1080),
        REFERENCE,
    ]
}

fn inside(rect: Rect, screen: (u32, u32)) -> bool {
    let whole = Rect::new(0, 0, screen.0, screen.1);
    rect.intersection(&whole) == rect
}

#[test]
fn a_1080p_screen_is_the_reference_board_at_twice_its_size() {
    let board = Board::new((1920, 1080), (0, 0), &Theme::dark()).expect("a board");
    assert_eq!(board.scale(), Scale::from_percent(200).expect("a scale"));
    let board = Board::new(REFERENCE, (0, 0), &Theme::dark()).expect("a board");
    assert_eq!(board.scale(), Scale::ONE);
}

#[test]
fn every_screen_shape_tiles_its_parts_on_screen_without_overlap() {
    for screen in screens() {
        let board = Board::new(screen, (0, 0), &Theme::dark()).expect("a board");
        for part in Part::ALL {
            let slot = board.slot(part);
            assert!(!slot.is_empty(), "{part:?} on {screen:?} has no room");
            assert!(inside(slot, screen), "{part:?} leaves {screen:?}");
            for other in Part::ALL.into_iter().filter(|other| *other != part) {
                assert!(
                    slot.intersection(&board.slot(other)).is_empty(),
                    "{part:?} overlaps {other:?} on {screen:?}"
                );
            }
        }
    }
}

#[test]
fn a_tall_screen_stacks_the_parts_and_a_wide_one_sets_them_side_by_side() {
    let wide = Board::new((1920, 1080), (0, 0), &Theme::dark()).expect("a board");
    assert_eq!(
        wide.slot(Part::Cpu).top(),
        wide.slot(Part::Tasks).top(),
        "the three headline parts share a row"
    );
    let tall = Board::new((1080, 1920), (0, 0), &Theme::dark()).expect("a board");
    assert!(tall.slot(Part::Memory).top() > tall.slot(Part::Cpu).top());
    assert_eq!(tall.slot(Part::Memory).left(), tall.slot(Part::Cpu).left());
}

#[test]
fn the_orbit_never_takes_a_part_off_the_screen() {
    let reach = i32::try_from(ORBIT).expect("small");
    for screen in screens() {
        for shift in [
            (reach, reach),
            (-reach, -reach),
            (reach, -reach),
            (-reach, reach),
        ] {
            let board = Board::new(screen, shift, &Theme::dark()).expect("a board");
            for part in Part::ALL {
                assert!(inside(board.slot(part), screen), "{part:?} at {shift:?}");
            }
        }
        let rest = Board::new(screen, (0, 0), &Theme::dark()).expect("a board");
        let moved = Board::new(screen, (reach, 0), &Theme::dark()).expect("a board");
        assert!(moved.slot(Part::Cpu).left() > rest.slot(Part::Cpu).left());
    }
}

#[test]
fn a_screen_too_small_for_its_margins_has_no_board() {
    assert!(Board::new((20, 20), (0, 0), &Theme::dark()).is_none());
}

#[test]
fn split_tiles_its_length_exactly() {
    let runs = split(10, 101, &[3, 2]);
    assert_eq!(runs, [(10, 60), (70, 41)]);
    let thirds = split(0, 100, &[1, 1, 1]);
    assert_eq!(thirds.iter().map(|(_, run)| run).sum::<u32>(), 100);
}

#[test]
fn every_core_is_seated_and_more_of_them_are_drawn_smaller() {
    let room = (300, 60);
    let mut last = u32::MAX;
    for count in [1usize, 4, 16, 128, 512] {
        let (side, columns) = cell_fit(count, room, 1, 20).expect("seated");
        let rows = u32::try_from(count).expect("small").div_ceil(columns);
        assert!(
            columns * (side + 1) <= room.0 + 1,
            "{count} cores overflow the width"
        );
        assert!(
            rows * (side + 1) <= room.1 + 1,
            "{count} cores overflow the height"
        );
        assert!(side <= last, "more cores drew larger cells");
        last = side;
    }
    assert_eq!(cell_fit(1, room, 1, 20).map(|(side, _)| side), Some(20));
    assert!(cell_fit(0, room, 1, 20).is_none());
    assert!(cell_fit(10_000, (10, 10), 1, 20).is_none());
}

/// Thirteen cells fit across here, so sixteen need two rows — eight to each,
/// not thirteen over a stub of three.
#[test]
fn cores_are_spread_evenly_over_the_rows_they_need() {
    assert_eq!(cell_fit(16, (13 * 21 - 1, 41), 1, 20), Some((20, 8)));
    assert_eq!(cell_fit(9, (13 * 21 - 1, 41), 1, 20), Some((20, 9)));
}

fn look(theme: &Theme) -> Look<'_> {
    Look {
        theme,
        scale: Scale::ONE,
    }
}

/// Every class holding memory, the kernel holding more than the page tables
/// listed before it.
fn classes() -> MachineComposition {
    let gib = 1u64 << 30;
    MachineComposition::new(
        [28 * gib, 6 * gib, gib / 2, 3 * gib, gib / 4, gib],
        25 * gib,
        64 * gib,
    )
    .expect("a composition")
}

fn labels(bar: &CompositionBar) -> Vec<&str> {
    bar.segments()
        .iter()
        .map(tairix_controls::CompositionSegment::label)
        .collect()
}

#[test]
fn a_composition_with_room_names_every_part_in_its_own_order() {
    let theme = Theme::dark();
    let bar = composition(&classes(), (10_000, 10_000), &look(&theme)).expect("a bar");
    assert_eq!(
        labels(&bar),
        [
            "Processes",
            "File cache",
            "Page tables",
            "Kernel",
            "Device buffers",
            "Compressed",
            "Free"
        ]
    );
    let amounts: Vec<&str> = bar
        .segments()
        .iter()
        .map(tairix_controls::CompositionSegment::amount)
        .collect();
    assert_eq!(amounts, ["43%", "9%", "<1%", "4%", "<1%", "1%", "39%"]);
}

/// However short of room, the bar fits it, every part it draws is named, and
/// the parts it names are the largest — a smaller class is never named where
/// a larger one is folded away.
#[test]
fn a_composition_short_of_room_folds_its_smallest_parts_and_never_a_lone_one() {
    let theme = Theme::dark();
    let look = look(&theme);
    let width = 200;
    let gib = 1u64 << 30;
    let bytes = |label: &str| match label {
        "Processes" => 28 * gib,
        "File cache" => 6 * gib,
        "Page tables" => gib / 2,
        "Kernel" => 3 * gib,
        "Device buffers" => gib / 4,
        "Compressed" => gib,
        other => panic!("{other} is not a class"),
    };
    let classes_all = [
        "Processes",
        "File cache",
        "Page tables",
        "Kernel",
        "Device buffers",
        "Compressed",
    ];
    let whole = composition(&classes(), (width, u32::MAX), &look).expect("a bar");
    let tallest = whole.measured_height(width, look.scale, look.theme);
    let mut folded_any = false;
    for room in (0..=tallest).rev() {
        let Some(bar) = composition(&classes(), (width, room), &look) else {
            continue;
        };
        assert!(bar.measured_height(width, look.scale, look.theme) <= room);
        let drawn = labels(&bar);
        let (named, folded): (Vec<&str>, Vec<&str>) = classes_all
            .into_iter()
            .partition(|class| drawn.contains(class));
        assert_eq!(drawn.contains(&"Other"), !folded.is_empty(), "{drawn:?}");
        assert_ne!(folded.len(), 1, "a lone part is renamed, not folded");
        if let (Some(least), Some(most)) = (
            named.iter().map(|class| bytes(class)).min(),
            folded.iter().map(|class| bytes(class)).max(),
        ) {
            assert!(most <= least, "{folded:?} folded while {named:?} named");
        }
        folded_any |= !folded.is_empty();
    }
    assert!(folded_any, "the sweep never folded anything");
    assert!(composition(&classes(), (width, 0), &look).is_none());
}

#[test]
fn a_part_holding_anything_never_reads_as_none_of_the_whole() {
    assert_eq!(share_text(1, 0), "<1%");
    assert_eq!(share_text(1, 9), "<1%");
    assert_eq!(share_text(0, 0), "0%");
    assert_eq!(share_text(5, 430), "43%");
}

/// A line too short for every fact is cut between them, never through one;
/// the first always stands, for the tile to cut with its own mark.
#[test]
fn a_line_of_detail_too_short_for_every_fact_drops_the_last_ones_whole() {
    let theme = Theme::dark();
    let font = look(&theme).font(TextRole::Body);
    let facts = [
        String::from("Degraded"),
        String::from("48.0 MiB/s read"),
        String::from("12.0 MiB/s write"),
    ];
    let bare = MetricTile::new("disk", "60%", PressureKind::Disk);
    let at = |width| detailed(bare.clone(), &facts, (width, font));
    assert_eq!(
        at(u32::MAX),
        bare.clone()
            .with_detail("Degraded \u{b7} 48.0 MiB/s read \u{b7} 12.0 MiB/s write")
    );
    let two = font.text_width("Degraded \u{b7} 48.0 MiB/s read");
    assert_eq!(
        at(two),
        bare.clone().with_detail("Degraded \u{b7} 48.0 MiB/s read")
    );
    assert_eq!(at(1), bare.clone().with_detail("Degraded"));
    assert_eq!(detailed(bare.clone(), &[], (u32::MAX, font)), bare);
}

#[test]
fn a_device_says_how_it_is_faring_first_and_how_full_last() {
    let device = MachineDevice {
        name: MachineDeviceName::new("sda").expect("a name"),
        availability: MountAvailability::Degraded,
        capacity: Some(
            tairix_abi::switchboard_ipc::DeviceCapacity::new(1 << 40, 1 << 39).expect("capacity"),
        ),
        busy: None,
        read_rate: Some(1 << 20),
        write_rate: Some(2 << 20),
    };
    let facts = device_facts(&device);
    assert_eq!(facts.first().map(String::as_str), Some("Degraded"));
    assert!(facts[1].ends_with("read") && facts[2].ends_with("write"));
    assert!(facts[3].contains("TiB"), "{facts:?}");
    let healthy = MachineDevice {
        availability: MountAvailability::Available,
        ..device
    };
    assert!(device_facts(&healthy)[0].ends_with("read"));
}

#[test]
fn an_interface_whose_link_is_down_says_so_first() {
    let interface = MachineInterface {
        name: MachineInterfaceName::new("eth1").expect("a name"),
        link_up: Some(false),
        receive_rate: Some(1024),
        send_rate: Some(2048),
    };
    assert_eq!(interface_facts(&interface)[0], "link down");
    let up = MachineInterface {
        link_up: Some(true),
        ..interface
    };
    assert_eq!(
        interface_facts(&up).last().map(String::as_str),
        Some("link up")
    );
    let unknown = MachineInterface {
        link_up: None,
        ..interface
    };
    assert_eq!(interface_facts(&unknown).len(), 2);
}

/// A list wide enough for two of its narrowest columns is laid in two, and
/// one too narrow even for one is still laid in one.
#[test]
fn a_wide_list_is_laid_in_columns_and_a_narrow_one_in_one() {
    let theme = Theme::dark();
    let look = look(&theme);
    let gap = look.gap();
    assert_eq!(columns(240 * 2 + gap, &look), 2);
    assert_eq!(columns(240 * 2 + gap - 1, &look), 1);
    assert_eq!(columns(0, &look), 1);
    assert_eq!(column_width(101, 2, 1), 50);
    assert_eq!(column_width(10, 0, 1), 10);
    assert_eq!(seats(100, (50, 2), 0), 4);
    assert_eq!(seats(99, (50, 2), 0), 2);
    assert_eq!(seats(100, (0, 2), 0), 200, "a zero pitch divides nothing");
}

#[test]
fn a_busier_core_is_lit_brighter_and_an_idle_one_is_the_groove() {
    let lit = Rgba::rgb(40, 160, 255);
    let groove = Rgba::rgb(30, 30, 34);
    let luma = |colour: Color| u32::from(colour.r) + u32::from(colour.g) + u32::from(colour.b);
    assert_eq!(heat(lit, groove, share(0)), Color::from(groove));
    assert_eq!(heat(lit, groove, share(1000)), Color::from(lit));
    let readings = [0, 250, 500, 750, 1000].map(|value| luma(heat(lit, groove, share(value))));
    assert!(readings.windows(2).all(|pair| pair[0] < pair[1]));
}

fn readings(verdict: &Verdict, stale: bool) -> Readings<'_> {
    Readings {
        host: "rack-07",
        time: "14:02",
        detail: "up 1d 2h 3m",
        verdict,
        name_tasks: true,
        stale,
    }
}

/// Each part draws inside its own slot and nowhere else, so repainting a
/// part's slot alone can never leave a stale pixel of it outside.
#[test]
fn every_part_is_drawn_inside_its_own_slot_alone() {
    let theme = Theme::dark();
    let board = Board::new(REFERENCE, (0, 0), &theme).expect("a board");
    let report = report();
    let verdict = Verdict::of(&report);
    for stale in [false, true] {
        for part in Part::ALL {
            let mut surface = Surface::new(REFERENCE.0, REFERENCE.1).expect("a surface");
            surface.fill(MARK);
            paint(
                &mut surface,
                &board,
                part,
                &theme,
                Some(&report),
                &readings(&verdict, stale),
            );
            let slot = board.slot(part);
            for y in 0..REFERENCE.1 {
                for x in 0..REFERENCE.0 {
                    let at = Point::new(
                        i32::try_from(x).expect("small"),
                        i32::try_from(y).expect("small"),
                    );
                    if !slot.contains(at) {
                        assert_eq!(
                            surface.get(x, y).map(|p| (p.r, p.g, p.b)),
                            Some((MARK.r, MARK.g, MARK.b)),
                            "{part:?} drew outside its slot at ({x}, {y})"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_stale_board_dims_its_panels() {
    let theme = Theme::dark();
    let board = Board::new(REFERENCE, (0, 0), &theme).expect("a board");
    let report = report();
    let verdict = Verdict::of(&report);
    let drawn = |stale| {
        let mut surface = Surface::new(REFERENCE.0, REFERENCE.1).expect("a surface");
        paint(
            &mut surface,
            &board,
            Part::Memory,
            &theme,
            Some(&report),
            &readings(&verdict, stale),
        );
        surface
    };
    let (live, stale) = (drawn(false), drawn(true));
    let light = |surface: &Surface| -> u64 {
        surface
            .pixels()
            .iter()
            .map(|p| u64::from(p.r) + u64::from(p.g) + u64::from(p.b))
            .sum()
    };
    assert!(light(&stale) < light(&live), "a stale panel reads as live");
}

#[test]
fn a_part_with_no_report_still_draws_its_plate() {
    let theme = Theme::dark();
    let board = Board::new(REFERENCE, (0, 0), &theme).expect("a board");
    let mut surface = Surface::new(REFERENCE.0, REFERENCE.1).expect("a surface");
    surface.fill(MARK);
    paint(
        &mut surface,
        &board,
        Part::Cpu,
        &theme,
        None,
        &readings(&Verdict::Waiting, false),
    );
    let slot = board.slot(Part::Cpu);
    let centre = surface
        .get(
            u32::try_from(slot.left()).expect("on screen") + slot.width / 2,
            u32::try_from(slot.top()).expect("on screen") + slot.height / 2,
        )
        .expect("in bounds");
    assert_ne!((centre.r, centre.g, centre.b), (MARK.r, MARK.g, MARK.b));
}

#[test]
fn the_detail_line_says_how_long_the_machine_has_been_up_and_the_date() {
    let up = Some(Duration64::from_secs(93_784));
    assert_eq!(detail_line(up, "Thu 1 Oct"), "up 1d 2h 3m \u{b7} Thu 1 Oct");
    assert_eq!(detail_line(up, ""), "up 1d 2h 3m");
    assert_eq!(detail_line(None, "Thu 1 Oct"), "Thu 1 Oct");
}

#[test]
fn the_census_names_what_needs_recovery_before_the_threads() {
    let load = Some(LoadAverage {
        load1: 0,
        load5: 0,
        load15: 0,
        runnable: 1,
        total_tasks: 412,
        users: 3,
    });
    let calm = MachineTasks::new(Some(100), 0, 0, &[]).expect("tasks");
    assert_eq!(census_line(&calm, load), "412 threads \u{b7} 3 signed in");
    let troubled = MachineTasks::new(Some(100), 2, 3, &[]).expect("tasks");
    assert_eq!(
        census_line(&troubled, load),
        "2 stopped \u{b7} 1 not responding"
    );
    assert_eq!(census_line(&calm, None), String::new());
}
