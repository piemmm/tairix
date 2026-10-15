//! Enables the `freestanding` cfg on a bare-metal target.

fn main() {
    tairix_buildcfg::emit_freestanding();
}
