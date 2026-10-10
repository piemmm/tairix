# `tairix-fdt`

`lib/fdt` reads the flattened device tree — the Devicetree Specification v0.4
blob a port's firmware hands it — once for every port, and writes one for a
build tool that hands a tree to firmware.

## Reading

`Fdt::new` validates a borrowed blob, and its readers walk it without copying:
the nodes, the operational ones (whose `status`, and their ancestors', lets
them be used), a node by `compatible` or by phandle, properties, the memory
regions and the CPUs. The architecture-specific queries — a PSCI method, a
timer's interrupts, a RISC-V timebase — stay in each port's own discovery.

The blob comes from firmware, so it is untrusted: every read is bounds-checked
against it, a malformed tree is refused with `FdtError`, and the node and
property iterators end at the first malformed token. The fuzz harness
`fuzz_fdt` drives mutated, truncated and arbitrary trees through every reader.

Above the walk, the crate decodes what discovery needs from the generic
bindings: a phandle-and-specifier list (`dmas`, `iommus`) framed by each
provider's cell count, an id map (`iommu-map`, `msi-map`), a translation
unit's specifier and the windows it keeps, a consumer's regulator supply, and
address translation through `ranges` and `dma-ranges`.

## Writing

`write::FdtWriter`, behind the `writer` feature since it allocates, is what
`tools/mkimage` writes the Raspberry Pi image's
[device-tree overlay](../platform/aarch64.md) with. A tree is written node by
node and laid out by `finish`: the header, an empty memory-reservation map,
the structure block and the strings block, each property name stored once —
the layout libfdt, and so a board's firmware, checks. `finish` refuses a tree
that does not nest into exactly one root, a property outside every node, and
a name or string holding a NUL, so a malformed tree is never written.

The test trees the ports' discovery tests read are built with the same writer
(`fixture`, behind `test-fixtures`), so the reader is proven against the one
encoder the tree has.

## Stability

**experimental**.
