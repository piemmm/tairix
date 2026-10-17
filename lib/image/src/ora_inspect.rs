//! An OpenRaster document's archive, as the structure report lists it.

use crate::inspect::Builder;

/// Report `bytes`, an OpenRaster document.
pub(crate) fn report(bytes: &[u8], out: &mut Builder) {
    out.heading(format_args!(
        "OpenRaster document, {} bytes: a ZIP archive",
        bytes.len()
    ));
    crate::zip::inspect::members(bytes, out);
}
