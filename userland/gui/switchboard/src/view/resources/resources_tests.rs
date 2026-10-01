//! Unit tests for the Resources section's pane as a scrolled view: that the
//! flow scrolls a pixel at a time beneath a banner that stands, that an item
//! scrolled part-way past keeps its natural size and is cut by the viewport
//! rather than squeezed, and that a refresh reports what it moved where it
//! shows.

use alloc::string::String;

use tairix_geometry::Scale;
use tairix_input::InputEvent;
use tairix_theme::Theme;

use tairix_controls::damage;

use super::pane::{self, ItemBody};
use super::{BlockBody, DeviceId, PaneBlock};
use crate::view::reading::ReadingFact;
use crate::view::test_support::{
    bounds, model, not_slid_up, pointer, refresh, report, shot, turn, unreported_change,
};
use crate::view::{ListInfo, Sweep, Switchboard, SwitchboardModel};

/// The screen showing `model` on `device`'s pane, laid out once.
fn on_device(model: &SwitchboardModel, device: DeviceId) -> Switchboard {
    let mut sb = Switchboard::new(model);
    sb.resources
        .select_device(device, &mut Sweep::adopting(&mut damage::sink()));
    let _ = shot(&mut sb);
    sb
}

/// The pane's flow as the fixture window lays it out.
fn flow(sb: &Switchboard) -> ListInfo {
    let layout = Switchboard::compute_layout(bounds(), Scale::ONE, &Theme::dark());
    sb.list_info(&layout, Scale::ONE, &Theme::dark())
}

/// A wheel turned `pixels` pixels' worth at the unit scale.
fn wheel_pixels(pixels: u32) -> InputEvent {
    InputEvent::PointerScrolled {
        dx: 0,
        dy: i32::try_from(pixels).unwrap_or(0) * 5 / 2,
    }
}

/// The fixture model with the memory pane made taller than its flow's
/// viewport by `blocks` more blocks of facts.
fn tall_memory(blocks: usize) -> SwitchboardModel {
    let mut m = model();
    let memory = m
        .resources
        .devices
        .iter_mut()
        .find(|device| device.id == DeviceId::Memory)
        .expect("the fixture carries a memory device");
    for block in 0..blocks {
        memory.blocks.push(PaneBlock::full(
            "KERNEL",
            BlockBody::Facts(
                (0..4)
                    .map(|fact| {
                        ReadingFact::text(alloc::format!("fact {block}.{fact}"), "measured")
                    })
                    .collect(),
            ),
        ));
    }
    m
}

#[test]
fn the_cpu_graph_keeps_its_full_height_when_scrolled_part_way() {
    // Two rows down, the hero's plot is half above the viewport. It keeps the
    // height its band gives it and slides; the viewport's edge cuts it, and
    // the item the bottom edge cuts is drawn at its own size too.
    let theme = Theme::dark();
    let mut sb = on_device(&model(), DeviceId::Cpu);
    let before = shot(&mut sb);
    let list = flow(&sb);
    let by = 2 * list.pitch;
    assert!(
        list.extent() > u64::from(list.viewport.height),
        "the fixture's processor pane is taller than its viewport"
    );

    let _ = pointer(&mut sb, bounds(), Scale::ONE, &theme, &wheel_pixels(by));
    assert_eq!(sb.scroll_offset(), u64::from(by));
    let after = shot(&mut sb);

    assert_eq!(
        not_slid_up(&before, &after, list.viewport, by),
        None,
        "every item keeps its natural size and simply moves"
    );
    let pad = tairix_controls::block::content_inset(Scale::ONE, &theme);
    let hero = sb
        .resources
        .items
        .iter()
        .find(|item| matches!(item.body, ItemBody::Hero { .. }))
        .and_then(|item| pane::item_rect(item, list.viewport, list.pitch, pad))
        .expect("the processor pane leads with its hero");
    let shown = list
        .view(sb.scroll_offset())
        .to_window(hero)
        .expect("half the hero still shows");
    assert_eq!(
        shown.bottom(),
        hero.bottom() - i32::try_from(by).unwrap_or(0),
        "the hero's lower edge moved by exactly the scroll"
    );
    assert_eq!(
        shown.top(),
        list.viewport.top(),
        "and the viewport's edge cuts it rather than its band shrinking"
    );
}

#[test]
fn a_banner_stands_above_the_flow_rather_than_being_counted_into_it() {
    // The banner was once reserved rows at the head of the flow *and* drawn
    // above it, so the hero sat two rows below the banner and the scroll
    // range spanned two rows the pane never drew in.
    let theme = Theme::dark();
    let sb = on_device(&model(), DeviceId::Memory);
    let layout = Switchboard::compute_layout(bounds(), Scale::ONE, &theme);
    let frame = sb.section_frame(&layout, Scale::ONE, &theme);
    let pane = sb.resources.pane_layout(&frame, Scale::ONE, &theme);
    let (band, _) = pane.banner.expect("the memory pane wears its banner");
    let list = flow(&sb);

    assert_eq!(
        list.viewport, pane.flow,
        "one split for the paint and the range"
    );
    assert_eq!(
        list.viewport.top(),
        band.bottom(),
        "the flow starts where the banner ends"
    );
    assert_eq!(list.viewport.bottom(), frame.primary.bottom());
    assert_eq!(
        sb.resources.items.first().map(|item| item.row),
        Some(0),
        "the flow keeps no rows of its own for the banner"
    );
    let range = sb.scroll.model().range();
    assert_eq!(range.viewport_extent(), u64::from(list.viewport.height));
    assert_eq!(range.content_extent(), list.extent());
}

#[test]
fn a_bannered_pane_scrolls_all_the_way_to_its_last_row() {
    let theme = Theme::dark();
    let mut sb = on_device(&tall_memory(3), DeviceId::Memory);
    let _ = pointer(&mut sb, bounds(), Scale::ONE, &theme, &turn(40));
    let rows = flow(&sb);
    let pad = tairix_controls::block::content_inset(Scale::ONE, &theme);
    let final_item = sb
        .resources
        .items
        .iter()
        .filter_map(|item| pane::item_rect(item, rows.viewport, rows.pitch, pad))
        .max_by_key(tairix_geometry::Rect::bottom)
        .expect("the pane has items");
    let shown = rows
        .view(sb.scroll_offset())
        .to_window(final_item)
        .expect("scrolled to its end, the pane shows its last item");
    assert_eq!(shown.height, final_item.height, "the last item shows whole");
    assert!(
        shown.bottom() <= rows.viewport.bottom(),
        "and inside the drawn flow, not under the window's edge"
    );
}

#[test]
fn a_refresh_on_a_bannered_pane_reports_every_pixel_it_moved() {
    // A moved reading in the flow and moved words in the banner: both are
    // drawn somewhere the report has to name.
    let mut sb = on_device(&model(), DeviceId::Memory);
    let before = shot(&mut sb);
    let mut moved = model();
    let memory = moved
        .resources
        .devices
        .iter_mut()
        .find(|device| device.id == DeviceId::Memory)
        .expect("the fixture carries a memory device");
    if let Some(BlockBody::Composition(parts)) = memory.blocks.first_mut().map(|b| &mut b.body) {
        parts[0].amount = String::from("4.4 GB");
    }
    if let Some(banner) = memory.banner.as_mut() {
        banner.summary = String::from("Memory pressure has stood in the moderate band for 5m");
    }

    let reported = refresh(&mut sb, &moved);
    let after = shot(&mut sb);

    assert!(!reported.is_empty());
    assert_eq!(
        unreported_change(&before, &after, bounds(), &reported),
        None,
        "a pixel the refresh moved was left out of its report"
    );
    assert_ne!(
        reported.bounds(),
        bounds(),
        "and it did not report the client"
    );
}

#[test]
fn a_banner_that_comes_or_goes_repaints_the_pane_it_moved() {
    // Whether the banner is up decides where the flow starts, so every item
    // moves when it comes or goes.
    let mut sb = on_device(&model(), DeviceId::Memory);
    let mut calm = model();
    for device in &mut calm.resources.devices {
        device.banner = None;
    }
    for next in [calm, model()] {
        let before = shot(&mut sb);
        let reported = refresh(&mut sb, &next);
        let after = shot(&mut sb);
        assert_eq!(
            unreported_change(&before, &after, bounds(), &reported),
            None,
            "the flow moved with the banner and was left out of the report"
        );
    }
}

#[test]
fn the_banner_stands_while_the_flow_scrolls() {
    let mut sb = on_device(&tall_memory(3), DeviceId::Memory);
    let before = shot(&mut sb);
    let layout = Switchboard::compute_layout(bounds(), Scale::ONE, &Theme::dark());
    let frame = sb.section_frame(&layout, Scale::ONE, &Theme::dark());
    let (band, _) = sb
        .resources
        .pane_layout(&frame, Scale::ONE, &Theme::dark())
        .banner
        .expect("the memory pane wears its banner");

    let reported = report(&mut sb, &wheel_pixels(20));
    assert_eq!(sb.scroll_offset(), 20);
    let after = shot(&mut sb);

    assert_eq!(
        unreported_change(&before, &after, band, &damage::sink()),
        None,
        "the banner is pinned: scrolling the flow moves none of it"
    );
    assert_eq!(not_slid_up(&before, &after, flow(&sb).viewport, 20), None);
    assert!(
        reported
            .rects()
            .iter()
            .all(|rect| rect.intersection(&band).is_empty()),
        "nor does the scroll report it: {:?}",
        reported.rects()
    );
}
