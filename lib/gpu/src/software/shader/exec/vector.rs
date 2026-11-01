//! Operations over whole vectors and matrices, packings, operations with two
//! results, bit fields, and components chosen by a lane's own index.
//!
//! Floats are accumulated in doubles and rounded once to their width.

use super::{Lanes, Machine};
use crate::software::shader::ir::{BitField, Linear, Num, Pack, Reg, Wide, LANES};
use crate::software::shader::ops;
use tairix_spirv::{MAX_MATRIX_COLUMNS, MAX_VECTOR_COMPONENTS as MAX_VECTOR};
use tairix_util::mathf;

/// The most components a matrix the admitted set declares holds.
const MAX_MATRIX: usize = MAX_VECTOR * MAX_MATRIX_COLUMNS;

/// One component of a value as floats, one per lane.
type Component = [f64; LANES];

impl Machine<'_, '_, '_> {
    /// Component `index` of `reg` as floats of `num`, one per lane.
    fn floats(&self, reg: Reg, num: Num, index: u32, out: &mut [f64; LANES]) {
        let mut bits = [0; LANES];
        let words = num.words();
        self.fetch(reg, index * words, words == 2, &mut bits);
        for lane in self.span.clone() {
            if let (Some(o), Some(&b)) = (out.get_mut(lane), bits.get(lane)) {
                *o = ops::float(num, b);
            }
        }
    }

    /// Write `values`, floats of `num`, as component `index` of `dst`.
    fn put_floats(&mut self, dst: Reg, num: Num, index: u32, values: &[f64; LANES]) {
        let mut bits = [0; LANES];
        for lane in self.span.clone() {
            if let (Some(b), Some(&v)) = (bits.get_mut(lane), values.get(lane)) {
                *b = ops::unfloat(num, v);
            }
        }
        let words = num.words();
        self.store(dst, index * words, words == 2, &bits);
    }

    /// The first `count` components of `reg` as floats of `num`, each
    /// fetched once however often an operation reads it, room for `ROWS`:
    /// a vector's for a vector, a matrix's only for a matrix.
    fn components<const ROWS: usize>(&self, reg: Reg, num: Num, count: u32) -> [Component; ROWS] {
        let mut all = [[0.0; LANES]; ROWS];
        for (component, lanes) in (0..count).zip(all.iter_mut()) {
            self.floats(reg, num, component, lanes);
        }
        all
    }

    /// The dot product of the first `n` components of `a` and `b`, a
    /// component of each read at a time.
    fn dot(&self, num: Num, (a, b): (Reg, Reg), n: u32) -> [f64; LANES] {
        let (mut first, mut second, mut sum) = ([0.0; LANES], [0.0; LANES], [0.0; LANES]);
        for component in 0..n {
            self.floats(a, num, component, &mut first);
            self.floats(b, num, component, &mut second);
            for lane in self.span.clone() {
                if let (Some(total), Some(&x), Some(&y)) =
                    (sum.get_mut(lane), first.get(lane), second.get(lane))
                {
                    *total += x * y;
                }
            }
        }
        sum
    }

    /// The sum over `terms` of the product of the components of `a` and `b`
    /// that `at` names for each term, lane by lane.
    fn products(
        &self,
        (a, b): (&[Component], &[Component]),
        terms: u32,
        at: impl Fn(u32) -> (u32, u32),
    ) -> [f64; LANES] {
        let mut sum = [0.0; LANES];
        for term in 0..terms {
            let (from_a, from_b) = at(term);
            let (Some(first), Some(second)) = (a.get(from_a as usize), b.get(from_b as usize))
            else {
                continue;
            };
            for lane in self.span.clone() {
                if let (Some(total), Some(&x), Some(&y)) =
                    (sum.get_mut(lane), first.get(lane), second.get(lane))
                {
                    *total += x * y;
                }
            }
        }
        sum
    }

    pub(super) fn linear(&mut self, op: Linear, num: Num, dst: Reg, (a, b, c): (Reg, Reg, Reg)) {
        match op {
            Linear::Scale { count } => self.scale(num, dst, (a, b), count),
            Linear::Dot { n } => {
                let sum = self.dot(num, (a, b), n);
                self.put_floats(dst, num, 0, &sum);
            }
            Linear::MatrixVector { rows, cols } => {
                let (left, right) = (
                    self.components::<MAX_MATRIX>(a, num, rows * cols),
                    self.components::<MAX_VECTOR>(b, num, cols),
                );
                for row in 0..rows {
                    let sum = self.products((&left, &right), cols, |col| (col * rows + row, col));
                    self.put_floats(dst, num, row, &sum);
                }
            }
            Linear::VectorMatrix { rows, cols } => {
                let (left, right) = (
                    self.components::<MAX_VECTOR>(a, num, rows),
                    self.components::<MAX_MATRIX>(b, num, rows * cols),
                );
                for col in 0..cols {
                    let sum = self.products((&left, &right), rows, |row| (row, col * rows + row));
                    self.put_floats(dst, num, col, &sum);
                }
            }
            Linear::MatrixMatrix { rows, inner, cols } => {
                let left = self.components::<MAX_MATRIX>(a, num, rows * inner);
                let right = self.components::<MAX_MATRIX>(b, num, inner * cols);
                for col in 0..cols {
                    for row in 0..rows {
                        let sum = self.products((&left, &right), inner, |term| {
                            (term * rows + row, col * inner + term)
                        });
                        self.put_floats(dst, num, col * rows + row, &sum);
                    }
                }
            }
            Linear::Outer { rows, cols } => {
                let (left, right) = (
                    self.components::<MAX_VECTOR>(a, num, rows),
                    self.components::<MAX_VECTOR>(b, num, cols),
                );
                for col in 0..cols {
                    for row in 0..rows {
                        let product = self.products((&left, &right), 1, |_| (row, col));
                        self.put_floats(dst, num, col * rows + row, &product);
                    }
                }
            }
            Linear::Transpose { rows, cols } => self.transpose(num, dst, a, (rows, cols)),
            Linear::Determinant { n } => {
                let matrices = self.square(a, num, n);
                let mut out = [0.0; LANES];
                for lane in self.span.clone() {
                    if let (Some(slot), Some(matrix)) = (out.get_mut(lane), matrices.get(lane)) {
                        *slot = determinant(matrix, n as usize);
                    }
                }
                self.put_floats(dst, num, 0, &out);
            }
            Linear::Inverse { n } => self.invert(num, dst, a, n),
            Linear::Length { n } => {
                let sum = self.dot(num, (a, a), n);
                self.put_floats(dst, num, 0, &sum.map(mathf::sqrt));
            }
            Linear::Distance { n } => self.distance(num, dst, (a, b), n),
            Linear::Normalize { n } => self.normalize(num, dst, a, n),
            Linear::Cross => {
                let (left, right) = (
                    self.components::<MAX_VECTOR>(a, num, 3),
                    self.components::<MAX_VECTOR>(b, num, 3),
                );
                for (component, (first, second)) in (0u32..).zip([(1, 2), (2, 0), (0, 1)]) {
                    let plus = self.products((&left, &right), 1, |_| (first, second));
                    let minus = self.products((&left, &right), 1, |_| (second, first));
                    let mut out = [0.0; LANES];
                    for lane in self.span.clone() {
                        if let (Some(slot), Some(&up), Some(&down)) =
                            (out.get_mut(lane), plus.get(lane), minus.get(lane))
                        {
                            *slot = up - down;
                        }
                    }
                    self.put_floats(dst, num, component, &out);
                }
            }
            Linear::FaceForward { n } => self.face_forward(num, dst, (a, b, c), n),
            Linear::Reflect { n } => self.reflect(num, dst, (a, b), n),
            Linear::Refract { n, eta } => self.refract((num, eta), dst, (a, b, c), n),
        }
    }

    /// The `count` components of `a` each times the scalar `b`.
    fn scale(&mut self, num: Num, dst: Reg, (a, b): (Reg, Reg), count: u32) {
        let (mut values, mut factor) = ([0.0; LANES], [0.0; LANES]);
        self.floats(b, num, 0, &mut factor);
        for component in 0..count {
            self.floats(a, num, component, &mut values);
            for lane in self.span.clone() {
                if let (Some(value), Some(&by)) = (values.get_mut(lane), factor.get(lane)) {
                    *value *= by;
                }
            }
            self.put_floats(dst, num, component, &values);
        }
    }

    /// The transpose of the `rows`-tall, `cols`-wide matrix `a`.
    fn transpose(&mut self, num: Num, dst: Reg, a: Reg, (rows, cols): (u32, u32)) {
        let words = num.words();
        let mut values = [0; LANES];
        for col in 0..cols {
            for row in 0..rows {
                for word in 0..words {
                    self.fetch(a, (col * rows + row) * words + word, false, &mut values);
                    self.store(dst, (row * cols + col) * words + word, false, &values);
                }
            }
        }
    }

    /// The inverse of the square matrix of side `n` `a` holds.
    fn invert(&mut self, num: Num, dst: Reg, a: Reg, n: u32) {
        let matrices = self.square(a, num, n);
        let mut inverses = [[0.0; MAX_MATRIX]; LANES];
        for lane in self.span.clone() {
            if let (Some(slot), Some(matrix)) = (inverses.get_mut(lane), matrices.get(lane)) {
                *slot = inverse(matrix, n as usize);
            }
        }
        for (component, index) in (0u32..).zip(0..(n as usize * n as usize).min(MAX_MATRIX)) {
            let mut values = [0.0; LANES];
            for lane in self.span.clone() {
                if let (Some(slot), Some(inverted)) = (values.get_mut(lane), inverses.get(lane)) {
                    *slot = inverted.get(index).copied().unwrap_or(0.0);
                }
            }
            self.put_floats(dst, num, component, &values);
        }
    }

    fn distance(&mut self, num: Num, dst: Reg, (a, b): (Reg, Reg), n: u32) {
        let (mut first, mut second, mut sum) = ([0.0; LANES], [0.0; LANES], [0.0; LANES]);
        for component in 0..n {
            self.floats(a, num, component, &mut first);
            self.floats(b, num, component, &mut second);
            for lane in self.span.clone() {
                if let (Some(total), Some(&x), Some(&y)) =
                    (sum.get_mut(lane), first.get(lane), second.get(lane))
                {
                    *total += (x - y) * (x - y);
                }
            }
        }
        self.put_floats(dst, num, 0, &sum.map(mathf::sqrt));
    }

    fn normalize(&mut self, num: Num, dst: Reg, a: Reg, n: u32) {
        let x = self.components::<MAX_VECTOR>(a, num, n);
        let sum = self.products((&x, &x), n, |term| (term, term));
        for (component, read) in (0..n).zip(&x) {
            let mut values = *read;
            for lane in self.span.clone() {
                if let (Some(value), Some(&squares)) = (values.get_mut(lane), sum.get(lane)) {
                    *value /= mathf::sqrt(squares);
                }
            }
            self.put_floats(dst, num, component, &values);
        }
    }

    /// `a` negated where `c` faces along `b`: `FaceForward(N, I, Nref)`.
    fn face_forward(&mut self, num: Num, dst: Reg, (a, b, c): (Reg, Reg, Reg), n: u32) {
        let facing = self.dot(num, (c, b), n);
        let mut values = [0.0; LANES];
        for component in 0..n {
            self.floats(a, num, component, &mut values);
            for lane in self.span.clone() {
                if let (Some(value), Some(&along)) = (values.get_mut(lane), facing.get(lane)) {
                    if along >= 0.0 {
                        *value = -*value;
                    }
                }
            }
            self.put_floats(dst, num, component, &values);
        }
    }

    /// `a` reflected about the normal `b`.
    fn reflect(&mut self, num: Num, dst: Reg, (a, b): (Reg, Reg), n: u32) {
        let (incidents, normals) = (
            self.components::<MAX_VECTOR>(a, num, n),
            self.components::<MAX_VECTOR>(b, num, n),
        );
        let along = self.products((&normals, &incidents), n, |term| (term, term));
        for (component, (read, normal)) in (0..n).zip(incidents.iter().zip(&normals)) {
            let mut incident = *read;
            for lane in self.span.clone() {
                if let (Some(value), Some(&towards), Some(&dot)) =
                    (incident.get_mut(lane), normal.get(lane), along.get(lane))
                {
                    *value -= 2.0 * dot * towards;
                }
            }
            self.put_floats(dst, num, component, &incident);
        }
    }

    /// `a` refracted through the normal `b` at the ratio `c`, a scalar of
    /// `eta`; zero past the critical angle.
    fn refract(&mut self, (num, eta): (Num, Num), dst: Reg, (a, b, c): (Reg, Reg, Reg), n: u32) {
        let (incidents, normals) = (
            self.components::<MAX_VECTOR>(a, num, n),
            self.components::<MAX_VECTOR>(b, num, n),
        );
        let along = self.products((&normals, &incidents), n, |term| (term, term));
        let mut ratio = [0.0; LANES];
        self.floats(c, eta, 0, &mut ratio);
        for (component, (read, normal)) in (0..n).zip(incidents.iter().zip(&normals)) {
            let mut incident = *read;
            for lane in self.span.clone() {
                let (Some(value), Some(&towards), Some(&dot), Some(&index)) = (
                    incident.get_mut(lane),
                    normal.get(lane),
                    along.get(lane),
                    ratio.get(lane),
                ) else {
                    continue;
                };
                let under_root = 1.0 - index * index * (1.0 - dot * dot);
                *value = if under_root < 0.0 {
                    0.0
                } else {
                    index * *value - (index * dot + mathf::sqrt(under_root)) * towards
                };
            }
            self.put_floats(dst, num, component, &incident);
        }
    }

    /// The square matrix of side `n` `a` holds, column-major, lane by lane.
    fn square(&self, a: Reg, num: Num, n: u32) -> [[f64; MAX_MATRIX]; LANES] {
        let mut matrices = [[0.0; MAX_MATRIX]; LANES];
        let mut values = [0.0; LANES];
        for (component, index) in (0u32..).zip(0..(n as usize * n as usize).min(MAX_MATRIX)) {
            self.floats(a, num, component, &mut values);
            for lane in self.span.clone() {
                if let (Some(slot), Some(&value)) = (
                    matrices.get_mut(lane).and_then(|m| m.get_mut(index)),
                    values.get(lane),
                ) {
                    *slot = value;
                }
            }
        }
        matrices
    }

    pub(super) fn pack(&mut self, op: Pack, dst: Reg, a: Reg) {
        let mut parts = [[0u64; LANES]; 4];
        let inputs = match op {
            Pack::Snorm4x8 | Pack::Unorm4x8 => 4,
            Pack::Snorm2x16 | Pack::Unorm2x16 | Pack::Half2x16 => 2,
            Pack::Halves { count } => count.min(4),
            Pack::UnpackHalves { count } => count.div_ceil(2).min(4),
            _ => 1,
        };
        for (word, lanes) in (0..inputs).zip(parts.iter_mut()) {
            self.fetch(a, word, false, lanes);
        }
        let outputs = match op {
            Pack::UnpackSnorm4x8 | Pack::UnpackUnorm4x8 => 4,
            Pack::UnpackSnorm2x16 | Pack::UnpackUnorm2x16 | Pack::UnpackHalf2x16 => 2,
            Pack::Halves { count } => count.div_ceil(2).min(4),
            Pack::UnpackHalves { count } => count.min(4),
            _ => 1,
        };
        let mut out = [[0u64; LANES]; 4];
        for lane in self.span.clone() {
            let input: [u64; 4] = core::array::from_fn(|i| parts[i][lane]);
            let packed = pack_lane(op, input);
            for (o, &value) in out.iter_mut().zip(&packed) {
                if let Some(slot) = o.get_mut(lane) {
                    *slot = value;
                }
            }
        }
        for (word, lanes) in (0..outputs).zip(&out) {
            self.store(dst, word, false, lanes);
        }
    }

    pub(super) fn wide(
        &mut self,
        op: Wide,
        num: Num,
        count: u32,
        (dst, second): (Reg, Reg),
        (a, b): (Reg, Reg),
    ) {
        let words = num.words();
        let wide = words == 2;
        let second_words = if op == Wide::Frexp { 1 } else { words };
        let (mut left, mut right) = ([0; LANES], [0; LANES]);
        let (mut first, mut other) = ([0; LANES], [0; LANES]);
        for component in 0..count {
            self.fetch(a, component * words, wide, &mut left);
            self.fetch(b, component * words, wide, &mut right);
            for lane in self.span.clone() {
                let (Some(&x), Some(&y)) = (left.get(lane), right.get(lane)) else {
                    continue;
                };
                let (one, two) = wide_lane(op, num, x, y);
                if let (Some(slot), Some(second_slot)) = (first.get_mut(lane), other.get_mut(lane))
                {
                    *slot = one;
                    *second_slot = two;
                }
            }
            self.store(dst, component * words, wide, &first);
            self.store(second, component * second_words, second_words == 2, &other);
        }
    }

    pub(super) fn bit_field(
        &mut self,
        op: BitField,
        (num, by): (Num, Num),
        count: u32,
        dst: Reg,
        (base, insert, offset, bits): (Reg, Reg, Reg, Reg),
    ) {
        let words = num.words();
        let wide = words == 2;
        let (mut at, mut span) = ([0; LANES], [0; LANES]);
        self.fetch(offset, 0, by.words() == 2, &mut at);
        self.fetch(bits, 0, by.words() == 2, &mut span);
        let (mut x, mut y) = ([0; LANES], [0; LANES]);
        for component in 0..count {
            self.fetch(base, component * words, wide, &mut x);
            if op == BitField::Insert {
                self.fetch(insert, component * words, wide, &mut y);
            }
            for lane in self.span.clone() {
                let (Some(xv), Some(&yv), Some(&o), Some(&c)) =
                    (x.get_mut(lane), y.get(lane), at.get(lane), span.get(lane))
                else {
                    continue;
                };
                let (o, c) = (o & ops::mask(by.bits()), c & ops::mask(by.bits()));
                *xv = match op {
                    BitField::Insert => ops::bit_insert(num, *xv, yv, o, c),
                    BitField::SExtract => ops::bit_extract(num, *xv, o, c, true),
                    BitField::UExtract => ops::bit_extract(num, *xv, o, c, false),
                };
            }
            self.store(dst, component * words, wide, &x);
        }
    }

    pub(super) fn ldexp(
        &mut self,
        (num, by): (Num, Num),
        count: u32,
        dst: Reg,
        (a, b): (Reg, Reg),
    ) {
        let (words, by_words) = (num.words(), by.words());
        let (mut x, mut y) = ([0; LANES], [0; LANES]);
        for component in 0..count {
            self.fetch(a, component * words, words == 2, &mut x);
            self.fetch(b, component * by_words, by_words == 2, &mut y);
            for lane in self.span.clone() {
                if let (Some(xv), Some(&yv)) = (x.get_mut(lane), y.get(lane)) {
                    *xv = ops::ldexp(num, *xv, ops::signed(yv, by.bits()));
                }
            }
            self.store(dst, component * words, words == 2, &x);
        }
    }

    /// Each lane's component of its own index, zero past the end.
    pub(super) fn extract(
        &mut self,
        dst: Reg,
        vector: Reg,
        (index, by): (Reg, Num),
        count: u32,
        per: u32,
    ) {
        let indices = self.indices(index, by);
        let mut part = [0; LANES];
        for word in 0..per {
            let mut out = [0; LANES];
            for component in 0..count {
                self.fetch(vector, component * per + word, false, &mut part);
                for lane in self.span.clone() {
                    if indices.get(lane) == Some(&u64::from(component)) {
                        if let (Some(o), Some(&p)) = (out.get_mut(lane), part.get(lane)) {
                            *o = p;
                        }
                    }
                }
            }
            self.store(dst, word, false, &out);
        }
    }

    /// `vector` with each lane's component of its own index replaced,
    /// unchanged past the end.
    pub(super) fn insert(
        &mut self,
        dst: Reg,
        (vector, value): (Reg, Reg),
        (index, by): (Reg, Num),
        count: u32,
        per: u32,
    ) {
        let indices = self.indices(index, by);
        let (mut part, mut new) = ([0; LANES], [0; LANES]);
        for word in 0..per {
            self.fetch(value, word, false, &mut new);
            for component in 0..count {
                self.fetch(vector, component * per + word, false, &mut part);
                for lane in self.span.clone() {
                    if indices.get(lane) == Some(&u64::from(component)) {
                        if let (Some(p), Some(&n)) = (part.get_mut(lane), new.get(lane)) {
                            *p = n;
                        }
                    }
                }
                self.store(dst, component * per + word, false, &part);
            }
        }
    }

    /// Each lane's index, as an unsigned value of its width: a negative one
    /// is past every end.
    fn indices(&self, index: Reg, by: Num) -> Lanes {
        let mut values = [0; LANES];
        self.fetch(index, 0, by.words() == 2, &mut values);
        for lane in self.span.clone() {
            if let Some(v) = values.get_mut(lane) {
                *v &= ops::mask(by.bits());
            }
        }
        values
    }
}

fn wide_lane(op: Wide, num: Num, x: u64, y: u64) -> (u64, u64) {
    let bits = num.bits();
    let mask = ops::mask(bits);
    match op {
        Wide::AddCarry => {
            let sum = u128::from(x & mask) + u128::from(y & mask);
            (low(sum) & mask, u64::from(sum >> bits != 0))
        }
        Wide::SubBorrow => {
            let (x, y) = (x & mask, y & mask);
            (x.wrapping_sub(y) & mask, u64::from(y > x))
        }
        Wide::UMulExtended => {
            let product = u128::from(x & mask) * u128::from(y & mask);
            (low(product) & mask, low(product >> bits) & mask)
        }
        Wide::SMulExtended => {
            let product = i128::from(ops::signed(x, bits)) * i128::from(ops::signed(y, bits));
            let raw = product.cast_unsigned();
            (low(raw) & mask, low(raw >> bits) & mask)
        }
        Wide::Modf => ops::modf(num, x),
        Wide::Frexp => {
            let (significand, exponent) = ops::frexp(num, x);
            (significand, exponent & ops::mask(32))
        }
    }
}

/// A packed code of at most sixteen bits, as a double.
fn code_value(code: i64) -> f64 {
    i32::try_from(code).map_or(0.0, f64::from)
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "the low 64 bits of a 128-bit value are asked for"
)]
const fn low(v: u128) -> u64 {
    v as u64
}

fn pack_lane(op: Pack, input: [u64; 4]) -> [u64; 4] {
    let single = |bits: u64| f64::from(f32::from_bits(super::low32(bits)));
    let as_single = |value: f64| u64::from(ops::single_bits(value));
    let norm = |value: f64, low: f64, scale: f64| mathf::round(value.clamp(low, 1.0) * scale);
    let mut out = [0; 4];
    match op {
        Pack::Snorm4x8 | Pack::Unorm4x8 => {
            let mut word = 0u64;
            for (k, &component) in input.iter().enumerate() {
                let code = if op == Pack::Snorm4x8 {
                    signed_code(norm(single(component), -1.0, 127.0)) & 0xff
                } else {
                    unsigned_code(norm(single(component), 0.0, 255.0)) & 0xff
                };
                word |= code << (8 * k);
            }
            out[0] = word;
        }
        Pack::Snorm2x16 | Pack::Unorm2x16 => {
            let mut word = 0u64;
            for (k, &component) in input.iter().take(2).enumerate() {
                let code = if op == Pack::Snorm2x16 {
                    signed_code(norm(single(component), -1.0, 32767.0)) & 0xffff
                } else {
                    unsigned_code(norm(single(component), 0.0, 65535.0)) & 0xffff
                };
                word |= code << (16 * k);
            }
            out[0] = word;
        }
        Pack::Half2x16 => {
            let half = |bits: u64| u64::from(mathf::to_half(f32::from_bits(super::low32(bits))));
            out[0] = half(input[0]) | half(input[1]) << 16;
        }
        Pack::UnpackSnorm4x8 | Pack::UnpackUnorm4x8 => {
            for (k, slot) in out.iter_mut().enumerate() {
                let byte = input[0] >> (8 * k) & 0xff;
                *slot = if op == Pack::UnpackSnorm4x8 {
                    as_single((code_value(ops::signed(byte, 8)) / 127.0).clamp(-1.0, 1.0))
                } else {
                    as_single(code_value(byte.cast_signed()) / 255.0)
                };
            }
        }
        Pack::UnpackSnorm2x16 | Pack::UnpackUnorm2x16 => {
            for (k, slot) in out.iter_mut().take(2).enumerate() {
                let half = input[0] >> (16 * k) & 0xffff;
                *slot = if op == Pack::UnpackSnorm2x16 {
                    as_single((code_value(ops::signed(half, 16)) / 32767.0).clamp(-1.0, 1.0))
                } else {
                    as_single(code_value(half.cast_signed()) / 65535.0)
                };
            }
        }
        Pack::UnpackHalf2x16 => {
            for (k, slot) in out.iter_mut().take(2).enumerate() {
                let half = u16::try_from(input[0] >> (16 * k) & 0xffff).unwrap_or(0);
                *slot = u64::from(mathf::from_half(half).to_bits());
            }
        }
        Pack::Halves { count } => {
            for (k, slot) in out.iter_mut().enumerate().take(count.div_ceil(2) as usize) {
                let low_half = input.get(2 * k).copied().unwrap_or(0) & 0xffff;
                let high_half = input.get(2 * k + 1).copied().unwrap_or(0) & 0xffff;
                *slot = low_half | high_half << 16;
            }
        }
        Pack::UnpackHalves { count } => {
            for (k, slot) in out.iter_mut().enumerate().take(count as usize) {
                *slot = input.get(k / 2).copied().unwrap_or(0) >> (16 * (k % 2)) & 0xffff;
            }
        }
    }
    out
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "the value was rounded and clamped into range"
)]
fn signed_code(value: f64) -> u64 {
    (value as i64).cast_unsigned()
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value was rounded and clamped into range"
)]
fn unsigned_code(value: f64) -> u64 {
    value as u64
}

/// The determinant of the column-major square matrix `m` of side `n`, by
/// cofactor expansion along its first column.
fn determinant(m: &[f64; MAX_MATRIX], n: usize) -> f64 {
    let at = |column: usize, row: usize| m.get(column * n + row).copied().unwrap_or(0.0);
    match n {
        2 => at(0, 0) * at(1, 1) - at(1, 0) * at(0, 1),
        3 => {
            at(0, 0) * (at(1, 1) * at(2, 2) - at(2, 1) * at(1, 2))
                - at(1, 0) * (at(0, 1) * at(2, 2) - at(2, 1) * at(0, 2))
                + at(2, 0) * (at(0, 1) * at(1, 2) - at(1, 1) * at(0, 2))
        }
        4 => (0..4)
            .map(|column| {
                let minor = minor(m, 4, column, 0);
                let sign = if column % 2 == 0 { 1.0 } else { -1.0 };
                sign * at(column, 0) * determinant(&minor, 3)
            })
            .sum(),
        _ => 0.0,
    }
}

/// The matrix of side `n - 1` left once column `skip_column` and row
/// `skip_row` are taken out.
fn minor(
    m: &[f64; MAX_MATRIX],
    n: usize,
    skip_column: usize,
    skip_row: usize,
) -> [f64; MAX_MATRIX] {
    let mut out = [0.0; MAX_MATRIX];
    let mut k = 0;
    for column in (0..n).filter(|&c| c != skip_column) {
        for row in (0..n).filter(|&r| r != skip_row) {
            if let (Some(slot), Some(&v)) = (out.get_mut(k), m.get(column * n + row)) {
                *slot = v;
            }
            k += 1;
        }
    }
    out
}

/// The inverse of the column-major square matrix `m` of side `n`: its
/// adjugate over its determinant, which a singular matrix makes infinite.
fn inverse(m: &[f64; MAX_MATRIX], n: usize) -> [f64; MAX_MATRIX] {
    let det = determinant(m, n);
    let mut out = [0.0; MAX_MATRIX];
    for column in 0..n {
        for row in 0..n {
            let cofactor = if n == 2 {
                let at = |c: usize, r: usize| m.get(c * n + r).copied().unwrap_or(0.0);
                at(1 - row, 1 - column)
            } else {
                determinant(&minor(m, n, row, column), n - 1)
            };
            let sign = if (row + column) % 2 == 0 { 1.0 } else { -1.0 };
            if let Some(slot) = out.get_mut(column * n + row) {
                *slot = sign * cofactor / det;
            }
        }
    }
    out
}
