//! The archive attachment as the driver reaches it (`plans/ARCHIVES.md` §2):
//! the archive itself, read by byte, and for a writable attachment its two
//! work files — the staging file written data waits in, and the next archive
//! a commit writes and the kernel then makes the archive.

use alloc::vec::Vec;

use tairix_abi::volume::{
    VolumeCtl, ARCHIVE_COPY_MAX, ARCHIVE_NEXT, ARCHIVE_REGION, VOLUME_IO_MAX,
};
use tairix_abi::DriverError;
use tairix_fsserve::VolumeAccess;

/// The regions of the archive attachment `access` reaches.
pub(crate) struct Regions<A> {
    access: A,
    /// The archive's length.
    len: u64,
}

impl<A: VolumeAccess> Regions<A> {
    /// The attachment `access` reaches, whose archive is `len` bytes long.
    pub(crate) const fn new(access: A, len: u64) -> Self {
        Self { access, len }
    }

    /// The archive's length.
    pub(crate) const fn len(&self) -> u64 {
        self.len
    }

    /// Read up to `buf.len()` bytes at `offset` of `region`, answering how
    /// many: fewer only where the region ends.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    pub(crate) fn read(
        &self,
        region: u8,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, DriverError> {
        let mut done = 0;
        while done < buf.len() {
            let want = (buf.len() - done).min(VOLUME_IO_MAX);
            let at = offset
                .checked_add(done as u64)
                .ok_or(DriverError::LengthOutOfRange)?;
            let got = self
                .access
                .region_read(region, at, &mut buf[done..done + want])
                .map_err(DriverError::from_errno)?;
            done += got;
            if got < want {
                break;
            }
        }
        Ok(done)
    }

    /// Fill `buf` from `offset` of the archive: what a structure the archive
    /// says lies there needs, so an archive that ends sooner is damaged.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] for an archive too short to hold it; the
    /// kernel's refusal.
    pub(crate) fn read_archive(&self, offset: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        if self.read(ARCHIVE_REGION, offset, buf)? == buf.len() {
            Ok(())
        } else {
            Err(DriverError::BadMagic)
        }
    }

    /// `len` bytes at `offset` of the archive, read whole.
    ///
    /// # Errors
    ///
    /// As [`Self::read_archive`], and [`DriverError::OutOfMemory`].
    pub(crate) fn archive_bytes(&self, offset: u64, len: usize) -> Result<Vec<u8>, DriverError> {
        let mut bytes = tairix_util::fallible::filled(len, 0u8).ok_or(DriverError::OutOfMemory)?;
        self.read_archive(offset, &mut bytes)?;
        Ok(bytes)
    }

    /// Write `data` at `offset` of work file `region`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    pub(crate) fn write(&self, region: u8, offset: u64, data: &[u8]) -> Result<(), DriverError> {
        for (at, chunk) in (0..).step_by(VOLUME_IO_MAX).zip(data.chunks(VOLUME_IO_MAX)) {
            let at = offset
                .checked_add(at)
                .ok_or(DriverError::LengthOutOfRange)?;
            self.access
                .region_write(region, at, chunk)
                .map_err(DriverError::from_errno)?;
        }
        Ok(())
    }

    /// Copy `len` bytes at `from_offset` of region `from` to `to_offset` of
    /// work file `to`, in the kernel.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    pub(crate) fn copy(
        &self,
        (from, from_offset): (u8, u64),
        (to, to_offset): (u8, u64),
        len: u64,
    ) -> Result<(), DriverError> {
        let mut done = 0;
        while done < len {
            let step = (len - done).min(ARCHIVE_COPY_MAX);
            self.ctl(&VolumeCtl::Copy {
                from,
                from_offset: from_offset + done,
                to,
                to_offset: to_offset + done,
                len: step,
            })?;
            done += step;
        }
        Ok(())
    }

    /// Cut or extend work file `region` to `len` bytes.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    pub(crate) fn truncate(&self, region: u8, len: u64) -> Result<(), DriverError> {
        self.ctl(&VolumeCtl::Truncate { region, len })
    }

    /// Make the next archive, cut to `len` bytes, the archive.
    ///
    /// # Errors
    ///
    /// The kernel's refusal, which leaves the archive as it was.
    pub(crate) fn commit(&mut self, len: u64) -> Result<(), DriverError> {
        self.ctl(&VolumeCtl::Commit { len })?;
        self.len = len;
        Ok(())
    }

    /// Make every write the attachment accepted durable.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    pub(crate) fn flush(&self) -> Result<(), DriverError> {
        self.ctl(&VolumeCtl::Flush)
    }

    fn ctl(&self, ctl: &VolumeCtl) -> Result<(), DriverError> {
        self.access
            .ctl(ctl)
            .map(|_| ())
            .map_err(DriverError::from_errno)
    }
}

/// How much of the archive an [`Input`] reads at once.
const INPUT_LEN: usize = 64 * 1024;

/// A range of the archive, read a buffer at a time, for a decoder that takes
/// its bytes as they come.
pub(crate) struct Input {
    /// The next byte not yet read into `buf`.
    next: u64,
    end: u64,
    buf: Vec<u8>,
    /// What of `buf` is read and not yet consumed.
    start: usize,
    stop: usize,
}

impl Input {
    /// The `len` bytes at `at` of the archive.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] for a range past any archive's end;
    /// [`DriverError::OutOfMemory`].
    pub(crate) fn new(at: u64, len: u64) -> Result<Self, DriverError> {
        Ok(Self {
            next: at,
            end: at.checked_add(len).ok_or(DriverError::BadMagic)?,
            buf: tairix_util::fallible::filled(INPUT_LEN, 0u8).ok_or(DriverError::OutOfMemory)?,
            start: 0,
            stop: 0,
        })
    }

    /// Read the `len` bytes at `at` instead, keeping the buffer.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] for a range past any archive's end.
    pub(crate) fn reset(&mut self, at: u64, len: u64) -> Result<(), DriverError> {
        self.end = at.checked_add(len).ok_or(DriverError::BadMagic)?;
        self.next = at;
        self.start = 0;
        self.stop = 0;
        Ok(())
    }

    /// Where the next unconsumed byte is in the archive.
    pub(crate) fn position(&self) -> u64 {
        self.next - (self.stop - self.start) as u64
    }

    /// Read on from `at`, what is held of the range before it let go.
    pub(crate) fn seek(&mut self, at: u64) {
        let held = self.next - self.stop as u64..self.next;
        match usize::try_from(at.wrapping_sub(held.start)) {
            Ok(into) if held.contains(&at) => self.start = into,
            _ => {
                self.next = at;
                self.start = 0;
                self.stop = 0;
            }
        }
    }

    /// The bytes not yet consumed, reading more when none are; empty at the
    /// range's end.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] for an archive shorter than the range; the
    /// kernel's refusal.
    pub(crate) fn fill<A: VolumeAccess>(
        &mut self,
        regions: &Regions<A>,
    ) -> Result<&[u8], DriverError> {
        if self.start == self.stop && self.next < self.end {
            let want =
                usize::try_from(self.end - self.next).map_or(INPUT_LEN, |left| left.min(INPUT_LEN));
            regions.read_archive(self.next, &mut self.buf[..want])?;
            self.next += want as u64;
            self.start = 0;
            self.stop = want;
        }
        Ok(&self.buf[self.start..self.stop])
    }

    /// The bytes held and not yet consumed, reading none.
    pub(crate) fn held(&self) -> &[u8] {
        &self.buf[self.start..self.stop]
    }

    /// Consume `n` of the bytes [`Self::fill`] answered.
    pub(crate) fn consume(&mut self, n: usize) {
        self.start = (self.start + n).min(self.stop);
    }
}

/// An input as a codec's, a byte at a time, holding a refusal met reading
/// for the codec's caller.
pub(crate) struct ByteInput<'i, 'r, A> {
    input: &'i mut Input,
    regions: &'r Regions<A>,
    fault: Option<DriverError>,
}

impl<'i, 'r, A> ByteInput<'i, 'r, A> {
    /// `input`, read through `regions`.
    pub(crate) fn new(input: &'i mut Input, regions: &'r Regions<A>) -> Self {
        Self {
            input,
            regions,
            fault: None,
        }
    }

    /// The read refusal the codec met, else `otherwise`.
    pub(crate) fn fault_or(&mut self, otherwise: DriverError) -> DriverError {
        self.fault.take().unwrap_or(otherwise)
    }
}

impl<A: VolumeAccess> ByteInput<'_, '_, A> {
    /// The bytes held and not yet consumed, reading more when none are;
    /// empty at the range's end or on a refusal, which is held.
    pub(crate) fn chunk(&mut self) -> &[u8] {
        match self.input.fill(self.regions) {
            Ok(_) => self.input.held(),
            Err(err) => {
                self.fault = Some(err);
                &[]
            }
        }
    }

    /// Consume `n` of the bytes [`Self::chunk`] answered.
    pub(crate) fn consume(&mut self, n: usize) {
        self.input.consume(n);
    }
}

impl<A: VolumeAccess> tairix_compress::Input for ByteInput<'_, '_, A> {
    fn byte(&mut self) -> Option<u8> {
        match self.input.fill(self.regions) {
            Ok(&[first, ..]) => {
                self.input.consume(1);
                Some(first)
            }
            Ok(_) => None,
            Err(err) => {
                self.fault = Some(err);
                None
            }
        }
    }
}

/// The next archive as an encoder's output, holding the refusal it met for
/// the encoder's caller.
pub(crate) struct SinkOutput<'s, 'r, A> {
    sink: &'s mut Sink<'r, A>,
    fault: Option<DriverError>,
}

impl<'s, 'r, A> SinkOutput<'s, 'r, A> {
    /// The output `sink` takes.
    pub(crate) fn new(sink: &'s mut Sink<'r, A>) -> Self {
        Self { sink, fault: None }
    }

    /// The refusal the encoder met writing, if it met one.
    pub(crate) fn fault(&mut self) -> Option<DriverError> {
        self.fault.take()
    }
}

impl<A: VolumeAccess> tairix_compress::Output for SinkOutput<'_, '_, A> {
    fn put(&mut self, bytes: &[u8]) -> bool {
        match self.sink.put(bytes) {
            Ok(()) => true,
            Err(err) => {
                self.fault = Some(err);
                false
            }
        }
    }
}

/// How much of the next archive is gathered before it is written.
const SINK_LEN: usize = 64 * 1024;

/// The next archive, written from its start to its end: bytes the driver
/// makes, gathered into whole writes, and ranges of the archive or the
/// staging file the kernel copies in their place.
pub(crate) struct Sink<'r, A> {
    regions: &'r Regions<A>,
    /// Bytes written or copied so far.
    at: u64,
    pending: Vec<u8>,
}

impl<'r, A: VolumeAccess> Sink<'r, A> {
    /// The next archive, empty, over `regions`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal to empty the next archive, or
    /// [`DriverError::OutOfMemory`].
    pub(crate) fn new(regions: &'r Regions<A>) -> Result<Self, DriverError> {
        regions.truncate(ARCHIVE_NEXT, 0)?;
        let mut pending = Vec::new();
        pending
            .try_reserve_exact(SINK_LEN)
            .map_err(|_| DriverError::OutOfMemory)?;
        Ok(Self {
            regions,
            at: 0,
            pending,
        })
    }

    /// Where the next byte lands.
    pub(crate) fn position(&self) -> u64 {
        self.at + self.pending.len() as u64
    }

    /// Append `bytes`.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    pub(crate) fn put(&mut self, mut bytes: &[u8]) -> Result<(), DriverError> {
        while !bytes.is_empty() {
            let room = SINK_LEN - self.pending.len();
            let (now, later) = bytes.split_at(room.min(bytes.len()));
            self.pending.extend_from_slice(now);
            bytes = later;
            if self.pending.len() == SINK_LEN {
                self.drain()?;
            }
        }
        Ok(())
    }

    /// Append `len` bytes at `offset` of region `from`, copied by the kernel.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    pub(crate) fn copy(&mut self, from: u8, offset: u64, len: u64) -> Result<(), DriverError> {
        if len == 0 {
            return Ok(());
        }
        self.drain()?;
        self.regions
            .copy((from, offset), (ARCHIVE_NEXT, self.at), len)?;
        self.at += len;
        Ok(())
    }

    /// Write `bytes` over what was put at `at`, which they must not run
    /// past: a header whose sizes are known once its member is written.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] for bytes past what was put; the
    /// kernel's refusal.
    pub(crate) fn patch(&mut self, at: u64, bytes: &[u8]) -> Result<(), DriverError> {
        let end = at
            .checked_add(bytes.len() as u64)
            .ok_or(DriverError::LengthOutOfRange)?;
        if end > self.position() {
            return Err(DriverError::LengthOutOfRange);
        }
        if at >= self.at {
            let from = usize::try_from(at - self.at).map_err(|_| DriverError::LengthOutOfRange)?;
            self.pending[from..from + bytes.len()].copy_from_slice(bytes);
            return Ok(());
        }
        // The part already written is rewritten in place, the rest patched in
        // what is gathered.
        let written = usize::try_from((self.at - at).min(bytes.len() as u64))
            .map_err(|_| DriverError::LengthOutOfRange)?;
        self.regions.write(ARCHIVE_NEXT, at, &bytes[..written])?;
        let rest = &bytes[written..];
        self.pending[..rest.len()].copy_from_slice(rest);
        Ok(())
    }

    /// Cut the next archive back to `at`, discarding what was put after it.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] for a place past what was put; the
    /// kernel's refusal.
    pub(crate) fn rewind(&mut self, at: u64) -> Result<(), DriverError> {
        if at > self.position() {
            return Err(DriverError::LengthOutOfRange);
        }
        if at >= self.at {
            let keep = usize::try_from(at - self.at).map_err(|_| DriverError::LengthOutOfRange)?;
            self.pending.truncate(keep);
            return Ok(());
        }
        self.pending.clear();
        self.regions.truncate(ARCHIVE_NEXT, at)?;
        self.at = at;
        Ok(())
    }

    /// Write what is gathered, answering the next archive's length.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    pub(crate) fn finish(mut self) -> Result<u64, DriverError> {
        self.drain()?;
        Ok(self.at)
    }

    fn drain(&mut self) -> Result<(), DriverError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        self.regions.write(ARCHIVE_NEXT, self.at, &self.pending)?;
        self.at += self.pending.len() as u64;
        self.pending.clear();
        Ok(())
    }
}
