//! Build script: select the freestanding build of the fixture and the
//! instruction set its compiler role writes functions in.
//!
//! The compiler role assembles a function that returns a chosen value, which
//! has no architecture-neutral spelling, so the instruction-set decision is
//! made here in build glue (`code_install_<arch>`), as `threads_program`'s is.

fn main() {
    tairix_buildcfg::emit_freestanding();
    for arch in tairix_buildcfg::NATIVE_ARCHES {
        println!("cargo:rustc-check-cfg=cfg(code_install_{arch})");
    }
    if let Some(arch) = tairix_buildcfg::native_arch() {
        println!("cargo:rustc-cfg=code_install_{arch}");
    }
}
