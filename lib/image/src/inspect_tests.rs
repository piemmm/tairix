use alloc::vec::Vec;

use tairix_util::report::{Report, MAX_LINE_BYTES, MAX_REPORT_LINES};

use super::inspect;
use crate::encode_fixture::rgba;
use crate::{
    encode_bmp, encode_gif, encode_jpeg, encode_png, encode_tiff, GifOptions, ImageFormat,
    JpegOptions, TiffOptions,
};

fn texts(report: &Report) -> Vec<&str> {
    report
        .lines()
        .iter()
        .map(|line| line.text.as_str())
        .collect()
}

/// Whether some line of `report` holds `needle`.
fn says(report: &Report, needle: &str) -> bool {
    report.lines().iter().any(|line| line.text.contains(needle))
}

fn picture() -> crate::Picture {
    rgba(5, 3, |x, y| {
        [
            u8::try_from(x * 40).expect("small"),
            u8::try_from(y * 70).expect("small"),
            90,
            255,
        ]
    })
}

/// A 4 by 2 picture of four colours, which GIF can hold.
fn indexed() -> crate::Picture {
    let palette = alloc::vec![
        [0, 0, 0, 255],
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255]
    ];
    let indices = (0..8_u8).map(|at| at % 4).collect();
    crate::Picture::indexed(4, 2, crate::IndexDepth::Two, palette, indices, None).expect("valid")
}

/// An `APP1` segment carrying `tiff` as its EXIF block.
fn app1(tiff: &[u8]) -> Vec<u8> {
    let length = u16::try_from(2 + 6 + tiff.len()).expect("a small block");
    let mut segment = alloc::vec![0xFF, 0xE1];
    segment.extend_from_slice(&length.to_be_bytes());
    segment.extend_from_slice(b"Exif\0\0");
    segment.extend_from_slice(tiff);
    segment
}

#[test]
fn a_png_lists_its_header_its_image_data_and_its_end() {
    let file = encode_png(&picture()).expect("encodes");
    let report = inspect(&file, ImageFormat::Png);
    let lines = texts(&report);
    assert!(lines[0].starts_with("PNG image"), "{lines:?}");
    assert!(says(&report, "IHDR"), "{lines:?}");
    assert!(says(&report, "header: 5 \u{d7} 3"), "{lines:?}");
    assert!(says(&report, "image data, 1 chunk"), "{lines:?}");
    assert!(
        lines.last().is_some_and(|line| line.contains("image end")),
        "{lines:?}"
    );
    assert!(!says(&report, "Stopped"), "{lines:?}");
}

#[test]
fn a_png_chunk_whose_crc_fails_is_said_to_and_a_cut_chunk_stops_the_walk() {
    let mut file = encode_png(&picture()).expect("encodes");
    // The IHDR CRC's last byte.
    file[8 + 8 + 13 + 3] ^= 0xFF;
    let report = inspect(&file, ImageFormat::Png);
    assert!(
        says(&report, "its CRC does not match"),
        "{:?}",
        texts(&report)
    );

    let cut = &file[..40];
    let report = inspect(cut, ImageFormat::Png);
    assert!(
        report
            .lines()
            .last()
            .is_some_and(|line| line.heading && line.text.starts_with("Stopped at")),
        "{:?}",
        texts(&report)
    );
}

#[test]
fn a_jpeg_lists_its_segments_in_order_and_reads_its_exif() {
    let file =
        encode_jpeg(&picture(), JpegOptions::new(90, [0; 3]).expect("a quality")).expect("encodes");
    let report = inspect(&file, ImageFormat::Jpeg);
    let lines = texts(&report);
    let order: Vec<usize> = ["SOI", "APP0", "DQT", "SOF0", "DHT", "SOS", "data", "EOI"]
        .iter()
        .map(|name| {
            lines
                .iter()
                .position(|line| line.get(10..).is_some_and(|rest| rest.starts_with(name)))
                .unwrap_or_else(|| panic!("{name} is listed: {lines:?}"))
        })
        .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{lines:?}");
    assert!(says(&report, "baseline, 8-bit, 5 \u{d7} 3"), "{lines:?}");

    let mut with_exif = file[..2].to_vec();
    with_exif.extend_from_slice(&app1(&crate::exif::tests::camera_block()));
    with_exif.extend_from_slice(&file[2..]);
    let report = inspect(&with_exif, ImageFormat::Jpeg);
    assert!(
        says(&report, "Make: \u{201c}Acme\u{201d}"),
        "{:?}",
        texts(&report)
    );
    assert!(
        says(&report, "ExposureTime: 1/125 s"),
        "{:?}",
        texts(&report)
    );
}

#[test]
fn every_format_heads_its_report_and_walks_a_whole_file_to_the_end() {
    let picture = picture();
    let files = [
        (
            ImageFormat::Gif,
            encode_gif(&indexed(), GifOptions::default()).expect("gif"),
            "GIF image",
        ),
        (
            ImageFormat::Bmp,
            encode_bmp(&picture).expect("bmp"),
            "BMP image",
        ),
        (
            ImageFormat::Tiff,
            encode_tiff(&[&picture], TiffOptions::default()).expect("tiff"),
            "TIFF image",
        ),
    ];
    for (format, file, heading) in files {
        let report = inspect(&file, format);
        assert!(report.lines()[0].heading, "{format:?}");
        assert!(report.lines()[0].text.starts_with(heading), "{format:?}");
        assert!(
            !says(&report, "Stopped"),
            "{format:?}: {:?}",
            texts(&report)
        );
    }
    let gif = inspect(
        &encode_gif(&indexed(), GifOptions::default()).expect("gif"),
        ImageFormat::Gif,
    );
    assert!(
        says(&gif, "frame 1: 4 \u{d7} 2 at (0, 0)"),
        "{:?}",
        texts(&gif)
    );
    assert!(says(&gif, "end of the file, 1 frames"), "{:?}", texts(&gif));
    let tiff = inspect(
        &encode_tiff(&[&picture], TiffOptions::default()).expect("tiff"),
        ImageFormat::Tiff,
    );
    assert!(says(&tiff, "Page 1 directory"), "{:?}", texts(&tiff));
    assert!(says(&tiff, "ImageWidth: 5 pixels"), "{:?}", texts(&tiff));
}

/// A lossless WEBP of one 2 by 2 picture, its bitstream header and no more.
fn webp() -> Vec<u8> {
    let bits: u32 = 1 | (1 << 14);
    let mut bitstream = alloc::vec![0x2F];
    bitstream.extend_from_slice(&bits.to_le_bytes());
    let mut chunk = b"VP8L".to_vec();
    chunk.extend_from_slice(&u32::try_from(bitstream.len()).expect("small").to_le_bytes());
    chunk.extend_from_slice(&bitstream);
    chunk.push(0);
    let mut file = b"RIFF".to_vec();
    file.extend_from_slice(&u32::try_from(4 + chunk.len()).expect("small").to_le_bytes());
    file.extend_from_slice(b"WEBP");
    file.extend_from_slice(&chunk);
    file
}

#[test]
fn a_webp_lists_its_chunks() {
    let report = inspect(&webp(), ImageFormat::Webp);
    assert!(
        says(&report, "lossless bitstream, 2 \u{d7} 2"),
        "{:?}",
        texts(&report)
    );
}

/// Inspecting any prefix of a file, or the file with any byte damaged, ends
/// in a bounded report and never a panic: the bytes are a hostile party's.
#[test]
fn a_damaged_or_cut_file_is_reported_within_bounds_in_every_format() {
    let picture = picture();
    let mut files = alloc::vec![
        (ImageFormat::Png, encode_png(&picture).expect("png")),
        (
            ImageFormat::Jpeg,
            encode_jpeg(&picture, JpegOptions::new(50, [0; 3]).expect("q")).expect("jpeg"),
        ),
        (
            ImageFormat::Gif,
            encode_gif(&indexed(), GifOptions::default()).expect("gif")
        ),
        (ImageFormat::Bmp, encode_bmp(&picture).expect("bmp")),
        (
            ImageFormat::Tiff,
            encode_tiff(&[&picture], TiffOptions::default()).expect("tiff"),
        ),
        (ImageFormat::Webp, webp()),
    ];
    files.push((
        ImageFormat::Ico,
        alloc::vec![0, 0, 1, 0, 2, 0, 16, 16, 0, 0, 1, 0, 32, 0],
    ));
    files.push((
        ImageFormat::Sprite,
        alloc::vec![1, 0, 0, 0, 16, 0, 0, 0, 0xFF, 0, 0, 0],
    ));
    files.push((ImageFormat::OpenRaster, b"PK\x03\x04garbage".to_vec()));
    for (format, file) in &files {
        for end in 0..=file.len() {
            within_bounds(&inspect(&file[..end], *format));
        }
        for at in 0..file.len() {
            let mut damaged = file.clone();
            damaged[at] ^= 0xA5;
            within_bounds(&inspect(&damaged, *format));
        }
    }
}

fn within_bounds(report: &Report) {
    assert!(report.lines().len() <= MAX_REPORT_LINES);
    for line in report.lines() {
        assert!(line.text.len() <= MAX_LINE_BYTES, "{:?}", line.text);
        assert!(!line.text.chars().any(char::is_control), "{:?}", line.text);
    }
}
