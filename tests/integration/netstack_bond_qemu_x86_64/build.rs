//! Build script for the x86_64 bond-failover netstack live-boot QEMU vertical
//! (`plans/NETWORK.md` N9b-3-2-β-2-ii-b-bond).
//!
//! One job on the freestanding `x86_64-tairix-none` target: hand the
//! production x86_64 kernel linker script to `rustc` — the single per-arch
//! script the architecture port owns, exactly as the sibling
//! `netstack_autoload_qemu_x86_64` / `netstack_static_qemu_x86_64` boot
//! verticals do (no duplication). QEMU's PVH `-kernel` loader enters the
//! kernel directly; the planted virtio-blk-pci disk and the two attached
//! `virtio-net-pci` devices populate the PCI bus the bootstrap-floor
//! virtio-PCI enumeration probes, so no boot media or embedded fixture is
//! needed.
//!
//! On any non-x86_64 target (host `cargo build --workspace`, clippy) it emits
//! only the target cfg; the kernel body that consumes the boot pipeline
//! compiles only for the freestanding x86_64 target.

fn main() {
    tairix_itest_harness::emit_target_cfg();
    println!("cargo:rerun-if-changed=build.rs");

    tairix_itest_harness::link_x86_64_kernel_layout();
}
