//! `COMMAND.COM` booted as the primary shell from a diskette built here,
//! typed at and read back from the screen.

use crate::chips::keyboard::Scan;
use crate::cpu::Bus;
use crate::dos::data::{sda, SDA};
use crate::dos::harness::{
    blocks_of, com, control, diskette_bytes, kernel16, prompted, screen, sector_of, settle,
    shell_machine, type_ahead, typed, unreliable_floppy, File, C,
};
use crate::machine::Machine;
use alloc::vec::Vec;

/// The handle table of the process whose PSP is `psp`.
fn handle_table(m: &Machine, psp: u16) -> Vec<u8> {
    use crate::dos::memory::psp::{JFT, JFT_COUNT};
    (0..JFT_COUNT)
        .map(|n| m.board().peek(crate::cpu::linear(psp, JFT + n)))
        .collect()
}

#[test]
fn the_primary_shell_runs_autoexec_and_shows_its_prompt() {
    let m = prompted(&[]);
    let text = screen(&m);
    assert!(text.contains("A:\\>"), "{text}");
    assert!(
        !text.contains("ECHO OFF"),
        "an @ line is not echoed:\n{text}"
    );
}

#[test]
fn ver_and_echo_answer_at_the_prompt() {
    let mut m = prompted(&[]);
    typed(&mut m, "ver");
    typed(&mut m, "echo Hello there");
    let text = screen(&m);
    assert!(text.contains("TAIRiX DOS Version 5.00"), "{text}");
    assert!(text.contains("\nHello there\n"), "{text}");
}

#[test]
fn a_command_it_does_not_know_is_said_so() {
    let mut m = prompted(&[]);
    typed(&mut m, "frobnicate");
    assert!(
        screen(&m).contains("Bad command or file name"),
        "{}",
        screen(&m)
    );
}

#[test]
fn without_autoexec_the_date_and_time_are_asked_for() {
    let mut m = shell_machine(&[]);
    assert!(screen(&m).contains("Enter new date"), "{}", screen(&m));
    typed(&mut m, "");
    assert!(screen(&m).contains("Enter new time"), "{}", screen(&m));
    typed(&mut m, "");
    let text = screen(&m);
    assert!(
        text.contains("TAIRiX DOS Version 5.00") && text.contains("A>"),
        "{text}"
    );
}

#[test]
fn dir_lists_files_their_sizes_and_the_space_left() {
    let mut m = prompted(&[File {
        name: b"README  TXT",
        bytes: &[b'x'; 100],
    }]);
    typed(&mut m, "dir");
    let text = screen(&m);
    assert!(text.contains("Volume in drive A"), "{text}");
    assert!(text.contains("Directory of A:\\"), "{text}");
    assert!(text.contains("README   TXT       100"), "{text}");
    assert!(
        text.contains("file(s)") && text.contains("bytes free"),
        "{text}"
    );
}

#[test]
fn type_writes_a_file_and_redirection_makes_one() {
    let mut m = prompted(&[]);
    typed(&mut m, "echo first line> notes.txt");
    typed(&mut m, "echo second line>>notes.txt");
    typed(&mut m, "type notes.txt");
    let text = screen(&m);
    assert!(text.contains("first line\nsecond line\n"), "{text}");
}

#[test]
fn copy_del_and_ren_change_the_files() {
    let mut m = prompted(&[File {
        name: b"ONE     TXT",
        bytes: b"one\r\n",
    }]);
    typed(&mut m, "copy one.txt two.txt");
    assert!(screen(&m).contains("1 file(s) copied"), "{}", screen(&m));
    typed(&mut m, "ren two.txt three.txt");
    typed(&mut m, "type three.txt");
    assert!(screen(&m).contains("\none\n"), "{}", screen(&m));
    typed(&mut m, "del three.txt");
    typed(&mut m, "type three.txt");
    assert!(screen(&m).contains("File not found"), "{}", screen(&m));
}

#[test]
fn md_cd_and_rd_move_through_directories() {
    let mut m = prompted(&[]);
    typed(&mut m, "md sub");
    typed(&mut m, "cd sub");
    assert!(screen(&m).contains("A:\\SUB>"), "{}", screen(&m));
    typed(&mut m, "cd ..");
    typed(&mut m, "rd sub");
    typed(&mut m, "cd sub");
    assert!(screen(&m).contains("Invalid directory"), "{}", screen(&m));
}

#[test]
fn set_keeps_variables_and_lists_them() {
    let mut m = prompted(&[]);
    typed(&mut m, "set colour=green");
    typed(&mut m, "set");
    let text = screen(&m);
    assert!(text.contains("COLOUR=green"), "{text}");
    assert!(text.contains("COMSPEC=A:\\COMMAND.COM"), "{text}");
    assert!(text.contains("PROMPT=$P$G"), "{text}");
}

#[test]
fn a_shell_keeps_the_comspec_and_every_variable_it_is_given() {
    // A shell started from another named itself at the boot drive's root in
    // place of the COMSPEC it was given, and kept no more of its environment
    // than its own size: the variable at the edge was cut in two.
    let long = "x".repeat(70);
    let autoexec = alloc::format!(
        "@ECHO OFF\r\nPROMPT $P$G\r\nSET COMSPEC=A:\\DOS\\COMMAND.COM\r\nSET ONE={long}\r\nSET TWO={long}\r\nSET THREE={long}\r\nSET FOUR={long}\r\n"
    );
    let mut m = shell_machine(&[
        File {
            name: b"CONFIG  SYS",
            bytes: b"SHELL=A:\\COMMAND.COM /P /E:1024\r\n",
        },
        File {
            name: b"AUTOEXECBAT",
            bytes: autoexec.as_bytes(),
        },
    ]);
    typed(&mut m, "cls");
    typed(&mut m, "command /c set");
    let text = screen(&m);
    assert!(text.contains("COMSPEC=A:\\DOS\\COMMAND.COM"), "{text}");
    for name in ["ONE", "TWO", "THREE", "FOUR"] {
        assert!(
            text.contains(&alloc::format!("{name}={long}")),
            "{name}:\n{text}"
        );
    }
}

#[test]
fn an_environment_size_no_number_holds_leaves_the_shell_its_own() {
    let mut m = prompted(&[]);
    typed(&mut m, "cls");
    typed(&mut m, "command /e:99999999999 /c echo started");
    assert!(screen(&m).contains("started"), "{}", screen(&m));
}

#[test]
fn a_shell_told_a_directory_checks_it_holds_the_shell() {
    let mut m = prompted(&[]);
    typed(&mut m, "cls");
    typed(&mut m, "command a:\\nowhere /c set");
    let text = screen(&m);
    assert!(
        text.contains("Specified COMMAND search directory bad"),
        "{text}"
    );
    assert!(text.contains("COMSPEC=A:\\COMMAND.COM"), "{text}");
    typed(&mut m, "md dos");
    typed(&mut m, "copy command.com dos");
    typed(&mut m, "cls");
    typed(&mut m, "command a:\\dos /c set");
    assert!(
        screen(&m).contains("COMSPEC=A:\\DOS\\COMMAND.COM"),
        "{}",
        screen(&m)
    );
}

#[test]
fn a_batch_file_takes_arguments_labels_and_conditions() {
    let batch = b"@echo off\r\necho [%1] [%2]\r\nshift\r\necho [%1]\r\nif \"%1\"==\"b\" goto yes\r\necho no\r\ngoto end\r\n:yes\r\necho yes\r\n:end\r\nfor %%x in (p q) do echo item %%x\r\n";
    let mut m = prompted(&[File {
        name: b"RUN     BAT",
        bytes: batch,
    }]);
    typed(&mut m, "run a b");
    let text = screen(&m);
    assert!(
        text.contains("[a] [b]\n[b]\nyes\nitem p\nitem q\n"),
        "{text}"
    );
}

/// A program that ends with code `code`.
fn ending_with(code: u8) -> Vec<u8> {
    com(|asm| {
        asm.mov_imm16(crate::asm::Reg16::Ax, 0x4C00 | u16::from(code));
        asm.int(0x21);
    })
}

#[test]
fn a_program_runs_and_its_code_is_the_errorlevel() {
    let three = ending_with(3);
    let batch = b"@echo off\r\nthree\r\nif errorlevel 3 echo at least three\r\nif errorlevel 4 echo at least four\r\n";
    let mut m = prompted(&[
        File {
            name: b"THREE   COM",
            bytes: &three,
        },
        File {
            name: b"CHECK   BAT",
            bytes: batch,
        },
    ]);
    typed(&mut m, "check");
    let text = screen(&m);
    assert!(
        text.contains("at least three") && !text.contains("at least four"),
        "{text}"
    );
}

#[test]
fn batch_lines_are_echoed_until_echo_off_and_at_lines_never() {
    let batch = b"echo one\r\n@echo two\r\n:label\r\necho off\r\necho three\r\n";
    let mut m = prompted(&[File {
        name: b"SHOW    BAT",
        bytes: batch,
    }]);
    typed(&mut m, "echo on");
    typed(&mut m, "show");
    let text = screen(&m);
    assert!(
        text.contains("A:\\>show\n\nA:\\>echo one\none\ntwo\n\nA:\\>echo off\nthree\n"),
        "{text}"
    );
}

#[test]
fn each_prompt_ends_the_last_commands_output_with_a_blank_line() {
    // COMMAND.COM begins each prompt on a line of its own after a command,
    // as a program's last line may not end; an empty line had no output.
    let mut m = prompted(&[]);
    typed(&mut m, "cls");
    typed(&mut m, "echo hi");
    typed(&mut m, "");
    typed(&mut m, "echo there");
    let text = screen(&m);
    assert!(
        text.starts_with("\nA:\\>echo hi\nhi\n\nA:\\>\nA:\\>echo there\nthere\n\nA:\\>"),
        "{text}"
    );
}

#[test]
fn echo_off_at_the_prompt_hides_the_prompt_until_echo_on() {
    let mut m = prompted(&[]);
    typed(&mut m, "echo off");
    typed(&mut m, "ver");
    typed(&mut m, "echo on");
    let text = screen(&m);
    assert!(
        text.contains("A:\\>echo off\nver\n"),
        "no prompt before ver:\n{text}"
    );
    assert!(text.trim_end().ends_with("A:\\>"), "{text}");
}

/// A filter: standard input to standard output, upper-cased.
const UPPER: [u8; 57] = [
    0xB4, 0x3F, 0x31, 0xDB, 0xB9, 0x80, 0x00, 0xBA, 0x00, 0x02, 0xCD, 0x21, 0x85, 0xC0, 0x74,
    0x24, // read; none: done
    0x89, 0xC1, 0x50, 0xBE, 0x00, 0x02, 0x8A, 0x04, 0x3C, 0x61, 0x72, 0x06, 0x3C, 0x7A, 0x77,
    0x02, // each byte
    0x2C, 0x20, 0x88, 0x04, 0x46, 0xE2, 0xEF, 0x59, 0xB4, 0x40, 0xBB, 0x01, 0x00, 0xBA, 0x00,
    0x02, // upper; write
    0xCD, 0x21, 0xEB, 0xCC, 0xB8, 0x00, 0x4C, 0xCD, 0x21, // again; done
];

fn with_upper(files: &[File<'_>]) -> Machine {
    let mut all = alloc::vec![File {
        name: b"UPPER   COM",
        bytes: &UPPER
    }];
    all.extend(files.iter().map(|f| File {
        name: f.name,
        bytes: f.bytes,
    }));
    prompted(&all)
}

#[test]
fn a_pipeline_passes_each_stages_output_to_the_next() {
    let mut m = with_upper(&[]);
    typed(&mut m, "echo hello | upper");
    assert!(screen(&m).contains("\nHELLO\n"), "{}", screen(&m));
    typed(&mut m, "echo abc|upper|upper");
    assert!(screen(&m).contains("\nABC\n"), "{}", screen(&m));
    typed(&mut m, "dir /b");
    assert!(
        !screen(&m).contains(".$$$") && screen(&m).matches("UPPER.COM").count() >= 1,
        "the pipe files are gone:\n{}",
        screen(&m)
    );
}

#[test]
fn input_is_read_from_the_file_a_stage_names() {
    let mut m = with_upper(&[File {
        name: b"WORDS   TXT",
        bytes: b"quiet words\r\n",
    }]);
    typed(&mut m, "upper < words.txt");
    assert!(screen(&m).contains("\nQUIET WORDS\n"), "{}", screen(&m));
    typed(&mut m, "upper < missing.txt");
    assert!(screen(&m).contains("File not found"), "{}", screen(&m));
}

#[test]
fn if_exist_and_for_over_a_wildcard_see_the_files() {
    let batch = b"@echo off\r\nif exist one.txt echo has one\r\nif not exist none.txt echo has no none\r\nfor %%f in (*.txt) do echo got %%f\r\n";
    let mut m = prompted(&[
        File {
            name: b"ONE     TXT",
            bytes: b"1",
        },
        File {
            name: b"TWO     TXT",
            bytes: b"2",
        },
        File {
            name: b"LOOK    BAT",
            bytes: batch,
        },
    ]);
    typed(&mut m, "look");
    let text = screen(&m);
    assert!(text.contains("has one\nhas no none\n"), "{text}");
    assert!(
        text.contains("got ONE.TXT") && text.contains("got TWO.TXT"),
        "{text}"
    );
}

#[test]
fn call_runs_a_batch_file_and_comes_back() {
    let outer = b"@echo off\r\necho before\r\ncall inner x\r\necho after\r\n";
    let inner = b"echo inside %1\r\n";
    let mut m = prompted(&[
        File {
            name: b"OUTER   BAT",
            bytes: outer,
        },
        File {
            name: b"INNER   BAT",
            bytes: inner,
        },
    ]);
    typed(&mut m, "outer");
    assert!(
        screen(&m).contains("before\ninside x\nafter\n"),
        "{}",
        screen(&m)
    );
}

#[test]
fn goto_a_missing_label_ends_the_batch_file() {
    let batch = b"@echo off\r\ngoto nowhere\r\necho not reached\r\n";
    let mut m = prompted(&[File {
        name: b"LOST    BAT",
        bytes: batch,
    }]);
    typed(&mut m, "lost");
    let text = screen(&m);
    assert!(
        text.contains("Label not found") && !text.contains("not reached"),
        "{text}"
    );
}

#[test]
fn ctrl_c_at_the_prompt_gives_a_new_prompt() {
    let mut m = prompted(&[]);
    control(&mut m, C);
    settle(&mut m);
    typed(&mut m, "ver");
    let text = screen(&m);
    assert!(
        text.contains("^C") && text.contains("TAIRiX DOS Version 5.00"),
        "{text}"
    );
}

#[test]
fn type_reads_a_device_until_its_end_of_file() {
    // A read that brought less than the buffer ended TYPE, and a device
    // brings a line a read: TYPE CON stopped at its first line.
    let mut m = prompted(&[]);
    typed(&mut m, "type con > typed.txt");
    typed(&mut m, "one");
    typed(&mut m, "two");
    control(&mut m, Scan::plain(0x2C));
    typed(&mut m, "");
    typed(&mut m, "cls");
    typed(&mut m, "type typed.txt");
    let text = screen(&m);
    assert!(text.contains("\none\ntwo\n"), "{text}");
}

#[test]
fn a_drive_with_no_diskette_asks_how_to_go_on() {
    let mut m = prompted(&[]);
    typed(&mut m, "dir b:");
    let text = screen(&m);
    assert!(text.contains("Not ready reading drive B"), "{text}");
    assert!(text.contains("Abort, Retry, Fail?"), "{text}");
    typed(&mut m, "f");
    // The failed drive is asked nothing more: DIR asked it again for the
    // label it had already failed to read.
    assert_eq!(
        screen(&m).matches("Abort, Retry, Fail?").count(),
        1,
        "{}",
        screen(&m)
    );
    typed(&mut m, "ver");
    assert!(
        screen(&m).contains("TAIRiX DOS Version 5.00"),
        "the shell goes on:\n{}",
        screen(&m)
    );
}

#[test]
fn a_command_failed_at_the_drive_says_nothing_more() {
    // After the failure was answered, each went on to say what it would
    // have of a name that was not there.
    let mut m = prompted(&[]);
    for command in [
        "type b:x.txt",
        "del b:x.txt",
        "ren b:x.txt y.txt",
        "cd b:\\x",
        "md b:\\x",
        "copy b:x.txt",
    ] {
        typed(&mut m, "cls");
        typed(&mut m, command);
        assert!(
            screen(&m).contains("Not ready reading drive B"),
            "{command}:\n{}",
            screen(&m)
        );
        typed(&mut m, "f");
        let text = screen(&m);
        assert_eq!(
            text.matches("Abort, Retry, Fail?").count(),
            1,
            "{command}:\n{text}"
        );
        for misleading in [
            "File not found",
            "Invalid directory",
            "Unable to create",
            "Duplicate file name",
        ] {
            assert!(!text.contains(misleading), "{command}:\n{text}");
        }
    }
}

#[test]
fn a_path_through_an_empty_drive_asks_once_for_it() {
    // Each name a directory might hold was looked for apart, the drive
    // asked about for every one.
    let mut m = prompted(&[]);
    typed(&mut m, "path b:\\");
    typed(&mut m, "cls");
    typed(&mut m, "nothing");
    typed(&mut m, "f");
    let text = screen(&m);
    assert_eq!(text.matches("Abort, Retry, Fail?").count(), 1, "{text}");
    assert!(text.contains("Bad command or file name"), "{text}");
}

#[test]
fn the_prompt_of_a_drive_that_cannot_be_read_says_it_is_no_longer_valid() {
    // DOS gave the directory without checking the disk, so the prompt
    // showed the root of a drive with nothing in it.
    let mut m = prompted(&[]);
    typed(&mut m, "b:");
    assert!(
        screen(&m).contains("Not ready reading drive B"),
        "{}",
        screen(&m)
    );
    type_ahead(&mut m, "f");
    settle(&mut m);
    let text = screen(&m);
    assert!(
        text.trim_end()
            .ends_with("Current drive is no longer valid>"),
        "{text}"
    );
    typed(&mut m, "a:");
    assert!(screen(&m).trim_end().ends_with("A:\\>"), "{}", screen(&m));
}

/// A program that stays resident with INT 21h hooked so function `19h`
/// answers drive `F0h`.
fn drive_hook() -> Vec<u8> {
    use crate::asm::{Cond, Reg16, Reg8};
    com(|asm| {
        let (handler, chain, old) = (asm.label(), asm.label(), asm.label());
        asm.mov_imm16(Reg16::Ax, 0x3521);
        asm.int(0x21);
        asm.mov_label(Reg16::Di, &old);
        asm.bytes(&[0x89, 0x1D, 0x8C, 0x45, 0x02]); // mov [di], bx; mov [di+2], es
        asm.mov_label(Reg16::Dx, &handler);
        asm.mov_imm16(Reg16::Ax, 0x2521);
        asm.int(0x21);
        asm.mov_imm16(Reg16::Dx, 0x20);
        asm.mov_imm16(Reg16::Ax, 0x3100);
        asm.int(0x21);
        asm.bind(&handler);
        asm.cmp_imm8(Reg8::Ah, 0x19);
        asm.jcc(Cond::Ne, &chain);
        asm.mov_imm8(Reg8::Al, 0xF0);
        asm.iret();
        asm.bind(&chain);
        asm.bytes(&[0xEA]); // jmp far to the vector kept here
        asm.bind(&old);
        asm.word(0);
        asm.word(0);
    })
}

#[test]
fn a_drive_number_past_z_is_spelt_as_dos_spells_it() {
    // A letter was `A` plus the number DOS gave, which a program hooking
    // DOS can make anything: past `Z` the sum overflowed and stopped the
    // emulator.
    let hook = drive_hook();
    let mut m = prompted(&[File {
        name: b"HOOK    COM",
        bytes: &hook,
    }]);
    typed(&mut m, "hook");
    assert!(screen(&m).trim_end().ends_with("1:\\>"), "{}", screen(&m));
    typed(&mut m, "cd");
    typed(&mut m, "vol");
    let text = screen(&m);
    assert!(text.contains("\n1:\\\n"), "{text}");
    assert!(text.contains("Invalid drive specification"), "{text}");
}

#[test]
fn a_drive_that_is_no_letter_is_an_invalid_drive() {
    // DIR's drive was the name's first byte less `A`, and its volume line
    // added `A` back past a byte.
    let mut m = prompted(&[]);
    for name in ["1:", "@:", "[:"] {
        typed(&mut m, "cls");
        typed(&mut m, &["dir ", name].concat());
        assert!(
            screen(&m).contains("Invalid drive specification"),
            "dir {name}:\n{}",
            screen(&m)
        );
    }
}

#[test]
fn a_copy_that_meets_a_bad_sector_stops_and_closes_what_it_opened() {
    // A read or write that failed returned at once, the source and target
    // still open.
    let big: Vec<u8> = (0..4096u16)
        .map(|n| n.to_le_bytes()[0] ^ n.to_le_bytes()[1])
        .collect();
    let mut m = prompted(&[File {
        name: b"BIG     TXT",
        bytes: &big,
    }]);
    let image = diskette_bytes(&mut m.board_mut().eject_floppy(0).unwrap());
    let bad = sector_of(&image, b"BIG     TXT", 3072).unwrap();
    m.board_mut()
        .insert_floppy(0, unreliable_floppy(image, move |sector, _| sector == bad))
        .unwrap();
    let shell = kernel16(&m, SDA + sda::PSP);
    let handles = handle_table(&m, shell);
    typed(&mut m, "cls");
    typed(&mut m, "copy big.txt copy.txt");
    assert!(
        screen(&m).contains("Data error reading drive A"),
        "{}",
        screen(&m)
    );
    typed(&mut m, "f");
    let text = screen(&m);
    assert!(text.contains("0 file(s) copied"), "{text}");
    assert_eq!(
        handle_table(&m, shell),
        handles,
        "nothing the copy opened is left open"
    );
}

#[test]
fn a_listing_broken_at_its_pause_gives_its_memory_back() {
    // A command a Ctrl+C abandons never reaches its own end, so the blocks
    // DIR keeps its walk and its sort in stayed the shell's.
    let names: Vec<[u8; 11]> = (0..30u8)
        .map(|n| {
            let mut name = *b"FILE    TXT";
            name[4..6].copy_from_slice(&[b'A' + n / 10, b'0' + n % 10]);
            name
        })
        .collect();
    let files: Vec<File<'_>> = names
        .iter()
        .map(|name| File { name, bytes: b"x" })
        .collect();
    let mut m = prompted(&files);
    let shell = kernel16(&m, SDA + sda::PSP);
    let held = blocks_of(&m, shell);
    typed(&mut m, "dir /s /o /p");
    assert!(
        screen(&m).contains("Press any key to continue"),
        "{}",
        screen(&m)
    );
    assert!(
        blocks_of(&m, shell).len() > held.len(),
        "the walk and the sort are held"
    );
    control(&mut m, C);
    settle(&mut m);
    assert!(screen(&m).contains("^C"), "{}", screen(&m));
    assert_eq!(blocks_of(&m, shell), held);
}

#[test]
fn a_shell_given_a_batch_file_to_run_ends_with_it() {
    // A batch file's end was not taken for the end of the one command a
    // `/C` shell runs, so it waited at a prompt of its own.
    let mut m = prompted(&[File {
        name: b"ONE     BAT",
        bytes: b"@echo ran one\r\n",
    }]);
    let shell = kernel16(&m, SDA + sda::PSP);
    typed(&mut m, "command /c one.bat");
    assert!(screen(&m).contains("ran one"), "{}", screen(&m));
    assert_eq!(
        kernel16(&m, SDA + sda::PSP),
        shell,
        "the first shell runs again"
    );
}

#[test]
fn a_second_shell_runs_one_command_or_until_exit() {
    let mut m = prompted(&[]);
    typed(&mut m, "command /c echo inner");
    assert!(screen(&m).contains("\ninner\n"), "{}", screen(&m));
    typed(&mut m, "command");
    typed(&mut m, "set nested=1");
    typed(&mut m, "exit");
    typed(&mut m, "set");
    let text = screen(&m);
    assert!(
        text.contains("TAIRiX DOS Version 5.00"),
        "the second shell's banner:\n{text}"
    );
    assert!(
        !text.contains("NESTED=1\n"),
        "its environment was its own:\n{text}"
    );
}

#[test]
fn path_finds_a_program_in_another_directory() {
    let mut m = with_upper(&[]);
    typed(&mut m, "md bin");
    typed(&mut m, "copy upper.com bin");
    typed(&mut m, "del upper.com");
    typed(&mut m, "echo from path | upper");
    assert!(
        screen(&m).contains("Bad command or file name"),
        "{}",
        screen(&m)
    );
    typed(&mut m, "path a:\\bin");
    typed(&mut m, "echo from path | upper");
    assert!(screen(&m).contains("\nFROM PATH\n"), "{}", screen(&m));
    typed(&mut m, "path");
    assert!(screen(&m).contains("PATH=A:\\BIN"), "{}", screen(&m));
}

#[test]
fn dir_lists_wide_bare_sorted_and_by_attribute() {
    let mut m = prompted(&[
        File {
            name: b"ZEBRA   TXT",
            bytes: b"z",
        },
        File {
            name: b"APPLE   TXT",
            bytes: b"aa",
        },
    ]);
    typed(&mut m, "cls");
    typed(&mut m, "dir /b /o");
    let text = screen(&m);
    let apple = text.find("APPLE.TXT").unwrap_or(usize::MAX);
    let zebra = text.find("ZEBRA.TXT").unwrap_or(0);
    assert!(apple < zebra, "sorted by name:\n{text}");
    typed(&mut m, "cls");
    typed(&mut m, "dir /w");
    assert!(
        screen(&m).contains("ZEBRA.TXT       APPLE.TXT"),
        "a column each:\n{}",
        screen(&m)
    );
    typed(&mut m, "cls");
    typed(&mut m, "dir *.txt /o-s /b");
    let text = screen(&m);
    assert!(
        text.find("APPLE.TXT") < text.find("ZEBRA.TXT"),
        "largest first:\n{text}"
    );
    typed(&mut m, "dir nothing.here");
    assert!(screen(&m).contains("File not found"), "{}", screen(&m));
}

#[test]
fn dir_s_walks_every_subdirectory() {
    let mut m = prompted(&[File {
        name: b"TOP     TXT",
        bytes: b"t",
    }]);
    typed(&mut m, "md a");
    typed(&mut m, "md a\\b");
    typed(&mut m, "md c");
    typed(&mut m, "copy top.txt a\\b\\deep.txt");
    typed(&mut m, "copy top.txt c\\side.txt");
    typed(&mut m, "cls");
    typed(&mut m, "dir *.txt /s /b");
    let text = screen(&m);
    let (deep, side) = (
        text.find("A:\\A\\B\\DEEP.TXT"),
        text.find("A:\\C\\SIDE.TXT"),
    );
    assert!(
        deep.is_some() && side.is_some() && deep < side,
        "a directory's tree, then its sibling's:\n{text}"
    );
    typed(&mut m, "cls");
    typed(&mut m, "dir *.txt /s");
    let text = screen(&m);
    assert!(text.contains("Directory of A:\\A\\B"), "{text}");
    assert!(text.contains("DEEP     TXT"), "{text}");
    assert!(text.contains("Total files listed:"), "{text}");
}

#[test]
fn copy_joins_files_and_copies_a_wildcard_to_a_directory() {
    let mut m = prompted(&[
        File {
            name: b"A       TXT",
            bytes: b"alpha\r\n",
        },
        File {
            name: b"B       TXT",
            bytes: b"beta\r\n",
        },
    ]);
    typed(&mut m, "copy a.txt+b.txt both.txt");
    typed(&mut m, "type both.txt");
    assert!(screen(&m).contains("alpha\nbeta\n"), "{}", screen(&m));
    typed(&mut m, "md keep");
    typed(&mut m, "copy *.txt keep");
    assert!(screen(&m).contains("3 file(s) copied"), "{}", screen(&m));
    typed(&mut m, "copy a.txt a.txt");
    assert!(
        screen(&m).contains("File cannot be copied onto itself"),
        "{}",
        screen(&m)
    );
    typed(&mut m, "ren keep\\*.txt *.bak");
    typed(&mut m, "dir keep /b");
    assert!(screen(&m).contains("BOTH.BAK"), "{}", screen(&m));
}

#[test]
fn del_of_every_file_asks_first() {
    let mut m = prompted(&[
        File {
            name: b"A       TXT",
            bytes: b"a",
        },
        File {
            name: b"B       TXT",
            bytes: b"b",
        },
    ]);
    typed(&mut m, "del *.*");
    assert!(screen(&m).contains("Are you sure (Y/N)?"), "{}", screen(&m));
    typed(&mut m, "n");
    typed(&mut m, "dir /b");
    assert!(screen(&m).contains("A.TXT"), "kept:\n{}", screen(&m));
    typed(&mut m, "del *.txt");
    typed(&mut m, "dir *.txt");
    assert!(screen(&m).contains("File not found"), "{}", screen(&m));
}

#[test]
fn a_drive_that_is_not_there_is_said_so() {
    let mut m = prompted(&[]);
    typed(&mut m, "q:");
    assert!(
        screen(&m).contains("Invalid drive specification"),
        "{}",
        screen(&m)
    );
    typed(&mut m, "truename one\\..\\two");
    assert!(screen(&m).contains("A:\\TWO"), "{}", screen(&m));
}

#[test]
fn the_switches_code_page_volume_and_console_are_said_and_set() {
    let mut m = prompted(&[File {
        name: b"PLAIN   TXT",
        bytes: b"a file",
    }]);
    let mut seen = alloc::string::String::new();
    for command in [
        "vol",
        "break",
        "break on",
        "break",
        "verify on",
        "verify",
        "break sideways",
        "chcp",
        "chcp 999",
    ] {
        typed(&mut m, "cls");
        typed(&mut m, command);
        seen.push_str(&screen(&m));
    }
    for said in [
        "Volume in drive A has no label",
        "Volume Serial Number is 1986-0310",
        "BREAK is off",
        "BREAK is on",
        "VERIFY is on",
        "Must specify ON or OFF",
        "Active code page: 437",
        "Invalid code page",
    ] {
        assert!(seen.contains(said), "{said}:\n{seen}");
    }
    typed(&mut m, "cls");
    // A file is no console: it was made standard input and output.
    typed(&mut m, "ctty plain.txt");
    typed(&mut m, "ctty nowhere");
    typed(&mut m, "rem nothing at all");
    typed(&mut m, "ver");
    let text = screen(&m);
    assert_eq!(text.matches("Invalid device").count(), 2, "{text}");
    assert!(
        text.contains("TAIRiX DOS Version 5.00"),
        "the console is still the console:\n{text}"
    );
}

#[test]
fn the_date_and_time_given_are_set() {
    let mut m = prompted(&[]);
    typed(&mut m, "date 12-25-1999");
    typed(&mut m, "time 13:45");
    typed(&mut m, "cls");
    typed(&mut m, "date");
    typed(&mut m, "");
    typed(&mut m, "time");
    typed(&mut m, "");
    let text = screen(&m);
    assert!(text.contains("Current date is Sat 12-25-1999"), "{text}");
    let time = text
        .lines()
        .find(|line| line.starts_with("Current time is"))
        .unwrap_or_default();
    assert!(
        time.starts_with("Current time is  1:45:") && time.ends_with('p'),
        "{text}"
    );
    typed(&mut m, "date 2-30-1999");
    assert!(screen(&m).contains("Invalid date"), "{}", screen(&m));
}

#[test]
fn a_batch_file_shifts_its_arguments_and_pauses() {
    let batch = b"@echo off\r\necho %1\r\nshift\r\necho %1\r\npause\r\necho done\r\n";
    let mut m = prompted(&[File {
        name: b"STEP    BAT",
        bytes: batch,
    }]);
    typed(&mut m, "cls");
    typed(&mut m, "step first second");
    let text = screen(&m);
    assert!(
        text.contains("first\nsecond\nPress any key to continue . . ."),
        "{text}"
    );
    assert!(!text.contains("done"), "it waits:\n{text}");
    typed(&mut m, "");
    assert!(screen(&m).contains("done"), "{}", screen(&m));
}

#[test]
fn a_program_loaded_high_runs_where_it_can() {
    let seven = ending_with(7);
    let batch = b"@echo off\r\nlh seven\r\nif errorlevel 7 echo it ran\r\n";
    let mut m = prompted(&[
        File {
            name: b"SEVEN   COM",
            bytes: &seven,
        },
        File {
            name: b"HIGH    BAT",
            bytes: batch,
        },
    ]);
    typed(&mut m, "high");
    assert!(screen(&m).contains("it ran"), "{}", screen(&m));
}

#[test]
fn a_copy_told_nothing_says_so_once() {
    // Its arguments were looked at again once the message was out, so it
    // said it again, for ever.
    let mut m = prompted(&[]);
    typed(&mut m, "cls");
    typed(&mut m, "copy");
    typed(&mut m, "copy /a");
    let text = screen(&m);
    assert_eq!(
        text.matches("Required parameter missing").count(),
        1,
        "{text}"
    );
    assert_eq!(text.matches("Invalid parameter").count(), 1, "{text}");
}

#[test]
fn a_critical_error_in_a_redirected_stage_asks_on_the_console() {
    // The question went to the stage's own output, a failing device here,
    // and the answer came from its input, a file at its end: it asked for
    // ever, and nobody saw it.
    let mut m = prompted(&[File {
        name: b"FILE    TXT",
        bytes: b"line\r\n",
    }]);
    typed(&mut m, "cls");
    typed(&mut m, "ver <file.txt >aux");
    let text = screen(&m);
    assert!(
        text.contains("Write fault error writing device AUX"),
        "{text}"
    );
    assert!(text.contains("Abort, Retry, Ignore, Fail?"), "{text}");
    typed(&mut m, "f");
    typed(&mut m, "ver");
    assert!(
        screen(&m).contains("TAIRiX DOS Version 5.00"),
        "the shell goes on:\n{}",
        screen(&m)
    );
}

#[test]
fn ctrl_c_in_a_redirected_batch_line_asks_on_the_console() {
    // The batch file's question went where its line's output went.
    let batch = b"@echo off\r\npause >nul\r\necho not reached\r\n";
    let mut m = prompted(&[File {
        name: b"WAIT    BAT",
        bytes: batch,
    }]);
    typed(&mut m, "cls");
    typed(&mut m, "wait");
    control(&mut m, C);
    settle(&mut m);
    assert!(
        screen(&m).contains("Terminate batch job (Y/N)?"),
        "{}",
        screen(&m)
    );
    typed(&mut m, "y");
    let text = screen(&m);
    assert!(!text.contains("not reached"), "{text}");
    typed(&mut m, "ver");
    assert!(
        screen(&m).contains("TAIRiX DOS Version 5.00"),
        "{}",
        screen(&m)
    );
}

#[test]
fn keys_typed_ahead_of_a_critical_error_never_answer_it() {
    // The handler took the next key there was: one typed before the question
    // was its answer, here a retry of an empty drive.
    let mut m = prompted(&[]);
    typed(&mut m, "cls");
    type_ahead(&mut m, "dir b:\rr");
    settle(&mut m);
    let text = screen(&m);
    assert_eq!(
        text.matches("Not ready reading drive B").count(),
        1,
        "{text}"
    );
    assert!(
        text.trim_end().ends_with("Abort, Retry, Fail?"),
        "it waits for an answer:\n{text}"
    );
    typed(&mut m, "f");
    typed(&mut m, "ver");
    assert!(
        screen(&m).contains("TAIRiX DOS Version 5.00"),
        "{}",
        screen(&m)
    );
}

#[test]
fn a_shell_whose_input_has_ended_ends_as_exit_would() {
    let mut m = prompted(&[File {
        name: b"LINES   TXT",
        bytes: b"echo from the file\r\n",
    }]);
    typed(&mut m, "cls");
    typed(&mut m, "command <lines.txt");
    assert!(screen(&m).contains("\nfrom the file\n"), "{}", screen(&m));
    typed(&mut m, "ver");
    assert!(
        screen(&m).contains("TAIRiX DOS Version 5.00"),
        "back at the first shell:\n{}",
        screen(&m)
    );
}

#[test]
fn a_critical_error_no_console_can_answer_is_failed() {
    // Failed, the batch file goes on; aborted, it would end with its shell.
    let batch = b"@echo off\r\nctty nul\r\ndir b:\r\nctty con\r\necho deaf goes on\r\n";
    let mut m = prompted(&[File {
        name: b"DEAF    BAT",
        bytes: batch,
    }]);
    typed(&mut m, "cls");
    typed(&mut m, "command /c deaf");
    let text = screen(&m);
    assert!(text.contains("\ndeaf goes on\n"), "{text}");
    assert!(
        !text.contains("Abort, Retry"),
        "asked where no one hears:\n{text}"
    );
}

#[test]
fn a_break_no_console_can_answer_ends_the_batch_job() {
    let batch = b"@echo off\r\nbreak on\r\nctty nul\r\n:top\r\ngoto top\r\n";
    let mut m = prompted(&[File {
        name: b"SPIN    BAT",
        bytes: batch,
    }]);
    typed(&mut m, "cls");
    type_ahead(&mut m, "command /c spin\r");
    let now = m.now();
    crate::bios::run_until(&mut m, now + crate::dos::harness::SECOND);
    control(&mut m, C);
    settle(&mut m);
    typed(&mut m, "ver");
    assert!(
        screen(&m).contains("TAIRiX DOS Version 5.00"),
        "back at the first shell:\n{}",
        screen(&m)
    );
}
