//! `plans/PI.md` P10 5d-2-ii(b-2-iii) + `plans/DISPLAY.md` D7d (first
//! stage) QEMU integration test: boot the production aarch64
//! `tairix-kernel` pipeline on the `virt` board **as a display world** —
//! a `ramfb` display beside the virtio keyboard and mouse — with a
//! planted whole-disk encrypted-root image that carries the
//! **kernel-signed virtio-input driver bundle and the framebuffer
//! display-service bundle** in `/System/Drivers/`, and prove the full
//! **driver-loading-by-discovery autoload path**: one user-space driver
//! instance per discovered virtio-input node (keyboard + mouse)
//! delivering typed keys and an injected mouse motion to the kernel
//! input-focus arbiter, the typed passphrase unlocking the encrypted
//! root through the video console end to end, and the display service
//! autoloading against the boot display node the kernel publishes for
//! its ramfb scan-out surface and binding the reserved
//! `DISPLAY_ENDPOINT`.
//!
//! ## What this test asserts — and how it differs from its siblings
//!
//! * `root_unlock_admission_qemu_aarch64` proves the in-kernel unlock kthread
//!   mounts the encrypted root and installs the users database (the
//!   *root-mount* path). It attaches no keyboard and plants no driver store.
//! * `input_virtio_mmio_qemu_aarch64` proves a discovered virtio-input device
//!   reaches the *in-kernel* scaffold decode path.
//! * `driver_spawn_qemu_aarch64` proves a discovered node → signed gate →
//!   process spawn handshake with a stub program.
//!
//! This vertical composes them on the production boot path: it attaches the
//! shared `tairix_test_encrypted_root_image` whole-disk image, additionally
//! planted with the autoload driver bundles the `image_drivers` pipeline
//! cross-compiles and signs (a three-partition disk whose **read-only
//! `/System` volume** carries the signed `virtio_kbd` bundle at the
//! volume-relative `Drivers/input/virtio_kbd/Run` and the signed framebuffer
//! display-service bundle at `Drivers/display/framebuffer/Run`, design B) as a
//! virtio-blk-mmio device
//! **plus** a `ramfb` display, a `virtio-keyboard-device`, and a
//! `virtio-mouse-device`, and boots `boot_aarch64::boot` verbatim. The
//! production path then:
//!
//! 1. **Discovers** the virtio-block root *and* the virtio-input nodes
//!    (bootstrap-floor virtio-MMIO enumeration). Each input node carries its
//!    register window, a coherent DMA constraint, **and** its discovered GICv2
//!    interrupt line as capability-grant requests; the framebuffer boot
//!    console comes up on the `ramfb` scan-out and the boot publishes the
//!    surface as the boot display node (a `Framebuffer` grant request keyed
//!    `simple-framebuffer`); the full hardware tree is stashed for the init
//!    seam.
//! 2. **Admits the unlock kthread**, which brings the root block device up over
//!    the device-IRQ path, mounts the read-only `/System` volume, and serves
//!    its signed driver store over the capability-gated IPC endpoint.
//! 3. **Reactive user-space autoload (Design D)**: the long-running
//!    `devmgr` service reads the hardware tree, lists the `/System` store over
//!    the IPC service, matches each signed bundle to its discovered node
//!    (`lib/devmatch` — the `virtio_kbd` bundle to each virtio-input node,
//!    the display bundle to the boot display node), and asks the kernel to
//!    load each; the kernel re-runs the full signed gate (verified against
//!    the embedded `KERNEL_DRIVER_SIGNER_PUBKEY`) and **spawns each into its
//!    own user-space process** with exactly its node's resource grants.
//! 4. Each spawned driver instance maps its register window, brings its
//!    virtio-input device up, **then binds its granted interrupt line and
//!    parks on `irq_wait`** (interrupt-driven, never a busy poll; the bind
//!    is `VirtioInput::open_armed`'s arm step, issued only once the eventq
//!    is live so the audited bind is a truthful readiness witness), and on
//!    each device interrupt pumps decoded events into the arbiter — key
//!    edges via `key_inject`, pointer records via `pointer_inject`.
//!
//! ## Why the PASS keys on six witnesses
//!
//! The audit sink reports PASS once it has seen all of:
//!
//! 1. `AuditEvent::InputDelivered` (`EventId` 4050) with `kind=key` — the
//!    one-shot witness the `key_inject` handler emits the first time a
//!    keyboard-class driver delivers to the arbiter; here, the first
//!    typed passphrase character.
//! 2. `AuditEvent::InputDelivered` with `kind=pointer` — its pointer
//!    sibling, from the injected mouse motion (the shared
//!    `PointerInput::from_device_event` mapping).
//! 3. `AuditEvent::UsersDbLoaded` — the users database was read off the
//!    unlocked encrypted root, so the passphrase **typed at the virtio
//!    keyboard** traversed the seat text sink, the video console's
//!    keyboard queue, and the unlock kthread's prompt, and unlocked the
//!    root end to end (the typed-dialogue facility the D7d login stage
//!    builds on).
//! 4. The kernel/ipc `CallEndpointCreated` (`EventId` 3040) whose
//!    `endpoint` field is the reserved `DISPLAY_ENDPOINT` — the
//!    autoloaded framebuffer display service resolved its granted
//!    scan-out surface and bound its rendezvous under
//!    `CAP_IPC_BIND_PRIVILEGED` (only the display service may bind a
//!    reserved endpoint id, so the witness is unforgeable by any other
//!    process in the image).
//! 5. The `appmgr` `APP_LOADED` record for [`TERMINAL_ROUND_TRIP_BUNDLE`]
//!    (`/System/Commands/sleep.app`) — the shell resolved and ran the typed
//!    command, attributed by the loaded bundle's own name rather than a
//!    fragile delivery count: the
//!    desktop session — logged in at the seat keyboard and driven by
//!    injected pointer clicks — autostarted the file manager, served
//!    its window over the reserved window rendezvous, routed the scripted
//!    in-window clicks app-ward (`plans/APPWIN.md` AW3), then opened the
//!    program-library popup from the Library button and spawned the
//!    terminal bundle through its planted catalog entry
//!    (`plans/NEW-TASKBAR.md` T5), focused the terminal's served window,
//!    and delivered the typed command's every key edge — whereupon the
//!    windowed terminal wrote the line to its hosted shell over its
//!    pipe, and the shell resolved and **spawned** the typed program:
//!    the AW4 shell round trip, every hop kernel-attested (the only
//!    spawn that can occur after the typing gate is the shell executing
//!    the typed command).
//! 6. The `appmgr` `APP_LOADED` record for [`CTRL_C_RECOVERY_BUNDLE`]
//!    (`/System/Commands/true.app`), after the `sleep` round trip — the pty
//!    `Ctrl-C` job-control round trip (`plans/PTY.md`), attributed by the
//!    recovered bundle's own name. Once witness 5's `sleep` load latched the
//!    guest emitted [`CTRL_C_ARM_MARKER`], the runner injected a `Ctrl-C`
//!    (which the terminal encodes as the `0x03` interrupt byte through the
//!    shared `lib/keymap` rule) and then [`TERMINAL_CTRL_C_RECOVERY`]'s
//!    `true` + Enter. The shell is parked in `wait` on `sleep`, so it can
//!    load and run `true` only once the pty's cooked-mode line discipline
//!    signalled the foreground `sleep` dead — an end-to-end witness of
//!    keyboard → session → terminal → pty cooked `^C` → foreground
//!    `Signal::Interrupt` → job death → shell recovery, every hop
//!    kernel-attested. A failed interrupt leaves `sleep` blocking past the
//!    run budget, so the witness never latches and the run times out (fail
//!    loud).
//!
//! [`TERMINAL_ROUND_TRIP_BUNDLE`]: tairix_test_autoload_input_qemu_aarch64::TERMINAL_ROUND_TRIP_BUNDLE
//! [`CTRL_C_RECOVERY_BUNDLE`]: tairix_test_autoload_input_qemu_aarch64::CTRL_C_RECOVERY_BUNDLE
//! [`CTRL_C_ARM_MARKER`]: tairix_test_autoload_input_qemu_aarch64::CTRL_C_ARM_MARKER
//! [`TERMINAL_CTRL_C_RECOVERY`]: tairix_test_autoload_input_qemu_aarch64::TERMINAL_CTRL_C_RECOVERY
//!
//! Reaching them requires every preceding step to have succeeded: the
//! `/System` volume mounted and served, the store listed, each signed
//! bundle verified, each node matched (the virtio-input transports and
//! the boot display node the kernel publishes for its ramfb surface), one
//! user-space process spawned per matched node with exactly its node's
//! grants, each device brought up, the typed keys decoded and delivered,
//! the passphrase accepted, and the display service's surface resolved
//! from its `Framebuffer` grant. The harness types only once both input
//! driver instances have armed their interrupts (the audited `irq_bind`
//! syscall, twice), and injects the mouse motion only once the key
//! witness's `kind=key` line appears on serial — so each witness is
//! attributable to its own injection. A run where any step fails never
//! reaches all six witnesses, so the harness times out — the documented
//! fail-loud behaviour.
//!
//! ## Embedded `virt` device tree
//!
//! QEMU's `-kernel <ELF>` aarch64 path passes no DTB pointer (`x0 = 0`), so the
//! canonical `virt` device tree is dumped and embedded at build time
//! (`build.rs`) and its address handed to the boot pipeline. The tree describes
//! the board's `virtio,mmio` transport slots; the planted disk and the attached
//! keyboard populate two slots' live `DeviceID`s, which the bootstrap-floor
//! enumeration reads.
//!
//! ## How it differs from a production kernel
//!
//! It reuses the entire production aarch64 boot pipeline and only replaces the
//! audit sink. Splitting the audit-observer behaviour into a separate bin
//! (instead of a Cargo feature on a production crate) prevents feature
//! unification from leaking the QEMU-exit shortcut into any production build
//! (fail closed; the harness never decides what the kernel
//! does next).

#![cfg_attr(itest_aarch64, no_std)]
#![cfg_attr(itest_aarch64, no_main)]
#![deny(missing_docs)]

// --- Freestanding test bin (`aarch64-unknown-none`) ----------------

#[cfg(itest_aarch64)]
mod kernel;

// --- Host stub -----------------------------------------------------
#[cfg(not(itest_aarch64))]
fn main() {}
