//! Machine snapshots: the `BEEBSNAP` container, and the codec every part of
//! the machine saves and restores its state through (`plans/BEEB.md` §2).
//!
//! A snapshot is untrusted: every field is checked as it is read, every
//! container is read only up to the bound its owner states, and every time is
//! held to the machine's clock. Anything else refuses the whole snapshot.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// What opens a snapshot.
pub const MAGIC: &[u8; 8] = b"BEEBSNAP";
/// The format this build writes and reads.
pub const VERSION: u16 = 1;
/// The largest body read: a machine with all its sideways RAM, two full
/// double-sided discs and an hour of tape fits comfortably.
pub const MAX_SNAPSHOT: usize = 48 << 20;
/// The latest machine time a snapshot may record: centuries of running, and
/// far enough below `u64::MAX` that no arithmetic on a time within
/// [`HORIZON`] of it overflows.
pub const MAX_CLOCK: u64 = 1 << 56;
/// How far past the machine's time a device may have scheduled anything: a
/// tape's length dwarfs every other wait.
pub const HORIZON: u64 = 1 << 40;

/// The magic and the version. The body's length is its compressed frame's.
const HEADER_LEN: usize = MAGIC.len() + 2;

/// Why a snapshot was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotError {
    /// Not a snapshot.
    Magic,
    /// A snapshot of another version.
    Version,
    /// Larger than any snapshot.
    TooLarge,
    /// Its compressed body does not decode.
    Corrupt,
    /// It ends inside a section or field.
    Truncated,
    /// A section or field holds what the machine cannot.
    Malformed,
    /// It was taken on a machine fitted with other ROMs or sideways RAM.
    Fitting,
}

/// Writes a snapshot.
#[derive(Debug, Default)]
pub struct Writer {
    out: Vec<u8>,
}

impl Writer {
    /// A snapshot begun.
    #[must_use]
    pub const fn new() -> Self {
        Self { out: Vec::new() }
    }

    /// The snapshot written, in its container.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::TooLarge`] for a body past [`MAX_SNAPSHOT`].
    pub fn finish(self) -> Result<Vec<u8>, SnapshotError> {
        if self.out.len() > MAX_SNAPSHOT {
            return Err(SnapshotError::TooLarge);
        }
        let mut sealed = vec![0; HEADER_LEN + tairix_compress::max_compressed_len(self.out.len())];
        sealed[..MAGIC.len()].copy_from_slice(MAGIC);
        sealed[MAGIC.len()..HEADER_LEN].copy_from_slice(&VERSION.to_le_bytes());
        let written = tairix_compress::compress(&self.out, &mut sealed[HEADER_LEN..])
            .map_err(|_| SnapshotError::TooLarge)?;
        sealed.truncate(HEADER_LEN + written);
        Ok(sealed)
    }

    /// Write `bytes` as they are.
    pub fn raw(&mut self, bytes: &[u8]) {
        self.out.extend_from_slice(bytes);
    }

    /// Write a section tagged `tag` holding what `body` writes.
    pub fn section(&mut self, tag: [u8; 4], body: impl FnOnce(&mut Self)) {
        self.raw(&tag);
        let at = self.out.len();
        self.raw(&[0; 4]);
        body(self);
        let len = u32::try_from(self.out.len() - at - 4).unwrap_or(u32::MAX);
        self.out[at..at + 4].copy_from_slice(&len.to_le_bytes());
    }

    /// Write a value.
    pub fn put<T: Save + ?Sized>(&mut self, value: &T) {
        value.save(self);
    }
}

/// The body of the snapshot `bytes`: its container checked, its body
/// decompressed. The body's declared length is held to what its compressed
/// bytes could expand to before anything is allocated for it.
///
/// # Errors
///
/// [`SnapshotError`]: not a snapshot, another version's, too large, or a body
/// that does not decode exactly.
pub fn open(bytes: &[u8]) -> Result<Vec<u8>, SnapshotError> {
    if bytes.len() > HEADER_LEN + tairix_compress::max_compressed_len(MAX_SNAPSHOT) {
        return Err(SnapshotError::TooLarge);
    }
    if !bytes.starts_with(MAGIC) {
        return Err(SnapshotError::Magic);
    }
    let header = bytes.get(..HEADER_LEN).ok_or(SnapshotError::Truncated)?;
    if header[MAGIC.len()..] != VERSION.to_le_bytes() {
        return Err(SnapshotError::Version);
    }
    let frame = &bytes[HEADER_LEN..];
    let len = tairix_compress::decompressed_len(frame).map_err(|_| SnapshotError::Corrupt)?;
    if len > MAX_SNAPSHOT {
        return Err(SnapshotError::TooLarge);
    }
    let mut body = vec![0; len];
    tairix_compress::decompress(frame, &mut body).map_err(|_| SnapshotError::Corrupt)?;
    Ok(body)
}

/// Reads a snapshot's body, checking as it goes.
#[derive(Debug)]
pub struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    /// The machine's time when the snapshot was taken, once it is read.
    clock: u64,
}

impl<'a> Reader<'a> {
    /// Read `body`, as [`open`] gives it.
    #[must_use]
    pub const fn new(body: &'a [u8]) -> Self {
        Self {
            bytes: body,
            at: 0,
            clock: 0,
        }
    }

    /// Take `len` bytes.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::Truncated`] past the end.
    pub fn raw(&mut self, len: usize) -> Result<&'a [u8], SnapshotError> {
        let end = self.at.checked_add(len).ok_or(SnapshotError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.at..end)
            .ok_or(SnapshotError::Truncated)?;
        self.at = end;
        Ok(bytes)
    }

    /// Bytes not yet read.
    #[must_use]
    pub const fn left(&self) -> usize {
        self.bytes.len() - self.at
    }

    /// Read the section `tag` with `body`, which must consume it exactly.
    ///
    /// # Errors
    ///
    /// Another tag, a length past the end, what `body` refuses, or bytes it
    /// left unread.
    pub fn section<T>(
        &mut self,
        tag: [u8; 4],
        body: impl FnOnce(&mut Reader<'a>) -> Result<T, SnapshotError>,
    ) -> Result<T, SnapshotError> {
        if self.raw(4)? != tag {
            return Err(SnapshotError::Malformed);
        }
        let len = usize::try_from(self.get::<u32>()?).map_err(|_| SnapshotError::Truncated)?;
        let mut inner = Reader {
            bytes: self.raw(len)?,
            at: 0,
            clock: self.clock,
        };
        let value = body(&mut inner)?;
        Self::check(inner.left() == 0)?;
        Ok(value)
    }

    /// Read a value.
    ///
    /// # Errors
    ///
    /// What the value's [`Load`] refuses.
    pub fn get<T: Load>(&mut self) -> Result<T, SnapshotError> {
        T::load(self)
    }

    /// Read a container holding no more than `max` things.
    ///
    /// # Errors
    ///
    /// A count past `max` or the bytes left, or what an element refuses.
    pub fn get_at_most<T: LoadAtMost>(&mut self, max: usize) -> Result<T, SnapshotError> {
        T::load_at_most(self, max)
    }

    /// Read a count of no more than `max` things, each at least a byte long,
    /// before anything is allocated for them.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::Malformed`] past `max`, [`SnapshotError::Truncated`]
    /// past the bytes left.
    pub fn count_at_most(&mut self, max: usize) -> Result<usize, SnapshotError> {
        let count = usize::try_from(self.get::<u32>()?).map_err(|_| SnapshotError::Truncated)?;
        Self::check(count <= max)?;
        if count > self.left() {
            return Err(SnapshotError::Truncated);
        }
        Ok(count)
    }

    /// Read a run of no more than `max` bytes [`Writer::bytes`] wrote.
    ///
    /// # Errors
    ///
    /// A run past `max` or the bytes left.
    pub fn bytes_at_most(&mut self, max: usize) -> Result<&'a [u8], SnapshotError> {
        let len = self.count_at_most(max)?;
        self.raw(len)
    }

    /// Take the machine's time, once read, as the one every later time is
    /// held to.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::Malformed`] past [`MAX_CLOCK`].
    pub fn set_clock(&mut self, clock: u64) -> Result<(), SnapshotError> {
        Self::check(clock <= MAX_CLOCK)?;
        self.clock = clock;
        Ok(())
    }

    /// The machine's time when the snapshot was taken.
    #[must_use]
    pub const fn clock(&self) -> u64 {
        self.clock
    }

    /// The latest time a device may have anything scheduled for.
    #[must_use]
    pub const fn horizon(&self) -> u64 {
        self.clock + HORIZON
    }

    /// Whether the machine had reached `at`.
    #[must_use]
    pub const fn reached(&self, at: u64) -> bool {
        at <= self.clock
    }

    /// Whether `at` is still to come: from the machine's time to the
    /// [`horizon`](Self::horizon). Every event a device has scheduled is, as
    /// a snapshot leaves it.
    #[must_use]
    pub const fn pending(&self, at: u64) -> bool {
        self.clock <= at && at <= self.horizon()
    }

    /// Fail unless `holds`.
    ///
    /// # Errors
    ///
    /// [`SnapshotError::Malformed`] when it does not.
    pub const fn check(holds: bool) -> Result<(), SnapshotError> {
        if holds {
            Ok(())
        } else {
            Err(SnapshotError::Malformed)
        }
    }
}

/// State a snapshot keeps.
pub trait Save {
    /// Write it.
    fn save(&self, w: &mut Writer);
}

/// State a snapshot restores, checked as it is read.
pub trait Load: Sized {
    /// Read it.
    fn load(r: &mut Reader<'_>) -> Result<Self, SnapshotError>;
}

macro_rules! integers {
    ($($t:ty),*) => {$(
        impl Save for $t {
            fn save(&self, w: &mut Writer) {
                w.raw(&self.to_le_bytes());
            }
        }
        impl Load for $t {
            fn load(r: &mut Reader<'_>) -> Result<Self, SnapshotError> {
                let bytes = r.raw(core::mem::size_of::<Self>())?;
                let mut array = [0; core::mem::size_of::<Self>()];
                array.copy_from_slice(bytes);
                Ok(Self::from_le_bytes(array))
            }
        }
    )*};
}

integers!(u8, u16, u32, u64, i32, i64);

impl Save for usize {
    fn save(&self, w: &mut Writer) {
        w.put(&u64::try_from(*self).unwrap_or(u64::MAX));
    }
}

impl Load for usize {
    fn load(r: &mut Reader<'_>) -> Result<Self, SnapshotError> {
        Self::try_from(r.get::<u64>()?).map_err(|_| SnapshotError::Malformed)
    }
}

impl Save for bool {
    fn save(&self, w: &mut Writer) {
        w.put(&u8::from(*self));
    }
}

impl Load for bool {
    fn load(r: &mut Reader<'_>) -> Result<Self, SnapshotError> {
        match r.get::<u8>()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SnapshotError::Malformed),
        }
    }
}

impl<T: Save> Save for Option<T> {
    fn save(&self, w: &mut Writer) {
        match self {
            None => w.put(&false),
            Some(value) => {
                w.put(&true);
                w.put(value);
            }
        }
    }
}

impl<T: Load> Load for Option<T> {
    fn load(r: &mut Reader<'_>) -> Result<Self, SnapshotError> {
        if r.get::<bool>()? {
            Ok(Some(r.get()?))
        } else {
            Ok(None)
        }
    }
}

impl<T: Save, const N: usize> Save for [T; N] {
    fn save(&self, w: &mut Writer) {
        for value in self {
            w.put(value);
        }
    }
}

impl<T: Load, const N: usize> Load for [T; N] {
    fn load(r: &mut Reader<'_>) -> Result<Self, SnapshotError> {
        let mut values = Vec::with_capacity(N);
        for _ in 0..N {
            values.push(r.get()?);
        }
        values.try_into().map_err(|_| SnapshotError::Malformed)
    }
}

impl<T: Save> Save for [T] {
    fn save(&self, w: &mut Writer) {
        w.put(&u32::try_from(self.len()).unwrap_or(u32::MAX));
        for value in self {
            w.put(value);
        }
    }
}

impl<T: Save> Save for Vec<T> {
    fn save(&self, w: &mut Writer) {
        self.as_slice().save(w);
    }
}

/// A container a snapshot restores, read only up to the bound its owner
/// states: a count is never trusted to size an allocation on its own, since
/// nested containers cost more memory than the bytes that describe them.
pub trait LoadAtMost: Sized {
    /// Read it, holding no more than `max` things.
    ///
    /// # Errors
    ///
    /// A count past `max` or the bytes left, or what an element refuses.
    fn load_at_most(r: &mut Reader<'_>, max: usize) -> Result<Self, SnapshotError>;
}

impl<T: Load> LoadAtMost for Vec<T> {
    fn load_at_most(r: &mut Reader<'_>, max: usize) -> Result<Self, SnapshotError> {
        let count = r.count_at_most(max)?;
        let mut values = Self::with_capacity(count);
        for _ in 0..count {
            values.push(r.get()?);
        }
        Ok(values)
    }
}

impl<T: Save> Save for Box<[T]> {
    fn save(&self, w: &mut Writer) {
        (**self).save(w);
    }
}

impl<T: Load> LoadAtMost for Box<[T]> {
    fn load_at_most(r: &mut Reader<'_>, max: usize) -> Result<Self, SnapshotError> {
        Ok(r.get_at_most::<Vec<T>>(max)?.into_boxed_slice())
    }
}

impl<A: Save, B: Save> Save for (A, B) {
    fn save(&self, w: &mut Writer) {
        w.put(&self.0);
        w.put(&self.1);
    }
}

impl<A: Load, B: Load> Load for (A, B) {
    fn load(r: &mut Reader<'_>) -> Result<Self, SnapshotError> {
        Ok((r.get()?, r.get()?))
    }
}

impl Save for String {
    fn save(&self, w: &mut Writer) {
        self.as_bytes().save(w);
    }
}

impl LoadAtMost for String {
    /// No more than `max` bytes of UTF-8.
    fn load_at_most(r: &mut Reader<'_>, max: usize) -> Result<Self, SnapshotError> {
        let text = core::str::from_utf8(r.bytes_at_most(max)?);
        Ok(text.map_err(|_| SnapshotError::Malformed)?.into())
    }
}

/// An unsigned number in as few bytes as it needs: seven bits a byte, least
/// significant first, the top bit set on every byte but the last. Only the
/// shortest encoding is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Varint(pub u64);

impl Save for Varint {
    fn save(&self, w: &mut Writer) {
        let mut value = self.0;
        loop {
            let low = value.to_le_bytes()[0] & 0x7F;
            value >>= 7;
            if value == 0 {
                w.raw(&[low]);
                return;
            }
            w.raw(&[low | 0x80]);
        }
    }
}

impl Load for Varint {
    fn load(r: &mut Reader<'_>) -> Result<Self, SnapshotError> {
        let mut value = 0u64;
        for shift in (0..u64::BITS).step_by(7) {
            let byte = r.get::<u8>()?;
            let bits = u64::from(byte & 0x7F);
            Reader::check(bits << shift >> shift == bits)?;
            value |= bits << shift;
            if byte & 0x80 == 0 {
                Reader::check(byte != 0 || shift == 0)?;
                return Ok(Self(value));
            }
        }
        Err(SnapshotError::Malformed)
    }
}

impl Writer {
    /// Write a run of bytes, its length first.
    pub fn bytes(&mut self, bytes: &[u8]) {
        self.put(&u32::try_from(bytes.len()).unwrap_or(u32::MAX));
        self.raw(bytes);
    }
}

/// Save and load a struct field by field, in order, then hold it to
/// `check`, a condition every restored value must meet, given the value and
/// the reader, whose clock its times are held to. A container field names
/// its bound (`field <= MAX`); a `transient` field is not kept, and restores
/// empty.
macro_rules! fields {
    (@load $r:ident) => {
        $r.get()?
    };
    (@load $r:ident, $max:tt) => {
        $r.get_at_most($max)?
    };
    (
        $type:ty { $($field:ident $(<= $max:tt)?),* $(,)? }
        $(transient { $($transient:ident),* $(,)? })?
        $(, check |$value:ident $(, $reader:ident)?| $check:expr)?
    ) => {
        impl $crate::snapshot::Save for $type {
            fn save(&self, w: &mut $crate::snapshot::Writer) {
                $(w.put(&self.$field);)*
            }
        }
        impl $crate::snapshot::Load for $type {
            fn load(r: &mut $crate::snapshot::Reader<'_>) -> Result<Self, $crate::snapshot::SnapshotError> {
                let value = Self {
                    $($field: $crate::snapshot::fields!(@load r $(, $max)?),)*
                    $($($transient: Default::default(),)*)?
                };
                $({
                    let $value = &value;
                    $(let $reader: &$crate::snapshot::Reader<'_> = r;)?
                    $crate::snapshot::Reader::check($check)?;
                })?
                Ok(value)
            }
        }
    };
}

pub(crate) use fields;

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    struct Sample {
        small: u8,
        flag: bool,
        maybe: Option<u16>,
        pair: [i32; 2],
        list: Vec<u64>,
        due: u64,
    }

    const LIST: usize = 3;

    fields!(
        Sample {
            small,
            flag,
            maybe,
            pair,
            list <= LIST,
            due
        },
        check | s,
        r | s.small < 200 && r.pending(s.due)
    );

    fn sample() -> Sample {
        Sample {
            small: 7,
            flag: true,
            maybe: Some(0xBEEB),
            pair: [-1, 2],
            list: vec![1, 2, 3],
            due: HORIZON,
        }
    }

    fn body(value: &Sample) -> Vec<u8> {
        let mut w = Writer::new();
        w.section(*b"TEST", |w| w.put(value));
        w.out
    }

    fn read(body: &[u8]) -> Result<Sample, SnapshotError> {
        Reader::new(body).section(*b"TEST", Reader::get::<Sample>)
    }

    #[test]
    fn a_section_round_trips_and_must_be_read_exactly() {
        let body = body(&sample());
        assert_eq!(read(&body), Ok(sample()));
        assert_eq!(
            Reader::new(&body).section(*b"TEST", Reader::get::<u8>),
            Err(SnapshotError::Malformed),
            "left over"
        );
        assert_eq!(
            Reader::new(&body).section(*b"NOPE", Reader::get::<Sample>),
            Err(SnapshotError::Malformed)
        );
    }

    #[test]
    fn a_field_out_of_range_or_a_time_past_the_horizon_is_refused() {
        let good = body(&sample());
        assert_eq!(read(&good[..good.len() - 1]), Err(SnapshotError::Truncated));
        // The tag and length, then `small` and `flag`.
        let mut flag = good.clone();
        flag[9] = 2;
        assert_eq!(read(&flag), Err(SnapshotError::Malformed));
        let mut big = good;
        big[8] = 250;
        assert_eq!(read(&big), Err(SnapshotError::Malformed), "the check");
        let late = body(&Sample {
            due: HORIZON + 1,
            ..sample()
        });
        assert_eq!(read(&late), Err(SnapshotError::Malformed));
        let mut later = Reader::new(&late);
        later.set_clock(1).unwrap();
        assert!(
            later.section(*b"TEST", Reader::get::<Sample>).is_ok(),
            "held to the clock"
        );
        assert_eq!(
            Reader::new(&[]).set_clock(MAX_CLOCK + 1),
            Err(SnapshotError::Malformed)
        );
    }

    /// The regression: a scheduled time was bounded only from above, so an
    /// event restored in the past replayed from there.
    #[test]
    fn a_scheduled_time_before_the_clock_is_refused() {
        let past = body(&Sample { due: 9, ..sample() });
        let mut r = Reader::new(&past);
        r.set_clock(10).unwrap();
        assert_eq!(
            r.section(*b"TEST", Reader::get::<Sample>),
            Err(SnapshotError::Malformed)
        );
        let mut r = Reader::new(&past);
        r.set_clock(9).unwrap();
        assert!(r.section(*b"TEST", Reader::get::<Sample>).is_ok());
    }

    #[test]
    fn the_container_round_trips_its_body_and_refuses_damage() {
        let mut w = Writer::new();
        w.section(*b"TEST", |w| w.put(&sample()));
        w.bytes(&[0x55; 4096]);
        let sealed = w.finish().unwrap();
        assert!(sealed.len() < 1024, "the body compresses");
        let opened = open(&sealed).unwrap();
        let mut r = Reader::new(&opened);
        assert_eq!(r.section(*b"TEST", Reader::get::<Sample>), Ok(sample()));
        assert_eq!(r.bytes_at_most(4096), Ok(&[0x55; 4096][..]));
        assert_eq!(r.left(), 0);

        assert_eq!(open(b"NOTASNAP\x01\x00"), Err(SnapshotError::Magic));
        assert_eq!(
            open(&sealed[..HEADER_LEN - 1]),
            Err(SnapshotError::Truncated)
        );
        let mut other = sealed.clone();
        other[8] = 2;
        assert_eq!(open(&other), Err(SnapshotError::Version));
        assert_eq!(
            open(&sealed[..sealed.len() - 2]),
            Err(SnapshotError::Corrupt),
            "cut short"
        );
        let mut trailing = sealed.clone();
        trailing.push(0);
        assert_eq!(
            open(&trailing),
            Err(SnapshotError::Corrupt),
            "a byte past the body"
        );
    }

    /// The regression: the body was allocated at the length its header gave
    /// before any compressed byte was looked at.
    #[test]
    fn a_body_longer_than_its_bytes_could_expand_to_is_refused_unallocated() {
        let sealed = Writer::new().finish().unwrap();
        let mut forged = sealed.clone();
        let frame = HEADER_LEN + 4;
        forged[frame..frame + 4]
            .copy_from_slice(&u32::try_from(MAX_SNAPSHOT).unwrap().to_le_bytes());
        assert_eq!(open(&forged), Err(SnapshotError::Corrupt));
    }

    /// The regression: a count was trusted up to the bytes left, so a list of
    /// lists cost many times its encoding in memory.
    #[test]
    fn a_count_beyond_its_bound_or_the_bytes_left_is_refused_before_allocating() {
        let mut w = Writer::new();
        w.put(&u32::MAX);
        assert_eq!(
            Reader::new(&w.out).get_at_most::<Vec<u64>>(usize::MAX),
            Err(SnapshotError::Truncated)
        );
        assert_eq!(
            Reader::new(&w.out).bytes_at_most(usize::MAX),
            Err(SnapshotError::Truncated)
        );
        let mut w = Writer::new();
        w.put(&vec![1u64, 2, 3, 4]);
        assert_eq!(
            Reader::new(&w.out).get_at_most::<Vec<u64>>(LIST),
            Err(SnapshotError::Malformed)
        );
        assert_eq!(
            Reader::new(&w.out).get_at_most::<Vec<u64>>(4),
            Ok(vec![1, 2, 3, 4])
        );
    }

    #[test]
    fn a_varint_takes_the_fewest_bytes_and_only_its_shortest_encoding_reads() {
        for (value, len) in [
            (0, 1),
            (127, 1),
            (128, 2),
            (16_383, 2),
            (16_384, 3),
            (u64::MAX, 10),
        ] {
            let mut w = Writer::new();
            w.put(&Varint(value));
            assert_eq!(w.out.len(), len, "{value}");
            assert_eq!(Reader::new(&w.out).get::<Varint>(), Ok(Varint(value)));
        }
        let refused: [&[u8]; 4] = [
            &[0x80, 0x00],
            &[0xFF; 10],
            &[0x80; 11],
            &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x02],
        ];
        for bytes in refused {
            assert_eq!(
                Reader::new(bytes).get::<Varint>(),
                Err(SnapshotError::Malformed),
                "{bytes:?}"
            );
        }
        assert_eq!(
            Reader::new(&[0x80]).get::<Varint>(),
            Err(SnapshotError::Truncated)
        );
    }
}
