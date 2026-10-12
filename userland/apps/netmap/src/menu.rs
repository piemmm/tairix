//! The context menu a secondary press opens: the rows a node offers, as the
//! desktop's own menu model, and the command each row stands for.
//!
//! The desktop draws, places and answers the menu; this builds the model and
//! keeps the commands its rows are numbered by, so an answer naming a row is
//! mapped back to what that row meant when the menu was opened.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::IpAddr;

use tairix_abi::net_ipc::address_parts;
use tairix_abi::window_ipc::{
    AppMenu, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuRow, APP_MENU_LABEL_MAX,
};
use tairix_abi::Errno;
use tairix_font::ELLIPSIS;
use tairix_geometry::Point;
use tairix_net::mdns::is_link_local_address;
use tairix_procinfo::{render_ip, render_mac};

use crate::model::{Endpoint, Node, NodeKey, NodeKind};

/// The title a menu on empty canvas carries.
const MAP_TITLE: &str = "Network Map";

/// What one menu row does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Something the view does to itself.
    Steer(Steer),
    /// Put this text on the clipboard, which the shell reaches.
    Copy(String),
}

/// What a menu row asks of the view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Steer {
    /// Spread this host's services around it.
    ShowServices(NodeKey),
    /// Fold this host's services back into it.
    HideServices(NodeKey),
    /// Fit the whole map in the window, and keep it fitted.
    Fit,
}

/// One context menu: where it was asked for, the model the desktop draws,
/// and the command behind each of its rows.
#[derive(Clone, Debug)]
pub struct NodeMenu {
    anchor: Point,
    model: AppMenu,
    commands: Vec<Command>,
}

impl NodeMenu {
    /// The window-local point the menu was asked for at.
    #[must_use]
    pub fn anchor(&self) -> Point {
        self.anchor
    }

    /// The model the desktop draws.
    #[must_use]
    pub fn model(&self) -> &AppMenu {
        &self.model
    }

    /// The command the row `item` stands for, or `None` for an id this menu
    /// never declared.
    #[must_use]
    pub fn command(&self, item: AppMenuItemId) -> Option<&Command> {
        self.commands.get(item.index())
    }
}

/// The node a menu is for, and what the view knows of it beyond the node
/// itself.
#[derive(Clone, Copy, Debug)]
pub struct Subject<'a> {
    /// The node.
    pub node: &'a Node,
    /// How many services it offers.
    pub services: usize,
    /// Whether those services are spread around it now.
    pub spread: bool,
}

/// The menu for `subject` — or for the map itself when the press found no
/// node — asked for at `anchor`.
///
/// # Errors
///
/// The menu model's own refusal, which a bounded set of short rows never
/// reaches.
pub fn menu(subject: Option<Subject<'_>>, anchor: Point) -> Result<NodeMenu, Errno> {
    let named = subject.map_or(MAP_TITLE, |subject| subject.node.label.as_str());
    // A name the title cannot carry costs the menu its name, not the menu.
    let title = AppMenuLabel::new(&elide(named, APP_MENU_LABEL_MAX))
        .or_else(|_| AppMenuLabel::new(MAP_TITLE))?;
    let mut built = Builder {
        model: AppMenu::titled(title),
        commands: Vec::new(),
        grouped: false,
    };
    if let Some(subject) = subject {
        built.group(services_rows(subject))?;
        built.group(copy_rows(subject.node))?;
    }
    built.group(Vec::from([(
        "Fit Map to Window",
        Command::Steer(Steer::Fit),
    )]))?;
    Ok(NodeMenu {
        anchor,
        model: built.model,
        commands: built.commands,
    })
}

/// A menu being built: each row numbered by its command's place in the list,
/// and a rule between groups.
struct Builder {
    model: AppMenu,
    commands: Vec<Command>,
    /// Whether a group has been laid down, so the next one is ruled off.
    grouped: bool,
}

impl Builder {
    fn group(&mut self, rows: Vec<(&'static str, Command)>) -> Result<(), Errno> {
        if rows.is_empty() {
            return Ok(());
        }
        if core::mem::replace(&mut self.grouped, true) {
            self.model.push(AppMenuRow::Separator)?;
        }
        for (label, command) in rows {
            let id = AppMenuItemId::for_index(self.commands.len()).ok_or(Errno::NoSpace)?;
            self.model.push(AppMenuRow::Item(AppMenuItem::new(
                id,
                AppMenuLabel::new(label)?,
            )))?;
            self.commands.push(command);
        }
        Ok(())
    }
}

fn services_rows(subject: Subject<'_>) -> Vec<(&'static str, Command)> {
    if subject.services == 0 {
        return Vec::new();
    }
    let host = subject.node.key.clone();
    Vec::from([if subject.spread {
        ("Hide Services", Command::Steer(Steer::HideServices(host)))
    } else {
        ("Show Services", Command::Steer(Steer::ShowServices(host)))
    }])
}

fn copy_rows(node: &Node) -> Vec<(&'static str, Command)> {
    let mut rows = Vec::new();
    match node.kind {
        NodeKind::Service => {
            if let Some(endpoint) = &node.endpoint {
                if let Some(address) = service_address(node, endpoint) {
                    rows.push(("Copy Address", Command::Copy(address)));
                }
                if let Some(url) = service_url(node, endpoint) {
                    rows.push(("Copy URL", Command::Copy(url)));
                }
            }
        }
        NodeKind::Subnet => rows.push(("Copy Network", Command::Copy(node.label.clone()))),
        NodeKind::LocalHost | NodeKind::Beyond => {}
        _ => host_address_rows(node, &mut rows),
    }
    if names_itself(node) {
        rows.push(("Copy Name", Command::Copy(node.label.clone())));
    }
    if let Some(mac) = node.mac {
        rows.push(("Copy Hardware Address", Command::Copy(render_mac(mac))));
    }
    rows
}

/// One row for a host known by one family of address, one per family when it
/// is known by both.
fn host_address_rows(node: &Node, rows: &mut Vec<(&'static str, Command)>) {
    let v4 = node.addrs.iter().find(|addr| addr.is_ipv4());
    let v6 = node
        .addrs
        .iter()
        .filter(|addr| addr.is_ipv6())
        .min_by_key(|addr| is_link_local_address(**addr));
    match (v4, v6) {
        (Some(v4), Some(v6)) => {
            rows.push(("Copy IPv4 Address", Command::Copy(spell(*v4))));
            rows.push(("Copy IPv6 Address", Command::Copy(spell(*v6))));
        }
        (Some(one), None) | (None, Some(one)) => {
            rows.push(("Copy Address", Command::Copy(spell(*one))));
        }
        (None, None) => {}
    }
}

/// Whether the node's label is a name of its own rather than its address or
/// a caption the map wrote.
fn names_itself(node: &Node) -> bool {
    !matches!(
        node.kind,
        NodeKind::LocalHost | NodeKind::Subnet | NodeKind::Beyond
    ) && !node.label.is_empty()
        && !node.addrs.iter().any(|addr| spell(*addr) == node.label)
}

/// An address in the one text spelling every surface prints.
fn spell(addr: IpAddr) -> String {
    let (family, bytes) = address_parts(addr);
    render_ip(family, &bytes)
}

/// Where a client connects to reach the service: `host:port`, an IPv6 host
/// in brackets.
#[must_use]
pub fn service_address(node: &Node, endpoint: &Endpoint) -> Option<String> {
    Some(format!("{}:{}", url_host(node, endpoint)?, endpoint.port))
}

/// The service's URL, for a web service: the scheme its type names, the
/// host, the port unless it is the scheme's own, and the path its `TXT`
/// record names when that is one a URL can carry as it stands.
#[must_use]
pub fn service_url(node: &Node, endpoint: &Endpoint) -> Option<String> {
    let (scheme, default_port) = match endpoint.service_type.as_str() {
        "_http._tcp" => ("http", 80),
        "_https._tcp" => ("https", 443),
        _ => return None,
    };
    let host = url_host(node, endpoint)?;
    let port = if endpoint.port == default_port {
        String::new()
    } else {
        format!(":{}", endpoint.port)
    };
    let path = endpoint
        .text_value("path")
        .filter(|path| is_uri_path(path))
        .unwrap_or("/");
    Some(format!("{scheme}://{host}{port}{path}"))
}

/// The host part of the service's address: its IPv4 address, else a routable
/// IPv6 one, else its advertised name when that is a plain DNS name. A
/// link-local IPv6 address alone would need the zone the URL cannot name
/// portably, so the name is preferred to it.
fn url_host(node: &Node, endpoint: &Endpoint) -> Option<String> {
    if let Some(v4) = node.addrs.iter().find(|addr| addr.is_ipv4()) {
        return Some(spell(*v4));
    }
    if let Some(v6) = node
        .addrs
        .iter()
        .find(|addr| addr.is_ipv6() && !is_link_local_address(**addr))
    {
        return Some(format!("[{}]", spell(*v6)));
    }
    let name = endpoint.host.strip_suffix('.').unwrap_or(&endpoint.host);
    is_dns_name(name).then(|| String::from(name))
}

/// Whether `name` is letters, digits and hyphens in dot-separated labels of
/// one to sixty-three bytes — a name a URL carries with no escaping.
fn is_dns_name(name: &str) -> bool {
    (1..=253).contains(&name.len())
        && name.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// Whether `path` is an absolute URI path (RFC 3986 `path-absolute`): a `/`,
/// then only path characters, each `%` the start of a percent-encoded byte.
fn is_uri_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.first() != Some(&b'/') {
        return false;
    }
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        if byte == b'%' {
            let encoded = bytes
                .get(at + 1..at + 3)
                .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit));
            if !encoded {
                return false;
            }
            at += 3;
        } else if is_path_byte(byte) {
            at += 1;
        } else {
            return false;
        }
    }
    true
}

/// RFC 3986's `pchar` less the percent-encoding, plus `/`.
fn is_path_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&byte)
}

/// The longest prefix of `text` that fits `max` bytes once marked as elided,
/// ending on a `char` boundary; `text` itself when it fits whole, and nothing
/// when not even the mark fits.
#[must_use]
pub(crate) fn elide(text: &str, max: usize) -> String {
    if text.len() <= max {
        return String::from(text);
    }
    if max < ELLIPSIS.len() {
        return String::new();
    }
    let end = text.floor_char_boundary(max - ELLIPSIS.len());
    format!("{}{ELLIPSIS}", &text[..end])
}

#[cfg(test)]
#[path = "menu_tests.rs"]
mod tests;
