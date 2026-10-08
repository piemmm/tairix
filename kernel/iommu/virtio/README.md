# `kernel/iommu/virtio` — virtio-iommu

One virtio-iommu, the paravirtual unit a hypervisor offers its guests: a
domain per owner, every attach, detach, map and unmap a request on its request
queue, answered only once the device has applied it, so an answered unmap is a
confirmed one; the device's reserved regions probed per endpoint and kept out
of the endpoint's domain, a device that cannot be probed refused, an endpoint
the device does not have — a
requester id the fabric delivers under an alias — attached and probed as
nothing; and fault reports read from its event queue in batches, each buffer
reposted as it is read. Bound by discovery to nodes keyed
`compatible "virtio,pci-iommu"` (`COMPATIBLE`) — a PCI function, from a device
tree or the ACPI VIOT — and to a `virtio,mmio` slot whose device is one
(`DEVICE_ID`). The design is `plans/IOMMU.md` IOM17.

The device keeps the translations, so the family keeps a shadow of each domain
in the radix tree every table-walking family builds, walked by no unit:
QEMU forgets a domain whose last endpoint detaches, so a domain attached again
has its mappings replayed into it, and an unmap is refused unless it names
whole mappings, as a split is refused everywhere else. An endpoint no domain
holds is blocked: bypass is cleared through the configuration where the device
lets it be written, and the feature that would let unattached endpoints bypass
is never accepted. A request the device leaves unanswered stops the unit: the
buffer may still be written and what the device did is unknown, so nothing
more is sent and nothing it held is reported gone.

## Stability tier

**experimental**.

## Hardware

Virtio 1.3 virtio-iommu devices on the modern transport whose smallest page
is at most 4 KiB. A PCI function raises its event queue on its INTx line where
the host's bring-up resolves it, else through its MSI-X entry; a slot raises
its one line. QEMU's `virtio-iommu-pci` has no MSI-X, and on x86_64 its INTx
is routed only through ACPI's `_PRT` (`plans/ACPI.md` A5), so there it
translates with its faults unrouted (`faults_unrouted`, `reason=no_line`). A
silenced endpoint's reports are dropped by the family, the device having no
way to stop them. Interrupt remapping, page requests and nested translation
are not offered by the device and are not used; the MMIO-region map flag is
not used.

## Tests

Host tests against a model device (its queues; its domains, forgotten on the
last detach as QEMU's are, or kept; its endpoints, reserved regions and fault
reports; and the devices the family refuses or holds back: one never
answering, one whose bypass will not clear, one offering only the older bypass
feature), including the shared conformance suite with no translated request
to carry; enrolled in `cargo xtask miri`. On QEMU,
`tairix-test-dma-translation-virtio-qemu-x86-64`,
`tairix-test-dma-translation-virtio-qemu-aarch64` and
`tairix-test-dma-translation-virtio-qemu-riscv64` run the production kernel
behind a `virtio-iommu-pci`, its x86_64 topology read from the VIOT.
