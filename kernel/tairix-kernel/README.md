# tairix-kernel

The freestanding kernel binary for the three bare-metal Tier-1 ports
(`x86_64-tairix-none`, `aarch64-unknown-none`, `riscv64gc-unknown-none-elf`).
It wires each architecture port (`kernel/arch/<target>`) to the
architecture-neutral [`kernel_core::kernel_main`](../core/src/init.rs), and
carries the boot-floor work that runs before the driver store is reachable:
the root volume's unlock and mount, the `/System` view, the driver store
server, and the hardware-tree store.

## Layout

The crate ships a `[lib]` shared by the production `[[bin]]` and the QEMU
verticals that boot it.

| Path                   | Role |
|------------------------|------|
| `src/main.rs`          | The production binary: each port's `#[global_allocator]`, `#[panic_handler]` and entry symbol. |
| `src/<arch>/`          | One port's own wiring (`x86_64`, `aarch64`, `riscv64`): `boot.rs` (the boot pipeline), `dispatch.rs` (the syscall-dispatch callback), `panic_ctx.rs` (the panic bridge), `root_unlock.rs`, `init_spawn.rs` and `spawn_producer.rs`, plus the port's interrupt controller and console. |
| `src/dispatch_core.rs` | What every port's dispatch callback shares: the argument frame, the result encoding, and the forward through the dispatch slot. |
| `src/kalloc.rs`        | Re-exports `lib/kalloc`'s freeing allocator and arena type. |
| `build.rs`             | Hands each port's boot linker script to `rustc`, emits the `freestanding` and `kernel_isa` names the crate gates on, and generates the target's bootstrap-floor table (`driver_catalog`) from the floor drivers' own `Floor.toml` declarations. |

The other modules in `src/` are arch-neutral and serve every port.

## Allocator

Each bin registers the shared `tairix_kalloc::FreeListAllocator`
(`lib/kalloc`, re-exported through `src/kalloc.rs`) as its
`#[global_allocator]` over a plain `static` bootstrap arena of
`BOOT_HEAP_BYTES`, and phase `Mem` installs the frame-backed growth source
(`kernel/core::kheap`). The allocator's design is in `lib/kalloc/README.md`.

## Production dispatch callback

Each port's `src/<arch>/dispatch.rs::production_dispatch` is the callback its
syscall trampoline calls; its signature is pinned at compile time against the
port's `SyscallDispatchFn` (`_DISPATCH_SIGNATURE_PINNED`). It forwards through
`dispatch_core::dispatch_via_slot`, which reads the hook `kernel_main`
publishes into the port's `DISPATCH_SLOT` during its `Syscall` phase:

* **An empty slot** means a syscall fired before that phase ran — a
  boot-ordering failure — and the callback halts the CPU.
* **An unattributable caller** (`DispatchOutcome::NoCallerContext`) never
  halts: the EL0 context on that CPU is one the kernel no longer owns, so it
  is killed the way a wild user fault is.

Results are encoded by `dispatch_core::encode_result`: `Ok(v)` returns `v`,
and `Err(e)` returns `-(e.as_i32() as i64)` as a `u64`, which userland reads
back with `(rax as i64) < 0`.

## `KernelArch::halt` proof

Each port's `KernelArch` implementation forwards `halt` to its arch crate's
free function, and pins the `-> !` signature at compile time
(`_BIN_ARCH_HALT_RETURNS_NEVER` on x86_64, `_AARCH64_BIN_ARCH_HALT_RETURNS_NEVER`
and `_RISCV_BIN_ARCH_HALT_RETURNS_NEVER` on the others).

## Panic handler

Each binary's `#[panic_handler]` calls its port's
`panic_ctx::handle_panic_via_kernel_core`, which runs the kernel's shared
post-mortem with the arch handle `boot()` published. A panic before that
handle exists goes to the port's own serial report, then halts.

## Published boot-state accessors

`src/x86_64/arch_wrapper.rs` exposes read-only accessors over
`tairix_sync::OnceCell` set-once slots, so an in-kernel bring-up observer (the
`tests/integration/irq_qemu_x86_64` vertical, say) can reach live boot state
without re-borrowing the `pub(crate)` `KernelState`:

* `published_irq_table()` / `published_irq_controller()` — the live
  `IrqTable` and the published `IrqController`.

An observer that needs DMA memory draws it from the kernel's own frame
allocator, `installed_spawn_services().frames()`: that allocator owns every
RAM frame it did not reserve, so a second allocator over the firmware memory
map would hand out frames the kernel is also using.

Each slot enforces one-shot publication: a second `set` is a rejected no-op,
and the accessors expose no writable surface.

## Tests

* **Host unit tests** (`cargo test -p tairix-kernel --lib`) cover the
  arch-neutral modules, including the dispatch frame and its result encoding
  and the forward through an empty, a resident and an unattributing slot.
* **QEMU verticals** (`cargo xtask test --qemu`) boot the kernel on each
  port; `tairix-test-kernel-arch-boot` and its `aarch64` and `riscv64`
  siblings pass on observing `AuditEvent::BootCompleted` (`EventId(4004)`).
