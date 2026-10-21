//! Deterministic fuzz harness for the `lib/binfmt` rxe inspection view and
//! the report of every TAIRiX container (decoders of untrusted
//! executable-file bytes).
//!
//! [`tairix_binfmt::rxe::RxeView::parse`] and
//! [`tairix_binfmt::report::report`] decode any file a viewer is pointed at.
//! The harness invariants:
//!
//! * parsing any byte string never panics — it returns a view or a typed
//!   error (fail closed);
//! * a successful parse yields a view whose every accessor (header,
//!   segments, needed libraries) can be walked without a panic;
//! * a report of any program, driver image or manifest holds to the report's
//!   bounds: no more lines than it may hold, none too long, none carrying a
//!   control character.
//!
//! TAIRiX pulls in no external fuzz runner: a per-run-seeded `Prng` mutates a
//! valid image, driver image, manifest and `AppInfo` built through the
//! `lib/abi` encoders and mixes in pure noise. A plain `cargo test` runs the [`SMOKE_ITERATIONS`] sweep
//! once from a fresh, logged seed; `cargo xtask fuzz` exports
//! `TAIRIX_FUZZ_BUDGET_SECS` to extend the loop to a wall-clock budget.

use tairix_abi::{
    manifest_header, CapabilityId, DriverBindKey, DriverKind, DriverManifest, HwMatchKey,
    LoadHeader, ManifestHeader, NeededLibrary, RxePermission, Segment, ABI_VERSION_CURRENT,
    APPINFO_MAGIC, DRIVER_MANIFEST_MAGIC, LOAD_FLAG_PIE, LOAD_MAGIC, MANIFEST_MAGIC,
    MIME_ENTRY_LEN, RXE_PAGE_SIZE,
};
use tairix_binfmt::report::report;
use tairix_binfmt::rxe::RxeView;
use tairix_fuzzseed::Prng;
use tairix_util::report::{MAX_LINE_BYTES, MAX_REPORT_LINES};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 20_000;

/// Largest arbitrary byte string fed to the decoders.
const MAX_NOISE: usize = 1024;

/// A valid load image built through the `lib/abi` encoders.
fn valid_image() -> Vec<u8> {
    let code = Segment {
        vaddr: RXE_PAGE_SIZE,
        file_offset: 0,
        file_size: 64,
        mem_size: RXE_PAGE_SIZE,
        permission: RxePermission::ReadExecute,
    };
    let data = Segment {
        vaddr: RXE_PAGE_SIZE * 2,
        file_offset: 64,
        file_size: 32,
        mem_size: RXE_PAGE_SIZE,
        permission: RxePermission::ReadWrite,
    };
    let needed =
        NeededLibrary::from_reference("/System/Libraries/libexample.so").expect("valid reference");
    let header = LoadHeader {
        magic: LOAD_MAGIC,
        abi_version: ABI_VERSION_CURRENT,
        flags: LOAD_FLAG_PIE,
        segment_count: 2,
        needed_count: 1,
        entry: RXE_PAGE_SIZE + 16,
        cfi_tag: [0xA5; 32],
    };
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&header.to_le_bytes());
    bytes.extend_from_slice(&code.to_le_bytes());
    bytes.extend_from_slice(&data.to_le_bytes());
    bytes.extend_from_slice(&needed.to_le_bytes());
    bytes
}

/// A valid manifest built through the `lib/abi` encoders.
fn valid_manifest() -> Vec<u8> {
    let header = ManifestHeader {
        magic: MANIFEST_MAGIC,
        abi_version: ABI_VERSION_CURRENT,
        flags: 0,
        capability_count: 2,
        reserved0: 0,
        syscall_table_hash: [0x11; 32],
        signer_pubkey: [0x22; 32],
        signature: [0x33; 64],
    };
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&header.to_le_bytes());
    bytes.extend_from_slice(&CapabilityId::FS_MOUNT.as_u16().to_le_bytes());
    bytes.extend_from_slice(&CapabilityId::NET_RAW.as_u16().to_le_bytes());
    bytes
}

/// A valid driver image: a manifest binding one virtio device and asking for
/// IRQ binding, then the valid load image as its program.
fn valid_driver() -> Vec<u8> {
    let manifest = DriverManifest {
        magic: DRIVER_MANIFEST_MAGIC,
        abi_version: ABI_VERSION_CURRENT,
        kind: DriverKind::UserSpace,
        bind_key_count: 1,
        capability_count: 1,
        syscall_table_hash: [0x44; 32],
        signer_pubkey: [0x55; 32],
        signature: [0x66; 64],
    };
    let mut bytes = manifest.to_le_bytes().to_vec();
    bytes.extend_from_slice(&CapabilityId::IRQ_BIND.as_u16().to_le_bytes());
    bytes.extend_from_slice(&DriverBindKey::new(1, HwMatchKey::virtio(2)).to_le_bytes());
    bytes.extend_from_slice(&valid_image());
    bytes
}

/// A valid `AppInfo` asking for one capability and claiming one file type.
fn valid_appinfo() -> Vec<u8> {
    let mut header = manifest_header("org.example.viewer", "viewer");
    header.capability_count = 1;
    header.mime_count = 1;
    let mut bytes = header.to_le_bytes().to_vec();
    bytes.extend_from_slice(&CapabilityId::NET_RAW.as_u16().to_le_bytes());
    let mut mime = [0u8; MIME_ENTRY_LEN];
    mime[0] = 10;
    mime[1..11].copy_from_slice(b"text/plain");
    bytes.extend_from_slice(&mime);
    bytes
}

/// Decode `bytes` every way; a success must be walkable without a panic and
/// a report must hold to its bounds.
fn exercise(bytes: &[u8]) {
    if let Ok(view) = RxeView::parse(bytes) {
        let _ = view.header();
        let _ = view.entry();
        let _ = view.is_pie();
        for segment in view.segments() {
            let _ = segment.permission.is_executable();
        }
        for reference in view.needed_libraries() {
            let _ = reference.len();
        }
    }
    if let Some(report) = report(bytes) {
        assert!(report.lines().len() <= MAX_REPORT_LINES);
        for line in report.lines() {
            assert!(line.text.len() <= MAX_LINE_BYTES, "{:?}", line.text);
            assert!(!line.text.chars().any(char::is_control), "{:?}", line.text);
        }
    }
}

#[test]
fn parse_never_panics_for_any_input() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "parse_never_panics_for_any_input",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));

    let templates = [
        valid_image(),
        valid_driver(),
        valid_manifest(),
        valid_appinfo(),
    ];
    let magics = [
        LOAD_MAGIC,
        DRIVER_MANIFEST_MAGIC,
        MANIFEST_MAGIC,
        APPINFO_MAGIC,
    ];

    let mut iteration: u64 = 0;
    loop {
        // 1. A valid file of each format with a handful of bytes flipped.
        let mut mutated = rng.pick(&templates).clone();
        for _ in 0..rng.at_most(8) {
            let pos = rng.below(mutated.len());
            mutated[pos] ^= rng.next_u8();
        }
        exercise(&mutated);

        // 2. The same, truncated or extended at random.
        let cut = rng.at_most(mutated.len());
        exercise(&mutated[..cut]);
        mutated.extend((0..rng.at_most(64)).map(|_| rng.next_u8()));
        exercise(&mutated);

        // 3. Pure noise, optionally forced to open with a real magic.
        let mut noise = vec![0u8; rng.at_most(MAX_NOISE)];
        rng.fill(&mut noise);
        if noise.len() >= 4 && rng.next_u64() & 1 == 0 {
            noise[..4].copy_from_slice(&rng.pick(&magics).to_le_bytes());
        }
        exercise(&noise);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
