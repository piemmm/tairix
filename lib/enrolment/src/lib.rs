//! TAIRiX service enrolment store engine: which discovered service bundles
//! are eligible to be brought up (`plans/NEW-SERVICEMANAGER.md` §2, §3.1).
//!
//! A service bundle on disk is not a running service until it is enrolled:
//! presence is never eligibility. Enrolment is layered. The image's layer,
//! [`Enrolment`], is compiled into PID 1's startup configuration, because no
//! document under `/System` is readable at the instant the manager decides
//! what to bring up. The administrator's layer, [`EnrolmentOverride`], is
//! `/System/Settings/Services/overrides` and holds only what was changed from
//! the image, so an update's new defaults reach everything the administrator
//! has not spoken about. [`enabled`] is the one precedence of the two.
//!
//! An override document is untrusted input, refused whole on any defect; a
//! refused or missing one leaves the image's layer standing. A service not
//! positively enrolled never starts, and enrolment decides eligibility only:
//! the kernel still derives a service's authority from its signed bundle at
//! spawn.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

use core::fmt;

use tairix_abi::ServiceEnrolment;
use tairix_util::conf::{setting_line, Located};

/// Maximum length, in bytes, of a single service name.
///
/// A validation bound on one identifier (never a cap on *how many* services
/// may be enrolled — that set is a growable capacity, not a fixed ceiling):
/// a service name is a short bundle identifier, and an over-long one is a
/// packaging defect, not a workload. Fail closed
/// ([`EnrolError::NameTooLong`]) rather than truncate.
pub const MAX_SERVICE_NAME_LEN: usize = 64;

/// Largest enrolment document, in bytes.
///
/// A validation bound, not a capacity: a document holds one short service
/// name per line, so anything larger is corrupt or hostile, and is refused
/// whole ([`EnrolError::TooLong`]) by every reader alike. It holds some 880
/// overrides at the longest names, and the writer refuses past it rather than
/// produce a document no reader would accept.
pub const MAX_DOCUMENT_LEN: usize = 64 * 1024;

/// Why an enrolment store text, or a change to an override layer, was
/// refused.
///
/// Every variant is a fail-closed refusal: the operation records nothing and
/// changes nothing, so a malformed store or request can never make a
/// surprising service eligible.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EnrolError {
    /// A service name was empty.
    NameEmpty,
    /// A service name exceeded [`MAX_SERVICE_NAME_LEN`].
    NameTooLong,
    /// A service name contained a byte outside the permitted set (a
    /// lowercase-ASCII bundle identifier: `[a-z0-9]` first, then
    /// `[a-z0-9._-]`). Rejecting anything else keeps a path-traversal- or
    /// case-collision-shaped name out of the store.
    NameInvalid,
    /// The store text names the same service more than once.
    Duplicate,
    /// An override line's disposition word was neither `enabled` nor
    /// `disabled`, or the line carried more than a name and a disposition.
    ServiceEnrolmentInvalid,
    /// The document is longer than [`MAX_DOCUMENT_LEN`].
    TooLong,
    /// There was no memory to hold the layer or its text.
    OutOfMemory,
}

impl fmt::Display for EnrolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NameEmpty => f.write_str("a service name is empty"),
            Self::NameTooLong => f.write_str("a service name is too long"),
            Self::NameInvalid => f.write_str("a service name contains an invalid character"),
            Self::Duplicate => f.write_str("a service is enrolled more than once"),
            Self::ServiceEnrolmentInvalid => {
                f.write_str("an override line is not `<service> enabled|disabled`")
            }
            Self::TooLong => f.write_str("the enrolment document is too long"),
            Self::OutOfMemory => f.write_str("no memory to hold the enrolment record"),
        }
    }
}

/// A refused store text, and the line that raised the refusal.
pub type LocatedError = Located<EnrolError>;

/// Validate a service name: a non-empty, lowercase-ASCII bundle identifier of
/// at most [`MAX_SERVICE_NAME_LEN`] bytes whose first byte is `[a-z0-9]` and
/// whose remaining bytes are `[a-z0-9._-]`.
///
/// The strict alphabet is a security control, not cosmetics: because a name
/// can neither start with `.` nor contain `/`, a store entry can never be a
/// `..` or path-traversal-shaped token, and because it is lowercase-only two
/// entries can never collide by case.
///
/// # Errors
///
/// [`EnrolError::NameEmpty`], [`EnrolError::NameTooLong`], or
/// [`EnrolError::NameInvalid`] for the respective defect.
pub fn validate_service_name(name: &str) -> Result<(), EnrolError> {
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        return Err(EnrolError::NameEmpty);
    }
    if bytes.len() > MAX_SERVICE_NAME_LEN {
        return Err(EnrolError::NameTooLong);
    }
    for (i, &b) in bytes.iter().enumerate() {
        let ok = b.is_ascii_lowercase()
            || b.is_ascii_digit()
            || (i > 0 && matches!(b, b'.' | b'_' | b'-'));
        if !ok {
            return Err(EnrolError::NameInvalid);
        }
    }
    Ok(())
}

/// The parsed enrolment record of one scope: the set of service names that
/// are eligible to be brought up.
///
/// The names are validated, unique, and held in sorted order so the store
/// has one canonical serialisation (a change to the set is a minimal,
/// order-stable diff). The set is a growable capacity — there is no
/// fixed cap on how many services may be enrolled.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Enrolment {
    /// Enabled service names, validated, unique, ascending.
    names: Vec<String>,
}

impl Enrolment {
    /// The empty enrolment: nothing is enabled.
    #[must_use]
    pub const fn empty() -> Self {
        Self { names: Vec::new() }
    }

    /// The enrolment of the services `names` name, in any order; a name
    /// given twice is enrolled once.
    ///
    /// # Errors
    ///
    /// The first name defect ([`EnrolError::NameEmpty`] / `NameTooLong` /
    /// `NameInvalid`).
    pub fn of<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<Self, EnrolError> {
        let mut set = BTreeSet::new();
        for name in names {
            validate_service_name(name)?;
            set.insert(name);
        }
        Ok(Self {
            names: set.into_iter().map(String::from).collect(),
        })
    }

    /// Whether the named service is enrolled (eligible to be brought up).
    #[must_use]
    pub fn is_enabled(&self, name: &str) -> bool {
        self.names
            .binary_search_by(|held| held.as_str().cmp(name))
            .is_ok()
    }

    /// The enrolled service names, sorted ascending.
    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Number of enrolled services.
    #[must_use]
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Whether no service is enrolled.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

/// The administrator's override layer: the services whose enrolment differs
/// from the image's [`Enrolment`], and how.
///
/// A service the administrator has not spoken about simply has no entry, so
/// there is no third state meaning "unspecified" — the image's layer decides
/// it.
///
/// Held sorted and unique so the document has one canonical serialisation.
/// Its only fixed bound is [`MAX_DOCUMENT_LEN`], which
/// [`to_store_text`](Self::to_store_text) holds the writer to.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EnrolmentOverride {
    /// `(service name, disposition)`, validated, unique, ascending by name.
    entries: Vec<(String, ServiceEnrolment)>,
}

impl EnrolmentOverride {
    /// The empty override layer: the image's enrolment stands unchanged.
    ///
    /// The fail-closed resolution of a **missing** document (no administrator
    /// has changed anything, or the root is not mounted) and of a
    /// **corrupt** one, which is the same answer: obey the signed image.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Parse an override document.
    ///
    /// One `<service> enabled|disabled` per line, read by the `key value`
    /// grammar every `/System/Settings` document shares, `#` comments and
    /// blank lines ignored. Fails closed on a malformed name, a missing or
    /// unknown disposition word, a line with extra words, or a duplicate.
    ///
    /// # Errors
    ///
    /// The first refusal at its line: a name defect,
    /// [`EnrolError::ServiceEnrolmentInvalid`] for a malformed line, or
    /// [`EnrolError::Duplicate`] at a service's second appearance; or
    /// [`EnrolError::OutOfMemory`], for the whole document, when there is no
    /// memory to hold it.
    pub fn parse(text: &str) -> Result<Self, LocatedError> {
        if text.len() > MAX_DOCUMENT_LEN {
            return Err(LocatedError::whole(EnrolError::TooLong));
        }
        let mut layer = Self::empty();
        for (index, line) in text.lines().enumerate() {
            let Some(setting) = setting_line(line) else {
                continue;
            };
            let refused = |kind| LocatedError::at(index + 1, kind);
            let name = setting.key;
            validate_service_name(name).map_err(refused)?;
            let disposition = setting
                .value
                .and_then(ServiceEnrolment::from_name)
                .ok_or(refused(EnrolError::ServiceEnrolmentInvalid))?;
            // Kept sorted as it is read, so a duplicate is found at the line
            // that repeats it and no second index is needed to find it.
            let Err(at) = layer.place(name) else {
                return Err(refused(EnrolError::Duplicate));
            };
            layer
                .insert(at, name, disposition)
                .map_err(LocatedError::whole)?;
        }
        Ok(layer)
    }

    /// Where `name`'s entry is, or where it would go.
    fn place(&self, name: &str) -> Result<usize, usize> {
        self.entries
            .binary_search_by(|(held, _)| held.as_str().cmp(name))
    }

    /// Hold `disposition` for `name` at `at`, asking the heap for its room
    /// first so a refusal changes nothing.
    fn insert(
        &mut self,
        at: usize,
        name: &str,
        disposition: ServiceEnrolment,
    ) -> Result<(), EnrolError> {
        let mut owned = String::new();
        owned
            .try_reserve_exact(name.len())
            .and_then(|()| self.entries.try_reserve(1))
            .map_err(|_| EnrolError::OutOfMemory)?;
        owned.push_str(name);
        self.entries.insert(at, (owned, disposition));
        Ok(())
    }

    /// Make `wanted` the enrolment of `name` over the image layer `vendor`,
    /// holding an entry only where the two differ, and answer the change
    /// [`undo`](Self::undo) takes back.
    ///
    /// Self-minimising: a service set to what the image already says has no
    /// entry, so a later image shipping a different default reaches it. The
    /// change is to this one entry; every other the administrator wrote
    /// stands as written.
    ///
    /// # Errors
    ///
    /// A name defect, or [`EnrolError::OutOfMemory`] when a new entry has no
    /// room; the layer is unchanged either way.
    pub fn set(
        &mut self,
        vendor: &Enrolment,
        name: &str,
        wanted: ServiceEnrolment,
    ) -> Result<Change, EnrolError> {
        validate_service_name(name)?;
        let default = vendor.is_enabled(name) == wanted.is_enabled();
        match self.place(name) {
            Ok(at) if default => {
                let (name, disposition) = self.entries.remove(at);
                Ok(Change::Removed(at, name, disposition))
            }
            Ok(at) if self.entries[at].1 == wanted => Ok(Change::None),
            Ok(at) => {
                let was = core::mem::replace(&mut self.entries[at].1, wanted);
                Ok(Change::Replaced(at, was))
            }
            Err(_) if default => Ok(Change::None),
            Err(at) => {
                self.insert(at, name, wanted)?;
                Ok(Change::Added(at))
            }
        }
    }

    /// Take back `change`, the most recent [`set`](Self::set) of this layer.
    ///
    /// Never asks the heap: an entry taken out is put back in the room its
    /// removal left.
    pub fn undo(&mut self, change: Change) {
        match change {
            Change::None => {}
            Change::Added(at) => {
                self.entries.remove(at);
            }
            Change::Replaced(at, was) => self.entries[at].1 = was,
            Change::Removed(at, name, disposition) => self.entries.insert(at, (name, disposition)),
        }
    }

    /// The administrator's disposition for `name`, or `None` if they have not
    /// spoken about it.
    #[must_use]
    pub fn disposition(&self, name: &str) -> Option<ServiceEnrolment> {
        self.entries
            .binary_search_by(|(held, _)| held.as_str().cmp(name))
            .ok()
            .map(|at| self.entries[at].1)
    }

    /// The recorded entries, ascending by service name.
    #[must_use]
    pub fn entries(&self) -> &[(String, ServiceEnrolment)] {
        &self.entries
    }

    /// Whether the administrator has changed nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Serialise to the canonical document text, which round-trips with
    /// [`parse`](Self::parse).
    ///
    /// # Errors
    ///
    /// [`EnrolError::TooLong`] when the text would exceed [`MAX_DOCUMENT_LEN`]:
    /// a document every reader refuses would bring back, at the next boot,
    /// every service the administrator disabled. [`EnrolError::OutOfMemory`]
    /// when there is no memory for the text.
    pub fn to_store_text(&self) -> Result<String, EnrolError> {
        const HEADER: &str =
            "# TAIRiX service enrolment overrides. One `<service> enabled|disabled` per line.\n";
        let len = self
            .entries
            .iter()
            .fold(HEADER.len(), |len, (name, disposition)| {
                len.saturating_add(name.len() + disposition.as_str().len() + 2)
            });
        if len > MAX_DOCUMENT_LEN {
            return Err(EnrolError::TooLong);
        }
        let mut out = String::new();
        out.try_reserve_exact(len)
            .map_err(|_| EnrolError::OutOfMemory)?;
        out.push_str(HEADER);
        for (name, disposition) in &self.entries {
            out.push_str(name);
            out.push(' ');
            out.push_str(disposition.as_str());
            out.push('\n');
        }
        Ok(out)
    }
}

/// One change [`EnrolmentOverride::set`] made, for
/// [`EnrolmentOverride::undo`] to take back.
#[derive(Debug, Eq, PartialEq)]
#[must_use]
pub enum Change {
    /// Nothing changed: the layer already said this.
    None,
    /// An entry was added at this place.
    Added(usize),
    /// The entry at this place said this before.
    Replaced(usize, ServiceEnrolment),
    /// This entry was taken from this place.
    Removed(usize, String, ServiceEnrolment),
}

/// Whether `name` is enrolled: the administrator's word where they have
/// spoken about it, else the image's.
///
/// The one definition of the precedence, so no consumer re-derives it, and a
/// query rather than a set built per question.
#[must_use]
pub fn enabled(vendor: &Enrolment, overrides: &EnrolmentOverride, name: &str) -> bool {
    overrides
        .disposition(name)
        .map_or_else(|| vendor.is_enabled(name), ServiceEnrolment::is_enabled)
}

#[cfg(test)]
#[global_allocator]
static ALLOC: tairix_fuzzseed::meter::Metered = tairix_fuzzseed::meter::Metered;

#[cfg(test)]
mod tests {
    use super::{
        enabled, Change, EnrolError, Enrolment, EnrolmentOverride, LocatedError, ServiceEnrolment,
        MAX_SERVICE_NAME_LEN,
    };
    use alloc::string::String;
    use alloc::vec::Vec;
    use tairix_fuzzseed::meter::{metered, refusing_after};
    use ServiceEnrolment::{Disabled, Enabled};

    fn names(e: &Enrolment) -> Vec<&str> {
        e.names().iter().map(String::as_str).collect()
    }

    /// The names of `vendor` and `extra` that `overrides` enrols over it.
    fn enrolled<'a>(
        vendor: &Enrolment,
        overrides: &EnrolmentOverride,
        candidates: &[&'a str],
    ) -> Vec<&'a str> {
        candidates
            .iter()
            .copied()
            .filter(|name| enabled(vendor, overrides, name))
            .collect()
    }

    #[test]
    fn an_enrolment_is_a_sorted_set_of_valid_names() {
        let e = Enrolment::of(["sysinfod", "netstack", "devmgr", "netstack"]).expect("valid");
        assert_eq!(names(&e), ["devmgr", "netstack", "sysinfod"]);
        assert!(e.is_enabled("netstack"));
        assert!(!e.is_enabled("fontd"));
        assert_eq!(e.len(), 3);
        assert_eq!(Enrolment::of(["a", "../etc"]), Err(EnrolError::NameInvalid));
        assert!(Enrolment::of([]).expect("empty").is_empty());
    }

    #[test]
    fn setting_keeps_the_layer_sorted() {
        let vendor = Enrolment::of(["b", "d"]).expect("valid");
        let mut overrides = EnrolmentOverride::empty();
        for name in ["c", "a", "e"] {
            let _ = overrides.set(&vendor, name, Enabled).expect("set");
        }
        let held: Vec<&str> = overrides
            .entries()
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(held, ["a", "c", "e"]);
    }

    #[test]
    fn an_override_line_without_a_disposition_is_refused() {
        assert_eq!(
            EnrolmentOverride::parse("timed\n"),
            Err(LocatedError::at(1, EnrolError::ServiceEnrolmentInvalid))
        );
        assert_eq!(
            EnrolmentOverride::parse("  timed   disabled   # why\n")
                .map(|o| o.disposition("timed")),
            Ok(Some(ServiceEnrolment::Disabled)),
            "the shared key-value grammar: white space and a trailing comment"
        );
    }

    #[test]
    fn name_validation_is_strict_and_fails_closed() {
        use super::validate_service_name;
        assert_eq!(validate_service_name(""), Err(EnrolError::NameEmpty));
        assert_eq!(
            validate_service_name(&"x".repeat(MAX_SERVICE_NAME_LEN + 1)),
            Err(EnrolError::NameTooLong),
        );
        // A leading dot (path traversal shape) is refused: the first byte
        // must be alphanumeric.
        assert_eq!(
            validate_service_name(".hidden"),
            Err(EnrolError::NameInvalid)
        );
        assert_eq!(validate_service_name("a/b"), Err(EnrolError::NameInvalid));
        assert_eq!(validate_service_name("Upper"), Err(EnrolError::NameInvalid));
        assert_eq!(validate_service_name("net-stack_2.0"), Ok(()));
    }

    #[test]
    fn setting_a_state_twice_changes_it_once() {
        let vendor = Enrolment::empty();
        let mut overrides = EnrolmentOverride::empty();
        assert_eq!(
            overrides.set(&vendor, "netstack", Enabled),
            Ok(Change::Added(0))
        );
        assert!(enabled(&vendor, &overrides, "netstack"));
        assert_eq!(
            overrides.set(&vendor, "netstack", Enabled),
            Ok(Change::None)
        );
    }

    #[test]
    fn setting_refuses_an_invalid_name_and_changes_nothing() {
        let mut overrides = EnrolmentOverride::empty();
        assert_eq!(
            overrides.set(&Enrolment::empty(), "Bad Name", Enabled),
            Err(EnrolError::NameInvalid),
        );
        assert!(overrides.is_empty());
    }

    #[test]
    fn disabling_an_image_service_records_it_and_again_changes_nothing() {
        let vendor = Enrolment::of(["a", "b", "c"]).expect("valid names");
        let mut overrides = EnrolmentOverride::empty();
        assert_eq!(overrides.set(&vendor, "b", Disabled), Ok(Change::Added(0)));
        assert_eq!(enrolled(&vendor, &overrides, &["a", "b", "c"]), ["a", "c"]);
        assert_eq!(overrides.set(&vendor, "b", Disabled), Ok(Change::None));
        assert_eq!(
            overrides.set(&vendor, "z", Disabled),
            Ok(Change::None),
            "a service nothing enrols is already disabled"
        );
    }

    #[test]
    fn an_absent_or_corrupt_override_document_leaves_the_image_layer_standing() {
        let vendor = Enrolment::of(["netstack", "timed"]).expect("valid names");
        // Missing document.
        assert_eq!(
            enrolled(
                &vendor,
                &EnrolmentOverride::empty(),
                &["fontd", "netstack", "timed"]
            ),
            names(&vendor)
        );
        // Corrupt documents fail closed; the caller resolves each to `empty`,
        // which is "obey the signed image".
        for text in [
            "timed\n",                         // no disposition word
            "timed disabled extra\n",          // an extra word
            "timed off\n",                     // an unknown disposition
            "BAD disabled\n",                  // a malformed name
            "timed disabled\ntimed enabled\n", // a duplicate
        ] {
            assert!(
                EnrolmentOverride::parse(text).is_err(),
                "override text should fail closed: {text:?}"
            );
        }
    }

    #[test]
    fn an_over_long_override_document_is_refused_whole() {
        let text = "a enabled\n".repeat(super::MAX_DOCUMENT_LEN / 10 + 1);
        assert_eq!(
            EnrolmentOverride::parse(&text),
            Err(LocatedError::whole(EnrolError::TooLong))
        );
    }

    #[test]
    fn an_override_refusal_names_its_line() {
        assert_eq!(
            EnrolmentOverride::parse("# admin\ntimed disabled\nfontd sometimes\n"),
            Err(LocatedError::at(3, EnrolError::ServiceEnrolmentInvalid))
        );
        assert_eq!(
            EnrolmentOverride::parse("timed disabled\n\ntimed enabled\n"),
            Err(LocatedError::at(3, EnrolError::Duplicate))
        );
    }

    #[test]
    fn an_override_disables_and_enables_over_the_image_layer() {
        let vendor = Enrolment::of(["netstack", "timed"]).expect("valid names");
        let overrides =
            EnrolmentOverride::parse("timed disabled\nfontd enabled # both directions\n")
                .expect("parses");
        assert_eq!(
            overrides.disposition("timed"),
            Some(ServiceEnrolment::Disabled)
        );
        assert_eq!(
            overrides.disposition("fontd"),
            Some(ServiceEnrolment::Enabled)
        );
        assert_eq!(overrides.disposition("netstack"), None);

        assert_eq!(
            enrolled(&vendor, &overrides, &["fontd", "netstack", "timed"]),
            ["fontd", "netstack"]
        );
    }

    #[test]
    fn override_text_round_trips_canonically() {
        let overrides =
            EnrolmentOverride::parse("timed disabled\nfontd enabled\n").expect("parses");
        let text = overrides.to_store_text().expect("fits");
        assert_eq!(
            EnrolmentOverride::parse(&text).expect("canonical text reparses"),
            overrides
        );
        // Ascending by name, one entry per line.
        assert!(text.contains("fontd enabled\n"));
        assert!(text.contains("timed disabled\n"));
        assert!(text.find("fontd").unwrap() < text.find("timed").unwrap());
    }

    #[test]
    fn setting_records_only_what_differs_from_the_image() {
        let vendor = Enrolment::of(["netstack", "timed"]).expect("valid names");
        let mut overrides = EnrolmentOverride::empty();

        // Disabling one of the image's services records exactly that.
        let _ = overrides.set(&vendor, "timed", Disabled).expect("set");
        assert_eq!(
            overrides.entries(),
            [(String::from("timed"), ServiceEnrolment::Disabled)]
        );

        // Re-enabling it empties the document rather than pinning the
        // default: a later image that ships `timed` disabled must then be
        // obeyed, because the administrator is no longer speaking about it.
        assert_eq!(
            overrides.set(&vendor, "timed", Enabled),
            Ok(Change::Removed(0, String::from("timed"), Disabled))
        );
        assert!(overrides.is_empty());

        // Enabling something the image does not ship records an Enabled entry.
        let _ = overrides.set(&vendor, "fontd", Enabled).expect("set");
        assert_eq!(
            overrides.entries(),
            [(String::from("fontd"), ServiceEnrolment::Enabled)]
        );
    }

    #[test]
    fn setting_each_name_reproduces_any_wanted_enrolment() {
        // Whatever the administrator wants, the layer reproduces it exactly
        // over the image layer — the property both PID 1's boot read and its
        // control path rely on — and its document is canonical.
        let vendor = Enrolment::of(["a", "b", "c"]).expect("valid names");
        let all = ["a", "b", "c", "d", "e"];
        let wanted: [&[&str]; 7] = [
            &[],
            &["a"],
            &["b", "c"],
            &["a", "b", "c"],
            &["d"],
            &["a", "d"],
            &["b", "d", "e"],
        ];
        for wanted in wanted {
            let mut overrides = EnrolmentOverride::empty();
            for name in all {
                let state = if wanted.contains(&name) {
                    Enabled
                } else {
                    Disabled
                };
                let _ = overrides.set(&vendor, name, state).expect("set");
            }
            assert_eq!(enrolled(&vendor, &overrides, &all), wanted);
            assert_eq!(
                EnrolmentOverride::parse(&overrides.to_store_text().expect("fits"))
                    .expect("reparses"),
                overrides
            );
        }
    }

    /// A change taken back is taken back with no memory to spare, so a
    /// record whose write was refused under pressure is restored rather than
    /// left describing a change that never reached the disk.
    ///
    /// Regression: the control path built the next layer from scratch, so a
    /// refused write had nothing to restore but needed memory to try.
    #[test]
    fn every_change_is_taken_back_without_the_heap() {
        let vendor = Enrolment::of(["netstack", "timed"]).expect("valid names");
        let base = EnrolmentOverride::parse(
            "fontd enabled
timed disabled
",
        )
        .expect("parses");
        for (name, state) in [
            ("discoveryd", Enabled),
            ("timed", Enabled),
            ("fontd", Disabled),
            ("netstack", Disabled),
        ] {
            let mut overrides = base.clone();
            let change = overrides.set(&vendor, name, state).expect("set");
            assert_ne!(change, Change::None, "{name}");
            refusing_after(0, || overrides.undo(change));
            assert_eq!(overrides, base, "{name}");
        }
    }

    /// The precedence is a query: asking it allocates nothing.
    ///
    /// Regression: every activation and enrolment question rebuilt the
    /// effective set, a string per enrolled name.
    #[test]
    fn asking_the_precedence_allocates_nothing() {
        let vendor = Enrolment::of(["netstack", "timed"]).expect("valid names");
        let overrides = EnrolmentOverride::parse(
            "timed disabled
",
        )
        .expect("parses");
        let ((), used) = metered(|| {
            for name in ["netstack", "timed", "fontd"] {
                let _ = enabled(&vendor, &overrides, name);
            }
        });
        assert_eq!(used.allocations, 0);
    }

    /// A layer the heap cannot hold is refused whole, as no memory rather
    /// than as a malformed document.
    #[test]
    fn a_document_the_heap_cannot_hold_is_refused_for_memory() {
        assert_eq!(
            refusing_after(1, || EnrolmentOverride::parse(
                "fontd enabled
timed disabled
"
            )),
            Err(LocatedError::whole(EnrolError::OutOfMemory))
        );
        let overrides = EnrolmentOverride::parse(
            "fontd enabled
",
        )
        .expect("parses");
        assert_eq!(
            refusing_after(0, || overrides.to_store_text()),
            Err(EnrolError::OutOfMemory)
        );
    }

    #[test]
    fn the_writer_never_produces_a_document_the_reader_refuses() {
        let render = |count: usize| {
            let names: Vec<String> = (0..count)
                .map(|i| alloc::format!("{i:0MAX_SERVICE_NAME_LEN$}"))
                .collect();
            let mut overrides = EnrolmentOverride::empty();
            for name in &names {
                let _ = overrides
                    .set(&Enrolment::empty(), name, Enabled)
                    .expect("set");
            }
            overrides.to_store_text().map(|text| (text, overrides))
        };
        let entry = MAX_SERVICE_NAME_LEN + " enabled\n".len();
        let header = render(1).expect("fits").0.len() - entry;
        let fitting = (super::MAX_DOCUMENT_LEN - header) / entry;

        let (text, overrides) = render(fitting).expect("the largest document fits");
        assert!(text.len() <= super::MAX_DOCUMENT_LEN);
        assert_eq!(EnrolmentOverride::parse(&text), Ok(overrides));
        assert_eq!(render(fitting + 1).err(), Some(EnrolError::TooLong));
    }
}
