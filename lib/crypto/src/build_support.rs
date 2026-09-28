//! The build script's target decision, shared with the crate's host tests.

/// Whether the audited `sha2` crate can select its SHA-NI backend at run time
/// on the target `arch`/`os`/`env`: only on x86_64 with an operating system,
/// because its detection (`cpufeatures`) answers nothing on a freestanding,
/// UEFI or SGX target, where only compile-time features count.
#[must_use]
pub fn sha2_selects_hardware_at_runtime(arch: &str, os: &str, env: &str) -> bool {
    arch == "x86_64" && !matches!(os, "none" | "uefi") && env != "sgx"
}

#[cfg(test)]
mod tests {
    use super::sha2_selects_hardware_at_runtime as selects;

    #[test]
    fn only_a_hosted_x86_64_target_runs_the_hardware_path() {
        assert!(selects("x86_64", "linux", "gnu"));
        assert!(
            !selects("x86_64", "none", ""),
            "the TAIRiX kernel and user space"
        );
        assert!(!selects("x86_64", "uefi", ""));
        assert!(!selects("x86_64", "linux", "sgx"));
        assert!(!selects("aarch64", "linux", "gnu"));
        assert!(!selects("aarch64", "none", ""));
        assert!(!selects("riscv64", "none", ""));
    }
}
