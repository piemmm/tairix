//! What the validator's tests write modules with: the crate's test assembler,
//! and the refusal a module text earns.

use alloc::string::String;

pub(crate) use crate::assemble::{admit, admit_version, assemble};
use crate::{Rule, Version};

/// The newest version, which the tests write unless they say otherwise.
pub(crate) const V1_6: Version = Version::NEWEST;

/// The rule `text` is refused by.
pub(crate) fn refused(text: &str) -> Rule {
    admit(text).expect_err("refused").rule
}

/// `text` with `from` replaced by `to`, which must occur in it.
pub(crate) fn with(text: &str, from: &str, to: &str) -> String {
    assert!(text.contains(from), "{from:?} is not in the module");
    text.replacen(from, to, 1)
}
