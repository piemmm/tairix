//! A ZIP archive's members, as the structure report lists them.

use super::{Archive, DEFLATED, STORED};
use tairix_util::report::{clean, Builder};

/// List `bytes`' members, read through the archive's central directory.
pub(crate) fn members(bytes: &[u8], out: &mut Builder) {
    let Ok(archive) = Archive::open(bytes) else {
        out.stop(0, "the archive's directory does not hold together");
        return;
    };
    for entry in &archive.entries {
        let method = match entry.method {
            STORED => "stored",
            DEFLATED => "deflated",
            _ => "an unknown method",
        };
        let name = archive.name(entry);
        if !out.record(
            usize::try_from(entry.local).unwrap_or(usize::MAX),
            "member",
            usize::try_from(entry.packed).unwrap_or(usize::MAX),
            format_args!(
                "\u{201c}{}\u{201d}, {method}, {} bytes, CRC {:08x}",
                clean(name),
                entry.size,
                entry.crc
            ),
        ) {
            return;
        }
    }
}
