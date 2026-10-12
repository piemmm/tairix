use super::*;
use core::net::{IpAddr, Ipv4Addr};

use tairix_input::Modifiers;
use tairix_theme::ThemeRegistry;

use crate::layout;
use crate::model::{EdgeKind, Map, NodeKey, NodeKind};

const CANVAS: Rect = Rect::new(0, 0, 800, 600);

fn host(n: u8) -> NodeKey {
    NodeKey::Neighbour(IpAddr::V4(Ipv4Addr::new(10, 0, 0, n)))
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

fn view() -> MapView {
    let mut view = MapView::new(CANVAS);
    view.set_map(sample());
    view
}

fn window_point(view: &MapView, key: &NodeKey) -> Point {
    let node = view.map().node(view.map().id_for(key).unwrap()).unwrap();
    view.viewport().to_window(node.pos)
}

fn click(view: &mut MapView, at: Point) -> bool {
    view.on_input(&InputEvent::PointerMoved { to: at }, Scale::ONE);
    view.on_input(
        &InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        Scale::ONE,
    );
    view.on_input(
        &InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
        Scale::ONE,
    )
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
    view.on_input(&InputEvent::PointerMoved { to: at }, Scale::ONE);
    view.on_input(
        &InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        Scale::ONE,
    );
    assert!(view.on_input(
        &InputEvent::PointerMoved {
            to: Point::new(at.x + 60, at.y),
        },
        Scale::ONE
    ));
    view.on_input(
        &InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
        Scale::ONE,
    );
    let dragged = i32::try_from(60 * 1000 / view.viewport().zoom()).unwrap();
    assert_eq!(view.viewport().center().x, before.x - dragged);
    assert!(view.selected().is_none());
}

#[test]
fn a_two_finger_scroll_pans_both_axes_and_ctrl_wheel_zooms_about_the_pointer() {
    let mut view = view();
    let before = view.viewport().center();
    assert!(view.on_input(
        &InputEvent::PointerScrolled { dx: 120, dy: 240 },
        Scale::ONE
    ));
    let after = view.viewport().center();
    assert!(after.x > before.x && after.y > before.y);

    let anchor = Point::new(500, 200);
    view.on_input(&InputEvent::PointerMoved { to: anchor }, Scale::ONE);
    let under = view.viewport().to_graph(anchor);
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    view.on_input(
        &InputEvent::ModifiersChanged { modifiers: ctrl },
        Scale::ONE,
    );
    let zoom = view.viewport().zoom();
    assert!(view.on_input(&InputEvent::PointerScrolled { dx: 0, dy: -120 }, Scale::ONE));
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
    view.on_input(&pinch(PinchPhase::Begin, PINCH_SCALE_ONE), Scale::ONE);
    view.on_input(&pinch(PinchPhase::Update, PINCH_SCALE_ONE * 2), Scale::ONE);
    assert_eq!(view.viewport().zoom(), start * 2);
    // Each update is relative to the zoom the pinch began at, not the last.
    view.on_input(
        &pinch(PinchPhase::Update, PINCH_SCALE_ONE * 3 / 2),
        Scale::ONE,
    );
    assert_eq!(view.viewport().zoom(), start * 3 / 2);
    view.on_input(&pinch(PinchPhase::Cancel, PINCH_SCALE_ONE * 4), Scale::ONE);
    assert_eq!(view.viewport().zoom(), start);
    // An update with no pinch begun does nothing.
    assert!(!view.on_input(&pinch(PinchPhase::Update, PINCH_SCALE_ONE * 2), Scale::ONE));
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
fn a_service_and_the_edge_to_it_wear_its_hosts_hue() {
    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let font = BitmapFont::for_role(theme.fonts(), tairix_theme::TextRole::Body, Scale::ONE);
    let mut map = Map::new();
    let local = map
        .intern(NodeKey::LocalHost, NodeKind::LocalHost, 0)
        .unwrap();
    let printer = map.intern(host(2), NodeKind::Neighbour, 0).unwrap();
    map.set_classification(
        printer,
        crate::model::Classification {
            device: DeviceClass::Printer,
            device_confidence: 80,
            ..crate::model::Classification::default()
        },
        Vec::new(),
    );
    let service = map
        .intern(
            NodeKey::Service(String::from("Office._ipp._tcp")),
            NodeKind::Service,
            0,
        )
        .unwrap();
    map.link(local, printer, EdgeKind::OnLink);
    map.link(printer, service, EdgeKind::Serves);
    layout(&mut map);
    let mut view = MapView::new(CANVAS);
    view.set_map(map);
    assert_eq!(
        view.hue(view.map().node(service).unwrap()),
        BadgeHue::Purple
    );

    let mut surface = Surface::new(CANVAS.width, CANVAS.height).unwrap();
    view.render(&mut surface, Scale::ONE, theme, font);
    let a = view
        .viewport()
        .to_window(view.map().node(printer).unwrap().pos);
    let b = view
        .viewport()
        .to_window(view.map().node(service).unwrap().pos);
    let mid = surface
        .get(
            u32::try_from(a.x.midpoint(b.x)).unwrap(),
            u32::try_from(a.y.midpoint(b.y)).unwrap(),
        )
        .unwrap();
    let want = BadgeHue::Purple.middle();
    assert!(
        mid.r.abs_diff(want.r) <= 8 && mid.g.abs_diff(want.g) <= 8 && mid.b.abs_diff(want.b) <= 8,
        "the edge is drawn in its host's hue: {mid:?}"
    );
}

#[test]
fn rendering_draws_the_map_and_a_selected_nodes_panel() {
    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let font = BitmapFont::for_role(theme.fonts(), tairix_theme::TextRole::Body, Scale::ONE);
    let mut view = view();
    let mut surface = Surface::new(CANVAS.width, CANVAS.height).unwrap();
    view.render(&mut surface, Scale::ONE, theme, font);
    let background = Color::from(theme.palette().surface).premultiply();
    let centre = window_point(&view, &host(2));
    let x = u32::try_from(centre.x).unwrap();
    let y = u32::try_from(centre.y).unwrap();
    assert_ne!(surface.get(x, y).unwrap(), background, "a node is drawn");

    let corner = (CANVAS.width - 40, 40);
    let empty = surface.get(corner.0, corner.1).unwrap();
    click(&mut view, centre);
    view.render(&mut surface, Scale::ONE, theme, font);
    assert_ne!(
        surface.get(corner.0, corner.1).unwrap(),
        empty,
        "the detail panel is drawn"
    );
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
    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let font = BitmapFont::for_role(theme.fonts(), tairix_theme::TextRole::Body, Scale::ONE);
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
        let mut view = MapView::new(CANVAS);
        view.set_map(map);
        let mut surface = Surface::new(CANVAS.width, CANVAS.height).unwrap();
        view.render(&mut surface, Scale::ONE, theme, font);
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
    let mut view = MapView::new(CANVAS);
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

    view.on_input(&InputEvent::PointerScrolled { dx: 0, dy: 240 }, Scale::ONE);
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
    assert!(view.on_input(&home, Scale::ONE), "Home brings it all back");
    assert!(all_in_sight(&view));
    assert!(!view.on_input(&home, Scale::ONE), "already in sight");
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
    let mut view = MapView::new(CANVAS);
    view.set_map(map);
    assert!(view.viewport().zoom() < 600, "the fit zooms out");
    let mut surface = Surface::new(CANVAS.width, CANVAS.height).unwrap();
    view.render(&mut surface, Scale::ONE, theme, font);
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
