//! Unit tests for the Tasks section: the table, its arrangement, and the
//! menu a row asks for.

use tairix_abi::ProcessState;
use tairix_geometry::{Point, Rect, Scale};
use tairix_icon::{IconKind, NoArtwork};
use tairix_input::{Key, NamedKey};
use tairix_raster::Surface;
use tairix_theme::Theme;

use tairix_controls::{
    ActivityState, CellAlign, ControlState, PointerState, RecoveryState, TableCell,
};

use super::{
    TaskAuthority, TaskOwner, TaskSummary, COLUMN_WEIGHTS, COL_ACTIVITY, COL_CORE, COL_CPU,
    COL_MEMORY, COL_NETWORK,
};
use tairix_controls::damage;
use tairix_controls::testkit::high_contrast;

use crate::panel::{WIN_HEIGHT, WIN_WIDTH};
use crate::view::frame::resolve_section_frame;
use crate::view::test_support::{
    bounds as fixture_bounds, centre, focus_task_row, font, has_ink, key, list_slot, model, moved,
    pointer, refresh, secondary_click, select_task_row, task_id, task_row_point, turn,
    unreported_change, PRESS, SECONDARY_PRESS,
};
use crate::view::Sweep;
use crate::view::{
    SectionView, Switchboard, SwitchboardAction, SwitchboardModel, UNMEASURED_READING,
};

/// A screen showing `model` on the Tasks section.
///
/// The surface opens on Resources — what the machine is doing is the question
/// a monitor is opened to answer — so a suite about the task list says so
/// rather than relying on whichever section happens to lead the rail.
fn on_tasks(model: &SwitchboardModel) -> Switchboard {
    let mut sb = Switchboard::new(model);
    sb.select_section(crate::view::Section::Tasks);
    sb
}

/// The window the Switchboard actually opens at.
///
/// The keyboard helper lays the screen out in the shared fixture window
/// instead, so a test comparing a key's geometry uses that one.
fn bounds() -> Rect {
    Rect::new(0, 0, WIN_WIDTH, WIN_HEIGHT)
}

/// The screen painted into a surface the size of [`bounds`].
fn painted(sb: &mut Switchboard) -> Surface {
    let b = bounds();
    let mut surface = Surface::new(b.width, b.height).expect("surface");
    sb.render(
        &mut surface,
        b,
        Scale::ONE,
        &Theme::dark(),
        font(),
        &mut NoArtwork,
    );
    surface
}

/// Every command permitted.
fn all_ready() -> TaskAuthority {
    TaskAuthority {
        switch: Ok(()),
        pause: Ok(()),
        resume: Ok(()),
        lower_priority: Ok(()),
        force_quit: Ok(()),
    }
}

#[test]
fn a_table_with_rows_selects_the_first() {
    let sb = on_tasks(&model());
    assert_eq!(
        sb.tasks.selected,
        Some(task_id(0)),
        "a table with something to show always has something selected"
    );
}

#[test]
fn an_empty_table_selects_nothing() {
    let sb = on_tasks(&SwitchboardModel::new("Switchboard"));
    assert_eq!(sb.tasks.selected, None);
}

#[test]
fn the_rows_are_the_whole_section() {
    // A task's commands are its row's own menu, and nothing the table shows
    // needs a control of its own beneath it, so every region but the rows is
    // left unclaimed.
    let theme = Theme::dark();
    let sb = on_tasks(&model());
    let layout = Switchboard::compute_layout(bounds(), Scale::ONE, &theme);
    let frame = resolve_section_frame(layout.content, sb.tasks.anatomy(), Scale::ONE, &theme);
    assert_eq!(frame.rail, None, "no command rail");
    assert_eq!(frame.footer.height, 0, "no footer band");
    assert_eq!(frame.header.height, 0, "no header band");
    assert_eq!(frame.primary, layout.content, "the table takes the content");
}

#[test]
fn a_primary_click_selects_a_row_and_asks_for_nothing() {
    let theme = Theme::dark();
    let mut sb = on_tasks(&model());
    select_task_row(&mut sb, bounds(), Scale::ONE, &theme, 1);
    assert_eq!(sb.tasks.selected, Some(task_id(1)));
}

#[test]
fn a_secondary_press_on_a_row_selects_it_and_asks_for_its_menu_at_the_press() {
    let theme = Theme::dark();
    let b = bounds();
    let mut sb = on_tasks(&model());
    let (x, y) = task_row_point(&sb, b, Scale::ONE, &theme, 2);

    let actions = secondary_click(&mut sb, b, Scale::ONE, &theme, (x, y));

    assert_eq!(
        actions,
        alloc::vec![SwitchboardAction::TaskMenu {
            proc_id: task_id(2),
            anchor: Rect::new(x, y, 0, 0),
        }],
        "one ask, on the press, naming the task and where the pointer was"
    );
    assert_eq!(
        sb.tasks.selected,
        Some(task_id(2)),
        "the row the menu is for is the one lit"
    );
}

#[test]
fn a_secondary_press_names_the_task_a_re_sort_put_under_the_pointer() {
    let theme = Theme::dark();
    let b = bounds();
    let mut sb = on_tasks(&mixed_model());
    // Reverse name order, then ask for the menu of the first row shown.
    for order in [super::SortOrder::Ascending, super::SortOrder::Descending] {
        sb.tasks.header.adopt_sort(Some((0, order)));
        sb.tasks.arrange(&mut Sweep::adopting(&mut damage::sink()));
    }
    let under = sb.tasks.order[0];
    let point = task_row_point(&sb, b, Scale::ONE, &theme, 0);

    let actions = secondary_click(&mut sb, b, Scale::ONE, &theme, point);

    assert!(
        matches!(
            actions.as_slice(),
            [SwitchboardAction::TaskMenu { proc_id, .. }] if *proc_id == task_id(under)
        ),
        "the menu names the task drawn there, not the model's first: {actions:?}"
    );
}

#[test]
fn a_secondary_press_anywhere_but_a_row_asks_for_nothing() {
    let theme = Theme::dark();
    let b = bounds();
    let mut sb = on_tasks(&table_model(&[row(
        "solo",
        1000,
        RecoveryState::None,
        None,
        None,
    )]));
    let layout = Switchboard::compute_layout(b, Scale::ONE, &theme);
    let rows = sb.list_info(&layout, Scale::ONE, &theme);
    let (x, _) = centre(rows.viewport);
    let misses = [
        // The pinned column headings above the rows.
        (x, rows.viewport.top() - 4),
        // The empty tail beneath the only row.
        (x, rows.item_rect(1).top() + 4),
        // The navigation rail.
        centre(layout.rail),
    ];
    for point in misses {
        assert!(
            secondary_click(&mut sb, b, Scale::ONE, &theme, point).is_empty(),
            "{point:?} is over no row"
        );
    }
    assert_eq!(sb.tasks.selected, Some(task_id(0)), "the selection stood");
}

#[test]
fn a_secondary_press_reports_every_pixel_its_selection_moved() {
    let theme = Theme::dark();
    let b = bounds();
    let mut sb = on_tasks(&model());
    let (x, y) = task_row_point(&sb, b, Scale::ONE, &theme, 3);
    assert_eq!(pointer(&mut sb, b, Scale::ONE, &theme, &moved(x, y)), None);
    let before = painted(&mut sb);

    let mut damage = damage::sink();
    let _ = sb.on_pointer(&SECONDARY_PRESS, b, Scale::ONE, &theme, font(), &mut damage);
    let after = painted(&mut sb);

    assert_ne!(before.pixels(), after.pixels(), "the selection moved");
    assert_eq!(
        unreported_change(&before, &after, b, &damage),
        None,
        "the rows the mark left and reached are what the press repainted"
    );
}

#[test]
fn enter_on_a_row_asks_for_its_menu_hanging_from_the_row() {
    let theme = Theme::dark();
    for activation in [Key::Named(NamedKey::Enter), Key::Char(' ')] {
        let mut sb = on_tasks(&model());
        focus_task_row(&mut sb, 1);
        let row = list_slot(&sb, fixture_bounds(), Scale::ONE, &theme, 1);
        assert_eq!(
            key(&mut sb, activation),
            Some(SwitchboardAction::TaskMenu {
                proc_id: task_id(1),
                anchor: row,
            }),
            "{activation:?}"
        );
        assert_eq!(sb.tasks.selected, Some(task_id(1)));
    }
}

#[test]
fn enter_on_a_row_scrolled_out_of_view_brings_it_back_before_the_menu_hangs() {
    // A keyboard reader who then scrolled with the wheel still has the cursor
    // on a row they cannot see; the menu must hang from that row, in view.
    let theme = Theme::dark();
    let b = fixture_bounds();
    let mut sb = on_tasks(&model());
    focus_task_row(&mut sb, 0);
    for _ in 0..8 {
        let _ = pointer(&mut sb, b, Scale::ONE, &theme, &turn(4));
    }
    let layout = Switchboard::compute_layout(b, Scale::ONE, &theme);
    let rows = sb.list_info(&layout, Scale::ONE, &theme);
    assert_eq!(
        rows.window_rect(0, sb.scroll_offset()),
        None,
        "row 0 is out of view"
    );

    let action = key(&mut sb, Key::Named(NamedKey::Enter));

    let shown = rows
        .window_rect(0, sb.scroll_offset())
        .expect("the row was scrolled back into view");
    assert_eq!(
        action,
        Some(SwitchboardAction::TaskMenu {
            proc_id: task_id(0),
            anchor: shown,
        })
    );
    assert_eq!(shown.height, rows.pitch, "and it shows whole");
}

#[test]
fn a_key_that_is_not_an_activation_asks_for_no_menu() {
    let mut sb = on_tasks(&model());
    focus_task_row(&mut sb, 1);
    assert_eq!(key(&mut sb, Key::Char('x')), None);
    assert_eq!(key(&mut sb, Key::Named(NamedKey::Escape)), None);
}

#[test]
fn the_selection_follows_the_task_when_a_re_sort_moves_it() {
    let theme = Theme::dark();
    let mut sb = on_tasks(&model());
    let b = bounds();
    select_task_row(&mut sb, b, Scale::ONE, &theme, 2);
    let chosen = sb.tasks.selected.expect("a selected task");

    // Reverse the name order; the chosen task is now somewhere else.
    sb.tasks
        .header
        .adopt_sort(Some((0, super::SortOrder::Ascending)));
    sb.tasks.arrange(&mut Sweep::adopting(&mut damage::sink()));
    sb.tasks
        .header
        .adopt_sort(Some((0, super::SortOrder::Descending)));
    sb.tasks.arrange(&mut Sweep::adopting(&mut damage::sink()));

    assert_eq!(
        sb.tasks.selected,
        Some(chosen),
        "the selection names the task, never the row it sat in"
    );
}

#[test]
fn a_sample_that_drops_the_selected_task_drops_the_selection() {
    let theme = Theme::dark();
    let mut sb = on_tasks(&model());
    select_task_row(&mut sb, bounds(), Scale::ONE, &theme, 0);
    assert!(sb.tasks.selected.is_some());

    sb.tasks
        .adopt(&table_model(&[]), &mut Sweep::adopting(&mut damage::sink()));

    assert_eq!(sb.tasks.selected, None);
}

#[test]
fn every_sample_is_adopted() {
    // The table always shows the latest sample: nothing holds it back.
    let mut sb = on_tasks(&mixed_model());
    let mut later = mixed_model();
    later.tasks.truncate(1);
    sb.tasks
        .adopt(&later, &mut Sweep::adopting(&mut damage::sink()));
    assert_eq!(sb.tasks.entries.len(), 1);
}

/// One row a table fixture is to build, spelled in the order the table
/// shows it: what it is called, which principal owns it, the condition it
/// is in, and the two figures the sort tests read.
struct RowSpec {
    name: &'static str,
    uid: u32,
    recovery: RecoveryState,
    cpu: Option<u16>,
    memory: Option<u64>,
}

/// One [`RowSpec`], so a fixture reads as a table of rows rather than as a
/// column of struct literals.
const fn row(
    name: &'static str,
    uid: u32,
    recovery: RecoveryState,
    cpu: Option<u16>,
    memory: Option<u64>,
) -> RowSpec {
    RowSpec {
        name,
        uid,
        recovery,
        cpu,
        memory,
    }
}

/// A model of exactly `rows`, so a test can state the sort inputs it is
/// about rather than filtering a generic fixture.
fn table_model(rows: &[RowSpec]) -> SwitchboardModel {
    let mut m = SwitchboardModel::new("Switchboard");
    for (index, spec) in rows.iter().enumerate() {
        m.tasks.push(TaskSummary {
            proc_id: task_id(index),
            name: alloc::string::String::from(spec.name),
            owner: TaskOwner::new(spec.uid),
            core: Some(0),
            lifecycle: Some(ProcessState::Running),
            cpu_permille: spec.cpu,
            memory_bytes: spec.memory,
            recovery: spec.recovery,
            authority: all_ready(),
            ..TaskSummary::default()
        });
    }
    m
}

/// Five rows of mixed owners, readings and conditions the sort tests read.
fn mixed_model() -> SwitchboardModel {
    table_model(&[
        row("alpha", 1000, RecoveryState::None, Some(300), Some(2048)),
        row("Beta", 1000, RecoveryState::None, Some(100), Some(1024)),
        row("gamma", 1000, RecoveryState::Hung, Some(200), None),
        row("delta", 0, RecoveryState::None, None, Some(4096)),
        row("epsilon", 0, RecoveryState::None, Some(50), Some(512)),
    ])
}

/// The shown rows' names, in the order the table would draw them.
fn shown(sb: &Switchboard) -> alloc::vec::Vec<alloc::string::String> {
    sb.tasks
        .order
        .iter()
        .filter_map(|index| sb.tasks.tasks.get(*index))
        .map(|task| task.name.clone())
        .collect()
}

/// Walk the action cursor to `index` within the focused stop.
fn walk_action_to(sb: &mut Switchboard, index: usize) {
    for _ in 0..index {
        assert_eq!(key(sb, Key::Named(NamedKey::Right)), None);
    }
    assert_eq!(sb.active().row_action(), index);
}

/// Sort by the column at `column`, returning the shown names.
fn sorted_by(model: &SwitchboardModel, column: usize) -> alloc::vec::Vec<alloc::string::String> {
    let mut sb = on_tasks(model);
    assert_eq!(
        sb.active().content_focus(),
        0,
        "the cursor opens on the headings"
    );
    walk_action_to(&mut sb, column);
    assert_eq!(key(&mut sb, Key::Named(NamedKey::Enter)), None);
    shown(&sb)
}

#[test]
fn each_sortable_column_orders_by_the_value_its_cells_show() {
    let m = mixed_model();
    assert_eq!(
        sorted_by(&m, 0),
        alloc::vec!["Beta", "alpha", "delta", "epsilon", "gamma"],
        "Task sorts by name"
    );
    assert_eq!(
        sorted_by(&m, 1),
        alloc::vec!["delta", "epsilon", "alpha", "Beta", "gamma"],
        "Owner sorts by uid, stably within an owner"
    );
    assert_eq!(
        sorted_by(&m, 4),
        alloc::vec!["epsilon", "Beta", "gamma", "alpha", "delta"],
        "CPU sorts by share, the unmeasured row last"
    );
    assert_eq!(
        sorted_by(&m, 5),
        alloc::vec!["epsilon", "Beta", "alpha", "delta", "gamma"],
        "Memory sorts by bytes, the unmeasured row last"
    );
}

#[test]
fn the_core_column_sorts_by_the_cpu_each_task_is_on() {
    // The heading offered a sort its comparison never made, so choosing it
    // left the rows as they were.
    let mut m = mixed_model();
    for (task, core) in m
        .tasks
        .iter_mut()
        .zip([Some(3), Some(1), None, Some(0), Some(2)])
    {
        task.core = core;
    }
    assert_eq!(
        sorted_by(&m, COL_CORE),
        alloc::vec!["delta", "Beta", "epsilon", "alpha", "gamma"],
        "Core sorts by the CPU index, the unplaced task last"
    );
}

#[test]
fn the_state_and_disk_columns_sort_by_their_own_readings() {
    let mut m = mixed_model();
    m.tasks[0].lifecycle = Some(ProcessState::Zombie);
    m.tasks[0].disk_bytes_per_sec = Some(90);
    m.tasks[1].disk_bytes_per_sec = Some(10);
    assert_eq!(
        sorted_by(&m, 2).first().map(alloc::string::String::as_str),
        Some("Beta"),
        "State sorts by its own text, so Running precedes Zombie"
    );
    let by_disk = sorted_by(&m, 6);
    assert_eq!(
        by_disk.first().map(alloc::string::String::as_str),
        Some("Beta"),
        "Disk sorts by rate"
    );
    assert_eq!(
        by_disk.get(1).map(alloc::string::String::as_str),
        Some("alpha")
    );
}

#[test]
fn a_second_press_reverses_the_sort_and_the_unmeasured_rows_stay_last() {
    let mut sb = on_tasks(&mixed_model());
    walk_action_to(&mut sb, 4);
    assert_eq!(key(&mut sb, Key::Named(NamedKey::Enter)), None);
    let ascending = shown(&sb);
    assert_eq!(key(&mut sb, Key::Named(NamedKey::Enter)), None);
    let descending = shown(&sb);
    assert_ne!(ascending, descending, "a second press reverses the order");
    assert_eq!(
        descending.first().map(alloc::string::String::as_str),
        Some("delta"),
        "reversing puts the unmeasured row first, never a fabricated zero"
    );
}

#[test]
fn the_sort_is_stable_across_rows_it_cannot_separate() {
    let m = table_model(&[
        row("d", 1000, RecoveryState::None, None, None),
        row("c", 1000, RecoveryState::None, None, None),
        row("b", 1000, RecoveryState::None, None, None),
        row("a", 1000, RecoveryState::None, None, None),
    ]);
    assert_eq!(
        sorted_by(&m, 1),
        alloc::vec!["d", "c", "b", "a"],
        "rows the sort cannot separate keep the order the sample reported"
    );
}

#[test]
fn a_refresh_keeps_the_heading_the_keyboard_is_on() {
    // A sample used to put the headings' cursor back on the first column, so a
    // reader stepping along them was moved every two seconds.
    let mut sb = on_tasks(&mixed_model());
    walk_action_to(&mut sb, 4);

    let _ = refresh(&mut sb, &mixed_model());

    assert_eq!(sb.active().row_action(), 4);
    assert_eq!(sb.tasks.header.focus(), Some(4), "the ring stayed on CPU");
    assert_eq!(key(&mut sb, Key::Named(NamedKey::Enter)), None);
    assert_eq!(
        shown(&sb),
        sorted_by(&mixed_model(), 4),
        "and Enter sorts by the heading the ring is on"
    );
}

#[test]
fn the_cursor_spans_the_headings_then_the_rows() {
    let mut sb = on_tasks(&mixed_model());
    let span = sb.active().focus_span();
    assert_eq!(span, 1 + 5, "one header stop, then five rows");

    let mut rows = alloc::vec::Vec::new();
    for stop in 0..span {
        if stop > 0 {
            assert_eq!(key(&mut sb, Key::Named(NamedKey::Down)), None);
        }
        assert_eq!(sb.active().content_focus(), stop);
        rows.push(sb.active().focus_row(stop));
    }
    let mut expected = alloc::vec![None];
    expected.extend((0..5).map(Some));
    assert_eq!(rows, expected, "only the rows name a line to scroll to");
    assert_eq!(key(&mut sb, Key::Named(NamedKey::Down)), None);
    assert_eq!(
        sb.active().content_focus(),
        span - 1,
        "the last row is the end"
    );
}

#[test]
fn a_table_emptied_under_the_cursor_puts_it_on_the_headings() {
    let mut sb = on_tasks(&mixed_model());
    focus_task_row(&mut sb, 3);
    sb.tasks
        .adopt(&table_model(&[]), &mut Sweep::adopting(&mut damage::sink()));
    assert_eq!(sb.active().content_focus(), 0);
}

#[test]
fn the_column_headings_take_the_ring_back_from_a_row() {
    let mut sb = on_tasks(&mixed_model());
    assert_eq!(key(&mut sb, Key::Named(NamedKey::Down)), None);
    let on_row = sb.tasks.header.clone();
    assert_eq!(key(&mut sb, Key::Named(NamedKey::Up)), None);
    assert_ne!(sb.tasks.header, on_row);
}

#[test]
fn the_activity_column_plots_the_tasks_own_cpu_history() {
    let mut m = mixed_model();
    m.tasks[0].cpu_history = alloc::vec![100, 200, 300];
    let sb = on_tasks(&m);
    assert!(
        !sb.tasks.entries[0].spark.is_empty(),
        "a measured task plots its own readings"
    );
    assert!(
        sb.tasks.entries[1].spark.is_empty(),
        "a task with no history plots nothing rather than a flat fabricated line"
    );
}

#[test]
fn the_activity_sparkline_is_drawn_into_its_own_column() {
    let theme = Theme::dark();
    let mut m = mixed_model();
    m.tasks[0].cpu_history = alloc::vec![100, 900, 100, 900];
    let mut sb = on_tasks(&m);
    let b = bounds();
    let mut surface = Surface::new(b.width, b.height).expect("surface");
    sb.render(&mut surface, b, Scale::ONE, &theme, font(), &mut NoArtwork);

    let layout = Switchboard::compute_layout(b, Scale::ONE, &theme);
    let info = sb.list_info(&layout, Scale::ONE, &theme);
    let item = info.item_rect(0);
    let cells = sb.tasks.entries[0]
        .row
        .cell_rects(item, Scale::ONE, &theme, &COLUMN_WEIGHTS);
    let activity = cells[COL_ACTIVITY];
    assert!(
        has_ink(&surface, activity),
        "the sparkline draws inside the Activity column's own rect"
    );
}

#[test]
fn a_working_task_wears_no_activity_seam_under_its_row() {
    let mut m = mixed_model();
    for task in &mut m.tasks {
        task.activity = ActivityState::Working;
    }
    let sb = on_tasks(&m);
    for entry in &sb.tasks.entries {
        assert_eq!(
            entry.row.state().activity,
            ActivityState::Idle,
            "a row's activity would paint a Heat Seam along its whole lower \
             edge, which reads as a rule under the table rather than as a \
             reading about one task"
        );
    }
}

#[test]
fn a_tasks_activity_changes_nothing_the_table_draws() {
    let theme = Theme::dark();
    let b = bounds();

    let paint = |working: bool| {
        let mut m = mixed_model();
        for task in &mut m.tasks {
            task.activity = if working {
                ActivityState::Working
            } else {
                ActivityState::Idle
            };
            task.cpu_history = alloc::vec![100, 900, 100, 900];
        }
        let mut sb = on_tasks(&m);
        let mut surface = Surface::new(b.width, b.height).expect("surface");
        sb.render(&mut surface, b, Scale::ONE, &theme, font(), &mut NoArtwork);
        surface
    };

    assert_eq!(
        paint(true).pixels(),
        paint(false).pixels(),
        "a row's activity must paint nothing of its own"
    );
}

#[test]
fn the_network_column_renders_an_explicit_unmeasured_mark() {
    let sb = on_tasks(&mixed_model());
    let cells = sb.tasks.entries[0].row.cells();
    // Per-task network has no interface at all, so the column says so —
    // disabled, never as a small figure a reader would take for a rate.
    assert_eq!(
        cells[COL_NETWORK],
        TableCell::new(UNMEASURED_READING)
            .with_align(CellAlign::Trailing)
            .with_state(ControlState::disabled())
    );
}

#[test]
fn the_core_column_reads_the_cpu_the_scheduler_placed_the_task_on() {
    let sb = on_tasks(&mixed_model());
    assert_eq!(
        sb.tasks.entries[0].row.cells()[COL_CORE],
        TableCell::new("0").with_align(CellAlign::Trailing)
    );
}

#[test]
fn an_unmeasured_figure_never_renders_as_a_zero() {
    let sb = on_tasks(&mixed_model());
    // `delta` has no CPU share and `gamma` no memory reading.
    let unmeasured = TableCell::new(UNMEASURED_READING)
        .with_align(CellAlign::Trailing)
        .with_state(ControlState::disabled());
    assert_eq!(sb.tasks.entries[3].row.cells()[COL_CPU], unmeasured);
    assert_eq!(sb.tasks.entries[2].row.cells()[COL_MEMORY], unmeasured);
    assert_eq!(
        sb.tasks.entries[0].row.cells()[COL_CPU],
        TableCell::numeric("30%").with_align(CellAlign::Trailing),
        "a measured share still reads as its own figure"
    );
}

/// The pointer state the row in shown slot `row` is wearing.
fn row_pointer(sb: &Switchboard, row: usize) -> PointerState {
    sb.tasks.entries[row].row.state().pointer
}

/// Move the pointer onto shown row `row`, the way the compositor delivers it.
fn hover_row(sb: &mut Switchboard, b: Rect, theme: &Theme, row: usize) {
    let (x, y) = task_row_point(sb, b, Scale::ONE, theme, row);
    assert_eq!(
        pointer(sb, b, Scale::ONE, theme, &moved(x, y)),
        None,
        "moving onto a row asks for nothing"
    );
}

#[test]
fn a_refresh_keeps_the_hover_on_the_row_under_the_pointer() {
    let m = model();
    let mut sb = on_tasks(&m);
    let (b, theme) = (bounds(), Theme::dark());
    hover_row(&mut sb, b, &theme, 2);
    assert_eq!(row_pointer(&sb, 2), PointerState::Hover);

    let _ = refresh(&mut sb, &m);

    assert_eq!(
        row_pointer(&sb, 2),
        PointerState::Hover,
        "a refresh moves neither the pointer nor the slot it is over"
    );
}

#[test]
fn a_refresh_drops_a_press_begun_on_a_row() {
    let m = model();
    let mut sb = on_tasks(&m);
    let (b, theme) = (bounds(), Theme::dark());
    hover_row(&mut sb, b, &theme, 2);
    assert_eq!(pointer(&mut sb, b, Scale::ONE, &theme, &PRESS), None);
    assert_eq!(row_pointer(&sb, 2), PointerState::Pressed);

    let _ = refresh(&mut sb, &m);

    assert_eq!(
        row_pointer(&sb, 2),
        PointerState::None,
        "the slot may now hold another task, so the press must not survive"
    );

    // A press latch holds wherever the pointer went, so it says nothing about
    // where the pointer is; the next motion states that.
    hover_row(&mut sb, b, &theme, 2);
    assert_eq!(row_pointer(&sb, 2), PointerState::Hover);
}

#[test]
fn a_refresh_that_drops_the_slot_carries_no_hover() {
    let m = model();
    let mut sb = on_tasks(&m);
    let (b, theme) = (bounds(), Theme::dark());
    hover_row(&mut sb, b, &theme, 2);

    let mut shorter = m.clone();
    shorter.tasks.truncate(1);
    let _ = refresh(&mut sb, &shorter);

    assert_eq!(sb.tasks.entries.len(), 1);
    assert_eq!(
        row_pointer(&sb, 0),
        PointerState::None,
        "the pointer is over no row once the slot it was over has gone"
    );
}

#[test]
fn the_table_renders_in_both_themes_and_under_heavier_contrast() {
    for theme in [Theme::dark(), Theme::light(), high_contrast()] {
        let mut m = mixed_model();
        m.tasks[0].cpu_history = alloc::vec![100, 500, 900];
        let mut sb = on_tasks(&m);
        let b = bounds();
        let mut surface = Surface::new(b.width, b.height).expect("surface");
        sb.render(&mut surface, b, Scale::ONE, &theme, font(), &mut NoArtwork);
        let layout = Switchboard::compute_layout(b, Scale::ONE, &theme);
        assert!(has_ink(&surface, layout.content), "the table draws");
    }
}

// --- The row's identity icon -----------------------------------------------

/// A one-task model whose sole task was launched from `bundle`.
fn one_task_from(bundle: Option<&str>) -> SwitchboardModel {
    let mut m = SwitchboardModel::new("Switchboard");
    m.tasks.push(TaskSummary {
        proc_id: task_id(0),
        name: alloc::string::String::from("terminal"),
        bundle: bundle.map(alloc::string::String::from),
        ..TaskSummary::default()
    });
    m
}

#[test]
fn a_row_launched_from_a_bundle_names_the_application_icon() {
    let launched = on_tasks(&one_task_from(Some("/System/Applications/terminal.app")));
    let unattested = on_tasks(&one_task_from(None));

    let leading = |sb: &Switchboard| {
        sb.tasks.entries[0]
            .row
            .cells()
            .iter()
            .find_map(TableCell::icon)
    };
    assert_eq!(leading(&launched), Some(IconKind::AppBundle));
    assert_eq!(
        leading(&unattested),
        Some(IconKind::Executable),
        "a process nothing attests keeps the executable class"
    );
}

#[test]
fn a_rows_bundle_is_carried_to_the_paint_that_resolves_its_picture() {
    let sb = on_tasks(&one_task_from(Some("/Apps/Terminal.app")));
    assert_eq!(
        sb.tasks.entries[0].bundle.as_deref(),
        Some("/Apps/Terminal.app")
    );
}

#[test]
fn a_row_draws_its_icon_whether_or_not_a_cache_answers() {
    // `NoArtwork` holds no cache, so the row falls back to the inline glyph
    // arithmetic. Either way the leading gutter must carry ink.
    let theme = Theme::dark();
    let mut sb = on_tasks(&one_task_from(Some("/Apps/Terminal.app")));
    let b = bounds();
    let mut surface = Surface::new(b.width, b.height).expect("surface");
    sb.render(&mut surface, b, Scale::ONE, &theme, font(), &mut NoArtwork);

    let layout = Switchboard::compute_layout(b, Scale::ONE, &theme);
    let info = sb.list_info(&layout, Scale::ONE, &theme);
    let item = info.item_rect(0);
    let side = sb.tasks.entries[0].row.icon_side(item, Scale::ONE, &theme);
    assert!(side > 0, "the row reserves a slot for its icon");
    let gutter = Rect::new(item.left(), item.top(), side, item.height);
    assert!(has_ink(&surface, gutter), "the icon slot draws something");
}

/// An artwork lookup that records every request it is asked for and answers
/// none, so a test can see what the render *asked* for rather than only what
/// it drew.
#[derive(Default)]
struct RecordingArtwork {
    asked: alloc::vec::Vec<(IconKind, u32)>,
}

impl tairix_icon::IconArtwork for RecordingArtwork {
    fn artwork(
        &mut self,
        request: tairix_icon::IconRequest<'_>,
        side: u32,
    ) -> Option<tairix_icon::IconPicture<'_>> {
        self.asked.push((request.icon_kind(), side));
        None
    }
}

#[test]
fn every_drawn_row_asks_the_cache_for_its_own_picture() {
    let theme = Theme::dark();
    let mut sb = on_tasks(&one_task_from(Some("/Apps/Terminal.app")));
    let b = bounds();
    let mut surface = Surface::new(b.width, b.height).expect("surface");
    let mut artwork = RecordingArtwork::default();
    sb.render(&mut surface, b, Scale::ONE, &theme, font(), &mut artwork);

    let layout = Switchboard::compute_layout(b, Scale::ONE, &theme);
    let info = sb.list_info(&layout, Scale::ONE, &theme);
    let side = sb.tasks.entries[0]
        .row
        .icon_side(info.item_rect(0), Scale::ONE, &theme);
    assert!(
        artwork.asked.contains(&(IconKind::AppBundle, side)),
        "the row must ask for its own picture at the side it draws at: {:?}",
        artwork.asked
    );
}

#[test]
fn a_pointer_over_the_pinned_headings_hovers_no_row_scrolled_beneath_them() {
    let theme = Theme::dark();
    let b = bounds();
    let mut sb = on_tasks(&model());
    let layout = Switchboard::compute_layout(b, Scale::ONE, &theme);
    let rows = sb.list_info(&layout, Scale::ONE, &theme);
    // Half a row down: the top half of row 0 is now under the headings.
    let half = rows.pitch / 2;
    pointer(
        &mut sb,
        b,
        Scale::ONE,
        &theme,
        &tairix_input::InputEvent::PointerScrolled {
            dx: 0,
            dy: i32::try_from(half).unwrap_or(0) * 5 / 2,
        },
    );
    assert_eq!(sb.scroll_offset(), u64::from(half));

    // A point on the headings a few pixels above the rows' viewport. Shifted
    // by the offset alone it would fall inside row 0's hidden top half.
    let (x, _) = centre(rows.viewport);
    let y = rows.viewport.top() - 4;
    assert!(
        rows.item_rect(0)
            .contains(Point::new(x, y + i32::try_from(half).unwrap_or(0))),
        "the probe must sit over the part of row 0 the headings hide"
    );
    let mut reported = damage::sink();
    sb.on_pointer(&moved(x, y), b, Scale::ONE, &theme, font(), &mut reported);

    assert_eq!(
        sb.tasks.entries[0].row.state().pointer,
        PointerState::None,
        "a row the reader cannot see under the pointer must not light"
    );
    assert!(
        reported
            .rects()
            .iter()
            .all(|rect| rect.intersection(&rows.viewport).is_empty()),
        "nothing in the rows' viewport changed: {:?}",
        reported.rects()
    );
    assert!(
        secondary_click(&mut sb, b, Scale::ONE, &theme, (x, y)).is_empty(),
        "nor may a secondary press there reach the hidden half of the row"
    );

    // Just inside the viewport, the visible half of that same row answers.
    sb.on_pointer(
        &moved(x, rows.viewport.top() + 2),
        b,
        Scale::ONE,
        &theme,
        font(),
        &mut damage::sink(),
    );
    assert_eq!(sb.tasks.entries[0].row.state().pointer, PointerState::Hover);
}
