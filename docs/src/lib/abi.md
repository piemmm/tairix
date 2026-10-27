# `tairix-abi`

Defines the `abi-v1` interface between the kernel and user space, and the
protocols programs speak to the system services.

## Scope

* `Errno` — stable `#[repr(i32)]` error codes.
* `CapabilityId` — dense `u16` identifier for kernel capabilities, bounded
  by `CAPABILITY_ID_MAX`.
* `SyscallNumber` — `u16` syscall identifier; kernel dispatch tables index
  this directly.
* `IpcMessageHeader` — 32-byte little-endian header carried in front of
  every IPC message.
* `PortName` — a validated, well-known endpoint name (≤ `PORT_NAME_MAX_LEN`
  ASCII bytes) used to resolve a kernel IPC port by a stable name instead
  of its numeric `endpoint`. Encodes to a 32-byte length-prefixed wire form
  and resolves through the kernel port registry; see
  [Kernel IPC subsystem](../architecture/ipc.md).
* `PointerInput` — 20-byte little-endian desktop pointer event (absolute
  move, or a resolved primary/secondary/middle press/release) the window
  manager and taskbar route. Distinct from the device-level
  `driver::input::InputEvent` (see [Input events](../abi/input.md)).
* `KeyInput` — 20-byte little-endian desktop keyboard event (a press or
  release carrying a `KeyValue` — a produced `Char` or a `NamedKeyCode` —
  and the held `Modifiers`) the window manager delivers to the focused
  window (see [Input events](../abi/input.md)).
* `ManifestHeader` — fixed-size prefix of the signed `rxe` manifest section,
  including the SHA-256 syscall-table fingerprint, the Ed25519 public key,
  and the signature.
* `Secret` — a password a request carries (an elevation, a login, a network
  share), borrowed from the buffer it crossed in. No `Debug` shows it, so a
  request holding one can be formatted without the password reaching a
  diagnostic.

## Wire format

All multi-byte fields are little-endian regardless of host endianness. The
total wire size of every struct is exposed via a `WIRE_LEN` constant and
asserted equal to `size_of::<T>()` in the unit tests; any padding regression
fails the build.

## Compatibility

TAIRiX has shipped no release, so `abi-v1` is not frozen: a type, constant or
frame that is wrong is changed in place, with every caller and the generated
views (`include/`, the syscall table) in the same change. From the first
release the numeric values of every public constant are part of the contract
and new behaviour ships as `abi-v2`.

The syscall numbering is `lib/abi/src/syscall.rs`; `lib/abi/src/syscalls.rs`
and the generated `kernel/syscall/src/table.rs` are cross-checked by
`cargo xtask abi-check`.
