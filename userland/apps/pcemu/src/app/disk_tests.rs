use super::*;
use core::cell::RefCell;

/// An image in memory, its writes counted.
#[derive(Default)]
struct Held {
    bytes: RefCell<Vec<u8>>,
    writes: RefCell<usize>,
    broken: bool,
}

impl Medium for Held {
    fn read_at(&self, offset: u64, out: &mut [u8]) -> Result<usize, MediaError> {
        if self.broken {
            return Err(MediaError::Failed);
        }
        let bytes = self.bytes.borrow();
        let from = usize::try_from(offset).unwrap().min(bytes.len());
        // A short read, as a file's may be, to show the run is completed.
        let n = (bytes.len() - from).min(out.len()).min(700);
        out[..n].copy_from_slice(&bytes[from..from + n]);
        Ok(n)
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> Result<(), MediaError> {
        if self.broken {
            return Err(MediaError::Failed);
        }
        *self.writes.borrow_mut() += 1;
        let mut bytes = self.bytes.borrow_mut();
        let at = usize::try_from(offset).unwrap();
        if bytes.len() < at + data.len() {
            bytes.resize(at + data.len(), 0);
        }
        bytes[at..at + data.len()].copy_from_slice(data);
        Ok(())
    }

    fn set_len(&self, len: u64) -> Result<(), MediaError> {
        self.bytes
            .borrow_mut()
            .resize(usize::try_from(len).unwrap(), 0);
        Ok(())
    }
}

#[test]
fn a_run_reads_whole_however_the_image_answers_and_past_its_end_as_zeros() {
    let image = Held::default();
    image.set_len(3 * SECTOR as u64).unwrap();
    store(&image, 1, &[7; SECTOR]).unwrap();
    store(&image, 2, &[9; SECTOR]).unwrap();
    let mut out = Vec::new();
    fetch(&image, 1, 3, &mut out).unwrap();
    assert_eq!(out.len(), 3 * SECTOR);
    assert!(out[..SECTOR].iter().all(|&b| b == 7));
    assert!(out[SECTOR..2 * SECTOR].iter().all(|&b| b == 9));
    assert!(out[2 * SECTOR..].iter().all(|&b| b == 0), "past the end");
}

#[test]
fn a_run_whose_offset_overflows_is_out_of_range() {
    let image = Held::default();
    let mut out = Vec::new();
    assert_eq!(
        fetch(&image, u64::MAX, 1, &mut out),
        Err(MediaError::OutOfRange)
    );
    assert_eq!(
        store(&image, u64::MAX / 2, &[0; SECTOR]),
        Err(MediaError::OutOfRange)
    );
}

#[test]
fn a_failing_image_is_the_guests_disk_error() {
    let image = Held {
        broken: true,
        ..Held::default()
    };
    let mut out = Vec::new();
    assert_eq!(fetch(&image, 0, 1, &mut out), Err(MediaError::Failed));
    assert_eq!(store(&image, 0, &[0; SECTOR]), Err(MediaError::Failed));
}

#[test]
fn a_sparse_disk_is_laid_over_what_the_image_held_by_the_sectors_it_holds_then_its_length() {
    let mut disk = SparseDisk::new(10_000);
    disk.write(3, &[1; SECTOR]).unwrap();
    disk.write(9_999, &[2; SECTOR]).unwrap();
    let image = Held::default();
    image.set_len(4 * SECTOR as u64).unwrap();
    store(&image, 0, &[5; SECTOR]).unwrap();
    *image.writes.borrow_mut() = 0;
    lay(&image, &disk).unwrap();
    assert_eq!(*image.writes.borrow(), 2, "a write a sector held");
    let mut out = Vec::new();
    fetch(&image, 0, 1, &mut out).unwrap();
    assert!(
        out.iter().all(|&b| b == 0),
        "what the image held before is gone"
    );
    assert_eq!(image.bytes.borrow().len(), 10_000 * SECTOR);
    fetch(&image, 3, 1, &mut out).unwrap();
    assert!(out.iter().all(|&b| b == 1));
    fetch(&image, 9_999, 1, &mut out).unwrap();
    assert!(out.iter().all(|&b| b == 2));
}
