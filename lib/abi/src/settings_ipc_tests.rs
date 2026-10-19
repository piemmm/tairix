extern crate std;

use std::string::String;
use std::vec;
use std::vec::Vec;

use super::{
    decode_reply, encode_read, encode_reply, encode_set, set_request_len, SettingsRequest,
    DOCUMENT_MAX, HEADER_LEN, MAX_SETTINGS, NAME_MAX, READ_REQUEST_LEN, SETTINGS_MAGIC,
    SETTINGS_REPLY_MAX, SETTINGS_REQUEST_MAX, SETTING_HEADER_LEN,
};
use crate::driver_store::SystemConfigFile;
use crate::Errno;

fn encoded(document: SystemConfigFile, settings: &[(&str, &str)]) -> Vec<u8> {
    let mut out = vec![0u8; SETTINGS_REQUEST_MAX];
    let len = encode_set(document, settings, &mut out).expect("the change encodes");
    out.truncate(len);
    out
}

fn decoded(bytes: &[u8]) -> (SystemConfigFile, Vec<(String, String)>) {
    let Ok(SettingsRequest::Set { document, settings }) = SettingsRequest::decode(bytes) else {
        panic!("a change decodes as one");
    };
    let settings = settings
        .iter()
        .map(|(name, value)| (String::from(name), String::from(value)))
        .collect();
    (document, settings)
}

fn set_count(bytes: &mut [u8], count: usize) {
    let count = u16::try_from(count).expect("fits");
    bytes[8..10].copy_from_slice(&count.to_le_bytes());
}

#[test]
fn a_change_round_trips_in_order() {
    let settings = [("time.network", "off"), ("time.zone", "Europe/London")];
    let bytes = encoded(SystemConfigFile::System, &settings);
    let (document, read) = decoded(&bytes);
    assert_eq!(document, SystemConfigFile::System);
    assert_eq!(
        read,
        [
            (String::from("time.network"), String::from("off")),
            (String::from("time.zone"), String::from("Europe/London")),
        ]
    );
    let Ok(SettingsRequest::Set { settings, .. }) = SettingsRequest::decode(&bytes) else {
        panic!("a change decodes as one");
    };
    assert_eq!(settings.iter().len(), 2);
}

#[test]
fn a_buffer_of_the_stated_length_holds_the_change_exactly() {
    for settings in [
        &[("time.network", "off")][..],
        &[("time.network", "off"), ("time.zone", "Europe/London")][..],
        &[("wan.ipv4.address", "")][..],
    ] {
        let len = set_request_len(settings);
        let mut out = vec![0u8; len];
        assert_eq!(
            encode_set(SystemConfigFile::System, settings, &mut out),
            Ok(len)
        );
        assert_eq!(
            encode_set(SystemConfigFile::System, settings, &mut out[..len - 1]),
            Err(Errno::BufferTooSmall),
            "not a byte to spare"
        );
    }
    let mut read = [0u8; READ_REQUEST_LEN];
    assert_eq!(
        encode_read(SystemConfigFile::Network, &mut read),
        Ok(READ_REQUEST_LEN)
    );
}

#[test]
fn a_read_round_trips_and_names_no_settings() {
    for document in [SystemConfigFile::System, SystemConfigFile::Network] {
        let mut out = [0u8; HEADER_LEN];
        let len = encode_read(document, &mut out).expect("the read encodes");
        assert_eq!(
            SettingsRequest::decode(&out[..len]),
            Ok(SettingsRequest::Read { document })
        );
        let mut naming = out.to_vec();
        set_count(&mut naming, 1);
        assert_eq!(SettingsRequest::decode(&naming), Err(Errno::OutOfRange));
        let mut padded = out.to_vec();
        padded.push(0);
        assert_eq!(SettingsRequest::decode(&padded), Err(Errno::OutOfRange));
    }
    assert_eq!(
        encode_read(SystemConfigFile::System, &mut [0u8; HEADER_LEN - 1]),
        Err(Errno::BufferTooSmall)
    );
}

#[test]
fn an_empty_value_is_carried_because_it_is_how_a_setting_is_unset() {
    let bytes = encoded(SystemConfigFile::Network, &[("wan.ipv4.address", "")]);
    let (document, read) = decoded(&bytes);
    assert_eq!(document, SystemConfigFile::Network);
    assert_eq!(read, [(String::from("wan.ipv4.address"), String::new())]);
}

#[test]
fn a_change_of_nothing_or_too_much_is_refused_both_ways() {
    let mut out = vec![0u8; SETTINGS_REQUEST_MAX];
    assert_eq!(
        encode_set(SystemConfigFile::System, &[], &mut out),
        Err(Errno::LengthOutOfRange)
    );
    let names: Vec<String> = (0..=MAX_SETTINGS).map(|i| std::format!("k{i}")).collect();
    let many: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), "v")).collect();
    assert_eq!(
        encode_set(SystemConfigFile::System, &many, &mut out),
        Err(Errno::LengthOutOfRange)
    );
    assert!(encode_set(SystemConfigFile::System, &many[..MAX_SETTINGS], &mut out).is_ok());

    let mut none = encoded(SystemConfigFile::System, &[("a", "b")]);
    none.truncate(HEADER_LEN);
    set_count(&mut none, 0);
    assert_eq!(SettingsRequest::decode(&none), Err(Errno::OutOfRange));
    let mut over = encoded(SystemConfigFile::System, &[("a", "b")]);
    set_count(&mut over, MAX_SETTINGS + 1);
    assert_eq!(SettingsRequest::decode(&over), Err(Errno::OutOfRange));
}

#[test]
fn names_are_bounded_and_never_empty() {
    let mut out = vec![0u8; SETTINGS_REQUEST_MAX];
    let longest = "n".repeat(NAME_MAX);
    assert!(encode_set(SystemConfigFile::System, &[(&longest, "v")], &mut out).is_ok());
    let over = "n".repeat(NAME_MAX + 1);
    assert_eq!(
        encode_set(SystemConfigFile::System, &[(&over, "v")], &mut out),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        encode_set(SystemConfigFile::System, &[("", "v")], &mut out),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn the_values_together_are_bounded() {
    let mut out = vec![0u8; SETTINGS_REQUEST_MAX];
    let half = "v".repeat(DOCUMENT_MAX / 2);
    let rest = "v".repeat(DOCUMENT_MAX - half.len());
    assert!(encode_set(
        SystemConfigFile::System,
        &[("a", &half), ("b", &rest)],
        &mut out
    )
    .is_ok());
    let more = std::format!("{rest}v");
    assert_eq!(
        encode_set(
            SystemConfigFile::System,
            &[("a", &half), ("b", &more)],
            &mut out
        ),
        Err(Errno::LengthOutOfRange)
    );

    // A frame no encoder writes: each value fits alone, both together do not.
    let mut forged = encoded(SystemConfigFile::System, &[("a", &half), ("b", &rest)]);
    let at = HEADER_LEN + SETTING_HEADER_LEN + 1 + half.len();
    forged.insert(at + SETTING_HEADER_LEN + 1, b'v');
    let longer = u16::try_from(rest.len() + 1).expect("fits");
    forged[at + 1..at + 3].copy_from_slice(&longer.to_le_bytes());
    assert_eq!(SettingsRequest::decode(&forged), Err(Errno::OutOfRange));
}

#[test]
fn a_control_character_is_refused_in_a_name_or_a_value() {
    let mut out = vec![0u8; SETTINGS_REQUEST_MAX];
    for (name, value) in [
        ("time.zone", "Europe/London\nos.loginType text"),
        ("time.zone", "a\tb"),
        ("time\r.zone", "x"),
        ("time.zone", "\u{7f}"),
    ] {
        assert_eq!(
            encode_set(SystemConfigFile::System, &[(name, value)], &mut out),
            Err(Errno::OutOfRange),
            "{name:?} = {value:?}"
        );
    }
    let mut forged = encoded(SystemConfigFile::System, &[("time.zone", "London")]);
    let last = forged.len() - 1;
    forged[last] = b'\n';
    assert_eq!(SettingsRequest::decode(&forged), Err(Errno::OutOfRange));
}

#[test]
fn text_that_is_not_utf8_is_refused() {
    let mut forged = encoded(SystemConfigFile::System, &[("time.zone", "London")]);
    let last = forged.len() - 1;
    forged[last] = 0xFF;
    assert_eq!(SettingsRequest::decode(&forged), Err(Errno::OutOfRange));
}

#[test]
fn the_header_is_checked_whole() {
    let good = encoded(SystemConfigFile::System, &[("time.network", "on")]);
    assert!(SettingsRequest::decode(&good).is_ok());

    let mut magic = good.clone();
    magic[0] ^= 1;
    assert_eq!(SettingsRequest::decode(&magic), Err(Errno::BadMagic));
    let mut version = good.clone();
    version[4] = 2;
    assert_eq!(
        SettingsRequest::decode(&version),
        Err(Errno::AbiVersionUnsupported)
    );
    for operation in [0, 3, u8::MAX] {
        let mut unknown = good.clone();
        unknown[6] = operation;
        assert_eq!(
            SettingsRequest::decode(&unknown),
            Err(Errno::OutOfRange),
            "operation {operation}"
        );
    }
    let mut document = good.clone();
    document[7] = 2;
    assert_eq!(SettingsRequest::decode(&document), Err(Errno::OutOfRange));
    for reserved in [10, 11] {
        let mut dirty = good.clone();
        dirty[reserved] = 1;
        assert_eq!(
            SettingsRequest::decode(&dirty),
            Err(Errno::OutOfRange),
            "byte {reserved}"
        );
    }
    assert_eq!(
        u32::from_le_bytes(good[..4].try_into().expect("four bytes")),
        SETTINGS_MAGIC
    );
}

#[test]
fn a_truncated_or_padded_change_is_refused() {
    let good = encoded(
        SystemConfigFile::Network,
        &[("wan.kind", "ethernet"), ("wan.mtu", "1500")],
    );
    for len in 0..good.len() {
        assert!(
            SettingsRequest::decode(&good[..len]).is_err(),
            "{len} of {} bytes",
            good.len()
        );
    }
    let mut padded = good.clone();
    padded.push(0);
    assert_eq!(SettingsRequest::decode(&padded), Err(Errno::OutOfRange));
}

#[test]
fn an_encoder_short_of_room_refuses() {
    let settings = [("time.network", "off")];
    let whole = encoded(SystemConfigFile::System, &settings).len();
    for room in 0..whole {
        let mut out = vec![0u8; room];
        assert_eq!(
            encode_set(SystemConfigFile::System, &settings, &mut out),
            Err(Errno::BufferTooSmall),
            "{room} bytes"
        );
    }
}

#[test]
fn the_longest_change_fits_the_request_bound_exactly() {
    let names: Vec<String> = (0..MAX_SETTINGS)
        .map(|i| std::format!("{i:0>NAME_MAX$}"))
        .collect();
    let mut values = vec![String::new(); MAX_SETTINGS];
    values[0] = "v".repeat(DOCUMENT_MAX);
    let settings: Vec<(&str, &str)> = names
        .iter()
        .zip(&values)
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let bytes = encoded(SystemConfigFile::Network, &settings);
    assert_eq!(bytes.len(), SETTINGS_REQUEST_MAX);
    let (_, read) = decoded(&bytes);
    assert_eq!(read.len(), MAX_SETTINGS);
}

#[test]
fn a_reply_round_trips_the_document_or_the_refusal() {
    let mut out = vec![0u8; SETTINGS_REPLY_MAX];
    for document in ["", "wan.kind ethernet\nwan.ipv4.method dhcp\n"] {
        let len = encode_reply(Ok(document), &mut out).expect("the reply encodes");
        assert_eq!(decode_reply(&out[..len]), Ok(document));
    }
    let longest = "x".repeat(DOCUMENT_MAX);
    let len = encode_reply(Ok(&longest), &mut out).expect("the longest reply encodes");
    assert_eq!(len, SETTINGS_REPLY_MAX);
    assert_eq!(decode_reply(&out[..len]), Ok(longest.as_str()));
    let over = "x".repeat(DOCUMENT_MAX + 1);
    assert_eq!(
        encode_reply(Ok(&over), &mut out),
        Err(Errno::LengthOutOfRange)
    );

    let len = encode_reply(Err(Errno::PermissionDenied), &mut out).expect("a refusal encodes");
    assert_eq!(decode_reply(&out[..len]), Err(Errno::PermissionDenied));
}

#[test]
fn a_reply_that_disagrees_with_itself_is_refused() {
    let mut out = vec![0u8; SETTINGS_REPLY_MAX];
    let len = encode_reply(Ok("time.network off\n"), &mut out).expect("the reply encodes");
    let good = out[..len].to_vec();
    for cut in 0..good.len() {
        assert!(decode_reply(&good[..cut]).is_err(), "{cut} bytes");
    }
    let mut padded = good.clone();
    padded.push(b'x');
    assert_eq!(decode_reply(&padded), Err(Errno::OutOfRange));
    let mut invalid = good.clone();
    let last = invalid.len() - 1;
    invalid[last] = 0xFF;
    assert_eq!(decode_reply(&invalid), Err(Errno::OutOfRange));
    let mut corrupt = good;
    corrupt[..4].copy_from_slice(&i32::MIN.to_le_bytes());
    assert_eq!(decode_reply(&corrupt), Err(Errno::OutOfRange));
}
