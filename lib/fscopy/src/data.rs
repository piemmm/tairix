//! A file's bytes copied with its holes.

use tairix_abi::fs::SeekRegion;
use tairix_abi::Errno;

/// How a copy treats a sparse file's holes (GNU `cp --sparse`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Sparse {
    /// Keep the source's holes as holes (`--sparse=auto`).
    #[default]
    Auto,
    /// Keep the source's holes, and leave unwritten every run of zeros the
    /// source stores besides (`--sparse=always`).
    Always,
    /// Write every byte, a hole's as zeros (`--sparse=never`).
    Never,
}

/// Bytes of the file-aligned units [`Sparse::Always`] tests for zeros: a
/// page, the granule at which a destination leaves space unallocated.
pub const ZERO_UNIT: usize = 4096;

/// A file a copy reads.
pub trait Source {
    /// Read up to `buf.len()` bytes from `offset`, answering how many; `0`
    /// only at the file's end.
    ///
    /// # Errors
    ///
    /// The read's refusal.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno>;

    /// Where the next region of `region`'s kind begins at or after
    /// `offset`, `None` when none does before the file's end.
    ///
    /// # Errors
    ///
    /// The seek's refusal.
    fn next_region(&mut self, offset: u64, region: SeekRegion) -> Result<Option<u64>, Errno>;

    /// The file's length now: asked once its data runs out, so a source that
    /// shrank is not padded back out to the length it had.
    ///
    /// # Errors
    ///
    /// The stat's refusal.
    fn size(&mut self) -> Result<u64, Errno>;
}

/// A file a copy writes, empty as the copy starts.
pub trait Target {
    /// Write every byte of `bytes` at `offset`.
    ///
    /// # Errors
    ///
    /// The write's refusal.
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Errno>;

    /// Set the file's length to `len`: bytes past what was written read as
    /// zeros.
    ///
    /// # Errors
    ///
    /// The truncate's refusal.
    fn set_len(&mut self, len: u64) -> Result<(), Errno>;
}

/// Why a copy's bytes stopped: the side that refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DataError {
    /// Reading the source failed — [`Errno::DeviceFault`] for regions that
    /// contradict themselves (data before where it was asked for, or a run
    /// found empty twice at one place), [`Errno::LengthOutOfRange`] for a read claiming
    /// more bytes than its buffer held, [`Errno::BufferTooSmall`] for an empty
    /// buffer to read into.
    Read(Errno),
    /// Writing the target failed.
    Write(Errno),
}

/// What one step of a [`DataCopy`] did.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Step {
    /// This many more of the file's bytes are accounted for: copied, or a
    /// hole passed over.
    Moved(u64),
    /// The target holds the whole file.
    Done,
}

/// One file's bytes copied a chunk at a time: data where the source stores
/// it, a hole where it keeps one — and under [`Sparse::Always`] where the
/// bytes it stores are zeros. A source that shrinks while it is copied ends
/// the copy where it ended.
#[derive(Clone, Debug)]
pub struct DataCopy {
    len: u64,
    at: u64,
    /// Where the data run being copied ends.
    run_end: u64,
    sparse: Sparse,
    /// Where the target's last write ended.
    written: u64,
    done: bool,
}

impl DataCopy {
    /// The copy of a file `len` bytes long.
    #[must_use]
    pub const fn new(len: u64, sparse: Sparse) -> Self {
        Self {
            len,
            at: 0,
            run_end: 0,
            sparse,
            written: 0,
            done: false,
        }
    }

    /// The bytes of the file not yet accounted for.
    #[must_use]
    pub const fn remaining(&self) -> u64 {
        self.len.saturating_sub(self.at)
    }

    /// Copy the next chunk of the file through `buf`.
    ///
    /// # Errors
    ///
    /// The side that refused ([`DataError`]).
    pub fn step(
        &mut self,
        source: &mut impl Source,
        target: &mut impl Target,
        buf: &mut [u8],
    ) -> Result<Step, DataError> {
        if self.done {
            return Ok(Step::Done);
        }
        if buf.is_empty() {
            return Err(DataError::Read(Errno::BufferTooSmall));
        }
        if self.at >= self.len {
            return self.finish(target);
        }
        if self.at >= self.run_end {
            let start = self.at;
            let Some((data, run_end)) = self.next_run(source, start).map_err(DataError::Read)?
            else {
                // The rest is a hole, unless the source has ended sooner since.
                let now = source.size().map_err(DataError::Read)?;
                self.len = self.len.min(now).max(start);
                if self.len == start {
                    return self.finish(target);
                }
                self.at = self.len;
                return Ok(Step::Moved(self.len - start));
            };
            self.run_end = run_end;
            if data > start {
                self.at = data;
                return Ok(Step::Moved(data - start));
            }
        }
        let want =
            usize::try_from(self.run_end - self.at).map_or(buf.len(), |left| left.min(buf.len()));
        let read = source
            .read_at(self.at, &mut buf[..want])
            .map_err(DataError::Read)?;
        if read > want {
            return Err(DataError::Read(Errno::LengthOutOfRange));
        }
        if read == 0 {
            self.len = self.at;
            return self.finish(target);
        }
        self.put(target, &buf[..read]).map_err(DataError::Write)?;
        self.at += read as u64;
        Ok(Step::Moved(read as u64))
    }

    /// Run the copy to its end through `buf`.
    ///
    /// # Errors
    ///
    /// As [`Self::step`].
    pub fn run(
        mut self,
        source: &mut impl Source,
        target: &mut impl Target,
        buf: &mut [u8],
    ) -> Result<(), DataError> {
        while self.step(source, target, buf)? != Step::Done {}
        Ok(())
    }

    /// The next data run at or after `start`, where it begins and ends, or
    /// `None` when the rest of the file is a hole.
    fn next_run(&self, source: &mut impl Source, start: u64) -> Result<Option<(u64, u64)>, Errno> {
        let mut from = start;
        let mut emptied = None;
        loop {
            let Some(data) = self.next_data(source, from)? else {
                return Ok(None);
            };
            if data < from || emptied == Some(data) {
                return Err(Errno::DeviceFault);
            }
            let end = self.data_end(source, data)?;
            if end > data {
                return Ok(Some((data, end)));
            }
            // A punch between the two questions emptied the run: data is
            // asked for again from there, each answer further on.
            emptied = Some(data);
            from = data;
        }
    }

    /// Where the next stored byte at or after `start` lies, `None` when the
    /// rest of the file is a hole.
    fn next_data(&self, source: &mut impl Source, start: u64) -> Result<Option<u64>, Errno> {
        if self.sparse == Sparse::Never {
            return Ok(Some(start));
        }
        match source.next_region(start, SeekRegion::Data) {
            Ok(data) => Ok(data.filter(|&data| data < self.len)),
            // A source with no word on its holes is all data.
            Err(Errno::NotSupported) => Ok(Some(start)),
            Err(err) => Err(err),
        }
    }

    /// Where the data run starting at `data` ends.
    fn data_end(&self, source: &mut impl Source, data: u64) -> Result<u64, Errno> {
        if self.sparse == Sparse::Never {
            return Ok(self.len);
        }
        match source.next_region(data, SeekRegion::Hole) {
            Ok(Some(hole)) if hole < data => Err(Errno::DeviceFault),
            Ok(hole) => Ok(hole.map_or(self.len, |hole| hole.min(self.len))),
            Err(Errno::NotSupported) => Ok(self.len),
            Err(err) => Err(err),
        }
    }

    /// Write `bytes` read at the copy's offset: whole, or under
    /// [`Sparse::Always`] all but its file-aligned units of zeros.
    fn put(&mut self, target: &mut impl Target, bytes: &[u8]) -> Result<(), Errno> {
        let at = self.at;
        if self.sparse != Sparse::Always {
            target.write_all_at(at, bytes)?;
            self.written = at + bytes.len() as u64;
            return Ok(());
        }
        let mut pos = 0usize;
        let mut span: Option<usize> = None;
        while pos < bytes.len() {
            let unit =
                ZERO_UNIT - usize::try_from((at + pos as u64) % ZERO_UNIT as u64).unwrap_or(0);
            let end = (pos + unit).min(bytes.len());
            let zero = bytes[pos..end].iter().all(|&byte| byte == 0);
            match (zero, span) {
                (false, None) => span = Some(pos),
                (true, Some(from)) => {
                    target.write_all_at(at + from as u64, &bytes[from..pos])?;
                    self.written = at + pos as u64;
                    span = None;
                }
                _ => {}
            }
            pos = end;
        }
        if let Some(from) = span {
            target.write_all_at(at + from as u64, &bytes[from..])?;
            self.written = at + bytes.len() as u64;
        }
        Ok(())
    }

    /// End the copy: the target given the file's length where its writes
    /// stopped short of it.
    fn finish(&mut self, target: &mut impl Target) -> Result<Step, DataError> {
        if self.written != self.len {
            target.set_len(self.len).map_err(DataError::Write)?;
        }
        self.done = true;
        Ok(Step::Done)
    }
}

#[cfg(test)]
#[path = "data_tests.rs"]
mod tests;
