//! A TIFF file's header and directory chain, as the structure report lists
//! them: each page's tags by name, the EXIF and position directories they
//! point to after them.

use crate::exif::{self, ChainNames, Directory};
use tairix_util::report::Builder;

/// Report `bytes`, a TIFF file.
pub(crate) fn report(bytes: &[u8], out: &mut Builder) {
    let Some((endian, first)) = exif::header(bytes) else {
        if bytes
            .get(2..4)
            .is_some_and(|version| version == [0, 43] || version == [43, 0])
        {
            out.stop(0, "a BigTIFF file, which this report does not read");
        } else {
            out.stop(0, "no TIFF header");
        }
        return;
    };
    out.heading(format_args!("TIFF image, {} bytes", bytes.len()));
    out.record(
        0,
        "header",
        8,
        format_args!(
            "{} byte order, first directory at {first:#x}",
            exif::order_name(endian)
        ),
    );
    exif::walk(
        bytes,
        endian,
        (Directory::Image, first),
        0,
        &mut ChainNames::Pages(0),
        out,
    );
}
