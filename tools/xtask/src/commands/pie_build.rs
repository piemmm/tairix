//! The image pipeline's position-independent cross-compile of the `Run`
//! programs it ships — every driver and application bundle alike — through
//! the one recipe the kernel's embedded programs and the test fixtures build
//! by (`tairix_itest_harness::pie_build`), in the directory every build of the
//! same configuration shares.

use tairix_itest_harness::pie::PieArch;
use tairix_itest_harness::pie_build::{PieBuild, Recipe};
use tairix_mkimage::ImageProfile;

use crate::{Context, LONG_BUILD_COMMAND_TIMEOUT};

/// Compile `package`'s binary `bin`, with `features`, position-independent
/// for `arch` in the Cargo profile `profile` builds in, and return the linked
/// ELF.
///
/// Every bundle is part of the generic per-arch user space, so it builds
/// against that image's CPU floor — the same floor the generic image's kernel
/// uses — ahead of the recipe's own flags.
///
/// # Errors
///
/// A failed cross-compile or an unreadable artefact.
pub fn cross_compile_pie_elf(
    ctx: &Context,
    arch: PieArch,
    package: &str,
    bin: &str,
    features: &[&str],
    profile: ImageProfile,
) -> Result<Vec<u8>, String> {
    let floor = crate::floor::floor_for_image(crate::floor::ImageKind::generic_for_pie_arch(arch))
        .floor_tokens();
    let root = ctx.target_dir();
    let profile = super::cargo_profile(profile);
    let build = PieBuild {
        root: &root,
        workspace: &ctx.workspace_root,
        arch,
        recipe: Recipe::Program,
        profile,
        floor: &floor,
        env: &[],
    };
    let shared = build.lock().map_err(|e| format!("image: {e}"))?;
    // A program's first build in a configuration compiles its sysroot from
    // source, which can outrun an incremental host compile pass.
    ctx.run_with_timeout(
        &format!(
            "image: program build ({package}, {}, {})",
            arch.target_triple(),
            profile.dir()
        ),
        shared.command(&[package], &[bin], features),
        LONG_BUILD_COMMAND_TIMEOUT,
    )?;
    let elf = shared.artefact(bin);
    std::fs::read(&elf)
        .map_err(|e| format!("image: cannot read program ELF {}: {e}", elf.display()))
}
