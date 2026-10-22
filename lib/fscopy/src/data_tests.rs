// Fixture files are a few hundred KiB, so every offset fits a usize.
#![allow(clippy::cast_possible_truncation)]

use alloc::vec;
use alloc::vec::Vec;

use super::*;

/// A file of stored runs and holes, `(start, bytes)` sorted and apart.
struct SparseFile {
    len: u64,
    runs: Vec<(u64, Vec<u8>)>,
    /// Answers every seek with `NotSupported`, as a backing with no word on
    /// its holes would.
    blind: bool,
    reads: usize,
}

impl SparseFile {
    fn new(len: u64, runs: Vec<(u64, Vec<u8>)>) -> Self {
        Self {
            len,
            runs,
            blind: false,
            reads: 0,
        }
    }

    fn content(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.len as usize];
        for (start, bytes) in &self.runs {
            out[*start as usize..*start as usize + bytes.len()].copy_from_slice(bytes);
        }
        out
    }
}

impl Source for SparseFile {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        self.reads += 1;
        let content = self.content();
        let Some(rest) = content.get(offset as usize..) else {
            return Ok(0);
        };
        let n = rest.len().min(buf.len());
        buf[..n].copy_from_slice(&rest[..n]);
        Ok(n)
    }

    fn next_region(&mut self, offset: u64, region: SeekRegion) -> Result<Option<u64>, Errno> {
        if self.blind {
            return Err(Errno::NotSupported);
        }
        if offset >= self.len {
            return Ok(None);
        }
        let inside = self
            .runs
            .iter()
            .find(|(start, bytes)| (*start..*start + bytes.len() as u64).contains(&offset));
        Ok(Some(match (region, inside) {
            (SeekRegion::Data, Some(_)) | (SeekRegion::Hole, None) => offset,
            (SeekRegion::Hole, Some((start, bytes))) => start + bytes.len() as u64,
            (SeekRegion::Data, None) => {
                match self
                    .runs
                    .iter()
                    .map(|(start, _)| *start)
                    .find(|&start| start > offset)
                {
                    Some(start) => start,
                    None => return Ok(None),
                }
            }
        }))
    }

    fn size(&mut self) -> Result<u64, Errno> {
        Ok(self.len)
    }
}

/// A destination recording every write and length it was given.
#[derive(Default)]
struct Recorded {
    bytes: Vec<u8>,
    writes: Vec<(u64, usize)>,
    set_lens: Vec<u64>,
}

impl Target for Recorded {
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Errno> {
        let end = offset as usize + bytes.len();
        if self.bytes.len() < end {
            self.bytes.resize(end, 0);
        }
        self.bytes[offset as usize..end].copy_from_slice(bytes);
        self.writes.push((offset, bytes.len()));
        Ok(())
    }

    fn set_len(&mut self, len: u64) -> Result<(), Errno> {
        self.bytes.resize(len as usize, 0);
        self.set_lens.push(len);
        Ok(())
    }
}

fn copy(source: &mut SparseFile, sparse: Sparse, buf_len: usize) -> Recorded {
    let mut target = Recorded::default();
    let mut buf = vec![0u8; buf_len];
    let mut copy = DataCopy::new(source.len, sparse);
    let mut accounted = 0;
    while let Step::Moved(n) = copy.step(source, &mut target, &mut buf).expect("copies") {
        accounted += n;
    }
    assert_eq!(accounted, source.len, "every byte is accounted for once");
    assert_eq!(copy.remaining(), 0);
    target
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) | 1)
        .collect()
}

#[test]
fn a_sparse_file_keeps_its_holes_and_reads_back_whole() {
    let mut source = SparseFile::new(
        200_000,
        vec![
            (0, pattern(5000, 1)),
            (65_536, pattern(9000, 2)),
            (131_072, pattern(100, 3)),
        ],
    );
    let target = copy(&mut source, Sparse::Auto, 4096);
    assert_eq!(target.bytes, source.content());
    let written: usize = target.writes.iter().map(|(_, len)| len).sum();
    assert_eq!(
        written,
        5000 + 9000 + 100,
        "only the stored bytes are written"
    );
    assert_eq!(
        target.set_lens,
        [200_000],
        "the trailing hole is the length set last"
    );
}

#[test]
fn a_hole_at_the_start_and_a_file_all_hole_are_kept() {
    let mut leading = SparseFile::new(10_000, vec![(8192, pattern(1808, 4))]);
    let target = copy(&mut leading, Sparse::Auto, 1024);
    assert_eq!(target.bytes, leading.content());
    assert_eq!(target.writes.first().map(|(at, _)| *at), Some(8192));
    assert!(
        target.set_lens.is_empty(),
        "a file whose last byte was written needs no length"
    );

    let mut empty = SparseFile::new(1 << 20, Vec::new());
    let target = copy(&mut empty, Sparse::Auto, 4096);
    assert!(target.writes.is_empty());
    assert_eq!(target.set_lens, [1 << 20]);
    assert_eq!(empty.reads, 0, "a hole is never read");
}

#[test]
fn never_writes_every_byte_and_always_also_skips_stored_zeros() {
    let mut zeros = pattern(3 * ZERO_UNIT, 5);
    zeros[ZERO_UNIT..2 * ZERO_UNIT].fill(0);
    let mut source = SparseFile::new(40_000, vec![(0, zeros.clone())]);
    let dense = copy(&mut source, Sparse::Never, 8192);
    assert_eq!(dense.bytes, source.content());
    let written: usize = dense.writes.iter().map(|(_, len)| len).sum();
    assert_eq!(written, 40_000, "the hole is written as zeros");

    let sparse = copy(&mut source, Sparse::Always, 3 * ZERO_UNIT);
    assert_eq!(sparse.bytes, source.content());
    assert_eq!(
        sparse.writes,
        [(0, ZERO_UNIT), (2 * ZERO_UNIT as u64, ZERO_UNIT)],
        "the stored unit of zeros is left a hole"
    );
}

#[test]
fn zero_units_are_aligned_to_the_file_not_the_buffer() {
    // A run starting mid-unit: the zeros straddling a unit boundary are
    // written, only a whole aligned unit of them is skipped.
    let mut bytes = pattern(3 * ZERO_UNIT, 6);
    bytes[ZERO_UNIT - 100..2 * ZERO_UNIT - 100].fill(0);
    let mut source = SparseFile::new(100 + 3 * ZERO_UNIT as u64, vec![(100, bytes)]);
    let target = copy(&mut source, Sparse::Always, 64 * 1024);
    assert_eq!(target.bytes, source.content());
    let skipped: usize = (3 * ZERO_UNIT) - target.writes.iter().map(|(_, len)| len).sum::<usize>();
    assert_eq!(skipped, ZERO_UNIT, "exactly the one aligned unit of zeros");
}

#[test]
fn a_source_with_no_word_on_its_holes_is_copied_dense() {
    let mut source = SparseFile::new(9000, vec![(4096, pattern(100, 7))]);
    source.blind = true;
    let target = copy(&mut source, Sparse::Auto, 4096);
    assert_eq!(target.bytes, source.content());
    assert_eq!(
        target.writes.iter().map(|(_, len)| len).sum::<usize>(),
        9000
    );
}

#[test]
fn a_source_that_shrinks_mid_copy_ends_the_copy_where_it_ended() {
    let mut source = SparseFile::new(10_000, vec![(0, pattern(10_000, 8))]);
    let mut target = Recorded::default();
    let mut buf = vec![0u8; 4096];
    let mut copy = DataCopy::new(source.len, Sparse::Auto);
    assert_eq!(
        copy.step(&mut source, &mut target, &mut buf),
        Ok(Step::Moved(4096))
    );
    source.len = 5000;
    source.runs[0].1.truncate(5000);
    while copy
        .step(&mut source, &mut target, &mut buf)
        .expect("copies")
        != Step::Done
    {}
    assert_eq!(target.bytes, source.content());
    assert!(
        target.set_lens.is_empty(),
        "the writes stopped where the source did"
    );
    assert_eq!(copy.remaining(), 0);
}

#[test]
fn regions_that_contradict_themselves_and_an_empty_buffer_are_refused() {
    struct Liar;
    impl Source for Liar {
        fn read_at(&mut self, _: u64, _: &mut [u8]) -> Result<usize, Errno> {
            Ok(0)
        }
        fn next_region(&mut self, offset: u64, _: SeekRegion) -> Result<Option<u64>, Errno> {
            Ok(Some(offset))
        }
        fn size(&mut self) -> Result<u64, Errno> {
            Ok(100)
        }
    }
    let mut target = Recorded::default();
    let mut copy = DataCopy::new(100, Sparse::Auto);
    assert_eq!(
        copy.step(&mut Liar, &mut target, &mut [0u8; 8]),
        Err(DataError::Read(Errno::DeviceFault))
    );
    let mut copy = DataCopy::new(100, Sparse::Auto);
    assert_eq!(
        copy.step(&mut Liar, &mut target, &mut []),
        Err(DataError::Read(Errno::BufferTooSmall))
    );
}

#[test]
fn a_read_claiming_more_than_its_buffer_held_is_refused_and_nothing_written() {
    struct Boaster;
    impl Source for Boaster {
        fn read_at(&mut self, _: u64, buf: &mut [u8]) -> Result<usize, Errno> {
            Ok(buf.len() + 1)
        }
        fn next_region(&mut self, offset: u64, _: SeekRegion) -> Result<Option<u64>, Errno> {
            Ok(Some(offset))
        }
        fn size(&mut self) -> Result<u64, Errno> {
            Ok(100)
        }
    }
    let mut target = Recorded::default();
    let mut copy = DataCopy::new(100, Sparse::Never);
    assert_eq!(
        copy.step(&mut Boaster, &mut target, &mut [0u8; 8]),
        Err(DataError::Read(Errno::LengthOutOfRange))
    );
    assert!(target.writes.is_empty());
}

#[test]
fn a_source_whose_data_ran_out_because_it_shrank_is_not_padded_back_out() {
    let mut source = SparseFile::new(
        20_000,
        vec![(0, pattern(4096, 9)), (12_000, pattern(4000, 10))],
    );
    let mut target = Recorded::default();
    let mut buf = vec![0u8; 4096];
    let mut copy = DataCopy::new(source.len, Sparse::Auto);
    assert_eq!(
        copy.step(&mut source, &mut target, &mut buf),
        Ok(Step::Moved(4096))
    );
    source.len = 8000;
    source.runs.truncate(1);
    while copy
        .step(&mut source, &mut target, &mut buf)
        .expect("copies")
        != Step::Done
    {}
    assert_eq!(target.bytes, source.content());
    assert_eq!(
        target.set_lens,
        [8000],
        "the source's new end, not its old one"
    );
}

#[test]
fn data_answered_before_where_it_was_asked_for_is_refused() {
    struct Backwards;
    impl Source for Backwards {
        fn read_at(&mut self, _: u64, buf: &mut [u8]) -> Result<usize, Errno> {
            buf.fill(7);
            Ok(buf.len())
        }
        fn next_region(&mut self, offset: u64, region: SeekRegion) -> Result<Option<u64>, Errno> {
            Ok(Some(match region {
                SeekRegion::Data => offset.saturating_sub(1),
                SeekRegion::Hole => offset + 4,
            }))
        }
        fn size(&mut self) -> Result<u64, Errno> {
            Ok(100)
        }
    }
    let mut target = Recorded::default();
    let mut copy = DataCopy::new(100, Sparse::Auto);
    let mut buf = [0u8; 8];
    assert_eq!(
        copy.step(&mut Backwards, &mut target, &mut buf),
        Ok(Step::Moved(4))
    );
    assert_eq!(
        copy.step(&mut Backwards, &mut target, &mut buf),
        Err(DataError::Read(Errno::DeviceFault))
    );
}

#[test]
fn a_run_punched_out_between_its_two_questions_is_asked_for_again() {
    /// Data at `0..100` when first asked; `0..50` punched out before the
    /// hole is asked for.
    struct Punched {
        data_asked: usize,
    }
    impl Source for Punched {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
            let n = buf.len().min(100 - offset as usize);
            buf[..n].fill(9);
            Ok(n)
        }
        fn next_region(&mut self, offset: u64, region: SeekRegion) -> Result<Option<u64>, Errno> {
            Ok(Some(match region {
                SeekRegion::Data => {
                    self.data_asked += 1;
                    if self.data_asked == 1 {
                        0
                    } else {
                        offset.max(50)
                    }
                }
                SeekRegion::Hole if offset < 50 => offset,
                SeekRegion::Hole => 100,
            }))
        }
        fn size(&mut self) -> Result<u64, Errno> {
            Ok(100)
        }
    }
    let mut source = Punched { data_asked: 0 };
    let mut target = Recorded::default();
    let mut buf = [0u8; 64];
    DataCopy::new(100, Sparse::Auto)
        .run(&mut source, &mut target, &mut buf)
        .expect("copies");
    let mut expected = vec![0u8; 50];
    expected.extend_from_slice(&[9u8; 50]);
    assert_eq!(target.bytes, expected);
    assert_eq!(target.writes, [(50, 50)]);
}
