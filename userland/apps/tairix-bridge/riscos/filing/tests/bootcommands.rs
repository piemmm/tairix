//! `BootCommands` through the commands a boot sequence types, each as RISC
//! OS 5 was recorded answering it.

mod common;

use common::{call, command, load, run_out, save, session, text, variable};
use tairix_riscos_kernel::memory::{FREE_POOL, RMA};
use tairix_riscos_kernel::os::{Ended, Fail, Kernel, Os, Outcome, Unwind};
use tairix_riscos_kernel::swi::os::{BYTE, CHANGE_ENVIRONMENT, FILE, GBPB, READ_DYNAMIC_AREA};

/// A directory made as `name`.
fn directory(os: &mut Os<'_>, name: &[u8]) {
    let name = text(os, name);
    call(os, FILE, &[8, name]).expect("a directory");
}

/// The error `outcome` refused with, by number.
fn refused(os: &mut Os<'_>, outcome: Result<(), Fail>) -> Option<u32> {
    match outcome {
        Err(Fail::Error(block)) => Some(os.read_word(block).expect("its number")),
        Ok(()) => None,
        Err(Fail::Unwind(unwind)) => panic!("unwound: {unwind:?}"),
    }
}

/// `line` run: the error it gave, by number.
fn run(os: &mut Os<'_>, line: &[u8]) -> Option<u32> {
    let outcome = command(os, line);
    refused(os, outcome)
}

/// A session with a RAM disc, the current filing system, and an
/// application slot.
fn kernel() -> Kernel {
    let mut kernel = common::kernel();
    session(&mut kernel, 0x1_0000);
    assert_eq!(run(&mut kernel.os(), b"RAM"), None);
    kernel
}

/// The size of `area`, `u32::MAX` for application space.
fn area(os: &mut Os<'_>, area: u32) -> u32 {
    call(os, READ_DYNAMIC_AREA, &[area]).expect("an area").r[1]
}

#[test]
fn x_swallows_an_error_and_keeps_the_first_in_x_error() {
    let mut kernel = kernel();
    let mut os = kernel.os();
    assert_eq!(run(&mut os, b"X NoSuchCommandHere"), None);
    let first = variable(&mut os, b"X$Error").expect("kept");
    assert!(first.starts_with(b"File 'NoSuchCommandHere'"), "{first:?}");
    assert_eq!(run(&mut os, b"X Error 17 Second"), None);
    assert_eq!(
        variable(&mut os, b"X$Error"),
        Some(first.clone()),
        "an earlier one is never replaced"
    );
    assert_eq!(run(&mut os, b"X Set Probe$X fine"), None);
    assert_eq!(variable(&mut os, b"Probe$X").as_deref(), Some(&b"fine"[..]));
    assert_eq!(
        variable(&mut os, b"X$Error"),
        Some(first),
        "nor cleared by a success"
    );
    assert_eq!(run(&mut os, b"X"), None);
}

/// An `*IfThere` line, the variable it sets and the value it leaves in it,
/// none for unset.
type IfThereCase = (&'static [u8], &'static [u8], Option<&'static [u8]>);

#[test]
fn if_there_runs_then_or_else_by_whether_os_file_17_finds_the_object() {
    let mut kernel = kernel();
    let mut os = kernel.os();
    save(&mut os, b"RAM:$.F", 0xFFF, b"f").expect("saved");
    directory(&mut os, b"RAM:$.D");
    let cases: [IfThereCase; 9] = [
        (
            b"IfThere RAM:$.F Then Set Probe$V yes",
            b"Probe$V",
            Some(b"yes"),
        ),
        (
            b"IfThere RAM:$.Missing Then Set Probe$W yes Else Set Probe$W no",
            b"Probe$W",
            Some(b"no"),
        ),
        (
            b"IfThere RAM:$.D THEN Set Probe$X dir",
            b"Probe$X",
            Some(b"dir"),
        ),
        (
            b"IfThere RAM:$.F* then Set Probe$Y wild",
            b"Probe$Y",
            Some(b"wild"),
        ),
        (
            b"IfThere RAM:$.F Then Set Probe$Z aElseb",
            b"Probe$Z",
            Some(b"aElseb"),
        ),
        (
            b"IfThere RAM:$.Missing Then Set Probe$K a Else Set Probe$K b Else c",
            b"Probe$K",
            Some(b"b Else c"),
        ),
        (
            b"IfThere RAM:$.a^b Then Set Probe$L x Else Set Probe$L refused",
            b"Probe$L",
            Some(b"refused"),
        ),
        (
            b"IfThere RAM:$.F.G Then Set Probe$M x Else Set Probe$M through",
            b"Probe$M",
            Some(b"through"),
        ),
        (
            b"IfThere RAM:$.Missing Then Set Probe$N a Else",
            b"Probe$N",
            None,
        ),
    ];
    for (line, name, value) in cases {
        assert_eq!(run(&mut os, line), None, "{line:?}");
        assert_eq!(variable(&mut os, name).as_deref(), value, "{line:?}");
    }
    assert_eq!(
        run(&mut os, b"IfThere RAM:$.F Set Probe$Z x"),
        Some(0xDC),
        "no Then"
    );
    assert_eq!(run(&mut os, b"IfThere RAM:$.F"), Some(0xDC));
    assert_eq!(run(&mut os, b"IfThere"), Some(0xDC));
    assert_eq!(run(&mut os, b"IfThere RAM:$.F Then"), None);
    assert_eq!(
        run(&mut os, b"IfThere RAM:$.F Then NoSuchCommandHere"),
        Some(0xD6)
    );
    os.set_string_variable(b"Probe$Name", b"RAM:$.F")
        .expect("set");
    assert_eq!(
        run(&mut os, b"IfThere <Probe$Name> Then Set Probe$G gs"),
        None
    );
    assert_eq!(
        variable(&mut os, b"Probe$G").as_deref(),
        Some(&b"gs"[..]),
        "the object GSTrans'd"
    );
}

#[test]
fn do_gstranses_its_command_once_before_running_it() {
    let mut kernel = kernel();
    let mut os = kernel.os();
    os.set_string_variable(b"Probe$X", b"hello").expect("set");
    assert_eq!(run(&mut os, b"Do SetMacro Probe$Y <Probe$X>"), None);
    assert_eq!(
        os.variable_text(b"Probe$Y").expect("read").as_deref(),
        Some(&b"hello"[..])
    );
    assert_eq!(run(&mut os, b"Do SetMacro Probe$W |<Probe$X|>"), None);
    assert_eq!(
        os.variable_text(b"Probe$W").expect("read").as_deref(),
        Some(&b"hello"[..]),
        "a macro of <Probe$X>, read expanded"
    );
    assert_eq!(run(&mut os, b"Do Set Probe$Q \"a b\" c"), None);
    assert_eq!(variable(&mut os, b"Probe$Q").as_deref(), Some(&b"a b"[..]));
    assert_eq!(run(&mut os, b"Do"), None);
    assert_eq!(run(&mut os, b"Do <Probe$Unset>"), None);
}

/// A RAM disc's directory `R` holding files and directories of each kind.
fn walked_directory(os: &mut Os<'_>) {
    directory(os, b"RAM:$.R");
    for (name, filetype) in [
        (&b"RAM:$.R.a"[..], 0xFFF),
        (b"RAM:$.R.b", 0xFEB),
        (b"RAM:$.R.e", 0xFFD),
        (b"RAM:$.R.G", 0xFFF),
    ] {
        save(os, name, filetype, b"x").expect("saved");
    }
    for name in [&b"RAM:$.R.c"[..], b"RAM:$.R.!d"] {
        directory(os, name);
    }
    os.set_string_variable(b"Alias$Rec", b"Set Probe$Log <Probe$Log>/%*0")
        .expect("an alias");
}

/// `*Repeat` with `switches` over `R`, run until it exits: what it logged.
fn walk(kernel: &mut Kernel, switches: &[u8]) -> Vec<u8> {
    let mut os = kernel.os();
    os.set_string_variable(b"Probe$Log", b"x").expect("set");
    let line = [&b"Repeat Rec RAM:$.R"[..], switches].concat();
    let started = command(&mut os, &line);
    assert!(
        matches!(started, Err(Fail::Unwind(Unwind::Transfer))),
        "{line:?} becomes the application: {started:?}"
    );
    assert_eq!(
        run_out(kernel),
        Outcome::Ended(Ended::Exit { code: 0 }),
        "{line:?}"
    );
    variable(&mut kernel.os(), b"Probe$Log").expect("logged")
}

#[test]
fn repeat_runs_its_command_on_each_object_the_switches_choose() {
    let mut kernel = kernel();
    walked_directory(&mut kernel.os());
    let all = b"x/RAM:$.R.!d/RAM:$.R.a/RAM:$.R.b/RAM:$.R.c/RAM:$.R.e/RAM:$.R.G";
    let files = b"x/RAM:$.R.a/RAM:$.R.b/RAM:$.R.e/RAM:$.R.G";
    let cases: [(&[u8], &[u8]); 9] = [
        (b"", all),
        (b" -Files", files),
        (b" -Directories", b"x/RAM:$.R.!d/RAM:$.R.c"),
        (b" -Applications", b"x/RAM:$.R.!d"),
        (b" -Type FEB", b"x/RAM:$.R.b"),
        (b" -Type Obey", b"x/RAM:$.R.b"),
        (b" -files -directories", all),
        (b" -F", files),
        (b" -Type FEB -CommandTail t", b"x/RAM:$.R.b t"),
    ];
    for (switches, logged) in cases {
        assert_eq!(walk(&mut kernel, switches), logged, "{switches:?}");
    }
}

#[test]
fn repeat_walks_nothing_where_its_directory_cannot_be_listed() {
    let mut kernel = kernel();
    walked_directory(&mut kernel.os());
    for line in [
        &b"Repeat Rec RAM:$.Missing"[..],
        b"Repeat Rec RAM:$.R.a",
        b"Repeat Rec RAM:$.R.c",
    ] {
        let mut os = kernel.os();
        os.set_string_variable(b"Probe$Log", b"x").expect("set");
        let started = command(&mut os, line);
        assert!(
            matches!(started, Err(Fail::Unwind(Unwind::Transfer))),
            "{started:?}"
        );
        assert_eq!(
            run_out(&mut kernel),
            Outcome::Ended(Ended::Exit { code: 0 })
        );
        assert_eq!(
            variable(&mut kernel.os(), b"Probe$Log").as_deref(),
            Some(&b"x"[..]),
            "{line:?}"
        );
    }
}

#[test]
fn repeat_refuses_what_is_not_its_syntax_before_it_starts() {
    let mut kernel = kernel();
    let mut os = kernel.os();
    walked_directory(&mut os);
    for line in [
        &b"Repeat"[..],
        b"Repeat Rec",
        b"Repeat Rec RAM:$.R -Bogus",
        b"Repeat Rec RAM:$.R -Type",
        b"Repeat Rec RAM:$.R extra",
    ] {
        assert_eq!(run(&mut os, line), Some(0xDC), "{line:?}");
    }
    assert!(
        run(&mut os, b"Repeat Rec RAM:$.R -Type Nonsense").is_some(),
        "an unknown type"
    );
}

/// An Absolute that runs `line` through `XOS_CLI`, then exits.
fn running(line: &[u8]) -> Vec<u8> {
    let code: [u32; 6] = [
        0xE28F_0010,
        0xEF02_0005,
        0xE3A0_0000,
        0xE3A0_1000,
        0xE3A0_2000,
        0xEF00_0011,
    ];
    let mut program: Vec<u8> = code.iter().flat_map(|word| word.to_le_bytes()).collect();
    program.extend_from_slice(line);
    program.push(0);
    program
}

#[test]
fn repeat_goes_on_once_each_program_it_runs_exits() {
    let mut kernel = kernel();
    let mut os = kernel.os();
    directory(&mut os, b"RAM:$.P");
    save(
        &mut os,
        b"RAM:$.P.A1",
        0xFF8,
        &running(b"Set Probe$Log <Probe$Log>/1"),
    )
    .expect("saved");
    save(
        &mut os,
        b"RAM:$.P.A2",
        0xFF8,
        &running(b"Set Probe$Log <Probe$Log>/2"),
    )
    .expect("saved");
    os.set_string_variable(b"Probe$Log", b"x").expect("set");
    let started = command(&mut os, b"Repeat Run RAM:$.P");
    assert!(
        matches!(started, Err(Fail::Unwind(Unwind::Transfer))),
        "{started:?}"
    );
    assert_eq!(
        run_out(&mut kernel),
        Outcome::Ended(Ended::Exit { code: 0 })
    );
    assert_eq!(
        variable(&mut kernel.os(), b"Probe$Log").as_deref(),
        Some(&b"x/1/2"[..]),
        "the second ran once the first exited"
    );
}

/// The files `Resources:$.Apps.<app>` holds, each its name and contents.
fn added(os: &mut Os<'_>, app: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let directory = [&b"Resources:$.Apps."[..], app].concat();
    let name = text(os, &directory);
    let buffer = os.rma_claim(256).expect("room");
    let mut files = Vec::new();
    let mut offset = 0;
    while offset != u32::MAX {
        let regs = call(os, GBPB, &[10, name, buffer, 1, offset, 256, 0]).expect("listed");
        offset = regs.r[4];
        if regs.r[3] == 0 {
            continue;
        }
        let length = os.read_word(buffer + 8).expect("its length") as usize;
        let leaf = os.read_zstring(buffer + 20, 200).expect("its name");
        let whole = [&directory[..], b".", &leaf].concat();
        files.push((leaf, load(os, &whole, length).expect("its contents")));
    }
    files
}

#[test]
fn add_app_hands_each_application_on_from_resources_apps() {
    let mut kernel = kernel();
    let mut os = kernel.os();
    for name in [&b"RAM:$.!Pa"[..], b"RAM:$.!Qa", b"RAM:$.!Qb"] {
        directory(&mut os, name);
    }
    for (name, filetype) in [
        (&b"RAM:$.!Pa.!Run"[..], 0xFEB),
        (b"RAM:$.!Pa.!Boot", 0xFEB),
        (b"RAM:$.!Qa.!Run", 0xFEB),
        (b"RAM:$.!Qa.!Sprites", 0xFF9),
        (b"RAM:$.!Qa.!Help", 0xFFF),
    ] {
        save(&mut os, name, filetype, b"x").expect("saved");
    }
    assert_eq!(
        run(&mut os, b"AddApp RAM:$.!Nothing*"),
        None,
        "a pattern that matches nothing"
    );
    assert_eq!(run(&mut os, b"AddApp RAM:$.!P*"), None);
    assert_eq!(run(&mut os, b"AddApp RAM:$.!Q*"), None);
    let pa = added(&mut os, b"!Pa");
    assert_eq!(
        pa,
        [
            (
                b"!Boot".to_vec(),
                b"/RAM::RamDisc0.$.!Pa.!Boot %*0\n".to_vec()
            ),
            (b"!Run".to_vec(), b"/RAM::RamDisc0.$.!Pa %*0\n".to_vec()),
        ]
    );
    let qa = added(&mut os, b"!Qa");
    assert_eq!(
        qa,
        [
            (
                b"!Boot".to_vec(),
                b"IconSprites RAM::RamDisc0.$.!Qa.!Sprites\n".to_vec()
            ),
            (
                b"!Help".to_vec(),
                b"Filer_Run RAM::RamDisc0.$.!Qa.!Help\n".to_vec()
            ),
            (b"!Run".to_vec(), b"/RAM::RamDisc0.$.!Qa %*0\n".to_vec()),
        ]
    );
    assert_eq!(
        added(&mut os, b"!Qb"),
        [(b"!Run".to_vec(), b"/RAM::RamDisc0.$.!Qb %*0\n".to_vec())]
    );
    assert_eq!(run(&mut os, b"AddApp RAM:$.!Pa"), None);
    assert_eq!(added(&mut os, b"!Pa"), pa, "added again, replaced");
    assert_eq!(
        run(&mut os, b"AddApp RAM:$.Plain"),
        Some(0xD6),
        "a name that matches nothing"
    );
    assert_eq!(
        run(&mut os, b"AddApp RAM:$.Nowhere.!X*"),
        Some(0xD6),
        "a directory that is not there"
    );
}

#[test]
fn applications_added_stay_through_the_modules_restart() {
    let mut kernel = kernel();
    let mut os = kernel.os();
    directory(&mut os, b"RAM:$.!Pa");
    save(&mut os, b"RAM:$.!Pa.!Run", 0xFEB, b"x").expect("saved");
    assert_eq!(run(&mut os, b"AddApp RAM:$.!Pa"), None);
    let pa = added(&mut os, b"!Pa");
    let title = text(&mut os, b"BootCommands");
    call(&mut os, tairix_riscos_kernel::swi::os::MODULE, &[3, title]).expect("restarted");
    assert_eq!(added(&mut os, b"!Pa"), pa, "still in ResourceFS");
    assert_eq!(run(&mut os, b"AddApp RAM:$.!Pa"), None);
    assert_eq!(
        added(&mut os, b"!Pa"),
        pa,
        "added again, replaced as before"
    );
}

#[test]
fn the_rma_commands_move_whole_pages_and_give_back_the_free_top() {
    let mut kernel = kernel();
    let mut os = kernel.os();
    let start = area(&mut os, RMA);
    assert_eq!(run(&mut os, b"AddToRMA 16K"), None);
    assert_eq!(area(&mut os, RMA), start + 0x4000);
    assert_eq!(run(&mut os, b"AddToRMA 4000"), None);
    assert_eq!(area(&mut os, RMA), start + 0x5000, "rounded up to a page");
    assert_eq!(run(&mut os, b"ShrinkRMA"), None);
    let shrunk = area(&mut os, RMA);
    assert!(shrunk <= start, "every free page at the top given back");
    assert_eq!(run(&mut os, b"AddToRMA 1M"), None);
    assert_eq!(area(&mut os, RMA), shrunk + 0x10_0000);
    assert_eq!(run(&mut os, b"AppSize 16K"), None);
    assert_eq!(area(&mut os, RMA), shrunk, "whatever the size");
    for line in [&b"AddToRMA x"[..], b"AppSize x", b"AppSlot x"] {
        assert_eq!(run(&mut os, line), Some(0x16B), "{line:?}");
    }
    assert_eq!(
        run(&mut os, b"AddToRMA 16Q"),
        None,
        "what follows the digits let be"
    );
    assert_eq!(area(&mut os, RMA), shrunk + 0x1000);
    assert_eq!(
        run(&mut os, b"AddToRMA 5000M"),
        Some(0x1C1),
        "948M, as 32 bits hold it"
    );
    assert_eq!(area(&mut os, RMA), shrunk + 0x1000);
    assert_eq!(
        run(&mut os, b"AddToRMA 3000M"),
        Some(0x1C1),
        "a shrink past what the RMA has free"
    );
    assert_eq!(area(&mut os, RMA), shrunk, "the free page given back");
}

/// The current active object made `address`, as a program run from
/// outside application space leaves it.
fn active_object(os: &mut Os<'_>, address: u32) {
    call(os, CHANGE_ENVIRONMENT, &[15, address, 0, 0]).expect("set");
}

#[test]
fn app_slot_makes_application_space_its_size_from_the_free_pool() {
    let mut kernel = kernel();
    let mut os = kernel.os();
    let outside = os.rma_claim(4).expect("room");
    active_object(&mut os, outside);
    let free = area(&mut os, FREE_POOL);
    let application = area(&mut os, u32::MAX);
    assert_eq!(run(&mut os, b"AppSlot 128K"), None);
    assert_eq!(area(&mut os, u32::MAX), 0x2_0000);
    assert_eq!(area(&mut os, FREE_POOL), free - (0x2_0000 - application));
    assert_eq!(run(&mut os, b"AppSlot 128K"), None, "already its size");
    assert_eq!(run(&mut os, b"AppSlot 64K"), None);
    assert_eq!(area(&mut os, u32::MAX), 0x1_0000);
    assert_eq!(run(&mut os, b"FreePool"), None);
    assert_eq!(
        area(&mut os, u32::MAX),
        0,
        "every page given to the free pool"
    );
    assert_eq!(area(&mut os, FREE_POOL), free + application);
}

#[test]
fn application_space_a_program_is_in_is_not_moved() {
    let mut kernel = kernel();
    let mut os = kernel.os();
    active_object(&mut os, 0x8000);
    assert_eq!(run(&mut os, b"AppSlot 128K"), Some(0x1C0));
    assert_eq!(run(&mut os, b"FreePool"), Some(0x1C0));
    assert_eq!(area(&mut os, u32::MAX), 0x1_0000, "nothing moved");
}

#[test]
fn save_cmos_writes_the_cmos_in_logical_order_then_the_release() {
    let mut kernel = kernel();
    let mut os = kernel.os();
    assert_eq!(run(&mut os, b"SaveCMOS RAM:$.C"), None);
    let name = text(&mut os, b"RAM:$.C");
    let read = call(&mut os, FILE, &[20, name]).expect("its information");
    assert_eq!((read.r[0], read.r[4], read.r[6]), (1, 260, 0xFF2));
    let saved = load(&mut os, b"RAM:$.C", 260).expect("loaded");
    for logical in 0..=255u32 {
        let byte = call(&mut os, BYTE, &[161, logical]).expect("read").r[2];
        assert_eq!(
            u32::from(saved[logical as usize]),
            byte,
            "location &{logical:X}"
        );
    }
    assert_eq!(saved[256..], 530u32.to_le_bytes());
    assert_eq!(run(&mut os, b"SaveCMOS RAM:$.Missing.C"), Some(0xD6));
}

#[test]
fn load_cmos_leaves_the_running_cmos_as_it_is() {
    let mut kernel = kernel();
    let mut os = kernel.os();
    let delay = call(&mut os, BYTE, &[161, 0x0C]).expect("read").r[2];
    call(&mut os, BYTE, &[162, 0x0C, 37]).expect("written");
    assert_eq!(run(&mut os, b"SaveCMOS RAM:$.C"), None);
    call(&mut os, BYTE, &[162, 0x0C, 38]).expect("written");
    assert_eq!(run(&mut os, b"LoadCMOS RAM:$.C"), None);
    assert_eq!(run(&mut os, b"LoadCMOS RAM:$.Missing"), None);
    assert_eq!(call(&mut os, BYTE, &[161, 0x0C]).expect("read").r[2], 38);
    call(&mut os, BYTE, &[162, 0x0C, delay]).expect("restored");
}

#[test]
fn safe_logon_hands_the_logon_to_the_filing_system() {
    let mut kernel = kernel();
    let mut os = kernel.os();
    assert_eq!(run(&mut os, b"RAM"), None);
    assert_eq!(run(&mut os, b"SafeLogon guest"), Some(0xD6));
    assert_eq!(run(&mut os, b"SafeLogon :fs guest"), Some(0xD6));
}
