//! The kernel's variables' tests: the return code's two, read and set
//! through `OS_ReadVarVal` and `OS_SetVarVal` as code variables are.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;

use crate::memory::layout::RISC_OS_5;
use crate::memory::Bits;
use crate::testkit::{with_application, Session, DATA};

const X: u32 = 0x2_0000;

/// `name`'s value read expanded, or its error's text.
fn read(s: &mut Session, name: &[u8]) -> Result<Vec<u8>, Vec<u8>> {
    s.write(DATA, name);
    s.write(DATA + crate::os::len32(name.len()), b"\0");
    let (regs, _) = s.user_swi(
        X | 0x23,
        [DATA, DATA + 0x100, 255, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    if regs.flag(psr::V) {
        return Err(s.error_text(regs.r[0]));
    }
    Ok(s.read(DATA + 0x100, regs.r[2] as usize))
}

/// `name` set to the string `value`, or its error's text.
fn set(s: &mut Session, name: &[u8], value: &[u8]) -> Result<(), Vec<u8>> {
    s.write(DATA, name);
    s.write(DATA + crate::os::len32(name.len()), b"\0");
    s.write(DATA + 0x100, value);
    let length = crate::os::len32(value.len());
    let (regs, _) = s.user_swi(
        X | 0x24,
        [DATA, DATA + 0x100, length, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    if regs.flag(psr::V) {
        return Err(s.error_text(regs.r[0]));
    }
    Ok(())
}

#[test]
fn the_return_code_and_its_limit_read_as_decimal_and_set_as_numbers() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    assert_eq!(read(&mut s, b"Sys$ReturnCode"), Ok(b"0".to_vec()));
    assert_eq!(read(&mut s, b"Sys$RCLimit"), Ok(b"256".to_vec()));
    assert_eq!(set(&mut s, b"Sys$RCLimit", b"100"), Ok(()));
    assert_eq!(read(&mut s, b"Sys$RCLimit"), Ok(b"100".to_vec()));
    assert_eq!(set(&mut s, b"Sys$ReturnCode", b"+7"), Ok(()));
    assert_eq!(read(&mut s, b"Sys$ReturnCode"), Ok(b"7".to_vec()));
    assert_eq!(
        set(&mut s, b"Sys$ReturnCode", b"101"),
        Err(b"Return code limit exceeded".to_vec())
    );
    assert_eq!(
        read(&mut s, b"Sys$ReturnCode"),
        Ok(b"101".to_vec()),
        "set though refused"
    );
    assert_eq!(
        set(&mut s, b"Sys$ReturnCode", b"-1"),
        Err(b"Return code limit exceeded".to_vec()),
        "negative, so past any limit"
    );
    assert_eq!(set(&mut s, b"Sys$ReturnCode", b"99999999999"), Ok(()));
    assert_eq!(
        read(&mut s, b"Sys$ReturnCode"),
        Ok(b"0".to_vec()),
        "too big for a word, nought"
    );
    assert_eq!(set(&mut s, b"Sys$RCLimit", b"-5"), Ok(()));
    assert_eq!(
        read(&mut s, b"Sys$RCLimit"),
        Ok(b"5".to_vec()),
        "a negative limit is its size"
    );
    assert_eq!(set(&mut s, b"Sys$RCLimit", b"x"), Ok(()));
    assert_eq!(
        read(&mut s, b"Sys$RCLimit"),
        Ok(b"0".to_vec()),
        "no number, nought"
    );
}

#[test]
fn the_kernel_s_strings_are_set_at_reset() {
    let mut s = with_application(&RISC_OS_5, Bits::Bit32);
    assert_eq!(read(&mut s, b"Alias$."), Ok(b"Cat ".to_vec()));
    assert_eq!(
        read(&mut s, b"Sys$DateFormat"),
        Ok(b"%24:%mi:%se %dy-%m3-%ce%yr".to_vec())
    );
}
