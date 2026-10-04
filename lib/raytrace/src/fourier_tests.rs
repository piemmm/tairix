//! Host tests of the Fourier transform: it is the discrete transform written
//! out, it undoes itself, keeps the energy, and comes out the same on any
//! runner; and a grid's columns transform as its transpose's rows.

use alloc::vec::Vec;

use tairix_parallel::Threaded;

use super::*;
use crate::sample::{mix32, unit};

fn values(length: usize, salt: u32) -> Vec<Complex> {
    (0..length)
        .map(|index| {
            let key = mix32(u32::try_from(index).expect("a small index") ^ salt);
            Complex::new(2.0 * unit(key) - 1.0, 2.0 * unit(mix32(key ^ 7)) - 1.0)
        })
        .collect()
}

/// The transform written out as its sum.
fn naive(values: &[Complex]) -> Vec<Complex> {
    let length = values.len();
    (0..length)
        .map(|k| {
            values
                .iter()
                .enumerate()
                .fold(Complex::ZERO, |sum, (j, value)| {
                    let angle = -TAU * real(j * k % length) / real(length);
                    sum + *value * Complex::new(mathf::cos(angle), mathf::sin(angle))
                })
        })
        .collect()
}

fn close(one: &[Complex], other: &[Complex], tolerance: f64) -> bool {
    one.len() == other.len()
        && one
            .iter()
            .zip(other)
            .all(|(a, b)| (a.re - b.re).abs() <= tolerance && (a.im - b.im).abs() <= tolerance)
}

#[test]
fn a_transform_is_the_discrete_transform_written_out() {
    for length in [1, 2, 4, 8, 64, 256] {
        let fourier = Fourier::new(length).expect("a transform");
        let input = values(length, 3);
        let mut output = input.clone();
        fourier.forward(&mut output).expect("the length it takes");
        assert!(
            close(&output, &naive(&input), 1e-9 * real(length)),
            "length {length}"
        );
    }
}

#[test]
fn a_transform_undoes_itself_and_keeps_its_energy() {
    for length in [2, 16, 1024] {
        let fourier = Fourier::new(length).expect("a transform");
        let input = values(length, 5);
        let mut output = input.clone();
        fourier.forward(&mut output).expect("the length it takes");
        let energy = |values: &[Complex]| {
            values
                .iter()
                .map(|value| value.re * value.re + value.im * value.im)
                .sum::<f64>()
        };
        // Parseval: the unscaled transform carries the length's times the energy.
        let ratio = energy(&output) / (real(length) * energy(&input));
        assert!((ratio - 1.0).abs() < 1e-12, "length {length}: {ratio}");
        fourier.inverse(&mut output).expect("the length it takes");
        assert!(close(&output, &input, 1e-12), "length {length}");
    }
}

#[test]
fn a_tone_lands_in_its_own_bin() {
    let length = 64;
    let fourier = Fourier::new(length).expect("a transform");
    for cycles in [1usize, 5, 31, 40] {
        let mut tone: Vec<Complex> = (0..length)
            .map(|j| {
                let angle = TAU * real(j * cycles) / real(length);
                Complex::new(mathf::cos(angle), mathf::sin(angle))
            })
            .collect();
        fourier.forward(&mut tone).expect("the length it takes");
        for (bin, value) in tone.iter().enumerate() {
            let expected = if bin == cycles { real(length) } else { 0.0 };
            assert!((value.re - expected).abs() < 1e-9 && value.im.abs() < 1e-9);
        }
        assert!((frequency(cycles, length) - real(cycles)).abs() < 1e-12 || cycles >= 32);
    }
    assert!((frequency(40, 64) + 24.0).abs() < 1e-12);
    assert!((frequency(31, 64) - 31.0).abs() < 1e-12);
}

#[test]
fn a_transform_refuses_a_length_not_its_own() {
    assert!(Fourier::new(0).is_none());
    assert!(Fourier::new(12).is_none());
    let fourier = Fourier::new(8).expect("a transform");
    let mut short = values(4, 1);
    let before = short.clone();
    assert!(fourier.forward(&mut short).is_none());
    assert_eq!(short, before);
}

/// A grid's rows transformed, transposed, and its transpose's rows
/// transformed is the grid's transform in two dimensions, written out; and
/// the same bit for bit on one thread as across several.
#[test]
fn a_grid_transforms_by_its_rows_and_its_transposes_rows() {
    let (rows, columns) = (8, 16);
    let grid = values(rows * columns, 9);
    let by_rows = Fourier::new(columns).expect("a transform");
    let by_columns = Fourier::new(rows).expect("a transform");
    let run = |runner: &dyn JobRunner| {
        let mut along = grid.clone();
        super::rows(&by_rows, &mut along, (3, false), runner).expect("whole rows");
        let mut turned = alloc::vec![Complex::ZERO; rows * columns];
        transposed(&along, (rows, columns), &mut turned, (0, 5), runner).expect("a transpose");
        super::rows(&by_columns, &mut turned, (2, false), runner).expect("whole rows");
        turned
    };
    let alone = run(&tairix_parallel::SERIAL);
    assert_eq!(alone, run(&Threaded::new(4)));
    for k_row in 0..rows {
        for k_column in 0..columns {
            let expected = grid
                .iter()
                .enumerate()
                .fold(Complex::ZERO, |sum, (at, value)| {
                    let (row, column) = (at / columns, at % columns);
                    let angle = -TAU
                        * (real(row * k_row) / real(rows)
                            + real(column * k_column) / real(columns));
                    sum + *value * Complex::new(mathf::cos(angle), mathf::sin(angle))
                });
            let got = alone[k_column * rows + k_row];
            assert!(
                (got.re - expected.re).abs() < 1e-9 && (got.im - expected.im).abs() < 1e-9,
                "({k_row}, {k_column})"
            );
        }
    }
}

/// A transpose written a part at a time is the transpose written whole, and
/// refuses grids that are not the size they are said to be.
#[test]
fn a_transpose_written_in_parts_is_the_transpose_written_whole() {
    let (rows, columns) = (6, 10);
    let grid = values(rows * columns, 2);
    let mut whole = alloc::vec![Complex::ZERO; rows * columns];
    transposed(
        &grid,
        (rows, columns),
        &mut whole,
        (0, 4),
        &tairix_parallel::SERIAL,
    )
    .expect("a transpose");
    let mut parts = alloc::vec![Complex::ZERO; rows * columns];
    for (part, first) in parts.chunks_mut(3 * rows).zip((0..).step_by(3)) {
        transposed(&grid, (rows, columns), part, (first, 2), &Threaded::new(3))
            .expect("a part of the transpose");
    }
    assert_eq!(whole, parts);
    assert_eq!(whole[3 * rows + 2], grid[2 * columns + 3]);
    let mut too_many = alloc::vec![Complex::ZERO; 2 * rows];
    assert!(transposed(
        &grid,
        (rows, columns),
        &mut too_many,
        (9, 1),
        &tairix_parallel::SERIAL
    )
    .is_none());
}
