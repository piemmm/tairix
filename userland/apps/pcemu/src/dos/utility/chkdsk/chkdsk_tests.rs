//! `CHKDSK`: its command line and report, then each check run at the
//! shell's prompt on a diskette built here and damaged as a test needs.

use super::*;
use crate::dos::harness::{diskette, diskette_bytes, screen, typed};
use crate::dos::volume;
use crate::machine::Machine;
use alloc::string::String;
use alloc::vec::Vec;
use tairix_fat::boot::{Bpb, Kind};
use tairix_fat::dir::{attr, ShortEntry};
use tairix_fat::stamp::Stamp;
use tairix_fat::table;

#[test]
fn chkdsk_reads_its_drive_file_and_switches() {
    let got = |tail: &'static [u8]| request(tail);
    assert_eq!(
        got(b""),
        Ok(Request {
            drive: None,
            spec: None,
            switches: 0
        })
    );
    assert_eq!(
        got(b" a: /f /V"),
        Ok(Request {
            drive: Some(0),
            spec: None,
            switches: switch::FIX | switch::VERBOSE
        })
    );
    assert_eq!(
        got(b" c:\\dos\\*.*"),
        Ok(Request {
            drive: Some(2),
            spec: Some(b"c:\\dos\\*.*"),
            switches: switch::NAMED
        })
    );
    assert_eq!(
        got(b" b: *.exe/f"),
        Ok(Request {
            drive: Some(1),
            spec: Some(b"*.exe"),
            switches: switch::NAMED | switch::FIX
        })
    );
    assert_eq!(
        got(b" *.*"),
        Ok(Request {
            drive: None,
            spec: Some(b"*.*"),
            switches: switch::NAMED
        })
    );
    assert_eq!(got(b" /f /f"), Err(Refusal::Switch(b"/f")), "a switch once");
    assert_eq!(got(b" /x"), Err(Refusal::Switch(b"/x")));
    assert_eq!(got(b" a.txt b.txt"), Err(Refusal::TooMany(b"b.txt")));
    assert_eq!(got(b" a: b: c:"), Err(Refusal::TooMany(b"c:")));
}

#[test]
fn chkdsk_checks_only_a_bpb_dos_s_own_does() {
    let bpb = |sector: u16, cluster: u8, fats: u8, root: u16, fat: u16| {
        let mut out = [0u8; 13];
        out[0..2].copy_from_slice(&sector.to_le_bytes());
        out[2] = cluster;
        out[5] = fats;
        out[6..8].copy_from_slice(&root.to_le_bytes());
        out[11..13].copy_from_slice(&fat.to_le_bytes());
        out
    };
    assert!(checkable(&bpb(512, 2, 2, 112, 2)));
    assert!(checkable(&bpb(128, 128, 1, 512, 9)));
    assert!(
        !checkable(&bpb(1024, 2, 2, 112, 2)),
        "a sector DOS 4.0 never wrote"
    );
    assert!(!checkable(&bpb(512, 3, 2, 112, 2)));
    assert!(!checkable(&bpb(512, 2, 3, 112, 2)));
    assert!(!checkable(&bpb(512, 2, 2, 513, 2)));
    assert!(!checkable(&bpb(512, 2, 2, 112, 0)));
}

#[test]
fn the_report_gives_each_count_in_ten_columns_and_leaves_out_none_there_are_not() {
    let mut counts = [0u32; count::LEN as usize];
    counts[usize::from(count::HIDDEN)] = 3;
    counts[usize::from(count::HIDDEN_CLUSTERS)] = 40;
    counts[usize::from(count::FILES)] = 12;
    counts[usize::from(count::FILE_CLUSTERS)] = 100;
    counts[usize::from(count::LOST)] = 5;
    counts[usize::from(count::CHAINS)] = 2;
    let totals = Totals {
        disk_bytes: 362_496,
        cluster_bytes: 1024,
        clusters: 354,
        counts,
        recovered: false,
        memory: 655_360,
        free_memory: 600_000,
    };
    let report = String::from_utf8(totals.report()).unwrap();
    let expected = "    362496 bytes total disk space\r\n     40960 bytes in 3 hidden files\r\n    102400 bytes in 12 user files\r\n      5120 bytes would be in 2 recovered files\r\n    214016 bytes available on disk\r\n\r\n      1024 bytes in each allocation unit\r\n       354 total allocation units on disk\r\n       209 available allocation units on disk\r\n\r\n    655360 total bytes memory\r\n    600000 bytes free\r\n";
    assert_eq!(report, expected);
    assert!(String::from_utf8(
        Totals {
            recovered: true,
            ..totals
        }
        .report()
    )
    .unwrap()
    .contains("      5120 bytes in 2 recovered files\r\n"));
}

/// A diskette to check in drive B:, holding `files`, every entry stamped
/// 17 June 1988 at 1:05 p.m., to be damaged before it goes in.
struct Disk {
    image: Vec<u8>,
    bpb: Bpb,
}

/// Three clusters' worth.
const FILLER: &[u8] = &[b'x'; 2500];

impl Disk {
    fn new(files: &[volume::File<'_>], label: Option<[u8; 11]>) -> Self {
        let civil = tairix_abi::time::CivilTime {
            year: 1988,
            month: 6,
            day: 17,
            hour: 13,
            minute: 5,
            second: 0,
        };
        let stamp = tairix_abi::time::DosTime::of_civil(&civil).unwrap();
        let stamp = Stamp {
            date: stamp.date,
            time: stamp.time,
            centis: 0,
        };
        let bpb = diskette();
        Self {
            image: volume::volume(&bpb, label, files, stamp).unwrap(),
            bpb,
        }
    }

    fn fat(&self) -> &[u8] {
        let start = usize::try_from(self.bpb.fat_start()).unwrap();
        &self.image[start..start + usize::try_from(self.bpb.fat_bytes()).unwrap()]
    }

    fn entry(&self, cluster: u16) -> u16 {
        u16::try_from(table::get(Kind::Fat12, self.fat(), u32::from(cluster)).unwrap()).unwrap()
    }

    /// Sets a cluster's entry in both FATs.
    fn link(&mut self, cluster: u16, value: u16) -> &mut Self {
        let (start, len) = (
            usize::try_from(self.bpb.fat_start()).unwrap(),
            usize::try_from(self.bpb.fat_bytes()).unwrap(),
        );
        for copy in 0..usize::from(self.bpb.fats) {
            let fat = &mut self.image[start + copy * len..start + (copy + 1) * len];
            assert!(table::set(
                Kind::Fat12,
                fat,
                u32::from(cluster),
                u32::from(value)
            ));
        }
        self
    }

    /// The highest cluster any entry's chain holds: the free ones lie past it.
    fn last_used(&self) -> u16 {
        (2..=u16::try_from(self.bpb.max_cluster()).unwrap())
            .rev()
            .find(|&c| self.entry(c) != 0)
            .unwrap()
    }

    /// Where the entry named `name` lies in the directory at byte `at`.
    fn slot(&self, at: usize, name: &[u8; 11]) -> usize {
        (0..usize::from(self.bpb.root_entries))
            .map(|n| at + n * 32)
            .find(|&slot| &self.image[slot..slot + 11] == name)
            .unwrap()
    }

    fn root_slot(&self, name: &[u8; 11]) -> usize {
        self.slot(usize::try_from(self.bpb.root_start()).unwrap(), name)
    }

    fn read(&self, slot: usize) -> ShortEntry {
        ShortEntry::decode(self.image[slot..slot + 32].try_into().unwrap())
    }

    fn write(&mut self, slot: usize, entry: &ShortEntry) -> &mut Self {
        self.image[slot..slot + 32].copy_from_slice(&entry.encode());
        self
    }

    /// The root entry `name`, changed by `change`.
    fn change(&mut self, name: &[u8; 11], change: impl FnOnce(&mut ShortEntry)) -> &mut Self {
        let slot = self.root_slot(name);
        let mut entry = self.read(slot);
        change(&mut entry);
        self.write(slot, &entry)
    }

    /// The first cluster of a subdirectory of the root.
    fn directory(&self, name: &[u8; 11]) -> usize {
        let cluster = self.read(self.root_slot(name)).cluster;
        usize::try_from(self.bpb.cluster_start(cluster).unwrap()).unwrap()
    }

    /// A machine at the prompt of its system diskette in A:, this one in B:.
    fn boot(&self) -> Machine {
        let mut m = crate::dos::utility::tests::with_utilities(&[]);
        m.board_mut()
            .insert_floppy(1, crate::dos::harness::floppy(self.image.clone()))
            .unwrap();
        m
    }
}

/// What a run of `line` leaves on the screen, the screen cleared first.
fn run(m: &mut Machine, line: &str) -> String {
    crate::dos::utility::tests::run(m, line)
}

/// The diskette in B: as the drive holds it now, taken out.
fn taken_out(m: &mut Machine) -> Disk {
    let image = diskette_bytes(&mut m.board_mut().eject_floppy(1).unwrap());
    let bpb = Bpb::parse(image[..512].try_into().unwrap()).unwrap();
    Disk { image, bpb }
}

/// Whether the last command left an error level of `code` or more, as
/// `IF ERRORLEVEL` reads it.
fn at_least(m: &mut Machine, code: u8) -> bool {
    let line = alloc::format!("if errorlevel {code} echo AT LEAST {code}");
    run(m, &line).contains(&alloc::format!("\nAT LEAST {code}\n"))
}

#[test]
fn chkdsk_reports_a_sound_diskette_with_its_label_and_hidden_files() {
    let files = [
        volume::File {
            path: "IO.SYS",
            bytes: FILLER,
            attributes: attr::HIDDEN | attr::SYSTEM,
        },
        volume::File {
            path: "DOS\\NOTES.TXT",
            bytes: FILLER,
            attributes: 0,
        },
    ];
    let disk = Disk::new(&files, Some(*b"TAIRIX DOS "));
    let mut m = disk.boot();
    let text = run(&mut m, "chkdsk b:");
    let expected = "A:\\>chkdsk b:\n\nVolume TAIRIX DOS  created 06-17-1988 1:05p\nVolume Serial Number is 1986-0310\n\n    362496 bytes total disk space\n      3072 bytes in 2 hidden files\n      1024 bytes in 1 directories\n      3072 bytes in 1 user files\n    355328 bytes available on disk\n\n      1024 bytes in each allocation unit\n       354 total allocation units on disk\n       347 available allocation units on disk\n\n    655360 total bytes memory\n";
    assert!(
        text.contains(expected),
        "the label is a hidden file of no clusters, as DOS counts it:\n{text}"
    );
    assert!(!at_least(&mut m, 1));
    let text = run(&mut m, "chkdsk b: /v");
    assert!(text.contains("\nDirectory B:\\\n        B:\\TAIRIX D.OS\n        B:\\IO.SYS\nDirectory B:\\DOS\n        B:\\DOS\\NOTES.TXT\n\n"), "{text}");
    assert!(
        run(&mut m, "cd").contains("\nA:\\\n"),
        "the current drive is put back"
    );
}

/// A diskette holding two files, and two chains no entry reaches: three
/// clusters and one.
fn with_lost_chains() -> (Disk, u16) {
    let files = [
        volume::File {
            path: "KEEP.TXT",
            bytes: FILLER,
            attributes: 0,
        },
        volume::File {
            path: "ALSO.TXT",
            bytes: b"a",
            attributes: 0,
        },
    ];
    let mut disk = Disk::new(&files, None);
    let free = disk.last_used() + 10;
    disk.link(free, free + 1)
        .link(free + 1, free + 2)
        .link(free + 2, 0xFFF)
        .link(free + 5, 0xFFF);
    (disk, free)
}

/// Types `line`, then answers the question it stops at with `answer`.
fn answered(m: &mut Machine, line: &str, answer: &str) -> String {
    typed(m, "cls");
    typed(m, line);
    typed(m, answer);
    screen(m)
}

#[test]
fn lost_chains_are_counted_and_without_f_nothing_is_written() {
    let (disk, free) = with_lost_chains();
    let mut m = disk.boot();
    let text = answered(&mut m, "chkdsk b:", "y");
    let expected = "\nErrors found, F parameter not specified\nCorrections will not be written to disk\n\n\n   4 lost allocation units found in 2 chains.\nConvert lost chains to files (Y/N)?y\n\n    362496 bytes total disk space\n      4096 bytes in 2 user files\n      4096 bytes would be in 2 recovered files\n    354304 bytes available on disk\n";
    assert!(text.contains(expected), "{text}");
    let after = taken_out(&mut m);
    assert!(after.image == disk.image, "nothing reached the disk");
    assert_eq!(after.entry(free), free + 1);
}

#[test]
fn lost_chains_become_files_in_the_root_with_f() {
    let (disk, free) = with_lost_chains();
    let mut m = disk.boot();
    let text = answered(&mut m, "chkdsk b: /f", "y");
    assert!(text.contains("\n   4 lost allocation units found in 2 chains.\nConvert lost chains to files (Y/N)?y\n\n"), "{text}");
    assert!(!text.contains("Errors found"), "{text}");
    assert!(
        text.contains("      4096 bytes in 2 recovered files\n"),
        "{text}"
    );
    let text = run(&mut m, "dir b:file*.chk");
    assert!(
        text.contains("FILE0000 CHK      3072") && text.contains("FILE0001 CHK      1024"),
        "{text}"
    );
    let after = taken_out(&mut m);
    assert_eq!(
        after.read(after.root_slot(b"FILE0000CHK")).cluster,
        u32::from(free)
    );
    assert_eq!(
        after.read(after.root_slot(b"FILE0001CHK")).cluster,
        u32::from(free + 5)
    );
    let mut m = after.boot();
    let text = run(&mut m, "chkdsk b:");
    assert!(
        !text.contains("lost") && text.contains("      8192 bytes in 4 user files\n"),
        "a second run finds them files:\n{text}"
    );
}

#[test]
fn lost_chains_are_freed_with_f_when_the_answer_is_no() {
    let (disk, free) = with_lost_chains();
    let mut m = disk.boot();
    let text = answered(&mut m, "chkdsk b: /f", "n");
    assert!(
        text.contains("Convert lost chains to files (Y/N)?n\n      4096 bytes disk space freed\n"),
        "{text}"
    );
    assert!(
        text.contains("    358400 bytes available on disk\n"),
        "{text}"
    );
    let after = taken_out(&mut m);
    assert!([free, free + 1, free + 2, free + 5]
        .iter()
        .all(|&c| after.entry(c) == 0));
    let text = answered(&mut with_lost_chains().0.boot(), "chkdsk b:", "n");
    assert!(
        text.contains("      4096 bytes disk space would be freed\n")
            && text.contains("    354304 bytes available on disk\n"),
        "{text}"
    );
}

#[test]
fn files_sharing_a_cluster_are_named_on_a_second_walk() {
    let files = [
        volume::File {
            path: "A.TXT",
            bytes: FILLER,
            attributes: 0,
        },
        volume::File {
            path: "B.TXT",
            bytes: b"b",
            attributes: 0,
        },
    ];
    let mut disk = Disk::new(&files, None);
    let (a, b) = (
        disk.read(disk.root_slot(b"A       TXT")).cluster,
        disk.read(disk.root_slot(b"B       TXT")).cluster,
    );
    let shared = u16::try_from(a).unwrap() + 1;
    disk.link(u16::try_from(b).unwrap(), shared);
    let mut m = disk.boot();
    let text = run(&mut m, "chkdsk b:");
    let crossed = alloc::format!("\n\nB:\\A.TXT\n   Is cross linked on allocation unit {shared}\nB:\\B.TXT\n   Is cross linked on allocation unit {shared}\n\n    362496 bytes total disk space\n");
    assert!(text.contains(&crossed), "{text}");
    assert!(
        !text.contains("Errors found"),
        "a crossing is said, never fixed:\n{text}"
    );
    assert!(
        taken_out(&mut m).image == disk.image,
        "nothing reached the disk"
    );
}

#[test]
fn a_size_its_chain_cannot_hold_is_adjusted() {
    let files = [
        volume::File {
            path: "SHORT.TXT",
            bytes: b"c",
            attributes: 0,
        },
        volume::File {
            path: "LONG.TXT",
            bytes: FILLER,
            attributes: 0,
        },
    ];
    let mut disk = Disk::new(&files, None);
    disk.change(b"SHORT   TXT", |entry| entry.size = 5000)
        .change(b"LONG    TXT", |entry| entry.size = 100);
    let mut m = disk.boot();
    let text = run(&mut m, "chkdsk b:");
    let said = "\nErrors found, F parameter not specified\nCorrections will not be written to disk\n\nB:\\SHORT.TXT\n   Allocation error, size adjusted\nB:\\LONG.TXT\n   Allocation error, size adjusted\n\n";
    assert!(text.contains(said), "{text}");
    let text = run(&mut m, "chkdsk b: /f");
    assert!(
        text.contains("\nB:\\SHORT.TXT\n   Allocation error, size adjusted\n")
            && !text.contains("Errors found"),
        "{text}"
    );
    let after = taken_out(&mut m);
    assert_eq!(
        after.read(after.root_slot(b"SHORT   TXT")).size,
        1024,
        "the size is its chain's"
    );
    assert_eq!(after.read(after.root_slot(b"LONG    TXT")).size, 3072);
}

#[test]
fn a_chain_linking_to_no_cluster_is_truncated_and_a_bad_first_cluster_empties_its_entry() {
    let files = [
        volume::File {
            path: "BROKEN.TXT",
            bytes: FILLER,
            attributes: 0,
        },
        volume::File {
            path: "GONE.TXT",
            bytes: b"f",
            attributes: 0,
        },
    ];
    let mut disk = Disk::new(&files, None);
    let broken = u16::try_from(disk.read(disk.root_slot(b"BROKEN  TXT")).cluster).unwrap();
    let gone = u16::try_from(disk.read(disk.root_slot(b"GONE    TXT")).cluster).unwrap();
    disk.link(broken + 1, 0)
        .change(b"GONE    TXT", |entry| entry.cluster = 2000);
    let mut m = disk.boot();
    let text = answered(&mut m, "chkdsk b: /f", "n");
    let said = "\nB:\\BROKEN.TXT\n   Has invalid allocation unit, file truncated\nB:\\GONE.TXT\n   First allocation unit is invalid, entry truncated\n\n   2 lost allocation units found in 2 chains.\n";
    assert!(
        text.contains(said),
        "the cut-off cluster and the one no entry names are lost:\n{text}"
    );
    let after = taken_out(&mut m);
    assert_eq!(
        after.entry(broken + 1),
        0xFFF,
        "the chain ends where its link went wrong"
    );
    assert_eq!(after.entry(broken + 2), 0, "the cluster past it freed");
    assert_eq!(after.entry(gone), 0);
    let entry = after.read(after.root_slot(b"GONE    TXT"));
    assert_eq!((entry.cluster, entry.size), (0, 0));
}

fn with_subdirectory() -> Disk {
    Disk::new(
        &[volume::File {
            path: "DOS\\NOTES.TXT",
            bytes: FILLER,
            attributes: 0,
        }],
        None,
    )
}

#[test]
fn a_directorys_dot_is_checked_and_put_right() {
    let mut disk = with_subdirectory();
    let dot = disk.directory(b"DOS        ");
    let mut entry = disk.read(dot);
    entry.cluster = 99;
    entry.size = 7;
    disk.write(dot, &entry);
    let mut m = disk.boot();
    let text = run(&mut m, "chkdsk b:");
    assert!(
        text.contains("\nB:\\DOS\n   Invalid sub-directory entry\n\n"),
        "without /V, said once:\n{text}"
    );
    let text = run(&mut m, "chkdsk b: /v");
    assert!(
        text.contains(
            "\nB:\\DOS\\.\n   Entry has a bad link\nB:\\DOS\\.\n   Entry has a bad size\n"
        ),
        "{text}"
    );
    run(&mut m, "chkdsk b: /f");
    let after = taken_out(&mut m);
    let entry = after.read(after.directory(b"DOS        "));
    assert_eq!(
        (entry.name, entry.cluster, entry.size),
        (
            *b".          ",
            disk.read(disk.root_slot(b"DOS        ")).cluster,
            0
        )
    );
}

#[test]
fn a_missing_dot_is_made_again_in_its_free_slot() {
    let mut disk = with_subdirectory();
    let dot = disk.directory(b"DOS        ");
    disk.image[dot] = 0xE5;
    let mut m = disk.boot();
    let text = run(&mut m, "chkdsk b: /v");
    assert!(text.contains("\nB:\\DOS\\.\n   Does not exist\n"), "{text}");
    let text = run(&mut m, "chkdsk b: /f");
    assert!(
        text.contains("\nB:\\DOS\n   Invalid sub-directory entry\n"),
        "{text}"
    );
    let after = taken_out(&mut m);
    let entry = after.read(after.directory(b"DOS        "));
    assert_eq!(
        (entry.name, entry.attributes, entry.cluster),
        (
            *b".          ",
            attr::DIRECTORY,
            disk.read(disk.root_slot(b"DOS        ")).cluster
        )
    );
    assert!(
        !run(&mut after.boot(), "chkdsk b:").contains("Invalid"),
        "a second run finds it sound"
    );
}

#[test]
fn a_subdirectory_entry_naming_no_cluster_is_made_a_file_when_asked() {
    let mut disk = with_subdirectory();
    disk.change(b"DOS        ", |entry| entry.cluster = 0);
    let mut m = disk.boot();
    typed(&mut m, "cls");
    typed(&mut m, "chkdsk b: /f");
    assert!(
        screen(&m).contains(
            "\nB:\\DOS\n   Invalid sub-directory entry\nConvert directory to file (Y/N)?"
        ),
        "{}",
        screen(&m)
    );
    typed(&mut m, "y");
    typed(&mut m, "n");
    let text = screen(&m);
    assert!(
        text.contains(
            "Convert directory to file (Y/N)?y\n\n   4 lost allocation units found in 2 chains.\n"
        ),
        "its own cluster and its file's are lost:\n{text}"
    );
    let after = taken_out(&mut m);
    let entry = after.read(after.root_slot(b"DOS        "));
    assert_eq!((entry.attributes & attr::DIRECTORY, entry.size), (0, 0));
}

#[test]
fn a_named_files_pieces_are_counted_last() {
    let files = [
        volume::File {
            path: "FRAG.TXT",
            bytes: FILLER,
            attributes: 0,
        },
        volume::File {
            path: "WHOLE.TXT",
            bytes: FILLER,
            attributes: 0,
        },
        volume::File {
            path: "DOS\\INSIDE.TXT",
            bytes: b"i",
            attributes: 0,
        },
    ];
    let mut disk = Disk::new(&files, None);
    let first = u16::try_from(disk.read(disk.root_slot(b"FRAG    TXT")).cluster).unwrap();
    disk.link(first, first + 2)
        .link(first + 2, first + 1)
        .link(first + 1, 0xFFF);
    let mut m = disk.boot();
    let text = run(&mut m, "chkdsk b:*.txt");
    assert!(
        text.contains(" bytes free\n\nB:\\FRAG.TXT Contains 3 non-contiguous blocks\n"),
        "{text}"
    );
    assert!(
        !text.contains("WHOLE") && !text.contains("contiguous\n"),
        "{text}"
    );
    let text = run(&mut m, "chkdsk b:\\dos\\*.*");
    assert!(
        text.contains("bytes free\n\nAll specified file(s) are contiguous\n"),
        "{text}"
    );
    assert!(
        run(&mut m, "cd b:").contains("\nB:\\\n"),
        "the directory searched is left again"
    );
    assert!(
        run(&mut m, "chkdsk b:none.txt").contains("bytes free\n\nFile not found\n"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "chkdsk b:\\nowhere\\x.txt").contains("bytes free\n\nPath not found\n"),
        "{}",
        screen(&m)
    );
}

#[test]
fn chkdsk_says_what_it_cannot_check() {
    let mut m = Disk::new(&[], None).boot();
    assert!(
        run(&mut m, "chkdsk /x").contains("\nInvalid switch - /X\n"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "chkdsk a b").contains("\nToo many parameters - B\n"),
        "{}",
        screen(&m)
    );
    assert!(
        run(&mut m, "chkdsk q:").contains("\nInvalid drive specification\n"),
        "{}",
        screen(&m)
    );
    assert!(
        !at_least(&mut m, 1),
        "as DOS 4.0's, a refusal leaves error level 0"
    );
}

#[test]
fn ctrl_c_at_a_question_ends_chkdsk_writing_nothing() {
    let (disk, _) = with_lost_chains();
    let mut m = disk.boot();
    typed(&mut m, "chkdsk b: /f");
    assert!(
        screen(&m).contains("Convert lost chains to files (Y/N)?"),
        "{}",
        screen(&m)
    );
    crate::dos::harness::control(&mut m, crate::dos::harness::C);
    crate::dos::harness::settle(&mut m);
    assert!(
        !screen(&m).contains("bytes total disk space"),
        "{}",
        screen(&m)
    );
    assert!(at_least(&mut m, 255));
    assert!(
        run(&mut m, "cd").contains("\nA:\\\n"),
        "the current drive is put back"
    );
    assert!(
        taken_out(&mut m).image == disk.image,
        "nothing reached the disk"
    );
}

#[test]
fn an_answer_the_input_cannot_give_ends_chkdsk() {
    let (disk, _) = with_lost_chains();
    let mut m = disk.boot();
    let text = run(&mut m, "chkdsk b: /f <nul");
    assert!(
        text.contains("Convert lost chains to files (Y/N)?\n")
            && !text.contains("bytes total disk space"),
        "{text}"
    );
    assert!(at_least(&mut m, 255));
    assert!(
        taken_out(&mut m).image == disk.image,
        "nothing reached the disk"
    );
}

#[test]
fn an_unreadable_fat_stops_it_and_an_unreadable_directory_is_failed_without_asking() {
    let disk = with_subdirectory();
    let fat = disk.bpb.fat_start() / 512;
    let copy = disk.bpb.fat_bytes() / 512;
    let mut m = crate::dos::utility::tests::with_utilities(&[]);
    // Function 36h counts the free clusters through DOS's buffers from the
    // first copy, read once; CHKDSK's own reads of either copy then fail.
    let unreadable = move |sector, before| {
        (fat + copy..fat + 2 * copy).contains(&sector)
            || ((fat..fat + copy).contains(&sector) && before > 0)
    };
    let floppy = crate::dos::harness::unreliable_floppy(disk.image.clone(), unreadable);
    m.board_mut().insert_floppy(1, floppy).unwrap();
    let text = run(&mut m, "chkdsk b:");
    assert!(
        text.contains("\n   Processing cannot continue    File allocation table bad B.\n"),
        "{text}"
    );
    assert!(at_least(&mut m, 255));
    let directory = disk
        .bpb
        .cluster_start(disk.read(disk.root_slot(b"DOS        ")).cluster)
        .unwrap()
        / 512;
    let floppy = crate::dos::harness::unreliable_floppy(disk.image.clone(), move |sector, _| {
        sector == directory
    });
    m.board_mut().eject_floppy(1);
    m.board_mut().insert_floppy(1, floppy).unwrap();
    typed(&mut m, "cls");
    typed(&mut m, "chkdsk b:");
    let text = screen(&m);
    assert!(!text.contains("Abort"), "{text}");
    assert!(
        text.contains(
            "\nB:\\DOS\n   Invalid sub-directory entry\nConvert directory to file (Y/N)?"
        ),
        "{text}"
    );
}

#[test]
fn a_protected_diskette_is_told_of_each_write_it_refuses_and_none_it_need_not_take() {
    let (disk, _) = with_lost_chains();
    let mut m = crate::dos::utility::tests::with_utilities(&[]);
    m.board_mut()
        .insert_floppy(1, crate::dos::harness::protected_floppy(disk.image.clone()))
        .unwrap();
    let text = answered(&mut m, "chkdsk b: /f", "y");
    assert!(
        text.contains("Convert lost chains to files (Y/N)?y\nWrite fault error\n"),
        "{text}"
    );
    assert!(
        !text.contains("Disk error writing FAT"),
        "sound chains made files leave the FAT as it was:\n{text}"
    );
    let text = answered(&mut m, "chkdsk b: /f", "n");
    assert!(text.contains("   Disk error writing FAT 1\n   Disk error writing FAT 2\n   Processing cannot continue .\n"), "{text}");
    assert!(at_least(&mut m, 255));
}

#[test]
fn an_empty_drive_is_asked_about_and_an_abort_puts_the_drive_back() {
    let (disk, _) = with_lost_chains();
    let mut m = disk.boot();
    typed(&mut m, "cls");
    typed(&mut m, "chkdsk b:*.txt");
    assert!(
        screen(&m).contains("Convert lost chains to files (Y/N)?"),
        "{}",
        screen(&m)
    );
    m.board_mut().eject_floppy(1);
    typed(&mut m, "n");
    let text = screen(&m);
    assert!(
        text.contains("bytes free\n\n\nNot ready reading drive B\nAbort, Retry, Ignore, Fail?"),
        "the named file's search asks through the shell's handler:\n{text}"
    );
    typed(&mut m, "a");
    assert!(run(&mut m, "cd").contains("\nA:\\\n"), "{}", screen(&m));
}
