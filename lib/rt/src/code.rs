//! Code a compiler sandbox writes for the process that started it to install
//! in another of its sandboxes (`code_install`, `plans/BROWSER.md` BR26).

use core::ptr::NonNull;

use tairix_abi::{Errno, MapFlags};

use crate::shm::{mapped_at, mapping};

/// Map fresh anonymous memory holding `code` and leave it mapped, for the
/// process that started this one to move into another of its sandboxes with
/// [`code_install`](crate::code_install); answers the base it is named by.
///
/// No value owns the pages: nothing in this process touches them again, so
/// their leaving it is never seen, and pages no install takes go with the
/// process.
///
/// # Errors
///
/// [`Errno::LengthOutOfRange`] for no code, `mem_map`'s refusal, and
/// [`Errno::BadAddress`] for a base no slice can be built over, which is
/// unmapped again.
pub fn stage_code(code: &[u8]) -> Result<u64, Errno> {
    if code.is_empty() {
        return Err(Errno::LengthOutOfRange);
    }
    let base = Errno::syscall_result(crate::mem_map(code.len(), MapFlags::empty(), 0))?;
    let Some(at) = mapped_at(base).and_then(mapping) else {
        let _ = crate::mem_unmap(base, code.len());
        return Err(Errno::BadAddress);
    };
    // SAFETY: the kernel has just mapped `code.len()` bytes, rounded up to
    // whole pages, writable at `base` for this call alone; nothing else in
    // this process knows the region.
    unsafe { place(at, code) };
    Ok(base)
}

/// Copies `code` to `to`.
///
/// # Safety
///
/// `to` is valid for `code.len()` bytes of writes that nothing else
/// references.
unsafe fn place(to: NonNull<u8>, code: &[u8]) {
    // SAFETY: as the caller promises.
    let region = unsafe { core::slice::from_raw_parts_mut(to.as_ptr(), code.len()) };
    region.copy_from_slice(code);
}

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;
    use std::vec;

    use tairix_abi::Errno;
    use tairix_abi_trap::seam::{self, refusal};

    use super::{place, stage_code};
    use crate::{NUM_MEM_MAP, NUM_MEM_UNMAP};

    #[test]
    fn no_code_is_staged_and_the_kernel_is_not_asked() {
        seam::no_trap(|| assert_eq!(stage_code(&[]), Err(Errno::LengthOutOfRange)));
    }

    #[test]
    fn a_refused_map_stages_nothing_and_says_why() {
        let (number, args) = seam::capture(refusal(Errno::OutOfMemory), || {
            assert_eq!(stage_code(&[0xC3; 5]), Err(Errno::OutOfMemory));
        });
        assert_eq!(number, NUM_MEM_MAP);
        assert_eq!(&args[..3], &[5, 0, 0], "the code's length, anywhere");
    }

    #[test]
    fn a_base_no_slice_can_be_built_over_is_given_back() {
        seam::arm_script(&[0], 0);
        assert_eq!(stage_code(&[0xC3; 5]), Err(Errno::BadAddress));
        let calls = seam::calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].0, NUM_MEM_UNMAP);
        assert_eq!(&calls[1].1[..2], &[0, 5]);
    }

    #[test]
    fn code_is_placed_whole_and_nothing_past_it_is_touched() {
        let mut memory = vec![0xAAu8; 8];
        let to = NonNull::new(memory.as_mut_ptr()).expect("a heap pointer");
        // SAFETY: `memory` holds eight bytes, of which three are written, and
        // nothing else borrows it while `place` runs.
        unsafe { place(to, &[1, 2, 3]) };
        assert_eq!(memory, [1, 2, 3, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA]);
    }
}
