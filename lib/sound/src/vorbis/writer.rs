//! A Vorbis stream written from the decoded values it is to carry, and those
//! values decoded again by the specification's own formulas in double
//! precision — direct sums, no shared tables — which is what the decoder is
//! checked against.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "the streams written here keep their indices and counts far within the types they cross"
)]

extern crate std;

use std::vec;
use std::vec::Vec;

use tairix_fuzzseed::Prng;

use super::bits::{ilog, BitWriter};

/// A codebook: its codeword lengths, zero for an unused entry, and its
/// vectors where it has them.
#[derive(Clone)]
pub(crate) struct Book {
    pub(crate) dimensions: u16,
    pub(crate) lengths: Vec<u8>,
    pub(crate) ordered: bool,
    pub(crate) values: Option<Values>,
}

/// A book's vector lookup: a lattice (type 1) or a list (type 2).
#[derive(Clone)]
pub(crate) struct Values {
    pub(crate) lattice: bool,
    pub(crate) minimum: u32,
    pub(crate) delta: u32,
    pub(crate) value_bits: u32,
    pub(crate) sequence: bool,
    pub(crate) multiplicands: Vec<u32>,
}

/// `value` in the VQ float format, exactly, for a whole number of at most
/// 21 bits times a power of two the format reaches.
pub(crate) fn vq_float(mantissa: i32, exponent: i32) -> u32 {
    let sign = if mantissa < 0 { 0x8000_0000 } else { 0 };
    let field = u32::try_from(exponent + 788).expect("an exponent in range");
    sign | field << 21 | mantissa.unsigned_abs()
}

fn vq_value(raw: u32) -> f64 {
    let mantissa = f64::from(raw & 0x1F_FFFF);
    let exponent = i32::try_from((raw & 0x7FE0_0000) >> 21).expect("ten bits") - 788;
    let value = mantissa * 2f64.powi(exponent);
    if raw & 0x8000_0000 != 0 {
        -value
    } else {
        value
    }
}

impl Book {
    /// A book of `entries` entries all `length` bits long, read for entry
    /// numbers only.
    pub(crate) fn scalar(entries: usize, length: u8) -> Self {
        Self {
            dimensions: 1,
            lengths: vec![length; entries],
            ordered: false,
            values: None,
        }
    }

    /// Each entry's codeword, by the specification's rule written out
    /// longhand: the lowest value of its length that no codeword assigned
    /// before it is a prefix of, nor has as a prefix.
    pub(crate) fn codewords(&self) -> Vec<Option<(u32, u8)>> {
        let mut assigned: Vec<(u64, u8)> = Vec::new();
        self.lengths
            .iter()
            .map(|&length| {
                if length == 0 {
                    return None;
                }
                let found = (0..1u64 << length).find(|&code| {
                    assigned.iter().all(|&(other, other_length)| {
                        let shared = other_length.min(length);
                        other >> (other_length - shared) != code >> (length - shared)
                    })
                })?;
                assigned.push((found, length));
                Some((u32::try_from(found).expect("32 bits"), length))
            })
            .collect()
    }

    /// Entry `entry`'s vector, from the specification's formulas.
    pub(crate) fn vector(&self, entry: u32) -> Vec<f64> {
        let values = self.values.as_ref().expect("a book with vectors");
        let dimensions = usize::from(self.dimensions);
        let entries = self.lengths.len();
        let mut last = 0.0;
        let mut out = Vec::new();
        let side = if values.lattice {
            (1..=entries)
                .take_while(|r: &usize| r.pow(u32::from(self.dimensions)) <= entries)
                .last()
                .expect("one")
        } else {
            0
        };
        for dimension in 0..dimensions {
            let offset = if values.lattice {
                entry as usize / side.pow(u32::try_from(dimension).expect("small")) % side
            } else {
                entry as usize * dimensions + dimension
            };
            let value = f64::from(values.multiplicands[offset]) * vq_value(values.delta)
                + vq_value(values.minimum)
                + last;
            if values.sequence {
                last = value;
            }
            out.push(value);
        }
        out
    }

    fn write(&self, out: &mut BitWriter) {
        out.write(0x56_4342, 24);
        out.write(u32::from(self.dimensions), 16);
        out.write(u32::try_from(self.lengths.len()).expect("small"), 24);
        out.flag(self.ordered);
        if self.ordered {
            let mut at = 0;
            let mut length = self.lengths[0];
            out.write(u32::from(length) - 1, 5);
            while at < self.lengths.len() {
                let run = self.lengths[at..]
                    .iter()
                    .take_while(|&&each| each == length)
                    .count();
                let left = u32::try_from(self.lengths.len() - at).expect("small");
                out.write(u32::try_from(run).expect("small"), ilog(left));
                at += run;
                length += 1;
            }
        } else {
            let sparse = self.lengths.contains(&0);
            out.flag(sparse);
            for &length in &self.lengths {
                if sparse {
                    out.flag(length > 0);
                }
                if length > 0 {
                    out.write(u32::from(length) - 1, 5);
                }
            }
        }
        match &self.values {
            None => out.write(0, 4),
            Some(values) => {
                out.write(if values.lattice { 1 } else { 2 }, 4);
                out.write(values.minimum, 32);
                out.write(values.delta, 32);
                out.write(values.value_bits - 1, 4);
                out.flag(values.sequence);
                for &multiplicand in &values.multiplicands {
                    out.write(multiplicand, values.value_bits);
                }
            }
        }
    }

    /// Write entry `entry`'s codeword, its first bit first.
    fn encode(&self, entry: u32, out: &mut BitWriter) {
        let (code, length) = self.codewords()[entry as usize].expect("a used entry");
        for bit in (0..length).rev() {
            out.flag(code >> bit & 1 == 1);
        }
    }
}

/// A floor 1 partition class.
#[derive(Clone)]
pub(crate) struct Class {
    pub(crate) dimensions: u8,
    pub(crate) subclass_bits: u8,
    pub(crate) masterbook: u8,
    pub(crate) subclass_books: Vec<Option<u8>>,
}

#[derive(Clone)]
pub(crate) enum FloorSpec {
    Zero {
        order: u8,
        rate: u16,
        bark_map_size: u16,
        amplitude_bits: u8,
        amplitude_offset: u8,
        books: Vec<u8>,
    },
    One {
        partition_classes: Vec<u8>,
        classes: Vec<Class>,
        multiplier: u8,
        range_bits: u8,
        /// The posts past the two implicit ones, in list order.
        x: Vec<u16>,
    },
}

#[derive(Clone)]
pub(crate) struct ResidueSpec {
    pub(crate) kind: u16,
    pub(crate) begin: u32,
    pub(crate) end: u32,
    pub(crate) partition_size: u32,
    pub(crate) classifications: u8,
    pub(crate) classbook: u8,
    pub(crate) books: Vec<[Option<u8>; 8]>,
}

#[derive(Clone)]
pub(crate) struct MappingSpec {
    pub(crate) coupling: Vec<(u8, u8)>,
    pub(crate) mux: Vec<u8>,
    /// Each submap's floor and residue.
    pub(crate) submaps: Vec<(u8, u8)>,
}

#[derive(Clone)]
pub(crate) struct Spec {
    pub(crate) channels: u8,
    pub(crate) rate: u32,
    /// The two block sizes' exponents.
    pub(crate) blocksizes: [u8; 2],
    pub(crate) books: Vec<Book>,
    pub(crate) floors: Vec<FloorSpec>,
    pub(crate) residues: Vec<ResidueSpec>,
    pub(crate) mappings: Vec<MappingSpec>,
    /// Each mode's block flag and mapping.
    pub(crate) modes: Vec<(bool, u8)>,
}

/// What one channel's floor carries in a packet.
#[derive(Clone, Debug)]
pub(crate) enum FloorValues {
    Unused,
    Zero {
        amplitude: u64,
        book: u32,
        entries: Vec<u32>,
    },
    /// Each post's coded value, and each partition's subclass choice.
    One {
        y: Vec<u32>,
        choices: Vec<u32>,
    },
}

/// What one submap's residue carries: each coded vector's class for each
/// partition, and the entries each pass codes them with, in decode order.
#[derive(Clone, Debug, Default)]
pub(crate) struct ResidueValues {
    pub(crate) classes: Vec<Vec<u8>>,
    /// For each pass, vector and partition the class codes in that pass,
    /// the entries read.
    pub(crate) entries: Vec<Vec<Vec<Vec<u32>>>>,
}

#[derive(Clone, Debug)]
pub(crate) struct PacketSpec {
    pub(crate) mode: u8,
    /// A long block's window flags: the blocks before and after are long.
    pub(crate) neighbours: [bool; 2],
    pub(crate) floors: Vec<FloorValues>,
    pub(crate) residues: Vec<ResidueValues>,
}

impl Spec {
    pub(crate) fn blocksize(&self, long: bool) -> usize {
        1 << self.blocksizes[usize::from(long)]
    }

    pub(crate) fn identification(&self) -> Vec<u8> {
        let mut out = vec![1];
        out.extend_from_slice(b"vorbis");
        out.extend_from_slice(&0u32.to_le_bytes());
        out.push(self.channels);
        out.extend_from_slice(&self.rate.to_le_bytes());
        out.extend_from_slice(&[0; 12]);
        out.push(self.blocksizes[1] << 4 | self.blocksizes[0]);
        out.push(1);
        out
    }

    pub(crate) fn comment(vendor: &str, fields: &[&str]) -> Vec<u8> {
        let mut out = vec![3];
        out.extend_from_slice(b"vorbis");
        out.extend_from_slice(&u32::try_from(vendor.len()).expect("small").to_le_bytes());
        out.extend_from_slice(vendor.as_bytes());
        out.extend_from_slice(&u32::try_from(fields.len()).expect("small").to_le_bytes());
        for field in fields {
            out.extend_from_slice(&u32::try_from(field.len()).expect("small").to_le_bytes());
            out.extend_from_slice(field.as_bytes());
        }
        out.push(1);
        out
    }

    pub(crate) fn setup(&self) -> Vec<u8> {
        let mut out = BitWriter::default();
        for byte in [5u8, b'v', b'o', b'r', b'b', b'i', b's'] {
            out.write(u32::from(byte), 8);
        }
        out.write(u32::try_from(self.books.len() - 1).expect("small"), 8);
        for book in &self.books {
            book.write(&mut out);
        }
        out.write(0, 6);
        out.write(0, 16);
        out.write(u32::try_from(self.floors.len() - 1).expect("small"), 6);
        for floor in &self.floors {
            write_floor(floor, &mut out);
        }
        out.write(u32::try_from(self.residues.len() - 1).expect("small"), 6);
        for residue in &self.residues {
            out.write(u32::from(residue.kind), 16);
            out.write(residue.begin, 24);
            out.write(residue.end, 24);
            out.write(residue.partition_size - 1, 24);
            out.write(u32::from(residue.classifications) - 1, 6);
            out.write(u32::from(residue.classbook), 8);
            for row in &residue.books {
                let cascade = row
                    .iter()
                    .enumerate()
                    .filter(|(_, book)| book.is_some())
                    .fold(0u32, |cascade, (pass, _)| cascade | 1 << pass);
                out.write(cascade & 7, 3);
                out.flag(cascade > 7);
                if cascade > 7 {
                    out.write(cascade >> 3, 5);
                }
            }
            for row in &residue.books {
                for book in row.iter().flatten() {
                    out.write(u32::from(*book), 8);
                }
            }
        }
        out.write(u32::try_from(self.mappings.len() - 1).expect("small"), 6);
        for mapping in &self.mappings {
            out.write(0, 16);
            out.flag(mapping.submaps.len() > 1);
            if mapping.submaps.len() > 1 {
                out.write(u32::try_from(mapping.submaps.len() - 1).expect("small"), 4);
            }
            out.flag(!mapping.coupling.is_empty());
            if !mapping.coupling.is_empty() {
                out.write(u32::try_from(mapping.coupling.len() - 1).expect("small"), 8);
                let width = ilog(u32::from(self.channels) - 1);
                for &(magnitude, angle) in &mapping.coupling {
                    out.write(u32::from(magnitude), width);
                    out.write(u32::from(angle), width);
                }
            }
            out.write(0, 2);
            if mapping.submaps.len() > 1 {
                for &submap in &mapping.mux {
                    out.write(u32::from(submap), 4);
                }
            }
            for &(floor, residue) in &mapping.submaps {
                out.write(0, 8);
                out.write(u32::from(floor), 8);
                out.write(u32::from(residue), 8);
            }
        }
        out.write(u32::try_from(self.modes.len() - 1).expect("small"), 6);
        for &(long, mapping) in &self.modes {
            out.flag(long);
            out.write(0, 16);
            out.write(0, 16);
            out.write(u32::from(mapping), 8);
        }
        out.flag(true);
        out.into_bytes()
    }

    /// The audio packet `spec` describes.
    pub(crate) fn packet(&self, spec: &PacketSpec) -> Vec<u8> {
        let mut out = BitWriter::default();
        out.flag(false);
        out.write(
            u32::from(spec.mode),
            ilog(u32::try_from(self.modes.len() - 1).expect("small")),
        );
        let (long, mapping) = self.modes[usize::from(spec.mode)];
        if long {
            out.flag(spec.neighbours[0]);
            out.flag(spec.neighbours[1]);
        }
        let mapping = &self.mappings[usize::from(mapping)];
        for (channel, values) in spec.floors.iter().enumerate() {
            let (floor, _) = mapping.submaps[usize::from(mapping.mux[channel])];
            self.write_floor_values(&self.floors[usize::from(floor)], values, &mut out);
        }
        for (submap, values) in spec.residues.iter().enumerate() {
            let (_, residue) = mapping.submaps[submap];
            self.write_residue(&self.residues[usize::from(residue)], values, &mut out);
        }
        out.into_bytes()
    }

    fn write_floor_values(&self, floor: &FloorSpec, values: &FloorValues, out: &mut BitWriter) {
        match (floor, values) {
            (FloorSpec::Zero { amplitude_bits, .. }, FloorValues::Unused) => {
                out.write(0, u32::from(*amplitude_bits));
            }
            (FloorSpec::One { .. }, FloorValues::Unused) => out.flag(false),
            (
                FloorSpec::Zero {
                    amplitude_bits,
                    books,
                    ..
                },
                FloorValues::Zero {
                    amplitude,
                    book,
                    entries,
                },
            ) => {
                let bits = u32::from(*amplitude_bits);
                out.write(
                    u32::try_from(amplitude & 0xFFFF_FFFF).expect("low"),
                    bits.min(32),
                );
                out.write(
                    u32::try_from(amplitude >> 32).expect("high"),
                    bits.saturating_sub(32),
                );
                out.write(*book, ilog(u32::try_from(books.len()).expect("small")));
                let coding = &self.books[usize::from(books[*book as usize])];
                for &entry in entries {
                    coding.encode(entry, out);
                }
            }
            (
                FloorSpec::One {
                    partition_classes,
                    classes,
                    multiplier,
                    ..
                },
                FloorValues::One { y, choices },
            ) => {
                out.flag(true);
                let range = [256u32, 128, 86, 64][usize::from(*multiplier) - 1];
                out.write(y[0], ilog(range - 1));
                out.write(y[1], ilog(range - 1));
                let mut post = 2;
                for (&class, &choice) in partition_classes.iter().zip(choices) {
                    let class = &classes[usize::from(class)];
                    if class.subclass_bits > 0 {
                        self.books[usize::from(class.masterbook)].encode(choice, out);
                    }
                    let mut rest = choice;
                    for _ in 0..class.dimensions {
                        let subclass = rest & ((1 << class.subclass_bits) - 1);
                        rest >>= class.subclass_bits;
                        if let Some(book) = class.subclass_books[subclass as usize] {
                            self.books[usize::from(book)].encode(y[post], out);
                        } else {
                            assert_eq!(y[post], 0, "a post with no book codes zero");
                        }
                        post += 1;
                    }
                }
            }
            _ => panic!("floor values of another type"),
        }
    }

    fn write_residue(&self, residue: &ResidueSpec, values: &ResidueValues, out: &mut BitWriter) {
        let classbook = &self.books[usize::from(residue.classbook)];
        let per_word = usize::from(classbook.dimensions);
        let vectors = values.classes.len();
        let partitions = values.classes.first().map_or(0, Vec::len);
        for (pass, pass_entries) in values.entries.iter().enumerate() {
            let mut partition = 0;
            while partition < partitions {
                if pass == 0 {
                    for vector in 0..vectors {
                        let mut word = 0u32;
                        for i in 0..per_word {
                            let class = values.classes[vector]
                                .get(partition + i)
                                .copied()
                                .unwrap_or(0);
                            word = word * u32::from(residue.classifications) + u32::from(class);
                        }
                        classbook.encode(word, out);
                    }
                }
                for _ in 0..per_word {
                    if partition == partitions {
                        break;
                    }
                    for (classes, entries) in values.classes.iter().zip(pass_entries) {
                        let Some(book) = residue.books[usize::from(classes[partition])][pass]
                        else {
                            continue;
                        };
                        for &entry in &entries[partition] {
                            self.books[usize::from(book)].encode(entry, out);
                        }
                    }
                    partition += 1;
                }
            }
        }
    }
}

fn write_floor(floor: &FloorSpec, out: &mut BitWriter) {
    match floor {
        FloorSpec::Zero {
            order,
            rate,
            bark_map_size,
            amplitude_bits,
            amplitude_offset,
            books,
        } => {
            out.write(0, 16);
            out.write(u32::from(*order), 8);
            out.write(u32::from(*rate), 16);
            out.write(u32::from(*bark_map_size), 16);
            out.write(u32::from(*amplitude_bits), 6);
            out.write(u32::from(*amplitude_offset), 8);
            out.write(u32::try_from(books.len() - 1).expect("small"), 4);
            for &book in books {
                out.write(u32::from(book), 8);
            }
        }
        FloorSpec::One {
            partition_classes,
            classes,
            multiplier,
            range_bits,
            x,
        } => {
            out.write(1, 16);
            out.write(u32::try_from(partition_classes.len()).expect("small"), 5);
            for &class in partition_classes {
                out.write(u32::from(class), 4);
            }
            for class in classes {
                out.write(u32::from(class.dimensions) - 1, 3);
                out.write(u32::from(class.subclass_bits), 2);
                if class.subclass_bits > 0 {
                    out.write(u32::from(class.masterbook), 8);
                }
                for book in &class.subclass_books {
                    out.write(book.map_or(0, |book| u32::from(book) + 1), 8);
                }
            }
            out.write(u32::from(*multiplier) - 1, 2);
            out.write(u32::from(*range_bits), 4);
            for &post in x {
                out.write(u32::from(post), u32::from(*range_bits));
            }
        }
    }
}

/// A book of `lengths`, coding vectors `dimensions` wide from `values`.
pub(crate) fn vq_book(dimensions: u16, lengths: Vec<u8>, ordered: bool, values: Values) -> Book {
    Book {
        dimensions,
        lengths,
        ordered,
        values: Some(values),
    }
}

/// Floor 1, residue 2 and a coupled stereo pair, two block sizes.
pub(crate) fn floor1_stereo() -> Spec {
    Spec {
        channels: 2,
        rate: 44_100,
        blocksizes: [6, 8],
        books: vec![
            Book::scalar(128, 7),
            Book::scalar(4, 2),
            Book {
                dimensions: 2,
                lengths: vec![2; 4],
                ordered: false,
                values: None,
            },
            vq_book(
                2,
                vec![3, 3, 3, 3, 3, 3, 3, 4, 4],
                false,
                Values {
                    lattice: true,
                    minimum: vq_float(-1, 0),
                    delta: vq_float(1, 0),
                    value_bits: 2,
                    sequence: false,
                    multiplicands: vec![0, 1, 2],
                },
            ),
            vq_book(
                4,
                vec![3; 8],
                true,
                Values {
                    lattice: false,
                    minimum: vq_float(-1, 0),
                    delta: vq_float(1, -2),
                    value_bits: 3,
                    sequence: true,
                    multiplicands: (0..32).map(|i| (i * 5 + 3) % 8).collect(),
                },
            ),
        ],
        floors: vec![FloorSpec::One {
            partition_classes: vec![0, 1, 0],
            classes: vec![
                Class {
                    dimensions: 2,
                    subclass_bits: 1,
                    masterbook: 1,
                    subclass_books: vec![None, Some(0)],
                },
                Class {
                    dimensions: 1,
                    subclass_bits: 0,
                    masterbook: 0,
                    subclass_books: vec![Some(0)],
                },
            ],
            multiplier: 2,
            range_bits: 7,
            x: vec![30, 90, 60, 15, 110],
        }],
        residues: vec![ResidueSpec {
            kind: 2,
            begin: 0,
            end: 256,
            partition_size: 8,
            classifications: 2,
            classbook: 2,
            books: vec![[None; 8], {
                let mut row = [None; 8];
                row[0] = Some(3);
                row[1] = Some(4);
                row
            }],
        }],
        mappings: vec![MappingSpec {
            coupling: vec![(0, 1)],
            mux: vec![0, 0],
            submaps: vec![(0, 0)],
        }],
        modes: vec![(false, 0), (true, 0)],
    }
}

/// Two floor 0 submaps over three channels, residues 0 and 1, a coupled
/// pair inside the first submap, a coefficient book listing its vectors,
/// and a book of one used entry.
/// The books of [`floor0_three_channels`]: a coefficient book listing its
/// vectors, a classbook, a lattice and a listed residue book, and a book of
/// one used entry.
fn floor0_books() -> Vec<Book> {
    vec![
        vq_book(
            2,
            vec![4; 16],
            false,
            // Line spectral pairs a real filter has: increasing, apart,
            // and within half a turn, so its response stays finite.
            Values {
                lattice: false,
                minimum: vq_float(5, -4),
                delta: vq_float(1, -4),
                value_bits: 2,
                sequence: true,
                multiplicands: (0..32).map(|i| (i * 7 + 1) % 4).collect(),
            },
        ),
        Book::scalar(3, 1).with_lengths(vec![1, 2, 2]),
        vq_book(
            1,
            vec![2, 2, 2, 3, 3],
            false,
            Values {
                lattice: true,
                minimum: vq_float(-2, -1),
                delta: vq_float(1, -1),
                value_bits: 3,
                sequence: false,
                multiplicands: vec![0, 1, 2, 3, 4],
            },
        ),
        vq_book(
            2,
            vec![2; 4],
            true,
            Values {
                lattice: false,
                minimum: vq_float(-3, -2),
                delta: vq_float(1, -2),
                value_bits: 3,
                sequence: false,
                multiplicands: vec![0, 6, 3, 1, 5, 2, 4, 6],
            },
        ),
        vq_book(
            2,
            vec![0, 1, 0],
            false,
            Values {
                lattice: false,
                minimum: vq_float(1, -2),
                delta: vq_float(1, -1),
                value_bits: 2,
                sequence: false,
                multiplicands: vec![0, 0, 1, 2, 0, 0],
            },
        ),
    ]
}

pub(crate) fn floor0_three_channels() -> Spec {
    let mut residue0 = [[None; 8]; 3];
    residue0[1][0] = Some(2);
    residue0[1][1] = Some(3);
    residue0[2][0] = Some(3);
    residue0[2][2] = Some(4);
    let mut residue1 = [[None; 8]; 3];
    residue1[1][0] = Some(2);
    residue1[2][0] = Some(3);
    residue1[2][3] = Some(2);
    Spec {
        channels: 3,
        rate: 22_050,
        blocksizes: [6, 7],
        books: floor0_books(),
        floors: vec![
            FloorSpec::Zero {
                order: 6,
                rate: 22_050,
                bark_map_size: 64,
                amplitude_bits: 8,
                amplitude_offset: 40,
                books: vec![0],
            },
            FloorSpec::Zero {
                order: 5,
                rate: 22_050,
                bark_map_size: 48,
                amplitude_bits: 7,
                amplitude_offset: 30,
                books: vec![0, 0],
            },
        ],
        residues: vec![
            ResidueSpec {
                kind: 0,
                begin: 4,
                end: 100,
                partition_size: 8,
                classifications: 3,
                classbook: 1,
                books: residue0.to_vec(),
            },
            ResidueSpec {
                kind: 1,
                begin: 0,
                end: 64,
                partition_size: 4,
                classifications: 3,
                classbook: 1,
                books: residue1.to_vec(),
            },
        ],
        mappings: vec![MappingSpec {
            coupling: vec![(2, 0)],
            mux: vec![0, 1, 0],
            submaps: vec![(0, 0), (1, 1)],
        }],
        modes: vec![(false, 0), (true, 0)],
    }
}

impl Book {
    pub(crate) fn with_lengths(mut self, lengths: Vec<u8>) -> Self {
        self.lengths = lengths;
        self
    }
}

/// The values a residue codes for one block of `half` lines a channel, every
/// class and entry drawn from `prng`.
pub(crate) fn residue_values(
    spec: &Spec,
    residue: &ResidueSpec,
    vectors: usize,
    half: usize,
    prng: &mut Prng,
) -> ResidueValues {
    let width = if residue.kind == 2 {
        half * vectors
    } else {
        half
    };
    let coded = (residue.end as usize)
        .min(width)
        .saturating_sub((residue.begin as usize).min(width));
    let partitions = coded / residue.partition_size as usize;
    let coded_vectors = if residue.kind == 2 { 1 } else { vectors };
    let classes: Vec<Vec<u8>> = (0..coded_vectors)
        .map(|_| {
            (0..partitions)
                .map(|_| prng.next_u8() % residue.classifications)
                .collect()
        })
        .collect();
    let passes = residue
        .books
        .iter()
        .flat_map(|row| {
            row.iter()
                .enumerate()
                .filter(|(_, book)| book.is_some())
                .map(|(pass, _)| pass + 1)
        })
        .max()
        .unwrap_or(0);
    let entries = (0..passes)
        .map(|pass| {
            (0..coded_vectors)
                .map(|vector| {
                    (0..partitions)
                        .map(|partition| {
                            let class = classes[vector][partition];
                            let Some(book) = residue.books[usize::from(class)][pass] else {
                                return Vec::new();
                            };
                            let book = &spec.books[usize::from(book)];
                            let dimensions = usize::from(book.dimensions);
                            let size = residue.partition_size as usize;
                            let reads = if residue.kind == 0 {
                                size / dimensions
                            } else {
                                size.div_ceil(dimensions)
                            };
                            let used: Vec<u32> = book
                                .lengths
                                .iter()
                                .enumerate()
                                .filter(|(_, &length)| length > 0)
                                .map(|(entry, _)| entry as u32)
                                .collect();
                            (0..reads)
                                .map(|_| used[usize::from(prng.next_u8()) % used.len()])
                                .collect()
                        })
                        .collect()
                })
                .collect()
        })
        .collect();
    ResidueValues { classes, entries }
}

/// A packet of mode `mode`, its floors and residues drawn from `prng`; a
/// channel's floor is unused where `unused` says.
pub(crate) fn packet(
    spec: &Spec,
    mode: u8,
    neighbours: [bool; 2],
    unused: &[bool],
    prng: &mut Prng,
) -> PacketSpec {
    let (long, mapping) = spec.modes[usize::from(mode)];
    let mapping = &spec.mappings[usize::from(mapping)];
    let half = spec.blocksize(long) / 2;
    let floors = (0..usize::from(spec.channels))
        .map(|channel| {
            if unused[channel] {
                return FloorValues::Unused;
            }
            let (floor, _) = mapping.submaps[usize::from(mapping.mux[channel])];
            match &spec.floors[usize::from(floor)] {
                FloorSpec::One {
                    partition_classes,
                    classes,
                    multiplier,
                    x,
                    ..
                } => {
                    let range = [256u32, 128, 86, 64][usize::from(*multiplier) - 1];
                    let mut y = vec![
                        u32::from(prng.next_u8()) % range,
                        u32::from(prng.next_u8()) % range,
                    ];
                    let mut choices = Vec::new();
                    for &class in partition_classes {
                        let class = &classes[usize::from(class)];
                        let choice = u32::from(prng.next_u8())
                            % (1 << (u32::from(class.subclass_bits) * u32::from(class.dimensions)));
                        let mut rest = choice;
                        for _ in 0..class.dimensions {
                            let subclass = rest & ((1 << class.subclass_bits) - 1);
                            rest >>= class.subclass_bits;
                            y.push(if class.subclass_books[subclass as usize].is_some() {
                                u32::from(prng.next_u8()) % 40
                            } else {
                                0
                            });
                        }
                        choices.push(choice);
                    }
                    assert_eq!(y.len(), x.len() + 2);
                    FloorValues::One { y, choices }
                }
                FloorSpec::Zero {
                    order,
                    amplitude_bits,
                    books,
                    ..
                } => {
                    let book = u32::from(prng.next_u8()) % books.len() as u32;
                    let coding = &spec.books[usize::from(books[book as usize])];
                    let reads = usize::from(*order).div_ceil(usize::from(coding.dimensions));
                    // A sixteenth of the range keeps the curve near unity,
                    // as an encoder's floors are.
                    FloorValues::Zero {
                        amplitude: 1 + u64::from(prng.next_u8()) % ((1u64 << amplitude_bits) / 16),
                        book,
                        entries: (0..reads)
                            .map(|_| u32::from(prng.next_u8()) % coding.lengths.len() as u32)
                            .collect(),
                    }
                }
            }
        })
        .collect();
    let residues = mapping
        .submaps
        .iter()
        .enumerate()
        .map(|(submap, &(_, residue))| {
            let vectors = mapping
                .mux
                .iter()
                .filter(|&&each| usize::from(each) == submap)
                .count();
            residue_values(
                spec,
                &spec.residues[usize::from(residue)],
                vectors,
                half,
                prng,
            )
        })
        .collect();
    PacketSpec {
        mode,
        neighbours,
        floors,
        residues,
    }
}

/// Packets of the given modes, each long block's flags stating its actual
/// neighbours, with the floors `unused` lists unused in each.
pub(crate) fn packets(
    spec: &Spec,
    modes: &[u8],
    unused: &[Vec<bool>],
    seed: u64,
) -> Vec<PacketSpec> {
    let mut prng = Prng::new(seed);
    let long = |mode: u8| spec.modes[usize::from(mode)].0;
    modes
        .iter()
        .enumerate()
        .map(|(at, &mode)| {
            let before = at.checked_sub(1).is_none_or(|before| long(modes[before]));
            let after = modes.get(at + 1).is_none_or(|&after| long(after));
            let none = vec![false; usize::from(spec.channels)];
            packet(
                spec,
                mode,
                [before, after],
                unused.get(at).unwrap_or(&none),
                &mut prng,
            )
        })
        .collect()
}

/// `packets` as an Ogg stream of serial `serial`: the identification header
/// alone on the first page, the comment and setup headers ending the next,
/// the first two audio packets on the next as the mapping lays them, then
/// each packet on a page of its own. Each granule is the mapping's count
/// moved by `shift`, the last page's less `trim`.
pub(crate) fn ogg_stream(
    spec: &Spec,
    packets: &[PacketSpec],
    serial: u32,
    shift: i64,
    trim: u64,
) -> Vec<u8> {
    let mut pages = crate::ogg::PageWriter::new(serial);
    pages.packet(&spec.identification(), 0);
    pages.flush(false);
    pages.packet(&Spec::comment("tairix tests", &["TITLE=Written here"]), 0);
    pages.packet(&spec.setup(), 0);
    pages.flush(false);
    let mut position = 0i64;
    let mut previous: Option<usize> = None;
    for (at, packet) in packets.iter().enumerate() {
        let size = spec.blocksize(spec.modes[usize::from(packet.mode)].0);
        if let Some(before) = previous {
            position += i64::try_from((before + size) / 4).expect("small");
        }
        previous = Some(size);
        let last = at + 1 == packets.len();
        let granule = position + shift
            - if last {
                i64::try_from(trim).expect("small")
            } else {
                0
            };
        // The first packet shares its page, whose granule is the second's.
        pages.packet(
            &spec.packet(packet),
            u64::try_from(granule.max(0)).expect("past zero"),
        );
        if at != 0 || last {
            pages.flush(last);
        }
    }
    pages.into_bytes()
}

/// The specification's decode of a stream: each block's spectrum built from
/// the values written, transformed by the direct inverse MDCT, windowed by
/// the specification's slopes and summed into the output at its absolute
/// place, the first block centred on sample zero.
pub(crate) struct Reference<'s> {
    spec: &'s Spec,
    /// Every sample from the first block's start, at its absolute place
    /// less the first block's half.
    sum: Vec<Vec<f64>>,
    /// Where the next block's centre lies, and the last block's size.
    centre: Option<(i64, usize)>,
    origin: i64,
}

/// Undo the coupling steps, last first (Vorbis I, section 4.3.5).
fn uncouple(spectra: &mut [Vec<f64>], coupling: &[(u8, u8)]) {
    for &(magnitude, angle) in coupling.iter().rev() {
        let (m, a) = (usize::from(magnitude), usize::from(angle));
        let pairs: Vec<(f64, f64)> = spectra[m]
            .iter()
            .copied()
            .zip(spectra[a].iter().copied())
            .collect();
        for (i, (mv, av)) in pairs.into_iter().enumerate() {
            let (nm, na) = if mv > 0.0 {
                if av > 0.0 {
                    (mv, mv - av)
                } else {
                    (mv + av, mv)
                }
            } else if av > 0.0 {
                (mv, mv + av)
            } else {
                (mv - av, mv)
            };
            spectra[m][i] = nm;
            spectra[a][i] = na;
        }
    }
}

impl<'s> Reference<'s> {
    pub(crate) fn new(spec: &'s Spec) -> Self {
        Self {
            spec,
            sum: vec![Vec::new(); usize::from(spec.channels)],
            centre: None,
            origin: 0,
        }
    }

    /// Decode one packet, as written.
    pub(crate) fn block(&mut self, packet: &PacketSpec) {
        let spec = self.spec;
        let (long, mapping) = spec.modes[usize::from(packet.mode)];
        let n = spec.blocksize(long);
        let half = n / 2;
        let mapping = &spec.mappings[usize::from(mapping)];
        let channels = usize::from(spec.channels);
        let mut spectra = vec![vec![0.0f64; half]; channels];
        let mut used: Vec<bool> = packet
            .floors
            .iter()
            .map(|values| !matches!(values, FloorValues::Unused))
            .collect();
        let floors_used = used.clone();
        for &(magnitude, angle) in &mapping.coupling {
            let either = used[usize::from(magnitude)] || used[usize::from(angle)];
            used[usize::from(magnitude)] = either;
            used[usize::from(angle)] = either;
        }
        for (submap, values) in packet.residues.iter().enumerate() {
            let (_, residue) = mapping.submaps[submap];
            let members: Vec<usize> = (0..channels)
                .filter(|&channel| usize::from(mapping.mux[channel]) == submap)
                .collect();
            self.residue(
                &spec.residues[usize::from(residue)],
                values,
                &members,
                &used,
                half,
                &mut spectra,
            );
        }
        uncouple(&mut spectra, &mapping.coupling);
        for channel in 0..channels {
            let (floor, _) = mapping.submaps[usize::from(mapping.mux[channel])];
            let curve = if floors_used[channel] {
                self.floor_curve(
                    &spec.floors[usize::from(floor)],
                    &packet.floors[channel],
                    half,
                )
            } else {
                vec![0.0; half]
            };
            for (line, level) in spectra[channel].iter_mut().zip(curve) {
                *line *= level;
            }
        }
        let window = self.window(long, packet.neighbours, n);
        let start = match self.centre {
            None => {
                self.origin = -i64::try_from(half).expect("small");
                self.origin
            }
            Some((centre, previous)) => {
                centre + i64::try_from(previous / 4 + n / 4).expect("small")
                    - i64::try_from(half).expect("small")
            }
        };
        let centre = start + i64::try_from(half).expect("small");
        for (channel, spectrum) in spectra.iter().enumerate() {
            let offset = usize::try_from(start - self.origin).expect("after the origin");
            let sum = &mut self.sum[channel];
            if sum.len() < offset + n {
                sum.resize(offset + n, 0.0);
            }
            for (i, w) in window.iter().enumerate() {
                let x: f64 = spectrum
                    .iter()
                    .enumerate()
                    .map(|(k, value)| {
                        value
                            * (core::f64::consts::PI / (2.0 * n as f64)
                                * (2.0 * i as f64 + 1.0 + n as f64 / 2.0)
                                * (2.0 * k as f64 + 1.0))
                                .cos()
                    })
                    .sum();
                sum[offset + i] += x * w;
            }
        }
        self.centre = Some((centre, n));
    }

    /// The samples finished so far, interleaved: from the first block's
    /// centre to the last's.
    pub(crate) fn samples(&self) -> Vec<f64> {
        let Some((centre, _)) = self.centre else {
            return Vec::new();
        };
        let from = usize::try_from(-self.origin).expect("the first centre");
        let to = usize::try_from(centre - self.origin).expect("after the origin");
        let mut out = Vec::new();
        for at in from..to {
            for channel in &self.sum {
                out.push(channel[at]);
            }
        }
        out
    }

    fn window(&self, long: bool, neighbours: [bool; 2], n: usize) -> Vec<f64> {
        let short = self.spec.blocksize(false);
        let slope = |i: usize, start: usize, len: usize, falling: bool| {
            let x = (i - start) as f64 + 0.5;
            let phase = x / len as f64 * core::f64::consts::FRAC_PI_2
                + if falling {
                    core::f64::consts::FRAC_PI_2
                } else {
                    0.0
                };
            (core::f64::consts::FRAC_PI_2 * phase.sin().powi(2)).sin()
        };
        let (left_start, left_end, left_n) = if long && !neighbours[0] {
            (n / 4 - short / 4, n / 4 + short / 4, short / 2)
        } else {
            (0, n / 2, n / 2)
        };
        let (right_start, right_end, right_n) = if long && !neighbours[1] {
            (n * 3 / 4 - short / 4, n * 3 / 4 + short / 4, short / 2)
        } else {
            (n / 2, n, n / 2)
        };
        (0..n)
            .map(|i| {
                if i < left_start || i >= right_end {
                    0.0
                } else if i < left_end {
                    slope(i, left_start, left_n, false)
                } else if i < right_start {
                    1.0
                } else {
                    slope(i, right_start, right_n, true)
                }
            })
            .collect()
    }

    fn floor_curve(&self, floor: &FloorSpec, values: &FloorValues, half: usize) -> Vec<f64> {
        match (floor, values) {
            (
                FloorSpec::One {
                    multiplier,
                    range_bits,
                    x,
                    ..
                },
                FloorValues::One { y, .. },
            ) => floor1_curve(*multiplier, *range_bits, x, y, half),
            (
                FloorSpec::Zero {
                    order,
                    rate,
                    bark_map_size,
                    amplitude_bits,
                    amplitude_offset,
                    books,
                },
                FloorValues::Zero {
                    amplitude,
                    book,
                    entries,
                },
            ) => {
                let coding = &self.spec.books[usize::from(books[*book as usize])];
                let mut coefficients = Vec::new();
                let mut last = 0.0;
                for &entry in entries {
                    let vector = coding.vector(entry);
                    let take = vector.len().min(usize::from(*order) - coefficients.len());
                    for value in &vector[..take] {
                        coefficients.push(value + last);
                    }
                    last = *coefficients.last().expect("one");
                }
                floor0_curve(
                    &coefficients,
                    *amplitude,
                    *amplitude_bits,
                    *amplitude_offset,
                    *rate,
                    *bark_map_size,
                    half,
                )
            }
            _ => panic!("floor values of another type"),
        }
    }

    fn residue(
        &self,
        residue: &ResidueSpec,
        values: &ResidueValues,
        members: &[usize],
        used: &[bool],
        half: usize,
        spectra: &mut [Vec<f64>],
    ) {
        let flattened = residue.kind == 2;
        let mut vectors: Vec<Vec<f64>> = if flattened {
            vec![vec![0.0; half * members.len()]]
        } else {
            vec![vec![0.0; half]; members.len()]
        };
        let size = residue.partition_size as usize;
        let begin = residue.begin as usize;
        for (pass, pass_entries) in values.entries.iter().enumerate() {
            for (vector, partitions) in pass_entries.iter().enumerate() {
                for (partition, entries) in partitions.iter().enumerate() {
                    let class = values.classes[vector][partition];
                    let Some(book) = residue.books[usize::from(class)][pass] else {
                        continue;
                    };
                    let book = &self.spec.books[usize::from(book)];
                    let offset = begin + partition * size;
                    let target = &mut vectors[vector];
                    if residue.kind == 0 {
                        let step = size / usize::from(book.dimensions);
                        for (i, &entry) in entries.iter().enumerate() {
                            for (j, value) in book.vector(entry).iter().enumerate() {
                                target[offset + i + j * step] += value;
                            }
                        }
                    } else {
                        let mut at = offset;
                        for &entry in entries {
                            for value in book.vector(entry) {
                                if at < offset + size {
                                    target[at] += value;
                                }
                                at += 1;
                            }
                        }
                    }
                }
            }
        }
        if flattened {
            if members.iter().any(|&channel| used[channel]) {
                for (i, value) in vectors[0].iter().enumerate() {
                    spectra[members[i % members.len()]][i / members.len()] += value;
                }
            }
        } else {
            for (vector, &channel) in members.iter().enumerate() {
                if used[channel] {
                    for (line, value) in spectra[channel].iter_mut().zip(&vectors[vector]) {
                        *line += value;
                    }
                }
            }
        }
    }
}

/// Floor 1's curve by the specification's two steps (section 7.2.4).
fn floor1_curve(
    multiplier: u8,
    range_bits: u8,
    x_rest: &[u16],
    y: &[u32],
    half: usize,
) -> Vec<f64> {
    let range = [256i64, 128, 86, 64][usize::from(multiplier) - 1];
    let mut x: Vec<i64> = vec![0, 1 << range_bits];
    x.extend(x_rest.iter().map(|&post| i64::from(post)));
    let mut final_y: Vec<i64> = y.iter().map(|&value| i64::from(value)).collect();
    let mut step2 = vec![true, true];
    step2.resize(x.len(), false);
    for i in 2..x.len() {
        let low = (0..i)
            .filter(|&j| x[j] < x[i])
            .max_by_key(|&j| x[j])
            .expect("below");
        let high = (0..i)
            .filter(|&j| x[j] > x[i])
            .min_by_key(|&j| x[j])
            .expect("above");
        let dy = final_y[high] - final_y[low];
        let offset = dy.abs() * (x[i] - x[low]) / (x[high] - x[low]);
        let predicted = if dy < 0 {
            final_y[low] - offset
        } else {
            final_y[low] + offset
        };
        let value = final_y[i];
        let high_room = range - predicted;
        let low_room = predicted;
        let room = if high_room < low_room {
            high_room * 2
        } else {
            low_room * 2
        };
        if value == 0 {
            final_y[i] = predicted;
            continue;
        }
        step2[low] = true;
        step2[high] = true;
        step2[i] = true;
        final_y[i] = if value >= room {
            if high_room > low_room {
                value - low_room + predicted
            } else {
                predicted - value + high_room - 1
            }
        } else if value % 2 == 1 {
            predicted - (value + 1) / 2
        } else {
            predicted + value / 2
        };
    }
    let mut order: Vec<usize> = (0..x.len()).collect();
    order.sort_by_key(|&post| x[post]);
    let mut floor = vec![0i64; half.max(1 << range_bits) + 1];
    let line = |floor: &mut Vec<i64>, x0: i64, y0: i64, x1: i64, y1: i64| {
        let dy = y1 - y0;
        let adx = x1 - x0;
        let base = dy / adx;
        let sy = if dy < 0 { base - 1 } else { base + 1 };
        let ady = dy.abs() - base.abs() * adx;
        let mut y = y0;
        let mut err = 0;
        floor[usize::try_from(x0).expect("in range")] = y;
        for at in x0 + 1..x1 {
            err += ady;
            if err >= adx {
                err -= adx;
                y += sy;
            } else {
                y += base;
            }
            floor[usize::try_from(at).expect("in range")] = y;
        }
    };
    let m = i64::from(multiplier);
    let (mut lx, mut ly, mut hx, mut hy) = (0, final_y[order[0]] * m, 0, 0);
    for &post in &order[1..] {
        if step2[post] {
            hy = final_y[post] * m;
            hx = x[post];
            line(&mut floor, lx, ly, hx, hy);
            lx = hx;
            ly = hy;
        }
    }
    let n = i64::try_from(half).expect("small");
    if hx < n {
        line(&mut floor, hx, hy, n, hy);
    }
    floor[..half]
        .iter()
        .map(|&value| {
            f64::from(super::floor::FLOOR1_INVERSE_DB[usize::try_from(value).expect("in table")])
        })
        .collect()
}

/// Floor 0's curve by the specification's formulas (section 6.2.3), its
/// Bark map computed from them too.
fn floor0_curve(
    coefficients: &[f64],
    amplitude: u64,
    amplitude_bits: u8,
    amplitude_offset: u8,
    rate: u16,
    bark_map_size: u16,
    half: usize,
) -> Vec<f64> {
    let bark =
        |x: f64| 13.1 * (0.00074 * x).atan() + 2.24 * (0.000_000_018_5 * x * x).atan() + 0.0001 * x;
    let size = f64::from(bark_map_size);
    let order = coefficients.len();
    (0..half)
        .map(|i| {
            let band = (bark(f64::from(rate) * i as f64 / (2.0 * half as f64)) * size
                / bark(0.5 * f64::from(rate)))
            .floor()
            .min(size - 1.0);
            let omega = core::f64::consts::PI * band / size;
            let cos_omega = omega.cos();
            let product = |start: usize| {
                (start..order)
                    .step_by(2)
                    .map(|j| 4.0 * (coefficients[j].cos() - cos_omega).powi(2))
                    .product::<f64>()
            };
            let (p, q) = if order % 2 == 1 {
                (
                    (1.0 - cos_omega * cos_omega) * product(1),
                    0.25 * product(0),
                )
            } else {
                (
                    (1.0 - cos_omega) / 2.0 * product(1),
                    f64::midpoint(1.0, cos_omega) * product(0),
                )
            };
            let max = (1u64 << amplitude_bits) as f64 - 1.0;
            let offset = f64::from(amplitude_offset);
            (0.115_129_25 * (amplitude as f64 * offset / (max * (p + q).sqrt()) - offset)).exp()
        })
        .collect()
}
