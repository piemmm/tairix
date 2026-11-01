//! The variable an operator raises every step's budget with, named once for
//! xtask and the tests that start it.

/// Raises every step's budget for a slower machine without editing the
/// built-in budgets; unset by default.
pub(crate) const TIMEOUT_ENV_VAR: &str = "TAIRIX_XTASK_TIMEOUT_SECS";
