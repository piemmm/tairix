//! The place chooser over a zone store (spec §11.47): the *Time Zone* dialog
//! Settings and Date & Time open, its rows the store's places by city, each
//! reading the local time there now.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Time64;
use tairix_abi::zone::ZoneName;
use tairix_geometry::{Rect, Region, Scale};
use tairix_theme::Theme;
use tairix_tz::listing::Listing;

use crate::place::{PlaceChooser, PlaceDialog, PlaceRow};

/// The title the dialog's window carries.
pub const TITLE: &str = "Time Zone";

/// What the search field says while it is empty: what a query is matched
/// against.
const SEARCH_HINT: &str = "Search for a city, a region or a zone";

/// The minute a dialog's readings were last written for, so a tick rewrites
/// them only once it has turned.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Minute(Option<i64>);

impl Minute {
    /// The minute `now` lies in, or none without a time.
    #[must_use]
    pub fn of(now: Option<Time64>) -> Self {
        Self(now.map(|now| now.secs().div_euclid(60)))
    }
}

/// `zone`'s reading in `listing` at `now`, or none without a time.
#[must_use]
pub fn reading(listing: &Listing<'_>, zone: &ZoneName, now: Option<Time64>) -> String {
    now.map_or_else(String::new, |now| listing.reading(zone, now))
}

/// `listing`'s places as the dialog's rows, in its order, each read at
/// `now`. A row's key is its zone's name, so a search for `Europe/` finds
/// what it holds.
pub fn rows<'a>(
    listing: &'a Listing<'_>,
    now: Option<Time64>,
) -> impl Iterator<Item = PlaceRow> + 'a {
    listing.entries().iter().map(move |entry| PlaceRow {
        name: entry.city.clone(),
        region: entry.region.clone(),
        reading: reading(listing, &entry.zone, now),
        key: String::from(entry.zone.as_str()),
    })
}

/// The dialog over `rows`, `highlight` highlighted and scrolled into view.
#[must_use]
pub fn dialog(rows: Vec<PlaceRow>, highlight: Option<usize>) -> PlaceDialog {
    let chooser = PlaceChooser::new(rows).with_placeholder(SEARCH_HINT);
    let chooser = match highlight {
        Some(row) => chooser.with_highlight(row),
        None => chooser,
    };
    PlaceDialog::new(TITLE, chooser)
}

/// Rewrite the readings once `now` has left the minute `written` holds, the
/// rows from `first` on being `listing`'s places in its order; damage reports
/// each row whose reading changed where it shows in a dialog drawn in
/// `bounds`. Answers whether the minute had turned, so an owner with rows of
/// its own before `first` rewrites those too.
pub fn refresh(
    dialog: &mut PlaceDialog,
    written: &mut Minute,
    (listing, first): (&Listing<'_>, usize),
    now: Time64,
    (bounds, scale, theme): (Rect, Scale, &Theme),
    damage: &mut Region,
) -> bool {
    let minute = Minute::of(Some(now));
    if *written == minute {
        return false;
    }
    *written = minute;
    let Some(list) = dialog.chooser_rect(bounds, scale, theme) else {
        return true;
    };
    let readings = listing
        .entries()
        .iter()
        .enumerate()
        .map(|(row, entry)| (first.saturating_add(row), listing.reading(&entry.zone, now)));
    dialog
        .chooser_mut()
        .set_readings(readings, list, scale, theme, damage);
    true
}
