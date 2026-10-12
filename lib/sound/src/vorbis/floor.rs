//! Floors (Vorbis I, sections 6 and 7): the spectral envelope each channel's
//! residue is scaled by, as line segments on a decibel scale (floor 1) or as
//! a line spectral pair filter's response on the Bark scale (floor 0).

use alloc::vec::Vec;

use tairix_util::mathf::{atan, cos, exp, floor, sqrt};

use super::bits::{ilog, Bits, EndOfPacket};
use super::codebook::Codebook;
use crate::DecodeError;

/// The most posts a floor 1 lists, its two implicit ones included.
const MAX_POSTS: usize = 65;

/// The highest order a floor 0 filter takes.
const MAX_ORDER: usize = 255;

/// Floor 1's amplitude range for each multiplier.
const RANGES: [i32; 4] = [256, 128, 86, 64];

/// A floor's setup.
pub(super) enum Floor {
    Zero(Floor0),
    One(Floor1),
}

pub(super) struct Floor0 {
    order: usize,
    amplitude_bits: u32,
    amplitude_offset: u8,
    books: Vec<u8>,
    bark_map_size: u16,
    /// For each block size, the Bark band of each spectral line.
    maps: [Vec<u16>; 2],
}

#[derive(Copy, Clone)]
struct Class {
    dimensions: usize,
    subclass_bits: u32,
    masterbook: u8,
    /// Each subclass's book, if it codes values at all.
    subclass_books: [Option<u8>; 8],
}

pub(super) struct Floor1 {
    partition_classes: Vec<u8>,
    classes: Vec<Class>,
    multiplier: i32,
    x: Vec<u16>,
    /// For each post past the first two, the posts its value is predicted
    /// between.
    neighbours: Vec<(u8, u8)>,
    /// The posts in ascending order of position.
    sorted: Vec<u8>,
}

/// One channel's floor as one packet decoded it.
pub(super) struct Decoded {
    used: bool,
    amplitude: u64,
    lsp: [f32; MAX_ORDER],
    y: [i32; MAX_POSTS],
    step2: [bool; MAX_POSTS],
}

impl Default for Decoded {
    fn default() -> Self {
        Self {
            used: false,
            amplitude: 0,
            lsp: [0.0; MAX_ORDER],
            y: [0; MAX_POSTS],
            step2: [false; MAX_POSTS],
        }
    }
}

impl Decoded {
    pub(super) const fn used(&self) -> bool {
        self.used
    }
}

/// A book number from the setup, refused past the books it configures.
fn book(bits: &mut Bits<'_>, books: &[Codebook]) -> Result<u8, DecodeError> {
    let number = bits.read(8).map_err(|_| DecodeError::VorbisBadSetup)?;
    let number = u8::try_from(number).map_err(|_| DecodeError::VorbisBadSetup)?;
    if usize::from(number) < books.len() {
        Ok(number)
    } else {
        Err(DecodeError::VorbisBadSetup)
    }
}

/// Read a floor's setup, its type first; `blocksizes` the stream's two.
pub(super) fn read(
    bits: &mut Bits<'_>,
    books: &[Codebook],
    blocksizes: [usize; 2],
) -> Result<Floor, DecodeError> {
    match bits.read(16).map_err(|_| DecodeError::VorbisBadSetup)? {
        0 => read_zero(bits, books, blocksizes).map(Floor::Zero),
        1 => read_one(bits, books).map(Floor::One),
        _ => Err(DecodeError::VorbisBadSetup),
    }
}

fn read_zero(
    bits: &mut Bits<'_>,
    books: &[Codebook],
    blocksizes: [usize; 2],
) -> Result<Floor0, DecodeError> {
    let bad = |_| DecodeError::VorbisBadSetup;
    let order = bits.read(8).map_err(bad)? as usize;
    let rate = bits.read(16).map_err(bad)?;
    let bark_map_size =
        u16::try_from(bits.read(16).map_err(bad)?).map_err(|_| DecodeError::VorbisBadSetup)?;
    let amplitude_bits = bits.read(6).map_err(bad)?;
    let amplitude_offset =
        u8::try_from(bits.read(8).map_err(bad)?).map_err(|_| DecodeError::VorbisBadSetup)?;
    let count = bits.read(4).map_err(bad)? as usize + 1;
    // libvorbis refuses each of these, as no filter can be drawn from them.
    if order == 0 || rate == 0 || bark_map_size == 0 {
        return Err(DecodeError::VorbisBadSetup);
    }
    let mut list = Vec::new();
    if list.try_reserve_exact(count).is_err() {
        return Err(DecodeError::OutOfMemory);
    }
    for _ in 0..count {
        let number = book(bits, books)?;
        if !books[usize::from(number)].has_values() {
            return Err(DecodeError::VorbisBadSetup);
        }
        list.push(number);
    }
    let maps = [
        bark_map(blocksizes[0] / 2, rate, bark_map_size)?,
        bark_map(blocksizes[1] / 2, rate, bark_map_size)?,
    ];
    Ok(Floor0 {
        order,
        amplitude_bits,
        amplitude_offset,
        books: list,
        bark_map_size,
        maps,
    })
}

/// The Bark band of each of `lines` spectral lines, worked in the mixed
/// precision libvorbis works it in so a band edge falls where its does.
fn bark_map(lines: usize, rate: u32, size: u16) -> Result<Vec<u16>, DecodeError> {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a sample rate and a line index are far within single precision's integers"
    )]
    let as_single = |value: u32| value as f32;
    let bark = |x: f32| {
        f64::from(13.1f32) * atan(f64::from(0.000_74f32 * x))
            + f64::from(2.24f32) * atan(f64::from(x * x * 1.85e-8f32))
            + f64::from(1e-4f32 * x)
    };
    let half = as_single(rate) / 2.0;
    let scale = crate::single(f64::from(size) / bark(half));
    let lines_single = as_single(u32::try_from(lines).map_err(|_| DecodeError::VorbisBadSetup)?);
    let mut map = Vec::new();
    if map.try_reserve_exact(lines).is_err() {
        return Err(DecodeError::OutOfMemory);
    }
    for line in 0..lines {
        let x = half / lines_single
            * as_single(u32::try_from(line).map_err(|_| DecodeError::VorbisBadSetup)?);
        let band = floor(bark(x) * f64::from(scale));
        let band = if band >= f64::from(size) {
            f64::from(size - 1)
        } else {
            band.max(0.0)
        };
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the band is a whole number held within the map's size"
        )]
        map.push(band as u16);
    }
    Ok(map)
}

fn read_one(bits: &mut Bits<'_>, books: &[Codebook]) -> Result<Floor1, DecodeError> {
    let bad = |_| DecodeError::VorbisBadSetup;
    let partitions = bits.read(5).map_err(bad)? as usize;
    let mut partition_classes = Vec::new();
    if partition_classes.try_reserve_exact(partitions).is_err() {
        return Err(DecodeError::OutOfMemory);
    }
    for _ in 0..partitions {
        partition_classes.push(
            u8::try_from(bits.read(4).map_err(bad)?).map_err(|_| DecodeError::VorbisBadSetup)?,
        );
    }
    let class_count = partition_classes
        .iter()
        .max()
        .map_or(0, |&most| usize::from(most) + 1);
    let mut classes = Vec::new();
    if classes.try_reserve_exact(class_count).is_err() {
        return Err(DecodeError::OutOfMemory);
    }
    for _ in 0..class_count {
        let dimensions = bits.read(3).map_err(bad)? as usize + 1;
        let subclass_bits = bits.read(2).map_err(bad)?;
        let masterbook = if subclass_bits > 0 {
            book(bits, books)?
        } else {
            0
        };
        let mut subclass_books = [None; 8];
        for slot in subclass_books.iter_mut().take(1 << subclass_bits) {
            let number = bits.read(8).map_err(bad)?;
            *slot = match number.checked_sub(1) {
                None => None,
                Some(number) if (number as usize) < books.len() => {
                    Some(u8::try_from(number).map_err(|_| DecodeError::VorbisBadSetup)?)
                }
                Some(_) => return Err(DecodeError::VorbisBadSetup),
            };
        }
        classes.push(Class {
            dimensions,
            subclass_bits,
            masterbook,
            subclass_books,
        });
    }
    let multiplier =
        i32::try_from(bits.read(2).map_err(bad)? + 1).map_err(|_| DecodeError::VorbisBadSetup)?;
    let range_bits = bits.read(4).map_err(bad)?;
    let mut x: Vec<u16> = Vec::new();
    if x.try_reserve_exact(MAX_POSTS).is_err() {
        return Err(DecodeError::OutOfMemory);
    }
    x.push(0);
    x.push(u16::try_from(1u32 << range_bits).map_err(|_| DecodeError::VorbisBadSetup)?);
    for &class in &partition_classes {
        for _ in 0..classes[usize::from(class)].dimensions {
            if x.len() == MAX_POSTS {
                return Err(DecodeError::VorbisBadSetup);
            }
            x.push(
                u16::try_from(bits.read(range_bits).map_err(bad)?)
                    .map_err(|_| DecodeError::VorbisBadSetup)?,
            );
        }
    }
    let mut sorted: Vec<u8> = (0..x.len())
        .filter_map(|post| u8::try_from(post).ok())
        .collect();
    sorted.sort_unstable_by_key(|&post| x[usize::from(post)]);
    if sorted
        .windows(2)
        .any(|pair| x[usize::from(pair[0])] == x[usize::from(pair[1])])
    {
        return Err(DecodeError::VorbisBadSetup);
    }
    let neighbours = (2..x.len())
        .map(|post| neighbours_of(&x, post))
        .collect::<Option<Vec<_>>>()
        .ok_or(DecodeError::VorbisBadSetup)?;
    Ok(Floor1 {
        partition_classes,
        classes,
        multiplier,
        x,
        neighbours,
        sorted,
    })
}

/// The posts before `post` whose positions lie nearest below and above its
/// own (Vorbis I, section 9.2.4 and 9.2.5).
fn neighbours_of(x: &[u16], post: usize) -> Option<(u8, u8)> {
    let at = x[post];
    let below = (0..post)
        .filter(|&other| x[other] < at)
        .max_by_key(|&other| x[other])?;
    let above = (0..post)
        .filter(|&other| x[other] > at)
        .min_by_key(|&other| x[other])?;
    Some((u8::try_from(below).ok()?, u8::try_from(above).ok()?))
}

impl Floor {
    /// Decode a channel's floor from the packet into `out`. A floor the
    /// packet marks unused, or whose bits run out, is unused.
    pub(super) fn decode(&self, bits: &mut Bits<'_>, books: &[Codebook], out: &mut Decoded) {
        let decoded = match self {
            Self::Zero(floor) => floor.decode(bits, books, out),
            Self::One(floor) => floor.decode(bits, books, out),
        };
        out.used = decoded.unwrap_or(false);
    }

    /// Multiply the `lines` spectral lines of `spectrum` by the curve `decoded`
    /// states, a block of size index `size`; an unused floor silences them.
    pub(super) fn apply(&self, decoded: &Decoded, size: usize, spectrum: &mut [f32]) {
        if !decoded.used {
            spectrum.fill(0.0);
            return;
        }
        match self {
            Self::Zero(floor) => floor.apply(decoded, size, spectrum),
            Self::One(floor) => floor.apply(decoded, spectrum),
        }
    }
}

impl Floor0 {
    fn decode(
        &self,
        bits: &mut Bits<'_>,
        books: &[Codebook],
        out: &mut Decoded,
    ) -> Result<bool, EndOfPacket> {
        let low = bits.read(self.amplitude_bits.min(32))?;
        let high = bits.read(self.amplitude_bits.saturating_sub(32))?;
        let amplitude = u64::from(high) << 32 | u64::from(low);
        if amplitude == 0 {
            return Ok(false);
        }
        let count = u32::try_from(self.books.len()).map_err(|_| EndOfPacket)?;
        let number = bits.read(ilog(count))?;
        let Some(&book) = self.books.get(number as usize) else {
            return Ok(false);
        };
        let book = &books[usize::from(book)];
        let mut filled = 0;
        let mut last = 0.0;
        while filled < self.order {
            let vector = book.vector(bits)?;
            let take = vector.len().min(self.order - filled);
            for (slot, value) in out.lsp[filled..filled + take].iter_mut().zip(vector) {
                *slot = value + last;
            }
            filled += take;
            last = out.lsp[filled - 1];
        }
        out.amplitude = amplitude;
        Ok(true)
    }

    /// The filter's response at each line's band, as libvorbis evaluates it.
    fn apply(&self, decoded: &Decoded, size: usize, spectrum: &mut [f32]) {
        let map = &self.maps[size];
        let order = self.order;
        let mut lsp = [0.0f32; MAX_ORDER];
        for (to, &from) in lsp.iter_mut().zip(&decoded.lsp[..order]) {
            *to = crate::single(2.0 * cos(f64::from(from)));
        }
        let max = (1u64 << self.amplitude_bits) - 1;
        #[allow(
            clippy::cast_precision_loss,
            reason = "an amplitude of at most 63 bits is weighed as libvorbis weighs it, in single precision"
        )]
        let amplitude = decoded.amplitude as f32 / max as f32 * f32::from(self.amplitude_offset);
        let offset = f64::from(self.amplitude_offset);
        let step = crate::single(core::f64::consts::PI / f64::from(self.bark_map_size));
        let mut line = 0;
        while line < spectrum.len().min(map.len()) {
            let band = map[line];
            let w = crate::single(2.0 * cos(f64::from(step * f32::from(band))));
            let (mut p, mut q) = (0.5f32, 0.5f32);
            let mut j = 1;
            while j < order {
                q *= w - lsp[j - 1];
                p *= w - lsp[j];
                j += 2;
            }
            // Squared as libvorbis squares them, so the products round alike.
            if j == order {
                q *= w - lsp[j - 1];
                p = p * (p * (4.0 - w * w));
                q = q * q;
            } else {
                p = p * (p * (2.0 - w));
                q = q * (q * (2.0 + w));
            }
            let level = f64::from(amplitude) / sqrt(f64::from(p + q)) - offset;
            let value = crate::single(exp(level * f64::from(0.115_129_25f32)));
            while line < spectrum.len().min(map.len()) && map[line] == band {
                spectrum[line] *= value;
                line += 1;
            }
        }
    }
}

impl Floor1 {
    fn range(&self) -> i32 {
        RANGES[usize::try_from(self.multiplier - 1).unwrap_or(0)]
    }

    fn decode(
        &self,
        bits: &mut Bits<'_>,
        books: &[Codebook],
        out: &mut Decoded,
    ) -> Result<bool, EndOfPacket> {
        if !bits.flag()? {
            return Ok(false);
        }
        let width = ilog(u32::try_from(self.range() - 1).unwrap_or(0));
        out.y[0] = i32::try_from(bits.read(width)?).unwrap_or(0);
        out.y[1] = i32::try_from(bits.read(width)?).unwrap_or(0);
        let mut offset = 2;
        for &class in &self.partition_classes {
            let class = self.classes[usize::from(class)];
            let mask = (1 << class.subclass_bits) - 1;
            let mut choice = if class.subclass_bits > 0 {
                books[usize::from(class.masterbook)].decode(bits)?
            } else {
                0
            };
            for slot in &mut out.y[offset..offset + class.dimensions] {
                *slot = match class.subclass_books[(choice & mask) as usize] {
                    Some(book) => {
                        i32::try_from(books[usize::from(book)].decode(bits)?).unwrap_or(i32::MAX)
                    }
                    None => 0,
                };
                choice >>= class.subclass_bits;
            }
            offset += class.dimensions;
        }
        self.synthesise(out);
        Ok(true)
    }

    /// Turn the packet's wrapped differences into final amplitudes (Vorbis I,
    /// section 7.2.4, step 1), held within the range as the specification
    /// suggests so a setup abusing its books cannot index past the table.
    fn synthesise(&self, out: &mut Decoded) {
        let range = self.range();
        out.step2[0] = true;
        out.step2[1] = true;
        for post in 2..self.x.len() {
            let (low, high) = self.neighbours[post - 2];
            let (low, high) = (usize::from(low), usize::from(high));
            let predicted = render_point(
                i32::from(self.x[low]),
                out.y[low],
                i32::from(self.x[high]),
                out.y[high],
                i32::from(self.x[post]),
            );
            let value = out.y[post];
            let high_room = range - predicted;
            let low_room = predicted;
            let room = 2 * high_room.min(low_room);
            if value == 0 {
                out.step2[post] = false;
                out.y[post] = predicted;
                continue;
            }
            out.step2[low] = true;
            out.step2[high] = true;
            out.step2[post] = true;
            out.y[post] = if value >= room {
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
        for y in &mut out.y[..self.x.len()] {
            *y = (*y).clamp(0, range - 1);
        }
    }

    /// Draw the curve through the posts that carry a value and scale each
    /// line by it (Vorbis I, section 7.2.4, step 2).
    fn apply(&self, decoded: &Decoded, spectrum: &mut [f32]) {
        let lines = spectrum.len();
        let first = usize::from(self.sorted[0]);
        let (mut lx, mut ly) = (0usize, decoded.y[first] * self.multiplier);
        for &post in &self.sorted[1..] {
            let post = usize::from(post);
            if !decoded.step2[post] {
                continue;
            }
            let hx = usize::from(self.x[post]);
            let hy = decoded.y[post] * self.multiplier;
            render_line(lx, ly, hx, hy, spectrum);
            lx = hx;
            ly = hy;
            if lx >= lines {
                return;
            }
        }
        let level = FLOOR1_INVERSE_DB[usize::try_from(ly).unwrap_or(0).min(255)];
        for line in &mut spectrum[lx..] {
            *line *= level;
        }
    }
}

/// The amplitude at `x` on the line through the two points, rounded toward
/// the first (Vorbis I, section 9.2.6).
const fn render_point(x0: i32, y0: i32, x1: i32, y1: i32, x: i32) -> i32 {
    let dy = y1 - y0;
    let offset = dy.abs() * (x - x0) / (x1 - x0);
    if dy < 0 {
        y0 - offset
    } else {
        y0 + offset
    }
}

/// Scale `spectrum` from `x0` up to `x1` along the integer line between the
/// two amplitudes (Vorbis I, section 9.2.7); lines past its end are not
/// drawn.
fn render_line(x0: usize, y0: i32, x1: usize, y1: i32, spectrum: &mut [f32]) {
    let end = x1.min(spectrum.len());
    if x0 >= end {
        return;
    }
    let dy = y1 - y0;
    let adx = i32::try_from(x1 - x0).unwrap_or(i32::MAX);
    let base = dy / adx;
    let step = if dy < 0 { base - 1 } else { base + 1 };
    let rise = dy.abs() - base.abs() * adx;
    let mut y = y0;
    let mut error = 0;
    let level = |y: i32| FLOOR1_INVERSE_DB[usize::try_from(y).unwrap_or(0).min(255)];
    spectrum[x0] *= level(y);
    for line in &mut spectrum[x0 + 1..end] {
        error += rise;
        if error >= adx {
            error -= adx;
            y += step;
        } else {
            y += base;
        }
        *line *= level(y);
    }
}

/// Floor 1's amplitudes, from about -140 dB to unity (Vorbis I, section
/// 10.1).
#[allow(
    clippy::unreadable_literal,
    clippy::excessive_precision,
    reason = "the specification's own literals, kept digit for digit"
)]
pub(super) const FLOOR1_INVERSE_DB: [f32; 256] = [
    1.0649863e-07,
    1.1341951e-07,
    1.2079015e-07,
    1.2863978e-07,
    1.3699951e-07,
    1.4590251e-07,
    1.5538408e-07,
    1.6548181e-07,
    1.7623575e-07,
    1.8768855e-07,
    1.9988561e-07,
    2.128753e-07,
    2.2670913e-07,
    2.4144197e-07,
    2.5713223e-07,
    2.7384213e-07,
    2.9163793e-07,
    3.1059021e-07,
    3.3077411e-07,
    3.5226968e-07,
    3.7516214e-07,
    3.9954229e-07,
    4.2550680e-07,
    4.5315863e-07,
    4.8260743e-07,
    5.1396998e-07,
    5.4737065e-07,
    5.8294187e-07,
    6.2082472e-07,
    6.6116941e-07,
    7.0413592e-07,
    7.4989464e-07,
    7.9862701e-07,
    8.5052630e-07,
    9.0579828e-07,
    9.6466216e-07,
    1.0273513e-06,
    1.0941144e-06,
    1.1652161e-06,
    1.2409384e-06,
    1.3215816e-06,
    1.4074654e-06,
    1.4989305e-06,
    1.5963394e-06,
    1.7000785e-06,
    1.8105592e-06,
    1.9282195e-06,
    2.0535261e-06,
    2.1869758e-06,
    2.3290978e-06,
    2.4804557e-06,
    2.6416497e-06,
    2.8133190e-06,
    2.9961443e-06,
    3.1908506e-06,
    3.3982101e-06,
    3.6190449e-06,
    3.8542308e-06,
    4.1047004e-06,
    4.3714470e-06,
    4.6555282e-06,
    4.9580707e-06,
    5.2802740e-06,
    5.6234160e-06,
    5.9888572e-06,
    6.3780469e-06,
    6.7925283e-06,
    7.2339451e-06,
    7.7040476e-06,
    8.2047000e-06,
    8.7378876e-06,
    9.3057248e-06,
    9.9104632e-06,
    1.0554501e-05,
    1.1240392e-05,
    1.1970856e-05,
    1.2748789e-05,
    1.3577278e-05,
    1.4459606e-05,
    1.5399272e-05,
    1.6400004e-05,
    1.7465768e-05,
    1.8600792e-05,
    1.9809576e-05,
    2.1096914e-05,
    2.2467911e-05,
    2.3928002e-05,
    2.5482978e-05,
    2.7139006e-05,
    2.8902651e-05,
    3.0780908e-05,
    3.2781225e-05,
    3.4911534e-05,
    3.7180282e-05,
    3.9596466e-05,
    4.2169667e-05,
    4.4910090e-05,
    4.7828601e-05,
    5.0936773e-05,
    5.4246931e-05,
    5.7772202e-05,
    6.1526565e-05,
    6.5524908e-05,
    6.9783085e-05,
    7.4317983e-05,
    7.9147585e-05,
    8.4291040e-05,
    8.9768747e-05,
    9.5602426e-05,
    0.00010181521,
    0.00010843174,
    0.00011547824,
    0.00012298267,
    0.00013097477,
    0.00013948625,
    0.00014855085,
    0.00015820453,
    0.00016848555,
    0.00017943469,
    0.00019109536,
    0.00020351382,
    0.00021673929,
    0.00023082423,
    0.00024582449,
    0.00026179955,
    0.00027881276,
    0.00029693158,
    0.00031622787,
    0.00033677814,
    0.00035866388,
    0.00038197188,
    0.00040679456,
    0.00043323036,
    0.00046138411,
    0.00049136745,
    0.00052329927,
    0.00055730621,
    0.00059352311,
    0.00063209358,
    0.00067317058,
    0.00071691700,
    0.00076350630,
    0.00081312324,
    0.00086596457,
    0.00092223983,
    0.00098217216,
    0.0010459992,
    0.0011139742,
    0.0011863665,
    0.0012634633,
    0.0013455702,
    0.0014330129,
    0.0015261382,
    0.0016253153,
    0.0017309374,
    0.0018434235,
    0.0019632195,
    0.0020908006,
    0.0022266726,
    0.0023713743,
    0.0025254795,
    0.0026895994,
    0.0028643847,
    0.0030505286,
    0.0032487691,
    0.0034598925,
    0.0036847358,
    0.0039241906,
    0.0041792066,
    0.0044507950,
    0.0047400328,
    0.0050480668,
    0.0053761186,
    0.0057254891,
    0.0060975636,
    0.0064938176,
    0.0069158225,
    0.0073652516,
    0.0078438871,
    0.0083536271,
    0.0088964928,
    0.009474637,
    0.010090352,
    0.010746080,
    0.011444421,
    0.012188144,
    0.012980198,
    0.013823725,
    0.014722068,
    0.015678791,
    0.016697687,
    0.017782797,
    0.018938423,
    0.020169149,
    0.021479854,
    0.022875735,
    0.024362330,
    0.025945531,
    0.027631618,
    0.029427276,
    0.031339626,
    0.033376252,
    0.035545228,
    0.037855157,
    0.040315199,
    0.042935108,
    0.045725273,
    0.048696758,
    0.051861348,
    0.055231591,
    0.058820850,
    0.062643361,
    0.066714279,
    0.071049749,
    0.075666962,
    0.080584227,
    0.085821044,
    0.091398179,
    0.097337747,
    0.10366330,
    0.11039993,
    0.11757434,
    0.12521498,
    0.13335215,
    0.14201813,
    0.15124727,
    0.16107617,
    0.17154380,
    0.18269168,
    0.19456402,
    0.20720788,
    0.22067342,
    0.23501402,
    0.25028656,
    0.26655159,
    0.28387361,
    0.30232132,
    0.32196786,
    0.34289114,
    0.36517414,
    0.38890521,
    0.41417847,
    0.44109412,
    0.46975890,
    0.50028648,
    0.53279791,
    0.56742212,
    0.60429640,
    0.64356699,
    0.68538959,
    0.72993007,
    0.77736504,
    0.82788260,
    0.88168307,
    0.9389798,
    1.0,
];

#[cfg(test)]
mod tests {
    use super::{render_line, render_point, FLOOR1_INVERSE_DB};

    #[test]
    fn the_table_runs_from_minus_140_db_to_unity_in_equal_steps() {
        assert_eq!(FLOOR1_INVERSE_DB[255].to_bits(), 1.0f32.to_bits());
        for pair in FLOOR1_INVERSE_DB.windows(2) {
            let ratio = f64::from(pair[1]) / f64::from(pair[0]);
            assert!((ratio - 1.064_986_3).abs() < 1e-5, "{ratio}");
        }
    }

    #[test]
    fn render_point_rounds_toward_the_first_point() {
        assert_eq!(render_point(0, 10, 10, 20, 5), 15);
        assert_eq!(render_point(0, 10, 3, 20, 1), 13);
        assert_eq!(render_point(0, 20, 3, 10, 1), 17);
    }

    /// The integer line steps by whole values and carries its remainder, so
    /// it lands on its end amplitude exactly.
    #[test]
    fn render_line_steps_by_its_remainder_and_lands_on_its_end() {
        let mut spectrum = [1.0f32; 9];
        render_line(0, 100, 8, 103, &mut spectrum);
        let indices: alloc::vec::Vec<usize> = spectrum
            .iter()
            .map(|value| {
                FLOOR1_INVERSE_DB
                    .iter()
                    .position(|level| level.to_bits() == value.to_bits())
                    .expect("a table value")
            })
            .collect();
        assert_eq!(indices[..8], [100, 100, 100, 101, 101, 101, 102, 102]);
        assert_eq!(
            spectrum[8].to_bits(),
            1.0f32.to_bits(),
            "the end line is the next segment's"
        );
    }
}
