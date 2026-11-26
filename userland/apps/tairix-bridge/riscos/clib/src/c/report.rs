//! What the C library tells the user of a program that cannot go on, on
//! `stderr` as RISC OS 5.30 writes it, and the program's end.

use tairix_riscos_kernel::os::{quietly, Fail, Os};

use super::stdio::stream::{self, flag, Stream};
use crate::kernel::exit::{self, Then};
use crate::kernel::statics::{at as kernel_at, Statics};
use crate::module::message;

/// The return code a program the library ends has.
const FAILURE: u32 = 1;

/// `text` written to `stderr`: through its stream where that is open to
/// write, else to the console, as before the streams are laid. What cannot
/// be written is let go, so a report never stops a program ending.
pub(crate) fn to_stderr(os: &mut Os<'_>, statics: Statics, text: &[u8]) -> Result<(), Fail> {
    Ok(quietly(written_to_stderr(os, statics, text))?)
}

fn written_to_stderr(os: &mut Os<'_>, statics: Statics, text: &[u8]) -> Result<(), Fail> {
    let mut error = Stream::load(os, stream::stderr(statics))?;
    if error.flag & flag::WRITE == 0 {
        stream::to_console(os, statics, text)?;
        return Ok(());
    }
    error.write_all(os, statics, text)?;
    error.store(os)
}

/// The library's message `token` on a line of its own on `stderr`, then the
/// program ended failing.
pub(crate) fn unrecoverable(os: &mut Os<'_>, statics: Statics, token: &str) -> Result<(), Fail> {
    let text = message(os, token)?;
    to_stderr(os, statics, &[&b"\n"[..], &text, b"\n"].concat())?;
    end_failing(os, statics)
}

/// The program ended with its finalisation, failing.
pub(crate) fn end_failing(os: &mut Os<'_>, statics: Statics) -> Result<(), Fail> {
    statics.write(os, kernel_at::RETURN_CODE, FAILURE)?;
    exit::finalise(os, statics, Then::Exit, 0)
}
