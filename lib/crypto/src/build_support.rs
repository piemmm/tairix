//! The build script's target decision, shared with the crate's host tests.

/// A hardware SHA-256 backend the audited `sha2` crate can take.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HwSha256 {
    /// x86's SHA extensions.
    ShaNi,
    /// The ARMv8 cryptographic extension's SHA-256 instructions.
    Armv8,
}

/// What a build target is, and how `sha2` is configured for it, as cargo tells
/// a build script.
pub struct Sha2Build<'a> {
    /// `CARGO_CFG_TARGET_ARCH`.
    pub arch: &'a str,
    /// `CARGO_CFG_TARGET_OS`.
    pub os: &'a str,
    /// `CARGO_CFG_TARGET_VENDOR`.
    pub vendor: &'a str,
    /// `CARGO_CFG_TARGET_ENV`.
    pub env: &'a str,
    /// `CARGO_CFG_TARGET_FEATURE`: the compile-time features, comma-separated.
    pub features: &'a str,
    /// Whether the build is interpreted, where `cpufeatures` answers from the
    /// compile-time features alone.
    pub miri: bool,
    /// The backend the family-wide `sha2_backend` cfg names, if any.
    pub backend: Option<&'a str>,
    /// The backend the SHA-256 `sha2_256_backend` cfg names, if any.
    pub backend_256: Option<&'a str>,
}

impl Sha2Build<'_> {
    fn has(&self, feature: &str) -> bool {
        self.features.split(',').any(|has| has == feature)
    }

    /// Whether either backend cfg names `backend`.
    fn names(&self, backend: &str) -> bool {
        self.backend == Some(backend) || self.backend_256 == Some(backend)
    }
}

/// Which hardware SHA-256 backend `sha2` 0.11 can take on `build`, as its own
/// dispatch decides: none where either cfg forces `soft`, the x86 or ARMv8
/// one where `sha2_256_backend` forces it; else on x86 SHA-NI where its
/// four features are compile-time ones or `cpufeatures` detects at run time —
/// an operating system that is not SGX's — and on AArch64 the ARMv8 extension
/// where `sha2` is a compile-time feature or detection runs (Linux, Android,
/// Apple). No TAIRiX target reaches either: no floor sets the features, and
/// detection answers nothing without an operating system.
///
/// # Errors
///
/// A forced backend this crate keeps no record of, named, so a build stops
/// rather than record software where hardware runs.
pub fn sha2_hardware_backend<'a>(build: &Sha2Build<'a>) -> Result<Option<HwSha256>, &'a str> {
    if build.names("soft") {
        return Ok(None);
    }
    if build.names("riscv-zknh") {
        return Err("riscv-zknh");
    }
    match build.backend_256 {
        Some("x86-sha") => return Ok(Some(HwSha256::ShaNi)),
        Some("aarch64-sha2") => return Ok(Some(HwSha256::Armv8)),
        _ => {}
    }
    let detects = !build.miri;
    Ok(match build.arch {
        "x86" | "x86_64" => {
            let compiled = ["sha", "sse2", "ssse3", "sse4.1"]
                .iter()
                .all(|feature| build.has(feature));
            let detected = !matches!(build.os, "none" | "uefi") && build.env != "sgx";
            (compiled || (detects && detected)).then_some(HwSha256::ShaNi)
        }
        "aarch64" => {
            let detected = matches!(build.os, "linux" | "android") || build.vendor == "apple";
            (build.has("sha2") || (detects && detected)).then_some(HwSha256::Armv8)
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::{sha2_hardware_backend, HwSha256, Sha2Build};

    fn build<'a>(arch: &'a str, os: &'a str, vendor: &'a str, features: &'a str) -> Sha2Build<'a> {
        Sha2Build {
            arch,
            os,
            vendor,
            env: if os == "linux" { "gnu" } else { "" },
            features,
            miri: false,
            backend: None,
            backend_256: None,
        }
    }

    #[test]
    fn the_hardware_backend_is_the_one_sha2_s_own_dispatch_takes() {
        let backend = |build: Sha2Build<'static>| sha2_hardware_backend(&build);
        let armv8 = Ok(Some(HwSha256::Armv8));
        let shani = Ok(Some(HwSha256::ShaNi));
        assert_eq!(backend(build("aarch64", "linux", "unknown", "neon")), armv8);
        assert_eq!(
            backend(build("aarch64", "android", "unknown", "neon")),
            armv8
        );
        assert_eq!(backend(build("aarch64", "macos", "apple", "neon")), armv8);
        assert_eq!(
            backend(build("aarch64", "none", "unknown", "neon")),
            Ok(None),
            "a TAIRiX target"
        );
        assert_eq!(backend(build("aarch64", "uefi", "unknown", "")), Ok(None));
        assert_eq!(
            backend(build("aarch64", "none", "unknown", "neon,sha2")),
            armv8
        );
        assert_eq!(backend(build("x86_64", "linux", "unknown", "sse2")), shani);
        assert_eq!(
            backend(build("x86_64", "none", "unknown", "sse2")),
            Ok(None)
        );
        assert_eq!(
            backend(build("x86_64", "none", "unknown", "sha,sse2,ssse3,sse4.1")),
            shani
        );
        assert_eq!(
            backend(Sha2Build {
                env: "sgx",
                ..build("x86_64", "unknown", "fortanix", "sse2")
            }),
            Ok(None)
        );
        assert_eq!(backend(build("riscv64", "none", "unknown", "")), Ok(None));
    }

    #[test]
    fn an_interpreted_build_counts_only_the_compile_time_features() {
        let interpreted = |arch, features| Sha2Build {
            miri: true,
            ..build(arch, "linux", "unknown", features)
        };
        assert_eq!(
            sha2_hardware_backend(&interpreted("x86_64", "sse2")),
            Ok(None)
        );
        assert_eq!(
            sha2_hardware_backend(&interpreted("aarch64", "neon")),
            Ok(None)
        );
        assert_eq!(
            sha2_hardware_backend(&interpreted("aarch64", "neon,sha2")),
            Ok(Some(HwSha256::Armv8))
        );
    }

    #[test]
    fn a_forced_backend_is_taken_as_sha2_takes_it() {
        let hosted = || build("aarch64", "linux", "unknown", "neon,sha2");
        let family = |backend| Sha2Build {
            backend: Some(backend),
            ..hosted()
        };
        let sha256 = |backend| Sha2Build {
            backend_256: Some(backend),
            ..hosted()
        };
        assert_eq!(sha2_hardware_backend(&family("soft")), Ok(None));
        assert_eq!(sha2_hardware_backend(&sha256("soft")), Ok(None));
        assert_eq!(
            sha2_hardware_backend(&sha256("x86-sha")),
            Ok(Some(HwSha256::ShaNi))
        );
        assert_eq!(
            sha2_hardware_backend(&family("x86-sha")),
            Ok(Some(HwSha256::Armv8)),
            "only the SHA-256 key forces a hardware backend"
        );
        assert_eq!(
            sha2_hardware_backend(&sha256("aarch64-sha2")),
            Ok(Some(HwSha256::Armv8))
        );
        assert_eq!(
            sha2_hardware_backend(&family("riscv-zknh")),
            Err("riscv-zknh"),
            "no record of it, so no build"
        );
    }
}
