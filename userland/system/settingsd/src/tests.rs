extern crate std;

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::driver_store::SystemConfigFile;
use tairix_abi::origin::{CapabilitySummary, Origin, ProcId, TrustDomain};
use tairix_abi::settings_ipc::{
    decode_reply, encode_read, encode_set, SETTINGS_REPLY_MAX, SETTINGS_REQUEST_MAX,
};
use tairix_abi::{CapabilityId, Errno};
use tairix_log::field::FieldValue;
use tairix_log::{Event, EventId, Sink};
use tairix_netconfig::NetworkConfig;
use tairix_sysconfig::{Switch, SystemConfig};

use super::{events, DocumentStore, SettingsService};

const ADMIN: u32 = 1000;

/// Both layers of both documents, in memory.
#[derive(Default)]
struct Layers {
    administrator: [Option<String>; 2],
    image: [Option<String>; 2],
    read_refusal: Option<Errno>,
    replace_refusal: Option<Errno>,
    replaced: usize,
}

const fn slot(which: SystemConfigFile) -> usize {
    which.as_u8() as usize
}

impl DocumentStore for &RefCell<Layers> {
    fn read(&mut self, which: SystemConfigFile) -> Result<Option<String>, Errno> {
        let layers = self.borrow();
        if let Some(err) = layers.read_refusal {
            return Err(err);
        }
        Ok(layers.administrator[slot(which)]
            .clone()
            .or_else(|| layers.image[slot(which)].clone()))
    }

    fn replace(&mut self, which: SystemConfigFile, text: &str) -> Result<(), Errno> {
        let mut layers = self.borrow_mut();
        if let Some(err) = layers.replace_refusal {
            return Err(err);
        }
        layers.administrator[slot(which)] = Some(text.to_string());
        layers.replaced += 1;
        Ok(())
    }
}

/// One decision: its id, and its uid and errno fields.
type Decision = (EventId, Option<u64>, Option<i64>);

/// Every decision recorded.
#[derive(Default)]
struct Recorded(RefCell<Vec<Decision>>);

impl Sink for &Recorded {
    fn write_event(&self, event: &Event<'_>) {
        let field = |key: &str| event.fields.iter().find(|field| field.key == key);
        let uid = field("uid").and_then(|field| match field.value {
            FieldValue::UnsignedInt(uid) => Some(uid),
            _ => None,
        });
        let errno = field("errno").and_then(|field| match field.value {
            FieldValue::SignedInt(errno) => Some(errno),
            _ => None,
        });
        self.0.borrow_mut().push((event.id, uid, errno));
    }
}

fn caller(holds: bool) -> Origin {
    let mut capabilities = CapabilitySummary::EMPTY;
    if holds {
        capabilities.insert(CapabilityId::SETTINGS_WRITE);
    }
    Origin::new(
        TrustDomain::User,
        ADMIN,
        ADMIN,
        7,
        ProcId::from_raw([7; 16]),
        capabilities,
        0,
    )
}

fn change(which: SystemConfigFile, settings: &[(&str, &str)]) -> Vec<u8> {
    let mut out = vec![0u8; SETTINGS_REQUEST_MAX];
    let len = encode_set(which, settings, &mut out).expect("the change encodes");
    out.truncate(len);
    out
}

fn read(which: SystemConfigFile) -> Vec<u8> {
    let mut out = vec![0u8; 16];
    let len = encode_read(which, &mut out).expect("the read encodes");
    out.truncate(len);
    out
}

/// Serve `request` as `from`, answering the reply frame.
fn serve(layers: &RefCell<Layers>, log: &Recorded, from: &Origin, request: &[u8]) -> Vec<u8> {
    let mut service = SettingsService::new(layers, log);
    let mut reply = vec![0u8; SETTINGS_REPLY_MAX];
    let len = service.serve(from, request, &mut reply);
    reply.truncate(len);
    reply
}

fn machine(layers: &RefCell<Layers>) -> SystemConfig {
    let text = layers.borrow().administrator[slot(SystemConfigFile::System)]
        .clone()
        .expect("the administrator's layer was written");
    SystemConfig::parse(&text).expect("the service writes what its engine reads")
}

const SHIPPED_NETWORK: &str = "\
wan.kind ethernet
wan.match.node 0xfd580000
wan.ipv4.method dhcp
vwan.kind ethernet
vwan.match.mac 52:54:00:00:00:01
vwan.ipv4.method dhcp
";

#[test]
fn a_change_lands_in_the_administrator_s_layer_rendered_by_its_engine() {
    let layers = RefCell::new(Layers::default());
    let log = Recorded::default();
    let reply = serve(
        &layers,
        &log,
        &caller(true),
        &change(
            SystemConfigFile::System,
            &[("time.network", "off"), ("time.zone", "Europe/London")],
        ),
    );
    let config = machine(&layers);
    assert_eq!(
        decode_reply(&reply),
        Ok(
            layers.borrow().administrator[slot(SystemConfigFile::System)]
                .as_deref()
                .expect("written")
        ),
        "a change is answered with the document it wrote"
    );
    assert_eq!(config.time_network, Switch::Off);
    assert_eq!(
        config
            .time_zone
            .as_ref()
            .map(tairix_abi::zone::ZoneName::as_str),
        Some("Europe/London")
    );
    let mut expected = SystemConfig::default();
    expected
        .set(tairix_sysconfig::Key::TimeNetwork, "off")
        .expect("on or off");
    expected
        .set(tairix_sysconfig::Key::TimeZone, "Europe/London")
        .expect("a zone name");
    assert_eq!(
        layers.borrow().administrator[slot(SystemConfigFile::System)].as_deref(),
        Some(expected.render().as_str())
    );
    assert_eq!(
        *log.0.borrow(),
        [(events::CHANGE_APPLIED, Some(u64::from(ADMIN)), Some(0))]
    );
}

#[test]
fn the_first_change_starts_from_the_image_s_document_and_keeps_it() {
    // Without the image's layer as its base, one changed key would replace
    // every interface the image shipped.
    let layers = RefCell::new(Layers::default());
    layers.borrow_mut().image[slot(SystemConfigFile::Network)] = Some(SHIPPED_NETWORK.to_string());
    let reply = serve(
        &layers,
        &Recorded::default(),
        &caller(true),
        &change(SystemConfigFile::Network, &[("wan.mtu", "1400")]),
    );
    assert_eq!(decode_reply(&reply).map(drop), Ok(()));
    let written = layers.borrow().administrator[slot(SystemConfigFile::Network)]
        .clone()
        .expect("written");
    let config = NetworkConfig::parse(&written).expect("parses");
    assert_eq!(config.interfaces().len(), 2);
    let wan = config.interface("wan").expect("kept");
    assert_eq!(wan.match_node, Some(0xfd58_0000));
    assert_eq!(
        wan.render_value(tairix_netconfig::IfaceKey::Mtu).as_deref(),
        Some("1400")
    );
    assert!(
        config.interface("vwan").is_some(),
        "the image's other interface stands"
    );
    assert_eq!(
        layers.borrow().image[slot(SystemConfigFile::Network)].as_deref(),
        Some(SHIPPED_NETWORK),
        "the image's layer is never written"
    );
}

#[test]
fn a_change_spanning_keys_is_checked_whole_and_the_empty_value_clears() {
    let layers = RefCell::new(Layers::default());
    layers.borrow_mut().administrator[slot(SystemConfigFile::Network)] = Some(
        "wan.match.mac 52:54:00:12:34:56\nwan.ipv4.method static\n\
         wan.ipv4.address 192.168.1.10/24\nwan.ipv4.gateway 192.168.1.1\n"
            .to_string(),
    );
    let log = Recorded::default();
    let reply = serve(
        &layers,
        &log,
        &caller(true),
        &change(
            SystemConfigFile::Network,
            &[
                ("wan.ipv4.method", "dhcp"),
                ("wan.ipv4.address", ""),
                ("wan.ipv4.gateway", ""),
            ],
        ),
    );
    assert_eq!(decode_reply(&reply).map(drop), Ok(()));
    let written = layers.borrow().administrator[slot(SystemConfigFile::Network)]
        .clone()
        .expect("written");
    let wan = NetworkConfig::parse(&written)
        .expect("parses")
        .interface("wan")
        .cloned()
        .expect("kept");
    assert_eq!(wan.ipv4_method(), tairix_netconfig::Ipv4Method::Dhcp);
    assert_eq!(
        wan.render_value(tairix_netconfig::IfaceKey::Ipv4Address),
        None
    );

    // Half of that change alone is a document the engine refuses.
    let refused = serve(
        &layers,
        &log,
        &caller(true),
        &change(SystemConfigFile::Network, &[("wan.ipv4.method", "static")]),
    );
    assert_eq!(decode_reply(&refused), Err(Errno::OutOfRange));
}

#[test]
fn any_refused_setting_writes_nothing_at_all() {
    for (which, settings, errno) in [
        (
            SystemConfigFile::System,
            &[("time.network", "off"), ("time.network.maybe", "on")][..],
            Errno::NotFound,
        ),
        (
            SystemConfigFile::System,
            &[("time.network", "off"), ("cache.all", "sometimes")][..],
            Errno::OutOfRange,
        ),
        (
            SystemConfigFile::System,
            &[("time.network", "off"), ("time.network", "on")][..],
            Errno::AlreadyExists,
        ),
        (
            SystemConfigFile::Network,
            &[("wan.kind", "ethernet"), ("wan.colour", "blue")][..],
            Errno::NotFound,
        ),
        (
            SystemConfigFile::Network,
            &[("time.network", "off")][..],
            Errno::NotFound,
        ),
    ] {
        let layers = RefCell::new(Layers::default());
        let log = Recorded::default();
        let reply = serve(&layers, &log, &caller(true), &change(which, settings));
        assert_eq!(decode_reply(&reply), Err(errno), "{settings:?}");
        assert_eq!(layers.borrow().replaced, 0, "{settings:?}");
        assert_eq!(
            *log.0.borrow(),
            [(
                events::CHANGE_REFUSED,
                Some(u64::from(ADMIN)),
                Some(i64::from(errno.as_i32()))
            )]
        );
    }
}

#[test]
fn a_document_the_engine_cannot_read_is_never_changed() {
    let layers = RefCell::new(Layers::default());
    layers.borrow_mut().administrator[slot(SystemConfigFile::System)] =
        Some("time.network sometimes\n".to_string());
    let reply = serve(
        &layers,
        &Recorded::default(),
        &caller(true),
        &change(SystemConfigFile::System, &[("time.zone", "default")]),
    );
    assert_eq!(decode_reply(&reply), Err(Errno::BadMagic));
    assert_eq!(layers.borrow().replaced, 0);
}

#[test]
fn a_layer_that_cannot_be_read_or_written_is_reported() {
    let layers = RefCell::new(Layers::default());
    layers.borrow_mut().read_refusal = Some(Errno::PermissionDenied);
    let reply = serve(
        &layers,
        &Recorded::default(),
        &caller(true),
        &change(SystemConfigFile::System, &[("time.network", "off")]),
    );
    assert_eq!(decode_reply(&reply), Err(Errno::PermissionDenied));

    let layers = RefCell::new(Layers::default());
    layers.borrow_mut().replace_refusal = Some(Errno::NoSpace);
    let reply = serve(
        &layers,
        &Recorded::default(),
        &caller(true),
        &change(SystemConfigFile::System, &[("time.network", "off")]),
    );
    assert_eq!(decode_reply(&reply), Err(Errno::NoSpace));
}

#[test]
fn a_read_answers_the_document_as_it_stands_and_is_recorded() {
    let layers = RefCell::new(Layers::default());
    let log = Recorded::default();
    let neither = serve(
        &layers,
        &log,
        &caller(true),
        &read(SystemConfigFile::Network),
    );
    assert_eq!(decode_reply(&neither), Ok(""));

    layers.borrow_mut().image[slot(SystemConfigFile::Network)] = Some(SHIPPED_NETWORK.to_string());
    let image = serve(
        &layers,
        &log,
        &caller(true),
        &read(SystemConfigFile::Network),
    );
    assert_eq!(decode_reply(&image), Ok(SHIPPED_NETWORK));

    layers.borrow_mut().administrator[slot(SystemConfigFile::Network)] =
        Some("lan.mtu 1500\n".to_string());
    let administrator = serve(
        &layers,
        &log,
        &caller(true),
        &read(SystemConfigFile::Network),
    );
    assert_eq!(decode_reply(&administrator), Ok("lan.mtu 1500\n"));
    assert!(log
        .0
        .borrow()
        .iter()
        .all(|(id, uid, _)| *id == events::READ_ANSWERED && *uid == Some(u64::from(ADMIN))));

    layers.borrow_mut().read_refusal = Some(Errno::MediumError);
    let refused = serve(
        &layers,
        &log,
        &caller(true),
        &read(SystemConfigFile::Network),
    );
    assert_eq!(decode_reply(&refused), Err(Errno::MediumError));
    assert_eq!(
        log.0.borrow().last().map(|(id, ..)| *id),
        Some(events::READ_REFUSED)
    );
}

#[test]
fn a_caller_not_holding_the_capability_is_refused_whatever_it_asks() {
    let layers = RefCell::new(Layers::default());
    layers.borrow_mut().image[slot(SystemConfigFile::Network)] = Some(SHIPPED_NETWORK.to_string());
    let log = Recorded::default();
    for request in [
        read(SystemConfigFile::Network),
        change(SystemConfigFile::System, &[("time.network", "off")]),
    ] {
        let reply = serve(&layers, &log, &caller(false), &request);
        assert_eq!(decode_reply(&reply), Err(Errno::PermissionDenied));
    }
    assert_eq!(layers.borrow().replaced, 0);
    assert!(log
        .0
        .borrow()
        .iter()
        .all(|(id, ..)| *id == events::REQUEST_REFUSED));
}

#[test]
fn a_malformed_request_is_refused_and_recorded() {
    let layers = RefCell::new(Layers::default());
    let log = Recorded::default();
    let mut request = change(SystemConfigFile::System, &[("time.network", "off")]);
    request[0] ^= 1;
    let reply = serve(&layers, &log, &caller(true), &request);
    assert_eq!(decode_reply(&reply), Err(Errno::BadMagic));
    assert_eq!(layers.borrow().replaced, 0);
    assert_eq!(
        *log.0.borrow(),
        [(
            events::REQUEST_REFUSED,
            Some(u64::from(ADMIN)),
            Some(i64::from(Errno::BadMagic.as_i32()))
        )]
    );
}

#[test]
fn the_document_ceilings_are_the_engines_own() {
    assert_eq!(
        super::document_ceiling(SystemConfigFile::System),
        tairix_sysconfig::MAX_CONFIG_LEN
    );
    assert_eq!(
        super::document_ceiling(SystemConfigFile::Network),
        tairix_netconfig::MAX_CONFIG_LEN
    );
}
