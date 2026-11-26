//! The client's side of a call: the arguments it stacked past R3, and its
//! own procedures called from inside the library, in the caller's own mode
//! on its stack.

use tairix_riscos_kernel::os::{Entry, Fail, Os, Regs, Target};

/// The first `N` words the caller stacked, its arguments after R3.
pub(crate) fn stacked<const N: usize>(os: &mut Os<'_>) -> Result<[u32; N], Fail> {
    let sp = os.registers().r[13];
    let mut words = [0; N];
    os.as_caller().read_words(sp, &mut words)?;
    Ok(words)
}

/// The client's procedure at `address` called with `regs`, R0 to R12 back as
/// it left them. A User mode caller's runs on `sp`; a privileged caller's on
/// the supervisor stack as it stands, which is the caller's own.
pub(crate) fn call(os: &mut Os<'_>, address: u32, regs: &mut Regs, sp: u32) -> Result<(), Fail> {
    let bits = os.caller_bits();
    let entry = if os.caller_privileged() {
        Entry::Supervisor
    } else {
        Entry::User(sp)
    };
    os.call(Target { address, bits }, regs, entry)?;
    Ok(())
}
