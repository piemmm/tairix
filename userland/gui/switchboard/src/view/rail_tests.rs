//! Unit tests for the surface's navigation rail: that a group with no
//! entries states why it is empty, in its own rail position, that stating it
//! shifts no entry's index, that pressing an entry selects it and repaints the
//! pane it now draws, and that a rail longer than its column scrolls a pixel
//! at a time behind a bar of its own.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_geometry::{Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey};
use tairix_theme::Theme;

use tairix_controls::{damage, Chart, PressureKind};
use tairix_theme::SignalRole;

use crate::view::reading::{Reading, Unmeasured};
use crate::view::resources::{
    DeviceId, PaneHero, RailGroup, ResourceDevice, ResourceReport, StorageId, Trace,
};
use crate::view::test_support::{
    bounds, centre, click, font, key, model, moved, refresh, report as reported_by, shot, turn,
    unreported_change, DETENT_PX, PRESS, RELEASE,
};
use crate::view::{RailSubject, Section, Switchboard, SwitchboardModel};

/// A bare rail entry in `group`, with no instrument and no pane detail: this
/// suite is about which groups the rail states, not what a pane draws.
fn entry(id: DeviceId, group: RailGroup, name: &str) -> ResourceDevice {
    ResourceDevice {
        id,
        group,
        name: String::from(name),
        kind: PressureKind::Disk,
        reading: Reading::measured("41%"),
        trend: Trace::Absent,
        hero: PaneHero::facts(Reading::measured("41%"), "%"),
        blocks: Vec::new(),
        banner: None,
        actions: Vec::new(),
    }
}

/// A report over `devices`, with the two absence verdicts as given.
fn report(
    devices: Vec<ResourceDevice>,
    storage_absent: Option<Unmeasured>,
    interfaces_absent: Option<Unmeasured>,
) -> ResourceReport {
    ResourceReport {
        devices,
        storage_absent,
        interfaces_absent,
    }
}

/// The processor and the machine's memory, which always answer.
fn resources() -> Vec<ResourceDevice> {
    alloc::vec![
        entry(DeviceId::Cpu, RailGroup::Resources, "CPU"),
        entry(DeviceId::Memory, RailGroup::Resources, "Memory"),
    ]
}

/// The processor, the machine's memory, and `device` beside them.
fn resources_with(device: ResourceDevice) -> Vec<ResourceDevice> {
    let mut devices = resources();
    devices.push(device);
    devices
}

/// One storage device on the rail.
fn disk() -> ResourceDevice {
    entry(
        DeviceId::Storage(StorageId::Device(7)),
        RailGroup::Storage,
        "virtio-blk · ARXFSRoot",
    )
}

/// The graphics entry, which always has a pane.
fn graphics() -> ResourceDevice {
    entry(DeviceId::Graphics, RailGroup::Graphics, "Compositor")
}

/// Each stated absence as `(heading, statement)`, in rail order.
/// The rail the shell builds for `report`, with `selected` showing.
fn rail_for(report: &ResourceReport, selected: DeviceId) -> tairix_controls::Tabs {
    let mut model = model();
    model.resources = report.clone();
    let mut screen = Switchboard::new(&model);
    screen.select_section(Section::Resources);
    screen
        .resources
        .select_device(selected, &mut super::Sweep::adopting(&mut damage::sink()));
    screen.build_rail(&model)
}

fn absences(report: &ResourceReport) -> Vec<tairix_controls::TabGroupAbsence> {
    let mut model = model();
    model.resources = report.clone();
    Switchboard::rail_absences(&model)
}

fn stated(report: &ResourceReport) -> Vec<(String, String)> {
    let mut model = model();
    model.resources = report.clone();
    Switchboard::rail_absences(&model)
        .iter()
        .map(|absence| {
            (
                String::from(absence.heading()),
                String::from(absence.statement()),
            )
        })
        .collect()
}

#[test]
fn a_group_with_entries_states_no_absence() {
    let report = report(
        alloc::vec![
            resources().remove(0),
            disk(),
            entry(DeviceId::Interface([0; 16]), RailGroup::Network, "eth0"),
            graphics(),
        ],
        None,
        None,
    );
    assert!(stated(&report).is_empty());
}

#[test]
fn an_empty_group_the_query_answered_says_there_is_none() {
    // The mount table answered and named no storage device: the machine has
    // none, which is a fact about the machine rather than about this
    // session's authority.
    let report = report(alloc::vec![resources().remove(0), graphics()], None, None);
    assert_eq!(
        stated(&report),
        alloc::vec![
            (
                String::from("STORAGE"),
                String::from("No storage device is present.")
            ),
            (
                String::from("NETWORK"),
                String::from("No managed interface is present.")
            ),
        ]
    );
}

#[test]
fn an_empty_group_the_query_was_refused_says_so_instead() {
    // A refusal and an absence are different answers, and the whole point of
    // the report carrying the verdict is that a reader can tell them apart.
    let report = report(
        alloc::vec![resources().remove(0), graphics()],
        Some(Unmeasured::NotPermitted),
        Some(Unmeasured::Unavailable),
    );
    let stated = stated(&report);
    assert_eq!(stated[0].0, "STORAGE");
    assert!(
        stated[0].1.contains("not permitted"),
        "a refused inventory must not read as a machine with no disk: {:?}",
        stated[0].1
    );
    assert_eq!(stated[1].0, "NETWORK");
    assert!(stated[1].1.contains("unavailable"), "{:?}", stated[1].1);
}

#[test]
fn an_empty_group_is_stated_in_its_own_rail_position() {
    // STORAGE sits between RESOURCES and GRAPHICS, so its absence belongs
    // there — not after everything, where it would read as a footnote.
    let mut devices = resources();
    devices.push(graphics());
    let report = report(devices, None, None);
    let absences = absences(&report);
    let storage = absences
        .iter()
        .find(|absence| absence.heading() == "STORAGE")
        .expect("the empty storage group is stated");
    assert_eq!(
        storage.before(),
        3,
        "before the graphics entry, after Tasks and the two resources entries"
    );
}

#[test]
fn a_trailing_empty_group_is_stated_last() {
    // Nothing follows NETWORK on this rail, so its absence sits at the end.
    let mut devices = resources();
    devices.push(disk());
    let report = report(devices, None, None);
    let absences = absences(&report);
    let network = absences
        .iter()
        .find(|absence| absence.heading() == "NETWORK")
        .expect("the empty network group is stated");
    assert_eq!(network.before(), 4);
}

#[test]
fn stating_an_absence_shifts_no_entry_index() {
    // The selection is resolved by counting *items*, so an absence drawn
    // among them must not move one: a rail that renumbered its entries would
    // select the device below the one a reader pressed.
    let mut devices = resources();
    devices.push(graphics());
    let report = report(devices, Some(Unmeasured::NotPermitted), None);
    let rail = rail_for(&report, DeviceId::Graphics);
    assert_eq!(rail.len(), 5, "Tasks, two resources, graphics, Recovery");
    assert_eq!(rail.absences().len(), 2);
    assert_eq!(
        rail.selected(),
        Some(3),
        "the graphics entry is the fourth *item*, whatever is drawn between them"
    );
}

/// Adopting the same readings twice must leave the rail alone: a strip that
/// restated as "moved" every sample would repaint the whole column once a
/// second for nothing.
#[test]
fn adopting_the_same_readings_twice_does_not_move_the_rail() {
    let m = model();
    let mut sb = Switchboard::new(&m);
    let _ = shot(&mut sb);
    let before = sb.rail.clone();
    let _ = refresh(&mut sb, &m);
    assert_eq!(sb.rail, before, "the same model must rebuild the same rail");
}

/// The screen on the Resources section, showing the shared fixture report.
fn resources_screen() -> Switchboard {
    let mut sb = Switchboard::new(&model());
    sb.select_section(Section::Resources);
    let _ = shot(&mut sb);
    sb
}

/// The window point that hits the rail entry for *device* `index`, read from
/// the strip's own layout so a test aims where the screen really seats it.
///
/// Offset past the Tasks entry that leads the rail, so a suite about devices
/// counts devices rather than rail rows.
fn rail_point(sb: &Switchboard, index: usize) -> (i32, i32) {
    centre(shown_entry(sb, index + 1).expect("the entry shows"))
}

/// The fixture window's rail column split between the strip and its bar.
fn rail_frame(sb: &Switchboard) -> super::RailFrame {
    let theme = Theme::dark();
    sb.rail_frame(rail_rect(), Scale::ONE, &theme)
}

/// Rail entry `index` in the strip's own unscrolled layout.
fn laid_entry(sb: &Switchboard, index: usize) -> Rect {
    sb.rail
        .tab_area(index, rail_frame(sb).strip, Scale::ONE, &Theme::dark())
        .expect("the rail has the entry")
}

/// How far the rail is scrolled where the paint draws it.
fn drawn_offset(sb: &Switchboard) -> u64 {
    sb.rail_model(&rail_frame(sb), Scale::ONE, &Theme::dark())
        .offset()
}

/// Where rail entry `index` shows in the window as the paint draws it, cut to
/// the rail's viewport, or `None` when the rail is scrolled clear of it.
fn shown_entry(sb: &Switchboard, index: usize) -> Option<Rect> {
    let rail = rail_frame(sb);
    tairix_controls::ScrollView::new(
        tairix_controls::ScrollOrientation::Vertical,
        rail.viewport,
        drawn_offset(sb),
    )
    .to_window(laid_entry(sb, index))
}

/// The navigation rail's own rectangle in the fixture window.
fn rail_rect() -> Rect {
    Switchboard::compute_layout(bounds(), Scale::ONE, &Theme::dark()).rail
}

/// Feed one event to the screen, accumulating what it reports into `reported`.
fn feed(sb: &mut Switchboard, event: &InputEvent, reported: &mut Region) {
    sb.on_pointer(
        event,
        bounds(),
        Scale::ONE,
        &Theme::dark(),
        font(),
        reported,
    );
}

/// The fixture's storage device, the entry these tests select onto.
const STORAGE: DeviceId = DeviceId::Storage(StorageId::Device(0x5953_2001));

#[test]
fn selecting_a_device_reports_the_pane_it_now_draws() {
    // The pane is the whole point of pressing a rail entry, so a press that
    // switches device owes every pixel the new pane draws. Reporting only
    // the strip's own lift leaves the reader looking at the previous
    // device's readings until something else repaints the window whole.
    let mut sb = resources_screen();
    let before = shot(&mut sb);
    let (x, y) = rail_point(&sb, 2);
    let mut reported = damage::sink();
    for event in [moved(x, y), PRESS, RELEASE] {
        feed(&mut sb, &event, &mut reported);
    }
    assert_eq!(sb.resources.selected, Some(STORAGE), "the press selected");
    let after = shot(&mut sb);
    assert_eq!(
        unreported_change(&before, &after, bounds(), &reported),
        None,
        "a press that switches pane must report every pixel it moved"
    );
}

#[test]
fn a_sample_landing_under_a_resting_pointer_does_not_swallow_the_click() {
    // A reader moves onto an entry, rests, and clicks. A sample lands in
    // between — they arrive about once a second — and the press must still
    // select what the pointer is over.
    let mut sb = resources_screen();
    let (x, y) = rail_point(&sb, 2);
    let mut reported = damage::sink();
    feed(&mut sb, &moved(x, y), &mut reported);
    let _ = refresh(&mut sb, &model());
    feed(&mut sb, &PRESS, &mut reported);
    feed(&mut sb, &RELEASE, &mut reported);
    assert_eq!(
        sb.resources.selected,
        Some(STORAGE),
        "a sample between the pointer's motion and its press swallowed the click"
    );
}

#[test]
fn a_sample_keeps_the_lift_under_a_resting_pointer() {
    // The lift states where the pointer is, and the pointer has not moved,
    // so re-deriving the strip from an identical sample must draw the same
    // sidebar rather than blinking the highlight off.
    let mut sb = resources_screen();
    let (x, y) = rail_point(&sb, 2);
    feed(&mut sb, &moved(x, y), &mut damage::sink());
    let before = shot(&mut sb);
    let _ = refresh(&mut sb, &model());
    let after = shot(&mut sb);
    assert_eq!(
        unreported_change(&before, &after, rail_rect(), &damage::sink()),
        None,
        "an identical sample must leave the rail's own pixels alone"
    );
}

#[test]
fn the_keyboard_reports_the_pane_it_selects_onto() {
    // The cursor on a rail entry *is* the selection, so Down owes the new
    // pane exactly as a press does.
    let mut sb = resources_screen();
    // The rail is a focus region of its own, reached by cycling Tab round
    // from the content to the scrollbar and on to the rail.
    let mut discard = damage::sink();
    for _ in 0..2 {
        sb.on_key(
            Key::Named(NamedKey::Tab),
            bounds(),
            Scale::ONE,
            &Theme::dark(),
            font(),
            &mut discard,
        );
    }
    let before = shot(&mut sb);
    let mut reported = damage::sink();
    sb.on_key(
        Key::Named(NamedKey::Down),
        bounds(),
        Scale::ONE,
        &Theme::dark(),
        font(),
        &mut reported,
    );
    assert_ne!(
        sb.resources.selected,
        Some(DeviceId::Cpu),
        "Down moved off the first entry"
    );
    let after = shot(&mut sb);
    assert_eq!(
        unreported_change(&before, &after, bounds(), &reported),
        None,
        "a cursor move that switches pane must report every pixel it moved"
    );
}

#[test]
fn the_pressure_banner_draws_nothing_outside_the_pane() {
    // The banner's summary and detail are model text of any length. Drawn
    // past the pane they land in the gap beside it and over the action
    // column, where no repaint of the pane can clean them up. The entry with
    // no banner is the control: nothing draws in the gap for either, so the
    // two must leave it identical.
    let theme = Theme::dark();
    let b = bounds();
    let mut sb = resources_screen();
    let bare = shot(&mut sb);

    let (x, y) = rail_point(&sb, 1);
    let _ = click(&mut sb, b, Scale::ONE, &theme, x, y);
    assert!(
        sb.resources
            .device()
            .and_then(|device| device.banner.as_ref())
            .is_some(),
        "the memory entry wears a pressure banner"
    );
    let bannered = shot(&mut sb);

    let layout = Switchboard::compute_layout(b, Scale::ONE, &theme);
    let ctx = sb.section_ctx(&layout, b, Scale::ONE, &theme, font());
    let pane = ctx.frame.primary;
    let rail = ctx.frame.rail.expect("the fixture window seats a rail");
    let gap = Rect::new(
        pane.right(),
        pane.top(),
        u32::try_from(rail.left().saturating_sub(pane.right())).unwrap_or(0),
        pane.height,
    );
    assert!(gap.width > 0, "the fixture seats a gap to overrun into");
    assert_eq!(
        unreported_change(&bare, &bannered, gap, &damage::sink()),
        None,
        "the banner drew outside its own pane"
    );
}

/// The keyboard cursor is the reader's own, not the sample's. A rebuild that
/// pinned it to the selection would snap a reader who has moved it down the
/// rail — to compare two devices before committing — back to the selected
/// entry once a second, and would light the focus ring on a rail nobody has
/// focused.
#[test]
fn a_rebuilt_rail_leaves_the_readers_cursor_alone() {
    let report = report(resources(), None, None);
    let rail = rail_for(&report, DeviceId::Memory);

    assert_eq!(
        rail.selected(),
        Some(2),
        "the rail shows the selected device, one past the Tasks entry"
    );
    assert_eq!(
        rail.current(),
        None,
        "a fresh sample claimed a keyboard cursor of its own"
    );
}

/// The rail draws every entry's trace through the one definition the pane
/// hero also draws through, so a direction's colour cannot differ between the
/// sidebar and the pane it opens.
#[test]
fn a_rail_entry_draws_its_devices_own_trace() {
    let read = alloc::vec![100u16, 400, 900];
    let write = alloc::vec![50u16, 80, 120];
    let mut disk = disk();
    disk.trend = Trace::Duplex {
        inbound: SignalRole::DiskRead,
        outbound: SignalRole::DiskWrite,
        into: read.clone(),
        out: write.clone(),
    };
    let report = report(resources_with(disk.clone()), None, None);
    let rail = rail_for(&report, disk.id);
    let entry = rail
        .tabs()
        .iter()
        .find(|tab| tab.label() == disk.name)
        .expect("the disk has a rail entry");
    assert_eq!(
        entry.trend(),
        disk.trend.chart().as_ref(),
        "the rail's chart is the trace's own"
    );
    // Reads and writes are separate directions, so the sidebar shows both.
    assert_eq!(
        disk.trend.chart(),
        Some(
            Chart::new(SignalRole::DiskRead)
                .with_samples(read)
                .with_opposing(SignalRole::DiskWrite, write)
        )
    );
}

/// A `Machine` entry states facts, not rates, and the absence of an
/// instrument is what says so.
#[test]
fn a_rail_entry_with_no_readings_draws_no_trace() {
    let report = report(resources(), None, None);
    let rail = rail_for(&report, DeviceId::Cpu);
    for tab in rail.tabs() {
        assert!(tab.trend().is_none(), "{} plots nothing", tab.label());
    }
}

/// The two subjects that are not devices carry their own signals: what the
/// machine is running, and what needs recovering. Neither borrows a
/// resource's hue — a task count drawn in the compute colour read as a second
/// CPU trace beside the real one, and the stopped share drew in the thermal
/// one.
#[test]
fn the_task_and_recovery_entries_carry_their_own_signals() {
    let mut model = model();
    model.tasks_trend = Trace::counted(SignalRole::Workload, alloc::vec![3, 5, 4], 8);
    model.recovery_trend = Trace::single(SignalRole::Recovery, alloc::vec![120, 240]);
    let mut screen = Switchboard::new(&model);
    screen.select_section(Section::Tasks);
    let rail = screen.build_rail(&model);
    let trend_of = |label: &str| {
        rail.tabs()
            .iter()
            .find(|tab| tab.label() == label)
            .and_then(|tab| tab.trend())
            .cloned()
    };
    assert_eq!(trend_of("Tasks"), model.tasks_trend.chart());
    assert_eq!(trend_of("Recovery"), model.recovery_trend.chart());
    assert_ne!(model.tasks_trend.chart(), model.recovery_trend.chart());
}

// --- A rail longer than its column --------------------------------------------

/// The fixture model with a dozen more storage devices, each with a trace,
/// filed with the storage group so the rail keeps its group order: a rail far
/// taller than the fixture window.
fn long_rail_model() -> SwitchboardModel {
    let mut m = model();
    let disks: Vec<ResourceDevice> = (0..12u64)
        .map(|index| {
            let mut disk = entry(
                DeviceId::Storage(StorageId::Device(100 + index)),
                RailGroup::Storage,
                &alloc::format!("disk {index}"),
            );
            disk.trend = Trace::single(SignalRole::DiskRead, alloc::vec![100, 400, 200]);
            disk
        })
        .collect();
    let after_storage = m
        .resources
        .devices
        .iter()
        .position(|device| device.group == RailGroup::Storage)
        .map_or(m.resources.devices.len(), |at| at + 1);
    m.resources
        .devices
        .splice(after_storage..after_storage, disks);
    m
}

/// The screen on the long rail, laid out once.
fn long_rail_screen() -> Switchboard {
    let mut sb = Switchboard::new(&long_rail_model());
    let _ = shot(&mut sb);
    sb
}

/// Every rectangle of `damage` lies inside `area`.
fn all_within(damage: &Region, area: Rect) -> bool {
    damage
        .rects()
        .iter()
        .all(|rect| rect.intersection(&area) == *rect)
}

#[test]
fn a_rail_taller_than_its_column_scrolls_behind_a_bar_of_its_own() {
    let theme = Theme::dark();
    let sb = long_rail_screen();
    let layout = Switchboard::compute_layout(bounds(), Scale::ONE, &theme);
    let rail = rail_frame(&sb);
    let bar = rail
        .bar
        .expect("a rail taller than its column carries a bar");

    assert_eq!(
        rail.viewport.width + bar.width,
        layout.rail.width,
        "the bar is carved from the rail's own column"
    );
    assert_eq!(bar.left(), rail.viewport.right());
    assert_eq!(
        layout.content.left(),
        layout.rail.right(),
        "so the pane beside the rail keeps its width"
    );
    let range = sb.rail_model(&rail, Scale::ONE, &theme).range();
    assert!(range.is_scrollable());
    assert_eq!(
        range.content_extent(),
        u64::from(sb.rail.measured_height(Scale::ONE, &theme)),
        "the rail's range is the strip's own natural height"
    );

    let tall =
        Switchboard::compute_layout(Rect::new(0, 0, bounds().width, 4000), Scale::ONE, &theme);
    let fits = sb.rail_frame(tall.rail, Scale::ONE, &theme);
    assert_eq!(fits.bar, None, "a rail its column seats carries no bar");
    assert_eq!(fits.viewport, tall.rail);
}

#[test]
fn the_wheel_over_the_rail_scrolls_the_rail_and_reports_it() {
    let mut sb = long_rail_screen();
    let (x, y) = centre(rail_frame(&sb).viewport);
    feed(&mut sb, &moved(x, y), &mut damage::sink());
    let before = shot(&mut sb);

    let reported = reported_by(&mut sb, &turn(1));
    let after = shot(&mut sb);

    assert_eq!(sb.rail_scroll.model().offset(), DETENT_PX);
    assert_eq!(
        sb.scroll_offset(),
        0,
        "the pane beside the rail did not move"
    );
    assert_eq!(
        unreported_change(&before, &after, bounds(), &reported),
        None,
        "the rail and its bar moved and were reported"
    );
    assert!(
        all_within(&reported, rail_rect()),
        "nothing beside the rail repaints: {:?}",
        reported.rects()
    );
}

#[test]
fn the_wheel_over_the_pane_leaves_the_rail_where_it_was() {
    let theme = Theme::dark();
    let mut sb = long_rail_screen();
    let layout = Switchboard::compute_layout(bounds(), Scale::ONE, &theme);
    let (x, y) = centre(layout.content);
    feed(&mut sb, &moved(x, y), &mut damage::sink());

    let _ = reported_by(&mut sb, &turn(1));

    assert_eq!(sb.rail_scroll.model().offset(), 0);
    assert_eq!(sb.scroll_offset(), DETENT_PX);
}

/// A wheel turn over the rail slides its entries under a pointer that did not
/// move, and the entry lit is the one now under it.
#[test]
fn a_wheel_turn_under_a_still_pointer_moves_the_rails_hover_to_the_entry_now_under_it() {
    let theme = Theme::dark();
    let at = centre(rail_frame(&long_rail_screen()).viewport);
    let away = centre(Switchboard::compute_layout(bounds(), Scale::ONE, &theme).content);
    let point = |sb: &mut Switchboard, (x, y): (i32, i32)| {
        feed(sb, &moved(x, y), &mut damage::sink());
    };
    let mut sb = long_rail_screen();
    point(&mut sb, at);
    let stale = sb.rail.clone();

    let _ = reported_by(&mut sb, &turn(1));

    assert_eq!(sb.rail_scroll.model().offset(), DETENT_PX);
    let mut fresh = long_rail_screen();
    point(&mut fresh, at);
    let _ = reported_by(&mut fresh, &turn(1));
    point(&mut fresh, away);
    point(&mut fresh, at);
    assert_ne!(fresh.rail, stale, "the premise: the lit entry moves");
    assert_eq!(
        sb.rail, fresh.rail,
        "the hover stayed on the entry carried away"
    );
}

#[test]
fn a_press_on_a_scrolled_rail_selects_the_entry_it_shows() {
    let theme = Theme::dark();
    let mut sb = long_rail_screen();
    let rail = rail_frame(&sb);
    let (x, y) = centre(rail.viewport);
    feed(&mut sb, &moved(x, y), &mut damage::sink());
    let _ = reported_by(&mut sb, &turn(3));
    assert_eq!(sb.rail_scroll.model().offset(), 3 * DETENT_PX);

    // The first disk the scrolled rail shows whole.
    let (index, shown) = (0..sb.rail.len())
        .filter(|&index| {
            matches!(
                sb.rail_subjects[index],
                RailSubject::Device(DeviceId::Storage(_))
            )
        })
        .find_map(|index| {
            let shown = shown_entry(&sb, index)?;
            (shown.height == laid_entry(&sb, index).height).then_some((index, shown))
        })
        .expect("a disk shows whole on the scrolled rail");
    let point = tairix_geometry::Point::new(centre(shown).0, centre(shown).1);
    assert_ne!(
        sb.rail.tab_at(rail.strip, Scale::ONE, &theme, point),
        Some(index),
        "unscrolled, the same point names another entry, so this proves the mapping"
    );

    let _ = click(&mut sb, bounds(), Scale::ONE, &theme, point.x, point.y);

    let RailSubject::Device(device) = sb.rail_subjects[index] else {
        unreachable!("filtered to devices above")
    };
    assert_eq!(sb.section(), Section::Resources);
    assert_eq!(sb.resources.selected, Some(device));
}

#[test]
fn walking_the_rail_cursor_to_its_end_scrolls_the_last_entry_into_view() {
    let mut sb = long_rail_screen();
    let last = sb.rail.len() - 1;
    assert_eq!(
        shown_entry(&sb, last),
        None,
        "the last entry starts out of view"
    );
    // The rail is a focus region of its own, reached from the content past
    // the scrollbar.
    for _ in 0..2 {
        let _ = key(&mut sb, Key::Named(NamedKey::Tab));
    }

    let _ = key(&mut sb, Key::Named(NamedKey::End));
    let _ = shot(&mut sb);

    assert_eq!(sb.section(), Section::Recovery);
    assert_eq!(
        shown_entry(&sb, last).map(|rect| rect.height),
        Some(laid_entry(&sb, last).height),
        "the entry the cursor walked onto shows whole"
    );

    // Walking back to the top brings the rail's head, heading and all, back.
    let _ = key(&mut sb, Key::Named(NamedKey::Home));
    let _ = shot(&mut sb);
    assert_eq!(drawn_offset(&sb), 0);
}

#[test]
fn opening_on_recovery_shows_its_rail_entry() {
    // The host chooses the section with no frame to reveal against, so the
    // rail scrolls at its first layout.
    let mut sb = Switchboard::new(&long_rail_model());
    let _ = sb.select_section(Section::Recovery);
    let _ = shot(&mut sb);

    let last = sb.rail.len() - 1;
    assert_eq!(sb.rail.selected(), Some(last));
    assert_eq!(
        shown_entry(&sb, last).map(|rect| rect.height),
        Some(laid_entry(&sb, last).height)
    );
}

#[test]
fn a_thumb_drag_on_the_rails_bar_scrolls_the_rail() {
    let theme = Theme::dark();
    let mut sb = long_rail_screen();
    let bar = rail_frame(&sb).bar.expect("the long rail carries a bar");
    let thumb = sb
        .rail_scroll
        .part_rect(tairix_controls::ScrollPart::Thumb, bar, Scale::ONE, &theme)
        .expect("a draggable thumb");
    let (x, y) = centre(thumb);
    let before = shot(&mut sb);
    let mut reported = damage::sink();
    for event in [moved(x, y), PRESS, moved(x, y + 10)] {
        feed(&mut sb, &event, &mut reported);
    }
    let after = shot(&mut sb);

    assert!(sb.rail_scroll.model().offset() > 0);
    assert_eq!(sb.scroll_offset(), 0);
    assert_eq!(
        unreported_change(&before, &after, bounds(), &reported),
        None
    );
    feed(&mut sb, &RELEASE, &mut damage::sink());
}

#[test]
fn a_rail_that_no_longer_needs_its_bar_drops_the_drag_it_held() {
    // A bar that is no longer drawn is fed no release, so a drag it kept
    // would carry on scrolling the rail whenever the bar came back.
    let theme = Theme::dark();
    let mut sb = long_rail_screen();
    let bar = rail_frame(&sb).bar.expect("the long rail carries a bar");
    let thumb = sb
        .rail_scroll
        .part_rect(tairix_controls::ScrollPart::Thumb, bar, Scale::ONE, &theme)
        .expect("a draggable thumb");
    let (x, y) = centre(thumb);
    for event in [moved(x, y), PRESS] {
        feed(&mut sb, &event, &mut damage::sink());
    }
    assert!(sb.rail_scroll.is_pressing());

    let mut short = model();
    short.resources.devices.truncate(1);
    let _ = refresh(&mut sb, &short);
    let _ = shot(&mut sb);
    assert_eq!(rail_frame(&sb).bar, None, "the short rail fits its column");
    assert!(!sb.rail_scroll.is_pressing());

    let _ = refresh(&mut sb, &long_rail_model());
    let _ = shot(&mut sb);
    feed(&mut sb, &moved(x, y + 40), &mut damage::sink());
    assert_eq!(
        sb.rail_scroll.model().offset(),
        0,
        "no stale drag moved the rail"
    );
}
