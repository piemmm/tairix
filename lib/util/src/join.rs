//! Joining a list into one bounded line: whole items in order while they fit,
//! then how many were left out — never an item cut in half, and never a line
//! past its bound.

use alloc::string::String;
use core::fmt::Write;

/// Between two items.
const SEPARATOR: &str = ", ";

/// Room for the note at the widest count it can carry, so the note always
/// fits whatever was kept.
const NOTE_RESERVE: usize = ", and 4294967295 more".len();

/// `items` joined with `, ` into at most `max` bytes.
///
/// Every item, when they all fit. Otherwise the longest run of whole items
/// from the front that leaves room for the note, then `, and N more` — or
/// `N more` when no item fits beside it. Empty when there are no items, and
/// the note is left out when `max` cannot hold even that.
#[must_use]
pub fn join_within<T: AsRef<str>>(items: impl IntoIterator<Item = T>, max: usize) -> String {
    let mut joined = String::new();
    let mut kept: u32 = 0;
    // The bytes and items of the longest prefix a note still fits after.
    let mut room_for_note = (0, 0);
    let mut total: u32 = 0;
    let mut overflowed = false;
    for item in items {
        total = total.saturating_add(1);
        if overflowed {
            continue;
        }
        let item = item.as_ref();
        let separator = if kept == 0 { "" } else { SEPARATOR };
        if joined.len() + separator.len() + item.len() > max {
            overflowed = true;
            continue;
        }
        joined.push_str(separator);
        joined.push_str(item);
        kept = kept.saturating_add(1);
        if joined.len() + NOTE_RESERVE <= max {
            room_for_note = (joined.len(), kept);
        }
    }
    if !overflowed {
        return joined;
    }
    let (bytes, shown) = room_for_note;
    joined.truncate(bytes);
    let lead = if shown == 0 { "" } else { ", and " };
    let mut note = String::new();
    // Writing into a String cannot fail.
    let _ = write!(note, "{lead}{} more", total.saturating_sub(shown));
    if joined.len() + note.len() <= max {
        joined.push_str(&note);
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::join_within;
    use alloc::format;
    use alloc::string::String;
    use alloc::vec::Vec;

    #[test]
    fn items_that_fit_are_joined_whole() {
        assert_eq!(join_within(["a", "b", "c"], 64), "a, b, c");
        assert_eq!(join_within(Vec::<String>::new(), 64), "");
    }

    #[test]
    fn a_list_past_its_bound_keeps_whole_items_and_counts_the_rest() {
        let items: Vec<String> = (0..64).map(|i| format!("10.0.0.{i}")).collect();
        let joined = join_within(&items, 120);
        assert!(joined.len() <= 120, "{joined}");
        let (shown, tail) = joined.rsplit_once(", and ").expect("a note");
        let kept: Vec<&str> = shown.split(", ").collect();
        let first: Vec<&str> = items[..kept.len()].iter().map(String::as_str).collect();
        assert_eq!(kept, first);
        assert_eq!(tail, format!("{} more", items.len() - kept.len()));
    }

    #[test]
    fn the_last_item_needs_no_room_for_a_note() {
        // "aaaa, bbbb" is ten bytes: it fits ten exactly, and would not if
        // the last item had to leave room for a note it never needs.
        assert_eq!(join_within(["aaaa", "bbbb"], 10), "aaaa, bbbb");
    }

    #[test]
    fn when_not_even_the_first_fits_only_the_count_is_said() {
        let wide = "x".repeat(40);
        assert_eq!(join_within([wide.as_str(), "y"], 30), "2 more");
    }

    #[test]
    fn a_bound_too_small_for_the_note_is_still_kept() {
        let joined = join_within(["abcdefgh", "ijklmnop"], 9);
        assert!(joined.len() <= 9, "{joined}");
        assert_eq!(joined, "2 more");
        assert_eq!(join_within(["abcdefgh", "ijklmnop"], 3), "");
    }
}
