//! `OS_BGet` and `OS_BPut` (`plans/RISCOS-EMULATOR.md` §6): a byte at an open
//! file's pointer.
//!
//! The first read at a file's end answers with C set, R0 the handle as RISC
//! OS 5 leaves it; the next is "End of file", from FileSwitch's own flag
//! without asking the filing system, until the pointer moves or the file is
//! written.

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::os::{Fail, Os, Regs};

use super::open::status;
use super::{call, error, open};

/// `BGetV`'s default.
pub(crate) fn bget(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let handle = regs.r[1];
    let mut stream = open::stream(os, handle)?;
    if !stream.readable() {
        return Err(error::fail(os, error::NOT_FOR_READING, &[]));
    }
    if stream.has(status::AT_END) {
        return Err(error::fail(os, error::END_OF_FILE, &[]));
    }
    let got = match stream.buffer {
        Some(mut buffer) => {
            let got = open::get_buffered(os, &stream, &mut buffer);
            stream.buffer = Some(buffer);
            got
        }
        None => call::get_byte(os, &stream.calls, stream.handle),
    };
    let answer = match got {
        Ok(Some(byte)) => {
            stream.mark(status::AT_END, false);
            regs.r[0] = u32::from(byte);
            regs.set_flag(psr::C, false);
            Ok(())
        }
        Ok(None) => {
            stream.mark(status::AT_END, true);
            regs.r[0] = handle;
            regs.set_flag(psr::C, true);
            Ok(())
        }
        Err(fail) => Err(fail),
    };
    open::put(os, handle, stream)?;
    answer
}

/// `BPutV`'s default.
pub(crate) fn bput(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let handle = regs.r[1];
    let mut stream = open::stream(os, handle)?;
    if !stream.has(status::WRITE) {
        return Err(error::fail(os, error::NOT_FOR_UPDATE, &[]));
    }
    let byte = regs.byte(0);
    let put = match stream.buffer {
        Some(mut buffer) => {
            let put = open::put_buffered(os, &stream, &mut buffer, byte);
            stream.buffer = Some(buffer);
            put
        }
        None => call::put_byte(os, &stream.calls, stream.handle, byte),
    };
    stream.mark(status::AT_END, false);
    stream.mark(status::MODIFIED, true);
    open::put(os, handle, stream)?;
    put
}
