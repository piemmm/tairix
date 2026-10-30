//! Host tests for the floating-point state policy: the `sstatus.FS` field
//! accessors and the two decisions a trap makes. The register moves are
//! riscv64 assembly and are covered by the QEMU vertical.

use super::{keeps_kernel_file, on_entry, on_return, FpArea, Fs, OnReturn, TrapAnchor, FS_MASK};

#[test]
fn the_field_round_trips_every_encoding() {
    for (fs, raw) in [
        (Fs::Off, 0),
        (Fs::Initial, 1),
        (Fs::Clean, 2),
        (Fs::Dirty, 3),
    ] {
        assert_eq!(Fs::of(raw << 13), fs);
        assert_eq!(fs as u64, raw);
        assert_eq!(Fs::of(fs.written_into(0)), fs);
    }
}

#[test]
fn writing_the_field_disturbs_no_other_bit() {
    // Every bit outside the field must survive, in both directions.
    let noise = !FS_MASK;
    for fs in [Fs::Off, Fs::Initial, Fs::Clean, Fs::Dirty] {
        let written = fs.written_into(noise);
        assert_eq!(written & !FS_MASK, noise);
        assert_eq!(Fs::of(written), fs);
        assert_eq!(fs.written_into(0) & !FS_MASK, 0);
    }
}

#[test]
fn a_trap_saves_only_a_dirty_file_and_marks_it_clean() {
    // The saved copy then matches the registers, so a task that computes
    // without writing again is not saved twice.
    assert_eq!(on_entry(Fs::Dirty), Some(Fs::Clean));
    for fs in [Fs::Off, Fs::Initial, Fs::Clean] {
        assert_eq!(on_entry(fs), None, "{fs:?} needs no save");
    }
}

#[test]
fn a_task_owning_no_state_returns_with_floating_point_off() {
    // This is what makes the residue unreadable rather than merely stale:
    // a task with no state of its own cannot access the file at all.
    assert_eq!(on_return(false), OnReturn::LeaveOff);
    assert_eq!(on_return(true), OnReturn::Reload);
}

#[test]
fn a_fresh_area_owns_nothing() {
    assert!(!FpArea::EMPTY.owned());
    assert!(!TrapAnchor::EMPTY.fp.owned());
    assert_eq!(TrapAnchor::EMPTY.kernel_tp, 0);
}

#[test]
fn the_anchor_holds_the_kernel_tp_first_and_the_file_at_a_fixed_offset() {
    // The save/restore assembly names `regs` at 16 bytes into the area, and
    // the trap vector reads the kernel `tp` at offset 0 of the anchor.
    assert_eq!(core::mem::offset_of!(TrapAnchor, kernel_tp), 0);
    assert_eq!(core::mem::offset_of!(TrapAnchor, fp), 8);
    assert_eq!(core::mem::offset_of!(FpArea, owned), 0);
    assert_eq!(core::mem::offset_of!(FpArea, fcsr), 8);
    assert_eq!(core::mem::offset_of!(FpArea, regs), 16);
    // 16-byte aligned so the trap frame built below it stays aligned.
    assert_eq!(core::mem::align_of::<TrapAnchor>(), 16);
    assert!(core::mem::size_of::<TrapAnchor>().is_multiple_of(16));
}

/// Only a dirty file holds values the interrupted kernel code relies on, so
/// only it is kept across an S-mode trap.
#[test]
fn a_kernel_trap_keeps_only_a_dirty_file() {
    assert!(keeps_kernel_file(Fs::Dirty));
    for fs in [Fs::Off, Fs::Initial, Fs::Clean] {
        assert!(!keeps_kernel_file(fs), "{fs:?}");
    }
}
