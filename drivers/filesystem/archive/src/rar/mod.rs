//! RAR archives (`plans/ARCHIVES.md` AR9), read only: RAR 1.5 to 4 and
//! RAR 5 headers, and every compression they use — RAR 1.5's adaptive
//! coding, RAR 2's LZ and audio modes, RAR 3's LZ, PPMd and standard
//! filters, and RAR 5's.
//!
//! A solid archive codes its members as one stream, each carrying on from
//! the decoder's state where the last left it, so a member there is read by
//! decoding its run from the start: the first member not solid. A cursor
//! keeps a run's decoder where the last read stopped, so a reader working
//! through a solid archive in order decodes it once.
//!
//! RAR's compression is its authors' and no RAR is written here: the format
//! is read only, and refuses every change.

mod bits;
mod filter;
mod huffman;
mod lz;
mod ppmd;
mod rar4;
mod rar5;
mod v15;
mod v20;
mod v29;
mod v50;
mod window;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{NameMatching, NodeTimes, WriteRefusal};
use tairix_abi::fs::FS_SYMLINK_MAX;
use tairix_abi::time::Time64;
use tairix_abi::DriverError;
use tairix_fsserve::VolumeAccess;

use crate::data::{Archived, Check as DataCheck, Coding, Data, Heap, Lead};
use crate::format::{leaves_out, Format, Read};
use crate::name::{spelled, Spelled};
use crate::region::{ByteInput, Input, Regions};
use crate::tree::{Body, Meta, Recorded, Tree};
use crate::zip::directory_meta;

use self::bits::{Bits, Reader as BitReader};
use self::window::Window;

/// What a RAR archive keeps of itself beyond its members: nothing, as
/// nothing of it is written.
pub(crate) struct Rar;

/// How a member's bytes are coded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Method {
    Stored,
    /// RAR 1.5's.
    V15,
    /// RAR 2's.
    V20,
    /// RAR 3's.
    V29,
    /// RAR 5's, or RAR 7's where `v1`.
    V50 {
        v1: bool,
    },
}

impl Method {
    /// The furthest back a match of this coding reaches in a member whose
    /// header gives `dict`: no further than its distance slots go, whatever
    /// a header claims.
    const fn reach(self, dict: u64) -> u64 {
        let farthest = match self {
            Self::Stored => 0,
            Self::V15 => 0x1_0000,
            Self::V20 => 0x10_0000,
            Self::V29 => 0x40_0000,
            Self::V50 { v1: false } => 1 << 32,
            Self::V50 { v1: true } => 1 << 40,
        };
        match self {
            Self::V29 | Self::V50 { .. } if dict < farthest => dict,
            _ => farthest,
        }
    }

    /// Whether a member coded so carries on from one coded as `prev`.
    const fn continues(self, prev: Self) -> bool {
        matches!(
            (self, prev),
            (Self::V15, Self::V15)
                | (Self::V20, Self::V20)
                | (Self::V29, Self::V29)
                | (Self::V50 { v1: false }, Self::V50 { v1: false })
                | (Self::V50 { v1: true }, Self::V50 { v1: true })
        )
    }
}

/// What a member's bytes are checked against.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Check {
    None,
    Crc32(u32),
    Blake2sp([u8; 32]),
}

/// A check being taken over a member's bytes.
enum Sum {
    None,
    Crc32(tairix_crc32::Crc32),
    Blake2sp(Heap<tairix_crypto::hash::Blake2spStream>),
}

impl Sum {
    fn of(check: &Check) -> Result<Self, DriverError> {
        Ok(match check {
            Check::None => Self::None,
            Check::Crc32(_) => Self::Crc32(tairix_crc32::Crc32::new()),
            Check::Blake2sp(_) => {
                Self::Blake2sp(Heap::new(tairix_crypto::hash::Blake2spStream::new())?)
            }
        })
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::None => {}
            Self::Crc32(crc) => crc.update(bytes),
            Self::Blake2sp(hash) => {
                if let Ok(hash) = hash.get() {
                    hash.update(bytes);
                }
            }
        }
    }

    fn holds(&self, check: &Check) -> bool {
        match (self, check) {
            (Self::None, Check::None) => true,
            (Self::Crc32(crc), Check::Crc32(expected)) => crc.finish() == *expected,
            (Self::Blake2sp(hash), Check::Blake2sp(expected)) => hash
                .get_ref()
                .is_ok_and(|hash| hash.finalize() == *expected),
            _ => false,
        }
    }
}

/// One member's bytes as the archive keeps them.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    /// Where its packed data lies in the archive, and how long it is.
    pub(crate) at: u64,
    pub(crate) packed: u64,
    /// How many bytes it decodes to.
    pub(crate) len: u64,
    pub(crate) method: Method,
    /// The dictionary its header gives.
    pub(crate) dict: u64,
    /// Whether it carries on from the member coded before it.
    pub(crate) solid: bool,
    pub(crate) check: Check,
    /// Why its bytes cannot be read, where they cannot.
    pub(crate) refused: Option<DriverError>,
}

impl Entry {
    /// Its packed data's length in bits: exact for every entry the reader
    /// does not refuse, which holds no more than that counts.
    const fn bits(&self) -> u64 {
        self.packed.saturating_mul(8)
    }

    /// The `packed` bytes at `at` of a member whose header could not be
    /// read, coded as `method` if at all: refused, and refusing every member
    /// carrying on from it, since none can be decoded past it.
    pub(super) const fn lost(at: u64, packed: u64, method: Method) -> Self {
        Self {
            at,
            packed,
            len: 0,
            method,
            dict: 0,
            solid: true,
            check: Check::None,
            refused: Some(DriverError::MediumError),
        }
    }
}

/// Where an entry stands in the archive's runs.
#[derive(Copy, Clone)]
struct Place {
    /// The first entry of its run.
    run: usize,
    /// The next entry of its run.
    next: Option<usize>,
    /// The bytes its run decodes in all, kept at the run's first entry.
    total: u64,
}

/// Where a reader's output goes.
struct Sink<'a> {
    out: &'a mut [u8],
    filled: usize,
}

impl<'a> Sink<'a> {
    fn new(out: &'a mut [u8]) -> Self {
        Self { out, filled: 0 }
    }

    /// Whether it has room for more.
    fn wants(&self) -> bool {
        self.filled < self.out.len()
    }

    /// How many more bytes it has room for.
    fn room(&self) -> u64 {
        (self.out.len() - self.filled) as u64
    }

    /// Take what it has room for of `bytes`, at most `limit` of them,
    /// answering how many.
    fn give(&mut self, bytes: &[u8], limit: u64) -> usize {
        let n = bytes
            .len()
            .min(self.out.len() - self.filled)
            .min(usize::try_from(limit).unwrap_or(usize::MAX));
        self.out[self.filled..self.filled + n].copy_from_slice(&bytes[..n]);
        self.filled += n;
        n
    }

    /// Take what it has room for of `len` decoded bytes from `from`.
    fn give_window(&mut self, win: &Window, from: u64, len: u64) -> Result<u64, DriverError> {
        let n = usize::try_from(len)
            .unwrap_or(usize::MAX)
            .min(self.out.len() - self.filled);
        win.read(from, &mut self.out[self.filled..self.filled + n])?;
        self.filled += n;
        Ok(n as u64)
    }
}

/// The decoder a run's coding needs.
enum Unpack {
    Stored,
    V15(Heap<v15::V15>),
    V20(Heap<v20::V20>),
    V29(Heap<v29::V29>),
    V50(Heap<v50::V50>),
}

impl Unpack {
    fn new(method: Method) -> Result<Self, DriverError> {
        Ok(match method {
            Method::Stored => Self::Stored,
            Method::V15 => Self::V15(Heap::new(v15::V15::new())?),
            Method::V20 => Self::V20(Heap::new(v20::V20::new())?),
            Method::V29 => Self::V29(Heap::new(v29::V29::new())?),
            Method::V50 { v1 } => Self::V50(Heap::new(v50::V50::new(v1))?),
        })
    }
}

/// A run being decoded, where the last read left it.
struct Cursor {
    run: usize,
    /// The entry it stands in, whether that entry's data is begun, and the
    /// bytes of it given out.
    entry: usize,
    begun: bool,
    given: u64,
    window: Window,
    unpack: Unpack,
    bits: Bits,
    input: Input,
    /// The entry's check, taken over its bytes from its first; `None` once
    /// bytes were passed over unread.
    sum: Option<Sum>,
    used: u64,
}

/// The most runs decoded at once: one each for a few readers.
const CURSORS: usize = 4;

/// Bytes passed over through a scratch buffer this long.
const SKIP_LEN: usize = 16 * 1024;

/// The archive's members' bytes, decoded as their runs require.
pub(crate) struct Reader {
    entries: Vec<Entry>,
    places: Vec<Place>,
    cursors: Vec<Cursor>,
    clock: u64,
}

impl Reader {
    /// The reader of `entries`, in archive order, each solid one joined to
    /// the run of the entry coded before it.
    fn new(mut entries: Vec<Entry>) -> Result<Self, DriverError> {
        let mut places = Vec::new();
        places
            .try_reserve_exact(entries.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        let mut last: Option<usize> = None;
        for k in 0..entries.len() {
            let entry = &mut entries[k];
            // Its bytes would run past its data into what follows, or are
            // more than any decoder counts.
            if (entry.method == Method::Stored && entry.packed != entry.len)
                || entry.packed > u64::MAX / 8
            {
                entry.refused.get_or_insert(DriverError::MediumError);
            }
            let entry = &entries[k];
            if entry.method == Method::Stored {
                places.push(Place {
                    run: k,
                    next: None,
                    total: entry.len,
                });
                continue;
            }
            let joined = last.filter(|_| entry.solid);
            let mut run = k;
            if let Some(prev) = joined {
                let prev_entry = &entries[prev];
                let start = places[prev].run;
                let total = places[start].total.checked_add(entry.len);
                let why = prev_entry.refused.or_else(|| {
                    (!entry.method.continues(prev_entry.method)
                        || entry.dict > entries[start].dict
                        || total.is_none())
                    .then_some(DriverError::MediumError)
                });
                match (why, total) {
                    (None, Some(total)) => {
                        run = start;
                        places[prev].next = Some(k);
                        places[start].total = total;
                    }
                    (why, _) => {
                        entries[k]
                            .refused
                            .get_or_insert(why.unwrap_or(DriverError::MediumError));
                    }
                }
            }
            places.push(Place {
                run,
                next: None,
                total: if run == k { entries[k].len } else { 0 },
            });
            last = Some(k);
        }
        Ok(Self {
            entries,
            places,
            cursors: Vec::new(),
            clock: 0,
        })
    }

    /// Read up to `buf.len()` bytes at `offset` of entry `entry`, answering
    /// how many.
    ///
    /// # Errors
    ///
    /// The entry's own refusal; [`DriverError::MediumError`] for data out of
    /// shape or failing its check; [`DriverError::OutOfMemory`] where its
    /// run's decoder cannot have the memory it needs, even once every other
    /// run's has given its own up.
    pub(crate) fn read<A: VolumeAccess>(
        &mut self,
        regions: &Regions<A>,
        entry: usize,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, DriverError> {
        let target = self.entries.get(entry).ok_or(DriverError::DeviceFault)?;
        if let Some(why) = target.refused {
            return Err(why);
        }
        let want = usize::try_from(target.len.saturating_sub(offset))
            .map_or(buf.len(), |left| left.min(buf.len()));
        if want == 0 {
            return Ok(0);
        }
        let buf = &mut buf[..want];
        match self.read_by_cursor(regions, entry, offset, buf) {
            Err(DriverError::OutOfMemory) if !self.cursors.is_empty() => {
                // Give the memory other runs' decoders hold to this one.
                self.cursors.clear();
                self.read_by_cursor(regions, entry, offset, buf)
            }
            outcome => outcome,
        }
    }

    /// [`Self::read`] through the cursor nearest before `offset`.
    fn read_by_cursor<A: VolumeAccess>(
        &mut self,
        regions: &Regions<A>,
        entry: usize,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, DriverError> {
        let at = self.cursor(entry, offset)?;
        self.clock += 1;
        let mut cursor = self.cursors.swap_remove(at);
        cursor.used = self.clock;
        let outcome = self.serve(regions, &mut cursor, entry, offset, buf);
        // A cursor that met an error is in no state to carry on.
        if outcome.is_ok() {
            self.cursors.push(cursor);
        }
        outcome
    }

    /// The slot of a cursor in `entry`'s run at or before `offset` of it,
    /// made afresh at the run's start where none is.
    fn cursor(&mut self, entry: usize, offset: u64) -> Result<usize, DriverError> {
        let run = self.places[entry].run;
        let stored = self.entries[entry].method == Method::Stored;
        let best = self
            .cursors
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                c.run == run
                    && (c.entry < entry || (c.entry == entry && c.given <= offset))
                    // Passing over a stored member's bytes ends the check a
                    // cursor holds them to, which its own reader still needs.
                    && !(stored && c.entry == entry && c.given < offset && c.sum.is_some())
            })
            .max_by_key(|(_, c)| (c.entry, c.given))
            .map(|(at, _)| at);
        if let Some(at) = best {
            return Ok(at);
        }
        if self.cursors.len() >= CURSORS {
            let oldest = self
                .cursors
                .iter()
                .enumerate()
                .min_by_key(|(_, c)| c.used)
                .map_or(0, |(at, _)| at);
            self.cursors.swap_remove(oldest);
        }
        let cursor = self.fresh(run)?;
        self.cursors
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        self.cursors.push(cursor);
        Ok(self.cursors.len() - 1)
    }

    fn fresh(&self, run: usize) -> Result<Cursor, DriverError> {
        let first = &self.entries[run];
        let window = if first.method == Method::Stored {
            Window::new(0, 0)?
        } else {
            Window::new(first.method.reach(first.dict), self.places[run].total)?
        };
        Ok(Cursor {
            run,
            entry: run,
            begun: false,
            given: 0,
            window,
            unpack: Unpack::new(first.method)?,
            bits: Bits::default(),
            input: Input::new(first.at, 0)?,
            sum: None,
            used: 0,
        })
    }

    /// Bring `cursor` to `offset` of `entry`, then give out `buf`'s worth.
    fn serve<A: VolumeAccess>(
        &self,
        regions: &Regions<A>,
        cursor: &mut Cursor,
        entry: usize,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, DriverError> {
        let mut scratch = [0u8; SKIP_LEN];
        loop {
            if !cursor.begun {
                self.begin(regions, cursor)?;
            }
            if cursor.entry == entry {
                break;
            }
            // An entry before the one wanted, decoded through and set aside.
            while cursor.given < self.entries[cursor.entry].len {
                let gap = self.entries[cursor.entry].len - cursor.given;
                let room = usize::try_from(gap).map_or(SKIP_LEN, |gap| gap.min(SKIP_LEN));
                let _ = self.give(regions, cursor, &mut scratch[..room], false)?;
            }
            self.finish(regions, cursor)?;
            cursor.entry = self.places[cursor.entry]
                .next
                .ok_or(DriverError::DeviceFault)?;
            cursor.begun = false;
        }
        if self.entries[entry].method == Method::Stored && cursor.given < offset {
            cursor.input.seek(self.entries[entry].at + offset);
            cursor.given = offset;
            cursor.sum = None;
        }
        while cursor.given < offset {
            let gap = offset - cursor.given;
            let room = usize::try_from(gap).map_or(SKIP_LEN, |gap| gap.min(SKIP_LEN));
            self.give(regions, cursor, &mut scratch[..room], true)?;
        }
        self.give(regions, cursor, buf, true)
    }

    /// Begin the cursor's entry: its data read from the start, its check
    /// begun.
    fn begin<A: VolumeAccess>(
        &self,
        regions: &Regions<A>,
        cursor: &mut Cursor,
    ) -> Result<(), DriverError> {
        let entry = &self.entries[cursor.entry];
        cursor.input.reset(entry.at, entry.packed)?;
        cursor.bits = Bits::default();
        cursor.given = 0;
        cursor.sum = Some(Sum::of(&entry.check)?);
        let Cursor {
            window,
            unpack,
            bits,
            input,
            ..
        } = cursor;
        let mut bytes = ByteInput::new(input, regions);
        let mut br = BitReader::new(bits, &mut bytes);
        let begun = match unpack {
            Unpack::Stored => Ok(()),
            Unpack::V15(d) => d.get().and_then(|d| d.begin(window, entry.len)),
            Unpack::V20(d) => d
                .get()
                .and_then(|d| d.begin(&mut br, window, entry.len, entry.packed)),
            Unpack::V29(d) => d
                .get()
                .and_then(|d| d.begin(&mut br, window, entry.len, entry.packed)),
            Unpack::V50(d) => d.get().and_then(|d| d.begin(&mut br, window, entry.bits())),
        };
        begun.map_err(|err| bytes.fault_or(err))?;
        cursor.begun = true;
        Ok(())
    }

    /// Give out the cursor's entry's next bytes into `out`, held to its
    /// check as they reach its end; a check that fails is refused only
    /// where `checked`.
    fn give<A: VolumeAccess>(
        &self,
        regions: &Regions<A>,
        cursor: &mut Cursor,
        out: &mut [u8],
        checked: bool,
    ) -> Result<usize, DriverError> {
        let entry = &self.entries[cursor.entry];
        let Cursor {
            window,
            unpack,
            bits,
            input,
            ..
        } = cursor;
        let filled = if let Unpack::Stored = unpack {
            let got = regions.read(tairix_abi::volume::ARCHIVE_REGION, input.position(), out)?;
            input.seek(input.position() + got as u64);
            got
        } else {
            let mut sink = Sink::new(out);
            let mut bytes = ByteInput::new(input, regions);
            let mut br = BitReader::new(bits, &mut bytes);
            let read = match unpack {
                Unpack::Stored => Ok(()),
                Unpack::V15(d) => d.get().and_then(|d| d.read(&mut br, window, &mut sink)),
                Unpack::V20(d) => d.get().and_then(|d| d.read(&mut br, window, &mut sink)),
                Unpack::V29(d) => d.get().and_then(|d| d.read(&mut br, window, &mut sink)),
                Unpack::V50(d) => d
                    .get()
                    .and_then(|d| d.read(&mut br, window, entry.len, entry.bits(), &mut sink)),
            };
            read.map_err(|err| bytes.fault_or(err))?;
            sink.filled
        };
        if filled == 0 {
            return Err(DriverError::MediumError);
        }
        if let Some(sum) = &mut cursor.sum {
            sum.update(&out[..filled]);
        }
        cursor.given += filled as u64;
        if cursor.given == entry.len {
            let holds = cursor.sum.take().is_none_or(|sum| sum.holds(&entry.check));
            if !holds && checked {
                return Err(DriverError::MediumError);
            }
        }
        Ok(filled)
    }

    /// Decode the rest of the cursor's entry's data, so the next entry of
    /// its run starts where it should.
    fn finish<A: VolumeAccess>(
        &self,
        regions: &Regions<A>,
        cursor: &mut Cursor,
    ) -> Result<(), DriverError> {
        let entry = &self.entries[cursor.entry];
        let Cursor {
            window,
            unpack,
            bits,
            input,
            ..
        } = cursor;
        let mut bytes = ByteInput::new(input, regions);
        let mut br = BitReader::new(bits, &mut bytes);
        let finished = match unpack {
            Unpack::Stored => Ok(()),
            Unpack::V15(d) => d.get().map(v15::V15::finish),
            Unpack::V20(d) => d.get().map(v20::V20::finish),
            Unpack::V29(d) => d.get().and_then(|d| d.finish(&mut br, window)),
            Unpack::V50(d) => d
                .get()
                .and_then(|d| d.finish(&mut br, window, entry.bits())),
        };
        finished.map_err(|err| bytes.fault_or(err))
    }
}

/// What a member is.
pub(super) enum Kind {
    Dir,
    File(Entry),
    /// A symbolic link whose target its header records.
    Link(Vec<u8>),
    /// A symbolic link whose target is its data.
    LinkData(Entry),
    /// A copy of the earlier member at this path.
    Copy(Vec<u8>),
    /// Bytes served under no name, which keep their place in their solid
    /// run: a member named as no volume serves a name, or one whose header
    /// could not be read.
    Unlisted(Entry),
}

/// A member as its header records it.
pub(super) struct Member {
    /// Its path, in UTF-8, cut at `/`.
    pub(super) name: Vec<u8>,
    pub(super) kind: Kind,
    pub(super) mode: u32,
    pub(super) modified: Time64,
}

impl Member {
    /// `entry`'s bytes, served under no name.
    pub(super) const fn unlisted(entry: Entry) -> Self {
        Self {
            name: Vec::new(),
            kind: Kind::Unlisted(entry),
            mode: 0,
            modified: Time64::UNIX_EPOCH,
        }
    }
}

/// Read the RAR archive `regions` holds.
///
/// # Errors
///
/// [`DriverError::BadMagic`] for no archive this reads; the refusal of an
/// archive this does not read — encrypted headers, several volumes — with
/// its reason.
pub(crate) fn read<A: VolumeAccess>(regions: &Regions<A>) -> Result<Read, DriverError> {
    // As much of the opening as the probe reads to know the generation.
    let head = regions.archive_bytes(
        0,
        usize::try_from(regions.len()).map_or(512, |len| len.min(512)),
    )?;
    let members = match tairix_fsprobe::archive::rar_version(&head) {
        Some(tairix_fsprobe::archive::RarVersion::Rar4) => rar4::list(regions)?,
        Some(tairix_fsprobe::archive::RarVersion::Rar5) => rar5::list(regions)?,
        None => return Err(DriverError::BadMagic),
    };
    build(regions, members)
}

/// The tree and reader of `members`, in archive order.
fn build<A: VolumeAccess>(regions: &Regions<A>, members: Vec<Member>) -> Result<Read, DriverError> {
    let mut entries = Vec::new();
    for member in &members {
        if let Kind::File(entry) | Kind::LinkData(entry) | Kind::Unlisted(entry) = &member.kind {
            entries
                .try_reserve(1)
                .map_err(|_| DriverError::OutOfMemory)?;
            entries.push(entry.clone());
        }
    }
    let mut reader = Reader::new(entries)?;
    let mut tree = Tree::new(NameMatching::Exact, directory_meta());
    let mut files: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
    let mut next_entry = 0usize;
    for member in members {
        let entry = match member.kind {
            Kind::Unlisted(_) => {
                next_entry += 1;
                continue;
            }
            Kind::File(_) | Kind::LinkData(_) => {
                next_entry += 1;
                Some(next_entry - 1)
            }
            _ => None,
        };
        match admit(&mut tree, &mut reader, regions, &mut files, member, entry) {
            Err(err) if !leaves_out(err) => return Err(err),
            _ => {}
        }
    }
    Ok(Read {
        tree,
        format: Format::Rar(Rar),
        refusal: Some(WriteRefusal::FormatReadOnly),
        stream: None,
        rar: Some(reader),
    })
}

/// Put `member`, whose bytes are entry `entry` where it has any, into
/// `tree`.
fn admit<A: VolumeAccess>(
    tree: &mut Tree,
    reader: &mut Reader,
    regions: &Regions<A>,
    files: &mut BTreeMap<Vec<u8>, usize>,
    member: Member,
    entry: Option<usize>,
) -> Result<(), DriverError> {
    let Spelled::Within(mut parts) = spelled(&member.name, b'/') else {
        return Err(DriverError::OutOfRange);
    };
    let leaf = parts.pop().ok_or(DriverError::OutOfRange)?;
    let meta = Meta {
        mode: member.mode,
        owner: None,
        times: NodeTimes {
            created: Time64::UNIX_EPOCH,
            modified: member.modified,
            accessed: Time64::UNIX_EPOCH,
            changed: Time64::UNIX_EPOCH,
        },
        acorn: None,
        recorded: Recorded::default(),
    };
    let dir = tree.directory(&parts, &directory_meta())?;
    let file = |entry: usize, len: u64| {
        Body::File(Data::Archived(Archived {
            at: 0,
            lead: Lead::None,
            len,
            coding: Coding::Rar {
                entry: u32::try_from(entry).unwrap_or(u32::MAX),
            },
            check: DataCheck::None,
        }))
    };
    let body = match member.kind {
        Kind::Dir => {
            return match tree.lookup(dir, &leaf) {
                Ok(made) if matches!(tree.node(made)?.body, Body::Dir(_)) => {
                    tree.node_mut(made)?.meta = meta;
                    Ok(())
                }
                Ok(_) => Err(DriverError::AlreadyExists),
                Err(_) => tree
                    .add(dir, &leaf, Body::Dir(BTreeMap::new()), meta, None)
                    .map(|_| ()),
            };
        }
        Kind::File(record) => {
            let entry = entry.ok_or(DriverError::DeviceFault)?;
            files.insert(path_key(&parts, &leaf)?, entry);
            file(entry, record.len)
        }
        Kind::Copy(target) => {
            let Spelled::Within(target) = spelled(&target, b'/') else {
                return Err(DriverError::NotFound);
            };
            let entry = *files
                .get(&target.join(&b'/'))
                .ok_or(DriverError::NotFound)?;
            file(entry, reader.entries[entry].len)
        }
        Kind::Link(target) => Body::Link(link(target)?),
        Kind::Unlisted(_) => return Err(DriverError::OutOfRange),
        Kind::LinkData(record) => {
            let entry = entry.ok_or(DriverError::DeviceFault)?;
            let len = usize::try_from(record.len)
                .ok()
                .filter(|&len| len > 0 && len <= FS_SYMLINK_MAX)
                .ok_or(DriverError::OutOfRange)?;
            let mut target =
                tairix_util::fallible::filled(len, 0u8).ok_or(DriverError::OutOfMemory)?;
            let mut got = 0;
            while got < len {
                match reader.read(regions, entry, got as u64, &mut target[got..]) {
                    Ok(0) => return Err(DriverError::OutOfRange),
                    Ok(n) => got += n,
                    Err(DriverError::MediumError | DriverError::PermissionDenied) => {
                        return Err(DriverError::Unsupported)
                    }
                    Err(err) => return Err(err),
                }
            }
            Body::Link(link(target)?)
        }
    };
    tree.add(dir, &leaf, body, meta, None).map(|_| ())
}

/// The path `parts` and `leaf` spell, as the index of files keys it.
fn path_key(parts: &[Vec<u8>], leaf: &[u8]) -> Result<Vec<u8>, DriverError> {
    let mut key = Vec::new();
    key.try_reserve(leaf.len() + parts.iter().map(|part| part.len() + 1).sum::<usize>())
        .map_err(|_| DriverError::OutOfMemory)?;
    for part in parts {
        key.extend_from_slice(part);
        key.push(b'/');
    }
    key.extend_from_slice(leaf);
    Ok(key)
}

/// A link target as served: within a link's length, with no NUL.
fn link(target: Vec<u8>) -> Result<Vec<u8>, DriverError> {
    if target.is_empty() || target.len() > FS_SYMLINK_MAX || target.contains(&0) {
        return Err(DriverError::OutOfRange);
    }
    Ok(target)
}

#[cfg(test)]
#[path = "rar_tests.rs"]
mod tests;
