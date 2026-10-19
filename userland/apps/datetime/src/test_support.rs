//! The fixture zone store the crate's tests read, kept for the process as
//! the loaded one is.

use tairix_tz::ZoneInfo;

/// The fixture store of real zones and regions.
pub(crate) fn zones() -> &'static ZoneInfo<'static> {
    let bytes: &'static [u8] =
        alloc::boxed::Box::leak(tairix_tz::testkit::store().into_boxed_slice());
    alloc::boxed::Box::leak(alloc::boxed::Box::new(
        ZoneInfo::parse(bytes).expect("the fixture parses"),
    ))
}
