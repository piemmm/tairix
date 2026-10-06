//! Shared flattened-device-tree (FDT / DTB) reader.
//!
//! A flattened device tree is the boot-time hardware description that
//! both the aarch64 and riscv64 platforms hand the kernel. The wire
//! format is identical across architectures, so the parser lives here
//! **once** and every architecture port builds its platform discovery on
//! it (no duplication); the arch-specific *queries*
//! (riscv64 `timebase-frequency`, aarch64 PSCI method / timer PPI) layer
//! on top in each port's `fdt`/`platform` module.
//!
//! The parser is `no_std`, allocation-free, and bounds-checks every read
//! against the blob length, returning [`FdtError`] rather than panicking. It is host-testable: [`Fdt::new`] accepts a
//! borrowed blob so the unit tests drive it against a hand-built fixture
//! without a freestanding target.
//!
//! The format is the Devicetree Specification v0.4 flattened layout: a
//! big-endian header, a structure block of `FDT_*` tokens, and a strings
//! block.
//!
//! The blob is firmware/bootloader-supplied, so it is untrusted input: every read is bounds-checked and a malformed tree is
//! rejected, never trusted (fail closed). That decode path carries a
//! fuzz harness (`tests/fuzz_fdt.rs`, registered as `fuzz_fdt` in
//! `cargo xtask fuzz`), which drives mutated, truncated, and arbitrary device
//! trees through [`Fdt::new`] and every public reader and asserts none of them
//! ever panics or reads out of bounds.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

#[cfg(any(test, feature = "test-fixtures"))]
extern crate alloc;

#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixture;

pub mod bus;
pub mod idmap;
pub mod iommu;
pub mod pci;
pub mod specifier;
pub mod supply;

pub use bus::{
    bus_level, dma_ranges, dma_ranges_aperture, dma_ranges_aperture_of, dma_reach,
    outbound_mmio_window, reg_entry_count, scan_translated, translate, translated_reg, BusLevel,
    DmaRange, DmaRanges, DmaReach, DmaWindow, MAX_DMA_WINDOWS, MAX_WALK_DEPTH,
};
pub use idmap::{IdMap, IdMapEntry};
pub use specifier::{phandle_args, PhandleArgs, PhandleArgsIter};
pub use supply::{
    gpio_enabled_regulator, gpio_selected_regulator, supply, GpioEnabledRegulator, GpioLine,
    GpioSelectedRegulator,
};

/// FDT header magic (`0xd00dfeed`, big-endian on the wire).
const FDT_MAGIC: u32 = 0xd00d_feed;

/// `FDT_BEGIN_NODE` — opens a node; followed by its NUL-terminated name,
/// padded to a 4-byte boundary.
const FDT_BEGIN_NODE: u32 = 0x0000_0001;
/// `FDT_END_NODE` — closes the most recently opened node.
const FDT_END_NODE: u32 = 0x0000_0002;
/// `FDT_PROP` — a property: `len: u32`, `nameoff: u32`, then `len` value
/// bytes padded to a 4-byte boundary.
const FDT_PROP: u32 = 0x0000_0003;
/// `FDT_NOP` — padding token, ignored.
const FDT_NOP: u32 = 0x0000_0004;
/// `FDT_END` — terminates the structure block.
const FDT_END: u32 = 0x0000_0009;

/// Devicetree default `#address-cells` when the root omits it.
const DEFAULT_ADDRESS_CELLS: u32 = 2;
/// Devicetree default `#size-cells` when the root omits it.
const DEFAULT_SIZE_CELLS: u32 = 1;

/// Maximum node-nesting depth the walker tracks. QEMU's `virt` tree is
/// shallow; 32 is generous headroom and bounds the parser's stack usage
/// (no unbounded recursion).
const MAX_DEPTH: usize = 32;

/// Reasons the FDT reader rejected a blob.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum FdtError {
    /// The header magic did not match the FDT magic (`0xd00dfeed`).
    BadMagic,
    /// The blob is shorter than the fixed 40-byte header.
    TooShort,
    /// A header offset/size pointed outside the blob.
    OutOfBounds,
    /// The structure block was malformed (truncated token, unterminated
    /// name, unknown token, or unbalanced node nesting).
    Malformed,
    /// A property does not decode as its binding lays it out.
    BadProperty,
}

/// A read-only view over a flattened device tree blob.
pub struct Fdt<'a> {
    blob: &'a [u8],
    struct_off: usize,
    struct_size: usize,
    strings_off: usize,
    strings_size: usize,
}

/// Read a big-endian `u32` at byte offset `off` in `bytes`.
fn be_u32(bytes: &[u8], off: usize) -> Option<u32> {
    let end = off.checked_add(4)?;
    let slice = bytes.get(off..end)?;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

impl<'a> Fdt<'a> {
    /// Validate the header and build a reader over `blob`.
    ///
    /// # Errors
    ///
    /// Returns [`FdtError`] if the magic is wrong, the blob is shorter
    /// than the 40-byte header, or a header offset/size escapes the blob.
    pub fn new(blob: &'a [u8]) -> Result<Self, FdtError> {
        if blob.len() < 40 {
            return Err(FdtError::TooShort);
        }
        if be_u32(blob, 0) != Some(FDT_MAGIC) {
            return Err(FdtError::BadMagic);
        }
        let struct_off = be_u32(blob, 8).ok_or(FdtError::TooShort)? as usize;
        let strings_off = be_u32(blob, 12).ok_or(FdtError::TooShort)? as usize;
        let strings_size = be_u32(blob, 32).ok_or(FdtError::TooShort)? as usize;
        let struct_size = be_u32(blob, 36).ok_or(FdtError::TooShort)? as usize;

        let struct_end = struct_off
            .checked_add(struct_size)
            .ok_or(FdtError::OutOfBounds)?;
        let strings_end = strings_off
            .checked_add(strings_size)
            .ok_or(FdtError::OutOfBounds)?;
        if struct_end > blob.len() || strings_end > blob.len() {
            return Err(FdtError::OutOfBounds);
        }
        // The structure block is a sequence of 4-byte tokens.
        if !struct_off.is_multiple_of(4) || !struct_size.is_multiple_of(4) {
            return Err(FdtError::Malformed);
        }
        Ok(Self {
            blob,
            struct_off,
            struct_size,
            strings_off,
            strings_size,
        })
    }

    /// Build a reader from a raw pointer to a blob in memory.
    ///
    /// Reads the `totalsize` header field to bound the blob, then
    /// delegates to [`Fdt::new`].
    ///
    /// # Safety
    ///
    /// `ptr` must point at the first byte of a flattened device tree whose
    /// `totalsize` header field truthfully describes its length, and the
    /// whole `totalsize` range must be readable for the lifetime `'a`. On
    /// the `virt` boards this is the pointer firmware hands the kernel,
    /// which lives in firmware-reserved RAM for the life of the guest.
    ///
    /// # Errors
    ///
    /// Propagates [`Fdt::new`]'s validation errors.
    pub unsafe fn from_ptr(ptr: *const u8) -> Result<Self, FdtError> {
        // Read the 8-byte prefix (magic + totalsize) to learn the length
        // before forming the full slice.
        // SAFETY: the caller guarantees `ptr` addresses a valid FDT; the
        // first 8 bytes (magic + totalsize) are always present in a
        // well-formed blob.
        let header = unsafe { core::slice::from_raw_parts(ptr, 8) };
        if be_u32(header, 0) != Some(FDT_MAGIC) {
            return Err(FdtError::BadMagic);
        }
        let total = be_u32(header, 4).ok_or(FdtError::TooShort)? as usize;
        if total < 40 {
            return Err(FdtError::TooShort);
        }
        // SAFETY: `total` is the blob's self-described length; the caller
        // guarantees the whole range is readable.
        let blob = unsafe { core::slice::from_raw_parts(ptr, total) };
        Self::new(blob)
    }

    /// Total size in bytes of the blob this reader covers — for a
    /// [`Fdt::from_ptr`] reader, the header's `totalsize`. Boot paths
    /// use it to account the firmware blob's RAM extent (the identity
    /// map must keep the tree readable for the post-MMU walks).
    #[must_use]
    pub fn total_size(&self) -> usize {
        self.blob.len()
    }

    /// Locate the first `/memory` node's first `reg` entry, returning
    /// `(base, size)` in bytes.
    ///
    /// Returns `None` if the tree contains no `/memory` node with a
    /// readable `reg` property. A machine whose RAM spans several ranges
    /// (the Raspberry Pi 4 carries windows below the MMIO hole, between
    /// 1 GiB and 4 GiB, and above 4 GiB) describes them as further `reg`
    /// pairs and further `/memory` nodes; a boot path that must see the
    /// machine's whole RAM iterates [`Self::each_memory_region`] instead.
    #[must_use]
    pub fn first_memory_region(&self) -> Option<(u64, u64)> {
        let mut first = None;
        self.each_memory_region(|base, size| {
            if first.is_none() {
                first = Some((base, size));
            }
        })
        .ok()?;
        first
    }

    /// Enumerate every RAM range the tree declares, invoking
    /// `f(base, size)` once per `(address, size)` pair of every enabled
    /// top-level `/memory` node's `reg` property, in tree order. A memory
    /// node whose `status` disables it describes RAM the kernel may not use
    /// (another agent's, or the secure world's), so it contributes nothing.
    ///
    /// The pairs are decoded with the root `#address-cells` /
    /// `#size-cells` (defaulting to the Devicetree-spec values until the
    /// root overrides them). Pairs are reported exactly as encoded —
    /// including a zero `size` — so the caller applies its own usability
    /// policy; a `reg` whose tail is truncated short of a whole pair, or
    /// whose cell counts cannot be represented in a `u64`, yields only the
    /// whole pairs before the defect (fail closed, never an invented
    /// range).
    ///
    /// # Errors
    ///
    /// Returns [`FdtError::Malformed`] if the structure block is malformed
    /// (a truncated token, an unterminated name, or unbalanced nesting);
    /// `f` is not invoked for a malformed tree prefix beyond the point of
    /// the defect.
    pub fn each_memory_region<F: FnMut(u64, u64)>(&self, mut f: F) -> Result<(), FdtError> {
        let struct_end = self.struct_off + self.struct_size;
        let mut pos = self.struct_off;

        // Root `#address-cells` / `#size-cells` govern the `/memory` `reg`
        // layout; default to the Devicetree-spec values until the root
        // node overrides them.
        let mut addr_cells = DEFAULT_ADDRESS_CELLS;
        let mut size_cells = DEFAULT_SIZE_CELLS;

        let mut memory: Option<OpenMemory<'_>> = None;
        let mut depth: usize = 0;

        while pos < struct_end {
            let token = be_u32(self.blob, pos).ok_or(FdtError::Malformed)?;
            pos += 4;
            match token {
                FDT_NOP => {}
                FDT_END => break,
                FDT_BEGIN_NODE => {
                    let name = self.read_node_name(&mut pos, struct_end)?;
                    if depth >= MAX_DEPTH {
                        return Err(FdtError::Malformed);
                    }
                    // A `/memory` node lives directly under root; its unit
                    // name is `memory` or `memory@<addr>`.
                    if depth == 1 && name_is_memory(name) {
                        memory = Some(OpenMemory::default());
                    }
                    depth += 1;
                }
                FDT_END_NODE => {
                    depth = depth.checked_sub(1).ok_or(FdtError::Malformed)?;
                    if depth == 1 {
                        if let Some(OpenMemory {
                            reg: Some(reg),
                            status,
                        }) = memory.take()
                        {
                            if status_enabled(status) {
                                each_reg_pair(reg, addr_cells, size_cells, &mut f);
                            }
                        }
                    }
                }
                FDT_PROP => {
                    let (prop_name, value) = self.read_prop(&mut pos, struct_end)?;
                    // Root props (depth 1) carry the cell counts.
                    if depth == 1 {
                        if prop_name == b"#address-cells" {
                            if let Some(v) = be_u32(value, 0) {
                                addr_cells = v;
                            }
                        } else if prop_name == b"#size-cells" {
                            if let Some(v) = be_u32(value, 0) {
                                size_cells = v;
                            }
                        }
                    }
                    if let (2, Some(open)) = (depth, memory.as_mut()) {
                        match prop_name {
                            b"reg" => open.reg = Some(value),
                            b"status" => open.status = Some(value),
                            _ => {}
                        }
                    }
                }
                _ => return Err(FdtError::Malformed),
            }
        }
        Ok(())
    }

    /// Read the `/cpus` `timebase-frequency` (the riscv64 `time` CSR tick
    /// rate in Hz).
    ///
    /// Returns the first occurrence found while walking the tree, or
    /// `None` if the property is absent.
    #[must_use]
    pub fn timebase_frequency(&self) -> Option<u64> {
        self.walk().ok().and_then(|w| w.timebase)
    }

    /// Enumerate every `/cpus/cpu@*` node in tree order, invoking
    /// `f(cpu)` once per CPU that may be started with that node's decoded
    /// [`CpuNode`]: a `"disabled"` CPU is quiescent and listed, a failed or
    /// reserved one is not.
    ///
    /// A CPU node with no readable `reg` is skipped (it cannot be
    /// matched to a logical CPU).
    ///
    /// # Errors
    ///
    /// Returns [`FdtError::Malformed`] if the structure block is
    /// malformed (a truncated token, an unterminated name, or unbalanced
    /// node nesting); the closure is not invoked for a malformed tree.
    pub fn each_cpu<F: FnMut(CpuNode)>(&self, mut f: F) -> Result<(), FdtError> {
        let struct_end = self.struct_off + self.struct_size;
        let mut pos = self.struct_off;

        // Per-depth "is this the `/cpus` container" / "is this a `cpu@*`
        // node" flags, restored implicitly by `depth` on `FDT_END_NODE`.
        let mut is_cpus = [false; MAX_DEPTH];
        let mut is_cpu = [false; MAX_DEPTH];
        let mut depth: usize = 0;

        // Accumulators for the cpu node currently open (they never nest).
        let mut reg: Option<u64> = None;
        let mut capacity: Option<u64> = None;
        let mut spin_table = false;
        let mut release_addr: Option<u64> = None;
        let mut status: Option<&[u8]> = None;

        while pos < struct_end {
            let token = be_u32(self.blob, pos).ok_or(FdtError::Malformed)?;
            pos += 4;
            match token {
                FDT_NOP => {}
                FDT_END => break,
                FDT_BEGIN_NODE => {
                    let name = self.read_node_name(&mut pos, struct_end)?;
                    if depth >= MAX_DEPTH {
                        return Err(FdtError::Malformed);
                    }
                    // `/cpus` is a direct child of root (open-time depth 1);
                    // a `cpu@*` node is a direct child of `/cpus`
                    // (open-time depth 2 with the parent flagged).
                    is_cpus[depth] = depth == 1 && name_stem(name) == b"cpus";
                    is_cpu[depth] = depth == 2 && is_cpus[depth - 1] && name_stem(name) == b"cpu";
                    if is_cpu[depth] {
                        reg = None;
                        capacity = None;
                        spin_table = false;
                        release_addr = None;
                        status = None;
                    }
                    depth += 1;
                }
                FDT_END_NODE => {
                    depth = depth.checked_sub(1).ok_or(FdtError::Malformed)?;
                    if is_cpu[depth] && cpu_startable(status) {
                        if let Some(mpidr) = reg {
                            f(CpuNode {
                                reg: mpidr,
                                capacity,
                                // The release address is meaningful only
                                // under the spin-table enable method; a
                                // zero address is the firmware's "not
                                // provided" and is refused rather than
                                // handed out as a writable target (fail
                                // closed).
                                spin_table_release: if spin_table {
                                    release_addr.filter(|&addr| addr != 0)
                                } else {
                                    None
                                },
                            });
                        }
                    }
                }
                FDT_PROP => {
                    let (prop_name, value) = self.read_prop(&mut pos, struct_end)?;
                    if depth >= 1 && is_cpu[depth - 1] {
                        if prop_name == b"reg" {
                            reg = read_int_cells(value);
                        } else if prop_name == b"capacity-dmips-mhz" {
                            capacity = read_int_cells(value);
                        } else if prop_name == b"enable-method" {
                            spin_table = str_prop_is(value, b"spin-table");
                        } else if prop_name == b"cpu-release-addr" {
                            release_addr = read_int_cells(value);
                        } else if prop_name == b"status" {
                            status = Some(value);
                        }
                    }
                }
                _ => return Err(FdtError::Malformed),
            }
        }
        Ok(())
    }

    /// Read the raw bytes of property `name` on the node reached by the
    /// child-name `path` from the root.
    ///
    /// Each `path` component matches a node's unit name with any
    /// `@<unit-address>` suffix stripped (so `b"psci"` matches `psci` and
    /// `b"memory"` matches `memory@80000000`). Returns the property value
    /// of the first matching node, or `None` if no such node/property
    /// exists or the tree is malformed.
    #[must_use]
    pub fn property(&self, path: &[&[u8]], name: &[u8]) -> Option<&'a [u8]> {
        if path.len() > MAX_DEPTH {
            return None;
        }
        self.find_property(path, name).ok().flatten()
    }

    /// Read property `name` on the node at `path` as an integer of one
    /// (`u32`) or two (`u64`) big-endian cells.
    #[must_use]
    pub fn property_u64(&self, path: &[&[u8]], name: &[u8]) -> Option<u64> {
        self.property(path, name).and_then(read_int_cells)
    }

    /// The firmware-provided random seed from `/chosen/rng-seed`, if present.
    ///
    /// Boot firmware and loaders (U-Boot, the Raspberry Pi firmware, and
    /// QEMU's `virt` board) place a block of entropy in the device tree's
    /// `/chosen` node for the kernel to fold into its CSPRNG seed — the
    /// well-established hand-off every general-purpose kernel consumes. The
    /// raw property bytes are returned verbatim (an empty property yields
    /// `Some(&[])`); the caller conditions them and never trusts them alone.
    #[must_use]
    pub fn chosen_rng_seed(&self) -> Option<&'a [u8]> {
        self.property(&[b"chosen"], b"rng-seed")
    }

    /// Single pass over the structure block collecting the timebase
    /// frequency.
    fn walk(&self) -> Result<WalkResult, FdtError> {
        let struct_end = self.struct_off + self.struct_size;
        let mut pos = self.struct_off;

        let mut depth: usize = 0;

        let mut result = WalkResult { timebase: None };

        while pos < struct_end {
            let token = be_u32(self.blob, pos).ok_or(FdtError::Malformed)?;
            pos += 4;
            match token {
                FDT_NOP => {}
                FDT_END => break,
                FDT_BEGIN_NODE => {
                    self.read_node_name(&mut pos, struct_end)?;
                    if depth >= MAX_DEPTH {
                        return Err(FdtError::Malformed);
                    }
                    depth += 1;
                }
                FDT_END_NODE => {
                    depth = depth.checked_sub(1).ok_or(FdtError::Malformed)?;
                }
                FDT_PROP => {
                    let (prop_name, value) = self.read_prop(&mut pos, struct_end)?;
                    if result.timebase.is_none() && prop_name == b"timebase-frequency" {
                        result.timebase = read_int_cells(value);
                    }
                }
                _ => return Err(FdtError::Malformed),
            }
        }
        Ok(result)
    }

    /// Walk the tree looking for `name` on the node matched by `path`.
    fn find_property(&self, path: &[&[u8]], name: &[u8]) -> Result<Option<&'a [u8]>, FdtError> {
        let struct_end = self.struct_off + self.struct_size;
        let mut pos = self.struct_off;

        // Per-level match flag. Flag index 0 is the unnamed root and is
        // always "matched" (transparent); a node at flag index `k >= 1`
        // corresponds to path component `path[k - 1]`. `depth` is the
        // number of currently-open nodes, so the node just opened sits at
        // flag index `depth - 1`.
        let mut matched = [false; MAX_DEPTH];
        let mut depth: usize = 0;

        while pos < struct_end {
            let token = be_u32(self.blob, pos).ok_or(FdtError::Malformed)?;
            pos += 4;
            match token {
                FDT_NOP => {}
                FDT_END => break,
                FDT_BEGIN_NODE => {
                    let node_name = self.read_node_name(&mut pos, struct_end)?;
                    if depth >= MAX_DEPTH {
                        return Err(FdtError::Malformed);
                    }
                    let idx = depth;
                    matched[idx] = if idx == 0 {
                        // The unnamed root is transparent.
                        true
                    } else {
                        let comp = idx - 1;
                        comp < path.len()
                            && matched[..idx].iter().all(|m| *m)
                            && name_stem(node_name) == path[comp]
                    };
                    depth += 1;
                }
                FDT_END_NODE => {
                    depth = depth.checked_sub(1).ok_or(FdtError::Malformed)?;
                }
                FDT_PROP => {
                    let (prop_name, value) = self.read_prop(&mut pos, struct_end)?;
                    // The current node sits at flag index `depth - 1`; it
                    // matches the full path iff that index equals
                    // `path.len()` (the root consumes no component) and
                    // every level matched.
                    if depth == path.len() + 1
                        && matched[..depth].iter().all(|m| *m)
                        && prop_name == name
                    {
                        return Ok(Some(value));
                    }
                }
                _ => return Err(FdtError::Malformed),
            }
        }
        Ok(None)
    }

    /// Read a NUL-terminated node name at `*pos`, advancing `*pos` past the
    /// 4-byte-aligned end of the name.
    fn read_node_name(&self, pos: &mut usize, struct_end: usize) -> Result<&'a [u8], FdtError> {
        read_node_name(self.blob, pos, struct_end)
    }

    /// Read an `FDT_PROP` body at `*pos` (`len`, `nameoff`, then the padded
    /// value), advancing `*pos` past it. Returns the property name and
    /// value slice.
    fn read_prop(
        &self,
        pos: &mut usize,
        struct_end: usize,
    ) -> Result<(&'a [u8], &'a [u8]), FdtError> {
        read_prop(
            self.blob,
            self.strings_off,
            self.strings_size,
            pos,
            struct_end,
        )
    }

    /// The first `compatible` string of the first `/cpus/cpu@*` node, in
    /// document order.
    ///
    /// This is the device tree's identity of the boot-relevant CPU model
    /// (e.g. `sifive,u74-mc`, `arm,cortex-a72`), which the architecture
    /// ports map to a human-readable processor name for the boot facts.
    /// Returns `None` — never a guess — when the tree has no cpu node,
    /// the node carries no `compatible`, the property is empty, or its
    /// first string is not UTF-8; a malformed structure block equally
    /// yields `None` (fail closed).
    #[must_use]
    pub fn boot_cpu_compatible(&self) -> Option<&'a str> {
        let mut in_cpus = false;
        for node in self.nodes() {
            let node = node.ok()?;
            match node.depth() {
                1 => in_cpus = name_stem(node.name()) == b"cpus",
                2 if in_cpus && name_stem(node.name()) == b"cpu" => {
                    let compatible = node.property("compatible")?;
                    let first = compatible.iter_strings().next()?;
                    if first.is_empty() {
                        return None;
                    }
                    return core::str::from_utf8(first).ok();
                }
                _ => {}
            }
        }
        None
    }

    /// Iterate every node of the tree in document order.
    ///
    /// Each item is a [`Node`] handle exposing the node's properties
    /// ([`Node::property`] / [`Node::is_compatible`]). The iterator yields
    /// `Err(FdtError)` and then stops if it meets a malformed token, so a
    /// hostile blob fails closed rather than silently under-enumerating. This
    /// is the generic walk the bus enumerators and the QEMU verticals
    /// discover the `virt` tree through — one parser for every consumer.
    #[must_use]
    pub fn nodes(&self) -> NodeIter<'a> {
        NodeIter {
            blob: self.blob,
            strings_off: self.strings_off,
            strings_size: self.strings_size,
            struct_end: self.struct_off + self.struct_size,
            pos: self.struct_off,
            depth: 0,
        }
    }

    /// Iterate every node in document order, each paired with whether a
    /// consumer may use it: it is operational ([`Node::is_operational`]) and
    /// so is every ancestor, as a disabled bus's devices are no more usable
    /// than the bus.
    #[must_use]
    pub fn nodes_in_use(&self) -> NodesInUse<'a> {
        NodesInUse {
            nodes: self.nodes(),
            unusable_below: None,
        }
    }

    /// Iterate, in document order, every node a consumer may use
    /// ([`Self::nodes_in_use`]).
    #[must_use]
    pub fn operational_nodes(&self) -> OperationalNodes<'a> {
        OperationalNodes {
            nodes: self.nodes_in_use(),
        }
    }

    /// The first node, in document order, whose `compatible` lists `target`.
    ///
    /// `None` when no node does or the walk meets a malformed token first.
    #[must_use]
    pub fn find_compatible(&self, target: impl AsRef<[u8]>) -> Option<Node<'a>> {
        let target = target.as_ref();
        for node in self.nodes() {
            let node = node.ok()?;
            if node.is_compatible(target) {
                return Some(node);
            }
        }
        None
    }

    /// The node whose phandle is `phandle`, found by one walk of the tree.
    ///
    /// `None` when no node carries it, `phandle` names no node (`0`,
    /// `0xFFFF_FFFF`), or the walk meets a malformed token first.
    #[must_use]
    pub fn node_by_phandle(&self, phandle: u32) -> Option<Node<'a>> {
        let wanted = phandle_ref(phandle)?;
        for node in self.nodes() {
            let node = node.ok()?;
            if node.phandle() == Some(wanted) {
                return Some(node);
            }
        }
        None
    }
}

/// Read the NUL-terminated string at `nameoff` in a strings block of
/// `[strings_off, strings_off + strings_size)` within `blob`.
fn string_at(
    blob: &[u8],
    strings_off: usize,
    strings_size: usize,
    nameoff: usize,
) -> Option<&[u8]> {
    let start = strings_off.checked_add(nameoff)?;
    if nameoff >= strings_size {
        return None;
    }
    let block_end = strings_off.checked_add(strings_size)?;
    let region = blob.get(start..block_end)?;
    let len = region.iter().position(|&b| b == 0)?;
    Some(&region[..len])
}

/// Read a NUL-terminated node name at `*pos`, advancing `*pos` past the
/// 4-byte-aligned end of the name.
fn read_node_name<'a>(
    blob: &'a [u8],
    pos: &mut usize,
    struct_end: usize,
) -> Result<&'a [u8], FdtError> {
    let region = blob.get(*pos..struct_end).ok_or(FdtError::Malformed)?;
    let len = region
        .iter()
        .position(|&b| b == 0)
        .ok_or(FdtError::Malformed)?;
    let name = &region[..len];
    // Advance past the name + NUL, rounded up to a 4-byte boundary.
    let consumed = align_up(len + 1, 4);
    *pos = pos.checked_add(consumed).ok_or(FdtError::Malformed)?;
    if *pos > struct_end {
        return Err(FdtError::Malformed);
    }
    Ok(name)
}

/// Read an `FDT_PROP` body at `*pos` (`len`, `nameoff`, then the padded
/// value), advancing `*pos` past it. Returns the property name and value
/// slice.
fn read_prop<'a>(
    blob: &'a [u8],
    strings_off: usize,
    strings_size: usize,
    pos: &mut usize,
    struct_end: usize,
) -> Result<(&'a [u8], &'a [u8]), FdtError> {
    let len = be_u32(blob, *pos).ok_or(FdtError::Malformed)? as usize;
    let nameoff = be_u32(blob, *pos + 4).ok_or(FdtError::Malformed)? as usize;
    let value_start = pos.checked_add(8).ok_or(FdtError::Malformed)?;
    let value_end = value_start.checked_add(len).ok_or(FdtError::Malformed)?;
    if value_end > struct_end {
        return Err(FdtError::Malformed);
    }
    let value = &blob[value_start..value_end];
    let name = string_at(blob, strings_off, strings_size, nameoff).ok_or(FdtError::Malformed)?;
    *pos = align_up(value_end, 4);
    Ok((name, value))
}

/// Iterator over the nodes of an [`Fdt`] in document order, produced by
/// [`Fdt::nodes`].
#[derive(Clone)]
pub struct NodeIter<'a> {
    blob: &'a [u8],
    strings_off: usize,
    strings_size: usize,
    struct_end: usize,
    pos: usize,
    depth: u32,
}

impl<'a> Iterator for NodeIter<'a> {
    type Item = Result<Node<'a>, FdtError>;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.step();
        // Past a malformed token the offsets no longer frame tokens, so
        // nothing after one is read as structure.
        if !matches!(item, Some(Ok(_))) {
            self.pos = self.struct_end;
        }
        item
    }
}

impl core::iter::FusedIterator for NodeIter<'_> {}

impl<'a> NodeIter<'a> {
    fn step(&mut self) -> Option<Result<Node<'a>, FdtError>> {
        loop {
            if self.pos >= self.struct_end {
                return None;
            }
            let Some(token) = be_u32(self.blob, self.pos) else {
                return Some(Err(FdtError::Malformed));
            };
            self.pos += 4;
            match token {
                FDT_NOP => {}
                FDT_END => return None,
                FDT_BEGIN_NODE => {
                    let name = match read_node_name(self.blob, &mut self.pos, self.struct_end) {
                        Ok(n) => n,
                        Err(e) => return Some(Err(e)),
                    };
                    let depth = self.depth;
                    self.depth += 1;
                    // Properties precede child nodes in a valid blob
                    // (Devicetree Spec v0.4), so the returned node's
                    // `PropIter` starting here stops at the first child.
                    return Some(Ok(Node {
                        blob: self.blob,
                        strings_off: self.strings_off,
                        strings_size: self.strings_size,
                        struct_end: self.struct_end,
                        name,
                        depth,
                        props_pos: self.pos,
                    }));
                }
                FDT_END_NODE => {
                    self.depth = match self.depth.checked_sub(1) {
                        Some(d) => d,
                        None => return Some(Err(FdtError::Malformed)),
                    };
                }
                FDT_PROP => {
                    if let Err(e) = read_prop(
                        self.blob,
                        self.strings_off,
                        self.strings_size,
                        &mut self.pos,
                        self.struct_end,
                    ) {
                        return Some(Err(e));
                    }
                }
                _ => return Some(Err(FdtError::Malformed)),
            }
        }
    }
}

/// A single device-tree node visited by [`NodeIter`].
#[derive(Copy, Clone)]
pub struct Node<'a> {
    blob: &'a [u8],
    strings_off: usize,
    strings_size: usize,
    struct_end: usize,
    name: &'a [u8],
    depth: u32,
    props_pos: usize,
}

impl<'a> Node<'a> {
    /// The node's unit-name bytes (e.g. `b"virtio_mmio@a000000"`); empty
    /// for the root node.
    #[must_use]
    pub fn name(&self) -> &'a [u8] {
        self.name
    }

    /// Depth within the tree; the root node is `0`.
    #[must_use]
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// Where the node's properties begin in the structure block: what tells
    /// one node from another.
    #[must_use]
    pub fn offset(&self) -> usize {
        self.props_pos
    }

    /// Iterate this node's immediate properties (not those of children).
    #[must_use]
    pub fn properties(&self) -> PropIter<'a> {
        PropIter {
            blob: self.blob,
            strings_off: self.strings_off,
            strings_size: self.strings_size,
            struct_end: self.struct_end,
            pos: self.props_pos,
        }
    }

    /// Return the property named `name`, if present.
    #[must_use]
    pub fn property(&self, name: &str) -> Option<Property<'a>> {
        self.properties().find_map(|p| match p {
            Ok(p) if p.name == name.as_bytes() => Some(p),
            _ => None,
        })
    }

    /// `true` iff this node's `compatible` property lists `target` as one
    /// of its NUL-separated strings.
    #[must_use]
    pub fn is_compatible(&self, target: impl AsRef<[u8]>) -> bool {
        let target = target.as_ref();
        self.property("compatible")
            .is_some_and(|p| p.iter_strings().any(|s| s == target))
    }

    /// The node's phandle: its `phandle` property, or the older
    /// `linux,phandle` spelling. `0` and `0xFFFF_FFFF` name no node
    /// (Devicetree Spec v0.4 §2.3.3), so neither is returned.
    #[must_use]
    pub fn phandle(&self) -> Option<u32> {
        let property = self
            .property("phandle")
            .or_else(|| self.property("linux,phandle"))?;
        if property.value().len() != 4 {
            return None;
        }
        phandle_ref(property.read_be_u32(0).ok()?)
    }

    /// Whether the node's `status` is absent, `"okay"`, or the legacy `"ok"`
    /// (Devicetree Spec v0.4 §2.3.4). `"disabled"`, `"reserved"`, `"fail"`,
    /// and a malformed value are not.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        status_enabled(self.property("status").map(|status| status.value()))
    }

    /// Whether a consumer may use the node: it is enabled, or it is a CPU
    /// whose `status` is `"disabled"`, which the CPU binding makes quiescent
    /// rather than absent (Devicetree Spec v0.4 §3.8.1).
    #[must_use]
    pub fn is_operational(&self) -> bool {
        let status = self.property("status").map(|status| status.value());
        let cpu = self
            .property("device_type")
            .and_then(|device_type| device_type.iter_strings().next())
            == Some(b"cpu");
        if cpu {
            cpu_startable(status)
        } else {
            status_enabled(status)
        }
    }
}

/// A `/memory` node being read: its ranges are known to be usable only once
/// the whole node is, as properties come in any order.
#[derive(Default)]
struct OpenMemory<'a> {
    reg: Option<&'a [u8]>,
    status: Option<&'a [u8]>,
}

/// Whether a `status` value, or its absence, leaves a node enabled.
fn status_enabled(status: Option<&[u8]>) -> bool {
    status.is_none_or(|value| matches!((StringList { rem: value }).next(), Some(b"okay" | b"ok")))
}

/// Whether a CPU with this `status` may be started: an enabled one, or a
/// `"disabled"` one, which is quiescent until its enable method starts it. A
/// failed CPU is broken or absent and a reserved one is another agent's.
fn cpu_startable(status: Option<&[u8]>) -> bool {
    status_enabled(status)
        || status.and_then(|value| (StringList { rem: value }).next()) == Some(b"disabled")
}

/// Iterator over every node of an [`Fdt`] in document order, each paired with
/// whether a consumer may use it, produced by [`Fdt::nodes_in_use`].
#[derive(Clone)]
pub struct NodesInUse<'a> {
    nodes: NodeIter<'a>,
    /// The depth of the unusable node whose subtree is being passed over.
    unusable_below: Option<u32>,
}

impl<'a> Iterator for NodesInUse<'a> {
    type Item = Result<(Node<'a>, bool), FdtError>;

    fn next(&mut self) -> Option<Self::Item> {
        let node = match self.nodes.next()? {
            Ok(node) => node,
            Err(err) => return Some(Err(err)),
        };
        if self
            .unusable_below
            .is_some_and(|depth| node.depth() > depth)
        {
            return Some(Ok((node, false)));
        }
        let usable = node.is_operational();
        self.unusable_below = (!usable).then_some(node.depth());
        Some(Ok((node, usable)))
    }
}

impl core::iter::FusedIterator for NodesInUse<'_> {}

/// Iterator over the nodes of an [`Fdt`] a consumer may use, in document
/// order, produced by [`Fdt::operational_nodes`].
#[derive(Clone)]
pub struct OperationalNodes<'a> {
    nodes: NodesInUse<'a>,
}

impl<'a> Iterator for OperationalNodes<'a> {
    type Item = Result<Node<'a>, FdtError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.nodes.next()? {
                Ok((node, true)) => return Some(Ok(node)),
                Ok((_, false)) => {}
                Err(err) => return Some(Err(err)),
            }
        }
    }
}

impl core::iter::FusedIterator for OperationalNodes<'_> {}

/// A cell read as a phandle reference: `0` and `0xFFFF_FFFF` name no node
/// (Devicetree Spec v0.4 §2.3.3), so both are refused.
#[must_use]
pub const fn phandle_ref(cell: u32) -> Option<u32> {
    match cell {
        0 | u32::MAX => None,
        phandle => Some(phandle),
    }
}

/// Iterator over the properties immediately under a [`Node`].
#[derive(Clone)]
pub struct PropIter<'a> {
    blob: &'a [u8],
    strings_off: usize,
    strings_size: usize,
    struct_end: usize,
    pos: usize,
}

impl<'a> Iterator for PropIter<'a> {
    type Item = Result<Property<'a>, FdtError>;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.step();
        if !matches!(item, Some(Ok(_))) {
            self.pos = self.struct_end;
        }
        item
    }
}

impl core::iter::FusedIterator for PropIter<'_> {}

impl<'a> PropIter<'a> {
    fn step(&mut self) -> Option<Result<Property<'a>, FdtError>> {
        loop {
            if self.pos >= self.struct_end {
                return None;
            }
            let Some(token) = be_u32(self.blob, self.pos) else {
                return Some(Err(FdtError::Malformed));
            };
            self.pos += 4;
            match token {
                FDT_NOP => {}
                FDT_PROP => {
                    return Some(
                        read_prop(
                            self.blob,
                            self.strings_off,
                            self.strings_size,
                            &mut self.pos,
                            self.struct_end,
                        )
                        .map(|(name, value)| Property { name, value }),
                    );
                }
                // Properties stop at the first non-property boundary: a
                // child node, the end of this node, or the end of block.
                FDT_BEGIN_NODE | FDT_END_NODE | FDT_END => return None,
                _ => return Some(Err(FdtError::Malformed)),
            }
        }
    }
}

/// A single property of a [`Node`].
#[derive(Copy, Clone)]
pub struct Property<'a> {
    name: &'a [u8],
    value: &'a [u8],
}

impl<'a> Property<'a> {
    /// The property name bytes (e.g. `b"reg"`, `b"compatible"`).
    #[must_use]
    pub fn name(&self) -> &'a [u8] {
        self.name
    }

    /// The raw property payload (big-endian on the wire).
    #[must_use]
    pub fn value(&self) -> &'a [u8] {
        self.value
    }

    /// Iterate the NUL-separated strings inside a stringlist property such
    /// as `compatible`.
    #[must_use]
    pub fn iter_strings(&self) -> StringList<'a> {
        StringList { rem: self.value }
    }

    /// Read a single big-endian `u32` at `offset` inside the value.
    ///
    /// # Errors
    ///
    /// Returns [`FdtError::OutOfBounds`] if `offset + 4` exceeds the value
    /// length.
    pub fn read_be_u32(&self, offset: usize) -> Result<u32, FdtError> {
        be_u32(self.value, offset).ok_or(FdtError::OutOfBounds)
    }

    /// Read a single big-endian `u64` (two cells) at `offset` inside the
    /// value.
    ///
    /// # Errors
    ///
    /// Returns [`FdtError::OutOfBounds`] if `offset + 8` exceeds the value
    /// length.
    pub fn read_be_u64(&self, offset: usize) -> Result<u64, FdtError> {
        read_cells(self.value, offset, 2).ok_or(FdtError::OutOfBounds)
    }
}

/// Iterator over the NUL-separated strings inside a stringlist property.
#[derive(Clone)]
pub struct StringList<'a> {
    rem: &'a [u8],
}

impl<'a> Iterator for StringList<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if self.rem.is_empty() {
            return None;
        }
        let nul = self.rem.iter().position(|&b| b == 0)?;
        let (head, tail) = self.rem.split_at(nul);
        self.rem = tail.get(1..).unwrap_or(&[]);
        Some(head)
    }
}

/// Collected results of one [`Fdt::walk`] pass.
struct WalkResult {
    timebase: Option<u64>,
}

/// Round `value` up to the next multiple of `align` (a power of two).
fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

/// `true` if `name` is the unit name of a `/memory` node (`memory` or
/// `memory@<addr>`).
fn name_is_memory(name: &[u8]) -> bool {
    name == b"memory" || name.starts_with(b"memory@")
}

/// The portion of a node's unit name before any `@<unit-address>` suffix
/// (`b"serial@3f201000"` → `b"serial"`).
///
/// This is the "generic names" stem the devicetree specification
/// recommends a node be named with (Devicetree Spec v0.4 §2.2.2), so it
/// is the role hint a consumer can classify a node by when no more
/// specific source exists.
#[must_use]
pub fn name_stem(name: &[u8]) -> &[u8] {
    match name.iter().position(|&b| b == b'@') {
        Some(at) => &name[..at],
        None => name,
    }
}

/// Decode `cells` big-endian `u32` cells starting at `off` into a `u64`.
///
/// Returns `None` if the slice is too short, or if `cells` is `0` (there
/// is no value to read — never an invented `0`) or greater than `2` (the
/// value cannot be represented in a `u64` — fail closed, never wrap).
#[must_use]
pub fn read_cells(value: &[u8], off: usize, cells: u32) -> Option<u64> {
    if cells == 0 || cells > 2 {
        return None;
    }
    let mut acc: u64 = 0;
    let mut o = off;
    for _ in 0..cells {
        let cell = be_u32(value, o)?;
        acc = (acc << 32) | u64::from(cell);
        o += 4;
    }
    Some(acc)
}

/// Invoke `f(address, size)` for every whole `(address, size)` pair in a
/// `reg` property value. Out-of-range cell counts are rejected by
/// [`read_cells`] (no pair is reported); a truncated trailing pair is
/// ignored (fail closed, never an invented range).
fn each_reg_pair<F: FnMut(u64, u64)>(value: &[u8], addr_cells: u32, size_cells: u32, f: &mut F) {
    let stride = (addr_cells as usize + size_cells as usize) * 4;
    if stride == 0 {
        return;
    }
    let mut off = 0;
    while off + stride <= value.len() {
        let Some(base) = read_cells(value, off, addr_cells) else {
            return;
        };
        let Some(size) = read_cells(value, off + (addr_cells as usize) * 4, size_cells) else {
            return;
        };
        f(base, size);
        off += stride;
    }
}

/// Read an integer property whose value is one `u32` cell or two (`u64`).
pub(crate) fn read_int_cells(value: &[u8]) -> Option<u64> {
    match value.len() {
        4 => be_u32(value, 0).map(u64::from),
        8 => read_cells(value, 0, 2),
        _ => None,
    }
}

/// `true` iff the string property `value` equals `expected` — with or
/// without the NUL terminator the Devicetree spec appends. Any other
/// shape (a different string, a string list, an empty value) is `false`
/// (fail closed, never a prefix match).
fn str_prop_is(value: &[u8], expected: &[u8]) -> bool {
    match value.split_last() {
        Some((0, head)) => head == expected,
        _ => value == expected,
    }
}

/// One `/cpus/cpu@*` node decoded by [`Fdt::each_cpu`].
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct CpuNode {
    /// The node's `reg` value — the CPU's `MPIDR_EL1` affinity on
    /// aarch64 / hart id on riscv64 — decoded from a one-cell (`u32`)
    /// or two-cell (`u64`) value.
    pub reg: u64,
    /// The `capacity-dmips-mhz` value (the per-core DMIPS rating used
    /// to classify `big.LITTLE` cores), or `None` when the node omits
    /// it — a homogeneous machine.
    pub capacity: Option<u64>,
    /// The spin-table release address (Devicetree `cpu-release-addr`),
    /// present only when the node's `enable-method` is `spin-table`
    /// and the address decodes non-zero. `None` for a PSCI-enabled or
    /// boot CPU node. This is the physical word firmware parks the CPU
    /// polling; writing an entry address there (and signalling an
    /// event) releases the CPU into that entry.
    pub spin_table_release: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{arm_with_cpus, virt_like, virt_like_arm, DtbBuilder};
    use alloc::vec::Vec;

    #[test]
    fn a_phandle_is_read_from_either_spelling_and_never_a_reserved_value() {
        let mut b = DtbBuilder::new();
        b.begin_node("");
        for (name, property, value) in [
            ("modern", "phandle", 7u32),
            ("legacy", "linux,phandle", 9),
            ("zero", "phandle", 0),
            ("all-ones", "phandle", u32::MAX),
        ] {
            b.begin_node(name);
            b.prop_u32(property, value);
            b.end_node();
        }
        b.begin_node("wide");
        b.prop("phandle", &[0, 0, 0, 1, 0, 0, 0, 2]);
        b.end_node();
        b.begin_node("none");
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let phandles: Vec<(Vec<u8>, Option<u32>)> = fdt
            .nodes()
            .skip(1)
            .map(|n| n.expect("well formed"))
            .map(|n| (n.name().to_vec(), n.phandle()))
            .collect();
        assert_eq!(
            phandles,
            [
                (b"modern".to_vec(), Some(7)),
                (b"legacy".to_vec(), Some(9)),
                (b"zero".to_vec(), None),
                (b"all-ones".to_vec(), None),
                (b"wide".to_vec(), None),
                (b"none".to_vec(), None),
            ]
        );
    }

    #[test]
    fn rejects_bad_magic() {
        let mut blob = virt_like(0x8000_0000, 0x1000_0000, 10_000_000);
        blob[0] = 0;
        assert_eq!(Fdt::new(&blob).err(), Some(FdtError::BadMagic));
    }

    #[test]
    fn rejects_short_blob() {
        let blob = [0u8; 8];
        assert_eq!(Fdt::new(&blob).err(), Some(FdtError::TooShort));
    }

    #[test]
    fn reads_memory_region() {
        let blob = virt_like(0x8000_0000, 0x1000_0000, 10_000_000);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(fdt.first_memory_region(), Some((0x8000_0000, 0x1000_0000)));
    }

    #[test]
    fn total_size_reports_the_whole_blob() {
        let blob = virt_like(0x8000_0000, 0x1000_0000, 10_000_000);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(fdt.total_size(), blob.len());
    }

    #[test]
    fn reads_timebase_frequency() {
        let blob = virt_like(0x8000_0000, 0x1000_0000, 10_000_000);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(fdt.timebase_frequency(), Some(10_000_000));
    }

    #[test]
    fn memory_uses_root_cell_counts() {
        // A tree declaring 1/1 root cells must read 32-bit base+size.
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.begin_node("memory@80000000");
        let mut reg = Vec::new();
        reg.extend_from_slice(&0x8000_0000u32.to_be_bytes());
        reg.extend_from_slice(&0x0800_0000u32.to_be_bytes());
        b.prop("reg", &reg);
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(fdt.first_memory_region(), Some((0x8000_0000, 0x0800_0000)));
    }

    #[test]
    fn missing_memory_node_returns_none() {
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("cpus");
        b.prop_u32("timebase-frequency", 10_000_000);
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(fdt.first_memory_region(), None);
        assert_eq!(fdt.timebase_frequency(), Some(10_000_000));
    }

    #[test]
    fn first_memory_node_wins_over_later_ones() {
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("memory@80000000");
        let mut reg = Vec::new();
        reg.extend_from_slice(&0x8000_0000u64.to_be_bytes());
        reg.extend_from_slice(&0x1000_0000u64.to_be_bytes());
        b.prop("reg", &reg);
        b.end_node();
        b.begin_node("memory@90000000");
        let mut reg2 = Vec::new();
        reg2.extend_from_slice(&0x9000_0000u64.to_be_bytes());
        reg2.extend_from_slice(&0x2000_0000u64.to_be_bytes());
        b.prop("reg", &reg2);
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(fdt.first_memory_region(), Some((0x8000_0000, 0x1000_0000)));
    }

    /// Collect every region [`Fdt::each_memory_region`] reports.
    fn all_regions(fdt: &Fdt<'_>) -> Vec<(u64, u64)> {
        let mut seen = Vec::new();
        fdt.each_memory_region(|base, size| seen.push((base, size)))
            .expect("well-formed tree");
        seen
    }

    #[test]
    fn each_memory_region_reports_every_reg_pair_and_every_memory_node() {
        // Pi 4 (8 GiB) shape: one /memory node whose reg carries the
        // below-hole window plus the 1 GiB..4 GiB window, and a second
        // /memory node for the range above 4 GiB.
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("memory@0");
        let mut reg = Vec::new();
        reg.extend_from_slice(&0x0000_0000u64.to_be_bytes());
        reg.extend_from_slice(&0x3B40_0000u64.to_be_bytes());
        reg.extend_from_slice(&0x4000_0000u64.to_be_bytes());
        reg.extend_from_slice(&0xBC00_0000u64.to_be_bytes());
        b.prop("reg", &reg);
        b.end_node();
        b.begin_node("memory@100000000");
        let mut high = Vec::new();
        high.extend_from_slice(&0x1_0000_0000u64.to_be_bytes());
        high.extend_from_slice(&0x1_0000_0000u64.to_be_bytes());
        b.prop("reg", &high);
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(
            all_regions(&fdt),
            alloc::vec![
                (0x0000_0000, 0x3B40_0000),
                (0x4000_0000, 0xBC00_0000),
                (0x1_0000_0000, 0x1_0000_0000),
            ]
        );
        // The single-window reader still reports the first pair.
        assert_eq!(fdt.first_memory_region(), Some((0x0000_0000, 0x3B40_0000)));
    }

    #[test]
    fn a_disabled_memory_node_contributes_no_ram_wherever_its_status_sits() {
        // QEMU `virt,secure=on` shape: memory only the secure world may use.
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.begin_node("memory@40000000");
        b.prop("reg", &[0x40, 0, 0, 0, 0x10, 0, 0, 0]);
        b.end_node();
        b.begin_node("memory@e000000");
        b.prop("reg", &[0x0E, 0, 0, 0, 0x01, 0, 0, 0]);
        b.prop_str("status", "disabled");
        b.end_node();
        b.begin_node("memory@f000000");
        b.prop_str("status", "reserved");
        b.prop("reg", &[0x0F, 0, 0, 0, 0x01, 0, 0, 0]);
        b.end_node();
        b.begin_node("memory@80000000");
        b.prop_str("status", "okay");
        b.prop("reg", &[0x80, 0, 0, 0, 0x10, 0, 0, 0]);
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(
            all_regions(&fdt),
            alloc::vec![(0x4000_0000, 0x1000_0000), (0x8000_0000, 0x1000_0000)]
        );
    }

    #[test]
    fn operational_nodes_skip_each_unusable_node_with_its_subtree() {
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.begin_node("cpus");
        b.begin_node("cpu@0");
        b.prop_str("device_type", "cpu");
        b.end_node();
        b.begin_node("cpu@1");
        b.prop_str("device_type", "cpu");
        b.prop_str("status", "disabled");
        b.end_node();
        b.begin_node("cpu@2");
        b.prop_str("device_type", "cpu");
        b.prop_str("status", "fail");
        b.end_node();
        b.end_node();
        b.begin_node("bus@0");
        b.prop_str("status", "disabled");
        b.begin_node("uart@0");
        b.prop_str("status", "okay");
        b.begin_node("port");
        b.end_node();
        b.end_node();
        b.end_node();
        b.begin_node("firmware@0");
        b.prop_str("status", "reserved");
        b.end_node();
        b.begin_node("uart@1");
        b.prop_str("status", "ok");
        b.end_node();
        b.begin_node("serial@2");
        b.prop_str("status", "disabled");
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let names: Vec<&[u8]> = fdt
            .operational_nodes()
            .map(|node| node.expect("well-formed").name())
            .collect();
        assert_eq!(
            names,
            [&b""[..], b"cpus", b"cpu@0", b"cpu@1", b"uart@1"],
            "a quiescent cpu stays, a failed one, a disabled bus's whole subtree, a reserved and a disabled device go"
        );
        let unusable: Vec<&[u8]> = fdt
            .nodes_in_use()
            .map(|node| node.expect("well-formed"))
            .filter(|(_, usable)| !usable)
            .map(|(node, _)| node.name())
            .collect();
        assert_eq!(
            unusable,
            [
                &b"cpu@2"[..],
                b"bus@0",
                b"uart@0",
                b"port",
                b"firmware@0",
                b"serial@2"
            ],
            "every node is reported, the unusable ones as such"
        );
    }

    #[test]
    fn each_cpu_lists_a_quiescent_cpu_and_no_failed_or_reserved_one() {
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.begin_node("cpus");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 0);
        for (reg, status) in [
            (0, None),
            (1, Some("disabled")),
            (2, Some("fail")),
            (3, Some("reserved")),
            (4, Some("okay")),
        ] {
            b.begin_node(&alloc::format!("cpu@{reg}"));
            b.prop_str("device_type", "cpu");
            b.prop_u32("reg", reg);
            if let Some(status) = status {
                b.prop_str("status", status);
            }
            b.end_node();
        }
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let mut started = Vec::new();
        fdt.each_cpu(|cpu| started.push(cpu.reg))
            .expect("well-formed");
        assert_eq!(started, [0, 1, 4]);
    }

    #[test]
    fn operational_nodes_end_at_a_malformed_token() {
        let mut blob = virt_like(0x4000_0000, 0x1000_0000, 10_000_000);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let whole = fdt.operational_nodes().count();
        let struct_off = u32::from_be_bytes(blob[8..12].try_into().expect("header")) as usize;
        blob[struct_off..struct_off + 4].copy_from_slice(&0x77u32.to_be_bytes());
        let fdt = Fdt::new(&blob).expect("header still valid");
        let walked: Vec<_> = fdt.operational_nodes().collect();
        assert!(whole > 1);
        assert_eq!(walked.len(), 1);
        assert!(walked[0].is_err());
    }

    #[test]
    fn each_memory_region_honours_one_cell_layouts() {
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 1);
        b.prop_u32("#size-cells", 1);
        b.begin_node("memory@80000000");
        let mut reg = Vec::new();
        reg.extend_from_slice(&0x8000_0000u32.to_be_bytes());
        reg.extend_from_slice(&0x0800_0000u32.to_be_bytes());
        reg.extend_from_slice(&0x9000_0000u32.to_be_bytes());
        reg.extend_from_slice(&0x0400_0000u32.to_be_bytes());
        b.prop("reg", &reg);
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(
            all_regions(&fdt),
            alloc::vec![(0x8000_0000, 0x0800_0000), (0x9000_0000, 0x0400_0000)]
        );
    }

    #[test]
    fn each_memory_region_ignores_a_truncated_trailing_pair() {
        // A reg holding one whole pair plus a dangling half pair yields
        // only the whole pair — never an invented range.
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        b.begin_node("memory@0");
        let mut reg = Vec::new();
        reg.extend_from_slice(&0x0u64.to_be_bytes());
        reg.extend_from_slice(&0x4000_0000u64.to_be_bytes());
        reg.extend_from_slice(&0x8000_0000u64.to_be_bytes());
        b.prop("reg", &reg);
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(all_regions(&fdt), alloc::vec![(0x0, 0x4000_0000)]);
    }

    #[test]
    fn from_ptr_matches_new() {
        let blob = virt_like(0x8000_0000, 0x1000_0000, 10_000_000);
        // SAFETY: `blob` is a valid FDT whose totalsize header equals its
        // length; the slice outlives the `Fdt` built from it.
        let fdt = unsafe { Fdt::from_ptr(blob.as_ptr()) }.expect("valid fdt");
        assert_eq!(fdt.first_memory_region(), Some((0x8000_0000, 0x1000_0000)));
    }

    #[test]
    fn property_reads_psci_method_and_timer_ppi() {
        let blob = virt_like_arm(0x4000_0000, 0x2000_0000, "hvc", 14);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(fdt.property(&[b"psci"], b"method"), Some(&b"hvc\0"[..]));
        // The /timer interrupts triple is <type, number, flags>; the
        // second cell is the PPI number.
        let interrupts = fdt.property(&[b"timer"], b"interrupts").expect("present");
        assert_eq!(be_u32(interrupts, 4), Some(14));
        assert_eq!(fdt.first_memory_region(), Some((0x4000_0000, 0x2000_0000)));
    }

    #[test]
    fn property_misses_unknown_node_and_prop() {
        let blob = virt_like_arm(0x4000_0000, 0x2000_0000, "smc", 14);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(fdt.property(&[b"nope"], b"method"), None);
        assert_eq!(fdt.property(&[b"psci"], b"absent"), None);
        // A deeper path than the tree has no match.
        assert_eq!(fdt.property(&[b"psci", b"child"], b"method"), None);
    }

    #[test]
    fn each_cpu_reads_mpidr_and_capacity_in_tree_order() {
        // A big.LITTLE part: two performance cores (cap 1024) and two
        // efficiency cores (cap 512); the last core omits the capacity.
        let blob = arm_with_cpus(
            0x4000_0000,
            0x2000_0000,
            &[
                (0x0, Some(1024)),
                (0x1, Some(512)),
                (0x100, Some(1024)),
                (0x101, None),
            ],
        );
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let mut seen: Vec<(u64, Option<u64>)> = Vec::new();
        fdt.each_cpu(|cpu| seen.push((cpu.reg, cpu.capacity)))
            .expect("walk succeeds");
        assert_eq!(
            seen,
            [
                (0x0, Some(1024)),
                (0x1, Some(512)),
                (0x100, Some(1024)),
                (0x101, None),
            ]
        );
    }

    #[test]
    fn each_cpu_reads_spin_table_release_addresses() {
        // The Pi 4 stock-firmware shape: the boot CPU has no release
        // address; each secondary declares `enable-method = "spin-table"`
        // and a two-cell `cpu-release-addr`.
        let blob = crate::fixture::arm_with_spin_table_cpus(
            0x0,
            0x4000_0000,
            &[
                (0x0, None),
                (0x1, Some(0xe0)),
                (0x2, Some(0xe8)),
                (0x3, Some(0xf0)),
            ],
        );
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let mut seen: Vec<(u64, Option<u64>)> = Vec::new();
        fdt.each_cpu(|cpu| seen.push((cpu.reg, cpu.spin_table_release)))
            .expect("walk succeeds");
        assert_eq!(
            seen,
            [
                (0x0, None),
                (0x1, Some(0xe0)),
                (0x2, Some(0xe8)),
                (0x3, Some(0xf0)),
            ]
        );
    }

    #[test]
    fn spin_table_release_requires_the_spin_table_enable_method() {
        // A `cpu-release-addr` without `enable-method = "spin-table"`
        // (or under a different method) is not a release target.
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.begin_node("cpus");
        b.begin_node("cpu@1");
        b.prop_u32("reg", 1);
        b.prop("enable-method", b"psci\0");
        b.prop("cpu-release-addr", &0xe0u64.to_be_bytes());
        b.end_node();
        b.begin_node("cpu@2");
        b.prop_u32("reg", 2);
        b.prop("cpu-release-addr", &0xe8u64.to_be_bytes());
        b.end_node();
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let mut seen: Vec<Option<u64>> = Vec::new();
        fdt.each_cpu(|cpu| seen.push(cpu.spin_table_release))
            .expect("walk succeeds");
        assert_eq!(seen, [None, None]);
    }

    #[test]
    fn spin_table_release_rejects_a_zero_or_malformed_address() {
        // A zero release address is firmware's "not provided"; a
        // wrong-sized property does not decode. Both fail closed.
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.begin_node("cpus");
        b.begin_node("cpu@1");
        b.prop_u32("reg", 1);
        b.prop("enable-method", b"spin-table\0");
        b.prop("cpu-release-addr", &0u64.to_be_bytes());
        b.end_node();
        b.begin_node("cpu@2");
        b.prop_u32("reg", 2);
        b.prop("enable-method", b"spin-table\0");
        b.prop("cpu-release-addr", &[0xe0u8, 0, 0]);
        b.end_node();
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let mut seen: Vec<Option<u64>> = Vec::new();
        fdt.each_cpu(|cpu| seen.push(cpu.spin_table_release))
            .expect("walk succeeds");
        assert_eq!(seen, [None, None]);
    }

    #[test]
    fn str_prop_is_matches_with_and_without_the_nul() {
        assert!(str_prop_is(b"spin-table\0", b"spin-table"));
        assert!(str_prop_is(b"spin-table", b"spin-table"));
        assert!(!str_prop_is(b"psci\0", b"spin-table"));
        assert!(!str_prop_is(b"spin-table-x\0", b"spin-table"));
        assert!(!str_prop_is(b"", b"spin-table"));
        // A string list is not a single matching string.
        assert!(!str_prop_is(b"spin-table\0psci\0", b"spin-table"));
    }

    #[test]
    fn each_cpu_yields_nothing_when_there_are_no_cpu_nodes() {
        let blob = virt_like_arm(0x4000_0000, 0x2000_0000, "hvc", 14);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let mut count = 0usize;
        fdt.each_cpu(|_| count += 1).expect("walk succeeds");
        assert_eq!(count, 0);
    }

    /// A minimal tree with one `/cpus/cpu@0` node carrying `compatible`,
    /// plus a `cpu-map` sibling that must not be mistaken for a cpu node.
    fn tree_with_cpu_compatible(compatible: Option<&[u8]>) -> Vec<u8> {
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.begin_node("cpus");
        b.begin_node("cpu-map");
        b.end_node();
        b.begin_node("cpu@0");
        if let Some(value) = compatible {
            b.prop("compatible", value);
        }
        b.prop_u32("reg", 0);
        b.end_node();
        b.end_node();
        b.end_node();
        b.build()
    }

    #[test]
    fn boot_cpu_compatible_reads_the_first_string() {
        let blob = tree_with_cpu_compatible(Some(b"sifive,u74-mc\0riscv\0"));
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(fdt.boot_cpu_compatible(), Some("sifive,u74-mc"));
    }

    #[test]
    fn boot_cpu_compatible_is_none_without_a_source() {
        // No cpu node at all.
        let blob = virt_like_arm(0x4000_0000, 0x2000_0000, "hvc", 14);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(fdt.boot_cpu_compatible(), None);
        // A cpu node with no `compatible` property.
        let blob = tree_with_cpu_compatible(None);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(fdt.boot_cpu_compatible(), None);
        // A cpu node with an empty `compatible` value.
        let blob = tree_with_cpu_compatible(Some(b"\0"));
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(fdt.boot_cpu_compatible(), None);
    }

    #[test]
    fn property_u64_decodes_single_and_double_cells() {
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.begin_node("chosen");
        b.prop_u32("one-cell", 0x1234);
        let mut two = Vec::new();
        two.extend_from_slice(&0x0123_4567_89ab_cdefu64.to_be_bytes());
        b.prop("two-cell", &two);
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(fdt.property_u64(&[b"chosen"], b"one-cell"), Some(0x1234));
        assert_eq!(
            fdt.property_u64(&[b"chosen"], b"two-cell"),
            Some(0x0123_4567_89ab_cdef)
        );
    }

    #[test]
    fn chosen_rng_seed_reads_the_seed_bytes_when_present() {
        let seed: [u8; 32] = core::array::from_fn(|i| {
            let byte = u8::try_from(i).unwrap_or(0);
            byte.wrapping_mul(7).wrapping_add(3)
        });
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.begin_node("chosen");
        b.prop("rng-seed", &seed);
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(fdt.chosen_rng_seed(), Some(&seed[..]));
    }

    #[test]
    fn chosen_rng_seed_is_none_without_the_property() {
        // A `/chosen` with an unrelated property, and a tree with no
        // `/chosen` at all, both yield `None` rather than a guessed value.
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.begin_node("chosen");
        b.prop("stdout-path", b"/serial\0");
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(fdt.chosen_rng_seed(), None);

        let blob = virt_like_arm(0x4000_0000, 0x2000_0000, "hvc", 14);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(fdt.chosen_rng_seed(), None);
    }

    #[test]
    fn nodes_enumerate_and_read_virtio_mmio_slots() {
        // A `virt`-shaped tree: two virtio-MMIO transports and an
        // unrelated `/memory` node, mirroring the QEMU `virt` layout the
        // bus enumerator walks.
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.prop_u32("#address-cells", 2);
        b.prop_u32("#size-cells", 2);
        for (i, base) in [0x0a00_0000u64, 0x0a00_0200].iter().enumerate() {
            let name = alloc::format!("virtio_mmio@{base:x}");
            b.begin_node(&name);
            b.prop("compatible", b"virtio,mmio\0");
            let mut reg = Vec::new();
            reg.extend_from_slice(&base.to_be_bytes());
            reg.extend_from_slice(&0x200u64.to_be_bytes());
            b.prop("reg", &reg);
            let mut irq = Vec::new();
            let irq_number = 0x10 + u32::try_from(i).expect("slot index fits u32");
            for cell in [0u32, irq_number, 0x04] {
                irq.extend_from_slice(&cell.to_be_bytes());
            }
            b.prop("interrupts", &irq);
            b.end_node();
        }
        b.begin_node("memory@40000000");
        b.prop("device_type", b"memory\0");
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");

        let mut slots: Vec<(u64, u64, u32)> = Vec::new();
        for node in fdt.nodes() {
            let node = node.expect("node parses");
            if !node.is_compatible("virtio,mmio") {
                continue;
            }
            let reg = node.property("reg").expect("reg present");
            let base = reg.read_be_u64(0).expect("base");
            let len = reg.read_be_u64(8).expect("len");
            let irq = node
                .property("interrupts")
                .expect("interrupts present")
                .read_be_u32(4)
                .expect("irq cell");
            slots.push((base, len, irq));
        }
        assert_eq!(
            slots,
            [(0x0a00_0000, 0x200, 0x10), (0x0a00_0200, 0x200, 0x11)]
        );
    }

    #[test]
    fn node_is_compatible_false_for_absent_or_mismatched() {
        let blob = virt_like_arm(0x4000_0000, 0x2000_0000, "hvc", 14);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let mut saw_psci = false;
        let mut saw_virtio = false;
        for node in fdt.nodes() {
            let node = node.expect("node parses");
            saw_psci |= node.is_compatible("arm,psci-1.0");
            saw_virtio |= node.is_compatible("virtio,mmio");
        }
        assert!(saw_psci);
        assert!(!saw_virtio);
        // The root node has no `compatible` and no arbitrary property.
        let root = fdt.nodes().next().expect("root present").expect("ok");
        assert!(!root.is_compatible("anything"));
        assert!(root.property("missing").is_none());
    }

    #[test]
    fn property_reads_fail_closed_past_the_value_end() {
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.begin_node("dev");
        b.prop("reg", &0x1234u32.to_be_bytes());
        b.end_node();
        b.end_node();
        let blob = b.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let dev = fdt
            .nodes()
            .find_map(|n| {
                let n = n.ok()?;
                (n.name() == b"dev").then_some(n)
            })
            .expect("dev node");
        let reg = dev.property("reg").expect("reg present");
        assert_eq!(reg.read_be_u32(0), Ok(0x1234));
        assert_eq!(reg.read_be_u64(0).err(), Some(FdtError::OutOfBounds));
        assert_eq!(reg.read_be_u32(4).err(), Some(FdtError::OutOfBounds));
    }

    #[test]
    fn nodes_fail_closed_on_malformed_token() {
        let blob = virt_like_arm(0x4000_0000, 0x2000_0000, "hvc", 14);
        let mut corrupt = blob.clone();
        // Overwrite the first structure-block token with an unknown value
        // (the structure block begins at the 40-byte header end).
        corrupt[40..44].copy_from_slice(&0x00ff_ff00u32.to_be_bytes());
        let fdt = Fdt::new(&corrupt).expect("header still valid");
        assert!(matches!(fdt.nodes().next(), Some(Err(FdtError::Malformed))));
    }

    /// Two sibling nodes, `a` then `b`, each carrying `reg = <0x1234>` and
    /// the compatible `vendor,<name>`.
    fn two_siblings() -> Vec<u8> {
        let mut b = DtbBuilder::new();
        b.begin_node("");
        for (name, compatible) in [("a", "vendor,a"), ("b", "vendor,b")] {
            b.begin_node(name);
            b.prop("reg", &0x1234u32.to_be_bytes());
            b.prop_str("compatible", compatible);
            b.end_node();
        }
        b.end_node();
        b.build()
    }

    /// The offset of the first `FDT_PROP` token whose value is `0x1234`.
    fn first_reg_prop(blob: &[u8]) -> usize {
        let mut header = [0u8; 8];
        header[..4].copy_from_slice(&FDT_PROP.to_be_bytes());
        header[4..].copy_from_slice(&4u32.to_be_bytes());
        (0..blob.len() - 16)
            .find(|&i| blob[i..i + 8] == header && blob[i + 12..i + 16] == 0x1234u32.to_be_bytes())
            .expect("a reg property")
    }

    #[test]
    fn the_node_walk_reads_nothing_past_a_malformed_token() {
        let mut blob = two_siblings();
        // Corrupt `a`'s first property so the words after it no longer frame
        // tokens, while `b` still sits intact further on.
        let nameoff = first_reg_prop(&blob) + 8;
        blob[nameoff..nameoff + 4].copy_from_slice(&0xFFFF_FFF0u32.to_be_bytes());
        let fdt = Fdt::new(&blob).expect("header still valid");
        let items: Vec<_> = fdt.nodes().map(|n| n.map(|n| n.name().to_vec())).collect();
        assert_eq!(
            items,
            [
                Ok(b"".to_vec()),
                Ok(b"a".to_vec()),
                Err(FdtError::Malformed)
            ]
        );
        assert!(fdt.find_compatible("vendor,b").is_none());
    }

    #[test]
    fn a_nodes_properties_end_at_a_malformed_one() {
        let mut blob = two_siblings();
        let nameoff = first_reg_prop(&blob) + 8;
        blob[nameoff..nameoff + 4].copy_from_slice(&0xFFFF_FFF0u32.to_be_bytes());
        let fdt = Fdt::new(&blob).expect("header still valid");
        let a = fdt.nodes().nth(1).expect("a").expect("a's header parses");
        let mut properties = a.properties();
        assert!(matches!(properties.next(), Some(Err(FdtError::Malformed))));
        assert!(properties.next().is_none());
        assert!(a.property("compatible").is_none());
    }

    #[test]
    fn a_nodes_properties_end_at_its_first_child() {
        let blob = two_siblings();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let root = fdt.nodes().next().expect("root").expect("ok");
        let mut properties = root.properties();
        assert!(properties.next().is_none());
        assert!(
            properties.next().is_none(),
            "the child's properties are not the root's"
        );
    }

    #[test]
    fn find_compatible_returns_the_first_node_listing_the_string() {
        let blob = two_siblings();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert_eq!(
            fdt.find_compatible(b"vendor,b").map(|n| n.name()),
            Some(&b"b"[..])
        );
        assert_eq!(
            fdt.find_compatible("vendor,a").map(|n| n.name()),
            Some(&b"a"[..])
        );
        assert!(fdt.find_compatible("vendor").is_none());
    }

    #[test]
    fn read_cells_decodes_and_fails_closed() {
        let v = [0x12u8, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0];
        assert_eq!(read_cells(&v, 0, 1), Some(0x1234_5678));
        assert_eq!(read_cells(&v, 4, 1), Some(0x9abc_def0));
        assert_eq!(read_cells(&v, 0, 2), Some(0x1234_5678_9abc_def0));
        // Past the end of the value.
        assert_eq!(read_cells(&v, 8, 1), None);
        assert_eq!(read_cells(&v, 4, 2), None);
        // Zero cells carry no value; never invent a `0`.
        assert_eq!(read_cells(&v, 0, 0), None);
        // Three or more cells cannot fit a `u64`; never wrap.
        assert_eq!(read_cells(&v, 0, 3), None);
    }

    #[test]
    fn name_stem_strips_the_unit_address() {
        assert_eq!(name_stem(b"serial@3f201000"), b"serial");
        assert_eq!(name_stem(b"timer"), b"timer");
        assert_eq!(name_stem(b""), b"");
        assert_eq!(name_stem(b"@7e340000"), b"");
    }
}
