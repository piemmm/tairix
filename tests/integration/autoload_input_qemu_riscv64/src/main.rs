//! `plans/NETWORK.md` N4e-riscv64 (first stage) QEMU integration test: boot
//! the production riscv64 (QEMU `virt` / SiFive) `tairix-kernel` pipeline with
//! a planted whole-disk encrypted-root image that carries the **kernel-signed
//! virtio-input keyboard driver bundle** in its always-readable `/System`
//! store, plus an attached `virtio-keyboard-device`, and prove the full
//! **driver-loading-by-discovery autoload path**: one user-space driver
//! instance for the discovered virtio-input node delivering a typed key to the
//! kernel input-focus arbiter.
//!
//! ## What this test asserts — and how it differs from its siblings
//!
//! * `spawn_init_qemu_riscv64` proves PID 1 reaches U-mode and traps back.
//! * `input_virtio_mmio_qemu_riscv64` proves a discovered virtio-input device
//!   reaches an *in-kernel* scaffold decode path (load → use → unload).
//!
//! This vertical composes autoload on the production boot path: it attaches
//! the shared `tairix_test_encrypted_root_image` whole-disk image, planted by
//! the `image_drivers` pipeline with the signed `virtio_kbd` bundle at the
//! `/System`-volume-relative `Drivers/input/virtio_kbd/Run` (cross-compiled
//! for riscv64), as a virtio-blk-mmio device **plus** a `virtio-keyboard-
//! device`, and boots `boot_riscv64::boot` verbatim. The production path then:
//!
//! 1. **Discovers** the virtio-block root *and* the virtio-input node
//!    (bootstrap-floor virtio-MMIO enumeration). The input node carries its
//!    register window, a coherent DMA constraint, **and** its discovered PLIC
//!    interrupt line as capability-grant requests.
//! 2. **Admits the unlock kthread**, which brings the root block device up
//!    over the production PLIC device-IRQ path, mounts the always-readable
//!    `/System` volume, and serves its signed driver store over the
//!    capability-gated IPC endpoint. Crucially the store binds **independently
//!    of** the encrypted-root passphrase (the riscv64 SBI console exposes no
//!    interactive input this slice, so the interactive unlock fails closed) —
//!    so the keyboard driver still autoloads.
//! 3. **Reactive user-space autoload (Design D)**: the long-running `devmgr`
//!    service reads the hardware tree, lists the `/System` store over the IPC
//!    service, matches the signed bundle to the discovered virtio-input node
//!    (`lib/devmatch`), and asks the kernel to load it; the kernel re-runs the
//!    full signed gate (verified against the embedded
//!    `KERNEL_DRIVER_SIGNER_PUBKEY`) and **spawns it into its own user-space
//!    process** with exactly the node's resource grants.
//! 4. The spawned driver instance maps its register window, brings its
//!    virtio-input device up, **then binds its granted interrupt line and
//!    parks on `irq_wait`** (interrupt-driven, never a busy poll), and on each
//!    device interrupt pumps decoded events into the arbiter via `key_inject`.
//!
//! ## Why the PASS keys on one witness
//!
//! The audit sink reports PASS once it has seen `AuditEvent::InputDelivered`
//! with `kind=key` — the one-shot witness the `key_inject` handler emits the
//! first time a keyboard-class driver delivers to the arbiter. Reaching it
//! requires every preceding step to have succeeded: the `/System` volume
//! mounted and served, the store listed, the signed bundle verified, the node
//! matched, one user-space process spawned with exactly its node's grants, the
//! device brought up, the driver's interrupt armed, and the typed key decoded
//! and delivered. A run where any step fails never reaches the witness, so the
//! harness times out — the documented fail-loud behaviour.
//!
//! This vertical deliberately does **not** key on the encrypted-root unlock
//! (`UsersDbLoaded`): the riscv64 SBI console has no interactive input drain
//! this slice, so no passphrase can be typed and the unlock fails closed by
//! design. Proving the passphrase-typed unlock and the desktop click-through
//! is the aarch64 `autoload_input_qemu_aarch64` vertical's job (a display
//! world); this one proves the autoload-input path end to end on riscv64.
//!
//! ## Real firmware device tree
//!
//! QEMU's riscv64 `virt` OpenSBI firmware hands the boot hart a valid
//! device-tree pointer in `a1`, so — unlike the aarch64 `-kernel` path — this
//! vertical forwards the verbatim pointer to the boot pipeline, which
//! discovers the board (including the `virtio,mmio` transport slots the disk
//! and the keyboard populate) from it exactly as it would from real firmware.
//!
//! ## How it differs from a production kernel
//!
//! It reuses the entire production riscv64 boot pipeline and only replaces the
//! audit sink. Splitting the audit-observer behaviour into a separate bin
//! (instead of a Cargo feature on a production crate) prevents feature
//! unification from leaking the QEMU-exit shortcut into any production build
//! (fail closed; the harness never decides what the kernel does next).

#![cfg_attr(itest_riscv64, no_std)]
#![cfg_attr(itest_riscv64, no_main)]
#![deny(missing_docs)]

// --- Freestanding test bin (`riscv64gc-unknown-none-elf`) ----------

#[cfg(itest_riscv64)]
mod kernel;

// --- Host stub -----------------------------------------------------
#[cfg(not(itest_riscv64))]
fn main() {}
