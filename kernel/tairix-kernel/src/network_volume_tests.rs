//! Unit tests for what a network volume is derived from: its identity, and
//! the presentation a principal may ask for. Its lifecycle over the real
//! service is the volume service's scenarios'.

use tairix_abi::netattach::Presentation;
use tairix_abi::{AppIdentity, CapabilityId, CapabilityQuery, Errno, PublisherId};
use tairix_kernel_core::fs::Principal;

use super::{identity, presented};

fn app(bundle: &str, publisher: u8) -> AppIdentity {
    AppIdentity::new(bundle, PublisherId::from_raw([publisher; 32])).expect("an app")
}

struct Holding(Option<CapabilityId>);
impl CapabilityQuery for Holding {
    fn holds(&self, cap: CapabilityId) -> bool {
        self.0 == Some(cap)
    }
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

#[test]
fn others_are_shown_a_volume_only_by_the_mount_authority() {
    let nothing = Holding(None);
    let caller = Principal {
        uid: 1000,
        gid: 100,
        caps: &nothing,
    };
    let private = presented(&caller, Presentation::Private).expect("anyone's own");
    assert_eq!(
        private,
        crate::volume_policy::Presented::private(1000, tairix_kernel_sec::GroupId(100))
    );
    let explicit = Presentation::Explicit {
        uid: 42,
        gid: 7,
        dir_mode: 0o750,
        file_mode: 0o640,
    };
    for asked in [Presentation::Shared, explicit] {
        assert_eq!(presented(&caller, asked), Err(Errno::PermissionDenied));
    }
    let mount = Holding(Some(CapabilityId::FS_MOUNT));
    let holder = Principal {
        caps: &mount,
        ..caller
    };
    assert_eq!(
        presented(&holder, explicit),
        Ok(crate::volume_policy::Presented::explicit(
            42,
            tairix_kernel_sec::GroupId(7),
            0o750,
            0o640
        ))
    );
}
