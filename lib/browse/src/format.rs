//! Display formatting for the item view's metadata columns.
//!
//! The list view shows each entry's byte [`size`](crate::Entry::size) and its
//! last-modification [`Time64`] alongside its name. Turning those raw figures
//! into the short strings a column shows is presentation, shared by the file
//! manager and the trusted picker so the two render a listing identically —
//! it lives here once rather than being re-derived per view.
//!
//! This is the *file-listing* convention (binary size units, an ISO calendar
//! date), deliberately distinct from the `top`/`sysinfo` figure spellings in
//! `lib/procinfo`: a directory listing reads by calendar date, not by an
//! uptime-relative figure. The calendar is [`CivilTime`]'s and the minute
//! spelling `tairix_fsmeta`'s, so a listing reads a date as every other surface
//! does. Every function is pure and total: every input produces a string, and
//! no path panics.

use alloc::format;
use alloc::string::String;

use tairix_abi::time::{CivilTime, Time64};
use tairix_fsmeta::calendar::iso_minute;

/// Render a byte count for the item view's size column in binary units:
/// `0 B`, `512 B`, `1.0 KiB`, `1.5 MiB`, `2.3 GiB`, … up to `PiB`.
///
/// Below one kibibyte the exact byte count is shown (`742 B`); at or above it
/// the value is rendered in the largest unit for which it is at least one,
/// with a single decimal place. The tenths are computed in `u128` so even a
/// multi-exbibyte size (the huge-volume floor the charter designs for) scales
/// without overflow rather than wrapping.
#[must_use]
pub fn format_size(bytes: u64) -> String {
    const UNIT: u128 = 1024;
    /// The scaled-unit letters, indexed by how many times `bytes` was divided
    /// by [`UNIT`] beyond the byte: `0` → KiB, `1` → MiB, …
    const LETTERS: [char; 5] = ['K', 'M', 'G', 'T', 'P'];

    if u128::from(bytes) < UNIT {
        return format!("{bytes} B");
    }

    let mut idx = 0usize;
    let mut divisor = UNIT;
    while u128::from(bytes) >= divisor.saturating_mul(UNIT) && idx + 1 < LETTERS.len() {
        divisor = divisor.saturating_mul(UNIT);
        idx += 1;
    }
    let tenths = u128::from(bytes).saturating_mul(10) / divisor;
    format!("{}.{} {}iB", tenths / 10, tenths % 10, LETTERS[idx])
}

/// Whether `stamp` is the epoch a backing with no stamp of that kind reports
/// (see [`Entry::modified`](crate::Entry::modified)), which a listing leaves
/// blank rather than showing a fabricated `1970-01-01`.
fn unstamped(stamp: Time64) -> bool {
    stamp.secs() == Time64::UNIX_EPOCH.secs()
        && stamp.subsec_nanos() == Time64::UNIX_EPOCH.subsec_nanos()
}

/// Render an entry's modification instant as an ISO calendar date
/// (`YYYY-MM-DD`), or the empty string when the instant is the epoch.
///
/// Dates before 1970 and far after 2038 render correctly: the civil date is
/// [`CivilTime`]'s, over the signed 64-bit seconds.
#[must_use]
pub fn format_date(modified: Time64) -> String {
    if unstamped(modified) {
        return String::new();
    }
    let civil = CivilTime::from_time64(modified);
    format!("{:04}-{:02}-{:02}", civil.year, civil.month, civil.day)
}

/// Render an instant as an ISO date and the time of day to the minute
/// (`YYYY-MM-DD HH:MM`), the spelling a listing's modified column shows, or the
/// empty string when the instant is the epoch.
#[must_use]
pub fn format_minute(stamp: Time64) -> String {
    if unstamped(stamp) {
        return String::new();
    }
    iso_minute(&CivilTime::from_time64(stamp))
}

/// Render an instant as an ISO calendar date *and* wall-clock time
/// (`YYYY-MM-DD HH:MM:SS`), or the empty string when the instant is the epoch.
///
/// The properties-view spelling of a timestamp, where the exact second
/// matters.
#[must_use]
pub fn format_datetime(stamp: Time64) -> String {
    if unstamped(stamp) {
        return String::new();
    }
    let civil = CivilTime::from_time64(stamp);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        civil.year, civil.month, civil.day, civil.hour, civil.minute, civil.second
    )
}

#[cfg(test)]
mod tests {
    use super::{format_date, format_datetime, format_minute, format_size};
    use tairix_abi::time::Time64;

    #[test]
    fn sizes_below_a_kibibyte_show_exact_bytes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1), "1 B");
        assert_eq!(format_size(742), "742 B");
        assert_eq!(format_size(1023), "1023 B");
    }

    #[test]
    fn sizes_scale_into_binary_units_with_one_decimal() {
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(1536), "1.5 KiB");
        assert_eq!(format_size(1024 * 1024), "1.0 MiB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(format_size(1024u64.pow(4)), "1.0 TiB");
        assert_eq!(format_size(1024u64.pow(5)), "1.0 PiB");
    }

    #[test]
    fn huge_sizes_stay_in_pib_without_overflow() {
        // Beyond a pebibyte the value stays in PiB rather than wrapping.
        assert_eq!(format_size(u64::MAX), "16383.9 PiB");
    }

    #[test]
    fn the_epoch_renders_blank_never_a_fabricated_date() {
        assert_eq!(format_date(Time64::UNIX_EPOCH), "");
    }

    #[test]
    fn dates_render_iso_after_and_before_the_epoch() {
        // 2021-01-01T00:00:00Z.
        assert_eq!(format_date(Time64::from_secs(1_609_459_200)), "2021-01-01");
        // A stamp late in a day still resolves to that civil day.
        assert_eq!(
            format_date(Time64::from_secs(1_609_459_200 + 86_399)),
            "2021-01-01"
        );
        // One second before the epoch is the last day of 1969.
        assert_eq!(format_date(Time64::from_secs(-1)), "1969-12-31");
        // Far after 2038 (the 32-bit boundary) still renders.
        assert_eq!(format_date(Time64::from_secs(4_102_444_800)), "2100-01-01");
    }

    #[test]
    fn datetime_renders_the_epoch_blank_never_a_fabricated_time() {
        assert_eq!(format_datetime(Time64::UNIX_EPOCH), "");
    }

    #[test]
    fn datetime_renders_date_and_wall_clock_time() {
        // 2021-01-01T00:00:00Z.
        assert_eq!(
            format_datetime(Time64::from_secs(1_609_459_200)),
            "2021-01-01 00:00:00"
        );
        // A stamp mid-day shows the floored hour/minute/second.
        assert_eq!(
            format_datetime(Time64::from_secs(1_609_459_200 + 13 * 3600 + 37 * 60 + 5)),
            "2021-01-01 13:37:05"
        );
        // Sub-second precision does not appear in the second-resolution render.
        assert_eq!(
            format_datetime(Time64::new(1_609_459_200, 999_999_999).expect("canonical")),
            "2021-01-01 00:00:00"
        );
        // One second before the epoch is 23:59:59 of the previous civil day,
        // never a negative field.
        assert_eq!(
            format_datetime(Time64::from_secs(-1)),
            "1969-12-31 23:59:59"
        );
        // Far after 2038 still renders with the time of day.
        assert_eq!(
            format_datetime(Time64::from_secs(4_102_444_800 + 59)),
            "2100-01-01 00:00:59"
        );
    }

    #[test]
    fn a_listings_modified_column_reads_to_the_minute_and_leaves_the_epoch_blank() {
        assert_eq!(format_minute(Time64::UNIX_EPOCH), "");
        assert_eq!(
            format_minute(Time64::from_secs(1_609_459_200 + 13 * 3600 + 37 * 60 + 59)),
            "2021-01-01 13:37"
        );
        assert_eq!(format_minute(Time64::from_secs(-1)), "1969-12-31 23:59");
        assert_eq!(
            format_minute(Time64::from_secs(4_102_444_800)),
            "2100-01-01 00:00"
        );
    }
}
