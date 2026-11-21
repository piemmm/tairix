//! What the VDU drivers' integration tests share: a session with the drivers
//! and their module, the VDU stream written, and whether the drivers still
//! answer.

#![allow(
    dead_code,
    reason = "each test crate including this uses the helpers its own tests need"
)]

use tairix_riscos_filing::session::{session, Session, Setup};
use tairix_riscos_kernel::os::{Fail, Regs};
use tairix_riscos_kernel::swi::os::{READ_MODE_VARIABLE, WRITE_C};
use tairix_riscos_kernel::swi::X;

/// The application space a session has.
pub const BYTES: u32 = 0x10_0000;

/// A session with the drivers and their module, `keys` to type.
pub fn drivers(keys: &[u8]) -> Session {
    session(&Setup {
        vdu: Some(tairix_riscos_vdu::DRIVERS),
        modules: &tairix_riscos_vdu::MODULES,
        bytes: BYTES,
        keys,
        disc: None,
    })
}

/// `bytes` written to the VDU a character at a time.
pub fn vdu(session: &mut Session, bytes: &[u8]) -> Result<(), Fail> {
    for &byte in bytes {
        let mut regs = Regs::new([u32::from(byte), 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        session.kernel.os().perform(X | WRITE_C, &mut regs)?;
    }
    Ok(())
}

/// Whether the drivers still read a mode variable.
pub fn answers(session: &mut Session) -> bool {
    let mut regs = Regs::new([u32::MAX, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    session
        .kernel
        .os()
        .perform(X | READ_MODE_VARIABLE, &mut regs)
        .is_ok()
}
