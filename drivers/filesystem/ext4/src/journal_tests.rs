use super::*;

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

use crate::bytes::{high32, low32};

/// The first sixteen blocks of a 4096-block internal journal that `debugfs`
/// (e2fsprogs 1.47.0) left needing recovery, with v3 checksums and 64-bit
/// tags: transaction 1 logs fs blocks 5000 and 5001, transaction 2 revokes
/// 5000, transaction 3 logs 5002. `e2fsck` replaying the same log writes
/// 5001 and 5002 and leaves 5000 alone.
const REVOKE_LOG: &[u8] = include_bytes!("../tests/fixtures/journal_revoke.bin");

/// The first four blocks of a 1024-block journal `debugfs` (e2fsprogs
/// 1.47.0) left needing recovery: one transaction logging fs block 5000,
/// whose first word is the journal magic, so it was logged escaped. `e2fsck`
/// replaying it writes the block home with the magic restored.
const ESCAPE_LOG: &[u8] = include_bytes!("../tests/fixtures/journal_escape.bin");
/// The first sixteen blocks of a 1024-block internal journal `debugfs`
/// (e2fsprogs 1.47.0) left needing recovery after `jo -c -v 2`: checksum v2
/// and 64-bit tags, transaction 1 logging fs blocks 5000 and 5001 and
/// transaction 2 logging 5002. `e2fsck` replaying it writes 5000 as `0x11`s,
/// 5001 as `0x22`s and 5002 as `0x33`s.
const V2_LOG: &[u8] = include_bytes!("../tests/fixtures/journal_v2.bin");

struct MemJournal {
    block_size: usize,
    blocks: Vec<Vec<u8>>,
    home: BTreeMap<u64, Vec<u8>>,
    flushes: u32,
    /// A journal block whose writes fail.
    failing: Option<u32>,
}

impl MemJournal {
    fn from_image(image: &[u8], block_size: usize, maxlen: usize) -> Self {
        let mut blocks: Vec<Vec<u8>> = image.chunks(block_size).map(<[u8]>::to_vec).collect();
        blocks.resize(maxlen, vec![0u8; block_size]);
        Self {
            block_size,
            blocks,
            home: BTreeMap::new(),
            flushes: 0,
            failing: None,
        }
    }

    fn journal(&self) -> Journal {
        Journal::parse(
            self.blocks[0].clone(),
            self.block_size,
            self.blocks.len() as u64,
        )
        .expect("parse")
    }
}

impl JournalIo for MemJournal {
    fn read_journal(&mut self, n: u32, buf: &mut [u8]) -> Result<(), DriverError> {
        buf.copy_from_slice(
            self.blocks
                .get(n as usize)
                .ok_or(DriverError::DeviceFault)?,
        );
        Ok(())
    }

    fn write_journal(&mut self, n: u32, buf: &[u8]) -> Result<(), DriverError> {
        if self.failing == Some(n) {
            return Err(DriverError::DeviceFault);
        }
        let slot = self
            .blocks
            .get_mut(n as usize)
            .ok_or(DriverError::DeviceFault)?;
        slot.copy_from_slice(buf);
        Ok(())
    }

    fn write_home(&mut self, block: u64, buf: &[u8]) -> Result<(), DriverError> {
        self.home.insert(block, buf.to_vec());
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.flushes += 1;
        Ok(())
    }
}

#[test]
fn a_real_log_replays_its_committed_blocks_and_honours_its_revokes() {
    let mut io = MemJournal::from_image(REVOKE_LOG, 1024, 4096);
    let mut journal = io.journal();
    assert!(journal.has_log());
    assert_eq!(journal.recover(&mut io), Ok(2));
    let expected_5001 = REVOKE_LOG[3 * 1024..4 * 1024].to_vec();
    let expected_5002 = REVOKE_LOG[8 * 1024..9 * 1024].to_vec();
    assert_eq!(io.home.get(&5001), Some(&expected_5001));
    assert_eq!(io.home.get(&5002), Some(&expected_5002));
    assert!(
        !io.home.contains_key(&5000),
        "a revoked block is not replayed"
    );
    // The log is empty again, under an id past every transaction it held.
    let after = io.journal();
    assert!(!after.has_log());
    assert_eq!(after.sequence, 5);
}

/// A version-2 journal superblock of `maxlen` 1 KiB blocks.
fn fresh(maxlen: u32, incompat: u32) -> MemJournal {
    let mut sb = vec![0u8; 1024];
    put_be32(&mut sb, 0, MAGIC);
    put_be32(&mut sb, 4, SUPERBLOCK_V2);
    put_be32(&mut sb, SB_BLOCKSIZE, 1024);
    put_be32(&mut sb, SB_MAXLEN, maxlen);
    put_be32(&mut sb, SB_FIRST, 1);
    put_be32(&mut sb, SB_SEQUENCE, 7);
    put_be32(&mut sb, SB_INCOMPAT, incompat);
    sb[SB_UUID..SB_UUID + UUID_LEN].copy_from_slice(&[0x5A; UUID_LEN]);
    if incompat & (INCOMPAT_CSUM_V2 | INCOMPAT_CSUM_V3) != 0 {
        sb[SB_CHECKSUM_TYPE] = CRC32C;
        let sum = sb_checksum(&sb);
        put_be32(&mut sb, SB_CHECKSUM, sum);
    }
    MemJournal::from_image(&sb, 1024, maxlen as usize)
}

fn images(n: usize) -> Vec<(u64, Vec<u8>)> {
    (0..n)
        .map(|i| {
            let mut block = vec![0u8; 1024];
            for (k, byte) in block.iter_mut().enumerate() {
                *byte = (i * 31 + k * 7).to_le_bytes()[0];
            }
            ((1u64 << 33) + i as u64 * 5, block)
        })
        .collect()
}

/// Commit `blocks` and then recover as a mount after a crash would.
fn crash_and_recover(mut io: MemJournal, blocks: &[(u64, Vec<u8>)]) -> MemJournal {
    let mut journal = io.journal();
    journal
        .commit(
            &mut io,
            blocks.iter().map(|(home, image)| (*home, image.as_slice())),
            blocks.len(),
            (1_792_000_000, 5),
        )
        .expect("commit");
    let mut after = io.journal();
    assert!(after.has_log(), "a committed log is published");
    assert_eq!(
        after.recover(&mut io),
        Ok(u32::try_from(blocks.len()).expect("fits"))
    );
    io
}

#[test]
fn a_committed_transaction_replays_after_a_crash() {
    for incompat in [
        INCOMPAT_CSUM_V3 | INCOMPAT_64BIT,
        INCOMPAT_CSUM_V2 | INCOMPAT_64BIT,
        INCOMPAT_64BIT,
    ] {
        let blocks = images(150);
        let io = crash_and_recover(fresh(1024, incompat), &blocks);
        for (home, image) in &blocks {
            assert_eq!(io.home.get(home), Some(image), "incompat {incompat:#x}");
        }
        assert!(!io.journal().has_log());
    }
}

#[test]
fn a_32_bit_journal_refuses_a_block_it_cannot_tag() {
    let mut io = fresh(256, 0);
    let mut journal = io.journal();
    let blocks = images(1);
    assert_eq!(
        journal.commit(
            &mut io,
            blocks.iter().map(|(h, i)| (*h, i.as_slice())),
            1,
            (0, 0)
        ),
        Err(CommitError::Unwritten(DriverError::DeviceFault))
    );
}

#[test]
fn a_block_starting_with_the_journal_magic_is_escaped_and_restored() {
    let mut block = vec![0x11u8; 1024];
    put_be32(&mut block, 0, MAGIC);
    let blocks = vec![(42u64, block.clone())];
    let io = crash_and_recover(fresh(64, INCOMPAT_CSUM_V3), &blocks);
    assert_eq!(io.home.get(&42), Some(&block));
}

#[test]
fn a_transaction_without_a_commit_block_is_discarded() {
    let mut io = fresh(64, INCOMPAT_CSUM_V3);
    let mut journal = io.journal();
    let blocks = images(3)
        .into_iter()
        .map(|(_, image)| (9u64, image))
        .collect::<Vec<_>>();
    journal
        .commit(
            &mut io,
            blocks.iter().map(|(h, i)| (*h, i.as_slice())),
            3,
            (0, 0),
        )
        .expect("commit");
    // Tear the commit block, as a crash before it reached the medium does.
    let commit_at = 1 + 1 + 3;
    io.blocks[commit_at][0] ^= 0xFF;
    let mut after = io.journal();
    assert_eq!(after.recover(&mut io), Ok(0));
    assert!(io.home.is_empty());
    assert!(!io.journal().has_log());
}

#[test]
fn a_corrupt_block_in_a_committed_transaction_refuses_recovery() {
    let mut io = fresh(64, INCOMPAT_CSUM_V3);
    let mut journal = io.journal();
    let blocks = images(2);
    journal
        .commit(
            &mut io,
            blocks.iter().map(|(h, i)| (*h, i.as_slice())),
            2,
            (0, 0),
        )
        .expect("commit");
    io.blocks[2][100] ^= 0x01;
    let mut after = io.journal();
    assert_eq!(after.recover(&mut io), Err(DriverError::DeviceFault));
}

#[test]
fn a_transaction_larger_than_its_share_of_the_log_is_refused() {
    let mut io = fresh(64, INCOMPAT_CSUM_V3);
    let mut journal = io.journal();
    let too_many = journal.capacity() + 1;
    let blocks = images(too_many);
    assert_eq!(
        journal.commit(
            &mut io,
            blocks.iter().map(|(h, i)| (*h, i.as_slice())),
            too_many,
            (0, 0)
        ),
        Err(CommitError::Unwritten(DriverError::NoSpace))
    );
}

#[test]
fn checkpointing_empties_the_log_and_moves_the_id_on() {
    let mut io = fresh(64, INCOMPAT_CSUM_V3);
    let mut journal = io.journal();
    let blocks = images(1);
    journal
        .commit(
            &mut io,
            blocks.iter().map(|(h, i)| (*h, i.as_slice())),
            1,
            (0, 0),
        )
        .expect("commit");
    journal.quiesce(&mut io).expect("quiesced");
    let after = io.journal();
    assert!(!after.has_log());
    assert_eq!(after.sequence, 8);
}

#[test]
fn a_superblock_this_driver_cannot_use_is_refused() {
    let io = fresh(64, 0x20);
    assert_eq!(
        Journal::parse(io.blocks[0].clone(), 1024, 64).err(),
        Some(Refusal::Feature(0x20))
    );
    let io = fresh(64, INCOMPAT_ASYNC_COMMIT);
    assert_eq!(
        Journal::parse(io.blocks[0].clone(), 1024, 64).err(),
        Some(Refusal::Feature(INCOMPAT_ASYNC_COMMIT))
    );
    let mut io = fresh(64, INCOMPAT_CSUM_V3);
    io.blocks[0][SB_SEQUENCE] ^= 1;
    assert_eq!(
        Journal::parse(io.blocks[0].clone(), 1024, 64).err(),
        Some(Refusal::Checksum)
    );
    let io = fresh(64, 0);
    assert_eq!(
        Journal::parse(io.blocks[0].clone(), 1024, 32).err(),
        Some(Refusal::Unrecognised),
        "a log longer than its file"
    );
    assert_eq!(
        Journal::parse(io.blocks[0].clone(), 2048, 64).err(),
        Some(Refusal::Unrecognised),
        "a block size other than the filesystem's"
    );
}

#[test]
fn adopting_features_turns_on_what_the_writer_needs() {
    let mut io = fresh(64, 0);
    let mut journal = io.journal();
    journal.adopt_features(true, true);
    let blocks = images(2);
    journal
        .commit(
            &mut io,
            blocks.iter().map(|(h, i)| (*h, i.as_slice())),
            2,
            (0, 0),
        )
        .expect("a 64-bit home now fits");
    let after = io.journal();
    assert!(after.v3() && after.wide());
}

#[test]
fn a_debugfs_escaped_block_replays_with_its_magic_restored() {
    let mut io = MemJournal::from_image(ESCAPE_LOG, 1024, 1024);
    let mut journal = io.journal();
    assert_eq!(journal.recover(&mut io), Ok(1));
    let home = io.home.get(&5000).expect("block 5000 replayed");
    assert_eq!(be32(home, 0), MAGIC, "the escaped first word is restored");
    assert_eq!(&home[4..], &ESCAPE_LOG[2 * 1024 + 4..3 * 1024]);
}

/// Lay a hand-built transaction `sequence` at journal block `at`: a
/// descriptor logging `logged`, or a revoke block revoking `revoked`, then
/// its commit block. Answers the block after it.
fn lay(
    io: &mut MemJournal,
    journal: &Journal,
    at: u32,
    sequence: u32,
    logged: Option<u64>,
    revoked: Option<u64>,
) -> u32 {
    let mut at = at;
    let header = |kind: u32| {
        let mut block = vec![0u8; 1024];
        put_be32(&mut block, 0, MAGIC);
        put_be32(&mut block, 4, kind);
        put_be32(&mut block, 8, sequence);
        block
    };
    if let Some(home) = logged {
        let data = vec![sequence.to_le_bytes()[0]; 1024];
        let mut d = header(DESCRIPTOR);
        put_be32(&mut d, HEADER, low32(home));
        put_be32(&mut d, HEADER + 4, FLAG_LAST_TAG);
        put_be32(&mut d, HEADER + 8, high32(home));
        put_be32(
            &mut d,
            HEADER + 12,
            journal.tag_checksum(sequence, &data, false),
        );
        let sum = journal.block_checksum(&d, 1024 - TAIL);
        put_be32(&mut d, 1024 - TAIL, sum);
        io.blocks[at as usize] = d;
        io.blocks[at as usize + 1] = data;
        at += 2;
    }
    if let Some(home) = revoked {
        let mut r = header(REVOKE);
        put_be32(
            &mut r,
            HEADER,
            u32::try_from(REVOKE_HEADER + 8).expect("one record"),
        );
        put_be32(&mut r, REVOKE_HEADER, high32(home));
        put_be32(&mut r, REVOKE_HEADER + 4, low32(home));
        let sum = journal.block_checksum(&r, 1024 - TAIL);
        put_be32(&mut r, 1024 - TAIL, sum);
        io.blocks[at as usize] = r;
        at += 1;
    }
    let mut c = header(COMMIT);
    let sum = journal.block_checksum(&c, COMMIT_CHECKSUM);
    put_be32(&mut c, COMMIT_CHECKSUM, sum);
    io.blocks[at as usize] = c;
    at + 1
}

/// A journal whose log starts at block 1 under transaction id `sequence`.
fn log_from(sequence: u32) -> MemJournal {
    let mut io = fresh(64, INCOMPAT_CSUM_V3 | INCOMPAT_64BIT | INCOMPAT_REVOKE);
    put_be32(&mut io.blocks[0], SB_SEQUENCE, sequence);
    put_be32(&mut io.blocks[0], SB_START, 1);
    let sum = sb_checksum(&io.blocks[0]);
    put_be32(&mut io.blocks[0], SB_CHECKSUM, sum);
    io
}

#[test]
fn a_revoke_holds_across_transaction_id_wraparound() {
    let mut io = log_from(u32::MAX);
    let journal = io.journal();
    let at = lay(&mut io, &journal, 1, u32::MAX, Some(9), None);
    let at = lay(&mut io, &journal, at, 0, None, Some(9));
    lay(&mut io, &journal, at, 1, Some(11), None);
    let mut recovering = io.journal();
    assert_eq!(recovering.recover(&mut io), Ok(1));
    assert!(
        !io.home.contains_key(&9),
        "revoked by a transaction past the wrap"
    );
    assert_eq!(io.home.get(&11), Some(&vec![1u8; 1024]));
    let after = io.journal();
    assert!(!after.has_log());
    assert_eq!(after.sequence, 3, "the next id is past the interrupted one");
}

#[test]
fn a_block_logged_after_its_revoke_is_replayed() {
    let mut io = log_from(7);
    let journal = io.journal();
    let at = lay(&mut io, &journal, 1, 7, Some(5), None);
    let at = lay(&mut io, &journal, at, 8, None, Some(5));
    lay(&mut io, &journal, at, 9, Some(5), None);
    let mut recovering = io.journal();
    assert_eq!(recovering.recover(&mut io), Ok(1));
    assert_eq!(
        io.home.get(&5),
        Some(&vec![9u8; 1024]),
        "the later image wins"
    );
}

fn commit_all(
    journal: &mut Journal,
    io: &mut MemJournal,
    blocks: &[(u64, Vec<u8>)],
) -> Result<(), CommitError> {
    journal.commit(
        io,
        blocks.iter().map(|(h, i)| (*h, i.as_slice())),
        blocks.len(),
        (1, 2),
    )
}

#[test]
fn each_transaction_follows_the_last_and_the_superblock_names_the_newest() {
    let mut io = fresh(64, INCOMPAT_CSUM_V3 | INCOMPAT_64BIT);
    let mut journal = io.journal();
    let first = images(3);
    commit_all(&mut journal, &mut io, &first).expect("first");
    // One descriptor, three images and a commit block from block 1.
    assert_eq!(be32(&io.blocks[0], SB_START), 1);
    let second: Vec<(u64, Vec<u8>)> = images(2)
        .into_iter()
        .map(|(home, mut image)| {
            image[100] ^= 0xFF;
            (home + 1000, image)
        })
        .collect();
    commit_all(&mut journal, &mut io, &second).expect("second");
    assert_eq!(
        be32(&io.blocks[0], SB_START),
        6,
        "the newest, after the first"
    );
    assert_eq!(be32(&io.blocks[0], SB_SEQUENCE), 8);
    // A crash now replays the newest alone: the first is home already.
    let mut recovering = io.journal();
    assert_eq!(recovering.recover(&mut io), Ok(2));
    for (home, image) in &second {
        assert_eq!(io.home.get(home), Some(image));
    }
    assert!(first.iter().all(|(home, _)| !io.home.contains_key(home)));
}

#[test]
fn a_log_wraps_back_to_its_first_block() {
    // Fifteen log blocks, four per transaction: the fourth wraps.
    let mut io = fresh(16, INCOMPAT_CSUM_V3 | INCOMPAT_64BIT);
    let mut journal = io.journal();
    let mut newest = Vec::new();
    for round in 0..6u8 {
        newest = images(2)
            .into_iter()
            .map(|(home, mut image)| {
                image[0] = round;
                (home, image)
            })
            .collect();
        commit_all(&mut journal, &mut io, &newest).expect("commit");
    }
    assert_eq!(
        be32(&io.blocks[0], SB_START),
        6,
        "round 5 starts past the wrap"
    );
    let mut recovering = io.journal();
    assert_eq!(recovering.recover(&mut io), Ok(2), "the newest alone");
    assert!(newest
        .iter()
        .all(|(home, image)| io.home.get(home) == Some(image)));
}

#[test]
fn a_commit_block_that_fails_spends_its_place_and_id() {
    let mut io = fresh(64, INCOMPAT_CSUM_V3 | INCOMPAT_64BIT);
    let mut journal = io.journal();
    let blocks = images(1);
    io.failing = Some(3);
    assert!(matches!(
        commit_all(&mut journal, &mut io, &blocks),
        Err(CommitError::Uncertain(DriverError::DeviceFault))
    ));
    io.failing = None;
    commit_all(&mut journal, &mut io, &blocks).expect("a retry");
    assert_eq!(
        be32(&io.blocks[0], SB_START),
        4,
        "written after the spent place"
    );
    assert_eq!(be32(&io.blocks[0], SB_SEQUENCE), 8, "under a fresh id");
}

#[test]
fn a_quiet_log_is_emptied_durably() {
    let mut io = fresh(64, INCOMPAT_CSUM_V3);
    let mut journal = io.journal();
    commit_all(&mut journal, &mut io, &images(1)).expect("commit");
    let flushes = io.flushes;
    journal.quiesce(&mut io).expect("quiesced");
    assert!(!io.journal().has_log());
    assert_eq!(
        io.flushes,
        flushes + 2,
        "home writes before, the empty log after"
    );
    journal.quiesce(&mut io).expect("idle");
    assert_eq!(io.flushes, flushes + 2, "an empty log costs nothing");
}

/// A v2 tag is two bytes longer than an unsummed one: read at the unsummed
/// length, every tag after a descriptor's first was misread.
#[test]
fn a_checksum_v2_log_replays_every_tag() {
    let mut io = MemJournal::from_image(V2_LOG, 1024, 1024);
    let mut journal = io.journal();
    assert_eq!(journal.recover(&mut io), Ok(3));
    for (home, fill) in [(5000, 0x11), (5001, 0x22), (5002, 0x33)] {
        assert_eq!(io.home.get(&home), Some(&vec![fill; 1024]), "{home}");
    }
}

/// This writer writes v3: a log checksummed in v2 moves to it once replayed.
#[test]
fn a_v2_log_is_written_on_in_v3() {
    let mut io = MemJournal::from_image(V2_LOG, 1024, 1024);
    let mut journal = io.journal();
    journal.recover(&mut io).expect("recover");
    journal.adopt_features(true, false);
    assert!(journal.v3());
    assert_eq!(journal.incompat & INCOMPAT_CSUM_V2, 0);
}

/// A descriptor that names more blocks than its ring holds makes a log that
/// laps itself: refused, never walked round and round.
#[test]
fn a_log_that_laps_its_ring_is_refused() {
    let mut io = fresh(8, INCOMPAT_64BIT);
    put_be32(&mut io.blocks[0], SB_START, 1);
    let mut descriptor = vec![0u8; 1024];
    put_be32(&mut descriptor, 0, MAGIC);
    put_be32(&mut descriptor, 4, DESCRIPTOR);
    put_be32(&mut descriptor, 8, 7);
    for (i, tag) in descriptor[HEADER..]
        .as_chunks_mut::<12>()
        .0
        .iter_mut()
        .enumerate()
    {
        put_be32(tag, 0, 100 + u32::try_from(i).expect("small"));
        put_be32(tag, 4, FLAG_SAME_UUID);
    }
    io.blocks[1] = descriptor;
    let mut journal = io.journal();
    assert_eq!(journal.recover(&mut io), Err(DriverError::DeviceFault));
}

/// No journal is longer than mke2fs makes one, whatever its file allows.
#[test]
fn a_journal_longer_than_mke2fs_makes_is_refused() {
    let mut sb = fresh(8, 0).blocks[0].clone();
    put_be32(&mut sb, SB_MAXLEN, MAX_JOURNAL_BLOCKS + 1);
    assert_eq!(
        Journal::parse(sb, 1024, u64::MAX).err(),
        Some(Refusal::Unrecognised)
    );
}

/// A descriptor failing its checksum ends a torn log, but one its commit
/// block vouches for is damage: refused, rather than the transaction dropped
/// and the evidence wiped with the log.
#[test]
fn a_damaged_descriptor_of_a_committed_transaction_refuses_recovery() {
    let mut io = log_from(7);
    let journal = io.journal();
    lay(&mut io, &journal, 1, 7, Some(5000), None);
    io.blocks[1][100] ^= 0xFF;
    let mut journal = io.journal();
    assert_eq!(journal.recover(&mut io), Err(DriverError::DeviceFault));
    assert!(io.home.is_empty(), "nothing replayed");

    // The same damage with no commit after it is the end of the log.
    let mut io = log_from(7);
    let journal = io.journal();
    let end = lay(&mut io, &journal, 1, 7, Some(5000), None);
    io.blocks[1][100] ^= 0xFF;
    io.blocks[end as usize - 1] = vec![0u8; 1024];
    let mut journal = io.journal();
    assert_eq!(journal.recover(&mut io), Ok(0));
}
