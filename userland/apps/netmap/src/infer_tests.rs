use super::*;
use alloc::vec;

extern crate std;

fn advertised(service: &str, text: &[&str]) -> AdvertisedService {
    AdvertisedService {
        instance: String::from("Some Device"),
        service: String::from(service),
        interface: [0; tairix_abi::net_ipc::IF_NAME_LEN],
        port: 1,
        host: String::from("device.local"),
        addresses: Vec::new(),
        text: text.iter().map(|t| String::from(*t)).collect(),
    }
}

fn of(
    role: Option<Role>,
    vendor: Option<&str>,
    services: &[AdvertisedService],
) -> (Classification, Vec<Evidence>) {
    let services: Vec<&AdvertisedService> = services.iter().collect();
    infer(&Signals {
        role,
        vendor,
        services: &services,
    })
}

#[test]
fn a_host_no_rule_speaks_for_is_unknown() {
    let (class, evidence) = of(
        None,
        Some("Acme Widgets Ltd"),
        &[advertised("_http._tcp", &[])],
    );
    assert_eq!(class, Classification::default());
    assert!(evidence.is_empty(), "nothing is guessed from nothing");
}

#[test]
fn the_default_gateway_is_a_router() {
    let (class, evidence) = of(Some(Role::Gateway), None, &[]);
    assert_eq!(
        (class.device, class.device_confidence),
        (DeviceClass::Router, GATEWAY_WEIGHT)
    );
    assert_eq!((class.os, class.os_confidence), (OsFamily::Unknown, 0));
    assert_eq!(evidence[0].seen, Seen::Gateway);
}

#[test]
fn agreeing_hints_add_up_without_passing_certainty() {
    let printer = [
        advertised("_ipp._tcp", &["ty=Brother HL-L2350DW series"]),
        advertised("_pdl-datastream._tcp", &[]),
    ];
    let (class, evidence) = of(None, Some("Brother Industries, LTD."), &printer);
    assert_eq!(class.device, DeviceClass::Printer);
    // 1 - (0.2 * 0.2 * 0.3) of the doubt is gone.
    assert_eq!(class.device_confidence, 98);
    assert_eq!(evidence.len(), 3);
}

#[test]
fn a_signal_seen_twice_counts_once() {
    let (class, evidence) = of(
        None,
        None,
        &[advertised("_ipp._tcp", &[]), advertised("_IPP._tcp", &[])],
    );
    assert_eq!(class.device_confidence, 80);
    assert_eq!(
        evidence,
        [Evidence {
            seen: Seen::Service(String::from("_ipp._tcp")),
            device: Some((DeviceClass::Printer, 80)),
            os: None,
        }]
    );
}

#[test]
fn a_named_model_says_what_the_machine_is_and_runs() {
    let info = advertised("_device-info._tcp", &["model=MacBookPro18,3", "osxvers=21"]);
    let (class, evidence) = of(None, Some("Apple, Inc."), &[info]);
    assert_eq!(
        (class.device, class.device_confidence),
        (DeviceClass::Laptop, 95)
    );
    assert_eq!(class.os, OsFamily::MacOs);
    assert_eq!(class.os_confidence, 99);
    assert!(evidence.contains(&Evidence {
        seen: Seen::Model(String::from("MacBookPro18,3")),
        device: Some((DeviceClass::Laptop, 95)),
        os: Some((OsFamily::MacOs, 95)),
    }));
    assert!(evidence.iter().any(|e| e.seen == Seen::MacOsVersion));
}

#[test]
fn a_model_matches_by_its_whole_family_name() {
    let model = |value: &str| {
        let text = alloc::format!("model={value}");
        of(
            None,
            None,
            &[advertised("_device-info._tcp", &[text.as_str()])],
        )
        .0
    };
    assert_eq!(model("Macmini9,1").device, DeviceClass::Desktop);
    assert_eq!(model("MacBookAir10,1").device, DeviceClass::Laptop);
    assert_eq!(model("iMacPro1,1").device, DeviceClass::Desktop);
    assert_eq!(
        (model("Mac14,2").device, model("Mac14,2").device_confidence),
        (DeviceClass::Desktop, 40),
        "a current model number says Mac, not which kind"
    );
    assert_eq!(
        model("MacSamba"),
        Classification::default(),
        "a file server's own name"
    );
    assert_eq!(
        model("macbookpro18,3"),
        Classification::default(),
        "models match as spelt"
    );
    let tv = of(None, None, &[advertised("_raop._tcp", &["AM=AppleTV11,1"])]).0;
    assert_eq!((tv.device, tv.os), (DeviceClass::Iot, OsFamily::Ios));
}

#[test]
fn a_vendor_matches_by_its_leading_words_in_any_case() {
    let vendor = |name: &str| of(None, Some(name), &[]).0;
    assert_eq!(vendor("CISCO SYSTEMS, INC").device, DeviceClass::Router);
    assert_eq!(
        vendor("Cisco Meraki").device,
        DeviceClass::AccessPoint,
        "the longest entry speaks"
    );
    assert_eq!(
        vendor("Ciscoe Ltd"),
        Classification::default(),
        "a whole word, not a prefix"
    );
    assert_eq!(vendor("Raspberry Pi Trading Ltd").os, OsFamily::Linux);
}

#[test]
fn conflicting_evidence_lowers_the_winners_confidence() {
    let (class, _) = of(Some(Role::Gateway), Some("Synology Incorporated"), &[]);
    assert_eq!(class.device, DeviceClass::Router);
    assert_eq!(class.device_confidence, 95 - 80 / 2);
    assert_eq!((class.os, class.os_confidence), (OsFamily::Linux, 50));
}

#[test]
fn an_exact_tie_is_no_guess_but_keeps_its_evidence() {
    let (class, evidence) = of(
        None,
        Some("Brother Industries, LTD."),
        &[advertised("_googlecast._tcp", &[])],
    );
    assert_eq!(class.device, DeviceClass::Unknown);
    assert_eq!(class.device_confidence, 0);
    assert_eq!(evidence.len(), 2, "the panel can still say what was seen");
}

#[test]
fn the_rule_tables_are_well_formed() {
    let weights = |hint: &Hint| {
        [hint.device.map(|(_, w)| w), hint.os.map(|(_, w)| w)]
            .into_iter()
            .flatten()
            .all(|w| (1..=100).contains(&w))
            && (hint.device.is_some() || hint.os.is_some())
    };
    for (table, lowercase) in [(VENDORS, true), (SERVICES, true), (MODELS, false)] {
        for (index, (key, hint)) in table.iter().enumerate() {
            assert!(weights(hint), "{key}");
            assert!(
                !lowercase || key.bytes().all(|b| !b.is_ascii_uppercase()),
                "{key}"
            );
            assert!(
                table[index + 1..].iter().all(|(other, _)| other != key),
                "{key} is listed twice"
            );
        }
    }
    for (service, _) in SERVICES {
        assert!(
            service.starts_with('_') && (service.ends_with("._tcp") || service.ends_with("._udp")),
            "{service}"
        );
    }
}

#[test]
fn every_vendor_rule_names_an_organisation_the_registry_lists() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../lib/devids/assets/oui.ids"
    );
    let registry = std::fs::read_to_string(path).expect("the committed OUI snapshot");
    let names: Vec<&str> = registry
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once("  ").map(|(_, name)| name))
        .collect();
    for (prefix, _) in VENDORS {
        let rule = vec![(*prefix, device(DeviceClass::Desktop, 1))];
        assert!(
            names
                .iter()
                .any(
                    |name| longest(&rule, name, true, |next| !next.is_ascii_alphanumeric())
                        .is_some()
                ),
            "no organisation answers to {prefix:?}"
        );
    }
}
