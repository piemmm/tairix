//! Unit tests for the Switchboard window's shared per-section skeleton
//! (spec §17, §20).
//!
//! These prove the composition is assembled from the shared controls and
//! behaves correctly: the window manager decorates server-side so the app's
//! own content fills the client from the top edge, the navigation rail
//! switches sections (by pointer and keyboard) and marks the one on show, a
//! host can
//! open the panel on any section and lands in exactly the state the keyboard
//! would have reached, a refreshed model re-derives the controls while leaving
//! the user's section, scroll offset, and focus alone and never lets a stale
//! gesture reach a replaced row, the mouse wheel and keyboard scroll the
//! active section, denied actions render distinctly from disabled ones, and
//! the layout scales.

use tairix_geometry::{to_i32, Point, Rect, Scale};
use tairix_icon::NoArtwork;
use tairix_input::{InputEvent, Key, NamedKey};
use tairix_raster::{Color, Surface};
use tairix_theme::{SurfaceGround, Theme, ThemeRegistry};

use tairix_controls::testkit::high_contrast;
use tairix_controls::{
    ActivityState, ControlDisposition, PressureState, RecoveryState, ScrollPart,
};

use crate::panel::{MIN_WIN_HEIGHT, MIN_WIN_WIDTH, WINDOW_GROUND};

use super::test_support::{
    bounds, centre, click, focus_task_row, font, key, model, moved, not_slid_up, pointer, refresh,
    report, resource_report, secondary_click, select_task_row, shot, task_id, task_row_point, turn,
    unreported_change, DETENT_PX, PRESS, RELEASE,
};
use super::{
    resolve_section_frame, ActionVerdict, DeviceAction, Reading, RecoveryControl, ResourceControl,
    Section, SectionAnatomy, Switchboard, SwitchboardAction, SwitchboardModel, TaskSummary,
};

/// A point over the first row of the active section's scrollable list.
///
/// Taken from the section's own list metrics rather than from the corner of
/// the content rect: the Tasks table pins its column headings above its
/// rows, and a probe at that corner would sample a heading instead.
fn content_point(sb: &Switchboard, theme: &Theme) -> (i32, i32) {
    let item = list_info(sb, theme).item_rect(0);
    (item.left() + 4, item.top() + to_i32(item.height / 2))
}

fn feed(sb: &mut Switchboard, theme: &Theme, event: &InputEvent) -> Option<SwitchboardAction> {
    pointer(sb, bounds(), Scale::ONE, theme, event)
}

/// Which Tasks row the content cursor is on, for a test that indexes the
/// section's rows directly.
fn focused_task_row(sb: &Switchboard) -> usize {
    sb.active()
        .focus_row(sb.active().content_focus())
        .expect("the cursor is on a task row")
}

/// A point the composition has no control at: outside the window entirely, so
/// a sample there crosses nothing whatever the active section holds.
fn inert_point() -> (i32, i32) {
    (bounds().right() + 10, bounds().bottom() + 10)
}

/// The active section's list metrics at the test bounds.
/// A screen showing `model` on the Tasks section.
///
/// The surface opens on Resources, whose pane is a flow of instrument items
/// rather than a list of rows. A test about rows — hover, focus rings, the
/// pixels a press moves — says which list it means rather than relying on
/// whichever subject leads the rail.
fn on_tasks(model: &SwitchboardModel) -> Switchboard {
    let mut sb = Switchboard::new(model);
    sb.select_section(Section::Tasks);
    sb
}

fn list_info(sb: &Switchboard, theme: &Theme) -> super::ListInfo {
    let layout = Switchboard::compute_layout(bounds(), Scale::ONE, theme);
    sb.list_info(&layout, Scale::ONE, theme)
}

/// What this machine is doing is the question a monitor is opened to answer,
/// so the surface leads with it rather than with the task list.
#[test]
fn new_starts_on_resources_at_offset_zero() {
    let sb = Switchboard::new(&model());
    assert_eq!(sb.section(), Section::Resources);
    assert_eq!(sb.scroll_offset(), 0);
    assert_eq!(
        sb.resources.selected,
        model().resources.devices.first().map(|device| device.id),
        "the rail opens on the processor, the first subject of the devices"
    );
}

#[test]
fn render_paints_content() {
    let theme = Theme::dark();
    let mut sb = on_tasks(&model());
    let mut surface = Surface::new(600, 400).expect("surface");
    sb.render(
        &mut surface,
        bounds(),
        Scale::ONE,
        &theme,
        font(),
        &mut NoArtwork,
    );
    assert!(surface.pixels().iter().any(|p| p.a > 0));
}

#[test]
fn scroll_track_sits_beside_the_content_inside_bounds() {
    let theme = Theme::dark();
    let b = bounds();
    let layout = Switchboard::compute_layout(b, Scale::ONE, &theme);
    // The content area stops where the scrollbar gutter begins, and the two
    // together stay inside the bounds the compositor carved out.
    assert_eq!(layout.content.right(), layout.scroll.left());
    assert!(layout.scroll.right() <= b.right());
    assert!(layout.content.bottom() <= b.bottom());
    assert!(layout.scroll.bottom() <= b.bottom());
}

#[test]
fn the_client_content_begins_at_the_top_of_bounds() {
    let theme = Theme::dark();
    let b = bounds();
    let layout = Switchboard::compute_layout(b, Scale::ONE, &theme);
    // The window manager decorates server-side, so the app draws no title bar
    // of its own: its first region, the navigation rail, sits at the very top
    // left of the bounds it was handed. A re-introduced private title bar
    // would inset the client and push it down, failing this.
    assert_eq!(layout.rail.top(), b.top());
    assert_eq!(layout.rail.left(), b.left());
    assert_eq!(layout.content.top(), b.top());
    // The rail claims the leading edge and the content sits beside it, so no
    // region overlaps another.
    assert_eq!(layout.content.left(), layout.rail.right());
    assert!(layout.scroll.left() >= layout.content.right());
}

#[test]
fn wheel_scrolls_the_active_section() {
    let theme = Theme::dark();
    let mut sb = on_tasks(&model());
    let b = bounds();
    let action = pointer(&mut sb, b, Scale::ONE, &theme, &turn(1));
    match action {
        Some(SwitchboardAction::Scrolled { offset }) => assert_eq!(offset, DETENT_PX),
        other => panic!("expected a scroll, got {other:?}"),
    }
    assert_eq!(
        sb.scroll_offset(),
        DETENT_PX,
        "a detent is one wheel step of pixels, not a row"
    );
}

#[test]
fn keyboard_scrolls_the_focused_scrollbar() {
    let theme = Theme::dark();
    let mut sb = on_tasks(&model());
    let mut surface = Surface::new(600, 400).expect("surface");
    // Render once so the scroll model matches the layout.
    sb.render(
        &mut surface,
        bounds(),
        Scale::ONE,
        &theme,
        font(),
        &mut NoArtwork,
    );
    // Cycle focus Content -> Scrollbar (one Tab).
    assert_eq!(key(&mut sb, Key::Named(NamedKey::Tab)), None);
    let action = key(&mut sb, Key::Named(NamedKey::Down));
    match action {
        Some(SwitchboardAction::Scrolled { offset }) => assert!(offset >= 1),
        other => panic!("expected a keyboard scroll, got {other:?}"),
    }
}

#[test]
fn no_part_of_the_client_is_left_transparent() {
    let b = bounds();
    for theme in [Theme::dark(), Theme::light(), high_contrast()] {
        let mut sb = on_tasks(&model());
        let mut surface = Surface::new(b.width, b.height).expect("surface");
        sb.render(&mut surface, b, Scale::ONE, &theme, font(), &mut NoArtwork);

        // The window manager decorates the window; its content pixels are the
        // client's own. Any pixel left clear shows whatever the shared frame
        // region held before, which reads as a transparent window.
        let clear = (0..b.width)
            .flat_map(|x| (0..b.height).map(move |y| (x, y)))
            .find(|&(x, y)| surface.get(x, y).is_some_and(|p| p.a == 0));
        assert_eq!(clear, None, "pixel {clear:?} was left transparent");
    }
}

#[test]
fn the_client_is_laid_over_the_theme_surface_tint() {
    let theme = Theme::dark();
    let b = bounds();
    let mut sb = on_tasks(&model());
    let mut surface = Surface::new(b.width, b.height).expect("surface");
    sb.render(&mut surface, b, Scale::ONE, &theme, font(), &mut NoArtwork);
    assert!(
        surface
            .pixels()
            .contains(&Color::from(theme.palette().surface).premultiply()),
        "the base surface tint must show wherever no control covers it"
    );
}

/// The window is cut from the icon bar's glass, frosted deeper: its bare
/// ground lets the desktop through at the bar's weight over a window's blur,
/// while everything laid on it — a rail entry, a block — stays solid.
#[test]
fn the_window_ground_is_the_bars_glass_and_what_is_on_it_is_solid() {
    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active_on(WINDOW_GROUND);
    assert_eq!(
        u32::from(theme.backdrop_blur()),
        theme.metrics().window_backdrop_blur,
        "the window asks for a blur other than a frosted window's"
    );
    assert!(
        theme.backdrop_blur() > themes.active_on(SurfaceGround::Floating).backdrop_blur(),
        "the window's glass is frosted no deeper than the bar's"
    );
    let p = *theme.palette();
    let b = bounds();
    let mut sb = Switchboard::new(&model());
    let mut surface = Surface::new(b.width, b.height).expect("surface");
    sb.render(&mut surface, b, Scale::ONE, theme, font(), &mut NoArtwork);

    let layout = Switchboard::compute_layout(b, Scale::ONE, theme);
    let rail = sb
        .section_frame(&layout, Scale::ONE, theme)
        .rail
        .expect("the pane seats its action rail");
    let gap = Scale::ONE.scale_length(theme.metrics().control_gap);
    let margin = super::block::plate_margin(Scale::ONE, theme);
    let beside = (
        u32::try_from(rail.left()).expect("on the surface") - gap / 2,
        u32::try_from(rail.top()).expect("on the surface") + rail.height / 2,
    );
    let inside = (
        u32::try_from(rail.left()).expect("on the surface") + rail.width / 2,
        u32::try_from(rail.bottom()).expect("on the surface") - margin - 4,
    );
    assert_eq!(
        surface.get(beside.0, beside.1),
        Some(Color::from(p.surface.with_alpha(p.chrome_alpha)).premultiply()),
        "the ground beside the rail is not the bar's glass"
    );
    assert_eq!(
        surface.get(inside.0, inside.1),
        Some(Color::from(p.surface_raised).premultiply()),
        "the rail's block let the desktop through"
    );

    let strip = sb.rail_frame(layout.rail, Scale::ONE, theme).strip;
    let entry = sb
        .rail
        .tab_area(0, strip, Scale::ONE, theme)
        .expect("the rail lists the task list");
    let (left, top) = (
        u32::try_from(entry.left()).expect("on the surface"),
        u32::try_from(entry.top()).expect("on the surface"),
    );
    for y in top..top + entry.height {
        for x in left..left + entry.width {
            assert_eq!(
                surface.get(x, y).map(|pixel| pixel.a),
                Some(u8::MAX),
                "({x}, {y}) of a rail entry lets the desktop through"
            );
        }
    }
}

#[test]
fn denied_action_renders_distinct_from_disabled() {
    // One command refused for want of authority beside one with no endpoint
    // behind it, so the two treatments can be told apart.
    let mut m = model();
    if let Some(cpu) = m.resources.devices.first_mut() {
        cpu.actions = alloc::vec![
            DeviceAction {
                verdict: ActionVerdict::DeniedByAuthority,
                ..DeviceAction::ready(ResourceControl::Relieve, "Reclaim now")
            },
            DeviceAction::absent(ResourceControl::CopyReadings, "Copy readings"),
        ];
    }
    let sb = Switchboard::new(&m);
    assert_eq!(sb.section(), Section::Resources);

    assert_eq!(
        sb.resources.actions.items()[0].state().disposition(),
        ControlDisposition::DeniedByAuthority,
        "a command the caller may not use wears the Authority Mark"
    );
    assert_eq!(
        sb.resources.actions.items()[1].state().disposition(),
        ControlDisposition::DisabledByState,
        "one with nothing behind it is plainly disabled instead"
    );
}

#[test]
fn layout_scales_with_the_ui_scale() {
    let theme = Theme::dark();
    let one = Switchboard::compute_layout(bounds(), Scale::ONE, &theme);
    let two =
        Switchboard::compute_layout(bounds(), Scale::from_percent(200).expect("scale"), &theme);
    assert!(two.rail.width > one.rail.width);
}

#[test]
fn light_theme_renders() {
    let theme = Theme::light();
    let mut sb = on_tasks(&model());
    let mut surface = Surface::new(600, 400).expect("surface");
    sb.render(
        &mut surface,
        bounds(),
        Scale::ONE,
        &theme,
        font(),
        &mut NoArtwork,
    );
    assert!(surface.pixels().iter().any(|p| p.a > 0));
}

#[test]
fn high_contrast_theme_renders() {
    let theme = high_contrast();
    let mut sb = on_tasks(&model());
    let mut surface = Surface::new(600, 400).expect("surface");
    sb.render(
        &mut surface,
        bounds(),
        Scale::ONE,
        &theme,
        font(),
        &mut NoArtwork,
    );
    assert!(surface.pixels().iter().any(|p| p.a > 0));
}

#[test]
fn window_too_short_for_the_anatomy_still_renders_in_bounds() {
    let theme = Theme::dark();
    let mut sb = on_tasks(&model());
    // Shorter than the location band would ordinarily need.
    let b = Rect::new(0, 0, 600, 24);
    let mut surface = Surface::new(b.width, b.height).expect("surface");
    // Must not panic: every region clips to the bounds instead.
    sb.render(&mut surface, b, Scale::ONE, &theme, font(), &mut NoArtwork);
    let layout = Switchboard::compute_layout(b, Scale::ONE, &theme);
    assert!(layout.rail.bottom() <= b.bottom());
    assert!(layout.content.bottom() <= b.bottom());
    assert!(layout.scroll.bottom() <= b.bottom());
}

#[test]
fn the_minimum_window_size_seats_every_declared_anatomy() {
    let theme = Theme::dark();
    let mut sb = on_tasks(&model());
    // The panel clamps a resize up so a section is never drawn into a box that
    // starves what it must keep. What it must keep is its primary column's
    // declared floor: the optional columns beside it are shed in the frame's
    // drop order, which is the designed outcome on a narrow window rather than
    // a lost region — but the sidebar and the action rail are last in that
    // order and must still be there. A section seated in this content area is
    // seated in the real client, and one that later declares a sidebar or rail
    // wider than this floor can hold fails here.
    let b = Rect::new(0, 0, MIN_WIN_WIDTH, MIN_WIN_HEIGHT);
    let layout = Switchboard::compute_layout(b, Scale::ONE, &theme);
    for section in Section::ALL {
        sb.select_section(section);
        let anatomy = sb.active().anatomy();
        let frame = resolve_section_frame(layout.content, anatomy, Scale::ONE, &theme);
        assert!(
            frame.primary.width >= SectionAnatomy::PRIMARY_FLOOR,
            "{}'s primary column falls below its declared floor",
            section.title()
        );
        assert!(
            anatomy.sidebar_width == 0 || frame.sidebar.is_some(),
            "{} loses its sidebar at the minimum window",
            section.title()
        );
        assert!(
            anatomy.rail_width == 0 || frame.rail.is_some(),
            "{} loses its action rail at the minimum window",
            section.title()
        );
        assert!(
            layout.content.height >= anatomy.minimum_height(Scale::ONE),
            "{} asks for more height than the minimum window seats",
            section.title()
        );
    }
}

#[test]
fn select_section_shows_that_section_and_names_it_in_the_trail() {
    let theme = Theme::dark();
    let b = bounds();
    let mut painted = alloc::vec::Vec::new();
    for section in Section::ALL {
        let mut sb = on_tasks(&model());
        let changed = sb.select_section(section);
        if section == Section::Tasks {
            assert_eq!(changed, None, "Tasks is what a fresh Switchboard shows");
        } else {
            assert_eq!(changed, Some(SwitchboardAction::SectionChanged { section }));
        }
        assert_eq!(sb.section(), section);
        let mut surface = Surface::new(b.width, b.height).expect("surface");
        sb.render(&mut surface, b, Scale::ONE, &theme, font(), &mut NoArtwork);
        painted.push((section, surface.pixels().to_vec()));
    }
    for (i, (section, pixels)) in painted.iter().enumerate() {
        for (other_section, other_pixels) in painted.iter().skip(i + 1) {
            assert_ne!(
                pixels, other_pixels,
                "{section:?} and {other_section:?} drew the same surface"
            );
        }
    }
}

#[test]
fn select_section_reranges_the_scroll_for_the_new_section() {
    let theme = Theme::dark();
    let mut sb = on_tasks(&model());
    let b = bounds();
    let mut surface = Surface::new(b.width, b.height).expect("surface");
    // Scroll deep into the long (50-item) Tasks list.
    pointer(&mut sb, b, Scale::ONE, &theme, &turn(5));
    let deep = sb.scroll_offset();
    assert!(deep > 0, "the long list must actually scroll");

    // The short (6-item) Recovery list opens at its own offset, never the
    // long list's, and the scrollbar is re-ranged to its content.
    assert_eq!(
        sb.select_section(Section::Recovery),
        Some(SwitchboardAction::SectionChanged {
            section: Section::Recovery
        })
    );
    sb.render(&mut surface, b, Scale::ONE, &theme, font(), &mut NoArtwork);
    let range = sb.scroll.model().range();
    let card = u64::from(Switchboard::card_item_height(Scale::ONE, &theme));
    assert_eq!(range.content_extent(), 6 * card, "six cards, in pixels");
    assert_eq!(sb.scroll_offset(), 0);
    assert!(range.offset() <= range.max_offset());

    // Going back restores the long list's own, still-valid offset.
    assert_eq!(
        sb.select_section(Section::Tasks),
        Some(SwitchboardAction::SectionChanged {
            section: Section::Tasks
        })
    );
    sb.render(&mut surface, b, Scale::ONE, &theme, font(), &mut NoArtwork);
    let range = sb.scroll.model().range();
    let row = u64::from(Switchboard::row_item_height(Scale::ONE, &theme));
    assert_eq!(range.content_extent(), 50 * row, "fifty rows, in pixels");
    assert_eq!(sb.scroll_offset(), deep);
    assert!(range.offset() <= range.max_offset());
}

#[test]
fn selecting_the_shown_section_changes_nothing() {
    let theme = Theme::dark();
    let mut sb = on_tasks(&model());
    let b = bounds();
    pointer(&mut sb, b, Scale::ONE, &theme, &turn(2));
    // Move the keyboard off the first item too, so a stray reset would show.
    assert_eq!(key(&mut sb, Key::Named(NamedKey::Down)), None);
    let before = sb.clone();
    let offset = sb.scroll_offset();

    assert_eq!(sb.select_section(Section::Tasks), None);

    assert_eq!(
        sb, before,
        "re-selecting the shown section must change nothing"
    );
    assert_eq!(sb.scroll_offset(), offset);
}

#[test]
fn pointer_after_selection_reaches_the_new_sections_content() {
    let theme = Theme::dark();
    let mut sb = on_tasks(&model());
    let b = bounds();
    sb.select_section(Section::Recovery);
    let layout = Switchboard::compute_layout(b, Scale::ONE, &theme);
    // Recovery's commands moved from the row into an anchored rail, so the
    // aim moved with them: the rail sits in a column the Tasks section does
    // not seat at all, so only the new section can answer.
    let frame = resolve_section_frame(layout.content, sb.active().anatomy(), Scale::ONE, &theme);
    let rail = crate::view::recovery::RecoverySection::rail_content(&frame, Scale::ONE, &theme)
        .expect("the default window seats the recovery rail");
    let (x, y) = centre(
        sb.recovery
            .rail
            .item_rect(rail, 0, Scale::ONE, &theme)
            .expect("the rail seats its restart command"),
    );
    let actions = click(&mut sb, b, Scale::ONE, &theme, x, y);
    assert!(actions.contains(&SwitchboardAction::Recovery {
        index: 0,
        control: RecoveryControl::Restart
    }));
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, SwitchboardAction::Task { .. })),
        "the superseded section must not still be receiving input"
    );
}

/// Walking the rail onto a subject and asking for its section directly are
/// the same transition, so they must leave the same state: the rail is the
/// only route a reader has, and a host that asks for a section must not land
/// somewhere the rail could not.
#[test]
fn direct_selection_and_the_keyboard_path_agree() {
    let mut by_key = on_tasks(&model());
    let mut direct = on_tasks(&model());
    // Put both on the rail (Content -> Scrollbar -> Rail) so the only
    // difference is how the subject is chosen.
    for _ in 0..2 {
        assert_eq!(key(&mut by_key, Key::Named(NamedKey::Tab)), None);
        assert_eq!(key(&mut direct, Key::Named(NamedKey::Tab)), None);
    }
    // Recovery is the last subject the rail lists, so End walks straight to
    // it — and on a rail the cursor *is* the choice.
    let by_key_action = key(&mut by_key, Key::Named(NamedKey::End));
    let direct_action = direct.select_section(Section::Recovery);

    assert_eq!(
        by_key_action,
        Some(SwitchboardAction::SectionChanged {
            section: Section::Recovery
        })
    );
    assert_eq!(by_key_action, direct_action);
    assert_eq!(by_key, direct, "one transition must leave one state");
}

/// A model of `tasks` tasks and `devices` resource devices and nothing else,
/// for a refresh that shortens, empties, or re-populates a section.
///
/// Its tasks' identities are distinct from the base [`model`]'s, so a test can
/// tell a refreshed row from the one it replaced.
fn refreshed_model(tasks: usize, devices: usize) -> SwitchboardModel {
    let mut m = SwitchboardModel::new("Switchboard");
    for i in 0..tasks {
        m.tasks.push(TaskSummary {
            proc_id: task_id(100 + i),
            name: alloc::format!("fresh {i}"),
            memory_bytes: Some(u64::try_from(i).unwrap_or(0) * 1024 * 1024),
            pressure: PressureState::None,
            activity: ActivityState::Idle,
            recovery: RecoveryState::None,
            ..TaskSummary::default()
        });
    }
    let report = resource_report();
    m.resources.devices = report.devices.into_iter().take(devices).collect();
    m
}

#[test]
fn set_model_clamps_an_offset_past_the_end_of_a_shorter_list() {
    let theme = Theme::dark();
    let b = bounds();
    let mut sb = on_tasks(&model());
    let mut surface = Surface::new(b.width, b.height).expect("surface");
    sb.render(&mut surface, b, Scale::ONE, &theme, font(), &mut NoArtwork);
    pointer(&mut sb, b, Scale::ONE, &theme, &turn(40));
    let row = u64::from(Switchboard::row_item_height(Scale::ONE, &theme));
    assert!(
        sb.scroll_offset() > 5 * row,
        "the 50-item list must scroll well past the height of a 5-item one"
    );

    // Five tasks have nowhere near that far to scroll: the refresh re-ranges
    // there and then, rather than leaving a dangling offset for the next frame.
    let _ = refresh(&mut sb, &refreshed_model(5, 3));

    let range = sb.scroll.model().range();
    assert_eq!(range.content_extent(), 5 * row);
    assert!(range.offset() <= range.max_offset());
    assert!(
        sb.scroll_offset() < 5 * row,
        "the offset must land inside the new list"
    );

    sb.render(&mut surface, b, Scale::ONE, &theme, font(), &mut NoArtwork);
    let range = sb.scroll.model().range();
    assert_eq!(range.content_extent(), 5 * row);
    assert!(range.offset() <= range.max_offset());
}

#[test]
fn set_model_to_an_empty_model_stays_valid_and_renderable() {
    let theme = Theme::dark();
    let b = bounds();
    let mut sb = on_tasks(&model());
    let mut surface = Surface::new(b.width, b.height).expect("surface");
    sb.render(&mut surface, b, Scale::ONE, &theme, font(), &mut NoArtwork);
    for _ in 0..4 {
        assert_eq!(key(&mut sb, Key::Named(NamedKey::Down)), None);
    }
    pointer(&mut sb, b, Scale::ONE, &theme, &turn(5));
    assert!(sb.scroll_offset() > 0);

    let _ = refresh(&mut sb, &SwitchboardModel::new("Switchboard"));

    assert_eq!(sb.scroll_offset(), 0, "an empty list has nowhere to scroll");
    assert_eq!(sb.active().content_focus(), 0);
    assert_eq!(
        key(&mut sb, Key::Named(NamedKey::Enter)),
        None,
        "an emptied section has nothing to activate"
    );
    sb.render(&mut surface, b, Scale::ONE, &theme, font(), &mut NoArtwork);
    let layout = Switchboard::compute_layout(b, Scale::ONE, &theme);
    assert!(layout.content.bottom() <= b.bottom());
}

#[test]
fn pointer_after_set_model_addresses_the_new_rows() {
    let theme = Theme::dark();
    let b = bounds();
    let mut sb = on_tasks(&model());
    let mut surface = Surface::new(b.width, b.height).expect("surface");
    sb.render(&mut surface, b, Scale::ONE, &theme, font(), &mut NoArtwork);

    // Three tasks replace fifty.
    let _ = refresh(&mut sb, &refreshed_model(3, 3));
    sb.render(&mut surface, b, Scale::ONE, &theme, font(), &mut NoArtwork);

    // Row 2's menu must name the task the refresh put there.
    let point = task_row_point(&sb, b, Scale::ONE, &theme, 2);
    let asked = secondary_click(&mut sb, b, Scale::ONE, &theme, point);
    assert!(
        matches!(
            asked.as_slice(),
            [SwitchboardAction::TaskMenu { proc_id, .. }] if *proc_id == task_id(102)
        ),
        "the menu names the task now at that row: {asked:?}"
    );
    select_task_row(&mut sb, b, Scale::ONE, &theme, 0);
    assert_eq!(sb.tasks.selected, Some(task_id(100)));

    // Row three is gone; a press one row-height below the last row it does
    // have must select nothing at all.
    let layout = Switchboard::compute_layout(b, Scale::ONE, &theme);
    let info = sb.list_info(&layout, Scale::ONE, &theme);
    let last = info.item_rect(2);
    let (x, y) = (
        centre(last).0,
        centre(last).1 + to_i32(last.height).saturating_add(4),
    );
    let before = sb.tasks.selected;
    assert!(click(&mut sb, b, Scale::ONE, &theme, x, y).is_empty());
    assert_eq!(
        sb.tasks.selected, before,
        "a row the refresh removed must never be selectable"
    );
}

#[test]
fn set_model_cannot_complete_a_press_begun_on_the_row_it_replaced() {
    let theme = Theme::dark();
    let b = bounds();
    let mut sb = on_tasks(&model());
    let mut surface = Surface::new(b.width, b.height).expect("surface");
    sb.render(&mut surface, b, Scale::ONE, &theme, font(), &mut NoArtwork);
    // Move the selection off row 0 first, so a press completing there would
    // be visible as a change rather than hidden by the resting selection.
    select_task_row(&mut sb, b, Scale::ONE, &theme, 3);
    let held = sb.tasks.selected.expect("row 3 selected");
    let (x, y) = task_row_point(&sb, b, Scale::ONE, &theme, 0);

    // Arm row 0, refresh under the held pointer, let go. The replacement row
    // sits at the same place, so only the dropped arm can keep the release
    // from selecting it.
    assert_eq!(pointer(&mut sb, b, Scale::ONE, &theme, &moved(x, y)), None);
    assert_eq!(pointer(&mut sb, b, Scale::ONE, &theme, &PRESS), None);
    let _ = refresh(&mut sb, &model());

    assert_eq!(pointer(&mut sb, b, Scale::ONE, &theme, &RELEASE), None);
    assert_eq!(
        sb.tasks.selected,
        Some(held),
        "a press must not complete against the row that replaced its target"
    );

    select_task_row(&mut sb, b, Scale::ONE, &theme, 0);
    assert_eq!(
        sb.tasks.selected,
        Some(task_id(0)),
        "a fresh gesture on the new row must still work"
    );
}

#[test]
fn new_then_set_model_draws_what_building_with_that_model_draws() {
    let theme = Theme::dark();
    let b = bounds();
    // Neither has been interacted with, so there is no preserved state to
    // account for: any difference would be a second derivation. Choosing a
    // section would be an interaction, so both stay where the surface opens.
    // A paint settles nothing, so each is laid out against the window by the
    // same refresh round.
    let mut refreshed = Switchboard::new(&model());
    let _ = refresh(&mut refreshed, &refreshed_model(4, 2));
    let mut built = Switchboard::new(&refreshed_model(4, 2));
    let _ = refresh(&mut built, &refreshed_model(4, 2));

    let mut refreshed_surface = Surface::new(b.width, b.height).expect("surface");
    let mut built_surface = Surface::new(b.width, b.height).expect("surface");
    refreshed.render(
        &mut refreshed_surface,
        b,
        Scale::ONE,
        &theme,
        font(),
        &mut NoArtwork,
    );
    built.render(
        &mut built_surface,
        b,
        Scale::ONE,
        &theme,
        font(),
        &mut NoArtwork,
    );

    assert_eq!(
        refreshed_surface.pixels(),
        built_surface.pixels(),
        "one derivation must draw one surface"
    );
    assert_eq!(refreshed, built, "one derivation must leave one state");
}

#[test]
fn action_focus_clamps_and_resets_with_the_row_focus() {
    let mut sb = on_tasks(&model());
    // The Tasks table's rows carry no controls of their own, so the sideways
    // cursor has nowhere to go within a row; the column headings, whose
    // sortable columns it does traverse, are where the clamp is worth
    // proving.
    focus_task_row(&mut sb, 0);
    for sideways in [NamedKey::Left, NamedKey::Right] {
        assert_eq!(key(&mut sb, Key::Named(sideways)), None);
        assert_eq!(
            sb.active().row_action(),
            0,
            "a row has one action slot, so sideways moves stay put"
        );
    }

    // A fresh screen rests on the column headings, which the sideways cursor
    // does traverse.
    let mut sb = on_tasks(&model());
    let stops = sb.tasks.header.columns().len();
    assert_eq!(key(&mut sb, Key::Named(NamedKey::Left)), None);
    assert_eq!(
        sb.active().row_action(),
        0,
        "Left at the first heading stays put"
    );
    for _ in 0..stops + 2 {
        assert_eq!(key(&mut sb, Key::Named(NamedKey::Right)), None);
    }
    assert_eq!(
        sb.active().row_action(),
        stops - 1,
        "Right clamps at the last heading"
    );
    assert_eq!(key(&mut sb, Key::Named(NamedKey::Down)), None);
    assert_eq!(
        sb.active().row_action(),
        0,
        "moving the cursor resets the action focus"
    );
}

// --- Render-equivalence equality (the host's repaint gate) ----------------

/// Paint `sb` at the standard bounds and hand back the surface a host would
/// present.
fn painted(sb: &mut Switchboard, theme: &Theme) -> Surface {
    let b = bounds();
    let mut surface = Surface::new(b.width, b.height).expect("surface");
    sb.render(&mut surface, b, Scale::ONE, theme, font(), &mut NoArtwork);
    surface
}

/// A Switchboard whose layout has been settled by one render, so a following
/// pointer event resolves against the geometry the next render will use.
fn settled(theme: &Theme) -> Switchboard {
    let mut sb = on_tasks(&model());
    let _ = painted(&mut sb, theme);
    sb
}

#[test]
fn pointer_move_that_crosses_no_control_leaves_the_composition_equal() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let (x, y) = inert_point();
    feed(&mut sb, &theme, &moved(x, y));

    let before = sb.clone();
    feed(&mut sb, &theme, &moved(x + 5, y + 1));

    assert_ne!(
        *sb.pointer, *before.pointer,
        "the sample must genuinely land on a new coordinate, or this proves \
         nothing"
    );
    assert_eq!(
        sb, before,
        "a sample at a new coordinate that crosses no control draws the same \
         pixels, so it must not defeat a host's repaint gate"
    );
}

#[test]
fn pointer_position_alone_never_changes_the_pixels() {
    let theme = Theme::dark();
    let mut moved_pointer = settled(&theme);
    let mut resting = moved_pointer.clone();
    *moved_pointer.pointer = Some(Point::new(517, 313));

    assert_ne!(
        *moved_pointer.pointer, *resting.pointer,
        "the two must genuinely differ in the excluded field"
    );
    assert_eq!(
        moved_pointer, resting,
        "the raw pointer coordinate is excluded from equality"
    );
    let a = painted(&mut moved_pointer, &theme);
    let b = painted(&mut resting, &theme);
    assert_eq!(
        a.pixels(),
        b.pixels(),
        "that exclusion is only sound because no render path reads it"
    );
}

#[test]
fn pointer_move_onto_a_row_changes_the_composition() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let (bx, by) = inert_point();
    feed(&mut sb, &theme, &moved(bx, by));

    let before = sb.clone();
    let (x, y) = content_point(&sb, &theme);
    feed(&mut sb, &theme, &moved(x, y));

    assert_ne!(
        sb, before,
        "a hover highlight is visible, so it must force a repaint"
    );
}

#[test]
fn press_and_release_each_change_the_composition() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let (x, y) = content_point(&sb, &theme);
    feed(&mut sb, &theme, &moved(x, y));

    let hovered = sb.clone();
    feed(&mut sb, &theme, &PRESS);
    assert_ne!(sb, hovered, "a press is visible on the pressed row");

    let pressed = sb.clone();
    feed(&mut sb, &theme, &RELEASE);
    assert_ne!(sb, pressed, "the release drops the pressed treatment");
}

#[test]
fn focus_change_changes_the_composition() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let before = sb.clone();

    key(&mut sb, Key::Named(NamedKey::Tab));

    assert_ne!(sb, before, "the focus ring moves to another region");
}

#[test]
fn the_focused_row_holds_the_ring_and_only_that_row() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    focus_task_row(&mut sb, 0);

    let focused = focused_task_row(&sb);
    let entry = &sb.tasks.entries[focused];
    assert!(
        entry.row.state().focus.in_focus_field,
        "the focused row is a member of its own field"
    );
    assert!(
        entry.row.state().focus.focused,
        "a row carries no controls of its own, so it takes the ring itself"
    );

    let other = &sb.tasks.entries[focused + 1];
    assert!(
        !other.row.state().focus.in_focus_field && !other.row.state().focus.focused,
        "an unfocused row is no part of the field"
    );
}

#[test]
fn leaving_the_content_region_clears_the_focus_field() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    focus_task_row(&mut sb, 1);
    assert!(
        sb.tasks.entries[focused_task_row(&sb)]
            .row
            .state()
            .focus
            .in_focus_field
    );

    // Content -> Scrollbar: the content list no longer holds the keyboard.
    key(&mut sb, Key::Named(NamedKey::Tab));

    assert!(
        sb.tasks
            .entries
            .iter()
            .all(|t| !t.row.state().focus.in_focus_field && !t.row.state().focus.focused),
        "no row glows once focus has left the list"
    );
}

#[test]
fn the_focus_field_is_visible_in_the_pixels() {
    let theme = Theme::dark();
    let mut in_field = settled(&theme);
    key(&mut in_field, Key::Named(NamedKey::Down));
    let mut elsewhere = in_field.clone();
    // Move focus off the list entirely, leaving the same rows on screen.
    key(&mut elsewhere, Key::Named(NamedKey::Tab));

    let a = painted(&mut in_field, &theme);
    let b = painted(&mut elsewhere, &theme);
    assert_ne!(
        a.pixels(),
        b.pixels(),
        "a Focus Field the user cannot see is not a Focus Field"
    );
}

#[test]
fn scrolling_the_content_changes_the_composition() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let before = sb.clone();

    feed(&mut sb, &theme, &turn(1));

    assert_ne!(sb.scroll_offset(), before.scroll_offset());
    assert_ne!(sb, before, "different rows are on screen");
}

#[test]
fn model_refresh_changes_the_composition() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let before = sb.clone();

    let mut refreshed = model();
    refreshed.tasks[0].cpu_permille = Some(990);
    let _ = refresh(&mut sb, &refreshed);

    assert_ne!(sb, before, "a re-derived row shows the new reading");
}

// --- What a round reports, and what it must cover -------------------------

#[test]
fn a_hover_reports_only_the_row_it_entered() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let row = list_info(&sb, &theme).item_rect(0);
    let (x, y) = centre(row);

    let damage = report(&mut sb, &moved(x, y));

    assert_eq!(
        damage.rects(),
        &[row],
        "entering a row from nowhere marks that row and nothing else"
    );
}

#[test]
fn a_second_sample_inside_the_same_row_reports_nothing() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let row = list_info(&sb, &theme).item_rect(0);
    let (x, y) = centre(row);
    let _ = report(&mut sb, &moved(x, y));

    let damage = report(&mut sb, &moved(x + 1, y));

    assert!(
        damage.is_empty(),
        "a motion that crosses no boundary changes no pixel and must report none"
    );
}

#[test]
fn a_hover_that_leaves_one_row_for_the_next_reports_both() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let info = list_info(&sb, &theme);
    let (first, second) = (info.item_rect(0), info.item_rect(1));
    let (x, y) = centre(first);
    let _ = report(&mut sb, &moved(x, y));

    let (x, y) = centre(second);
    let damage = report(&mut sb, &moved(x, y));

    let mut want = tairix_controls::damage::sink();
    want.add(first);
    want.add(second);
    assert_eq!(
        damage.rects(),
        want.rects(),
        "the row left and the row entered are the two that changed"
    );
}

#[test]
fn every_pixel_a_walk_moves_lies_inside_what_it_reported() {
    let theme = Theme::dark();
    for section in Section::ALL {
        let mut sb = settled(&theme);
        sb.select_section(section);
        let _ = painted(&mut sb, &theme);

        let info = list_info(&sb, &theme);
        let (row0, row1) = (centre(info.item_rect(0)), centre(info.item_rect(1)));
        let layout = Switchboard::compute_layout(bounds(), Scale::ONE, &theme);
        let rail_entry = centre(Rect::new(
            layout.rail.left(),
            layout.rail.top(),
            layout.rail.width,
            super::Switchboard::row_item_height(Scale::ONE, &theme),
        ));
        let steps = [
            moved(row0.0, row0.1),
            PRESS,
            RELEASE,
            moved(row1.0, row1.1),
            turn(1),
            moved(rail_entry.0, rail_entry.1),
            PRESS,
            RELEASE,
        ];

        let mut moved_any = false;
        for (index, step) in steps.iter().enumerate() {
            let before = shot(&mut sb);
            let damage = report(&mut sb, step);
            let after = shot(&mut sb);
            assert_eq!(
                unreported_change(&before, &after, bounds(), &damage),
                None,
                "{section:?} step {index} moved a pixel it did not report"
            );
            moved_any |= before.pixels() != after.pixels();
        }
        assert!(
            moved_any,
            "{section:?} drew nothing new for the whole walk, so it proved nothing"
        );
    }
}

#[test]
fn a_scroll_reports_the_whole_list_the_bar_alone_does_not_describe() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let before = shot(&mut sb);

    let damage = report(&mut sb, &turn(1));
    let after = shot(&mut sb);

    assert_ne!(sb.scroll_offset(), 0, "the fixture list is scrollable");
    assert_eq!(
        unreported_change(&before, &after, bounds(), &damage),
        None,
        "every row is drawn somewhere new, not just the scrollbar's thumb"
    );
}

// --- A still pointer follows the rows that move under it ---------------

/// The pointer state task row `row` wears.
fn row_look(sb: &Switchboard, row: usize) -> tairix_controls::state::PointerState {
    sb.tasks.entries[row].row.state().pointer
}

/// The task row whose shown part holds `at`.
fn row_under(sb: &Switchboard, theme: &Theme, (x, y): (i32, i32)) -> Option<usize> {
    let info = list_info(sb, theme);
    (0..info.count).find(|&row| {
        info.window_rect(row, sb.scroll_offset())
            .is_some_and(|shown| shown.contains(Point::new(x, y)))
    })
}

/// Whether `damage` covers every pixel of `rect`.
fn covers(damage: &tairix_geometry::Region, rect: Rect) -> bool {
    let mut uncovered = tairix_geometry::Region::new();
    uncovered.add(rect);
    for covered in damage.rects() {
        uncovered.subtract(*covered);
    }
    uncovered.is_empty()
}

/// A wheel turn slides the rows under a pointer that did not move: the row
/// now under it takes the hover from the row carried away, and the round
/// reports both of them along with the list and the bar the scroll moved.
#[test]
fn a_wheel_turn_under_a_still_pointer_moves_the_hover_to_the_row_now_under_it() {
    use tairix_controls::state::PointerState;
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let at = centre(list_info(&sb, &theme).item_rect(2));
    let _ = report(&mut sb, &moved(at.0, at.1));
    assert_eq!(row_look(&sb, 2), PointerState::Hover);

    let damage = report(&mut sb, &turn(1));

    let under = row_under(&sb, &theme, at).expect("a row is under the pointer");
    assert_ne!(under, 2, "a detent carries another row under the pointer");
    assert_eq!(row_look(&sb, under), PointerState::Hover);
    assert_eq!(row_look(&sb, 2), PointerState::None);
    let info = list_info(&sb, &theme);
    let offset = sb.scroll_offset();
    let bar = Switchboard::compute_layout(bounds(), Scale::ONE, &theme).scroll;
    let lit = info.window_rect(under, offset).expect("the lit row shows");
    let left = info
        .window_rect(2, offset)
        .expect("the row left still shows");
    for (what, rect) in [
        ("the row now lit", lit),
        ("the row left", left),
        ("the list", info.viewport),
        ("the bar", bar),
    ] {
        assert!(covers(&damage, rect), "{what} was not reported");
    }
}

/// A row the wheel carries clean out of view drops its hover too, rather
/// than keeping it to be drawn lit when it scrolls back.
#[test]
fn a_row_the_wheel_carries_out_of_view_drops_its_hover() {
    use tairix_controls::state::PointerState;
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let at = centre(list_info(&sb, &theme).item_rect(0));
    let _ = report(&mut sb, &moved(at.0, at.1));
    assert_eq!(row_look(&sb, 0), PointerState::Hover);

    let _ = report(&mut sb, &turn(3));

    assert_eq!(
        list_info(&sb, &theme).window_rect(0, sb.scroll_offset()),
        None,
        "the premise: the first row left the view"
    );
    assert_eq!(row_look(&sb, 0), PointerState::None);
    let under = row_under(&sb, &theme, at).expect("a row is under the pointer");
    assert_eq!(row_look(&sb, under), PointerState::Hover);
}

/// A sample that shortens a scrolled list clamps it under a still pointer, and
/// the row lit is the one the clamp brought there.
#[test]
fn a_refresh_that_clamps_the_list_lights_the_row_now_under_the_pointer() {
    use tairix_controls::state::PointerState;
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let _ = report(&mut sb, &turn(80));
    let at = centre(list_info(&sb, &theme).viewport);
    let _ = report(&mut sb, &moved(at.0, at.1));
    let scrolled = sb.scroll_offset();

    let mut shorter = model();
    shorter.tasks.truncate(20);
    let _ = refresh(&mut sb, &shorter);

    assert!(
        sb.scroll_offset() < scrolled,
        "the premise: the list clamped"
    );
    let under = row_under(&sb, &theme, at).expect("a row is under the pointer");
    for row in 0..sb.tasks.entries.len() {
        let want = if row == under {
            PointerState::Hover
        } else {
            PointerState::None
        };
        assert_eq!(row_look(&sb, row), want, "row {row}");
    }
}

/// A key that scrolls the focused row into view moves the hover with the rows
/// exactly as the wheel does.
#[test]
fn a_keyboard_reveal_under_a_still_pointer_moves_the_hover_to_the_row_now_under_it() {
    use tairix_controls::state::PointerState;
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let at = centre(list_info(&sb, &theme).item_rect(1));
    let _ = report(&mut sb, &moved(at.0, at.1));
    focus_task_row(&mut sb, 20);
    assert!(
        sb.scroll_offset() > 0,
        "the premise: the walk scrolled the list"
    );

    let under = row_under(&sb, &theme, at).expect("a row is under the pointer");
    assert_ne!(under, 1);
    assert_eq!(row_look(&sb, under), PointerState::Hover);
    assert_eq!(row_look(&sb, 1), PointerState::None);
}

// --- What a fresh reading reports ---------------------------------------

/// A model whose readings have all moved: every task's CPU cell, every fault
/// card's state, and the CPU pane's hero value.
///
/// One fixture rather than three, so the three tests below all publish the
/// same kind of change and none can pass on a section the edit missed.
fn moved_reading() -> SwitchboardModel {
    let mut m = model();
    for (index, task) in m.tasks.iter_mut().enumerate() {
        task.cpu_permille = Some(u16::try_from(index).unwrap_or(0) * 7 + 3);
    }
    for item in &mut m.recovery {
        item.recovery = RecoveryState::RestartRecommended;
    }
    if let Some(cpu) = m.resources.devices.first_mut() {
        cpu.reading = Reading::measured("77%");
        cpu.hero.value = Reading::measured("77");
    }
    m
}

#[test]
fn a_refresh_reports_every_pixel_it_moved_in_every_section() {
    for section in Section::ALL {
        let mut sb = on_tasks(&model());
        let _ = sb.select_section(section);
        let before = shot(&mut sb);

        let damage = refresh(&mut sb, &moved_reading());
        let after = shot(&mut sb);

        assert!(
            !damage.is_empty(),
            "{section:?}: a reading that moved reports the instrument it moved in"
        );
        assert_eq!(
            unreported_change(&before, &after, bounds(), &damage),
            None,
            "{section:?}: a pixel the refresh moved was left out of its report"
        );
    }
}

#[test]
fn a_refresh_reports_less_than_the_client_in_every_section() {
    for section in Section::ALL {
        let mut sb = on_tasks(&model());
        let _ = sb.select_section(section);
        let _ = shot(&mut sb);

        let damage = refresh(&mut sb, &moved_reading());

        assert_ne!(
            damage.bounds(),
            bounds(),
            "{section:?}: the whole client is what this exists to avoid"
        );
    }
}

#[test]
fn a_refresh_that_changed_the_count_reports_every_pixel_it_moved() {
    // A sample that shortens a list moves the scrollbar's thumb as well as
    // the rows, and the bar is no section's region to report.
    for section in Section::ALL {
        let mut sb = on_tasks(&model());
        let _ = sb.select_section(section);
        let before = shot(&mut sb);

        let damage = refresh(&mut sb, &refreshed_model(4, 2));
        let after = shot(&mut sb);

        assert_eq!(
            unreported_change(&before, &after, bounds(), &damage),
            None,
            "{section:?}: a pixel the shortened sample moved was left out of its report"
        );
    }
}

#[test]
fn a_refresh_that_moved_nothing_reports_nothing_in_every_section() {
    for section in Section::ALL {
        let mut sb = on_tasks(&model());
        let _ = sb.select_section(section);
        let _ = shot(&mut sb);

        let damage = refresh(&mut sb, &model());

        assert!(
            damage.is_empty(),
            "{section:?}: an unmoved reading owes the screen nothing, reported {:?}",
            damage.rects()
        );
    }
}

// --- Pixel scrolling ---------------------------------------------------------

/// A wheel turned `units` of the seat's scroll units, a fraction of a detent
/// where `units` is short of one.
fn wheel_units(units: i32) -> InputEvent {
    InputEvent::PointerScrolled { dx: 0, dy: units }
}

#[test]
fn a_fraction_of_a_detent_scrolls_a_fraction_of_a_row() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let row = u64::from(Switchboard::row_item_height(Scale::ONE, &theme));

    // A third of a detent is sixteen pixels: less than a row, and not a
    // multiple of one, so the list comes to rest between two rows.
    feed(&mut sb, &theme, &wheel_units(40));
    assert_eq!(sb.scroll_offset(), DETENT_PX / 3);
    assert_ne!(sb.scroll_offset() % row, 0, "the offset sits between rows");

    // What a turn falls short of a whole pixel is carried, not dropped: a
    // unit then the rest of a detent is exactly a detent in all.
    let mut sb = settled(&theme);
    feed(&mut sb, &theme, &wheel_units(1));
    assert_eq!(sb.scroll_offset(), 0, "one unit is less than a pixel");
    feed(&mut sb, &theme, &wheel_units(119));
    assert_eq!(sb.scroll_offset(), DETENT_PX);
}

#[test]
fn a_thumb_drag_lands_between_rows() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let layout = Switchboard::compute_layout(bounds(), Scale::ONE, &theme);
    let thumb = sb
        .scroll
        .part_rect(ScrollPart::Thumb, layout.scroll, Scale::ONE, &theme)
        .expect("the fifty-row list has a thumb to drag");
    let (x, y) = centre(thumb);

    for event in [moved(x, y), PRESS, moved(x, y + 1), RELEASE] {
        feed(&mut sb, &theme, &event);
    }

    let row = u64::from(Switchboard::row_item_height(Scale::ONE, &theme));
    let offset = sb.scroll_offset();
    assert!(
        offset > 0 && offset < row,
        "a thumb moved one pixel moves the list a few pixels, not a row: {offset}"
    );
}

#[test]
fn a_row_scrolled_part_way_past_is_cut_not_squeezed() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let before = shot(&mut sb);

    // Half a row: the first row is half under the pinned headings and the
    // last one shown is cut by the viewport's bottom edge.
    let by = Switchboard::row_item_height(Scale::ONE, &theme) / 2;
    feed(
        &mut sb,
        &theme,
        &wheel_units(i32::try_from(by).unwrap_or(0) * 5 / 2),
    );
    assert_eq!(sb.scroll_offset(), u64::from(by));
    let after = shot(&mut sb);

    assert_eq!(
        not_slid_up(&before, &after, list_info(&sb, &theme).viewport, by),
        None,
        "every row keeps its natural size and simply moves: none is re-laid \
         out into what is left of the viewport"
    );
}

#[test]
fn a_scroll_reports_its_list_and_bar_and_nothing_beside_them() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let before = shot(&mut sb);

    let damage = report(&mut sb, &turn(1));
    let after = shot(&mut sb);

    let layout = Switchboard::compute_layout(bounds(), Scale::ONE, &theme);
    let viewport = list_info(&sb, &theme).viewport;
    assert!(
        damage.contains(centre_point(layout.scroll)),
        "the thumb moved, so the bar is reported"
    );
    assert_eq!(
        unreported_change(&before, &after, bounds(), &damage),
        None,
        "every row is drawn somewhere new"
    );
    // The list abuts its bar, so the region may hold one band spanning both.
    assert!(
        damage
            .rects()
            .iter()
            .all(|rect| within_either(rect, viewport, layout.scroll)),
        "the pinned headings did not move: {:?}",
        damage.rects()
    );
}

#[test]
fn the_keyboard_reveals_a_row_the_list_had_scrolled_part_way_past() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let row = Switchboard::row_item_height(Scale::ONE, &theme);
    focus_task_row(&mut sb, 3);
    // Scroll so row three is half under the headings.
    let offset = 3 * row + row / 2;
    feed(
        &mut sb,
        &theme,
        &wheel_units(i32::try_from(offset).unwrap_or(0) * 5 / 2),
    );
    assert_eq!(sb.scroll_offset(), u64::from(offset));

    // Up onto row two and back down onto three: the cursor lands on a row the
    // reader can see whole, and the list moves no further than that takes.
    assert_eq!(key(&mut sb, Key::Named(NamedKey::Up)), None);
    assert_eq!(sb.scroll_offset(), u64::from(2 * row));
    let info = list_info(&sb, &theme);
    assert_eq!(
        info.window_rect(2, sb.scroll_offset()),
        Some(Rect::new(
            info.viewport.left(),
            info.viewport.top(),
            info.viewport.width,
            row
        )),
        "the revealed row stands whole at the top of the viewport"
    );
}

#[test]
fn every_pixel_a_keyboard_walk_moves_lies_inside_what_it_reported() {
    // A focus ring or a Focus Field written through a plain setter reports
    // nothing on its own, so every region's marks have to be reported by
    // whoever moved them: the rows, the cards, the commands, the footer, and
    // the scrollbar's own ring.
    let theme = Theme::dark();
    for section in Section::ALL {
        let mut sb = settled(&theme);
        sb.select_section(section);
        let _ = painted(&mut sb, &theme);
        let span = sb.active().focus_span();
        let mut keys = alloc::vec::Vec::new();
        keys.extend(core::iter::repeat_n(NamedKey::Down, span + 1));
        keys.extend([NamedKey::Right, NamedKey::Left]);
        keys.extend(core::iter::repeat_n(NamedKey::Up, span + 1));
        keys.extend([
            NamedKey::Tab,
            NamedKey::Down,
            NamedKey::PageDown,
            NamedKey::Tab,
            NamedKey::Down,
            NamedKey::End,
            NamedKey::Home,
            NamedKey::Tab,
            NamedKey::Down,
        ]);

        let mut moved_any = false;
        for (index, named) in keys.into_iter().enumerate() {
            let before = shot(&mut sb);
            let mut damage = tairix_controls::damage::sink();
            sb.on_key(
                Key::Named(named),
                bounds(),
                Scale::ONE,
                &theme,
                font(),
                &mut damage,
            );
            let after = shot(&mut sb);
            assert_eq!(
                unreported_change(&before, &after, bounds(), &damage),
                None,
                "{section:?} step {index} ({named:?}) moved a pixel it did not report"
            );
            moved_any |= before.pixels() != after.pixels();
        }
        assert!(moved_any, "{section:?}: the walk drew nothing new");
    }
}

/// The centre of `rect` as a point.
fn centre_point(rect: Rect) -> Point {
    let (x, y) = centre(rect);
    Point::new(x, y)
}

/// Whether every pixel of `rect` lies inside `a` or inside `b`.
fn within_either(rect: &Rect, a: Rect, b: Rect) -> bool {
    (rect.top()..rect.bottom()).all(|y| {
        (rect.left()..rect.right()).all(|x| {
            let at = Point::new(x, y);
            a.contains(at) || b.contains(at)
        })
    })
}

// --- The Edge Wake beside a scrolled list ----------------------------------

/// The command rail's content rectangle for the section on show.
fn command_rail(sb: &Switchboard, theme: &Theme) -> Rect {
    let layout = Switchboard::compute_layout(bounds(), Scale::ONE, theme);
    let ctx = sb.section_ctx(&layout, bounds(), Scale::ONE, theme, font());
    super::block::titled_content(
        ctx.frame
            .rail
            .expect("the fixture window seats the commands"),
        Scale::ONE,
        theme,
    )
    .expect("the rail has room for its commands")
}

/// How many columns of the Edge Wake's colour run in from `rail`'s leading
/// edge, halfway down it: the wake's painted thickness, or nought unlit.
fn wake_columns(surface: &Surface, rail: Rect, theme: &Theme) -> u32 {
    let wake = Color::from(theme.palette().rim_active).premultiply();
    let y = u32::try_from(rail.top()).unwrap_or(0) + rail.height / 2;
    let left = u32::try_from(rail.left()).unwrap_or(0);
    (left..left + rail.width)
        .take_while(|&x| surface.get(x, y) == Some(wake))
        .fold(0, |columns, _| columns + 1)
}

/// The processor's pane, laid out by one render: the surface's default
/// subject, and a flow long enough to scroll beside its device commands.
fn settled_pane(theme: &Theme) -> Switchboard {
    let mut sb = Switchboard::new(&model());
    let _ = painted(&mut sb, theme);
    sb
}

#[test]
fn the_commands_beside_an_unscrolled_pane_wear_no_edge_wake() {
    let theme = Theme::dark();
    let mut sb = settled_pane(&theme);
    let surface = painted(&mut sb, &theme);
    assert_eq!(wake_columns(&surface, command_rail(&sb, &theme), &theme), 0);
}

#[test]
fn scrolling_a_pane_back_to_its_start_puts_the_edge_wake_out() {
    let theme = Theme::dark();
    let mut sb = settled_pane(&theme);
    let _ = report(&mut sb, &turn(1));
    let before = shot(&mut sb);

    let damage = report(&mut sb, &turn(-1));
    let after = shot(&mut sb);

    assert_eq!(sb.scroll_offset(), 0);
    assert_eq!(wake_columns(&after, command_rail(&sb, &theme), &theme), 0);
    assert_eq!(unreported_change(&before, &after, bounds(), &damage), None);
}

#[test]
fn a_refresh_that_returns_a_pane_to_its_start_puts_the_edge_wake_out() {
    let theme = Theme::dark();
    let mut sb = settled_pane(&theme);
    let _ = report(&mut sb, &turn(4));
    let before = shot(&mut sb);
    assert!(sb.scroll_offset() > 0, "the processor pane scrolls");
    assert!(wake_columns(&before, command_rail(&sb, &theme), &theme) > 0);

    // A pane of its hero alone fits the viewport, so the flow is clamped back
    // to its start.
    let mut short = model();
    if let Some(cpu) = short.resources.devices.first_mut() {
        cpu.blocks.clear();
    }
    let damage = refresh(&mut sb, &short);
    let after = shot(&mut sb);

    assert_eq!(sb.scroll_offset(), 0);
    assert_eq!(wake_columns(&after, command_rail(&sb, &theme), &theme), 0);
    assert_eq!(unreported_change(&before, &after, bounds(), &damage), None);
}

#[test]
fn the_task_table_lights_no_edge_wake_having_no_commands_beside_it() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    let _ = report(&mut sb, &turn(1));
    assert!(sb.scroll_offset() > 0, "the rows scroll");
    let layout = Switchboard::compute_layout(bounds(), Scale::ONE, &theme);
    let frame = sb.section_frame(&layout, Scale::ONE, &theme);
    assert_eq!(frame.rail, None);
    assert_eq!(sb.active().wake_rail(&frame, Scale::ONE, &theme), None);
}

#[test]
fn a_scrolled_pane_lights_the_edge_wake_on_its_device_commands() {
    let theme = Theme::dark();
    let mut sb = settled_pane(&theme);
    let before = shot(&mut sb);

    let damage = report(&mut sb, &turn(1));
    let after = shot(&mut sb);

    assert!(sb.scroll_offset() > 0, "the processor pane scrolls");
    assert!(wake_columns(&after, command_rail(&sb, &theme), &theme) > 0);
    assert_eq!(unreported_change(&before, &after, bounds(), &damage), None);
}

#[test]
fn a_card_section_lights_no_edge_wake() {
    let theme = Theme::dark();
    let mut sb = settled(&theme);
    sb.select_section(Section::Recovery);
    let _ = painted(&mut sb, &theme);

    let _ = report(&mut sb, &turn(1));
    let surface = painted(&mut sb, &theme);

    assert!(sb.scroll_offset() > 0, "the fault cards scroll");
    assert_eq!(wake_columns(&surface, command_rail(&sb, &theme), &theme), 0);
}

#[test]
fn heavy_contrast_draws_a_thicker_edge_wake() {
    let dark = Theme::dark();
    let heavy = high_contrast();
    let thickness = |theme: &Theme| {
        let mut sb = settled_pane(theme);
        let _ = sb.on_pointer(
            &turn(1),
            bounds(),
            Scale::ONE,
            theme,
            font(),
            &mut tairix_controls::damage::sink(),
        );
        let surface = painted(&mut sb, theme);
        wake_columns(&surface, command_rail(&sb, theme), theme)
    };
    let (normal, strong) = (thickness(&dark), thickness(&heavy));
    assert!(normal > 0, "the wake is lit");
    assert!(
        strong > normal,
        "heavy contrast widens the wake: {strong} against {normal}"
    );
}
