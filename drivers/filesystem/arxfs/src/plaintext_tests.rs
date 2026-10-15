//! The plaintext protection end to end: a volume with no key formats, mounts,
//! keeps its files, checks and rescues, stores what it was given as it was
//! given, catches what rots, and refuses to be opened as anything else.

use super::*;

fn fmt_plain(block_size: u32, block_count: u64) -> ARXFS<MemBlock> {
    publish_boot_hash_key();
    ARXFS::format(
        MemBlock::new(block_size, block_count),
        32,
        Keying::Plaintext,
        &mut TestEntropy::new(),
    )
    .expect("format a plaintext volume")
    .stamped()
}

/// A distinctive payload no metadata or padding could spell by accident.
fn marked_payload() -> alloc::vec::Vec<u8> {
    let mut payload = alloc::vec::Vec::new();
    for _ in 0..200 {
        payload.extend_from_slice(b"PLAINTEXT-MARKER");
    }
    payload
}

#[test]
fn a_plaintext_volume_keeps_its_files_across_a_remount_with_no_key() {
    let name: &[u8] = b"notes.txt";
    let payload = alloc::vec![0x3Cu8; 512 * 5 + 17];
    let mut fs = fmt_plain(512, 256);
    let root = fs.root();
    fs.create(root, name, NodeKind::RegularFile)
        .expect("create");
    fs.create(root, b"folder", NodeKind::Directory)
        .expect("mkdir");
    assert_eq!(fs.write_at(root, name, 0, &payload), Ok(payload.len()));
    let bytes = fs.into_block().expect("the volume closes").bytes();

    let mut back = ARXFS::open(
        MemBlock::from_bytes(bytes.clone(), 512, 256),
        Keying::Plaintext,
    )
    .expect("a plaintext volume mounts with no key");
    let node = back.lookup(back.root(), name).expect("lookup");
    let mut read = alloc::vec![0u8; payload.len()];
    assert_eq!(back.read_at(node, 0, &mut read), Ok(payload.len()));
    assert_eq!(read, payload);
    assert!(back.lookup(back.root(), b"folder").is_ok());

    let mut ro = ARXFS::open_read_only(MemBlock::from_bytes(bytes, 512, 256), Keying::Plaintext)
        .expect("and mounts read-only");
    let node = ro.lookup(ro.root(), name).expect("lookup");
    let mut read = alloc::vec![0u8; payload.len()];
    assert_eq!(ro.read_at(node, 0, &mut read), Ok(payload.len()));
    assert_eq!(read, payload);
}

#[test]
fn a_plaintext_volume_stores_names_and_content_as_written_and_an_encrypted_one_never() {
    let name: &[u8] = b"ZxQvBnMkLpVisible";
    let payload = marked_payload();
    let at_rest = |mut fs: ARXFS<MemBlock>| {
        let root = fs.root();
        fs.create(root, name, NodeKind::RegularFile)
            .expect("create");
        assert_eq!(fs.write_at(root, name, 0, &payload), Ok(payload.len()));
        fs.into_block().expect("the volume closes").bytes()
    };

    let plain = at_rest(fmt_plain(512, 512));
    assert!(
        contains(&plain, name),
        "a plaintext name is stored as written"
    );
    assert!(contains(&plain, b"PLAINTEXT-MARKER"));

    let encrypted = at_rest(fmt(512, 512, 32));
    assert!(!contains(&encrypted, name));
    assert!(!contains(&encrypted, b"PLAINTEXT-MARKER"));
}

#[test]
fn each_protection_is_probed_without_a_key_and_refuses_to_be_opened_as_the_other() {
    let plain = fmt_plain(512, 256)
        .into_block()
        .expect("the volume closes")
        .bytes();
    let encrypted = fmt(512, 256, 32)
        .into_block()
        .expect("the volume closes")
        .bytes();
    let device = |bytes: &alloc::vec::Vec<u8>| MemBlock::from_bytes(bytes.clone(), 512, 256);

    assert_eq!(
        ARXFS::probe_protection(&mut device(&plain)),
        Ok(VolumeProtection::Plaintext)
    );
    assert_eq!(
        ARXFS::probe_protection(&mut device(&encrypted)),
        Ok(VolumeProtection::Encrypted)
    );
    assert_eq!(
        ARXFS::probe_protection(&mut MemBlock::new(512, 256)),
        Err(DriverError::BadMagic)
    );

    assert_eq!(
        ARXFS::open(device(&plain), TEST_KEYING).err(),
        Some(DriverError::Unsupported),
        "a caller expecting protection is never handed a volume without it"
    );
    assert_eq!(
        ARXFS::open_read_only(device(&plain), TEST_KEYING).err(),
        Some(DriverError::Unsupported)
    );
    assert_eq!(
        ARXFS::open(device(&encrypted), Keying::Plaintext).err(),
        Some(DriverError::PermissionDenied)
    );
    assert_eq!(
        ARXFS::open_read_only(device(&encrypted), Keying::Plaintext).err(),
        Some(DriverError::PermissionDenied)
    );
}

#[test]
fn rotted_plaintext_content_is_caught_and_never_returned() {
    let payload = marked_payload();
    let mut fs = fmt_plain(512, 512);
    let root = fs.root();
    fs.create(root, b"f", NodeKind::RegularFile)
        .expect("create");
    assert_eq!(fs.write_at(root, b"f", 0, &payload), Ok(payload.len()));
    let mut bytes = fs.into_block().expect("the volume closes").bytes();
    let at = bytes
        .windows(16)
        .position(|window| window == b"PLAINTEXT-MARKER")
        .expect("the content is on disk");
    bytes[at] ^= 0x20;

    let mut back = ARXFS::open(MemBlock::from_bytes(bytes, 512, 512), Keying::Plaintext)
        .expect("the metadata is intact");
    let node = back.lookup(back.root(), b"f").expect("lookup");
    let mut read = alloc::vec![0u8; payload.len()];
    assert_eq!(
        back.read_at(node, 0, &mut read),
        Err(DriverError::DeviceFault)
    );
}

#[test]
fn a_plaintext_superblock_that_rots_is_served_by_its_mirror_and_the_ring_by_none() {
    let bytes = fmt_plain(512, 256)
        .into_block()
        .expect("the volume closes")
        .bytes();
    let mut one_copy = bytes.clone();
    for slot in 0..RING_SLOTS {
        one_copy[as_usize(slot_block(slot)) * 512 + HEADER_LEN] ^= 0x01;
    }
    assert!(
        ARXFS::open(MemBlock::from_bytes(one_copy, 512, 256), Keying::Plaintext).is_ok(),
        "every primary rotted, every companion intact"
    );

    let mut ring = bytes;
    damage_superblock_ring(&mut ring, 512);
    assert!(ARXFS::open(MemBlock::from_bytes(ring, 512, 256), Keying::Plaintext).is_err());
}

#[test]
fn a_protection_word_naming_neither_protection_refuses_the_mount() {
    for word in [0u32, 3] {
        let mut bytes = fmt_plain(512, 256)
            .into_block()
            .expect("the volume closes")
            .bytes();
        let mut resealed = 0;
        for block in 0..RING_BLOCKS {
            let start = as_usize(block) * 512;
            let slot = &mut bytes[start..start + 512];
            let Some(header) = BlockHeader::try_decode(
                slot,
                BlockType::Superblock,
                rd_u128(slot, 16),
                slot_block(block / 2),
                &MetaSeal::Checksum,
            ) else {
                continue;
            };
            wr_u32(slot, superblock::P_PROTECTION, word);
            header
                .seal(slot, &MetaSeal::Checksum)
                .expect("reseal the slot over the new word");
            resealed += 1;
        }
        assert!(resealed >= 2, "both copies of the committed slot");
        let mut device = MemBlock::from_bytes(bytes.clone(), 512, 256);
        assert_eq!(
            ARXFS::probe_protection(&mut device),
            Err(DriverError::Unsupported),
            "word {word}"
        );
        assert_eq!(
            ARXFS::open(MemBlock::from_bytes(bytes, 512, 256), Keying::Plaintext).err(),
            Some(DriverError::Unsupported),
            "word {word}"
        );
    }
}

#[test]
fn a_plaintext_volume_checks_sound_and_is_rescued_with_no_key() {
    let bs = 4096usize;
    let mut fs = ARXFS::format(
        MemBlock::new(4096, 512),
        128,
        Keying::Plaintext,
        &mut TestEntropy::new(),
    )
    .expect("format")
    .stamped();
    let root = fs.root();
    let cap = as_usize(fs.data_capacity());
    let body = incompressible(cap + cap / 2);
    fs.create(root, b"doc", NodeKind::RegularFile)
        .expect("create");
    fs.write_at(root, b"doc", 0, &body).expect("write");
    let doc_ino = u32::try_from(fs.lookup(root, b"doc").expect("lookup").raw()).unwrap();
    assert_eq!(check_full(&mut fs).structure, StructureVerdict::Sound);
    let mut bytes = fs.into_block().expect("the volume closes").bytes();

    damage_superblock_ring(&mut bytes, bs);
    let mut out = CollectSink::new();
    let report = ARXFS::rescue(
        MemBlock::from_bytes(bytes.clone(), 4096, 512),
        Keying::Plaintext,
        &GrantAll,
        &NullSink,
        &mut out,
    )
    .expect("rescue");
    assert!(report.found_root());
    let b0 = out.blocks.get(&(doc_ino, 0)).expect("block 0 recovered");
    assert_eq!(&b0[..cap], &body[..cap]);
    assert_eq!(
        ARXFS::rescue(
            MemBlock::from_bytes(bytes, 4096, 512),
            TEST_KEYING,
            &GrantAll,
            &NullSink,
            &mut CollectSink::new(),
        )
        .err(),
        Some(DriverError::Unsupported),
        "rescue is held to the protection the volume has"
    );
}

/// The start of every written primary superblock copy in `bytes`.
fn written_primaries(bytes: &[u8], bs: usize) -> alloc::vec::Vec<usize> {
    (0..RING_SLOTS)
        .map(|slot| as_usize(slot_block(slot)) * bs)
        .filter(|&start| header::claimed_uuid(&bytes[start..start + bs]).is_some())
        .collect()
}

#[test]
fn a_damaged_uuid_in_one_copy_neither_misnames_nor_blocks_the_volume() {
    for keying in [Keying::Plaintext, TEST_KEYING] {
        publish_boot_hash_key();
        let fs = ARXFS::format(MemBlock::new(512, 256), 32, keying, &mut TestEntropy::new())
            .expect("format");
        let uuid = fs.volume_uuid();
        let mut bytes = fs.into_block().expect("the volume closes").bytes();
        let primaries = written_primaries(&bytes, 512);
        assert!(!primaries.is_empty());
        for start in primaries {
            bytes[start + 20] ^= 0x10;
        }
        let fs = ARXFS::open(MemBlock::from_bytes(bytes, 512, 256), keying)
            .expect("each slot is served by its mirror");
        assert_eq!(fs.volume_uuid(), uuid);
    }
}

#[test]
fn a_protection_word_rotted_in_one_copy_is_passed_over() {
    for (keying, protection) in [
        (Keying::Plaintext, VolumeProtection::Plaintext),
        (TEST_KEYING, VolumeProtection::Encrypted),
    ] {
        publish_boot_hash_key();
        let fs = ARXFS::format(MemBlock::new(512, 256), 32, keying, &mut TestEntropy::new())
            .expect("format");
        let mut bytes = fs.into_block().expect("the volume closes").bytes();
        for start in written_primaries(&bytes, 512) {
            wr_u32(&mut bytes[start..start + 512], superblock::P_PROTECTION, 0);
        }
        assert_eq!(
            ARXFS::probe_protection(&mut MemBlock::from_bytes(bytes.clone(), 512, 256)),
            Ok(protection)
        );
        assert!(ARXFS::open(MemBlock::from_bytes(bytes, 512, 256), keying).is_ok());
    }
}

#[test]
fn a_reformat_leaves_no_superblock_of_the_volume_before_it() {
    publish_boot_hash_key();
    let mut entropy = TestEntropy::new();
    let mut old = ARXFS::format(MemBlock::new(512, 256), 32, Keying::Plaintext, &mut entropy)
        .expect("format the first volume");
    let root = old.root();
    for name in [b"a".as_slice(), b"b", b"c", b"d"] {
        old.create(root, name, NodeKind::RegularFile)
            .expect("create");
        old.commit().expect("commit");
    }
    let old_uuid = u128::from_le_bytes(old.volume_uuid());
    let bytes = old.into_block().expect("the volume closes").bytes();
    let carries_old = |bytes: &[u8]| {
        (0..RING_BLOCKS)
            .filter(|&block| {
                let start = as_usize(block) * 512;
                header::claimed_uuid(&bytes[start..start + 512]) == Some(old_uuid)
            })
            .count()
    };
    assert!(
        carries_old(&bytes) > 2,
        "the first volume wrote more than one slot"
    );

    let new = ARXFS::format(
        MemBlock::from_bytes(bytes, 512, 256),
        32,
        Keying::Plaintext,
        &mut entropy,
    )
    .expect("format over it");
    assert_ne!(u128::from_le_bytes(new.volume_uuid()), old_uuid);
    let mut bytes = new.into_block().expect("the volume closes").bytes();
    assert_eq!(carries_old(&bytes), 0);

    damage_superblock_ring(&mut bytes, 512);
    assert_eq!(
        ARXFS::open(MemBlock::from_bytes(bytes, 512, 256), Keying::Plaintext).err(),
        Some(DriverError::BadMagic),
        "nothing of the first volume mounts in the second's place"
    );
}

#[test]
fn a_plaintext_slot_is_believed_only_with_no_key_material_and_its_own_word() {
    let bytes = fmt_plain(512, 256)
        .into_block()
        .expect("the volume closes")
        .bytes();
    let start = *written_primaries(&bytes, 512)
        .first()
        .expect("a committed slot");
    let primary = u64::try_from(start / 512).expect("a ring block");
    let slot = &bytes[start..start + 512];
    assert!(matches!(
        Superblock::try_decode(slot, None, primary, &MetaSeal::Checksum),
        Ok(Some(_))
    ));
    let resealed = |edit: &dyn Fn(&mut [u8])| {
        let mut copy = slot.to_vec();
        let header = BlockHeader::try_decode(
            &copy,
            BlockType::Superblock,
            rd_u128(&copy, 16),
            primary,
            &MetaSeal::Checksum,
        )
        .expect("a sealed slot");
        edit(&mut copy);
        header
            .seal(&mut copy, &MetaSeal::Checksum)
            .expect("reseal the slot over the edit");
        Superblock::try_decode(&copy, None, primary, &MetaSeal::Checksum)
    };
    assert!(
        matches!(
            resealed(&|copy| copy[superblock::CRYPTO_OFFSET + 3] = 1),
            Ok(None)
        ),
        "key material on a plaintext slot"
    );
    assert!(
        matches!(
            resealed(&|copy| wr_u32(
                copy,
                superblock::P_PROTECTION,
                VolumeProtection::Encrypted.word()
            )),
            Ok(None)
        ),
        "a word the seal contradicts"
    );
}

/// The content hash a volume formatted from `entropy` stores for one block of
/// `content`.
fn stored_content_hash(
    keying: Keying<'_>,
    entropy: &mut TestEntropy,
    content: &[u8],
) -> [u8; LOGICAL_HASH_LEN] {
    publish_boot_hash_key();
    let mut fs = ARXFS::format(MemBlock::new(512, 256), 32, keying, entropy).expect("format");
    let root = fs.root();
    fs.create(root, b"f", NodeKind::RegularFile)
        .expect("create");
    assert_eq!(fs.write_at(root, b"f", 0, content), Ok(content.len()));
    let phys = data_block_phys(&mut fs, b"f", 0);
    let mut raw = [0u8; MAX_BLOCK_SIZE];
    fs.read_block(phys, &mut raw).expect("raw read");
    let off = fs.logical_hash_offset();
    let mut hash = [0u8; LOGICAL_HASH_LEN];
    hash.copy_from_slice(&raw[off..off + LOGICAL_HASH_LEN]);
    hash
}

#[test]
fn an_encrypted_volume_s_stored_content_hash_confirms_no_guessed_plaintext() {
    let content = [0x5Au8; 300];
    let mut slot = alloc::vec![0u8; as_usize(fmt_plain(512, 256).data_capacity())];
    slot[..content.len()].copy_from_slice(&content);
    let guess = tairix_crypto::sha256(&slot);

    let mut entropy = TestEntropy::new();
    assert_eq!(
        stored_content_hash(Keying::Plaintext, &mut entropy, &content),
        guess
    );
    let keyed = stored_content_hash(TEST_KEYING, &mut entropy, &content);
    assert_ne!(keyed, guess, "whoever holds the disk confirms no guess");
    assert_ne!(
        stored_content_hash(TEST_KEYING, &mut entropy, &content),
        keyed,
        "each volume's own master key names its content its own way"
    );
}
