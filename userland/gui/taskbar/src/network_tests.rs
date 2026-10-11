//! The network signal and readout from the reports the Switchboard publishes.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::audio::AudioGain;
use tairix_abi::net_ipc::{
    NetAddrFamily, NetAddrState, NetIfAddr, NetInterfaceStateRecord, IF_NAME_LEN, NET_IF_MAX_ADDRS,
};
use tairix_abi::switchboard_ipc::{CommandSection, NetworkReport, NETWORK_REPORT_INTERFACES_MAX};
use tairix_abi::window_ipc::AppMenuItemId;
use tairix_controls::damage::Repaint;
use tairix_controls::plate_border;

use tairix_geometry::{Point, Rect, Scale};
use tairix_input::{InputEvent, PointerButton, PointerFocus};
use tairix_theme::{Contrast, Theme};

use super::{Condition, NetworkStatus, NETWORK_SIGNAL};
use crate::input::{TaskbarInput, TaskbarResponse};
use crate::menu::MenuSubject;
use crate::network_menu::NetworkRow;
use crate::notifications::StatusKind;
use crate::repaint::TaskbarRepaint;
use crate::sound::{OutputState, SoundState};
use crate::taskbar::{Taskbar, TaskbarConfig};

fn v4(octets: [u8; 4], state: NetAddrState) -> NetIfAddr {
    let mut addr = [0u8; 16];
    addr[..4].copy_from_slice(&octets);
    NetIfAddr {
        family: NetAddrFamily::V4,
        prefix: 24,
        state,
        addr,
    }
}

fn v6(addr: [u8; 16], state: NetAddrState) -> NetIfAddr {
    NetIfAddr {
        family: NetAddrFamily::V6,
        prefix: 64,
        state,
        addr,
    }
}

const LINK_LOCAL_V6: [u8; 16] = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
const GLOBAL_V6: [u8; 16] = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
const LOOPBACK_V6: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

fn interface(name: &str, link_up: bool, addrs: &[NetIfAddr]) -> NetInterfaceStateRecord {
    let mut bytes = [0u8; IF_NAME_LEN];
    bytes[..name.len()].copy_from_slice(name.as_bytes());
    let mut slots = [NetInterfaceStateRecord::EMPTY_ADDR; NET_IF_MAX_ADDRS];
    slots[..addrs.len()].copy_from_slice(addrs);
    NetInterfaceStateRecord {
        name: bytes,
        link_up,
        addr_count: u8::try_from(addrs.len()).expect("a few"),
        addrs: slots,
    }
}

fn status(interfaces: &[NetInterfaceStateRecord]) -> NetworkStatus {
    let mut status = NetworkStatus::default();
    assert!(status.adopt(Some(NetworkReport::measured(interfaces))));
    status
}

#[test]
fn only_a_preferred_address_beyond_the_link_and_the_machine_connects_an_interface() {
    let preferred = NetAddrState::Preferred;
    let cases = [
        (
            interface("eth0", false, &[v4([10, 0, 2, 15], preferred)]),
            Condition::NoLink,
        ),
        (interface("eth0", true, &[]), Condition::NoAddress),
        (
            interface("eth0", true, &[v4([169, 254, 3, 4], preferred)]),
            Condition::NoAddress,
        ),
        (
            interface("eth0", true, &[v6(LINK_LOCAL_V6, preferred)]),
            Condition::NoAddress,
        ),
        (
            interface("eth0", true, &[v6(GLOBAL_V6, NetAddrState::Tentative)]),
            Condition::NoAddress,
        ),
        (
            interface("lo", true, &[v4([127, 0, 0, 1], preferred)]),
            Condition::NoAddress,
        ),
        (
            interface("lo", true, &[v6(LOOPBACK_V6, preferred)]),
            Condition::NoAddress,
        ),
        (
            interface("eth0", true, &[v4([10, 0, 2, 15], preferred)]),
            Condition::Connected,
        ),
        (
            interface("eth0", true, &[v6(GLOBAL_V6, preferred)]),
            Condition::Connected,
        ),
    ];
    for (interface, want) in cases {
        assert_eq!(Condition::of(&interface), want, "{interface:?}");
    }
}

#[test]
fn no_measured_interface_means_no_signal_never_an_offline_one() {
    let mut status = NetworkStatus::default();
    assert_eq!(status.signal(), None);
    assert!(status.adopt(Some(NetworkReport::UNMEASURED)));
    assert_eq!(status.signal(), None);
    assert!(status.adopt(Some(NetworkReport::measured(&[]))));
    assert_eq!(status.signal(), None);
    assert!(
        !status.adopt(Some(NetworkReport::measured(&[]))),
        "the same report"
    );
}

#[test]
fn the_signal_is_connected_while_any_interface_is() {
    let up = interface("eth0", true, &[v4([10, 0, 2, 15], NetAddrState::Preferred)]);
    let down = interface("eth1", false, &[]);
    let signal = status(&[down, up]).signal().expect("a signal");
    assert_eq!(signal.id, NETWORK_SIGNAL);
    assert_eq!(signal.kind, StatusKind::Network);
    assert_eq!(
        status(&[down]).signal().expect("a signal").kind,
        StatusKind::NetworkOffline
    );
}

#[test]
fn the_readout_states_each_interface_its_addresses_and_what_it_left_out() {
    let addrs = [
        v4([10, 0, 2, 15], NetAddrState::Preferred),
        v6(LINK_LOCAL_V6, NetAddrState::Tentative),
    ];
    let mut many: Vec<NetInterfaceStateRecord> = Vec::new();
    many.push(interface("eth0", true, &addrs));
    for _ in 1..NETWORK_REPORT_INTERFACES_MAX + 3 {
        many.push(interface("eth9", false, &[]));
    }
    let eleven = status(&many);
    let stated = rows(&eleven);
    assert_eq!(stated[0], ("eth0", "Connected"));
    assert_eq!(stated[1], ("IPv4", "10.0.2.15/24"));
    assert_eq!(stated[2], ("IPv6", "fe80::1/64 (tentative)"));
    assert_eq!(stated[3], ("eth9", "No link"));
    assert_eq!(stated.last(), Some(&("Not shown", "3 more interfaces")));

    many.truncate(NETWORK_REPORT_INTERFACES_MAX + 1);
    let nine = status(&many);
    assert_eq!(rows(&nine).last(), Some(&("Not shown", "1 more interface")));
}

/// The readout's rows as label and value.
fn rows(status: &NetworkStatus) -> Vec<(&str, &str)> {
    status
        .facts()
        .facts()
        .iter()
        .map(|fact| (fact.label(), fact.value()))
        .collect()
}

/// One Ethernet interface with a preferred IPv4 address.
pub(crate) fn connected() -> NetworkReport {
    NetworkReport::measured(&[interface(
        "eth0",
        true,
        &[v4([10, 0, 2, 15], NetAddrState::Preferred)],
    )])
}

fn bar() -> Taskbar {
    let mut bar = Taskbar::new(
        TaskbarConfig::bottom_bar(1000, 800),
        &Theme::dark().floating(),
    );
    let _ = bar.take_repaint();
    bar
}

fn move_to(input: &mut TaskbarInput, bar: &mut Taskbar, to: Point) {
    let _ = input.handle(InputEvent::PointerMoved { to }, bar, Scale::ONE, 0);
}

/// Rest the pointer on the network signal, which leads the area.
fn hover_signal(input: &mut TaskbarInput, bar: &mut Taskbar) {
    let at = centre(bar.layout(Scale::ONE).notifications[0]);
    move_to(input, bar, at);
}

fn centre(rect: Rect) -> Point {
    Point::new(
        rect.left() + i32::try_from(rect.width / 2).unwrap_or(0),
        rect.top() + i32::try_from(rect.height / 2).unwrap_or(0),
    )
}

fn kinds(bar: &Taskbar) -> Vec<StatusKind> {
    bar.notifications()
        .signals()
        .iter()
        .map(|signal| signal.kind)
        .collect()
}

#[test]
fn the_network_signal_leads_and_leaves_the_sound_signals_alone() {
    let mut bar = bar();
    let _ = bar.set_sound(SoundState {
        output: Some(OutputState {
            device_id: 3,
            name: String::from("Speakers"),
            level: AudioGain::new(-600).expect("attenuation"),
            muted: false,
            may_change: true,
        }),
        recording: false,
    });
    let _ = bar.take_repaint();

    assert!(bar.set_network(Some(connected())));
    assert_eq!(kinds(&bar), [StatusKind::Network, StatusKind::Volume]);
    let owed = bar.take_repaint();
    assert!(!owed.bar.is_clean(), "the signal is drawn on the bar");
    assert_eq!(owed.network, Repaint::Whole);

    assert!(!bar.set_network(Some(connected())), "the same report");
    assert_eq!(bar.take_repaint(), TaskbarRepaint::NONE);

    assert!(bar.set_network(None));
    assert_eq!(kinds(&bar), [StatusKind::Volume], "no publisher, no signal");
}

#[test]
fn the_readout_holds_open_all_the_way_from_the_signal_onto_itself() {
    let mut bar = bar();
    assert!(bar.set_network(Some(connected())));
    let mut input = TaskbarInput::new();
    let slot = bar.layout(Scale::ONE).notifications[0];
    assert!(bar.is_network_slot(0));

    let start = centre(slot);
    move_to(&mut input, &mut bar, start);
    let panel = bar
        .network_readout_layout(Scale::ONE)
        .expect("hovering the signal opens its readout")
        .panel;
    assert!(
        panel.bottom() <= slot.top(),
        "a bottom bar's readout opens above"
    );
    let _ = bar.take_repaint();

    // Straight up, one row at a time, across the bar's rim and into the panel.
    for y in (centre(panel).y..start.y).rev() {
        move_to(&mut input, &mut bar, Point::new(start.x, y));
        assert!(
            bar.network_readout_layout(Scale::ONE).is_some(),
            "the readout closed at y = {y} on the way to it"
        );
    }
    assert_eq!(bar.take_repaint(), TaskbarRepaint::NONE, "nothing moved");

    // Away to the far end of the bar: the readout goes.
    let library = centre(bar.layout(Scale::ONE).library);
    move_to(&mut input, &mut bar, library);
    assert!(bar.network_readout_layout(Scale::ONE).is_none());
    assert_eq!(bar.take_repaint().network, Repaint::Whole);

    // And the seat taking the pointer elsewhere takes it down too.
    move_to(&mut input, &mut bar, start);
    assert!(bar.network_readout_layout(Scale::ONE).is_some());
    input.set_pointer_focus(PointerFocus::Left, &mut bar, Scale::ONE);
    assert!(bar.network_readout_layout(Scale::ONE).is_none());
}

/// The readout's plate wears a rim, doubled under heavy contrast, inside which
/// the facts keep the control padding: a panel sized from the facts plus the
/// padding alone squeezes them by the rim and elides what they state.
#[test]
fn the_readout_fits_what_it_states_inside_its_rim() {
    for theme in [
        Theme::dark().floating(),
        crate::tests::dark_with_contrast(Contrast::High),
    ] {
        let mut bar = Taskbar::new(TaskbarConfig::bottom_bar(1000, 800), &theme);
        assert!(bar.set_network(Some(connected())));
        let mut input = TaskbarInput::new();
        hover_signal(&mut input, &mut bar);
        let facts = bar.network_facts();
        let edges = 2 * (plate_border(&theme, Scale::ONE) + theme.metrics().control_inset);
        let panel = bar
            .network_readout_layout(Scale::ONE)
            .expect("an open readout")
            .panel;
        assert_eq!(
            panel.width,
            facts.measured_width(Scale::ONE, &theme) + edges,
            "{:?}",
            theme.contrast()
        );
        assert_eq!(
            panel.height,
            facts.measured_height(Scale::ONE, &theme) + edges,
            "{:?}",
            theme.contrast()
        );
    }
}

/// A report naming more than the room above a bottom bar holds is cut to that
/// room, never laid back over the bar the readout opens from.
#[test]
fn a_readout_taller_than_the_room_beside_the_bar_stops_at_the_bar() {
    let mut bar = Taskbar::new(TaskbarConfig::bottom_bar(1000, 120), &Theme::dark());
    let addrs = [
        v4([10, 0, 2, 15], NetAddrState::Preferred),
        v6(GLOBAL_V6, NetAddrState::Preferred),
        v6(LINK_LOCAL_V6, NetAddrState::Preferred),
    ];
    let many = [
        interface("eth0", true, &addrs),
        interface("eth1", true, &addrs),
        interface("eth2", true, &addrs),
    ];
    assert!(bar.set_network(Some(NetworkReport::measured(&many))));
    let mut input = TaskbarInput::new();
    hover_signal(&mut input, &mut bar);
    let layout = bar.layout(Scale::ONE);
    let panel = bar
        .network_readout_layout(Scale::ONE)
        .expect("an open readout")
        .panel;
    assert!(
        bar.network_facts().measured_height(Scale::ONE, bar.theme()) > panel.height,
        "the report states more than the room holds"
    );
    assert!(panel.top() >= 0);
    assert!(
        panel.bottom() <= layout.bar.top(),
        "{panel:?} runs over the bar at {:?}",
        layout.bar
    );
}

/// The readout hangs from the slot it opened at. A signal arriving beside the
/// network signal moves that slot, so the readout goes rather than hanging
/// from where the signal was; the pointer's next motion opens it again.
#[test]
fn a_signal_arriving_beside_the_network_signal_takes_its_readout_down() {
    let mut bar = bar();
    assert!(bar.set_network(Some(connected())));
    let mut input = TaskbarInput::new();
    hover_signal(&mut input, &mut bar);
    let before = bar.layout(Scale::ONE).notifications[0];
    assert!(bar.network_readout_layout(Scale::ONE).is_some());
    let _ = bar.take_repaint();

    assert!(bar.set_sound(crate::sound::tests::speakers(-600, false, true)));
    assert_ne!(
        bar.layout(Scale::ONE).notifications[0],
        before,
        "the slot moved"
    );
    assert!(bar.network_readout_layout(Scale::ONE).is_none());
    assert_eq!(bar.take_repaint().network, Repaint::Whole);

    hover_signal(&mut input, &mut bar);
    assert!(bar.network_readout_layout(Scale::ONE).is_some());
}

/// A report that changes what the signal says but not where it sits keeps
/// the readout open over the same slot, stating the new facts.
#[test]
fn a_report_that_leaves_the_signal_in_place_keeps_its_readout_open() {
    let mut bar = bar();
    assert!(bar.set_network(Some(connected())));
    let mut input = TaskbarInput::new();
    hover_signal(&mut input, &mut bar);
    let open = bar.network_readout_layout(Scale::ONE).expect("open");
    let offline = NetworkReport::measured(&[interface("eth0", false, &[])]);
    assert!(bar.set_network(Some(offline)));
    let now = bar
        .network_readout_layout(Scale::ONE)
        .expect("still open")
        .panel;
    assert_eq!(
        (now.left(), now.bottom()),
        (open.panel.left(), open.panel.bottom()),
        "anchored where it was, sized to what it now states"
    );
    assert_eq!(bar.take_repaint().network, Repaint::Whole);
}

#[test]
fn losing_the_signal_takes_its_readout_down() {
    let mut bar = bar();
    assert!(bar.set_network(Some(connected())));
    let mut input = TaskbarInput::new();
    hover_signal(&mut input, &mut bar);
    assert!(bar.network_readout_layout(Scale::ONE).is_some());
    assert!(bar.set_network(Some(NetworkReport::UNMEASURED)));
    assert!(bar.network_readout_layout(Scale::ONE).is_none());
    assert_eq!(bar.take_repaint().network, Repaint::Whole);
}

#[test]
fn a_secondary_press_on_the_signal_asks_for_the_network_menu() {
    let mut bar = bar();
    assert!(bar.set_network(Some(connected())));
    let mut input = TaskbarInput::new();
    hover_signal(&mut input, &mut bar);
    let TaskbarResponse::OpenMenu(request) = input.handle(
        InputEvent::PointerPressed {
            button: PointerButton::Secondary,
        },
        &mut bar,
        Scale::ONE,
        0,
    ) else {
        panic!("the signal asks for its menu");
    };
    assert_eq!(request.subject, MenuSubject::Network);
    let chosen = |bar: &mut Taskbar, row: NetworkRow| {
        let index = crate::network_menu::ROWS
            .iter()
            .position(|listed| *listed == row)
            .expect("a listed row");
        bar.menu_chosen(
            &request.subject,
            AppMenuItemId::for_index(index).expect("a numbered row"),
        )
    };
    assert_eq!(
        chosen(&mut bar, NetworkRow::Settings),
        Some(TaskbarResponse::OpenNetworkSettings)
    );
    assert_eq!(
        chosen(&mut bar, NetworkRow::Activity),
        Some(TaskbarResponse::OpenSwitchboard {
            section: CommandSection::Resources
        })
    );
}

#[test]
fn a_primary_press_on_the_signal_is_claimed_and_inert() {
    let mut bar = bar();
    assert!(bar.set_network(Some(connected())));
    let mut input = TaskbarInput::new();
    hover_signal(&mut input, &mut bar);
    assert_eq!(
        input.handle(
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            },
            &mut bar,
            Scale::ONE,
            0,
        ),
        TaskbarResponse::Ignored
    );
    assert!(
        !bar.sound().is_open(),
        "only the volume signal opens a panel"
    );
}
