use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::*;
use crate::cpu::jit::dispatch::Waits;
use crate::cpu::jit::frame::at;

/// A bus whose refresh falls every `period` cycles from `due`, taking
/// `cost` waits each time, which records every cycle it was asked at.
struct Bus {
    due: u64,
    period: u64,
    cost: u32,
    asked: Vec<u64>,
}

impl Waits for Bus {
    fn take(&mut self, at: u64) -> u32 {
        self.asked.push(at);
        if at < self.due {
            return 0;
        }
        self.due += self.period;
        self.cost
    }

    fn next_refresh(&self) -> u64 {
        self.due
    }
}

fn bus() -> Bus {
    Bus {
        due: 1_000,
        period: 72,
        cost: 4,
        asked: Vec::new(),
    }
}

/// What a test's executor gave back.
type Unmapped = Rc<RefCell<Vec<(u64, u64)>>>;

#[allow(unsafe_code)] // An executor made to run the stand-ins below.
fn executor() -> (Native, Unmapped) {
    let unmapped = Unmapped::default();
    let record = Rc::clone(&unmapped);
    // SAFETY: the tests hand `adopt` no address it would take: every block
    // they run is installed as the function it is.
    let native =
        unsafe { Native::new(Box::new(move |at, len| record.borrow_mut().push((at, len)))) };
    (native, unmapped)
}

/// Runs the block `code` names on `frame` over `pages` and `bus`, with a
/// store of its own.
fn run(
    native: &mut Native,
    code: u64,
    frame: &mut Frame,
    pages: &PageMap,
    bus: &mut Bus,
) -> Option<Exit> {
    let mut store = vec![0u8; 64];
    let base = store.as_ptr().addr() as u64;
    let mut env = Env {
        store: &mut store,
        base,
        waits: bus,
    };
    native.run(code, frame, pages, &mut env)
}

/// Stands in for a block: AX counted up, IP moved on, run on into the next.
#[allow(unsafe_code)] // Entered as a block's code is.
unsafe extern "C" fn counting(entered: *mut Entered<'_>, _: *const u64, _: *const Helpers) -> u64 {
    // SAFETY: entered as `Entry` says, with the frame live and unborrowed.
    let frame = unsafe { &mut (*entered).frame };
    frame.set_u16(at::GPR, frame.u16(at::GPR).wrapping_add(1));
    frame.set_u16(at::IP, frame.u16(at::IP).wrapping_add(3));
    Exit::Continue as u64
}

/// Stands in for a block that reaches the bus twice: refresh taken at its
/// cycle count and a hundred cycles on, the waits added to AX.
#[allow(unsafe_code)] // Entered as a block's code is.
unsafe extern "C" fn refreshing(
    entered: *mut Entered<'_>,
    _: *const u64,
    helpers: *const Helpers,
) -> u64 {
    // SAFETY: entered as `Entry` says: the frame is read and written only
    // between the calls, and the table's refresh is handed back the pointer
    // the block was entered with, as generated code hands it.
    unsafe {
        let cycles = (*entered).frame.u64(at::CYCLES);
        let first = ((*helpers).refresh)(entered, cycles);
        let second = ((*helpers).refresh)(entered, cycles + 100);
        let waits = u16::try_from(first + second).unwrap_or(u16::MAX);
        let ax = (*entered).frame.u16(at::GPR);
        (*entered).frame.set_u16(at::GPR, ax.wrapping_add(waits));
    }
    Exit::Step as u64
}

/// Stands in for a block that reads the page map: page 3's two words left
/// in the frame's cycle and limit fields.
#[allow(unsafe_code)] // Entered as a block's code is.
unsafe extern "C" fn paging(
    entered: *mut Entered<'_>,
    pages: *const u64,
    _: *const Helpers,
) -> u64 {
    // SAFETY: entered as `Entry` says: `pages` is the map's words, which
    // hold page 3's entry, and the frame is live and unborrowed.
    unsafe {
        let (base, kind) = (*pages.add(6), *pages.add(7));
        (*entered).frame.set_u64(at::CYCLES, base);
        (*entered).frame.set_u64(at::LIMIT, kind);
    }
    Exit::Limit as u64
}

/// Stands in for code that answers no exit there is.
#[allow(unsafe_code)] // Entered as a block's code is.
unsafe extern "C" fn lost(entered: *mut Entered<'_>, _: *const u64, _: *const Helpers) -> u64 {
    // SAFETY: entered as `Entry` says, with the frame live and unborrowed.
    unsafe { (*entered).frame.set_u16(at::GPR, 0xDEAD) };
    7
}

fn frame() -> Frame {
    let mut frame = Frame::default();
    frame.set_u16(at::GPR, 0x1233);
    frame.set_u16(at::IP, 0x0100);
    frame.set_u64(at::CYCLES, 990);
    frame.set_u64(at::REFRESH, 1_000);
    frame
}

#[test]
fn a_block_is_entered_with_its_frame_and_leaves_it_as_it_wrote_it() {
    let (mut native, _) = executor();
    let code = native.install(counting, 0x10_0000, 64).unwrap();
    let mut frame = frame();
    let mut expected = frame.clone();
    expected.set_u16(at::GPR, 0x1234);
    expected.set_u16(at::IP, 0x0103);
    let exit = run(
        &mut native,
        code,
        &mut frame,
        &PageMap::default(),
        &mut bus(),
    );
    assert_eq!(exit, Some(Exit::Continue));
    assert_eq!(frame, expected, "every other byte as it was");
}

#[test]
fn the_refresh_helper_reaches_the_bus_the_block_was_entered_over() {
    let (mut native, _) = executor();
    let code = native.install(refreshing, 0x10_0000, 64).unwrap();
    let mut frame = frame();
    let mut bus = bus();
    let exit = run(&mut native, code, &mut frame, &PageMap::default(), &mut bus);
    assert_eq!(exit, Some(Exit::Step));
    assert_eq!(
        bus.asked,
        [990, 1_090],
        "asked at each cycle the block named"
    );
    assert_eq!(frame.u16(at::GPR), 0x1233 + 4, "one refresh fell due");
    assert_eq!(
        frame.u64(at::REFRESH),
        1_072,
        "the frame told where the next falls"
    );
}

#[test]
fn the_page_map_is_handed_to_the_block_as_it_stands() {
    let (mut native, _) = executor();
    let code = native.install(paging, 0x10_0000, 64).unwrap();
    let mut pages = PageMap::default();
    pages.set(3, 0xABCD_0000, 2, true);
    let mut frame = frame();
    let exit = run(&mut native, code, &mut frame, &pages, &mut bus());
    assert_eq!(exit, Some(Exit::Limit));
    assert_eq!(frame.u64(at::CYCLES), 0xABCD_0000);
    assert_eq!(
        frame.u64(at::LIMIT),
        pages
            .load(3 * crate::cpu::jit::page::ENTRY_BYTES + 8, 8)
            .unwrap()
    );
}

#[test]
fn code_that_answers_no_exit_and_a_handle_that_names_none_are_refused() {
    let (mut native, _) = executor();
    let code = native.install(lost, 0x10_0000, 64).unwrap();
    let mut frame = frame();
    let pages = PageMap::default();
    assert_eq!(run(&mut native, code, &mut frame, &pages, &mut bus()), None);
    assert_eq!(
        frame.u16(at::GPR),
        0xDEAD,
        "what it wrote is the caller's to discard"
    );
    let mut untouched = self::frame();
    for handle in [code + 1, u64::MAX] {
        assert_eq!(
            run(&mut native, handle, &mut untouched, &pages, &mut bus()),
            None
        );
    }
    assert_eq!(untouched, self::frame(), "no block ran");
}

#[test]
fn every_pure_helper_in_the_table_is_the_interpreter_s() {
    let mut rng = tairix_fuzzseed::Prng::new(0x4E47_0001);
    for _ in 0..500 {
        let args: [u64; MOST_ARGS] = core::array::from_fn(|_| rng.next_u64() >> rng.below(64));
        for (n, helper) in (1..).zip(HELPERS.pure) {
            assert_eq!(
                helper(args[0], args[1], args[2], args[3], args[4]),
                helper::call(n, &args).unwrap(),
                "helper {n} at {args:x?}"
            );
        }
    }
}

#[test]
fn code_given_back_is_unmapped_whole_and_its_pages_no_longer_held() {
    let (mut native, unmapped) = executor();
    let first = native.install(counting, 0x10_0000, 5_000).unwrap();
    let second = native.install(counting, 0x20_0000, 1).unwrap();
    assert_eq!(native.held(), 3 * PAGE_SIZE as u64, "whole pages");
    native.release(first);
    assert_eq!(*unmapped.borrow(), [(0x10_0000, 5_000)]);
    assert_eq!(native.held(), PAGE_SIZE as u64);
    let mut frame = frame();
    let pages = PageMap::default();
    assert_eq!(
        run(&mut native, first, &mut frame, &pages, &mut bus()),
        None
    );
    native.release(first);
    native.release(u64::MAX);
    assert_eq!(unmapped.borrow().len(), 1, "nothing given back twice");
    let third = native.install(counting, 0x30_0000, 64).unwrap();
    assert_eq!(third, first, "a handle given back is taken again");
    assert_eq!(
        run(&mut native, second, &mut frame, &pages, &mut bus()),
        Some(Exit::Continue)
    );
}

#[test]
fn every_block_still_held_is_given_back_with_the_executor() {
    let (mut native, unmapped) = executor();
    let first = native.install(counting, 0x10_0000, 64).unwrap();
    native.install(counting, 0x20_0000, 64).unwrap();
    native.release(first);
    drop(native);
    assert_eq!(*unmapped.borrow(), [(0x10_0000, 64), (0x20_0000, 64)]);
}

#[test]
fn what_cannot_be_a_block_s_code_is_not_taken() {
    let (mut native, unmapped) = executor();
    let page = PAGE_SIZE as u64;
    for (address, len) in [
        (0, 64),
        (page + 8, 64),
        (page, 0),
        (page, MOST_CODE_BYTES as u64 + 1),
        (u64::MAX - page + 1, 64),
    ] {
        assert_eq!(native.adopt(address, len), None, "{address:#x}, {len}");
    }
    assert!(
        unmapped.borrow().is_empty(),
        "nothing it did not take is unmapped"
    );
    assert_eq!(native.held(), 0);
}
