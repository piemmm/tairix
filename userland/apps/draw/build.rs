//! Sets the `freestanding` cfg for the bare-metal targets the `Run` binary is
//! built for.

fn main() {
    tairix_buildcfg::emit_freestanding();
}
