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
//! has not spoken about. [`effective`] is the one fold of the two.
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
/// whole ([`EnrolError::TooLong`]) by every reader alike.
pub const MAX_DOCUMENT_LEN: usize = 64 * 1024;

/// Why an enrolment store text, or an [`enrol`] / [`unenrol`] request, was
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
    /// [`unenrol`] named a service that is not currently enrolled; nothing
    /// changed.
    NotEnrolled,
    /// An override line's disposition word was neither `enabled` nor
    /// `disabled`, or the line carried more than a name and a disposition.
    ServiceEnrolmentInvalid,
    /// The document is longer than [`MAX_DOCUMENT_LEN`].
    TooLong,
}

impl fmt::Display for EnrolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NameEmpty => f.write_str("a service name is empty"),
            Self::NameTooLong => f.write_str("a service name is too long"),
            Self::NameInvalid => f.write_str("a service name contains an invalid character"),
            Self::Duplicate => f.write_str("a service is enrolled more than once"),
            Self::NotEnrolled => f.write_str("the service is not enrolled"),
            Self::ServiceEnrolmentInvalid => {
                f.write_str("an override line is not `<service> enabled|disabled`")
            }
            Self::TooLong => f.write_str("the enrolment document is too long"),
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

/// Produce the enrolment with `name` **enabled**.
///
/// Idempotent: enabling an already-enrolled service returns an equal set. The
/// operation is pure — it returns the *new* enrolment for the caller to
/// persist — and it decides eligibility only. It cannot widen authority
/// because it never names one: the kernel derives a service's grant from its
/// signed bundle and its account's ceiling at spawn, and the service
/// manager's authority scope decides which accounts it may enrol at all.
///
/// # Errors
///
/// A name defect ([`EnrolError::NameEmpty`] / `NameTooLong` / `NameInvalid`).
pub fn enrol(current: &Enrolment, name: &str) -> Result<Enrolment, EnrolError> {
    validate_service_name(name)?;
    let mut names = current.names.clone();
    if let Err(at) = names.binary_search_by(|held| held.as_str().cmp(name)) {
        names.insert(at, String::from(name));
    }
    Ok(Enrolment { names })
}

/// Produce the enrolment with `name` **disabled**.
///
/// The operation is pure — it returns the new enrolment for the caller to
/// write back. Disabling requires no capability check (removing eligibility
/// only ever narrows authority) but does fail closed if the service was not
/// enrolled, so a control tool reports honestly that nothing changed.
///
/// # Errors
///
/// A name defect, or [`EnrolError::NotEnrolled`] if `name` is not currently
/// enrolled.
pub fn unenrol(current: &Enrolment, name: &str) -> Result<Enrolment, EnrolError> {
    validate_service_name(name)?;
    if !current.is_enabled(name) {
        return Err(EnrolError::NotEnrolled);
    }
    let names = current
        .names
        .iter()
        .filter(|n| n.as_str() != name)
        .cloned()
        .collect();
    Ok(Enrolment { names })
}

/// The administrator's override layer: the services whose enrolment differs
/// from the image's [`Enrolment`], and how.
///
/// A service the administrator has not spoken about simply has no entry, so
/// there is no third state meaning "unspecified" — the image's layer decides
/// it.
///
/// Held sorted and unique so the document has one canonical serialisation,
/// and a growable capacity like the vendor layer — the only fixed bound is a
/// single name's length.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EnrolmentOverride {
    /// `(service name, disposition)`, validated, unique, ascending by name.
    entries: Vec<(String, ServiceEnrolment)>,
}

impl EnrolmentOverride {
    /// The empty override layer: the image's enrolment stands unchanged.
    ///
    /// The fail-closed resolution of a **missing** document (no administrator
    /// has changed anything, or the encrypted root is not mounted) and of a
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
    /// [`EnrolError::Duplicate`] at a service's second appearance.
    pub fn parse(text: &str) -> Result<Self, LocatedError> {
        if text.len() > MAX_DOCUMENT_LEN {
            return Err(LocatedError::whole(EnrolError::TooLong));
        }
        let mut entries: Vec<(String, ServiceEnrolment)> = Vec::new();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
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
            if !seen.insert(name) {
                return Err(refused(EnrolError::Duplicate));
            }
            entries.push((String::from(name), disposition));
        }
        entries.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
        Ok(Self { entries })
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

    /// Serialise to the canonical document text.
    ///
    /// Round-trips with [`parse`](Self::parse).
    #[must_use]
    pub fn to_store_text(&self) -> String {
        let mut out = String::from(
            "# TAIRiX service enrolment overrides. One `<service> enabled|disabled` per line.\n",
        );
        for (name, disposition) in &self.entries {
            out.push_str(name);
            out.push(' ');
            out.push_str(disposition.as_str());
            out.push('\n');
        }
        out
    }
}

/// The enrolment a manager obeys: `vendor` with `overrides` applied.
///
/// The one definition of the precedence, so no consumer re-derives it.
#[must_use]
pub fn effective(vendor: &Enrolment, overrides: &EnrolmentOverride) -> Enrolment {
    let mut names: BTreeSet<&str> = vendor
        .names()
        .iter()
        .map(String::as_str)
        .filter(|n| overrides.disposition(n) != Some(ServiceEnrolment::Disabled))
        .collect();
    for (name, disposition) in overrides.entries() {
        if disposition.is_enabled() {
            names.insert(name);
        }
    }
    Enrolment {
        names: names.into_iter().map(String::from).collect(),
    }
}

/// The override layer that makes `desired` the [`effective`] enrolment over
/// `vendor`.
///
/// Self-minimising by construction: a service whose desired state already
/// matches the image's default gets no entry, so the document holds only what
/// was changed and a system update shipping a different default reaches
/// everything the administrator has not spoken about.
#[must_use]
pub fn overrides_for(vendor: &Enrolment, desired: &Enrolment) -> EnrolmentOverride {
    let mut entries: Vec<(String, ServiceEnrolment)> = Vec::new();
    for name in desired.names() {
        if !vendor.is_enabled(name) {
            entries.push((name.clone(), ServiceEnrolment::Enabled));
        }
    }
    for name in vendor.names() {
        if !desired.is_enabled(name) {
            entries.push((name.clone(), ServiceEnrolment::Disabled));
        }
    }
    entries.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
    EnrolmentOverride { entries }
}

#[cfg(test)]
mod tests {
    use super::{
        effective, enrol, overrides_for, unenrol, EnrolError, Enrolment, EnrolmentOverride,
        LocatedError, ServiceEnrolment, MAX_SERVICE_NAME_LEN,
    };
    use alloc::string::String;
    use alloc::vec::Vec;

    fn names(e: &Enrolment) -> Vec<&str> {
        e.names().iter().map(String::as_str).collect()
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
    fn enrolling_keeps_the_set_sorted() {
        let e = Enrolment::of(["b", "d"]).expect("valid");
        let e = enrol(&e, "c").expect("enrols");
        let e = enrol(&e, "a").expect("enrols");
        assert_eq!(names(&e), ["a", "b", "c", "d"]);
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
    fn enrol_is_idempotent_and_adds_a_service() {
        let e0 = Enrolment::empty();
        let e1 = enrol(&e0, "netstack").expect("enrols");
        assert!(e1.is_enabled("netstack"));
        // Enabling again changes nothing.
        assert_eq!(e1, enrol(&e1, "netstack").expect("idempotent"));
    }

    #[test]
    fn enrol_rejects_an_invalid_name() {
        assert_eq!(
            enrol(&Enrolment::empty(), "Bad Name"),
            Err(EnrolError::NameInvalid),
        );
    }

    #[test]
    fn unenrol_removes_a_service_and_fails_closed_on_absent() {
        let e = Enrolment::of(["a", "b", "c"]).expect("valid names");
        let after = unenrol(&e, "b").expect("removes");
        assert_eq!(names(&after), ["a", "c"]);
        assert!(!after.is_enabled("b"));
        // Disabling a service that is not enrolled fails closed.
        assert_eq!(unenrol(&e, "z"), Err(EnrolError::NotEnrolled));
    }

    #[test]
    fn an_absent_or_corrupt_override_document_leaves_the_image_layer_standing() {
        let vendor = Enrolment::of(["netstack", "timed"]).expect("valid names");
        // Missing document.
        assert_eq!(effective(&vendor, &EnrolmentOverride::empty()), vendor);
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

        let eff = effective(&vendor, &overrides);
        assert_eq!(names(&eff), ["fontd", "netstack"]);
    }

    #[test]
    fn override_text_round_trips_canonically() {
        let overrides =
            EnrolmentOverride::parse("timed disabled\nfontd enabled\n").expect("parses");
        let text = overrides.to_store_text();
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
    fn overrides_for_records_only_what_differs_from_the_image() {
        let vendor = Enrolment::of(["netstack", "timed"]).expect("valid names");

        // Disabling one of the image's services records exactly that.
        let desired = unenrol(&vendor, "timed").expect("removes");
        let overrides = overrides_for(&vendor, &desired);
        assert_eq!(
            overrides.entries(),
            [(String::from("timed"), ServiceEnrolment::Disabled)]
        );
        assert_eq!(effective(&vendor, &overrides), desired);

        // Re-enabling it empties the document rather than pinning the
        // default: a later image that ships `timed` disabled must then be
        // obeyed, because the administrator is no longer speaking about it.
        let back = enrol(&desired, "timed").expect("enrols");
        assert!(overrides_for(&vendor, &back).is_empty());

        // Enabling something the image does not ship records an Enabled entry.
        let extra = enrol(&vendor, "fontd").expect("enrols");
        assert_eq!(
            overrides_for(&vendor, &extra).entries(),
            [(String::from("fontd"), ServiceEnrolment::Enabled)]
        );
    }

    #[test]
    fn effective_and_overrides_for_are_inverse_over_every_pair() {
        // Whatever the administrator wants, the derived document reproduces
        // it exactly over the image layer — the property both PID 1's boot
        // read and its control path rely on.
        let vendor = Enrolment::of(["a", "b", "c"]).expect("valid names");
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
            let desired = Enrolment::of(wanted.iter().copied()).expect("valid names");
            let overrides = overrides_for(&vendor, &desired);
            assert_eq!(
                effective(&vendor, &overrides),
                desired,
                "round trip failed for {wanted:?}"
            );
            // The derived document is itself canonical.
            assert_eq!(
                EnrolmentOverride::parse(&overrides.to_store_text()).expect("reparses"),
                overrides
            );
        }
    }
}
