//! The glue every freestanding fixture program's build script shares: the
//! one nested build ([`GuestBuild`](crate::program_fixture::GuestBuild)) that
//! turns a guest crate into the `rxe` image the kernel spawn path loads, and
//! the one emitter of the `PROGRAM_RXE` + `USER_BIAS` source they `include!`.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tairix_abi::syscall::SYSCALL_TABLE_HASH_LEN;

use crate::pie::{PieArch, Profile};
use crate::pie_build::{self, PieBuild, Recipe};
use crate::USER_IMAGE_BIAS;

/// One nested `cargo build` of a fixture's guest crate for a freestanding
/// target, in the directory every build of its configuration shares
/// ([`PieBuild`]), with the outer fixture's freshness taken from the
/// compiler's own dep-info record: an edit anywhere in the guest's dependency
/// closure rebuilds the embedded blob rather than leaving the vertical to pass
/// against code no longer in the tree.
pub struct GuestBuild<'a> {
    /// Directory the nested `cargo` runs in — the fixture's own manifest
    /// directory, so it resolves this workspace.
    pub manifest_dir: &'a str,
    /// The fixture's `OUT_DIR`, which the shared directory's root is found
    /// from and an archive is kept in.
    pub out_dir: &'a str,
    /// Freestanding target the guest is cross-compiled for.
    pub arch: PieArch,
    /// Cargo package to build, which also names its artefact.
    pub package: &'a str,
    /// Environment the guest's own source reads through `env!`, so this
    /// script stays the single source of truth for the constants both halves
    /// of a fixture share. Cargo rebuilds the guest when a value changes.
    pub env: &'a [(&'a str, String)],
}

impl GuestBuild<'_> {
    /// Cross-compile the guest position-independent against the one PIE link
    /// script and convert the linked ELF into the `rxe` load image the kernel
    /// spawn path accepts, with relocations baked for [`USER_IMAGE_BIAS`] and
    /// the kernel's syscall `cfi_tag` stamped in.
    ///
    /// # Panics
    ///
    /// If the nested build fails, its ELF is unreadable, or the ELF is not
    /// convertible: a fixture that cannot produce its program must fail the
    /// build loudly rather than embed a stale or empty blob.
    #[must_use]
    pub fn program_rxe(&self, cfi_tag: &[u8; SYSCALL_TABLE_HASH_LEN]) -> Vec<u8> {
        let elf = self.build(Recipe::Program, Profile::Dev, |elf| fs::read(elf));
        crate::elf2rxe::elf_to_rxe(&elf, cfi_tag, USER_IMAGE_BIAS)
            .unwrap_or_else(|e| panic!("convert {} into an rxe image: {e:?}", self.label()))
    }

    /// Cross-compile the guest as a position-independent `staticlib`, for a
    /// vertical that links a foreign object against the runtime rather than
    /// spawning a Rust program, and return the archive, kept in `OUT_DIR`.
    ///
    /// # Panics
    ///
    /// If the nested build fails or the archive cannot be kept, for the reason
    /// [`program_rxe`](Self::program_rxe) states.
    #[must_use]
    pub fn static_archive(&self) -> PathBuf {
        self.build(Recipe::Archive, Profile::Release, |archive| {
            let kept = Path::new(self.out_dir).join(archive.file_name().unwrap_or_default());
            fs::copy(archive, &kept).map(|_| kept)
        })
    }

    /// Drive the nested build, register every input it consumed with the
    /// outer cargo, and `take` from the artefact while the shared directory
    /// is still held, so no other build relinks it in between.
    fn build<T>(
        &self,
        recipe: Recipe,
        profile: Profile,
        take: impl FnOnce(&Path) -> io::Result<T>,
    ) -> T {
        let label = self.label();
        let root = pie_build::build_root(Path::new(self.out_dir), self.arch.target_triple())
            .unwrap_or_else(|e| panic!("building {label}: {e}"));
        let build = PieBuild {
            root: &root,
            workspace: Path::new(self.manifest_dir),
            arch: self.arch,
            recipe,
            profile,
            floor: &[],
            env: self.env,
        };
        let shared = build
            .lock()
            .unwrap_or_else(|e| panic!("building {label}: {e}"));
        let status = shared
            .command(&[self.package], &[], &[])
            .status()
            .unwrap_or_else(|e| panic!("spawn cargo to build {label}: {e}"));
        assert!(status.success(), "building {label} failed");
        let artefact = shared.artefact(self.package);
        shared.emit_reruns(&artefact);
        take(&artefact).unwrap_or_else(|e| panic!("{} for {label}: {e}", artefact.display()))
    }

    /// How this build names itself in a failure message: its package, and
    /// the environment that tells two builds of it apart.
    fn label(&self) -> String {
        let mut label = self.package.to_string();
        for (index, (key, value)) in self.env.iter().enumerate() {
            label.push_str(if index == 0 { " (" } else { ", " });
            let _ = write!(label, "{key}={value}");
        }
        if !self.env.is_empty() {
            label.push(')');
        }
        label
    }
}

/// Buffer length [`format_grouped_hex`] renders into: `0x`, sixteen hex
/// digits, and the three `_` separators between the four digit groups.
pub const GROUPED_HEX_LEN: usize = 2 + 16 + 3;

/// Render `value` as a Rust hexadecimal literal with `_` between each group
/// of four digits (`0x0010_0000_0000`), into `out`.
///
/// The build script emits Rust source the crate `include!`s, and generated
/// source is linted like any other: a bare `{value:#x}` there trips the
/// "long literal lacking separators" lint at the *include site*, where it
/// cannot be fixed. Grouping at the point of emission keeps the generated
/// file idiomatic on its own terms, instead of stamping a blanket lint
/// exemption into it that would also hide a future real finding in the same
/// file. Leading all-zero groups are dropped, but never the last one, so
/// zero renders as `0x0000`. It renders into the caller's buffer rather
/// than allocating, so the kernel image builder can also call it while
/// laying out its own generated constants.
pub fn format_grouped_hex(value: u64, out: &mut [u8; GROUPED_HEX_LEN]) -> &str {
    let mut digits = [b'0'; 16];
    for (i, slot) in digits.iter_mut().enumerate() {
        let nibble = ((value >> ((15 - i) * 4)) & 0xf) as u8;
        *slot = match nibble {
            0..=9 => b'0' + nibble,
            _ => b'a' + (nibble - 10),
        };
    }
    // Whole groups only: every emitted group is four digits wide, which is
    // exactly the shape the lint accepts.
    let mut first = 0;
    while first + 4 < digits.len() && digits[first..first + 4] == [b'0'; 4] {
        first += 4;
    }

    out[0] = b'0';
    out[1] = b'x';
    let mut len = 2;
    for group in digits[first..].as_chunks::<4>().0 {
        if len > 2 {
            out[len] = b'_';
            len += 1;
        }
        out[len..len + 4].copy_from_slice(group);
        len += 4;
    }
    // Every byte written above is ASCII, so the prefix is valid UTF-8; fall
    // back to the empty string rather than panicking if that were broken.
    core::str::from_utf8(&out[..len]).unwrap_or("")
}

/// The heap region [`push_heap_region`] emits: 64 GiB from 80 GiB, clear of
/// the image at [`USER_IMAGE_BIAS`] and inside every port's user address
/// space.
const USER_HEAP_BASE: u64 = 0x14_0000_0000;
const USER_HEAP_LEN: u64 = 0x10_0000_0000;

/// Open a fixture source: the generated-file banner and the shared
/// [`USER_IMAGE_BIAS`] the images' relocations were baked for, emitted as
/// `USER_BIAS`.
///
/// The bias is rendered through [`format_grouped_hex`] because the emitted
/// file is compiled and linted like any other source; a bare
/// `0x1000000000` there trips the missing-separator lint at the `include!`
/// site, where it cannot be fixed.
///
/// A caller pins its own extra constants onto the returned string, appends
/// each image with [`push_rxe_blob`], and writes the whole thing with
/// [`write_fixture`].
#[must_use]
pub fn fixture_header() -> String {
    let mut out = String::new();
    out.push_str("// Auto-generated by build.rs. DO NOT EDIT.\n");
    out.push_str("/// Virtual base every program image is mapped at.\n");
    let mut bias = [0u8; GROUPED_HEX_LEN];
    let _ = writeln!(
        out,
        "pub const USER_BIAS: u64 = {};",
        format_grouped_hex(USER_IMAGE_BIAS, &mut bias)
    );
    out
}

/// Append the heap region a fixture that builds its own spawn request hands
/// its program, as `USER_HEAP_BASE` and `USER_HEAP_LEN`.
pub fn push_heap_region(out: &mut String) {
    let mut hex = [0u8; GROUPED_HEX_LEN];
    out.push_str("/// The address space the program's heap grows in.\n");
    let _ = writeln!(
        out,
        "pub const USER_HEAP_BASE: u64 = {};",
        format_grouped_hex(USER_HEAP_BASE, &mut hex)
    );
    let _ = writeln!(
        out,
        "pub const USER_HEAP_LEN: u64 = {};",
        format_grouped_hex(USER_HEAP_LEN, &mut hex)
    );
}

/// Append one converted `rxe` image to a fixture source as
/// `pub const <name>: &[u8]`, documented as the image of `description`
/// ("the heap fixture program", "the parent role").
///
/// A host build passes an empty `rxe`, so the vertical's inert stub still
/// compiles against the same names.
pub fn push_rxe_blob(out: &mut String, name: &str, description: &str, rxe: &[u8]) {
    let _ = writeln!(out, "/// The converted `rxe` image of {description}.");
    let _ = write!(out, "pub const {name}: &[u8] = &[");
    for (i, b) in rxe.iter().enumerate() {
        if i % 16 == 0 {
            out.push_str("\n    ");
        }
        let _ = write!(out, "0x{b:02x}, ");
    }
    out.push_str("\n];\n");
}

/// Write a completed fixture `source` to `path`.
///
/// # Panics
///
/// If the write fails: a build that could not emit its fixture must fail
/// loudly rather than leave a stale one in place for the compiler to pick
/// up.
pub fn write_fixture(path: &Path, source: &str) {
    fs::write(path, source).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A build of `tairix-test-probe` with the given pinned environment.
    fn probe<'a>(env: &'a [(&'a str, String)]) -> GuestBuild<'a> {
        GuestBuild {
            manifest_dir: ".",
            out_dir: "/out",
            arch: PieArch::Aarch64,
            package: "tairix-test-probe",
            env,
        }
    }

    /// No fixture build script may enumerate a guest crate's cargo-tracked
    /// inputs by hand. An edit to a crate the guest *depends on* matches no
    /// such list, so the outer script does not rerun, the nested build is
    /// never re-driven, and the vertical passes against a blob built from
    /// code that is no longer in the tree — the dangerous failure, because
    /// it is green. [`GuestBuild`] derives the closure from the compiler's
    /// own dep-info record instead.
    #[test]
    fn no_fixture_build_script_enumerates_its_guest_inputs_by_hand() {
        let verticals = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the harness sits inside tests/integration");
        let mut offenders: Vec<String> = Vec::new();
        for entry in fs::read_dir(verticals).expect("read tests/integration") {
            let script = entry.expect("read a vertical").path().join("build.rs");
            let Ok(source) = fs::read_to_string(&script) else {
                continue;
            };
            for (number, line) in source.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                for hand_kept in ["/src/main.rs", "/src/lib.rs", "/Cargo.toml"] {
                    if code.contains(hand_kept) {
                        offenders.push(format!("{}:{}", script.display(), number + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these build scripts name a guest's cargo-tracked inputs by hand \
             instead of deriving the closure through GuestBuild: {offenders:#?}"
        );
    }

    /// A build names itself by its package and the environment that tells
    /// two builds of it apart.
    #[test]
    fn a_build_names_its_package_and_environment_when_it_fails() {
        assert_eq!(probe(&[]).label(), "tairix-test-probe");
        let roled = [
            ("TAIRIX_WAIT_ROLE", String::from("parent")),
            ("TAIRIX_WAIT_CHILD_CODE", String::from("23")),
        ];
        assert_eq!(
            probe(&roled).label(),
            "tairix-test-probe (TAIRIX_WAIT_ROLE=parent, TAIRIX_WAIT_CHILD_CODE=23)"
        );
    }

    /// A composed fixture source carries the shared bias as a grouped
    /// literal, the caller's own pinned constants, and every image byte.
    #[test]
    fn a_composed_fixture_source_carries_the_bias_and_every_byte() {
        let mut src = fixture_header();
        assert!(
            src.contains("pub const USER_BIAS: u64 = 0x0010_0000_0000;"),
            "{src}"
        );
        push_heap_region(&mut src);
        for line in [
            "pub const USER_HEAP_BASE: u64 = 0x0014_0000_0000;",
            "pub const USER_HEAP_LEN: u64 = 0x0010_0000_0000;",
        ] {
            assert!(src.contains(line), "{src}");
        }
        src.push_str("pub const ROUNDS: u64 = 3;\n");
        push_rxe_blob(
            &mut src,
            "PROGRAM_RXE",
            "the probe fixture program",
            &[0x7f, 0x45, 0x4c, 0x46],
        );
        assert!(
            src.contains("/// The converted `rxe` image of the probe fixture program."),
            "{src}"
        );
        assert!(src.contains("pub const ROUNDS: u64 = 3;"), "{src}");
        assert!(src.contains("0x7f, 0x45, 0x4c, 0x46,"), "{src}");
        // A host build emits the empty form, which must still be valid source.
        let mut stub = fixture_header();
        push_rxe_blob(&mut stub, "PROGRAM_RXE", "the probe fixture program", &[]);
        assert!(
            stub.contains("pub const PROGRAM_RXE: &[u8] = &[\n];"),
            "{stub}"
        );
    }

    #[test]
    fn grouped_hex_literals_are_separated_every_four_digits() {
        let mut buf = [0u8; GROUPED_HEX_LEN];
        // The user-image bias the generator emits: the regression this
        // helper exists for, a bare `0x1000000000` in generated source.
        assert_eq!(
            format_grouped_hex(0x10_0000_0000, &mut buf),
            "0x0010_0000_0000"
        );
        // Leading all-zero groups are dropped so the literal stays short…
        assert_eq!(format_grouped_hex(0x1234, &mut buf), "0x1234");
        assert_eq!(format_grouped_hex(0x1_2345, &mut buf), "0x0001_2345");
        // …but never the last group, so zero is still a literal.
        assert_eq!(format_grouped_hex(0, &mut buf), "0x0000");
        // Full width round-trips every nibble.
        assert_eq!(
            format_grouped_hex(u64::MAX, &mut buf),
            "0xffff_ffff_ffff_ffff"
        );
        assert_eq!(
            format_grouped_hex(0x0123_4567_89ab_cdef, &mut buf),
            "0x0123_4567_89ab_cdef"
        );
    }

    #[test]
    fn grouped_hex_output_parses_back_to_the_value_it_rendered() {
        // The generated file is compiled, so the rendering must be a Rust
        // literal that means exactly the value asked for — a separator in
        // the wrong place would silently change a mapped base address.
        for value in [
            0,
            1,
            0xffff,
            0x1_0000,
            0x10_0000_0000,
            0xdead_beef_cafe_f00d,
            u64::MAX,
        ] {
            let mut buf = [0u8; GROUPED_HEX_LEN];
            let rendered = format_grouped_hex(value, &mut buf);
            let mut parsed: u64 = 0;
            for digit in rendered.trim_start_matches("0x").bytes() {
                if digit == b'_' {
                    continue;
                }
                let nibble = char::from(digit).to_digit(16).expect("hex digit");
                parsed = (parsed << 4) | u64::from(nibble);
            }
            assert_eq!(parsed, value, "{rendered}");
            // Every group is exactly four digits — the shape the
            // separator lint accepts without complaint.
            for group in rendered.trim_start_matches("0x").split('_') {
                assert_eq!(group.len(), 4, "{rendered}");
            }
        }
    }
}
