//! The fake kernel keeps the kernel's rules.

use tairix_abi::hwtree::HwResource;
use tairix_abi::{Errno, ProcId, PROC_ID_LEN};

use crate::testkit::FakeKernel;
use crate::SupplierHost;

const HOLDER: ProcId = ProcId::from_raw([1; PROC_ID_LEN]);
const OTHER: ProcId = ProcId::from_raw([2; PROC_ID_LEN]);

#[test]
fn a_grant_counts_only_for_its_holder_and_only_where_it_covers() {
    let kernel = FakeKernel::new();
    kernel.grant(HOLDER, HwResource::mmio(0x1000, 0x100));
    let held = kernel.call(HOLDER);
    let other = kernel.call(OTHER);
    assert_eq!(
        kernel.caller_holds(held, &HwResource::mmio(0x1040, 0x10)),
        Ok(true)
    );
    assert_eq!(
        kernel.caller_holds(held, &HwResource::mmio(0x10F0, 0x20)),
        Ok(false)
    );
    assert_eq!(
        kernel.caller_holds(other, &HwResource::mmio(0x1040, 0x10)),
        Ok(false)
    );
    kernel.revoke(HOLDER, &HwResource::mmio(0x1000, 0x100));
    assert_eq!(
        kernel.caller_holds(held, &HwResource::mmio(0x1040, 0x10)),
        Ok(false)
    );
}

#[test]
fn an_answered_or_retired_call_is_out_of_service() {
    let mut kernel = FakeKernel::new();
    let answered = kernel.call(HOLDER);
    let retired = kernel.call(HOLDER);
    assert_eq!(kernel.caller(answered), Ok(HOLDER));
    assert_eq!(kernel.reply(answered, b"ok"), Ok(()));
    assert_eq!(kernel.answer(answered).as_deref(), Some(&b"ok"[..]));
    assert_eq!(kernel.caller(answered), Err(Errno::NotFound));

    kernel.retire(retired);
    assert_eq!(kernel.caller(retired), Err(Errno::NotFound));
    assert_eq!(
        kernel.caller_holds(retired, &HwResource::mmio(0, 4)),
        Err(Errno::NotFound)
    );
    assert_eq!(kernel.reply(retired, b"late"), Err(Errno::NotFound));
    assert_eq!(kernel.answer(retired), None);
    assert_eq!(kernel.answered(), 1);
}

#[test]
#[should_panic(expected = "answered twice")]
fn answering_a_call_twice_fails_the_test() {
    let mut kernel = FakeKernel::new();
    let ticket = kernel.call(HOLDER);
    let _ = kernel.reply(ticket, b"once");
    let _ = kernel.reply(ticket, b"twice");
}

#[test]
fn an_ended_peer_cannot_be_watched_and_its_watch_is_dropped() {
    let mut kernel = FakeKernel::new();
    assert_eq!(kernel.watch(HOLDER), Ok(()));
    assert_eq!(kernel.watch(HOLDER), Ok(()));
    assert_eq!(kernel.watch(OTHER), Ok(()));
    assert_eq!(kernel.watched(), [HOLDER, OTHER]);
    kernel.end(HOLDER);
    assert_eq!(kernel.watched(), [OTHER]);
    assert_eq!(kernel.watch(HOLDER), Err(Errno::NotFound));
    kernel.unwatch(OTHER);
    assert!(kernel.watched().is_empty());
}
