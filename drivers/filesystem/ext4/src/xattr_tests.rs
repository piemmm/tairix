use super::*;

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::MAX_ACL_ENTRIES;
use tairix_abi::driver::filesystem::{FilesystemRead, FilesystemSecurity};

use crate::tests::{sparse, SizedBlock};

/// The `/f` ACL e2fsprogs wrote into the fixtures, as stored on disk.
const FIXTURE_ACL: [u8; 36] = [
    0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x06, 0x00, 0x02, 0x00, 0x06, 0x00, 0xE8, 0x03, 0x00, 0x00,
    0x04, 0x00, 0x04, 0x00, 0x08, 0x00, 0x04, 0x00, 0xD0, 0x07, 0x00, 0x00, 0x10, 0x00, 0x06, 0x00,
    0x20, 0x00, 0x04, 0x00,
];

const ACL_BLOCK: &[u8] = include_bytes!("../tests/fixtures/acl_block.sparse");
/// `/f`'s attribute block in that fixture.
const FIXTURE_XATTR_BLOCK: u64 = 33;

/// An ext4-encoded ACL of `(tag, perm, id)` entries; `id` is written only
/// for the named tags.
fn acl(entries: &[(u16, u16, u32)]) -> Vec<u8> {
    let mut value = ACL_VERSION.to_le_bytes().to_vec();
    for &(tag, perm, id) in entries {
        value.extend_from_slice(&tag.to_le_bytes());
        value.extend_from_slice(&perm.to_le_bytes());
        if tag == ACL_USER || tag == ACL_GROUP {
            value.extend_from_slice(&id.to_le_bytes());
        }
    }
    value
}

#[test]
fn the_on_disk_encoding_decodes_entry_by_entry() {
    let decoded = PosixAcl::decode(&FIXTURE_ACL).expect("e2fsprogs' encoding");
    assert_eq!(decoded.user_obj, 0o6);
    assert_eq!(decoded.group_obj, 0o4);
    assert_eq!(decoded.mask, Some(0o6));
    assert_eq!(decoded.other, 0o4);
    assert_eq!(
        decoded.named,
        [
            (SecuritySubject::User(1000), 0o6),
            (SecuritySubject::Group(2000), 0o4)
        ]
    );
}

#[test]
fn the_userspace_encoding_is_not_the_disk_one() {
    // `getxattr`'s form: version 2, every entry eight bytes. A volume never
    // holds it, so meeting it is damage, not an ACL to read.
    let mut vfs = 2u32.to_le_bytes().to_vec();
    for (tag, perm) in [(ACL_USER_OBJ, 6u16), (ACL_GROUP_OBJ, 4), (ACL_OTHER, 4)] {
        vfs.extend_from_slice(&tag.to_le_bytes());
        vfs.extend_from_slice(&perm.to_le_bytes());
        vfs.extend_from_slice(&u32::MAX.to_le_bytes());
    }
    assert_eq!(PosixAcl::decode(&vfs), Err(DriverError::DeviceFault));
}

#[test]
fn a_malformed_acl_grants_nothing() {
    let base = [
        (ACL_USER_OBJ, 6, 0),
        (ACL_GROUP_OBJ, 4, 0),
        (ACL_OTHER, 0, 0),
    ];
    let cases: [(&str, Vec<u8>); 6] = [
        ("no owner entry", acl(&base[1..])),
        (
            "a repeated owning-group entry",
            acl(&[base[0], base[1], base[1], base[2]]),
        ),
        (
            "named entries with no mask",
            acl(&[base[0], (ACL_USER, 7, 5), base[1], base[2]]),
        ),
        (
            "a subject named twice",
            acl(&[
                base[0],
                (ACL_USER, 4, 5),
                (ACL_USER, 7, 5),
                base[1],
                (ACL_MASK, 7, 0),
                base[2],
            ]),
        ),
        (
            "an unknown tag",
            acl(&[base[0], (0x40, 7, 0), base[1], base[2]]),
        ),
        ("a torn entry", {
            let mut torn = acl(&[
                base[0],
                (ACL_USER, 7, 5),
                base[1],
                (ACL_MASK, 7, 0),
                base[2],
            ]);
            torn.truncate(10);
            torn
        }),
    ];
    for (what, value) in cases {
        assert_eq!(
            PosixAcl::decode(&value),
            Err(DriverError::DeviceFault),
            "{what}"
        );
    }
}

#[test]
fn an_acl_with_a_mask_takes_the_posix_model() {
    let mut sec = NodeSecurity::new(0o4777, 1, 1);
    PosixAcl::decode(&FIXTURE_ACL)
        .expect("decodes")
        .apply(&mut sec)
        .expect("applies");
    assert_eq!(
        sec.mode, 0o4664,
        "special bits kept, triads from the entries"
    );
    assert_eq!(
        sec.acl_model,
        AclModel::Posix {
            group_obj: 0o4,
            mask: 0o6
        }
    );
    assert_eq!(sec.acl().len(), 2);
}

#[test]
fn a_minimal_acl_is_the_mode_alone() {
    let value = acl(&[
        (ACL_USER_OBJ, 7, 0),
        (ACL_GROUP_OBJ, 5, 0),
        (ACL_OTHER, 1, 0),
    ]);
    let mut sec = NodeSecurity::new(0o600, 1, 1);
    PosixAcl::decode(&value)
        .expect("decodes")
        .apply(&mut sec)
        .expect("applies");
    assert_eq!(sec.mode, 0o751);
    assert_eq!(sec.acl_model, AclModel::Additive);
    assert!(sec.acl().is_empty());
}

#[test]
fn more_named_entries_than_a_record_holds_is_refused() {
    let mut entries = vec![(ACL_USER_OBJ, 7, 0)];
    for uid in 0..=u32::try_from(MAX_ACL_ENTRIES).expect("small") {
        entries.push((ACL_USER, 4, uid));
    }
    entries.extend([(ACL_GROUP_OBJ, 0, 0), (ACL_MASK, 4, 0), (ACL_OTHER, 0, 0)]);
    let mut sec = NodeSecurity::new(0, 0, 0);
    let decoded = PosixAcl::decode(&acl(&entries)).expect("decodes");
    assert_eq!(decoded.apply(&mut sec), Err(DriverError::Unsupported));
}

#[test]
fn a_mode_change_rewrites_the_mirrored_entries_only() {
    let mut value = FIXTURE_ACL;
    mirror_mode(&mut value, 0o750).expect("mirrors");
    let after = PosixAcl::decode(&value).expect("decodes");
    assert_eq!(
        (after.user_obj, after.mask, after.other),
        (0o7, Some(0o5), 0o0)
    );
    assert_eq!(after.group_obj, 0o4, "the mask carries the group triad");
    assert_eq!(
        after.named,
        PosixAcl::decode(&FIXTURE_ACL).expect("decodes").named
    );

    let mut minimal = acl(&[
        (ACL_USER_OBJ, 6, 0),
        (ACL_GROUP_OBJ, 4, 0),
        (ACL_OTHER, 4, 0),
    ]);
    mirror_mode(&mut minimal, 0o711).expect("mirrors");
    let after = PosixAcl::decode(&minimal).expect("decodes");
    assert_eq!(
        (after.user_obj, after.group_obj, after.other),
        (0o7, 0o1, 0o1)
    );
}

#[test]
fn the_entry_hash_matches_e2fsprogs() {
    assert_eq!(
        entry_hash(b"", &FIXTURE_ACL),
        0x03F7_07F3,
        "debugfs's e_hash for /f"
    );
}

#[test]
fn a_block_hash_is_zero_while_any_entry_hash_is() {
    let mut block = vec![0u8; 1024];
    put_le32(&mut block, 0, MAGIC);
    for (k, hash) in [(0usize, 0x1111_1111u32), (1, 0)] {
        let at = BLOCK_HEADER + k * 16;
        put_le32(&mut block, at + 12, hash);
        put_le32(&mut block, at + 8, 4);
        put_le16(&mut block, at + 2, 1000);
    }
    assert_eq!(block_hash(&block), 0);
    put_le32(&mut block, BLOCK_HEADER + 16 + 12, 0x2222_2222);
    assert_ne!(block_hash(&block), 0);
}

/// The fixture with `/f`'s attribute block recorded as shared by `refs`
/// inodes, resealed.
fn shared(refs: u32) -> Ext4<SizedBlock> {
    let mut image = sparse(ACL_BLOCK);
    let at = usize::try_from(FIXTURE_XATTR_BLOCK).expect("small") * 1024;
    put_le32(&mut image[at..at + 1024], BLOCK_REFCOUNT, refs);
    let seeded = Ext4::open(SizedBlock::over(image.clone())).expect("opens");
    let sum = seeded.xattr_block_checksum(FIXTURE_XATTR_BLOCK, &image[at..at + 1024]);
    put_le32(&mut image[at..at + 1024], BLOCK_CHECKSUM, sum);
    Ext4::open(SizedBlock::over(image)).expect("reopens")
}

#[test]
fn a_mode_change_copies_a_shared_attribute_block_first() {
    let mut fs = shared(2);
    let file = fs.lookup(fs.root(), b"f").expect("f");
    let mut raw = vec![0u8; 128];
    fs.read_inode_raw(12, &mut raw).expect("the record");
    let counted = crate::file::sectors(&raw, &fs.layout);
    let mut sec = fs.security(file).expect("security");
    sec.mode = 0o600;
    fs.set_security(file, sec).expect("chmod");
    fs.read_inode_raw(12, &mut raw).expect("the record");
    assert_eq!(
        crate::file::sectors(&raw, &fs.layout),
        counted,
        "it counted the shared block, and counts its own copy instead"
    );
    let copy = block_of(&raw, &fs.layout).expect("an attribute block");
    assert_ne!(copy, FIXTURE_XATTR_BLOCK, "this inode's own copy");
    let mut buf = vec![0u8; 1024];
    fs.read_xattr_block(FIXTURE_XATTR_BLOCK, &mut buf)
        .expect("the original");
    assert_eq!(le32(&buf, BLOCK_REFCOUNT), 1, "the other sharer keeps it");
    let found = find(&buf, BLOCK_HEADER, 0, INDEX_POSIX_ACL_ACCESS, b"")
        .expect("parses")
        .expect("present");
    assert_eq!(
        &buf[found.value.0..found.value.1],
        &FIXTURE_ACL,
        "unchanged"
    );
    fs.read_xattr_block(copy, &mut buf).expect("the copy");
    assert_eq!(le32(&buf, BLOCK_REFCOUNT), 1);
    let mut fs = Ext4::open(fs.into_block().expect("commit")).expect("reopens");
    let file = fs.lookup(fs.root(), b"f").expect("f");
    assert_eq!(fs.security(file).expect("both blocks sealed").mode, 0o600);
}

#[test]
fn deleting_releases_a_block_only_with_its_last_sharer() {
    for refs in [1, 2] {
        let mut fs = shared(refs);
        let mut raw = vec![0u8; 128];
        fs.read_inode_raw(12, &mut raw).expect("the record");
        fs.release_xattr_block(&mut raw).expect("released");
        assert_eq!(block_of(&raw, &fs.layout), None);
        let freed = fs
            .freed
            .overlaps(FIXTURE_XATTR_BLOCK, FIXTURE_XATTR_BLOCK + 1);
        assert_eq!(freed, refs == 1, "{refs} sharer(s)");
        if refs == 2 {
            let mut buf = vec![0u8; 1024];
            fs.read_xattr_block(FIXTURE_XATTR_BLOCK, &mut buf)
                .expect("still sealed");
            assert_eq!(le32(&buf, BLOCK_REFCOUNT), 1);
        }
    }
}
