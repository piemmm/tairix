use super::*;
use alloc::string::ToString;
use core::net::{Ipv4Addr, Ipv6Addr};

use tairix_abi::window_ipc::AppMenuRowView;

use crate::model::Map;

fn node(map: &mut Map, key: NodeKey, kind: NodeKind, label: &str, addrs: &[IpAddr]) -> Node {
    let id = map.intern(key, kind, 0).unwrap();
    map.set_label(id, label);
    for addr in addrs {
        map.add_addr(id, *addr);
    }
    map.node(id).unwrap().clone()
}

fn v4(d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(192, 168, 1, d))
}

fn global_v6() -> IpAddr {
    IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 7))
}

fn link_v6() -> IpAddr {
    IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 7))
}

/// Each row's label and the command behind it, in order.
fn rows(menu: &NodeMenu) -> Vec<(String, Command)> {
    menu.model()
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => {
                Some((item.label.to_string(), menu.command(item.id)?.clone()))
            }
            _ => None,
        })
        .collect()
}

fn labels(menu: &NodeMenu) -> Vec<String> {
    rows(menu).into_iter().map(|(label, _)| label).collect()
}

fn copied(menu: &NodeMenu, label: &str) -> Option<String> {
    rows(menu)
        .into_iter()
        .find_map(|(row, command)| match command {
            Command::Copy(text) if row == label => Some(text),
            _ => None,
        })
}

/// A node offering no services.
fn subject(node: &Node) -> Subject<'_> {
    Subject {
        node,
        services: 0,
        spread: false,
    }
}

fn web(port: u16, kind: &str, host: &str, text: &[&str]) -> Endpoint {
    Endpoint {
        service_type: String::from(kind),
        port,
        host: String::from(host),
        text: text.iter().map(|entry| String::from(*entry)).collect(),
    }
}

fn service(endpoint: Endpoint, addrs: &[IpAddr]) -> Node {
    let mut map = Map::new();
    let mut node = node(
        &mut map,
        NodeKey::Service(String::from("Console._http._tcp")),
        NodeKind::Service,
        "Console",
        addrs,
    );
    node.endpoint = Some(endpoint);
    node
}

#[test]
fn a_host_offering_services_can_spread_them_and_names_how_to_reach_it() {
    let mut map = Map::new();
    let mut printer = node(
        &mut map,
        NodeKey::Neighbour(v4(20)),
        NodeKind::Neighbour,
        "printer.local",
        &[v4(20)],
    );
    printer.mac = Some([0x00, 0x80, 0x77, 1, 2, 3]);
    let folded = menu(
        Some(Subject {
            node: &printer,
            services: 4,
            spread: false,
        }),
        Point::new(5, 6),
    )
    .unwrap();
    assert_eq!(folded.anchor(), Point::new(5, 6));
    assert_eq!(folded.model().title(), "printer.local");
    assert_eq!(
        rows(&folded),
        [
            (
                String::from("Show Services"),
                Command::Steer(Steer::ShowServices(printer.key.clone()))
            ),
            (
                String::from("Copy Address"),
                Command::Copy(String::from("192.168.1.20"))
            ),
            (
                String::from("Copy Name"),
                Command::Copy(String::from("printer.local"))
            ),
            (
                String::from("Copy Hardware Address"),
                Command::Copy(String::from("00:80:77:01:02:03"))
            ),
            (
                String::from("Fit Map to Window"),
                Command::Steer(Steer::Fit)
            ),
            (String::from("Probe Settings…"), Command::OpenSettings),
        ]
    );
    let separators = folded
        .model()
        .rows()
        .filter(|(row, _)| matches!(row, AppMenuRowView::Separator))
        .count();
    assert_eq!(separators, 2, "one rule between each group");

    let spread = menu(
        Some(Subject {
            node: &printer,
            services: 4,
            spread: true,
        }),
        Point::ORIGIN,
    )
    .unwrap();
    assert_eq!(
        rows(&spread)[0].1,
        Command::Steer(Steer::HideServices(printer.key.clone()))
    );
}

#[test]
fn an_unnamed_host_offers_no_name_and_one_address_per_family() {
    let mut map = Map::new();
    let bare = node(
        &mut map,
        NodeKey::Neighbour(v4(7)),
        NodeKind::Neighbour,
        "192.168.1.7",
        &[v4(7), link_v6(), global_v6()],
    );
    let built = menu(Some(subject(&bare)), Point::ORIGIN).unwrap();
    assert_eq!(
        labels(&built),
        [
            "Copy IPv4 Address",
            "Copy IPv6 Address",
            "Fit Map to Window",
            "Probe Settings…"
        ]
    );
    assert_eq!(
        copied(&built, "Copy IPv6 Address").as_deref(),
        Some("2001:db8::7"),
        "a routable address is preferred to a link-local one"
    );
}

#[test]
fn a_network_offers_itself_and_this_machine_offers_only_the_map() {
    let mut map = Map::new();
    let mut eth0 = [0u8; tairix_abi::net_ipc::IF_NAME_LEN];
    eth0[..4].copy_from_slice(b"eth0");
    let subnet = node(
        &mut map,
        NodeKey::Subnet {
            iface: eth0,
            network: v4(0),
            prefix: 24,
        },
        NodeKind::Subnet,
        "192.168.1.0/24",
        &[],
    );
    let built = menu(Some(subject(&subnet)), Point::ORIGIN).unwrap();
    assert_eq!(
        labels(&built),
        ["Copy Network", "Fit Map to Window", "Probe Settings…"]
    );
    assert_eq!(
        copied(&built, "Copy Network").as_deref(),
        Some("192.168.1.0/24")
    );

    let local = node(
        &mut map,
        NodeKey::LocalHost,
        NodeKind::LocalHost,
        "This machine",
        &[],
    );
    let built = menu(Some(subject(&local)), Point::ORIGIN).unwrap();
    assert_eq!(labels(&built), ["Fit Map to Window", "Probe Settings…"]);
}

#[test]
fn the_maps_own_menu_fits_it() {
    let built = menu(None, Point::new(1, 2)).unwrap();
    assert_eq!(built.model().title(), "Network Map");
    assert_eq!(
        rows(&built),
        [
            (
                String::from("Fit Map to Window"),
                Command::Steer(Steer::Fit)
            ),
            (String::from("Probe Settings…"), Command::OpenSettings),
        ]
    );
}

#[test]
fn a_web_service_offers_where_it_is_and_its_url() {
    let console = service(web(8080, "_http._tcp", "printer.local.", &[]), &[v4(20)]);
    let built = menu(Some(subject(&console)), Point::ORIGIN).unwrap();
    assert_eq!(
        labels(&built),
        [
            "Copy Address",
            "Copy URL",
            "Copy Name",
            "Fit Map to Window",
            "Probe Settings…"
        ]
    );
    assert_eq!(
        copied(&built, "Copy Address").as_deref(),
        Some("192.168.1.20:8080")
    );
    assert_eq!(
        copied(&built, "Copy URL").as_deref(),
        Some("http://192.168.1.20:8080/")
    );
}

#[test]
fn a_url_leaves_out_the_schemes_own_port_and_keeps_a_valid_path() {
    let endpoint = web(
        443,
        "_https._tcp",
        "nas.local",
        &["txtvers=1", "PATH=/admin/%7Eroot"],
    );
    let console = service(endpoint.clone(), &[v4(30)]);
    assert_eq!(
        service_url(&console, &endpoint).as_deref(),
        Some("https://192.168.1.30/admin/%7Eroot"),
        "TXT keys match without regard to case"
    );
}

#[test]
fn a_peer_authored_path_that_is_not_a_uri_path_is_dropped() {
    for path in [
        "path=relative",
        "path=/with space",
        "path=/\\007bell",
        "path=/q?x=1",
        "path=/frag#top",
        "path=/bad%zz",
        "path=/cut%2",
        "path=",
        "path",
    ] {
        let endpoint = web(80, "_http._tcp", "box.local", &[path]);
        let console = service(endpoint.clone(), &[v4(9)]);
        assert_eq!(
            service_url(&console, &endpoint).as_deref(),
            Some("http://192.168.1.9/"),
            "{path}"
        );
    }
}

#[test]
fn a_url_host_is_an_address_a_routable_v6_one_or_a_plain_name() {
    let endpoint = web(80, "_http._tcp", "box.local", &[]);
    let v6 = service(endpoint.clone(), &[link_v6(), global_v6()]);
    assert_eq!(
        service_url(&v6, &endpoint).as_deref(),
        Some("http://[2001:db8::7]/")
    );
    assert_eq!(
        service_address(&v6, &endpoint).as_deref(),
        Some("[2001:db8::7]:80")
    );

    let link_only = service(endpoint.clone(), &[link_v6()]);
    assert_eq!(
        service_url(&link_only, &endpoint).as_deref(),
        Some("http://box.local/"),
        "a link-local address needs a zone a URL cannot carry portably"
    );

    for odd in [
        "box\\032two.local",
        "-box.local",
        "box..local",
        "",
        "b_x.local",
    ] {
        let endpoint = web(80, "_http._tcp", odd, &[]);
        let unresolved = service(endpoint.clone(), &[]);
        assert_eq!(service_url(&unresolved, &endpoint), None, "{odd:?}");
        assert_eq!(service_address(&unresolved, &endpoint), None, "{odd:?}");
    }
}

#[test]
fn only_web_services_offer_a_url() {
    let endpoint = web(631, "_ipp._tcp", "printer.local", &[]);
    let queue = service(endpoint.clone(), &[v4(20)]);
    assert_eq!(service_url(&queue, &endpoint), None);
    let built = menu(Some(subject(&queue)), Point::ORIGIN).unwrap();
    assert!(!labels(&built).contains(&String::from("Copy URL")));
}

#[test]
fn an_id_the_menu_never_declared_names_no_command() {
    let built = menu(None, Point::ORIGIN).unwrap();
    assert!(built.command(AppMenuItemId::new(99).unwrap()).is_none());
}

#[test]
fn a_long_name_is_elided_to_what_a_title_carries() {
    let mut map = Map::new();
    let long = "a-very-long-host-name-that-runs-on.example.org";
    let named = node(
        &mut map,
        NodeKey::Neighbour(v4(40)),
        NodeKind::Neighbour,
        long,
        &[v4(40)],
    );
    let built = menu(Some(subject(&named)), Point::ORIGIN).unwrap();
    let title = built.model().title();
    assert!(title.len() <= APP_MENU_LABEL_MAX, "{title}");
    assert!(title.ends_with(ELLIPSIS), "{title}");
    assert!(long.starts_with(title.trim_end_matches(ELLIPSIS)));
    assert_eq!(
        copied(&built, "Copy Name").as_deref(),
        Some(long),
        "copied whole"
    );
}

#[test]
fn eliding_keeps_whole_characters() {
    assert_eq!(elide("short", 10), "short");
    let elided = elide("ééééé", 6);
    assert!(elided.len() <= 6, "{elided}");
    assert!(elided.starts_with('é') && elided.ends_with(ELLIPSIS));
    assert_eq!(elide("abcdef", 2), "", "no room even for the mark");
}

#[test]
fn a_service_types_short_name_and_txt_values_are_read_as_dns_sd_writes_them() {
    let endpoint = web(
        80,
        "_http._tcp",
        "h.local",
        &["Path=/x", "flag", "model=Box"],
    );
    assert_eq!(endpoint.short_type(), "http");
    assert_eq!(endpoint.text_value("path"), Some("/x"));
    assert_eq!(endpoint.text_value("flag"), None, "a bare key has no value");
    assert_eq!(endpoint.text_value("absent"), None);
    assert_eq!(
        Endpoint {
            service_type: String::from("odd"),
            ..Endpoint::default()
        }
        .short_type(),
        "odd"
    );
}
