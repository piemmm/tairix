//! Unit tests over the committed faces.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use std::fs;
use std::path::PathBuf;

use crate::{CellGeometry, Coverage, CoverageError, Face, FontFamily, ATLAS_EM_PX};

/// The native cell height the atlas is authored at (ascent 13 + descent 3).
const NATIVE_HEIGHT: u32 = 16;

/// Read a committed face by its `<family>/<file>` path under
/// `lib/font/assets`.
pub(crate) fn asset(rel: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../font/assets")
        .join(rel);
    fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The count of inked (non-zero coverage) bytes in a bitmap.
pub(crate) fn ink(bitmap: &[u8]) -> usize {
    bitmap.iter().filter(|&&c| c > 0).count()
}

fn primary_bytes() -> Vec<u8> {
    asset("mono/Inconsolata-EX.ttf")
}

fn family_bytes() -> [Vec<u8>; 4] {
    [
        asset("mono/Inconsolata-EX.ttf"),
        asset("mono/MPLUS1Code-Regular.ttf"),
        asset("mono/D2Coding-Regular.ttf"),
        asset("mono/NotoSansHebrew-ExtraCondensed.ttf"),
    ]
}

fn family_from(bytes: &[Vec<u8>; 4]) -> FontFamily<'_> {
    FontFamily::parse(&[
        bytes[0].as_slice(),
        bytes[1].as_slice(),
        bytes[2].as_slice(),
        bytes[3].as_slice(),
    ])
    .expect("family parses")
}

/// The pixels-per-em and cell geometry the runtime uses to render a glyph at
/// `height` physical pixels, scaled linearly from the native reference.
fn runtime_geometry(face: &Face<'_>, advance: u16, height: u32) -> (CellGeometry, f64) {
    let native = CellGeometry::derive(face, advance, ATLAS_EM_PX).expect("native geometry");
    let width = (native.width * height).div_ceil(NATIVE_HEIGHT).max(1);
    let baseline = native.baseline * height / NATIVE_HEIGHT;
    let px_per_em = f64::from(ATLAS_EM_PX) * f64::from(height) / f64::from(NATIVE_HEIGHT);
    (
        CellGeometry {
            width,
            height,
            baseline,
        },
        px_per_em,
    )
}

#[test]
fn native_geometry_matches_the_atlas_cell() {
    let bytes = primary_bytes();
    let face = Face::parse(&bytes).expect("primary parses");
    let advance = face.uniform_advance().expect("monospace");
    let geometry = CellGeometry::derive(&face, advance, ATLAS_EM_PX).expect("geometry");
    assert_eq!(geometry.width, 8);
    assert_eq!(geometry.height, NATIVE_HEIGHT);
    assert_eq!(geometry.baseline, 13);
}

#[test]
fn ascii_resolves_to_the_primary_face_and_rasterises_with_ink() {
    let bytes = family_bytes();
    let family = family_from(&bytes);
    let advance = family.primary().uniform_advance().expect("monospace");
    let (index, glyph) = family.resolve(u32::from('A')).expect("A is covered");
    assert_eq!(index, 0, "ASCII comes from the primary face");

    let (geometry, px_per_em) = runtime_geometry(family.primary(), advance, NATIVE_HEIGHT);
    let bitmap = family
        .rasterise(index, glyph, &geometry, px_per_em, geometry.width * 2)
        .expect("rasterises");
    assert_eq!(
        bitmap.len(),
        (geometry.width * 2 * geometry.height) as usize
    );
    assert!(ink(&bitmap) > 0, "the letter A drew no ink");
    assert!(bitmap.iter().all(|&c| c <= 15), "coverage exceeds 4 bits");
}

#[test]
fn a_space_glyph_is_blank() {
    let bytes = family_bytes();
    let family = family_from(&bytes);
    let advance = family.primary().uniform_advance().expect("monospace");
    let (index, glyph) = family.resolve(u32::from(' ')).expect("space is covered");
    let (geometry, px_per_em) = runtime_geometry(family.primary(), advance, NATIVE_HEIGHT);
    let bitmap = family
        .rasterise(index, glyph, &geometry, px_per_em, geometry.width * 2)
        .expect("rasterises");
    assert_eq!(ink(&bitmap), 0, "space is not blank");
}

#[test]
fn a_larger_cell_rasterises_a_taller_glyph_directly_from_the_outline() {
    // The whole point of the engine: a bigger request rasterises a bigger
    // glyph from the outline, not an upscaled bitmap. Ink scales up with the
    // cell, and the bitmap is exactly the requested dimensions.
    let bytes = family_bytes();
    let family = family_from(&bytes);
    let advance = family.primary().uniform_advance().expect("monospace");
    let (index, glyph) = family.resolve(u32::from('B')).expect("B is covered");

    let render = |height: u32| {
        let (geometry, px_per_em) = runtime_geometry(family.primary(), advance, height);
        let bitmap = family
            .rasterise(index, glyph, &geometry, px_per_em, geometry.width * 2)
            .expect("rasterises");
        assert_eq!(bitmap.len(), (geometry.width * 2 * height) as usize);
        ink(&bitmap)
    };

    let small = render(14);
    let native = render(NATIVE_HEIGHT);
    let large = render(200);
    assert!(small > 0 && native > 0 && large > 0);
    assert!(
        native > small,
        "native glyph should have more ink than 14px"
    );
    assert!(
        large > native,
        "200px glyph should have more ink than native"
    );
}

#[test]
fn an_implausibly_wide_cell_is_refused_rather_than_allocated() {
    // A cell's width is not the engine's to choose: the service derives it
    // from the face's declared advance, so a face claiming an advance of a
    // hundred ems would otherwise have the sandboxed service allocate
    // hundreds of megabytes for one glyph. The tight proportional path
    // already refuses a runaway ink box; the cell path is held to the same
    // bound, and fails closed rather than trying.
    const IMPLAUSIBLE: u32 = 1 << 20;
    let bytes = primary_bytes();
    let face = Face::parse(&bytes).expect("primary parses");
    let advance = face.uniform_advance().expect("monospace");
    let geometry = CellGeometry::derive(&face, advance, ATLAS_EM_PX).expect("geometry");
    let glyph = face.glyph_for(u32::from('M')).expect("M is covered");
    assert!(
        face.rasterise_glyph(glyph, &geometry, f64::from(ATLAS_EM_PX), IMPLAUSIBLE)
            .is_err(),
        "a million-pixel cell was accepted"
    );
    assert!(
        face.rasterise_glyph(glyph, &geometry, f64::from(ATLAS_EM_PX), geometry.width)
            .is_ok(),
        "the face's own cell must still rasterise"
    );
}

#[test]
fn a_face_wider_than_the_cell_is_drawn_across_the_cells_it_covers() {
    // A monospace face is fitted so its advance lands on the cell, which is
    // right where the cell *is* its advance. A fallback face on someone
    // else's grid need not be: a full-width face on a half-width grid covers
    // two cells, and squeezing it into one would halve every glyph it lends.
    // The primary face at half its own cell stands in for that: 8.4 pixels of
    // advance over a 4-pixel cell is two cells, and the letter must be drawn
    // across both.
    let bytes = primary_bytes();
    let face = Face::parse(&bytes).expect("primary parses");
    let advance = face.uniform_advance().expect("monospace");
    let native = CellGeometry::derive(&face, advance, ATLAS_EM_PX).expect("geometry");
    let halved = CellGeometry {
        width: native.width / 2,
        ..native
    };
    let glyph = face.glyph_for(u32::from('M')).expect("M is covered");
    let bitmap = face
        .rasterise_glyph(glyph, &halved, f64::from(ATLAS_EM_PX), native.width)
        .expect("rasterises");
    let inked = |column: u32| {
        bitmap
            .chunks(native.width as usize)
            .any(|row| row[column as usize] > 0)
    };
    let rightmost = (0..native.width).rfind(|&column| inked(column));
    assert!(
        rightmost.is_some_and(|column| column > halved.width),
        "M was squeezed into one {}-pixel cell instead of the two it covers",
        halved.width
    );
}

#[test]
fn merged_repertoire_is_sorted_and_deduplicated() {
    let bytes = family_bytes();
    let family = family_from(&bytes);
    let merged = family.merged();
    assert!(!merged.is_empty());
    // Strictly ascending codepoints: earliest-face-wins leaves exactly one
    // entry per codepoint.
    assert!(
        merged.windows(2).all(|w| w[0].0 < w[1].0),
        "merged repertoire is not strictly ascending / deduplicated"
    );
    // ASCII 'A' comes from the primary face; its merged entry agrees with
    // resolution (earliest face wins) for every codepoint.
    let a = merged.iter().find(|&&(code, ..)| code == u32::from('A'));
    assert_eq!(a.map(|&(_, face, _)| face), Some(0));
    for &(code, face, glyph) in merged.iter().step_by(97) {
        assert_eq!(
            family.resolve(code),
            Some((face, glyph)),
            "merged entry disagrees with resolve at U+{code:04X}"
        );
    }
}

#[test]
fn garbage_fails_closed_rather_than_panicking() {
    assert!(Face::parse(&[]).is_err());
    assert!(Face::parse(&[0u8; 32]).is_err());
    assert!(FontFamily::parse(&[]).is_err());
}

/// Every committed face, by its path under `lib/font/assets`.
fn committed_faces() -> Vec<(String, Vec<u8>)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../font/assets");
    let mut faces = Vec::new();
    let mut dirs = vec![root];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).expect("assets list") {
            let path = entry.expect("asset entry").path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|ext| ext == "ttf") {
                let bytes = fs::read(&path).expect("asset reads");
                faces.push((path.display().to_string(), bytes));
            }
        }
    }
    assert!(faces.len() >= 4, "the committed faces are found");
    faces
}

/// A positioned read over `face` that records every span it was asked for.
fn recording_read<'f>(
    face: &'f [u8],
    spans: &'f core::cell::RefCell<Vec<(u64, usize)>>,
) -> impl FnMut(u64, &mut [u8]) -> Result<usize, ()> + 'f {
    move |offset, buf| {
        spans.borrow_mut().push((offset, buf.len()));
        let start = usize::try_from(offset).map_err(|_| ())?.min(face.len());
        let filled = buf.len().min(face.len() - start);
        buf[..filled].copy_from_slice(&face[start..start + filled]);
        Ok(filled)
    }
}

/// What a face can draw is read from its directory and `cmap` alone, and is
/// exactly what the parsed face maps: a scalar the coverage says no face
/// covers must never be one a face would have drawn.
#[test]
fn coverage_is_what_the_parsed_face_maps_read_from_the_cmap_alone() {
    for (name, bytes) in committed_faces() {
        let spans = core::cell::RefCell::new(Vec::new());
        let face_len = u64::try_from(bytes.len()).expect("small face");
        let coverage = Coverage::read(face_len, recording_read(&bytes, &spans))
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let face = Face::parse(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            coverage.count(),
            u64::try_from(face.mapped().len()).expect("count"),
            "{name}"
        );
        for &(code, _) in face.mapped() {
            assert!(coverage.covers(code), "{name}: U+{code:04X}");
        }
        assert!(!coverage.covers(0x10_FFFF), "{name}");

        let count = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
        let cmap_len = (0..count)
            .map(|i| 12 + 16 * i)
            .find(|&at| &bytes[at..at + 4] == b"cmap")
            .map(|at| u32::from_be_bytes(bytes[at + 12..at + 16].try_into().expect("u32")))
            .expect("a cmap record");
        let read: usize = spans.borrow().iter().map(|&(_, len)| len).sum();
        assert_eq!(
            read,
            12 + 16 * count + usize::try_from(cmap_len).expect("len"),
            "{name}: only the directory and the cmap are read"
        );
    }
}

#[test]
fn coverage_refuses_a_malformed_face_and_passes_a_read_failure_on() {
    let spans = core::cell::RefCell::new(Vec::new());
    for garbage in [&[][..], &[0u8; 32][..]] {
        let len = u64::try_from(garbage.len()).expect("len");
        assert!(matches!(
            Coverage::read(len, recording_read(garbage, &spans)),
            Err(CoverageError::Malformed(_))
        ));
    }

    // A directory naming a `cmap` far larger than the face: refused before
    // anything is allocated or read for it.
    let mut face = primary_bytes();
    let count = usize::from(u16::from_be_bytes([face[4], face[5]]));
    let cmap = (0..count)
        .map(|i| 12 + 16 * i)
        .find(|&at| &face[at..at + 4] == b"cmap")
        .expect("a cmap record");
    face[cmap + 12..cmap + 16].copy_from_slice(&u32::MAX.to_be_bytes());
    spans.borrow_mut().clear();
    let len = u64::try_from(face.len()).expect("len");
    assert!(matches!(
        Coverage::read(len, recording_read(&face, &spans)),
        Err(CoverageError::Malformed(_))
    ));
    assert_eq!(spans.borrow().len(), 2, "the header and the records only");

    let failing = |_: u64, _: &mut [u8]| Err::<usize, &str>("disk gone");
    assert_eq!(
        Coverage::read(len, failing),
        Err(CoverageError::Read("disk gone"))
    );
}

/// A face of one table, a `cmap` whose format-4 segments are `segments`, each
/// mapping its codes one glyph on.
fn face_with_segments(segments: &[(u16, u16)]) -> Vec<u8> {
    let count = u16::try_from(segments.len()).expect("few segments");
    let mut subtable = Vec::new();
    for word in [4, 0, 0, count * 2, 0, 0, 0] {
        subtable.extend_from_slice(&u16::to_be_bytes(word));
    }
    let column = |subtable: &mut Vec<u8>, word: &dyn Fn(u16, u16) -> u16| {
        for &(start, end) in segments {
            subtable.extend_from_slice(&word(start, end).to_be_bytes());
        }
    };
    column(&mut subtable, &|_, end| end);
    subtable.extend_from_slice(&[0, 0]);
    column(&mut subtable, &|start, _| start);
    column(&mut subtable, &|_, _| 1);
    column(&mut subtable, &|_, _| 0);
    let length = u16::try_from(subtable.len()).expect("a short subtable");
    subtable[2..4].copy_from_slice(&length.to_be_bytes());

    let mut cmap = Vec::new();
    for word in [0u16, 1, 3, 1] {
        cmap.extend_from_slice(&word.to_be_bytes());
    }
    cmap.extend_from_slice(&12u32.to_be_bytes());
    cmap.extend_from_slice(&subtable);

    let mut face = Vec::new();
    face.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    face.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]);
    face.extend_from_slice(b"cmap");
    face.extend_from_slice(&0u32.to_be_bytes());
    face.extend_from_slice(&28u32.to_be_bytes());
    face.extend_from_slice(&u32::try_from(cmap.len()).expect("len").to_be_bytes());
    face.extend_from_slice(&cmap);
    face
}

fn coverage_of(face: &[u8]) -> Result<Coverage, CoverageError<()>> {
    let spans = core::cell::RefCell::new(Vec::new());
    Coverage::read(
        u64::try_from(face.len()).expect("len"),
        recording_read(face, &spans),
    )
}

/// The lookup takes the first segment ending at or after a code, so a code
/// an earlier segment reaches is never a later one's.
#[test]
fn overlapping_cmap_segments_decode_as_the_lookup_reads_them() {
    let coverage =
        coverage_of(&face_with_segments(&[(10, 20), (15, 30), (0xFFFF, 0xFFFF)])).expect("read");
    assert!((10..=30).all(|code| coverage.covers(code)));
    assert!(!coverage.covers(9) && !coverage.covers(31));
    assert_eq!(coverage.count(), 21);

    let coverage =
        coverage_of(&face_with_segments(&[(10, 20), (5, 30), (0xFFFF, 0xFFFF)])).expect("read");
    assert!(
        !coverage.covers(7),
        "the first segment ending past it starts after it"
    );
    assert!((10..=30).all(|code| coverage.covers(code)));
}

/// Segments that do not ascend would each be decoded whole: twenty spanning
/// the plane are over a million entries from a table of a few hundred bytes.
#[test]
fn a_cmap_whose_segments_do_not_ascend_is_refused_without_decoding_them() {
    let segments = [(0, 0xFFFE); 20];
    assert!(matches!(
        coverage_of(&face_with_segments(&segments)),
        Err(CoverageError::Malformed(_))
    ));
}
