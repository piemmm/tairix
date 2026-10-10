# tairix-fdt

The one flattened-device-tree (DTB) reader, for the Devicetree Specification
v0.4 layout. Every port whose firmware hands it a device tree builds its
platform discovery on it; the architecture-specific queries (PSCI method,
timer interrupts) stay in each port.

## API

- `Fdt::new` validates a borrowed blob; its readers walk the tree without
  copying: `nodes`, `operational_nodes` and `nodes_in_use` (the nodes whose
  `status`, and their ancestors', lets them be used), `find_compatible`,
  `node_by_phandle`, `property`, the enabled memory regions, the CPUs that
  may be started, `timebase_frequency`, `chosen_rng_seed`,
  `boot_cpu_compatible`. The node and property iterators end at the first
  malformed token.
- `phandle_args` walks a phandle-and-specifier list (`dmas`, `iommus`), each
  entry framed by its provider's cell count.
- `IdMap` reads an id map (`iommu-map` or `msi-map`, with its mask), refused
  whole when it does not decode; `pci` gives each generic host its
  `iommu_map`.
- `iommu` marks a translation unit (`iommu_cells`), reads a one-cell
  specifier's stream id, and the `iommu-addresses` windows a master's
  `memory-region` regions ask its translation to keep.
- `supply` resolves a consumer's `<name>-supply` regulator and decodes the
  two GPIO-switched shapes: a `regulator-gpio` selecting voltage `states`
  (`gpio_selected_regulator`) and a `regulator-fixed` with an enable line
  (`gpio_enabled_regulator`).
- `bus` translates addresses through the tree's `ranges` and `dma-ranges`:
  `translate`, `translated_reg`, `dma_ranges`, `dma_reach`,
  `outbound_mmio_window`.
- `write` (feature `writer`) writes a tree, for a build tool emitting one for
  firmware: `FdtWriter` lays out the header, an empty reservation map, the
  structure and the strings, each name stored once, and `finish` refuses a
  tree that does not nest into one root or holds a NUL in a name.
- `fixture` (feature `test-fixtures`) builds the trees the ports' discovery
  tests read, with the same writer.

## Design

- `no_std`, and allocation-free outside the writer.
- The blob comes from firmware and is untrusted: every read is bounds-checked
  and a malformed tree is refused with `FdtError`. `fuzz_fdt`
  (`tests/fuzz_fdt.rs`) drives mutated and truncated trees through every
  reader.

## Stability

Tier: `experimental`.
