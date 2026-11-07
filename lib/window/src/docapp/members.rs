//! Member windows: further windows onto one document window's document, which
//! the host routes to that window, its primary, wherever the document is what
//! a request is about.

use super::Request;

/// Where a member window's request is carried out.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Route {
    /// By its primary, whose document it is about: a save, a Save As or a
    /// copy.
    Primary,
    /// By the member alone: a menu, an open, or the application's own.
    Member,
    /// The member closes: it holds nothing its primary does not, so it is
    /// never asked whether to save.
    Close,
    /// Its primary saves, and the member closes.
    SaveThenClose,
}

/// Where a member window's `request` is carried out.
#[must_use]
pub(super) const fn route<K, O>(request: &Request<K, O>) -> Route {
    match request {
        Request::Save | Request::SaveAs | Request::SaveWhere { .. } | Request::SaveCopy => {
            Route::Primary
        }
        Request::SaveThenClose => Route::SaveThenClose,
        Request::Close | Request::AskToClose => Route::Close,
        Request::Open | Request::Menu { .. } | Request::Own(_) => Route::Member,
    }
}

/// The windows, of `windows` (each its id and the primary it is a member of),
/// that show the document of window `primary`: they close with it.
pub(super) fn members_of(
    primary: u64,
    windows: impl IntoIterator<Item = (u64, Option<u64>)>,
) -> impl Iterator<Item = u64> {
    windows
        .into_iter()
        .filter_map(move |(id, of)| (of == Some(primary)).then_some(id))
}

#[cfg(test)]
#[path = "members_tests.rs"]
mod tests;
