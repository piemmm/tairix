//! Build script: the aarch64 `virt` vertical build, embedding for each binary
//! the tree of the SMMU machine it boots.

use tairix_itest_harness::DmaTranslation;

fn main() {
    tairix_itest_harness::aarch64_virt_guest_build_trees(
        1,
        &[
            ("stage2_tree", DmaTranslation::Smmuv3Stage2),
            ("stage1_tree", DmaTranslation::Smmuv3Stage1),
        ],
    );
}
