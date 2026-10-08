//! Build script: the `itest_*` flags this support library selects its
//! aarch64 half by.

fn main() {
    tairix_itest_harness::emit_target_cfg();
}
