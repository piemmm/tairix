use tairix_abi::driver::filesystem::{FilesystemAttrsProvider, FilesystemRead, WriteRefusal};
use tairix_abi::volume::{FormatName, VolumeFsType};
use tairix_abi::CapabilityId;

use super::{bind_key, Opened, REQUIRED_CAPABILITIES};
use crate::testfs::TestFs;

#[test]
fn every_format_binds_the_nodes_published_for_it_alone() {
    let formats = VolumeFsType::ALL;
    for fstype in formats {
        let key = bind_key(fstype);
        for other in formats {
            assert_eq!(
                key.key.compatible_bytes() == other.served_compatible(),
                fstype == other,
                "{fstype:?} against {other:?}"
            );
        }
    }
}

#[test]
fn a_served_driver_holds_the_serving_authority_and_its_diagnostics_only() {
    assert_eq!(
        REQUIRED_CAPABILITIES,
        [CapabilityId::FS_SERVE, CapabilityId::LOG_EMIT]
    );
}

#[test]
fn the_offer_is_what_the_driver_opened() {
    let mut opened = Opened {
        fs: TestFs::new(),
        format: FormatName::Fat32,
        write_refusal: Some(WriteRefusal::FormatReadOnly),
    };
    let offer = opened.offer();
    assert_eq!(offer.root, opened.fs.root());
    assert_eq!(offer.name_matching, opened.fs.name_matching());
    assert_eq!(offer.node_identity, opened.fs.node_identity());
    assert_eq!(offer.attrs, opened.fs.attrs_fs().is_some());
    assert_eq!(offer.write_refusal, Some(WriteRefusal::FormatReadOnly));
    assert_eq!(offer.format, FormatName::Fat32);
}

/// A volume whose driver keeps no title — its format keeps none, or this
/// volume cannot, as an encrypted ARXFS one cannot — is offered untitled, so
/// nothing offers to retitle it only to be refused.
#[test]
fn the_offer_says_whether_the_volume_keeps_a_title() {
    let mut opened = Opened {
        fs: TestFs::new(),
        format: FormatName::Fat32,
        write_refusal: None,
    };
    opened.fs.label = None;
    assert!(!opened.offer().title);
    opened.fs.label =
        Some(tairix_abi::driver::filesystem::VolumeTitle::new("Disk").expect("a title"));
    assert!(opened.offer().title);
}
