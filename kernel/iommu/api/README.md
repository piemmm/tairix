# `kernel/iommu/api` — the DMA translation unit contract

What every translation-unit family implements and what the kernel builds on
it: the `IommuUnit` contract, a `Domain` (a unit's domain, its IOVA space and
the ledger of what it maps), the top-down buddy `IovaSpace`, the radix
`IoPageTable` engine over a `PteFormat`, the per-stream `FaultBudget`, and the
conformance suite a family passes against its register model. The design is
`plans/IOMMU.md`; the kernel's use of it is
[`docs/src/security/iommu.md`](../../../docs/src/security/iommu.md).

## Stability tier

**experimental**.

## Invariants

- Nothing a domain removed is reused before the unit confirms no cached
  translation of it survives; an operation the unit cannot confirm answers
  `Unconfirmed`, and what it covered stays out of reuse for good.
- Allocation failure is a value: every structure reserves room before it
  changes, so running out of memory leaves it as it was.

## Tests

Host unit tests over `hostmem::HostFrames` and the reference `model::ModelUnit`
(behind `host-tests` for the family crates' own tests); the crate is enrolled
in `cargo xtask miri`.
