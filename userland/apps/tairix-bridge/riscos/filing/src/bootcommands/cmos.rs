//! `BootCommands`' CMOS commands: `*SaveCMOS`, the CMOS RAM saved as RISC OS
//! 5 saves it, and `*LoadCMOS`.
//!
//! A saved file is the 256 bytes in logical order, then a word naming the
//! release that saved them, 530 for 5.30, as type `&FF2`. As RISC OS 5's,
//! the file is opened as `OS_Find` opens one to write afresh and typed once
//! closed, and a write it refuses goes unreported: a locked file keeps its
//! bytes and takes the type. RISC OS 5 was recorded leaving the running
//! machine's CMOS as it was after `*LoadCMOS`, whatever the file held and
//! whether or not there was one; the emulator's session, whose CMOS starts
//! afresh each time, keeps its own the same way.

use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::release::RELEASE;
use tairix_riscos_kernel::swi::os::{self as swi, file, find, gbpb};
use tairix_riscos_kernel::swi::X;

/// The bytes of CMOS RAM a file holds.
const SIZE: u32 = 256;
/// A saved file's type: Configuration.
const CONFIGURATION: u32 = 0xFF2;
/// `OS_NVMemory`'s reason that reads bytes by logical location.
const READ_BLOCK: u32 = 3;

/// The release as a number: 530 for 5.30.
fn release() -> u32 {
    RELEASE
        .iter()
        .filter(|b| b.is_ascii_digit())
        .fold(0, |n, &b| n * 10 + u32::from(b - b'0'))
}

/// `*SaveCMOS <filename>`.
pub(super) fn save(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let name = regs.r[0];
    let length = SIZE + 4;
    os.below_supervisor_stack(length, |os, image| {
        let mut read = Regs::default();
        read.r[..4].copy_from_slice(&[READ_BLOCK, 0, image, SIZE]);
        os.perform(X | swi::NV_MEMORY, &mut read)?;
        os.write_word(image.wrapping_add(SIZE), release())?;
        let mut open = Regs::default();
        open.r[..2].copy_from_slice(&[find::OUTPUT, name]);
        os.perform(X | swi::FIND, &mut open)?;
        let handle = open.r[0];
        if handle != 0 {
            let mut write = Regs::default();
            write.r[..4].copy_from_slice(&[gbpb::WRITE, handle, image, length]);
            match os.perform(X | swi::GBPB, &mut write) {
                Ok(()) | Err(Fail::Error(_)) => {}
                Err(unwind) => return Err(unwind),
            }
            let mut close = Regs::default();
            close.r[1] = handle;
            os.perform(X | swi::FIND, &mut close)?;
        }
        let mut typed = Regs::default();
        typed.r[..3].copy_from_slice(&[file::SET_TYPE, name, CONFIGURATION]);
        os.perform(X | swi::FILE, &mut typed)
    })
}

/// `*LoadCMOS <filename>`: the running CMOS left as it is.
#[expect(clippy::unnecessary_wraps, reason = "a command's signature")]
pub(super) fn load(_os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_release_is_its_digits() {
        assert_eq!(super::release(), 530);
    }
}
