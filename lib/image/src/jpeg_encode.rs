//! A baseline JPEG encoder (ITU-T T.81): JFIF framing, 8-bit samples,
//! Huffman coding with the typical tables of Annex K, and the quality
//! scaling of the Independent JPEG Group's reference encoder, so a quality
//! number means what it means to every other encoder.
//!
//! The forward DCT is the accurate integer transform of the reference
//! encoder (Loeffler, Ligtenberg and Moschytz), whose output is the true
//! transform scaled by eight — which the quantiser divides back out.

use alloc::vec::Vec;

use crate::encode::{
    indices_fit, palette_fits, scratch, EncodeError, JpegOptions, Output, RowBuffers,
};
use crate::huffman::{assign, Assigned, MAX_CODE_BITS};
use crate::jpeg::{
    descale, jfif_payload, APP0, DCT_PASS1_BITS, DCT_SCALE_BITS, DHT, DQT, EOI, FIX_0_298631336,
    FIX_0_390180644, FIX_0_541196100, FIX_0_765366865, FIX_0_899976223, FIX_1_175875602,
    FIX_1_501321110, FIX_1_847759065, FIX_1_961570560, FIX_2_053119869, FIX_2_562915447,
    FIX_3_072711026, JFIF_LEN, SOF0, SOI, SOS, ZIGZAG,
};
use crate::picture::{flatten_row, PictureKind, PictureSource};
use crate::RGBA_BYTES;

/// Largest width or height a frame header's two-byte fields may state.
const MAX_SIDE: u32 = u16::MAX as u32;

/// Quality at and above which colour keeps full resolution.
const FULL_CHROMA_QUALITY: u8 = 90;

/// The `APP0` segment's length field: itself and its payload.
const JFIF_SEGMENT_LEN: u16 = 16;
const _: () = assert!(JFIF_SEGMENT_LEN as usize == 2 + JFIF_LEN);

/// Table K.1: the luminance quantisation table, natural order.
const LUMA_QUANT: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];

/// Table K.2: the chrominance quantisation table, natural order.
const CHROMA_QUANT: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

/// One Huffman table as a `DHT` segment states it: the count of codes of
/// each length, then the symbols in code order.
struct TableSpec {
    counts: [u8; MAX_CODE_BITS],
    symbols: &'static [u8],
}

/// Table K.3: luminance DC differences.
const DC_LUMA: TableSpec = TableSpec {
    counts: [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0],
    symbols: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
};

/// Table K.4: chrominance DC differences.
const DC_CHROMA: TableSpec = TableSpec {
    counts: [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0],
    symbols: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
};

/// Table K.5: luminance AC coefficients.
const AC_LUMA: TableSpec = TableSpec {
    counts: [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7D],
    symbols: &[
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61,
        0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52,
        0xD1, 0xF0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25,
        0x26, 0x27, 0x28, 0x29, 0x2A, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45,
        0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64,
        0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83,
        0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
        0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6,
        0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3,
        0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8,
        0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA,
    ],
};

/// Table K.6: chrominance AC coefficients.
const AC_CHROMA: TableSpec = TableSpec {
    counts: [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77],
    symbols: &[
        0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61,
        0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xA1, 0xB1, 0xC1, 0x09, 0x23, 0x33,
        0x52, 0xF0, 0x15, 0x62, 0x72, 0xD1, 0x0A, 0x16, 0x24, 0x34, 0xE1, 0x25, 0xF1, 0x17, 0x18,
        0x19, 0x1A, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44,
        0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63,
        0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A,
        0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
        0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4,
        0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA,
        0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7,
        0xE8, 0xE9, 0xEA, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA,
    ],
};

/// The AC symbol that ends a block's run of zeros.
const END_OF_BLOCK: u8 = 0x00;

/// The AC symbol for sixteen zeros in a row.
const ZERO_RUN: u8 = 0xF0;

/// JFIF's BT.601 colour transform, scaled by `1 << 16`.
const Y_R: i32 = 19595;
const Y_G: i32 = 38470;
const Y_B: i32 = 7471;
const CB_R: i32 = 11059;
const CB_G: i32 = 21709;
const CR_G: i32 = 27439;
const CR_B: i32 = 5329;
const HALF: i32 = 1 << 15;

/// Each table's codes, assigned when the crate is built.
const DC_LUMA_CODES: Assigned = assign(&DC_LUMA.counts, DC_LUMA.symbols);
const AC_LUMA_CODES: Assigned = assign(&AC_LUMA.counts, AC_LUMA.symbols);
const DC_CHROMA_CODES: Assigned = assign(&DC_CHROMA.counts, DC_CHROMA.symbols);
const AC_CHROMA_CODES: Assigned = assign(&AC_CHROMA.counts, AC_CHROMA.symbols);

/// The DC and AC codes of the luminance and the chrominance tables.
const CODES: [[&Assigned; 2]; 2] = [
    [&DC_LUMA_CODES, &AC_LUMA_CODES],
    [&DC_CHROMA_CODES, &AC_CHROMA_CODES],
];

/// The entropy-coded segment being written, bytes stuffed as the format
/// requires.
struct Bits<'a> {
    out: &'a mut Output,
    buffer: u32,
    count: u32,
}

impl Bits<'_> {
    /// Emit the low `len` bits of `value`, most significant first.
    fn put(&mut self, value: u32, len: u32) -> Result<(), EncodeError> {
        if len == 0 {
            return Ok(());
        }
        self.buffer = self.buffer << len | (value & ((1 << len) - 1));
        self.count += len;
        while self.count >= 8 {
            self.count -= 8;
            let byte = u8::try_from(self.buffer >> self.count & 0xFF).unwrap_or(0);
            self.out.byte(byte)?;
            // A 0xFF data byte would read as a marker without its stuffing.
            if byte == 0xFF {
                self.out.byte(0)?;
            }
        }
        self.buffer &= (1 << self.count) - 1;
        Ok(())
    }

    /// Emit `symbol`'s code from `codes`.
    fn code(&mut self, codes: &Assigned, symbol: u8) -> Result<(), EncodeError> {
        let symbol = usize::from(symbol);
        self.put(u32::from(codes.code[symbol]), u32::from(codes.len[symbol]))
    }

    /// Pad the last byte with one-bits, which no code can end on.
    fn finish(&mut self) -> Result<(), EncodeError> {
        let pad = (8 - self.count % 8) % 8;
        self.put((1 << pad) - 1, pad)
    }
}

/// The bits needed to state `value`'s magnitude: its category.
fn category(value: i32) -> u32 {
    u32::BITS - value.unsigned_abs().leading_zeros()
}

/// A coefficient as the entropy coder states it after its category: the
/// value itself when positive, its ones' complement when negative.
fn magnitude_bits(value: i32) -> u32 {
    if value < 0 {
        (value - 1).cast_unsigned()
    } else {
        value.cast_unsigned()
    }
}

/// `base` scaled to `quality` as the reference encoder does it, clamped to
/// what a baseline eight-bit table may hold.
fn scaled_table(base: &[u16; 64], quality: u8) -> [u16; 64] {
    let quality = u32::from(quality);
    let scale = if quality < 50 {
        5000 / quality
    } else {
        200 - quality * 2
    };
    base.map(|entry| {
        let value = (u32::from(entry) * scale + 50) / 100;
        u16::try_from(value.clamp(1, 255)).unwrap_or(255)
    })
}

/// The accurate integer forward DCT of one level-shifted block, in place:
/// rows, then columns. The result is the true transform scaled by eight.
fn forward_dct(block: &mut [i32; 64]) {
    for row in block.as_chunks_mut::<8>().0 {
        let (even, odd) = butterfly([
            row[0], row[1], row[2], row[3], row[4], row[5], row[6], row[7],
        ]);
        row[0] = (even[0] + even[1]) << DCT_PASS1_BITS;
        row[4] = (even[0] - even[1]) << DCT_PASS1_BITS;
        let z1 = (even[3] + even[2]) * FIX_0_541196100;
        let shift = DCT_SCALE_BITS - DCT_PASS1_BITS;
        row[2] = descale(z1 + even[2] * FIX_0_765366865, shift);
        row[6] = descale(z1 - even[3] * FIX_1_847759065, shift);
        let parts = odd_part(odd);
        for (slot, value) in [7, 5, 3, 1].into_iter().zip(parts) {
            row[slot] = descale(value, shift);
        }
    }
    for column in 0..8 {
        let at = |row: usize| block[row * 8 + column];
        let (even, odd) = butterfly([at(0), at(1), at(2), at(3), at(4), at(5), at(6), at(7)]);
        block[column] = descale(even[0] + even[1], DCT_PASS1_BITS);
        block[32 + column] = descale(even[0] - even[1], DCT_PASS1_BITS);
        let z1 = (even[3] + even[2]) * FIX_0_541196100;
        let shift = DCT_SCALE_BITS + DCT_PASS1_BITS;
        block[16 + column] = descale(z1 + even[2] * FIX_0_765366865, shift);
        block[48 + column] = descale(z1 - even[3] * FIX_1_847759065, shift);
        let parts = odd_part(odd);
        for (row, value) in [7, 5, 3, 1].into_iter().zip(parts) {
            block[row * 8 + column] = descale(value, shift);
        }
    }
}

/// The first butterfly of the transform: the even part's sums and
/// differences `[tmp10, tmp11, tmp13, tmp12]`, and the odd part's four
/// differences `[tmp4, tmp5, tmp6, tmp7]`.
fn butterfly(d: [i32; 8]) -> ([i32; 4], [i32; 4]) {
    let (tmp0, tmp7) = (d[0] + d[7], d[0] - d[7]);
    let (tmp1, tmp6) = (d[1] + d[6], d[1] - d[6]);
    let (tmp2, tmp5) = (d[2] + d[5], d[2] - d[5]);
    let (tmp3, tmp4) = (d[3] + d[4], d[3] - d[4]);
    (
        [tmp0 + tmp3, tmp1 + tmp2, tmp0 - tmp3, tmp1 - tmp2],
        [tmp4, tmp5, tmp6, tmp7],
    )
}

/// The odd part of the transform: outputs 7, 5, 3 and 1, before descaling.
fn odd_part([tmp4, tmp5, tmp6, tmp7]: [i32; 4]) -> [i32; 4] {
    let (z1, z2, z3, z4) = (tmp4 + tmp7, tmp5 + tmp6, tmp4 + tmp6, tmp5 + tmp7);
    let z5 = (z3 + z4) * FIX_1_175875602;
    let (z1, z2) = (z1 * -FIX_0_899976223, z2 * -FIX_2_562915447);
    let (z3, z4) = (z3 * -FIX_1_961570560 + z5, z4 * -FIX_0_390180644 + z5);
    [
        tmp4 * FIX_0_298631336 + z1 + z3,
        tmp5 * FIX_2_053119869 + z2 + z4,
        tmp6 * FIX_3_072711026 + z2 + z3,
        tmp7 * FIX_1_501321110 + z1 + z4,
    ]
}

/// Quantise a transformed block into zig-zag order, rounding half away
/// from zero as the reference encoder does.
fn quantise(block: &[i32; 64], table: &[u16; 64], out: &mut [i32; 64]) {
    for (slot, &natural) in out.iter_mut().zip(&ZIGZAG) {
        let divisor = i32::from(table[natural]) << 3;
        let value = block[natural];
        let rounded = (value.abs() + divisor / 2) / divisor;
        *slot = if value < 0 { -rounded } else { rounded };
    }
}

/// One colour component of the frame.
struct Component {
    id: u8,
    /// Blocks across and down one MCU.
    sampling: (u8, u8),
    /// The quantisation and Huffman tables it takes: luminance or
    /// chrominance.
    table: u8,
}

/// The component set a picture is written with.
struct Frame {
    components: [Component; 3],
    /// How many of `components` it has: one for grey, three for colour.
    count: usize,
    /// Pixels across and down one MCU.
    mcu: (u32, u32),
}

impl Frame {
    fn new(grey: bool, full_chroma: bool) -> Self {
        let luma = if grey || full_chroma { (1, 1) } else { (2, 2) };
        Self {
            components: [
                Component {
                    id: 1,
                    sampling: luma,
                    table: 0,
                },
                Component {
                    id: 2,
                    sampling: (1, 1),
                    table: 1,
                },
                Component {
                    id: 3,
                    sampling: (1, 1),
                    table: 1,
                },
            ],
            count: if grey { 1 } else { 3 },
            mcu: (u32::from(luma.0) * 8, u32::from(luma.1) * 8),
        }
    }

    fn components(&self) -> &[Component] {
        &self.components[..self.count]
    }
}

pub(crate) fn encode(
    source: &dyn PictureSource,
    options: JpegOptions,
) -> Result<Vec<u8>, EncodeError> {
    let (width, height) = (source.width(), source.height());
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(EncodeError::TooLarge);
    }
    if let PictureKind::Indexed { depth, palette, .. } = source.kind() {
        palette_fits(depth, palette)?;
    }
    let mut reading = Reading {
        source,
        rows: RowBuffers::for_source(source)?,
        rgba: scratch(width as usize * RGBA_BYTES)?,
        background: options.background(),
    };
    let grey = reading.all_grey()?;
    let frame = Frame::new(grey, options.quality() >= FULL_CHROMA_QUALITY);
    let tables = [
        scaled_table(&LUMA_QUANT, options.quality()),
        scaled_table(&CHROMA_QUANT, options.quality()),
    ];
    let used_tables: u16 = if grey { 1 } else { 2 };

    let mut out = Output::new();
    out.push(&[0xFF, SOI, 0xFF, APP0])?;
    out.be_u16(JFIF_SEGMENT_LEN)?;
    out.push(&jfif_payload(source.density()))?;

    out.push(&[0xFF, DQT])?;
    out.be_u16(2 + 65 * used_tables)?;
    for (id, table) in (0u8..).zip(&tables).take(usize::from(used_tables)) {
        out.byte(id)?;
        for &natural in &ZIGZAG {
            out.byte(u8::try_from(table[natural]).unwrap_or(u8::MAX))?;
        }
    }

    out.push(&[0xFF, SOF0])?;
    let count = u8::try_from(frame.count).unwrap_or(1);
    out.be_u16(8 + 3 * u16::from(count))?;
    out.byte(8)?;
    out.be_u16(u16::try_from(height).map_err(|_| EncodeError::TooLarge)?)?;
    out.be_u16(u16::try_from(width).map_err(|_| EncodeError::TooLarge)?)?;
    out.byte(count)?;
    for component in frame.components() {
        let (h, v) = component.sampling;
        out.push(&[component.id, h << 4 | v, component.table])?;
    }

    let specs: [[&TableSpec; 2]; 2] = [[&DC_LUMA, &AC_LUMA], [&DC_CHROMA, &AC_CHROMA]];
    out.push(&[0xFF, DHT])?;
    let dht_len: usize = specs[..usize::from(used_tables)]
        .iter()
        .flatten()
        .map(|spec| 1 + MAX_CODE_BITS + spec.symbols.len())
        .sum();
    out.be_u16(u16::try_from(2 + dht_len).map_err(|_| EncodeError::TooLarge)?)?;
    for (id, pair) in (0u8..).zip(&specs).take(usize::from(used_tables)) {
        for (class, spec) in (0u8..).zip(pair) {
            out.byte(class << 4 | id)?;
            out.push(&spec.counts)?;
            out.push(spec.symbols)?;
        }
    }

    out.push(&[0xFF, SOS])?;
    out.be_u16(6 + 2 * u16::from(count))?;
    out.byte(count)?;
    for component in frame.components() {
        out.push(&[component.id, component.table << 4 | component.table])?;
    }
    out.push(&[0, 63, 0])?;

    let mut bits = Bits {
        out: &mut out,
        buffer: 0,
        count: 0,
    };
    write_scan(&mut reading, &frame, &tables, &mut bits)?;
    bits.finish()?;
    out.push(&[0xFF, EOI])?;
    Ok(out.into_bytes())
}

/// The picture being read, a row at a time, composited over the background.
struct Reading<'a> {
    source: &'a dyn PictureSource,
    rows: RowBuffers,
    rgba: Vec<u8>,
    background: [u8; 3],
}

impl Reading<'_> {
    /// Read row `y` into `rgba`, every pixel made opaque over the background.
    ///
    /// # Errors
    ///
    /// [`EncodeError::IndexOutOfRange`] for an index past the palette.
    fn row(&mut self, y: u32) -> Result<&[u8], EncodeError> {
        self.rows.read(self.source, y);
        let kind = self.source.kind();
        if let PictureKind::Indexed { palette, .. } = kind {
            indices_fit(&self.rows.samples, palette.len())?;
        }
        flatten_row(kind, &self.rows.samples, &self.rows.mask, &mut self.rgba);
        for pixel in self.rgba.as_chunks_mut::<RGBA_BYTES>().0 {
            let alpha = u32::from(pixel[3]);
            if alpha == u32::from(u8::MAX) {
                continue;
            }
            for (channel, &under) in pixel.iter_mut().zip(&self.background) {
                let mixed =
                    (u32::from(*channel) * alpha + u32::from(under) * (255 - alpha) + 127) / 255;
                *channel = u8::try_from(mixed).unwrap_or(u8::MAX);
            }
            pixel[3] = u8::MAX;
        }
        Ok(&self.rgba)
    }

    /// Whether every pixel, once composited, is grey.
    ///
    /// # Errors
    ///
    /// As [`row`](Self::row).
    fn all_grey(&mut self) -> Result<bool, EncodeError> {
        for y in 0..self.source.height() {
            let grey = self
                .row(y)?
                .as_chunks::<RGBA_BYTES>()
                .0
                .iter()
                .all(|pixel| pixel[0] == pixel[1] && pixel[1] == pixel[2]);
            if !grey {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// One pixel's JFIF luma and chroma.
fn ycbcr(pixel: [u8; RGBA_BYTES]) -> [u8; 3] {
    let [r, g, b] = [pixel[0], pixel[1], pixel[2]].map(i32::from);
    // A bias one short of a half keeps full blue or red at 255, not 256.
    let blue = (HALF * b - CB_R * r - CB_G * g + (128 << 16) + HALF - 1) >> 16;
    let red = (HALF * r - CR_G * g - CR_B * b + (128 << 16) + HALF - 1) >> 16;
    let [blue, red] = [blue, red].map(|value| u8::try_from(value.clamp(0, 255)).unwrap_or(0));
    [luma(pixel), blue, red]
}

/// One pixel's JFIF luma.
fn luma(pixel: [u8; RGBA_BYTES]) -> u8 {
    let [r, g, b] = [pixel[0], pixel[1], pixel[2]].map(i32::from);
    let luma = (Y_R * r + Y_G * g + Y_B * b + HALF) >> 16;
    u8::try_from(luma.clamp(0, 255)).unwrap_or(0)
}

/// Encode every MCU of the picture, a row of MCUs at a time.
fn write_scan(
    reading: &mut Reading<'_>,
    frame: &Frame,
    tables: &[[u16; 64]; 2],
    bits: &mut Bits<'_>,
) -> Result<(), EncodeError> {
    let (width, height) = (reading.source.width(), reading.source.height());
    let (mcu_w, mcu_h) = frame.mcu;
    let padded = width.div_ceil(mcu_w) * mcu_w;
    let stride = padded as usize;
    let plane_len = stride * mcu_h as usize;
    let grey = frame.count == 1;
    let chroma = |grey| {
        if grey {
            Ok(Vec::new())
        } else {
            scratch(plane_len)
        }
    };
    let mut planes = [scratch(plane_len)?, chroma(grey)?, chroma(grey)?];
    let mut last_dc = [0i32; 3];
    let mut block = [0i32; 64];
    let mut zigzag = [0i32; 64];
    let last_column = width as usize - 1;
    for band in 0..height.div_ceil(mcu_h) {
        for line in 0..mcu_h {
            // Rows and columns past the edge repeat the last ones, which is
            // what keeps the edge blocks free of a false step.
            let y = (band * mcu_h + line).min(height - 1);
            let pixels = reading.row(y)?.as_chunks::<RGBA_BYTES>().0;
            let at = line as usize * stride;
            for x in 0..stride {
                let pixel = pixels[x.min(last_column)];
                if grey {
                    planes[0][at + x] = luma(pixel);
                } else {
                    for (plane, value) in planes.iter_mut().zip(ycbcr(pixel)) {
                        plane[at + x] = value;
                    }
                }
            }
        }
        for mcu in 0..padded / mcu_w {
            for (index, component) in frame.components().iter().enumerate() {
                let (h, v) = (
                    u32::from(component.sampling.0),
                    u32::from(component.sampling.1),
                );
                let factor = (mcu_w / (h * 8), mcu_h / (v * 8));
                let table = usize::from(component.table);
                for by in 0..v {
                    for bx in 0..h {
                        let origin = (mcu * mcu_w + bx * 8 * factor.0, by * 8 * factor.1);
                        load_block(&planes[index], stride, origin, factor, &mut block);
                        forward_dct(&mut block);
                        quantise(&block, &tables[table], &mut zigzag);
                        code_block(&zigzag, &mut last_dc[index], &CODES[table], bits)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Load the level-shifted 8x8 block whose top-left sample is `origin`,
/// averaging `factor` samples each way for a subsampled component.
fn load_block(
    plane: &[u8],
    stride: usize,
    origin: (u32, u32),
    factor: (u32, u32),
    block: &mut [i32; 64],
) {
    let (fx, fy) = (factor.0 as usize, factor.1 as usize);
    let count = i32::try_from(fx * fy).unwrap_or(1);
    for (i, slot) in block.iter_mut().enumerate() {
        let (x, y) = (
            origin.0 as usize + (i % 8) * fx,
            origin.1 as usize + (i / 8) * fy,
        );
        let mut sum = 0i32;
        for dy in 0..fy {
            for dx in 0..fx {
                sum += i32::from(plane.get((y + dy) * stride + x + dx).copied().unwrap_or(0));
            }
        }
        *slot = (sum + count / 2) / count - 128;
    }
}

/// Entropy-code one quantised block, zig-zag ordered.
fn code_block(
    zigzag: &[i32; 64],
    last_dc: &mut i32,
    [dc, ac]: &[&Assigned; 2],
    bits: &mut Bits<'_>,
) -> Result<(), EncodeError> {
    let difference = zigzag[0] - *last_dc;
    *last_dc = zigzag[0];
    let size = category(difference);
    bits.code(dc, u8::try_from(size).unwrap_or(0))?;
    bits.put(magnitude_bits(difference), size)?;
    let mut run = 0u32;
    for &value in &zigzag[1..] {
        if value == 0 {
            run += 1;
            continue;
        }
        while run > 15 {
            bits.code(ac, ZERO_RUN)?;
            run -= 16;
        }
        let size = category(value);
        bits.code(ac, u8::try_from(run << 4 | size).unwrap_or(0))?;
        bits.put(magnitude_bits(value), size)?;
        run = 0;
    }
    if run > 0 {
        bits.code(ac, END_OF_BLOCK)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "jpeg_encode_tests.rs"]
mod tests;
