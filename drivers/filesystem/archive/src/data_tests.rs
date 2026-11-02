//! A staged file's runs held against a plain byte model through writes,
//! overwrites and truncations, read back through an attachment's staging
//! file as the driver reads one.

extern crate std;

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::volume::ARCHIVE_STAGING;
use tairix_fuzzseed::Prng;

use super::Staged;
use crate::region::Regions;
use crate::test_support::MemoryAttachment;

/// The runs are ordered, apart, of some length, within the file, and none
/// carries straight on from the one before in both the file and the
/// staging file, which a write would have merged.
fn assert_runs_in_shape(staged: &Staged) {
    let mut end = 0;
    let mut last: Option<(u64, u64, u64)> = None;
    for (&start, run) in &staged.runs {
        assert!(run.len > 0);
        assert!(start >= end, "runs overlap");
        assert!(start + run.len <= staged.len, "a run past the file's end");
        if let Some((last_start, last_at, last_len)) = last {
            assert!(
                !(last_start + last_len == start && last_at + last_len == run.at),
                "a write carrying straight on was not merged"
            );
        }
        last = Some((start, run.at, run.len));
        end = start + run.len;
    }
}

#[test]
fn staged_runs_read_back_exactly_what_a_byte_model_holds() {
    let attachment = MemoryAttachment::new(b"", true, b"archive.zip");
    let regions = Regions::new(attachment.access(), 0);
    let mut rng = Prng::new(0x5ea7_ed00);
    for _ in 0..40 {
        let mut staged = Staged::default();
        let mut model: Vec<u8> = Vec::new();
        let mut staging_end = 0u64;
        for _ in 0..60 {
            if rng.below(5) == 0 {
                let len = rng.below(3000);
                staged.truncate(len as u64);
                model.resize(len, 0);
            } else {
                let offset = rng.below(2500);
                let len = 1 + rng.below(700);
                let bytes: Vec<u8> = (0..len).map(|_| rng.next_u8()).collect();
                regions.write(ARCHIVE_STAGING, staging_end, &bytes).unwrap();
                staged
                    .wrote(offset as u64, len as u64, staging_end)
                    .unwrap();
                staging_end += len as u64;
                if model.len() < offset + len {
                    model.resize(offset + len, 0);
                }
                model[offset..offset + len].copy_from_slice(&bytes);
            }
            assert_eq!(staged.len, model.len() as u64);
            assert_runs_in_shape(&staged);
            let mut read = vec![0xAAu8; model.len() + 16];
            let got = staged.read(&regions, 0, &mut read).unwrap();
            assert_eq!(&read[..got], &model[..]);
            let from = rng.below(model.len() + 1);
            let mut piece = [0xAAu8; 37];
            let got = staged.read(&regions, from as u64, &mut piece).unwrap();
            assert_eq!(
                &piece[..got],
                &model[from..(from + piece.len()).min(model.len())]
            );
        }
    }
}

#[test]
fn a_write_that_carries_straight_on_lengthens_its_run() {
    let mut staged = Staged::default();
    staged.wrote(0, 10, 100).unwrap();
    staged.wrote(10, 5, 110).unwrap();
    assert_eq!(staged.runs.len(), 1);
    // The same place in the file, but elsewhere in the staging file.
    staged.wrote(15, 5, 500).unwrap();
    assert_eq!(staged.runs.len(), 2);
    assert_eq!(
        staged.wrote(u64::MAX, 2, 0),
        Err(tairix_abi::DriverError::LengthOutOfRange)
    );
}
