use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::*;
use crate::cpu::jit::dispatch::Waits;
use crate::cpu::jit::frame::at;

struct Quiet;

impl Waits for Quiet {
    fn take(&mut self, _: u64) -> u32 {
        0
    }

    fn next_refresh(&self) -> u64 {
        u64::MAX
    }
}

/// Stands in for a block: AX counted up.
#[allow(unsafe_code)] // Entered as a block's code is.
unsafe extern "C" fn counting(entered: *mut Entered<'_>, _: *const u64, _: *const Helpers) -> u64 {
    // SAFETY: entered as `Entry` says, with the frame live and unborrowed.
    let frame = unsafe { &mut (*entered).frame };
    frame.set_u16(at::GPR, frame.u16(at::GPR).wrapping_add(1));
    Exit::Continue as u64
}

#[test]
#[allow(unsafe_code)] // An executor made to run `counting`.
fn code_made_of_its_address_runs_as_the_function_there() {
    // SAFETY: the one block adopted is `counting`, entered as `Entry` says.
    let mut native = unsafe { Native::new(Box::new(|_, _| {})) };
    let entry = entry_at(counting as Entry as usize as u64).unwrap();
    let code = native.install(entry, 0x10_0000, 64).unwrap();
    let mut store = vec![0u8; 16];
    let base = store.as_ptr().addr() as u64;
    let mut env = Env {
        store: &mut store,
        base,
        waits: &mut Quiet,
    };
    let mut frame = Frame::default();
    let exit = native.run(code, &mut frame, &PageMap::default(), &mut env);
    assert_eq!(exit, Some(Exit::Continue));
    assert_eq!(frame.u16(at::GPR), 1);
}

#[test]
#[allow(unsafe_code)] // An executor that runs nothing.
fn code_there_is_no_room_to_keep_is_given_back_not_leaked() {
    let unmapped: Rc<RefCell<Vec<(u64, u64)>>> = Rc::default();
    unmapped.borrow_mut().reserve(1);
    let record = Rc::clone(&unmapped);
    // SAFETY: nothing adopted here is run.
    let mut native =
        unsafe { Native::new(Box::new(move |at, len| record.borrow_mut().push((at, len)))) };
    let page = PAGE_SIZE as u64;
    let adopted = tairix_fuzzseed::meter::refusing_after(0, || native.adopt(page, 64));
    assert_eq!(adopted, None);
    assert_eq!(*unmapped.borrow(), [(page, 64)]);
    assert_eq!(native.held(), 0);
}
