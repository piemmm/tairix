//! CallASWI as RISC OS 5 answers for it. The module brings `OS_CallASWI` and
//! the SWIs added since RISC OS 3.70 to systems older than them; RISC OS 5's
//! kernel performs each itself, so the module is present, at the version
//! that names every one of them, and provides nothing of its own.

use crate::module::Builtin;

/// CallASWI.
pub(crate) static CALL_A_SWI: Builtin = Builtin {
    title: "CallASWI",
    help_name: None,
    help_suffix: None,
    bridge: false,
    swis: None,
    services: None,
    commands: &[],
    init: None,
    finalise: None,
    start: None,
    routines: &[],
    vectors: &[],
    resources: &[],
    code: None,
};
