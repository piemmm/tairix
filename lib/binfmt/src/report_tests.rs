use alloc::vec::Vec;

use tairix_abi::{
    manifest_header, CapabilityId, DriverBindKey, DriverKind, DriverManifest, HwMatchKey,
    LibraryCategory, ProgramKind, APPINFO_FLAG_DOCUMENT_WRITE, APPINFO_FLAG_MULTI_INSTANCE,
    DRIVER_MANIFEST_MAGIC, MIME_ENTRY_LEN,
};
use tairix_util::report::{Report, MAX_LINE_BYTES, MAX_REPORT_LINES};

use super::report;

fn texts(report: &Report) -> Vec<&str> {
    report
        .lines()
        .iter()
        .map(|line| line.text.as_str())
        .collect()
}

/// Whether some line of `report` holds `needle`.
fn says(report: &Report, needle: &str) -> bool {
    report.lines().iter().any(|line| line.text.contains(needle))
}

fn rxe() -> Vec<u8> {
    crate::rxe::tests::valid_image()
}

/// A driver image binding a device-tree node and a PCI function, asking for
/// IRQ binding, and running the test program.
fn driver() -> Vec<u8> {
    let keys = [
        DriverBindKey::new(
            10,
            HwMatchKey::compatible(b"brcm,bcm2711-hdmi0").expect("a short name"),
        ),
        DriverBindKey::new(5, HwMatchKey::pci(0x8086, 0x1234, 0x03_0000)),
    ];
    let manifest = DriverManifest {
        magic: DRIVER_MANIFEST_MAGIC,
        abi_version: tairix_abi::ABI_VERSION_CURRENT,
        kind: DriverKind::InKernel,
        bind_key_count: 2,
        capability_count: 1,
        syscall_table_hash: [0x5e; 32],
        signer_pubkey: [0x8a; 32],
        signature: [0x77; 64],
    };
    let mut image = manifest.to_le_bytes().to_vec();
    image.extend_from_slice(&CapabilityId::IRQ_BIND.as_u16().to_le_bytes());
    for key in keys {
        image.extend_from_slice(&key.to_le_bytes());
    }
    image.extend_from_slice(&rxe());
    image
}

/// An application's `AppInfo` claiming PNG and asking for raw sockets.
fn appinfo() -> Vec<u8> {
    let mut header = manifest_header("org.tairix.paint", "Paint");
    header.flags = APPINFO_FLAG_MULTI_INSTANCE | APPINFO_FLAG_DOCUMENT_WRITE;
    header.capability_count = 1;
    header.mime_count = 1;
    header.kind = ProgramKind::Application.to_wire();
    header.library = LibraryCategory::to_wire(Some(LibraryCategory::Graphics));
    let mut bytes = header.to_le_bytes().to_vec();
    bytes.extend_from_slice(&CapabilityId::NET_RAW.as_u16().to_le_bytes());
    let mut mime = [0u8; MIME_ENTRY_LEN];
    mime[0] = 9;
    mime[1..10].copy_from_slice(b"image/png");
    bytes.extend_from_slice(&mime);
    bytes
}

/// One file of every format the report reads.
fn every_format() -> [(&'static str, Vec<u8>); 6] {
    [
        ("rxe", rxe()),
        ("driver", driver()),
        (
            "manifest",
            crate::rxe::tests::manifest_with(&[CapabilityId::FS_MOUNT]),
        ),
        ("appinfo", appinfo()),
        ("elf", crate::elf::tests::fixture()),
        ("wasm", crate::wasm::tests::fixture()),
    ]
}

fn within_bounds(report: &Report) {
    assert!(report.lines().len() <= MAX_REPORT_LINES);
    for line in report.lines() {
        assert!(line.text.len() <= MAX_LINE_BYTES, "{:?}", line.text);
        assert!(!line.text.chars().any(char::is_control), "{:?}", line.text);
    }
}

#[test]
fn a_program_lists_its_header_segments_libraries_and_their_bytes() {
    let report = report(&rxe()).expect("an rxe is reported");
    let lines = texts(&report);
    assert!(
        lines[0].starts_with("TAIRiX program (rxe), ABI 1, position-independent"),
        "{lines:?}"
    );
    assert!(report.lines()[0].heading);
    assert!(says(&report, "00000000  header"), "{lines:?}");
    assert!(says(&report, "entry 0x1010"), "{lines:?}");
    assert!(
        says(&report, &alloc::format!("CFI tag {}", "a5".repeat(32))),
        "{lines:?}"
    );
    assert!(says(&report, "00000038  segments"), "{lines:?}");
    assert!(
        says(&report, "r-x at 0x1000: 0x40 file bytes at 0x0"),
        "{lines:?}"
    );
    assert!(says(&report, "rw- at 0x2000"), "{lines:?}");
    assert!(says(&report, "1 needed"), "{lines:?}");
    assert!(
        says(&report, "/System/Libraries/libexample.so"),
        "{lines:?}"
    );
    assert!(says(&report, "segment"), "{lines:?}");
    assert!(says(&report, "0, code"), "{lines:?}");
    assert!(!says(&report, "Stopped"), "{lines:?}");
}

#[test]
fn a_driver_lists_its_manifest_its_bindings_and_the_program_it_runs() {
    let image = driver();
    let report = report(&image).expect("a driver is reported");
    let lines = texts(&report);
    assert_eq!(
        lines[0], "TAIRiX driver, ABI 1, runs in the kernel",
        "{lines:?}"
    );
    assert!(says(&report, "signed driver manifest"), "{lines:?}");
    assert!(
        says(&report, &alloc::format!("signer {}", "8a".repeat(32))),
        "{lines:?}"
    );
    assert!(says(&report, "(not checked here)"), "{lines:?}");
    assert!(says(&report, "CAP_IRQ_BIND"), "{lines:?}");
    assert!(says(&report, "2 keys"), "{lines:?}");
    assert!(
        says(&report, "priority 10: compatible \"brcm,bcm2711-hdmi0\""),
        "{lines:?}"
    );
    assert!(
        says(&report, "priority 5: PCI 8086:1234, class 030000"),
        "{lines:?}"
    );
    let program = DriverManifest::WIRE_LEN + 2 + 2 * DriverBindKey::WIRE_LEN;
    assert!(
        says(&report, &alloc::format!("{program:08x}  program")),
        "{lines:?}"
    );
    assert!(
        says(&report, &alloc::format!("{program:08x}  header")),
        "the program's own parts sit at their place in the file: {lines:?}"
    );
    assert!(
        report
            .lines()
            .iter()
            .filter(|line| line.heading)
            .any(|line| line.text.starts_with("TAIRiX program (rxe)")),
        "{lines:?}"
    );
}

#[test]
fn a_manifest_and_an_appinfo_list_what_they_request_and_claim() {
    let manifest = crate::rxe::tests::manifest_with(&[CapabilityId::FS_MOUNT]);
    let report = report(&manifest).expect("a manifest is reported");
    assert_eq!(texts(&report)[0], "TAIRiX program manifest, ABI 1");
    assert!(says(&report, "CAP_FS_MOUNT"));
    assert!(says(
        &report,
        &alloc::format!("syscall table {}", "11".repeat(32))
    ));

    let report = report_of(&appinfo());
    let lines = texts(&report);
    assert_eq!(
        lines[0], "Bundle manifest (AppInfo) of an application, ABI 1",
        "{lines:?}"
    );
    for needle in [
        "org.tairix.paint",
        "name Paint",
        "version 1.0.0",
        "listed in the program library under Graphics",
        "runs any number of instances, with a slot on the icon bar",
        "may write the documents it opens",
        "signs its own builds",
        "CAP_NET_RAW",
        "1 claimed",
        "image/png",
    ] {
        assert!(says(&report, needle), "{needle}: {lines:?}");
    }
}

fn report_of(bytes: &[u8]) -> Report {
    report(bytes).expect("a recognised format")
}

#[test]
fn an_elf_and_a_module_list_their_tables() {
    let report = report_of(&crate::elf::tests::fixture());
    let lines = texts(&report);
    assert_eq!(lines[0], "ELF64 executable for x86_64", "{lines:?}");
    assert!(says(&report, "1 segment"), "{lines:?}");
    assert!(says(&report, "LOAD r-x at"), "{lines:?}");
    assert!(says(&report, "5 sections"), "{lines:?}");
    assert!(says(&report, ".text PROGBITS a-x"), "{lines:?}");
    assert!(says(&report, ".symtab SYMTAB"), "{lines:?}");
    assert!(says(&report, "2 symbols"), "{lines:?}");

    let report = report_of(&crate::wasm::tests::fixture());
    let lines = texts(&report);
    assert_eq!(lines[0], "WebAssembly module, 4 sections", "{lines:?}");
    assert!(says(&report, "custom"), "{lines:?}");
    assert!(says(&report, "named name"), "{lines:?}");
    assert!(says(&report, "1 entry"), "{lines:?}");
    assert!(says(&report, "2 entries"), "{lines:?}");
}

#[test]
fn a_file_no_format_recognises_has_no_report() {
    assert_eq!(report(b"plain text"), None);
    assert_eq!(report(&[]), None);
}

#[test]
fn a_cut_driver_stops_where_its_parts_run_out() {
    let image = driver();
    let cut = &image[..=DriverManifest::WIRE_LEN];
    let report = report_of(cut);
    let last = report.lines().last().expect("a line");
    assert!(
        last.heading && last.text.starts_with("Stopped at 00000000"),
        "{last:?}"
    );

    let mut damaged = image.clone();
    let program = DriverManifest::WIRE_LEN + 2 + 2 * DriverBindKey::WIRE_LEN;
    damaged[program] ^= 0xFF;
    let report = report_of(&damaged);
    assert!(
        says(&report, &alloc::format!("Stopped at {program:08x}")),
        "{:?}",
        texts(&report)
    );
}

#[test]
fn every_cut_or_damaged_file_is_reported_within_bounds() {
    for (name, file) in every_format() {
        within_bounds(&report_of(&file));
        for len in 4..file.len() {
            if let Some(report) = report(&file[..len]) {
                within_bounds(&report);
            }
        }
        for at in 0..file.len() {
            let mut damaged = file.clone();
            damaged[at] ^= 0xA5;
            if let Some(report) = report(&damaged) {
                within_bounds(&report);
            } else {
                assert!(at < 6, "{name}: only the magic decides the format");
            }
        }
    }
}

#[test]
fn a_manifest_cut_short_or_naming_an_unknown_capability_stops_with_why() {
    let mut bytes = crate::rxe::tests::manifest_with(&[CapabilityId::FS_MOUNT]);
    let cut = report_of(&bytes[..bytes.len() - 1]);
    assert!(
        says(&cut, "the file ends before the capabilities it lists"),
        "{:?}",
        texts(&cut)
    );
    let body = bytes.len() - 2;
    bytes[body..].copy_from_slice(&u16::MAX.to_le_bytes());
    let unknown = report_of(&bytes);
    let last = unknown.lines().last().expect("a line");
    assert!(
        last.heading && last.text.starts_with("Stopped at 00000090"),
        "{last:?}"
    );
}
