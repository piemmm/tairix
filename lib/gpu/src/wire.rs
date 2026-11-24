//! The seam's descriptions on the `gpu-v1` wire: what a client sends a
//! served device and what it answers, each a little-endian encoding a
//! [`Reader`] takes back whole or refuses as [`Error::Malformed`]. Decoding is
//! total: no byte string panics, and none decodes to a value a client could
//! not have encoded.

use alloc::vec::Vec;

use tairix_abi::gpu::GpuError;
use tairix_abi::Duration64;
use tairix_collections::{Key, SlotKey};
use tairix_spirv::{Disagreement, SpecError};

use crate::{Error, Loss, SubmitError};

mod commands;
mod desc;
pub(crate) mod ops;

pub(crate) use commands::{put_command, put_first_draws, put_more_draws, take_commands, Arrival};
pub(crate) use desc::{put_info, take_info};

/// Bytes a value is written into: fallible, so an encoding past what can be
/// had is refused rather than aborting.
pub(crate) struct Writer<'a> {
    out: &'a mut Vec<u8>,
}

impl<'a> Writer<'a> {
    pub(crate) fn new(out: &'a mut Vec<u8>) -> Self {
        Self { out }
    }

    pub(crate) fn bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.out
            .try_reserve(bytes.len())
            .map_err(|_| Error::OutOfMemory)?;
        self.out.extend_from_slice(bytes);
        Ok(())
    }

    /// `bytes`, after their count, which a reader bounds.
    pub(crate) fn blob(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let count = u32::try_from(bytes.len()).map_err(|_| Error::TooMany)?;
        count.put(self)?;
        self.bytes(bytes)
    }

    /// `items`, each `put`, after their count, which a reader bounds.
    pub(crate) fn list<T>(
        &mut self,
        items: &[T],
        put: impl Fn(&mut Self, &T) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let count = u32::try_from(items.len()).map_err(|_| Error::TooMany)?;
        count.put(self)?;
        items.iter().try_for_each(|item| put(self, item))
    }
}

/// Bytes a value is read from, refused whole where they run short or name
/// what no encoding writes.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub(crate) fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let (taken, rest) = self
            .bytes
            .split_first_chunk::<N>()
            .ok_or(Error::Malformed)?;
        self.bytes = rest;
        Ok(*taken)
    }

    pub(crate) fn slice(&mut self, len: usize) -> Result<&'a [u8], Error> {
        let (taken, rest) = self.bytes.split_at_checked(len).ok_or(Error::Malformed)?;
        self.bytes = rest;
        Ok(taken)
    }

    /// A count written by [`Writer::list`], refused past `most`.
    pub(crate) fn count(&mut self, most: usize) -> Result<usize, Error> {
        let count = usize::try_from(u32::take(self)?).map_err(|_| Error::Malformed)?;
        if count > most {
            return Err(Error::Malformed);
        }
        Ok(count)
    }

    /// Bytes written by [`Writer::blob`], at most `most` of them.
    pub(crate) fn blob(&mut self, most: usize) -> Result<&'a [u8], Error> {
        let count = self.count(most)?;
        self.slice(count)
    }

    /// A list of at most `most` items, each `take`n.
    pub(crate) fn list<T>(
        &mut self,
        most: usize,
        take: impl Fn(&mut Self) -> Result<T, Error>,
    ) -> Result<Vec<T>, Error> {
        let count = self.count(most)?;
        // Each item takes at least a byte, so a count past what is left
        // cannot be met: refused before room is made for it.
        if count > self.bytes.len() {
            return Err(Error::Malformed);
        }
        let mut items = Vec::new();
        items
            .try_reserve_exact(count)
            .map_err(|_| Error::OutOfMemory)?;
        for _ in 0..count {
            items.push(take(self)?);
        }
        Ok(items)
    }

    /// A reader of what is left, which reading leaves this one where it was.
    pub(crate) const fn peek(&self) -> Self {
        Self { bytes: self.bytes }
    }

    /// Refused unless every byte was read.
    pub(crate) fn finish(self) -> Result<(), Error> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(Error::Malformed)
        }
    }
}

/// A value on the wire.
pub(crate) trait Wire: Sized {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error>;
    fn take(reader: &mut Reader<'_>) -> Result<Self, Error>;
}

macro_rules! wire_int {
    ($($int:ty),*) => {$(
        impl Wire for $int {
            fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
                writer.bytes(&self.to_le_bytes())
            }

            fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
                Ok(Self::from_le_bytes(reader.take()?))
            }
        }
    )*};
}

wire_int!(u8, u16, u32, u64, i32);

impl Wire for bool {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        u8::from(*self).put(writer)
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::take(reader)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::Malformed),
        }
    }
}

// A float crosses as its bits: what the seam checks of it — finiteness, its
// range — it checks where it arrives.
impl Wire for f32 {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        self.to_bits().put(writer)
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        u32::take(reader).map(Self::from_bits)
    }
}

impl Wire for f64 {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        self.to_bits().put(writer)
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        u64::take(reader).map(Self::from_bits)
    }
}

impl<T: Wire + Copy + Default, const N: usize> Wire for [T; N] {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        self.iter().try_for_each(|item| item.put(writer))
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        let mut items = [T::default(); N];
        for item in &mut items {
            *item = T::take(reader)?;
        }
        Ok(items)
    }
}

impl<T: Wire> Wire for Option<T> {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        match self {
            None => false.put(writer),
            Some(value) => {
                true.put(writer)?;
                value.put(writer)
            }
        }
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        if bool::take(reader)? {
            T::take(reader).map(Some)
        } else {
            Ok(None)
        }
    }
}

impl Wire for Duration64 {
    fn put(&self, writer: &mut Writer<'_>) -> Result<(), Error> {
        writer.bytes(&self.to_le_bytes())
    }

    fn take(reader: &mut Reader<'_>) -> Result<Self, Error> {
        Self::from_bytes(&reader.take::<{ Duration64::WIRE_LEN }>()?).map_err(|_| Error::Malformed)
    }
}

/// A name — an entry point's — as its UTF-8 bytes.
pub(crate) fn put_name(writer: &mut Writer<'_>, name: &str) -> Result<(), Error> {
    writer.blob(name.as_bytes())
}

/// A name of at most the longest string a module holds.
pub(crate) fn take_name(reader: &mut Reader<'_>) -> Result<alloc::string::String, Error> {
    let bytes = reader.blob(tairix_spirv::MAX_STRING)?;
    let name = core::str::from_utf8(bytes).map_err(|_| Error::Malformed)?;
    let mut owned = alloc::string::String::new();
    owned
        .try_reserve_exact(name.len())
        .map_err(|_| Error::OutOfMemory)?;
    owned.push_str(name);
    Ok(owned)
}

/// A model key as one word: an identifier the model's tables check, never an
/// authority.
pub(crate) fn put_key<K: Key>(writer: &mut Writer<'_>, key: K) -> Result<(), Error> {
    key.slot().to_bits().put(writer)
}

pub(crate) fn take_key<K: Key>(reader: &mut Reader<'_>) -> Result<K, Error> {
    let bits = u64::take(reader)?;
    SlotKey::from_bits(bits)
        .map(K::from_slot)
        .ok_or(Error::Malformed)
}

/// An enum whose variants carry nothing, each its index: written by an
/// exhaustive match, so a variant added without its index fails to build.
macro_rules! wire_enum {
    ($ty:ty { $($variant:ident = $index:literal),* $(,)? }) => {
        impl crate::wire::Wire for $ty {
            fn put(&self, writer: &mut crate::wire::Writer<'_>) -> Result<(), crate::Error> {
                let index: u8 = match self {
                    $(Self::$variant => $index,)*
                };
                index.put(writer)
            }

            fn take(reader: &mut crate::wire::Reader<'_>) -> Result<Self, crate::Error> {
                match u8::take(reader)? {
                    $($index => Ok(Self::$variant),)*
                    _ => Err(crate::Error::Malformed),
                }
            }
        }
    };
}

pub(crate) use wire_enum;

/// Flags whose every set bit is one the type names.
/// A set of flags, its bits as they are: what bits a use admits is the
/// model's to judge, as it judges a caller in its own process.
macro_rules! wire_flags {
    ($ty:ty, $bits:ty) => {
        impl crate::wire::Wire for $ty {
            fn put(&self, writer: &mut crate::wire::Writer<'_>) -> Result<(), crate::Error> {
                self.bits().put(writer)
            }

            fn take(reader: &mut crate::wire::Reader<'_>) -> Result<Self, crate::Error> {
                Ok(<$ty>::from_bits(<$bits>::take(reader)?))
            }
        }
    };
}

pub(crate) use wire_flags;

wire_enum!(Loss {
    Overran = 0,
    Reset = 1,
    Fault = 2,
    OutOfMemory = 3,
});

/// The code a refusal crosses as, and the detail its reply's body carries.
pub(crate) fn put_error(writer: &mut Writer<'_>, error: Error) -> Result<GpuError, Error> {
    let code = match error {
        Error::Lost(loss) => {
            loss.put(writer)?;
            GpuError::Lost
        }
        Error::Disagreement(disagreement) => {
            let (kind, at): (u8, (u32, u32)) = match disagreement {
                Disagreement::Unordered => (0, (0, 0)),
                Disagreement::Missing(set, binding) => (1, (set, binding)),
                Disagreement::Kind(set, binding) => (2, (set, binding)),
                Disagreement::Count(set, binding) => (3, (set, binding)),
                Disagreement::Access(set, binding) => (4, (set, binding)),
                Disagreement::Stage(set, binding) => (5, (set, binding)),
                Disagreement::PushConstants => (6, (0, 0)),
            };
            kind.put(writer)?;
            at.0.put(writer)?;
            at.1.put(writer)?;
            GpuError::Disagreement
        }
        Error::Specialization(refused) => {
            let (kind, id): (u8, u32) = match refused {
                SpecError::UnknownId(id) => (0, id),
                SpecError::Duplicate(id) => (1, id),
                SpecError::Width(id) => (2, id),
                SpecError::Workgroup => (3, 0),
                SpecError::Budget => (4, 0),
                SpecError::OutOfMemory => (5, 0),
                SpecError::Malformed => (6, 0),
            };
            kind.put(writer)?;
            id.put(writer)?;
            GpuError::Specialization
        }
        Error::Sampling(image, sampler) => {
            for word in [image.0, image.1, sampler.0, sampler.1] {
                word.put(writer)?;
            }
            GpuError::Sampling
        }
        plain => plain_code(plain),
    };
    Ok(code)
}

/// The refusal `code` and its detail name.
pub(crate) fn take_error(code: GpuError, reader: &mut Reader<'_>) -> Result<Error, Error> {
    let error = match code {
        GpuError::Lost => Error::Lost(Loss::take(reader)?),
        GpuError::Disagreement => {
            let kind = u8::take(reader)?;
            let at = (u32::take(reader)?, u32::take(reader)?);
            Error::Disagreement(match kind {
                0 => Disagreement::Unordered,
                1 => Disagreement::Missing(at.0, at.1),
                2 => Disagreement::Kind(at.0, at.1),
                3 => Disagreement::Count(at.0, at.1),
                4 => Disagreement::Access(at.0, at.1),
                5 => Disagreement::Stage(at.0, at.1),
                6 => Disagreement::PushConstants,
                _ => return Err(Error::Malformed),
            })
        }
        GpuError::Specialization => {
            let kind = u8::take(reader)?;
            let id = u32::take(reader)?;
            Error::Specialization(match kind {
                0 => SpecError::UnknownId(id),
                1 => SpecError::Duplicate(id),
                2 => SpecError::Width(id),
                3 => SpecError::Workgroup,
                4 => SpecError::Budget,
                5 => SpecError::OutOfMemory,
                6 => SpecError::Malformed,
                _ => return Err(Error::Malformed),
            })
        }
        GpuError::Sampling => {
            let image = (u32::take(reader)?, u32::take(reader)?);
            let sampler = (u32::take(reader)?, u32::take(reader)?);
            Error::Sampling(image, sampler)
        }
        plain => of_plain_code(plain).ok_or(Error::Malformed)?,
    };
    Ok(error)
}

/// The code of a refusal carrying no detail.
fn plain_code(error: Error) -> GpuError {
    match error {
        Error::Destroyed => GpuError::Destroyed,
        Error::Foreign => GpuError::Foreign,
        Error::Stale => GpuError::Stale,
        Error::OutOfMemory => GpuError::OutOfMemory,
        Error::OverLimit => GpuError::OverLimit,
        Error::HeapFull => GpuError::HeapFull,
        Error::NoSuchMemoryType => GpuError::NoSuchMemoryType,
        Error::NoSuchQueue => GpuError::NoSuchQueue,
        Error::TooLarge => GpuError::TooLarge,
        Error::Empty => GpuError::Empty,
        Error::Misaligned => GpuError::Misaligned,
        Error::OutOfRange => GpuError::OutOfRange,
        Error::Usage => GpuError::Usage,
        Error::NotHostVisible => GpuError::NotHostVisible,
        Error::Overlap => GpuError::Overlap,
        Error::TimelineOrder => GpuError::TimelineOrder,
        Error::Deadline => GpuError::Deadline,
        Error::TooMany => GpuError::TooMany,
        Error::TimedOut => GpuError::TimedOut,
        Error::Malformed => GpuError::Malformed,
        Error::Shape => GpuError::Shape,
        Error::Samples => GpuError::Samples,
        Error::Aspect => GpuError::Aspect,
        Error::Unsupported => GpuError::Unsupported,
        Error::Bindings => GpuError::Bindings,
        Error::EntryPoint => GpuError::EntryPoint,
        Error::Interface => GpuError::Interface,
        Error::Lost(_) => GpuError::Lost,
        Error::Disagreement(_) => GpuError::Disagreement,
        Error::Specialization(_) => GpuError::Specialization,
        Error::Sampling(..) => GpuError::Sampling,
    }
}

/// The refusal a code carrying no detail names.
fn of_plain_code(code: GpuError) -> Option<Error> {
    Some(match code {
        GpuError::Destroyed => Error::Destroyed,
        GpuError::Foreign => Error::Foreign,
        GpuError::Stale => Error::Stale,
        GpuError::OutOfMemory => Error::OutOfMemory,
        GpuError::OverLimit => Error::OverLimit,
        GpuError::HeapFull => Error::HeapFull,
        GpuError::NoSuchMemoryType => Error::NoSuchMemoryType,
        GpuError::NoSuchQueue => Error::NoSuchQueue,
        GpuError::TooLarge => Error::TooLarge,
        GpuError::Empty => Error::Empty,
        GpuError::Misaligned => Error::Misaligned,
        GpuError::OutOfRange => Error::OutOfRange,
        GpuError::Usage => Error::Usage,
        GpuError::NotHostVisible => Error::NotHostVisible,
        GpuError::Overlap => Error::Overlap,
        GpuError::TimelineOrder => Error::TimelineOrder,
        GpuError::Deadline => Error::Deadline,
        GpuError::TooMany => Error::TooMany,
        GpuError::TimedOut => Error::TimedOut,
        GpuError::Malformed => Error::Malformed,
        GpuError::Shape => Error::Shape,
        GpuError::Samples => Error::Samples,
        GpuError::Aspect => Error::Aspect,
        GpuError::Unsupported => Error::Unsupported,
        GpuError::Bindings => Error::Bindings,
        GpuError::EntryPoint => Error::EntryPoint,
        GpuError::Interface => Error::Interface,
        GpuError::Lost | GpuError::Disagreement | GpuError::Specialization | GpuError::Sampling => {
            return None
        }
    })
}

/// A submission's refusal: the command it names, where it names one, then
/// the error.
pub(crate) fn put_submit_error(
    writer: &mut Writer<'_>,
    refused: SubmitError,
) -> Result<GpuError, Error> {
    let command = refused
        .command
        .map_or(u32::MAX, |at| u32::try_from(at).unwrap_or(u32::MAX - 1));
    command.put(writer)?;
    put_error(writer, refused.error)
}

pub(crate) fn take_submit_error(
    code: GpuError,
    reader: &mut Reader<'_>,
) -> Result<SubmitError, Error> {
    let command = u32::take(reader)?;
    let error = take_error(code, reader)?;
    let command = (command != u32::MAX)
        .then(|| usize::try_from(command))
        .transpose()
        .map_err(|_| Error::Malformed)?;
    Ok(SubmitError { command, error })
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
