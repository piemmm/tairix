//! Unit tests for what a network volume's identity is derived from. Its
//! lifecycle over the real service is the volume service's scenarios'.

use tairix_abi::{AppIdentity, PublisherId};

use super::identity;

fn app(bundle: &str, publisher: u8) -> AppIdentity {
    AppIdentity::new(bundle, PublisherId::from_raw([publisher; 32])).expect("an app")
}

#[test]
fn a_volumes_identity_is_its_servers_its_principals_and_its_keys() {
    let key = *b"0123456789abcdef";
    let server = app("org.tairix.smbfs", 1);
    let base = identity(&server, 1000, &key);
    assert_eq!(
        identity(&server, 1000, &key),
        base,
        "the same volume each time"
    );
    for other in [
        identity(&app("org.tairix.smbfs", 2), 1000, &key),
        identity(&app("org.tairix.smbfs2", 1), 1000, &key),
        identity(&server, 1001, &key),
        identity(&server, 1000, b"0123456789abcdeg"),
    ] {
        assert_ne!(other, base);
    }
    assert_ne!(base, [0; 16]);
}
