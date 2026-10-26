use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use tairix_abi::net_ipc::IF_NAME_LEN;
use tairix_discovery::catalogue::{Endpoint, Found, Listing, Source, SMB_PORT};
use tairix_discovery::OwnedMembership;
use tairix_icon::IconKind;

use super::{
    carried, items, order, NetworkItem, MIXED_SCRIPTS_LABEL, NOT_LISTED_LABEL, TRUNCATED_LABEL,
};
use crate::sort::{Folders, SortDirection, SortKey, SortMode};

fn interface(name: &str) -> [u8; IF_NAME_LEN] {
    let mut field = [0u8; IF_NAME_LEN];
    field[..name.len()].copy_from_slice(name.as_bytes());
    field
}

/// `nas.local` in wire form.
fn host() -> Vec<u8> {
    b"\x03nas\x05local\x00".to_vec()
}

fn v4(last: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(192, 168, 1, last))
}

fn endpoint(name: &str, found: Found, addresses: Vec<IpAddr>) -> Endpoint {
    Endpoint {
        interface: (found != Found::Named).then(|| interface("eth0")),
        name: name.as_bytes().to_vec(),
        found,
        host: matches!(found, Found::Advertised | Found::Both).then(host),
        addresses,
        port: SMB_PORT,
        membership: OwnedMembership::Unstated,
    }
}

fn listing(endpoints: Vec<Endpoint>) -> Listing {
    Listing {
        endpoints,
        ..Listing::default()
    }
}

fn shown(listing: Listing) -> Vec<NetworkItem> {
    items(listing)
}

fn labels(items: &[NetworkItem]) -> Vec<&str> {
    items.iter().map(NetworkItem::label).collect()
}

fn by(key: SortKey, direction: SortDirection) -> SortMode {
    SortMode {
        key,
        direction,
        folders: Folders::First,
    }
}

#[test]
fn a_listing_shows_its_endpoints_then_what_it_held_back_then_that_it_is_short() {
    let mut held = listing(vec![endpoint("Backup", Found::Advertised, vec![v4(20)])]);
    held.omitted = vec![(interface("eth0"), Source::Address(v4(9)), 3)];
    held.truncated = true;
    let items = shown(held);
    assert_eq!(
        labels(&items),
        ["Backup", "3 more from 192.168.1.9", TRUNCATED_LABEL]
    );
    assert_eq!(items[1].kind_label(), NOT_LISTED_LABEL);
    assert_eq!(items[1].address(), "192.168.1.9");
    assert_eq!(items[2].kind_label(), NOT_LISTED_LABEL);
    for row in &items[1..] {
        assert!(!row.is_available(), "{row:?} names nothing to open");
        assert!(row.endpoint().is_none());
        assert_eq!(row.icon(), IconKind::Info);
    }
}

#[test]
fn a_held_back_host_is_named_as_dns_spells_it() {
    let held = Listing {
        omitted: vec![(interface("eth0"), Source::Host(host()), 12)],
        ..Listing::default()
    };
    assert_eq!(labels(&shown(held)), ["12 more from nas.local"]);
}

#[test]
fn a_peers_name_is_shown_so_nothing_in_it_hides() {
    let items = shown(listing(vec![endpoint(
        "Back\u{200B}up",
        Found::Advertised,
        vec![v4(20)],
    )]));
    assert_eq!(items[0].label(), "Back\u{FFFD}up");
}

#[test]
fn a_name_that_is_not_text_is_shown_rather_than_dropped() {
    let mut raw = endpoint("x", Found::Advertised, vec![v4(20)]);
    raw.name = vec![b'N', b'A', b'S', 0xFF];
    let items = shown(listing(vec![raw]));
    assert_eq!(items[0].label(), "NAS\u{FFFD}");
}

#[test]
fn a_name_mixing_scripts_wears_the_caution_instead_of_its_kind() {
    // A Cyrillic "а" among Latin letters.
    let items = shown(listing(vec![
        endpoint("B\u{0430}ckup", Found::Advertised, vec![v4(20)]),
        endpoint("Backup", Found::Advertised, vec![v4(21)]),
    ]));
    let mixed = items[0].endpoint().expect("an endpoint");
    assert!(mixed.mixes_scripts());
    assert_eq!(mixed.icon(), IconKind::AlertWarning);
    assert_eq!(mixed.kind_label(), MIXED_SCRIPTS_LABEL);
    let plain = items[1].endpoint().expect("an endpoint");
    assert!(!plain.mixes_scripts());
    assert_eq!(plain.icon(), IconKind::Server);
}

#[test]
fn what_found_an_endpoint_chooses_its_picture_and_type() {
    let items = shown(listing(vec![
        endpoint("a", Found::Advertised, vec![v4(1)]),
        endpoint("b", Found::Announced, vec![v4(2)]),
        endpoint("c", Found::Both, vec![v4(3)]),
        endpoint("d", Found::Named, vec![]),
    ]));
    let seen: Vec<(IconKind, &str)> = items
        .iter()
        .map(|item| (item.icon(), item.kind_label()))
        .collect();
    assert_eq!(
        seen,
        [
            (IconKind::Server, "File server"),
            (IconKind::Computer, "Computer"),
            (IconKind::Server, "File server"),
            (IconKind::Server, "Named by you"),
        ]
    );
}

#[test]
fn an_endpoint_with_no_address_cannot_be_reached_unless_the_user_named_it() {
    let items = shown(listing(vec![
        endpoint("gone", Found::Advertised, vec![]),
        endpoint("named", Found::Named, vec![]),
        endpoint("here", Found::Advertised, vec![v4(4)]),
    ]));
    let available: Vec<bool> = items.iter().map(NetworkItem::is_available).collect();
    assert_eq!(available, [false, true, true]);
    // Until an address is known it is shown where it was advertised.
    assert_eq!(items[0].address(), "nas.local");
    assert_eq!(items[1].address(), "");
}

#[test]
fn an_address_that_names_a_host_only_on_its_link_carries_the_link() {
    let link_local = IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1));
    let global = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1));
    let items = shown(listing(vec![
        endpoint("a", Found::Announced, vec![link_local]),
        endpoint("b", Found::Announced, vec![global]),
        endpoint("c", Found::Announced, vec![v4(7)]),
    ]));
    let addresses: Vec<&str> = items.iter().map(NetworkItem::address).collect();
    assert_eq!(addresses, ["fe80::1%eth0", "2001:db8::1", "192.168.1.7"]);
}

#[test]
fn a_computer_states_its_workgroup_or_its_domain() {
    let mut workgroup = endpoint("a", Found::Announced, vec![v4(1)]);
    workgroup.membership = OwnedMembership::Workgroup(String::from("HOME"));
    let mut domain = endpoint("b", Found::Announced, vec![v4(2)]);
    domain.membership = OwnedMembership::Domain(String::from("corp.example"));
    let unstated = endpoint("c", Found::Announced, vec![v4(3)]);
    let items = shown(listing(vec![workgroup, domain, unstated]));
    let memberships: Vec<&str> = items.iter().map(NetworkItem::membership).collect();
    assert_eq!(memberships, ["HOME", "corp.example (domain)", ""]);
}

#[test]
fn a_membership_a_peer_chose_is_shown_too() {
    let mut hidden = endpoint("a", Found::Announced, vec![v4(1)]);
    hidden.membership = OwnedMembership::Workgroup(String::from("HO\u{202E}ME"));
    assert_eq!(shown(listing(vec![hidden]))[0].membership(), "HO\u{FFFD}ME");
}

fn ordered(items: &[NetworkItem], mode: SortMode) -> Vec<String> {
    let mut items = items.to_vec();
    order(&mut items, mode);
    items
        .iter()
        .map(|item| String::from(item.label()))
        .collect()
}

fn sample() -> Vec<NetworkItem> {
    let mut beta = endpoint("beta", Found::Announced, vec![v4(9)]);
    beta.membership = OwnedMembership::Workgroup(String::from("ZETA"));
    let mut alpha = endpoint("Alpha", Found::Advertised, vec![v4(30)]);
    alpha.membership = OwnedMembership::Workgroup(String::from("ALPHA"));
    let gamma = endpoint("gamma", Found::Advertised, vec![]);
    let mut held = listing(vec![beta, alpha, gamma]);
    held.omitted = vec![(interface("eth0"), Source::Address(v4(5)), 2)];
    held.truncated = true;
    shown(held)
}

#[test]
fn endpoints_order_by_name_either_way_and_what_is_held_back_stays_last() {
    let items = sample();
    assert_eq!(
        ordered(&items, by(SortKey::Name, SortDirection::Ascending)),
        [
            "Alpha",
            "beta",
            "gamma",
            "2 more from 192.168.1.5",
            TRUNCATED_LABEL
        ]
    );
    assert_eq!(
        ordered(&items, by(SortKey::Name, SortDirection::Descending)),
        [
            "gamma",
            "beta",
            "Alpha",
            "2 more from 192.168.1.5",
            TRUNCATED_LABEL
        ]
    );
}

#[test]
fn endpoints_order_by_address_with_those_reached_nowhere_first() {
    assert_eq!(
        ordered(&sample(), by(SortKey::Address, SortDirection::Ascending))[..3],
        ["gamma", "beta", "Alpha"]
    );
}

#[test]
fn endpoints_order_by_workgroup_and_by_type() {
    let items = sample();
    assert_eq!(
        ordered(&items, by(SortKey::Workgroup, SortDirection::Ascending))[..3],
        ["gamma", "Alpha", "beta"]
    );
    assert_eq!(
        ordered(&items, by(SortKey::Kind, SortDirection::Ascending))[..3],
        ["Alpha", "gamma", "beta"]
    );
}

#[test]
fn a_key_no_endpoint_carries_orders_them_by_name() {
    assert_eq!(
        ordered(&sample(), by(SortKey::Size, SortDirection::Descending))[..3],
        ["Alpha", "beta", "gamma"]
    );
}

#[test]
fn an_endpoint_is_followed_across_answers_when_its_addresses_move() {
    let before = shown(listing(vec![
        endpoint("Backup", Found::Advertised, vec![]),
        endpoint("Media", Found::Advertised, vec![v4(2)]),
    ]));
    let after = shown(listing(vec![
        endpoint("Media", Found::Advertised, vec![v4(2)]),
        endpoint("Backup", Found::Advertised, vec![v4(1), v4(3)]),
        endpoint("New", Found::Announced, vec![v4(4)]),
    ]));
    assert_eq!(
        carried(&before, &after),
        Some(vec![Some(1), Some(0)]),
        "each kept endpoint is found where it went"
    );
    let gone = shown(listing(vec![endpoint(
        "Media",
        Found::Advertised,
        vec![v4(2)],
    )]));
    assert_eq!(carried(&before, &gone), Some(vec![None, Some(0)]));
}

#[test]
fn a_computer_nothing_advertised_is_the_computer_at_its_address() {
    let before = shown(listing(vec![endpoint("PC", Found::Announced, vec![v4(1)])]));
    let moved = shown(listing(vec![endpoint("PC", Found::Announced, vec![v4(2)])]));
    assert_eq!(carried(&before, &moved), Some(vec![None]));
}

mod location {
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;

    use tairix_abi::window_ipc::{AppMenuItemId, AppMenuRowView};
    use tairix_abi::Errno;
    use tairix_discovery::catalogue::{Found, Source};

    use super::{endpoint, interface, listing, shown, v4};
    use crate::activate::{Activation, BundleIntent};
    use crate::browser::{Browser, Location};
    use crate::chrome::{
        context_choice_from_item, network_context_menu, ContextChoice, ContextCommand,
    };
    use crate::clipboard::ClipboardOp;
    use crate::create::CreateError;
    use crate::entry::Entry;
    use crate::error::BrowseError;
    use crate::network::NetworkItem;
    use crate::presentation::Presentation;
    use crate::rename::RenameError;
    use crate::render::ListColumns;
    use crate::sort::{SortDirection, SortKey, SortMode};
    use crate::source::{DirectorySource, Listing};

    /// `/` holding `Users`, and an empty `/Users`.
    #[derive(Clone, Debug)]
    struct Folders;

    impl DirectorySource for Folders {
        fn list(&mut self, components: &[String]) -> Result<Listing, Errno> {
            match components {
                [] => Ok(Listing::Ready(vec![Entry::directory("Users")])),
                [users] if users == "Users" => Ok(Listing::Ready(Vec::new())),
                _ => Err(Errno::NotFound),
            }
        }
    }

    fn at_root() -> Browser<Folders> {
        Browser::open_root(Folders).expect("the root lists")
    }

    fn answer() -> Vec<NetworkItem> {
        let mut held = listing(vec![
            endpoint("Media", Found::Advertised, vec![v4(2)]),
            endpoint("Backup", Found::Advertised, vec![v4(1)]),
            endpoint("Gone", Found::Advertised, vec![]),
        ]);
        held.omitted = vec![(interface("eth0"), Source::Address(v4(9)), 2)];
        shown(held)
    }

    fn labels(browser: &Browser<Folders>) -> Vec<&str> {
        browser
            .network_items()
            .iter()
            .map(NetworkItem::label)
            .collect()
    }

    fn on_network() -> Browser<Folders> {
        let mut browser = at_root();
        assert!(browser.navigate_to_network());
        assert!(browser.show_network(answer()));
        browser
    }

    #[test]
    fn the_network_lands_with_the_catalogues_answer_and_until_then_nothing_moves() {
        let mut browser = at_root();
        assert!(browser.navigate_to_network());
        assert!(browser.wants_network());
        assert!(browser.is_listing());
        assert!(browser.listing_elsewhere());
        assert_eq!(browser.listing_target(), Some(&Location::Network));
        assert_eq!(browser.folder(), Some(&[][..]), "the folder is still shown");
        assert!(
            !browser.can_go_back(),
            "the history waits for the answer too"
        );
        assert_eq!(browser.resume(), Ok(false), "a folder read never lands it");
        assert!(browser.show_network(answer()));
        assert!(browser.is_network());
        assert_eq!(browser.folder(), None);
        assert_eq!(browser.path(), None);
        assert!(browser.entries().is_empty());
        assert_eq!(
            labels(&browser),
            ["Backup", "Gone", "Media", "2 more from 192.168.1.9"]
        );
        assert!(browser.can_go_back());
        assert!(!browser.is_listing());
    }

    #[test]
    fn back_and_forward_cross_between_a_folder_and_the_network() {
        let mut browser = on_network();
        assert!(!browser.navigate_to_network(), "already there");
        assert_eq!(browser.go_back(), Ok(true));
        assert_eq!(browser.folder(), Some(&[][..]));
        assert!(!browser.wants_network(), "leaving stops the browse");
        assert!(browser.can_go_forward());
        assert_eq!(browser.go_forward(), Ok(true));
        assert!(browser.wants_network());
        assert!(
            !browser.is_network(),
            "forward waits on the answer like any move"
        );
        assert!(browser.show_network(answer()));
        assert!(browser.is_network());
        assert!(!browser.can_go_forward());
    }

    #[test]
    fn a_browser_opened_at_the_network_lists_once_the_first_answer_lands() {
        let mut browser = Browser::open_network(Folders);
        assert!(browser.is_network());
        assert!(browser.is_listing());
        assert!(browser.wants_network());
        assert!(!browser.can_go_back());
        assert!(browser.show_network(answer()));
        assert!(!browser.is_listing());
        assert!(!browser.can_go_back(), "opening is no move");
        assert_eq!(browser.item_count(), 4);
    }

    #[test]
    fn an_answer_with_nothing_waiting_moves_no_folder() {
        let mut browser = at_root();
        assert!(!browser.show_network(answer()));
        assert_eq!(browser.folder(), Some(&[][..]));
        assert_eq!(browser.entries().len(), 1);
    }

    #[test]
    fn a_catalogue_that_cannot_be_read_refuses_the_move_and_empties_a_shown_network() {
        let mut browser = at_root();
        assert!(browser.navigate_to_network());
        assert!(!browser.stop_network(), "nothing shown changed");
        assert!(!browser.wants_network());
        assert!(!browser.is_listing());
        assert_eq!(browser.folder(), Some(&[][..]));

        let mut browser = on_network();
        assert!(browser.select(0).is_ok());
        assert!(browser.stop_network());
        assert!(browser.network_stopped());
        assert_eq!(browser.item_count(), 0);
        assert!(browser.selection().is_empty());
        assert!(!browser.stop_network(), "already stopped");
        assert!(browser.refresh().is_ok());
        assert!(browser.wants_network());
        assert!(browser.show_network(answer()));
        assert!(!browser.network_stopped());
        assert_eq!(browser.item_count(), 4);
        assert!(browser.can_go_back(), "a refresh keeps the history");
    }

    /// `/` holding `Users`, whose listing never arrives.
    #[derive(Clone, Debug)]
    struct UsersPending;

    impl DirectorySource for UsersPending {
        fn list(&mut self, components: &[String]) -> Result<Listing, Errno> {
            match components {
                [] => Ok(Listing::Ready(vec![Entry::directory("Users")])),
                _ => Ok(Listing::Pending),
            }
        }
    }

    #[test]
    fn a_network_shown_while_a_folder_is_on_its_way_is_not_waiting_on_the_catalogue() {
        let mut browser = Browser::open_root(UsersPending).expect("the root lists");
        assert!(browser.navigate_to_network());
        assert!(browser.awaits_network());
        assert!(browser.show_network(answer()));
        assert!(!browser.awaits_network(), "the answer landed it");
        assert_eq!(browser.navigate_to(vec![String::from("Users")]), Ok(true));
        assert!(browser.is_listing());
        assert!(browser.wants_network(), "it still shows the network");
        assert!(!browser.awaits_network(), "but waits on a folder");
        let mut later = answer();
        later.truncate(2);
        assert!(
            browser.show_network(later),
            "the network shown is brought up to date"
        );
        assert_eq!(
            browser.listing_target(),
            Some(&Location::Folder(vec![String::from("Users")])),
            "and the folder is still on its way"
        );
        assert!(!browser.can_go_forward());
    }

    #[test]
    fn a_later_answer_keeps_the_selection_on_the_endpoint_it_named() {
        let mut browser = on_network();
        assert!(browser.select(2).is_ok());
        assert_eq!(
            browser.chosen_network_item().map(NetworkItem::label),
            Some("Media")
        );
        let mut later = listing(vec![
            endpoint("Media", Found::Advertised, vec![v4(2), v4(3)]),
            endpoint("Archive", Found::Advertised, vec![v4(4)]),
            endpoint("Backup", Found::Advertised, vec![v4(1)]),
        ]);
        later.truncated = true;
        assert!(browser.show_network(shown(later)));
        assert_eq!(
            browser.chosen_network_item().map(NetworkItem::label),
            Some("Media")
        );
        assert!(
            !browser.show_network(browser.network_items().to_vec()),
            "nothing moved"
        );
    }

    #[test]
    fn reordering_the_network_keeps_the_selection_on_its_item() {
        let mut browser = on_network();
        assert!(browser.select(0).is_ok());
        browser.set_sort_mode(SortMode {
            key: SortKey::Name,
            direction: SortDirection::Descending,
            ..SortMode::default_order()
        });
        assert_eq!(
            labels(&browser),
            ["Media", "Gone", "Backup", "2 more from 192.168.1.9"]
        );
        assert_eq!(
            browser.chosen_network_item().map(NetworkItem::label),
            Some("Backup")
        );
    }

    #[test]
    fn showing_hidden_items_reads_no_network_again() {
        let mut browser = on_network();
        let presentation = Presentation {
            hidden_items: !browser.presentation().hidden_items,
            ..*browser.presentation()
        };
        assert_eq!(browser.set_presentation(presentation), Ok(false));
        assert!(!browser.is_listing());
    }

    #[test]
    fn activating_an_endpoint_names_it_for_the_mount() {
        let mut browser = on_network();
        let Ok(Activation::MountEndpoint(chosen)) = browser.activate_index(0, BundleIntent::Launch)
        else {
            panic!("a reachable endpoint is named for the mount");
        };
        assert_eq!(chosen.name(), "Backup");
        assert_eq!(chosen.endpoint().addresses, [v4(1)]);
        assert_eq!(
            browser.activate_index(1, BundleIntent::Launch),
            Err(BrowseError::Unreachable)
        );
        assert_eq!(
            browser.activate_index(3, BundleIntent::Launch),
            Err(BrowseError::NoSuchEntry),
            "a row standing for what is not listed opens nothing"
        );
        assert_eq!(
            browser.activate_index(9, BundleIntent::Browse),
            Err(BrowseError::NoSuchEntry)
        );
        assert!(browser.is_network(), "activating moved nothing");
    }

    #[test]
    fn no_verb_that_names_a_node_reaches_anything_on_the_network() {
        let mut browser = on_network();
        browser.select_all();
        assert_eq!(browser.selection().len(), 4);
        assert!(browser.plan_delete().is_none());
        assert!(browser.clipboard(ClipboardOp::Copy).is_none());
        assert!(browser.listed_selection().is_empty());
        assert!(browser.select(0).is_ok());
        assert!(browser.chosen_entry().is_none());
        assert!(browser.chosen_target_path().is_none());
        assert_eq!(
            browser.prepare_rename("x", Presentation::EXACT.endings),
            Err(RenameError::NoSelection)
        );
        assert_eq!(
            browser.prepare_entry("New folder"),
            Err(CreateError::NoFolder)
        );
        assert_eq!(browser.set_drop_mark(Some(0)), None);
        assert_eq!(
            browser.drop_mark(),
            None,
            "nothing on the network takes a drop"
        );
        assert!(!browser.can_go_up());
        assert_eq!(browser.go_up(), Ok(false));
        assert_eq!(browser.open_index(0), Err(BrowseError::NoSuchEntry));
        assert!(browser.follows());
        assert!(browser.is_network());
    }

    #[test]
    fn the_network_lists_its_own_columns() {
        let browser = on_network();
        assert_eq!(ListColumns::of_browser(&browser), ListColumns::Network);
        assert_eq!(
            ListColumns::Network.keys(),
            [
                SortKey::Name,
                SortKey::Address,
                SortKey::Workgroup,
                SortKey::Kind
            ]
        );
        assert_eq!(ListColumns::of_browser(&at_root()), ListColumns::Files);
    }

    fn item_rows(menu: &tairix_abi::window_ipc::AppMenu) -> Vec<(String, bool, AppMenuItemId)> {
        menu.rows()
            .filter_map(|(row, _)| match row {
                AppMenuRowView::Item(item) => Some((item.label.to_string(), item.enabled, item.id)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_networks_menu_offers_open_and_its_orders_and_nothing_that_writes() {
        let mut browser = on_network();
        let menu = network_context_menu(&browser, "Network").expect("the menu builds");
        let rows = item_rows(&menu);
        let open = rows
            .iter()
            .find(|(label, _, _)| label == ContextCommand::Open.label())
            .expect("Open is offered");
        assert!(!open.1, "nothing is selected yet");
        assert_eq!(
            context_choice_from_item(open.2),
            Some(ContextChoice::Command(ContextCommand::Open))
        );
        for refused in [
            ContextCommand::Rename,
            ContextCommand::Delete,
            ContextCommand::Paste,
            ContextCommand::Copy,
        ] {
            assert!(
                rows.iter().all(|(label, _, _)| label != refused.label()),
                "{refused:?} is not offered"
            );
        }
        let address = rows
            .iter()
            .find(|(label, _, _)| label == SortKey::Address.label())
            .expect("the network orders by address");
        assert_eq!(
            context_choice_from_item(address.2),
            Some(ContextChoice::SortBy(SortKey::Address))
        );
        assert!(rows
            .iter()
            .all(|(label, _, _)| label != SortKey::Size.label()));

        assert!(browser.select(0).is_ok());
        let menu = network_context_menu(&browser, "Network").expect("the menu builds");
        assert!(item_rows(&menu)
            .iter()
            .any(|(label, enabled, _)| label == ContextCommand::Open.label() && *enabled));
        assert!(browser.select(1).is_ok());
        let menu = network_context_menu(&browser, "Network").expect("the menu builds");
        assert!(item_rows(&menu)
            .iter()
            .any(|(label, enabled, _)| label == ContextCommand::Open.label() && !*enabled));
    }

    mod painted {
        use tairix_geometry::{Point, Rect, Region, Scale};
        use tairix_icon::NoArtwork;
        use tairix_raster::Surface;
        use tairix_theme::Theme;

        use super::{answer, on_network, Folders};
        use crate::browser::Browser;
        use crate::layout::ViewMode;
        use crate::render::{
            drop_folder_at, entry_index_at, entry_rect, header_sort_at, listing_damage,
            render_into, shown_listing, ManagerChrome,
        };
        use crate::sort::{SortDirection, SortKey};
        use crate::ToolbarBand;

        const WINDOW: Rect = Rect::new(0, 0, 480, 320);
        const BAND: ToolbarBand = ManagerChrome::none().toolbar;

        fn paint(browser: &Browser<Folders>) -> Surface {
            let mut surface = Surface::new(WINDOW.width, WINDOW.height).expect("surface");
            render_into(
                &mut surface,
                browser,
                Scale::ONE,
                &Theme::dark(),
                WINDOW,
                &ManagerChrome::none(),
                &mut NoArtwork,
            );
            surface
        }

        #[test]
        fn an_empty_network_says_it_is_looking_and_a_stopped_one_says_it_stopped() {
            let mut looking = on_network();
            assert!(looking.show_network(alloc::vec::Vec::new()));
            let mut stopped = on_network();
            assert!(stopped.stop_network());
            let listed = on_network();
            let (looking, stopped, listed) = (paint(&looking), paint(&stopped), paint(&listed));
            assert_ne!(looking.pixels(), stopped.pixels());
            assert_ne!(looking.pixels(), listed.pixels());
            assert_ne!(stopped.pixels(), listed.pixels());
        }

        #[test]
        fn a_listed_network_draws_alike_in_every_display() {
            let mut browser = on_network();
            for mode in ViewMode::ALL {
                browser.set_view_mode(mode);
                let theme = Theme::dark();
                let row = entry_rect(&browser, Scale::ONE, &theme, WINDOW, BAND, 0)
                    .expect("the first endpoint shows");
                assert_eq!(
                    entry_index_at(&browser, Scale::ONE, &theme, WINDOW, BAND, row.center()),
                    Some(0),
                    "{mode:?}: a press on the endpoint finds it"
                );
                assert_eq!(
                    drop_folder_at(&browser, Scale::ONE, &theme, WINDOW, BAND, row.center()),
                    None,
                    "{mode:?}: nothing on the network takes a drop"
                );
                let _ = paint(&browser);
            }
        }

        #[test]
        fn the_lists_address_header_orders_the_network_by_address() {
            let mut browser = on_network();
            browser.set_view_mode(ViewMode::List);
            let theme = Theme::dark();
            let row = entry_rect(&browser, Scale::ONE, &theme, WINDOW, BAND, 0)
                .expect("the first endpoint shows");
            // The header sits just above the first row; the address column is
            // the second of four, a little past the name's share of the width.
            let header_y = row.top() - 4;
            let found = (row.left()..row.right()).step_by(8).find_map(|x| {
                header_sort_at(
                    &browser,
                    Scale::ONE,
                    &theme,
                    WINDOW,
                    BAND,
                    Point::new(x, header_y),
                )
                .filter(|order| order.key == SortKey::Address)
            });
            let order = found.expect("a press on the Address title orders by address");
            assert_eq!(order.direction, SortDirection::Ascending);
            browser.set_sort_mode(order);
            assert_eq!(
                browser.network_items()[0].label(),
                "Gone",
                "one reached nowhere comes first"
            );
        }

        #[test]
        fn an_answer_repaints_only_the_slots_it_altered() {
            let mut browser = on_network();
            browser.set_view_mode(ViewMode::List);
            let theme = Theme::dark();
            let before = shown_listing(&browser, Scale::ONE, &theme, WINDOW, BAND);
            let mut damage = Region::new();
            assert!(!listing_damage(
                &before,
                &browser,
                Scale::ONE,
                &theme,
                WINDOW,
                BAND,
                &mut damage
            ));
            let mut later = answer();
            later.truncate(3);
            assert!(browser.show_network(later));
            assert!(listing_damage(
                &before,
                &browser,
                Scale::ONE,
                &theme,
                WINDOW,
                BAND,
                &mut damage
            ));
            let first = entry_rect(&browser, Scale::ONE, &theme, WINDOW, BAND, 0)
                .expect("the first endpoint shows");
            assert!(
                !damage.intersects(first),
                "the first row did not change, so it is not repainted"
            );
        }
    }
}
