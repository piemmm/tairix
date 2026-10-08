//! Build script: the aarch64 `virt` vertical build, embedding for each binary
//! the tree of the machine it boots, its unit included.

use tairix_itest_harness::{Board, DmaTranslation, InterruptControllers};

fn main() {
    tairix_itest_harness::aarch64_virt_guest_build_trees(&[
        (
            "stage2_tree",
            Board::translated(DmaTranslation::Smmuv3Stage2),
            1,
        ),
        (
            "stage1_tree",
            Board::translated(DmaTranslation::Smmuv3Stage1),
            1,
        ),
        (
            "virtio_tree",
            Board::translated(DmaTranslation::VirtioIommu),
            1,
        ),
        (
            "gicv3_tree",
            Board::translated(DmaTranslation::Smmuv3Stage2)
                .with_interrupts(InterruptControllers::Gicv3),
            1,
        ),
    ]);
}
