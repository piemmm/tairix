use super::*;
use alloc::string::ToString;
use core::net::{IpAddr, Ipv4Addr};

use tairix_abi::window_ipc::AppMenuRowView;
use tairix_input::Modifiers;
use tairix_theme::{ThemeRegistry, Timeline};

use crate::layout;
use crate::menu::Command;
use crate::model::{EdgeKind, Endpoint, Map, NodeKey, NodeKind};

const CANVAS: Rect = Rect::new(0, 0, 800, 600);
/// The instant the tests' gestures arrive at, well clear of zero.
const NOW: u64 = 1_000_000_000;

fn host(n: u8) -> NodeKey {
    NodeKey::Neighbour(IpAddr::V4(Ipv4Addr::new(10, 0, 0, n)))
}

fn service(name: &str) -> NodeKey {
    NodeKey::Service(String::from(name))
}

/// Motion that lands every change at once.
fn instant() -> MotionTheme {
    Theme::dark().motion().with_reduced_motion(true)
}

/// The desktop's own pace.
fn paced() -> MotionTheme {
    Theme::dark().motion()
}

fn panel_open_ns() -> u64 {
    u64::from(paced().duration(MotionInteraction::PanelOpen)) * 1_000_000
}

/// The local host and two neighbours, laid out.
fn sample() -> Map {
    let mut map = Map::new();
    let local = map
        .intern(NodeKey::LocalHost, NodeKind::LocalHost, 0)
        .unwrap();
    for n in [2, 3] {
        let id = map.intern(host(n), NodeKind::Neighbour, 0).unwrap();
        map.link(local, id, EdgeKind::OnLink);
    }
    layout(&mut map);
    map
}

/// [`sample`] with host 2 a printer offering three services: two web pages
/// and its print queue.
fn printing() -> Map {
    let mut map = Map::new();
    let local = map
        .intern(NodeKey::LocalHost, NodeKind::LocalHost, 0)
        .unwrap();
    let mut hosts = Vec::new();
    for n in [2, 3] {
        let id = map.intern(host(n), NodeKind::Neighbour, 0).unwrap();
        map.set_label(id, &alloc::format!("host{n}"));
        map.add_addr(id, IpAddr::V4(Ipv4Addr::new(10, 0, 0, n)));
        map.link(local, id, EdgeKind::OnLink);
        hosts.push(id);
    }
    map.set_classification(
        hosts[0],
        crate::model::Classification {
            device: DeviceClass::Printer,
            device_confidence: 80,
            ..crate::model::Classification::default()
        },
        Vec::new(),
    );
    for (name, kind, port) in [
        ("Admin._http._tcp", "_http._tcp", 80),
        ("Office._ipp._tcp", "_ipp._tcp", 631),
        ("Status._http._tcp", "_http._tcp", 8080),
    ] {
        let id = map.intern(service(name), NodeKind::Service, 0).unwrap();
        let (instance, _) = name.split_once('.').unwrap();
        map.set_label(id, instance);
        map.set_endpoint(
            id,
            Endpoint {
                service_type: String::from(kind),
                port,
                host: String::from("printer.local"),
                text: Vec::new(),
            },
        );
        map.add_addr(id, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)));
        map.link(hosts[0], id, EdgeKind::Serves);
    }
    layout(&mut map);
    map
}

fn view() -> MapView {
    let mut view = MapView::new(CANVAS, instant());
    view.set_map(sample());
    view
}

fn printing_view(motion: MotionTheme) -> MapView {
    let mut view = MapView::new(CANVAS, motion);
    view.set_map(printing());
    view
}

fn window_point(view: &MapView, key: &NodeKey) -> Point {
    let node = view.map().node(view.map().id_for(key).unwrap()).unwrap();
    view.viewport().to_window(node.pos)
}

fn repaints(reaction: &Reaction) -> bool {
    matches!(reaction, Reaction::Repaint)
}

fn input(view: &mut MapView, event: &InputEvent, now: u64) -> Reaction {
    view.on_input(event, Scale::ONE, now)
}

fn click_at(view: &mut MapView, at: Point, now: u64) -> bool {
    input(view, &InputEvent::PointerMoved { to: at }, now);
    input(
        view,
        &InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        now,
    );
    repaints(&input(
        view,
        &InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
        now,
    ))
}

fn click(view: &mut MapView, at: Point) -> bool {
    click_at(view, at, NOW)
}

/// Where each service is laid at `now`, by key.
fn services_out(view: &mut MapView, now: u64) -> Vec<(NodeKey, Point)> {
    view.lay_plates(Scale::ONE, now);
    view.plates
        .iter()
        .filter(|plate| plate.layer == Layer::Service)
        .map(|plate| (view.map().node(plate.id).unwrap().key.clone(), plate.at))
        .collect()
}

fn render(view: &mut MapView, now: u64) -> Surface {
    let themes = ThemeRegistry::with_builtins();
    let mut surface = Surface::new(CANVAS.width, CANVAS.height).unwrap();
    view.render(&mut surface, Scale::ONE, themes.active(), now);
    surface
}

fn distance(a: Point, b: Point) -> f64 {
    mathf::hypot(f64::from(a.x - b.x), f64::from(a.y - b.y))
}

#[test]
fn a_click_on_a_node_selects_it_and_a_click_on_nothing_clears_it() {
    let mut view = view();
    let at = window_point(&view, &host(2));
    assert!(click(&mut view, at));
    assert_eq!(view.selected().map(|n| n.key.clone()), Some(host(2)));
    assert!(click(&mut view, Point::new(799, 1)));
    assert!(view.selected().is_none());
}

#[test]
fn a_drag_pans_and_does_not_select() {
    let mut view = view();
    let at = window_point(&view, &host(2));
    let before = view.viewport().center();
    input(&mut view, &InputEvent::PointerMoved { to: at }, NOW);
    input(
        &mut view,
        &InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        NOW,
    );
    assert!(repaints(&input(
        &mut view,
        &InputEvent::PointerMoved {
            to: Point::new(at.x + 60, at.y),
        },
        NOW,
    )));
    input(
        &mut view,
        &InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
        NOW,
    );
    let dragged = i32::try_from(60 * 1000 / view.viewport().zoom()).unwrap();
    assert_eq!(view.viewport().center().x, before.x - dragged);
    assert!(view.selected().is_none());
}

#[test]
fn a_two_finger_scroll_pans_both_axes_and_ctrl_wheel_zooms_about_the_pointer() {
    let mut view = view();
    let before = view.viewport().center();
    assert!(repaints(&input(
        &mut view,
        &InputEvent::PointerScrolled { dx: 120, dy: 240 },
        NOW,
    )));
    let after = view.viewport().center();
    assert!(after.x > before.x && after.y > before.y);

    let anchor = Point::new(500, 200);
    input(&mut view, &InputEvent::PointerMoved { to: anchor }, NOW);
    let under = view.viewport().to_graph(anchor);
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    input(
        &mut view,
        &InputEvent::ModifiersChanged { modifiers: ctrl },
        NOW,
    );
    let zoom = view.viewport().zoom();
    assert!(repaints(&input(
        &mut view,
        &InputEvent::PointerScrolled { dx: 0, dy: -120 },
        NOW,
    )));
    assert!(view.viewport().zoom() > zoom);
    let held = view.viewport().to_window(under);
    assert!((held.x - anchor.x).abs() <= 2 && (held.y - anchor.y).abs() <= 2);
}

#[test]
fn a_pinch_zooms_from_where_it_began_and_a_cancel_returns_there() {
    let mut view = view();
    let at = Point::new(300, 300);
    let start = view.viewport().zoom();
    let pinch = |phase, scale| InputEvent::Pinch { phase, scale, at };
    input(&mut view, &pinch(PinchPhase::Begin, PINCH_SCALE_ONE), NOW);
    input(
        &mut view,
        &pinch(PinchPhase::Update, PINCH_SCALE_ONE * 2),
        NOW,
    );
    assert_eq!(view.viewport().zoom(), start * 2);
    // Each update is relative to the zoom the pinch began at, not the last.
    input(
        &mut view,
        &pinch(PinchPhase::Update, PINCH_SCALE_ONE * 3 / 2),
        NOW,
    );
    assert_eq!(view.viewport().zoom(), start * 3 / 2);
    input(
        &mut view,
        &pinch(PinchPhase::Cancel, PINCH_SCALE_ONE * 4),
        NOW,
    );
    assert_eq!(view.viewport().zoom(), start);
    // An update with no pinch begun does nothing.
    assert!(!repaints(&input(
        &mut view,
        &pinch(PinchPhase::Update, PINCH_SCALE_ONE * 2),
        NOW,
    )));
}

#[test]
fn the_selection_follows_its_node_across_a_rebuild_and_drops_when_it_goes() {
    let mut view = view();
    let at = window_point(&view, &host(3));
    click(&mut view, at);
    view.set_map(sample());
    assert_eq!(view.selected().map(|n| n.key.clone()), Some(host(3)));
    view.set_map(Map::new());
    assert!(view.selected().is_none());
}

#[test]
fn every_device_class_has_its_own_glyph_and_an_unknown_host_is_never_blank() {
    let mut map = Map::new();
    let id = map.intern(host(9), NodeKind::Neighbour, 0).unwrap();
    assert_eq!(
        node_look(map.node(id).unwrap()).glyph,
        IconKind::DeviceUnknown
    );
    let gateway = map
        .intern(
            NodeKey::Gateway(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
            NodeKind::Gateway,
            0,
        )
        .unwrap();
    assert_eq!(
        node_look(map.node(gateway).unwrap()).glyph,
        IconKind::Router
    );
    map.set_classification(
        id,
        crate::model::Classification {
            device: DeviceClass::Printer,
            device_confidence: 70,
            ..crate::model::Classification::default()
        },
        Vec::new(),
    );
    assert_eq!(node_look(map.node(id).unwrap()).glyph, IconKind::Printer);
}

#[test]
fn a_node_wears_the_hue_of_what_it_is() {
    let mut map = Map::new();
    let id = map.intern(host(9), NodeKind::Neighbour, 0).unwrap();
    assert_eq!(node_look(map.node(id).unwrap()).hue, BadgeHue::Grey);
    let gateway = map
        .intern(
            NodeKey::Gateway(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
            NodeKind::Gateway,
            0,
        )
        .unwrap();
    assert_eq!(node_look(map.node(gateway).unwrap()).hue, BadgeHue::Orange);
    for (class, hue) in [
        (DeviceClass::Printer, BadgeHue::Purple),
        (DeviceClass::Phone, BadgeHue::Green),
        (DeviceClass::Nas, BadgeHue::Teal),
        (DeviceClass::Iot, BadgeHue::Pink),
        (DeviceClass::Server, BadgeHue::Graphite),
        (DeviceClass::Laptop, BadgeHue::Blue),
    ] {
        map.set_classification(
            id,
            crate::model::Classification {
                device: class,
                device_confidence: 50,
                ..crate::model::Classification::default()
            },
            Vec::new(),
        );
        assert_eq!(node_look(map.node(id).unwrap()).hue, hue, "{class:?}");
    }
}

#[test]
fn a_hosts_services_are_neither_drawn_nor_hit_until_it_is_spread() {
    let mut view = printing_view(instant());
    assert!(services_out(&mut view, NOW).is_empty(), "folded by default");
    // Every folded service lies under its host, so a click there is the host's.
    let printer = window_point(&view, &host(2));
    click(&mut view, printer);
    assert_eq!(view.selected().map(|n| n.key.clone()), Some(host(2)));
    let out = services_out(&mut view, NOW);
    assert_eq!(out.len(), 3, "the click spread all three: {out:?}");
    for (key, at) in &out {
        assert!(
            distance(*at, printer) >= f64::from(SPREAD_FIRST_ARC) - 1.0,
            "{key:?} stands clear of its host"
        );
    }
}

#[test]
fn a_click_on_the_spread_host_folds_it_and_keeps_it_selected() {
    let mut view = printing_view(instant());
    let printer = window_point(&view, &host(2));
    click(&mut view, printer);
    assert!(view.is_spread(&host(2)));
    assert!(click(&mut view, printer), "folding is a change");
    assert!(!view.is_spread(&host(2)));
    assert_eq!(view.selected().map(|n| n.key.clone()), Some(host(2)));
    render(&mut view, NOW);
    assert!(services_out(&mut view, NOW).is_empty());
    click(&mut view, printer);
    assert!(view.is_spread(&host(2)), "and a third click spreads again");
}

#[test]
fn choosing_anything_else_folds_the_spread_and_escape_folds_and_deselects() {
    let mut view = printing_view(instant());
    let printer = window_point(&view, &host(2));
    click(&mut view, printer);
    let at = window_point(&view, &host(3));
    click(&mut view, at);
    assert!(!view.is_spread(&host(2)), "another host folds it");

    click(&mut view, printer);
    click(&mut view, Point::new(799, 1));
    assert!(!view.is_spread(&host(2)), "empty canvas folds it");
    assert!(view.selected().is_none());

    click(&mut view, printer);
    let escape = InputEvent::KeyPressed {
        key: Key::Named(NamedKey::Escape),
        modifiers: Modifiers::default(),
    };
    assert!(repaints(&input(&mut view, &escape, NOW)));
    assert!(!view.is_spread(&host(2)));
    assert!(view.selected().is_none());
    assert!(
        !repaints(&input(&mut view, &escape, NOW)),
        "nothing left to put away"
    );
}

#[test]
fn a_spread_service_is_hit_where_it_is_drawn_and_keeps_its_host_spread() {
    let mut view = printing_view(instant());
    let at = window_point(&view, &host(2));
    click(&mut view, at);
    let (key, at) = services_out(&mut view, NOW)[0].clone();
    assert!(CANVAS.contains(at), "{at:?} is on the canvas");
    assert!(click(&mut view, at));
    assert_eq!(view.selected().map(|n| n.key.clone()), Some(key));
    assert!(view.is_spread(&host(2)));
}

#[test]
fn a_spread_comes_out_over_the_panels_duration_and_then_owes_no_frame() {
    let mut view = printing_view(paced());
    let printer = window_point(&view, &host(2));
    click_at(&mut view, printer, NOW);
    let first = view.next_frame_in(NOW).expect("a frame is owed");
    assert!(first <= Timeline::FRAME_NS);

    let span = panel_open_ns();
    let halfway = services_out(&mut view, NOW + span / 2);
    let landed = services_out(&mut view, NOW + span);
    assert_eq!((halfway.len(), landed.len()), (3, 3));
    for ((key, mid), (_, end)) in halfway.iter().zip(&landed) {
        let (mid, end) = (distance(*mid, printer), distance(*end, printer));
        assert!(
            0.0 < mid && mid < end,
            "{key:?} is on its way: {mid} of {end}"
        );
    }

    render(&mut view, NOW + span);
    assert_eq!(
        view.next_frame_in(NOW + span),
        None,
        "the finished frame was drawn, so nothing more is owed"
    );
    assert_eq!(
        services_out(&mut view, NOW + span * 4),
        landed,
        "and it stays out"
    );
}

#[test]
fn a_fold_that_interrupts_a_spread_reverses_from_where_it_stands() {
    let mut view = printing_view(paced());
    let printer = window_point(&view, &host(2));
    click_at(&mut view, printer, NOW);
    let turn = NOW + panel_open_ns() / 3;
    let before = services_out(&mut view, turn);
    assert_eq!(before.len(), 3, "part of the way out");
    click_at(&mut view, printer, turn);
    assert!(!view.is_spread(&host(2)));
    assert_eq!(
        services_out(&mut view, turn),
        before,
        "the fold starts where the spread stood"
    );
    let end = turn + panel_open_ns();
    render(&mut view, end);
    assert!(
        services_out(&mut view, end).is_empty(),
        "folded all the way"
    );
    assert_eq!(view.next_frame_in(end), None);
}

#[test]
fn reduced_motion_lands_a_spread_at_once_and_arms_nothing() {
    let mut view = printing_view(instant());
    let at = window_point(&view, &host(2));
    click(&mut view, at);
    assert_eq!(view.next_frame_in(NOW), None);
    assert_eq!(services_out(&mut view, NOW).len(), 3);
}

#[test]
fn a_spread_follows_its_host_across_a_rebuild_and_drops_when_it_goes() {
    let mut view = printing_view(instant());
    let at = window_point(&view, &host(2));
    click(&mut view, at);
    view.set_map(printing());
    assert!(view.is_spread(&host(2)));
    assert_eq!(services_out(&mut view, NOW).len(), 3);
    view.set_map(sample());
    assert!(
        !view.is_spread(&host(2)),
        "a host with no services keeps no spread"
    );
}

#[test]
fn a_host_with_no_services_has_nothing_to_spread() {
    let mut view = printing_view(instant());
    assert!(!view.steer(&Steer::ShowServices(host(3)), NOW));
    assert!(!view.is_spread(&host(3)));
}

#[test]
fn services_fan_away_from_this_machine_on_arcs_that_never_crowd() {
    // A plate and its halo's whole square, so two never touch.
    let footprint = f64::from(2 * (NODE_RADIUS + NODE_RADIUS / 5 + 1));
    let mut offsets = Vec::new();
    for outward in [0.0, 1.0, -2.5, FRAC_PI_2] {
        for count in 1..48 {
            spread_offsets(count, outward, Scale::ONE, &mut offsets);
            assert_eq!(offsets.len(), count);
            for (i, &(ax, ay)) in offsets.iter().enumerate() {
                let (x, y) = (f64::from(ax), f64::from(ay));
                let towards =
                    (x * mathf::cos(outward) + y * mathf::sin(outward)) / mathf::hypot(x, y);
                assert!(
                    towards > mathf::cos(0.76 * core::f64::consts::PI),
                    "{count} services: #{i} faces into the line that reaches the host"
                );
                for &(bx, by) in &offsets[i + 1..] {
                    let apart = mathf::hypot(x - f64::from(bx), y - f64::from(by));
                    assert!(apart >= footprint, "{count} services crowd: {apart}");
                }
            }
        }
    }
}

#[test]
fn a_host_with_services_wears_their_count_on_its_shoulder() {
    let shoulder_pixels = |map: Map| {
        let mut view = MapView::new(CANVAS, instant());
        view.set_map(map);
        let surface = render(&mut view, NOW);
        let centre = window_point(&view, &host(2));
        let offset = i32::try_from(NODE_RADIUS * 7 / 10).unwrap();
        (-3..=3)
            .flat_map(|dy| (-3..=3).map(move |dx| (dx, dy)))
            .map(|(dx, dy)| {
                let x = u32::try_from(centre.x + offset + dx).unwrap();
                let y = u32::try_from(centre.y - offset + dy).unwrap();
                surface.get(x, y).unwrap()
            })
            .collect::<Vec<_>>()
    };
    let themes = ThemeRegistry::with_builtins();
    let accent = Color::from(themes.active().palette().accent).premultiply();
    assert!(
        shoulder_pixels(printing()).contains(&accent),
        "the count's pill is drawn"
    );
    assert!(
        !shoulder_pixels(sample()).contains(&accent),
        "a host offering nothing wears no count"
    );
}

#[test]
fn a_spread_service_and_the_line_to_it_wear_its_hosts_hue() {
    let mut map = printing();
    // Unnamed, so no label is drawn across the line.
    map.set_label(map.id_for(&host(2)).unwrap(), "");
    let mut view = MapView::new(CANVAS, instant());
    view.set_map(map);
    let printer = window_point(&view, &host(2));
    let id = view.map().id_for(&service("Office._ipp._tcp")).unwrap();
    assert_eq!(view.hue(view.map().node(id).unwrap()), BadgeHue::Purple);
    click(&mut view, printer);
    let surface = render(&mut view, NOW);
    let out = services_out(&mut view, NOW);
    let (_, at) = out
        .iter()
        .find(|(key, _)| *key == service("Office._ipp._tcp"))
        .unwrap();
    // Halfway out is past the host's plate and short of the service's.
    let x = printer.x + (at.x - printer.x) / 2;
    let y = printer.y + (at.y - printer.y) / 2;
    let mid = surface
        .get(u32::try_from(x).unwrap(), u32::try_from(y).unwrap())
        .unwrap();
    let want = BadgeHue::Purple.middle();
    assert!(
        mid.r.abs_diff(want.r) <= 8 && mid.g.abs_diff(want.g) <= 8 && mid.b.abs_diff(want.b) <= 8,
        "the line is drawn in its host's hue: {mid:?}"
    );
}

#[test]
fn a_hosts_tip_names_it_and_what_it_offers() {
    let mut view = printing_view(instant());
    let printer = window_point(&view, &host(2));
    input(&mut view, &InputEvent::PointerMoved { to: printer }, NOW);
    let (region, line) = view.tip(Scale::ONE, NOW).expect("a tip");
    assert!(region.contains(printer));
    assert_eq!(line, "host2 — 3 services: http (2), ipp");

    let at = window_point(&view, &host(3));

    input(&mut view, &InputEvent::PointerMoved { to: at }, NOW);
    assert_eq!(view.tip(Scale::ONE, NOW).unwrap().1, "host3");
    input(
        &mut view,
        &InputEvent::PointerMoved {
            to: Point::new(799, 1),
        },
        NOW,
    );
    assert!(
        view.tip(Scale::ONE, NOW).is_none(),
        "nothing under the pointer"
    );
}

#[test]
fn a_spread_services_tip_names_its_type_and_port() {
    let mut view = printing_view(instant());
    let at = window_point(&view, &host(2));
    click(&mut view, at);
    let (key, at) = services_out(&mut view, NOW)
        .into_iter()
        .find(|(key, _)| *key == service("Status._http._tcp"))
        .unwrap();
    input(&mut view, &InputEvent::PointerMoved { to: at }, NOW);
    assert_eq!(
        view.tip(Scale::ONE, NOW).unwrap().1,
        "Status — http, port 8080",
        "{key:?}"
    );
}

#[test]
fn a_long_offering_keeps_whole_types_within_the_tooltips_bound() {
    let mut map = printing();
    let printer = map.id_for(&host(2)).unwrap();
    for i in 0..24 {
        let key = service(&alloc::format!("s{i}._kind{i:02}-type._tcp"));
        let id = map.intern(key, NodeKind::Service, 0).unwrap();
        map.set_endpoint(
            id,
            Endpoint {
                service_type: alloc::format!("_kind{i:02}-type._tcp"),
                port: 1000,
                ..Endpoint::default()
            },
        );
        map.link(printer, id, EdgeKind::Serves);
    }
    layout(&mut map);
    let mut view = MapView::new(CANVAS, instant());
    view.set_map(map);
    let at = window_point(&view, &host(2));
    input(&mut view, &InputEvent::PointerMoved { to: at }, NOW);
    let line = view.tip(Scale::ONE, NOW).unwrap().1;
    assert!(line.len() <= TOOLTIP_TEXT_MAX, "{line}");
    assert!(line.starts_with("host2 — 27 services: "), "{line}");
    let (_, more) = line.rsplit_once(", and ").expect("a count of the rest");
    assert!(more.ends_with(" more"), "{line}");
    assert!(
        tairix_abi::window_ipc::TooltipText::new(&line).is_ok(),
        "the wire carries it"
    );
}

#[test]
fn no_tip_is_offered_while_a_spread_moves_or_a_drag_is_under_way() {
    let mut view = printing_view(paced());
    let printer = window_point(&view, &host(2));
    click_at(&mut view, printer, NOW);
    assert!(view.tip(Scale::ONE, NOW).is_none(), "a plate is moving");
    let span = panel_open_ns();
    render(&mut view, NOW + span);
    assert!(
        view.tip(Scale::ONE, NOW + span).is_some(),
        "and at rest it has one"
    );
    input(
        &mut view,
        &InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        NOW + span,
    );
    assert!(
        view.tip(Scale::ONE, NOW + span).is_none(),
        "a press is under way"
    );
}

fn menu_of(reaction: Reaction) -> Box<NodeMenu> {
    match reaction {
        Reaction::Menu(Ok(menu)) => menu,
        other => panic!("no menu: {other:?}"),
    }
}

fn row_labels(menu: &NodeMenu) -> Vec<String> {
    menu.model()
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some(item.label.to_string()),
            _ => None,
        })
        .collect()
}

fn secondary_press(view: &mut MapView, at: Point) -> Box<NodeMenu> {
    input(view, &InputEvent::PointerMoved { to: at }, NOW);
    menu_of(input(
        view,
        &InputEvent::PointerPressed {
            button: PointerButton::Secondary,
        },
        NOW,
    ))
}

#[test]
fn a_secondary_press_asks_for_the_menu_of_what_it_lands_on() {
    let mut view = printing_view(instant());
    let printer = window_point(&view, &host(2));
    let menu = secondary_press(&mut view, printer);
    assert_eq!(menu.anchor(), printer);
    assert_eq!(menu.model().title(), "host2");
    assert_eq!(
        row_labels(&menu),
        [
            "Show Services",
            "Copy Address",
            "Copy Name",
            "Fit Map to Window"
        ]
    );

    click(&mut view, printer);
    let spread = secondary_press(&mut view, printer);
    assert_eq!(row_labels(&spread)[0], "Hide Services");

    let canvas = secondary_press(&mut view, Point::new(799, 599));
    assert_eq!(canvas.model().title(), "Network Map");
    assert_eq!(row_labels(&canvas), ["Fit Map to Window"]);
    assert!(
        view.is_spread(&host(2)),
        "asking for a menu changes nothing"
    );
}

#[test]
fn what_a_menu_row_asks_is_what_the_view_does() {
    let mut view = printing_view(instant());
    let printer = window_point(&view, &host(2));
    let menu = secondary_press(&mut view, printer);
    let command = |label: &str| {
        let (row, _) = menu
            .model()
            .rows()
            .find(|(row, _)| matches!(row, AppMenuRowView::Item(item) if item.label == label))
            .unwrap();
        let AppMenuRowView::Item(item) = row else {
            unreachable!()
        };
        menu.command(item.id).unwrap().clone()
    };
    let Command::Steer(show) = command("Show Services") else {
        panic!("showing is the view's");
    };
    assert!(view.steer(&show, NOW));
    assert!(view.is_spread(&host(2)));
    assert_eq!(
        command("Copy Address"),
        Command::Copy(String::from("10.0.0.2"))
    );

    input(
        &mut view,
        &InputEvent::PointerScrolled { dx: 0, dy: 600 },
        NOW,
    );
    let Command::Steer(fit) = command("Fit Map to Window") else {
        panic!("fitting is the view's");
    };
    assert!(view.steer(&fit, NOW), "the scrolled map is fitted again");
    assert!(!view.steer(&fit, NOW), "and is already fitted");
}

#[test]
fn rendering_draws_the_map_and_a_selected_nodes_panel() {
    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let mut view = view();
    let surface = render(&mut view, NOW);
    let background = Color::from(theme.palette().surface).premultiply();
    let centre = window_point(&view, &host(2));
    let x = u32::try_from(centre.x).unwrap();
    let y = u32::try_from(centre.y).unwrap();
    assert_ne!(surface.get(x, y).unwrap(), background, "a node is drawn");

    let corner = (CANVAS.width - 40, 40);
    let empty = surface.get(corner.0, corner.1).unwrap();
    click(&mut view, centre);
    let surface = render(&mut view, NOW);
    assert_ne!(
        surface.get(corner.0, corner.1).unwrap(),
        empty,
        "the detail panel is drawn"
    );
}

#[test]
fn the_panel_states_a_typed_hardware_address_and_where_a_service_is() {
    let mut map = printing();
    let printer = map.id_for(&host(2)).unwrap();
    map.set_mac(printer, [0x02, 0, 0, 0, 0, 7]);
    let facts = node_facts(map.node(printer).unwrap());
    let stated = |label: &str| {
        facts
            .iter()
            .find(|fact| fact.label() == label)
            .map(|fact| fact.value().to_string())
    };
    assert_eq!(
        stated("Hardware address").as_deref(),
        Some("02:00:00:00:00:07 (locally administered)")
    );

    let queue = map.id_for(&service("Office._ipp._tcp")).unwrap();
    let facts = node_facts(map.node(queue).unwrap());
    let stated = |label: &str| {
        facts
            .iter()
            .find(|fact| fact.label() == label)
            .map(|fact| fact.value().to_string())
    };
    assert_eq!(stated("Service type").as_deref(), Some("_ipp._tcp"));
    assert_eq!(stated("Port").as_deref(), Some("631"));
    assert_eq!(stated("Host").as_deref(), Some("printer.local"));
    assert_eq!(stated("Address").as_deref(), Some("10.0.0.2"));
}

#[test]
fn a_family_with_an_emblem_is_marked_with_it() {
    use crate::model::OsFamily;
    assert_eq!(os_emblem(OsFamily::Linux), Some(IconKind::OsLinux));
    assert_eq!(os_emblem(OsFamily::Android), Some(IconKind::OsAndroid));
    assert_eq!(os_emblem(OsFamily::Windows), Some(IconKind::OsWindows));
    assert_eq!(os_emblem(OsFamily::MacOs), Some(IconKind::OsApple));
    assert_eq!(os_emblem(OsFamily::Ios), Some(IconKind::OsApple));
    for none in [OsFamily::Unknown, OsFamily::Tairix, OsFamily::NetworkOs] {
        assert_eq!(os_emblem(none), None, "{none:?}");
    }
}

#[test]
fn a_hosts_system_emblem_is_drawn_on_its_shoulder() {
    let shoulder = |os| {
        let mut map = sample();
        let id = map.id_for(&host(2)).unwrap();
        map.set_classification(
            id,
            crate::model::Classification {
                os,
                os_confidence: 60,
                ..crate::model::Classification::default()
            },
            Vec::new(),
        );
        let mut view = MapView::new(CANVAS, instant());
        view.set_map(map);
        let surface = render(&mut view, NOW);
        let centre = window_point(&view, &host(2));
        let offset = i32::try_from(NODE_RADIUS * 7 / 10).unwrap();
        let mut drawn = Vec::new();
        for dy in -4..=4 {
            for dx in -4..=4 {
                let x = u32::try_from(centre.x - offset + dx).unwrap();
                let y = u32::try_from(centre.y + offset + dy).unwrap();
                drawn.push(surface.get(x, y).unwrap());
            }
        }
        drawn
    };
    let plain = shoulder(crate::model::OsFamily::Unknown);
    let linux = shoulder(crate::model::OsFamily::Linux);
    let apple = shoulder(crate::model::OsFamily::MacOs);
    assert_ne!(plain, linux, "the emblem is drawn");
    assert_ne!(linux, apple, "each family is its own emblem");
}

/// A map that spreads wider than the canvas: this machine, its interface, a
/// network, and the hosts on it, each a ring further out.
fn wide() -> Map {
    let mut map = Map::new();
    let local = map
        .intern(NodeKey::LocalHost, NodeKind::LocalHost, 0)
        .unwrap();
    let mut eth0 = [0u8; tairix_abi::net_ipc::IF_NAME_LEN];
    eth0[..4].copy_from_slice(b"eth0");
    let iface = map
        .intern(NodeKey::Interface(eth0), NodeKind::Interface, 0)
        .unwrap();
    map.link(local, iface, EdgeKind::Hosts);
    let subnet = map
        .intern(
            NodeKey::Subnet {
                iface: eth0,
                network: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 0)),
                prefix: 24,
            },
            NodeKind::Subnet,
            0,
        )
        .unwrap();
    map.link(iface, subnet, EdgeKind::OnLink);
    for n in 2..40 {
        let id = map.intern(host(n), NodeKind::Neighbour, 0).unwrap();
        map.link(subnet, id, EdgeKind::OnLink);
    }
    layout(&mut map);
    map
}

fn all_in_sight(view: &MapView) -> bool {
    view.map().nodes().iter().all(|node| {
        let at = view.viewport().to_window(node.pos);
        CANVAS.contains(at)
    })
}

#[test]
fn a_fresh_view_keeps_the_whole_map_in_sight_until_it_is_steered() {
    let mut view = MapView::new(CANVAS, instant());
    view.set_map(wide());
    assert!(all_in_sight(&view), "the first map is fitted");
    view.resize(Rect::new(0, 0, 400, 300));
    assert!(
        view.map().nodes().iter().all(|node| {
            Rect::new(0, 0, 400, 300).contains(view.viewport().to_window(node.pos))
        }),
        "and refitted as the window changes"
    );
    view.resize(CANVAS);

    input(
        &mut view,
        &InputEvent::PointerScrolled { dx: 0, dy: 240 },
        NOW,
    );
    let steered = (view.viewport().zoom(), view.viewport().center());
    view.set_map(wide());
    assert_eq!(
        (view.viewport().zoom(), view.viewport().center()),
        steered,
        "a refresh leaves a steered view where the user put it"
    );

    let home = InputEvent::KeyPressed {
        key: Key::Named(NamedKey::Home),
        modifiers: Modifiers::default(),
    };
    assert!(
        repaints(&input(&mut view, &home, NOW)),
        "Home brings it all back"
    );
    assert!(all_in_sight(&view));
    assert!(!repaints(&input(&mut view, &home, NOW)), "already in sight");
}

#[test]
fn a_claim_succeeds_only_where_nothing_is_yet() {
    let mut occupancy = Occupancy::default();
    occupancy.reset(Rect::new(10, 10, 100, 100));
    assert!(occupancy.claim(Rect::new(10, 10, 30, 10)));
    assert!(
        !occupancy.claim(Rect::new(30, 15, 30, 10)),
        "it overlaps the first"
    );
    assert!(
        occupancy.claim(Rect::new(60, 10, 30, 10)),
        "beside it is free"
    );
    occupancy.mark(Rect::new(10, 60, 100, 10));
    assert!(
        !occupancy.claim(Rect::new(40, 62, 5, 5)),
        "a mark is a claim too"
    );
    assert!(
        occupancy.claim(Rect::new(-500, -500, 20, 20)),
        "off the canvas costs nothing"
    );
    assert!(
        occupancy.claim(Rect::new(105, 105, 40, 40)),
        "clipped to what is on it"
    );
    assert!(!occupancy.claim(Rect::new(108, 108, 1, 1)));
}

#[test]
fn a_zoomed_out_map_still_names_what_has_room() {
    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let font = BitmapFont::for_role(theme.fonts(), tairix_theme::TextRole::Body, Scale::ONE);
    let mut map = wide();
    let local = map.id_for(&NodeKey::LocalHost).unwrap();
    map.set_label(local, "This machine");
    let mut view = MapView::new(CANVAS, instant());
    view.set_map(map);
    assert!(view.viewport().zoom() < 600, "the fit zooms out");
    let surface = render(&mut view, NOW);
    let centre = window_point(&view, &NodeKey::LocalHost);
    let label = Label::plan(
        font,
        centre,
        Scale::ONE.scale_length(NODE_RADIUS),
        "This machine",
        Scale::ONE,
    )
    .unwrap();
    let background = Color::from(theme.palette().surface).premultiply();
    let inked = (0..label.rect.width).any(|dx| {
        (0..label.rect.height).any(|dy| {
            let x = u32::try_from(label.rect.left()).unwrap() + dx;
            let y = u32::try_from(label.rect.top()).unwrap() + dy;
            surface.get(x, y).unwrap() != background
        })
    });
    assert!(inked, "the name is drawn");
}

#[test]
fn a_nodes_own_plate_never_keeps_its_name_off_the_map() {
    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let font = BitmapFont::for_role(theme.fonts(), tairix_theme::TextRole::Body, Scale::ONE);
    let radius = Scale::ONE.scale_length(NODE_RADIUS);
    for step in 0..64 {
        let mut occupancy = Occupancy::default();
        occupancy.reset(CANVAS);
        let centre = Point::new(200 + step, 200 + step);
        occupancy.mark(plate_rect(centre, radius));
        let label = Label::plan(font, centre, radius, "printer.local", Scale::ONE).unwrap();
        assert!(occupancy.claim(label.rect), "at {centre:?}");
    }
}

#[test]
fn the_detail_panel_covers_the_map_under_it() {
    let mut view = printing_view(instant());
    let printer = window_point(&view, &host(2));
    click(&mut view, printer);
    render(&mut view, NOW);
    let panel = view.panel.expect("the panel is drawn");
    let inside = Point::new(panel.left() + 8, panel.top() + 8);

    input(&mut view, &InputEvent::PointerMoved { to: inside }, NOW);
    assert!(
        view.tip(Scale::ONE, NOW).is_none(),
        "no tip for what it hides"
    );
    assert!(!click(&mut view, inside), "a click on it changes nothing");
    assert_eq!(view.selected().map(|n| n.key.clone()), Some(host(2)));

    let menu = secondary_press(&mut view, inside);
    assert_eq!(
        menu.model().title(),
        "host2",
        "its menu is the node it describes"
    );

    click(&mut view, Point::new(1, 1));
    assert!(view.panel.is_none(), "the panel goes with the selection");
}

/// This machine, a network, and a ring of named hosts on it, the fifth of
/// them offering services, so a spread fans out among its neighbours' names.
fn crowded() -> Map {
    let mut map = Map::new();
    let local = map
        .intern(NodeKey::LocalHost, NodeKind::LocalHost, 0)
        .unwrap();
    map.set_label(local, "This machine");
    let subnet = map
        .intern(
            NodeKey::Subnet {
                iface: [0; tairix_abi::net_ipc::IF_NAME_LEN],
                network: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 0)),
                prefix: 24,
            },
            NodeKind::Subnet,
            0,
        )
        .unwrap();
    map.set_label(subnet, "10.0.0.0/24");
    map.link(local, subnet, EdgeKind::OnLink);
    for n in 2..14 {
        let id = map.intern(host(n), NodeKind::Neighbour, 0).unwrap();
        map.set_label(id, &alloc::format!("neighbour-{n}.example.org"));
        map.link(subnet, id, EdgeKind::OnLink);
        if n == 5 {
            for name in ["Files._smb._tcp", "Web._http._tcp", "Shell._ssh._tcp"] {
                let service = map.intern(service(name), NodeKind::Service, 0).unwrap();
                let (instance, kind) = name.split_once('.').unwrap();
                map.set_label(service, &alloc::format!("{instance} on neighbour-5"));
                map.set_endpoint(
                    service,
                    Endpoint {
                        service_type: String::from(kind),
                        port: 1,
                        ..Endpoint::default()
                    },
                );
                map.link(id, service, EdgeKind::Serves);
            }
        }
    }
    layout(&mut map);
    map
}

#[test]
fn a_frame_that_only_moves_services_changes_nothing_outside_its_damage() {
    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let mut view = MapView::new(CANVAS, paced());
    view.set_map(crowded());
    let at = window_point(&view, &host(5));
    click(&mut view, at);
    let span = panel_open_ns();
    let earlier = render(&mut view, NOW + span / 3);
    let damage = view
        .frame_damage(Scale::ONE, theme)
        .expect("a moving spread has a damage");
    let later = render(&mut view, NOW + 2 * span / 3);
    let mut changed = 0;
    for y in 0..CANVAS.height {
        for x in 0..CANVAS.width {
            if earlier.get(x, y) != later.get(x, y) {
                changed += 1;
                let at = Point::new(i32::try_from(x).unwrap(), i32::try_from(y).unwrap());
                assert!(damage.contains(at), "{at:?} changed outside {damage:?}");
            }
        }
    }
    assert!(changed > 0, "the frames differ");

    // A frame drawn only inside its damage over the one before it is the
    // whole frame drawn afresh.
    let mut scoped = earlier;
    let (x, y) = damage.surface_origin().unwrap();
    scoped.with_clip(x, y, damage.width, damage.height, |surface| {
        view.render(surface, Scale::ONE, theme, NOW + 2 * span / 3);
    });
    for y in 0..CANVAS.height {
        for x in 0..CANVAS.width {
            assert_eq!(scoped.get(x, y), later.get(x, y), "at ({x}, {y})");
        }
    }
}

#[test]
fn a_map_at_rest_has_no_frame_damage() {
    let themes = ThemeRegistry::with_builtins();
    let mut view = MapView::new(CANVAS, paced());
    view.set_map(crowded());
    assert_eq!(view.frame_damage(Scale::ONE, themes.active()), None);
    let at = window_point(&view, &host(5));
    click(&mut view, at);
    render(&mut view, NOW + panel_open_ns());
    assert_eq!(
        view.frame_damage(Scale::ONE, themes.active()),
        None,
        "a settled spread moves nothing"
    );
}

/// A host straight below this machine offering one service, which spreads
/// further down, and beside it a neighbour whose long name runs across the
/// service's path and far past it: placed by hand, at one-to-one zoom, so
/// the service crosses the name partway through its travel.
fn beside_a_name() -> Map {
    let mut map = Map::new();
    let local = map
        .intern(NodeKey::LocalHost, NodeKind::LocalHost, 0)
        .unwrap();
    let spreading = map.intern(host(2), NodeKind::Neighbour, 0).unwrap();
    let named = map.intern(host(3), NodeKind::Neighbour, 0).unwrap();
    map.set_label(named, "a-neighbour-with-a-long-name.example.org");
    let shared = map
        .intern(service("Web._http._tcp"), NodeKind::Service, 0)
        .unwrap();
    map.set_label(shared, "Web");
    map.link(local, spreading, EdgeKind::OnLink);
    map.link(local, named, EdgeKind::OnLink);
    map.link(spreading, shared, EdgeKind::Serves);
    map.set_pos(spreading, Point::new(0, 200));
    map.set_pos(shared, Point::new(0, 200));
    map.set_pos(named, Point::new(60, 230));
    map
}

#[test]
fn a_spread_crossing_a_neighbours_name_never_reaches_past_its_damage() {
    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let mut view = MapView::new(CANVAS, paced());
    view.set_map(beside_a_name());
    assert_eq!(
        view.viewport().zoom(),
        crate::DEFAULT_ZOOM_PER_MILLE,
        "drawn one to one"
    );
    let at = window_point(&view, &host(2));
    click(&mut view, at);
    let span = panel_open_ns();
    let earlier = render(&mut view, NOW + span / 3);
    let damage = view.frame_damage(Scale::ONE, theme).expect("it moves");
    let later = render(&mut view, NOW + 2 * span / 3);
    let outside = (0..CANVAS.height)
        .flat_map(|y| (0..CANVAS.width).map(move |x| (x, y)))
        .filter(|&(x, y)| earlier.get(x, y) != later.get(x, y))
        .map(|(x, y)| Point::new(i32::try_from(x).unwrap(), i32::try_from(y).unwrap()))
        .find(|at| !damage.contains(*at));
    assert_eq!(outside, None, "a pixel changed outside {damage:?}");
}
