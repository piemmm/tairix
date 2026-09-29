# tairix-fdt

The one flattened-device-tree (DTB) reader, for the Devicetree Specification
v0.4 layout. Every port whose firmware hands it a device tree builds its
platform discovery on it; the architecture-specific queries (PSCI method,
timer interrupts) stay in each port.

## API

- `Fdt::new` validates a borrowed blob; its readers walk the tree without
  copying: `nodes`, `find_compatible`, `node_by_phandle`, `property`, the
  memory regions, the CPUs, `timebase_frequency`, `chosen_rng_seed`,
  `boot_cpu_compatible`. The node and property iterators end at the first
  malformed token.
- `supply` resolves a consumer's `<name>-supply` regulator and decodes the
  two GPIO-switched shapes: a `regulator-gpio` selecting voltage `states`
  (`gpio_selected_regulator`) and a `regulator-fixed` with an enable line
  (`gpio_enabled_regulator`).
- `bus` translates addresses through the tree's `ranges` and `dma-ranges`:
  `translate`, `translated_reg`, `dma_ranges`, `dma_reach`,
  `outbound_mmio_window`.
- `fixture` (feature `test-fixtures`) builds small trees in memory for the
  ports' discovery tests.

## Design

- `no_std`, and allocation-free outside the fixture builder.
- The blob comes from firmware and is untrusted: every read is bounds-checked
  and a malformed tree is refused with `FdtError`. `fuzz_fdt`
  (`tests/fuzz_fdt.rs`) drives mutated and truncated trees through every
  reader.

## Stability

Tier: `experimental`.
