//! Residues (Vorbis I, section 8): each channel's fine spectral detail,
//! classified a partition at a time and coded as the sum of up to eight
//! passes of vector-quantised values.

use alloc::vec::Vec;

use super::bits::{Bits, EndOfPacket};
use super::codebook::Codebook;
use crate::DecodeError;

/// The passes a residue's cascade can state.
const PASSES: usize = 8;

/// A residue's setup.
pub(super) struct Residue {
    kind: Kind,
    begin: usize,
    end: usize,
    partition_size: usize,
    classifications: u32,
    classbook: u8,
    /// Classwords the classbook can state: `classifications` to the power of
    /// its dimensions, which its entries are at least.
    classwords: u32,
    /// For each classification, the book each pass codes it with.
    books: Vec<[Option<u8>; PASSES]>,
    /// Passes up to the last any classification codes in; libvorbis reads
    /// no classwords for a residue coding none.
    passes: usize,
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum Kind {
    /// Each partition's values interleaved by the book's dimension.
    Interleaved,
    /// Each partition's values in order.
    Ordered,
    /// The bundle's vectors interleaved into one, then coded in order.
    Flattened,
}

pub(super) fn read(bits: &mut Bits<'_>, books: &[Codebook]) -> Result<Residue, DecodeError> {
    let bad = |_| DecodeError::VorbisBadSetup;
    let kind = match bits.read(16).map_err(bad)? {
        0 => Kind::Interleaved,
        1 => Kind::Ordered,
        2 => Kind::Flattened,
        _ => return Err(DecodeError::VorbisBadSetup),
    };
    let begin = bits.read(24).map_err(bad)? as usize;
    let end = bits.read(24).map_err(bad)? as usize;
    let partition_size = bits.read(24).map_err(bad)? as usize + 1;
    let classifications = bits.read(6).map_err(bad)? + 1;
    let classbook =
        u8::try_from(bits.read(8).map_err(bad)?).map_err(|_| DecodeError::VorbisBadSetup)?;
    let mut cascades = [0u32; 64];
    let count = classifications as usize;
    for cascade in &mut cascades[..count] {
        let low = bits.read(3).map_err(bad)?;
        let high = if bits.flag().map_err(bad)? {
            bits.read(5).map_err(bad)?
        } else {
            0
        };
        *cascade = high << 3 | low;
    }
    let mut list = Vec::new();
    if list.try_reserve_exact(count).is_err() {
        return Err(DecodeError::OutOfMemory);
    }
    let mut passes = 0;
    for &cascade in &cascades[..count] {
        let mut row = [None; PASSES];
        for (pass, slot) in row.iter_mut().enumerate() {
            if cascade >> pass & 1 == 0 {
                continue;
            }
            let number = bits.read(8).map_err(bad)? as usize;
            if !books.get(number).is_some_and(Codebook::has_values) {
                return Err(DecodeError::VorbisBadSetup);
            }
            *slot = Some(u8::try_from(number).map_err(|_| DecodeError::VorbisBadSetup)?);
            passes = passes.max(pass + 1);
        }
        list.push(row);
    }
    let phrasebook = books
        .get(usize::from(classbook))
        .ok_or(DecodeError::VorbisBadSetup)?;
    let mut classwords = 1u32;
    for _ in 0..phrasebook.dimensions() {
        classwords = classwords
            .checked_mul(classifications)
            .filter(|&words| words <= phrasebook.entries())
            .ok_or(DecodeError::VorbisBadSetup)?;
    }
    Ok(Residue {
        kind,
        begin,
        end,
        partition_size,
        classifications,
        classbook,
        classwords,
        books: list,
        passes,
    })
}

/// The channels of one submap a residue decodes into: their vectors in the
/// packet's spectrum, `half` lines each, and which of them carry a floor.
pub(super) struct Bundle<'b> {
    pub(super) spectrum: &'b mut [f32],
    pub(super) half: usize,
    pub(super) channels: &'b [u8],
    pub(super) decode: &'b [bool],
}

impl Residue {
    /// The classifications one packet can need held, for `vectors` coded
    /// vectors of `half` lines: every partition, rounded up to whole
    /// classwords.
    pub(super) fn classes_needed(&self, half: usize, vectors: usize, books: &[Codebook]) -> usize {
        let span = if self.kind == Kind::Flattened {
            half * vectors
        } else {
            half
        };
        let coded = self.end.min(span).saturating_sub(self.begin.min(span));
        let per_word = books[usize::from(self.classbook)].dimensions();
        let partitions = (coded / self.partition_size).div_ceil(per_word) * per_word;
        if self.kind == Kind::Flattened {
            partitions
        } else {
            partitions * vectors
        }
    }

    /// Decode the residue into `bundle`, adding to its zeroed vectors; the
    /// packet ending part way stops it there, as the format intends.
    pub(super) fn decode(
        &self,
        bits: &mut Bits<'_>,
        books: &[Codebook],
        bundle: &mut Bundle<'_>,
        classes: &mut [u8],
    ) {
        let _ = self.try_decode(bits, books, bundle, classes);
    }

    fn try_decode(
        &self,
        bits: &mut Bits<'_>,
        books: &[Codebook],
        bundle: &mut Bundle<'_>,
        classes: &mut [u8],
    ) -> Result<(), EndOfPacket> {
        let flattened = self.kind == Kind::Flattened;
        let any = bundle
            .channels
            .iter()
            .any(|&channel| bundle.decode[usize::from(channel)]);
        if !any {
            return Ok(());
        }
        let width = if flattened {
            bundle.half * bundle.channels.len()
        } else {
            bundle.half
        };
        let begin = self.begin.min(width);
        let end = self.end.min(width);
        let partitions = end.saturating_sub(begin) / self.partition_size;
        if partitions == 0 {
            return Ok(());
        }
        let classbook = &books[usize::from(self.classbook)];
        let per_word = classbook.dimensions();
        let stride = partitions.div_ceil(per_word) * per_word;
        let vectors = if flattened { 1 } else { bundle.channels.len() };
        let classes = classes.get_mut(..stride * vectors).ok_or(EndOfPacket)?;
        for pass in 0..self.passes {
            let mut partition = 0;
            while partition < partitions {
                if pass == 0 {
                    for vector in 0..vectors {
                        if !self.coded(bundle, vector) {
                            continue;
                        }
                        let mut word = classbook.decode(bits)?;
                        if word >= self.classwords {
                            return Err(EndOfPacket);
                        }
                        let row = &mut classes[vector * stride + partition..][..per_word];
                        for slot in row.iter_mut().rev() {
                            *slot = u8::try_from(word % self.classifications).unwrap_or(0);
                            word /= self.classifications;
                        }
                    }
                }
                for _ in 0..per_word {
                    if partition == partitions {
                        break;
                    }
                    for vector in 0..vectors {
                        if !self.coded(bundle, vector) {
                            continue;
                        }
                        let class = usize::from(classes[vector * stride + partition]);
                        let Some(book) = self.books[class][pass] else {
                            continue;
                        };
                        let offset = begin + partition * self.partition_size;
                        self.partition(bits, &books[usize::from(book)], bundle, vector, offset)?;
                    }
                    partition += 1;
                }
            }
        }
        Ok(())
    }

    /// Whether coded vector `vector` of the bundle is decoded: a flattened
    /// residue's one vector whenever any channel is, otherwise the channel's
    /// own floor's say.
    fn coded(&self, bundle: &Bundle<'_>, vector: usize) -> bool {
        self.kind == Kind::Flattened || bundle.decode[usize::from(bundle.channels[vector])]
    }

    /// Add one partition's values, from `offset` within coded vector
    /// `vector`, coded with `book`.
    fn partition(
        &self,
        bits: &mut Bits<'_>,
        book: &Codebook,
        bundle: &mut Bundle<'_>,
        vector: usize,
        offset: usize,
    ) -> Result<(), EndOfPacket> {
        let size = self.partition_size;
        let half = bundle.half;
        match self.kind {
            Kind::Interleaved => {
                let lines = channel(bundle, vector);
                let step = size / book.dimensions();
                for i in 0..step {
                    let values = book.vector(bits)?;
                    for (j, value) in values.iter().enumerate() {
                        lines[offset + i + j * step] += value;
                    }
                }
            }
            Kind::Ordered => {
                let lines = &mut channel(bundle, vector)[offset..offset + size];
                let mut at = 0;
                while at < size {
                    let values = book.vector(bits)?;
                    for (line, value) in lines[at..].iter_mut().zip(values) {
                        *line += value;
                    }
                    at += values.len();
                }
            }
            Kind::Flattened => {
                let count = bundle.channels.len();
                let mut at = offset;
                let stop = offset + size;
                while at < stop {
                    let values = book.vector(bits)?;
                    for value in values.iter().take(stop - at) {
                        let channel = usize::from(bundle.channels[at % count]);
                        bundle.spectrum[channel * half + at / count] += value;
                        at += 1;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Coded vector `vector`'s lines: its channel's spectrum.
fn channel<'s>(bundle: &'s mut Bundle<'_>, vector: usize) -> &'s mut [f32] {
    let channel = usize::from(bundle.channels[vector]);
    &mut bundle.spectrum[channel * bundle.half..(channel + 1) * bundle.half]
}
