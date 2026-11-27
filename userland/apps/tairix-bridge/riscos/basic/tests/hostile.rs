//! A program may overwrite any word of BASIC's own workspace and stack, which
//! live in its memory, and a filing system may answer with a length no file
//! has. BASIC then meets an error, or goes on as ARM arithmetic would: the
//! host never panics.
//!
//! The workspace's words, as `src/workspace.rs` lays them out about ARGP,
//! &8700: PAGE &866C, TOP &8670, FSA &8674, the installed libraries' list
//! &86A0, `ON ERROR`'s line &8740, `AUTO`'s line &8748 and step &874C, and the
//! chain of variables whose names begin `Q` at &8544.

mod common;

use common::{command, kernel, run_within, Screen};
use tairix_riscos_filing::session::SLOT_BYTES;
use tairix_riscos_kernel::os::{Fail, Outcome, Unwind};

/// Instructions a run may spend.
const BUDGET: u64 = 40_000_000;

/// What BASIC V printed running `program`.
fn run(program: &str) -> String {
    run_within("BASIC", program, b"", BUDGET).text()
}

/// What BASIC V printed at its prompt as `keys` were typed.
fn typed(keys: &str) -> String {
    let mut kernel = kernel(SLOT_BYTES, keys.as_bytes());
    let entered = command(&mut kernel.os(), "BASIC");
    assert!(
        matches!(entered, Err(Fail::Unwind(Unwind::Transfer))),
        "{entered:?}"
    );
    kernel.run(BUDGET);
    let screen = kernel
        .os()
        .extension::<Screen>()
        .map(|s| std::mem::take(&mut s.0))
        .unwrap_or_default();
    String::from_utf8_lossy(&screen).replace("\n\r", "\n")
}

fn assert_says(said: &str, expected: &str) {
    assert!(said.contains(expected), "expected {expected:?} in {said:?}");
}

#[test]
fn a_local_array_past_any_stack_is_no_room_for_it() {
    let said =
        run("10 PROCa\n20 END\n30 DEF PROCa\n40 LOCAL a%()\n50 DIM a%(&3FFFFFFB)\n60 ENDPROC\n");
    assert_says(&said, "No room for this dimension at line 50");
}

#[test]
fn a_local_block_past_what_a_record_s_first_word_holds_is_unwound_whole() {
    let said = run(
        "10 PROCa:PRINT \"back\"\n20 END\n30 DEF PROCa\n40 DIM x% LOCAL 65535\n\
         50 FOR I%=0 TO 65532 STEP 4:x%!I%=-1:NEXT\n60 ENDPROC\n",
    );
    assert_eq!(said, "back\n");
}

#[test]
fn a_library_list_overwritten_is_a_bad_program() {
    assert_says(&run("10 !&86A0=-1:PROCq\n"), "Bad program at line 10");
}

#[test]
fn a_matrix_product_whose_count_overflows_is_the_wrong_size() {
    let said = run("10 DIM l%(65535,0),r%(0,65535),c%(0):c%()=l%().r%()\n");
    assert_says(&said, "Can't assign to array of this size at line 10");
}

#[test]
fn end_below_basic_s_memory_once_fsa_is_overwritten_is_refused() {
    let said = run("10 !&8674=0:SYS \"OS_WriteC\",32:END=&200\n");
    assert_says(&said, "Attempt to allocate insufficient memory at line 10");
}

#[test]
fn an_error_handler_whose_line_is_overwritten_runs_on() {
    let ran = run_within(
        "BASIC",
        "10 ON ERROR ERROR 1,\"x\"\n20 !&8740=-1\n30 ERROR 1,\"x\"\n",
        b"",
        BUDGET,
    );
    assert_eq!(
        ran.outcome,
        Outcome::Budget,
        "the handler raises its error again and again"
    );
}

#[test]
fn a_return_whose_line_is_overwritten_still_reports_its_error() {
    let said = run("10 GOSUB 30:ERROR 1,\"x\"\n20 END\n30 !(HIMEM-8)=-1:RETURN\n");
    assert!(said.starts_with("x at line "), "{said:?}");
}

#[test]
fn auto_steps_on_from_an_overwritten_step_as_arm_arithmetic_wraps() {
    let said = typed("!&8748=10:!&874C=-1\r\r");
    assert_says(&said, "\n   10\n    9");
}

#[test]
fn help_gives_an_overwritten_top_s_size_as_genuine_basic_s_subtraction_would() {
    assert_says(
        &typed("!&8670=0\rHELP\r"),
        "The program is -36608 bytes long",
    );
}

#[test]
fn a_command_meeting_a_top_below_its_lines_is_a_bad_program() {
    for keys in [
        "10 REM\r!&8670=PAGE\rDELETE 10,10\r",
        "10 REM\r!&8670=PAGE\r20 REM\r",
        "!&8670=-1\r10 REM\r",
    ] {
        assert_says(&typed(keys), "\nBad program\n");
    }
    assert_says(&typed("!&8670=-1\rTEXTSAVE \"f\"\r"), "\nNo room\n");
}

/// What follows the abort `said` reports, where the place it was reported
/// at lies in BASIC's own module, as an access BASIC makes aborts there.
fn after_abort_in_basic(said: &str) -> Option<&str> {
    let (_, at) = said.split_once("Data transfer aborted at &")?;
    let address = u32::from_str_radix(at.get(..8)?, 16).ok()?;
    let mut kernel = kernel(SLOT_BYTES, b"");
    let basic = tairix_riscos_basic::MODULES[0];
    kernel.os().holds_code_of(basic, address).then(|| &at[8..])
}

#[test]
fn new_and_old_with_page_overwritten_abort_their_access() {
    for command in ["NEW", "OLD"] {
        let said = typed(&format!("!&866C=-1\r{command}\r"));
        assert!(
            after_abort_in_basic(&said).is_some_and(|rest| rest.starts_with('\n')),
            "{said:?}"
        );
    }
}

#[test]
fn a_variable_chain_overwritten_aborts_its_read() {
    let said = run("10 !&8544=&FFFFFFFC:!&8674=-1\n20 FOR I%=1TO5000:NEXT\n30 Q=1\n");
    assert!(
        after_abort_in_basic(&said).is_some_and(|rest| rest.starts_with(" at line 30")),
        "{said:?}"
    );
}

/// Every statement whose waiting record holds a count, an index or an
/// address, each word above the function that rewrites it, through the
/// waiting records, set to each value: an error or the statement's end,
/// never a panic.
#[test]
fn a_waiting_record_a_function_rewrites_is_refused() {
    const FORMS: [&str; 10] = [
        "PRINT LEFT$(\"ab\",@)",
        "MOVE 0,@",
        "SYS \"OS_WriteC\",@",
        "PROCp(@)",
        "A%=INSTR(\"abc\",\"b\",@)",
        "SYS \"OS_WriteC\",65 TO B%(@)",
        "MOUSE X%,Y%,B%(@)",
        "CALL C%,B%(@)",
        "PRINT MID$(\"abc\",2,@)",
        "RESTORE +@",
    ];
    let mut corrupt = 0;
    for form in FORMS {
        for offset in (0..176).step_by(4) {
            for value in ["-1", "10", "&7FFFFFFF", "&FFFFFFFC"] {
                let statement = form.replace('@', &format!("FNz({offset},{value})"));
                let program = format!(
                    "10 DIM B%(70),C% 8:!C%=&E1A0F00E\n20 {statement}\n30 END\n\
                     40 DEF FNz(K%,V%):LOCAL B%:DIM B% LOCAL 0:B%!K%=V%:=1\n50 DEF PROCp(A):ENDPROC\n"
                );
                for dialect in ["BASIC", "BASIC64"] {
                    let said = run_within(dialect, &program, b"", BUDGET).text();
                    corrupt += usize::from(said.starts_with("Corruption of stack"));
                }
            }
        }
    }
    assert!(corrupt > 0, "some rewritten word is a waiting record's own");
}

/// A `FileV` claimant answering `OS_File` 5 for any object with a file of
/// &FFFFFFFF bytes, claimed at the prompt.
const HUGE_FILES: &str = "DIM c% 64:P%=c%:[OPT 0:CMP R0,#5:MOVNE PC,R14:MOV R0,#1:MOV R2,#0:MOV R3,#0:MVN R4,#0:MOV R5,#3:LDMFD R13!,{PC}:]\r\
                          SYS \"OS_Claim\",8,c%,0\r";

#[test]
fn a_file_longer_than_memory_is_no_room_for_it() {
    let said = typed(&format!("{HUGE_FILES}APPEND \"x\"\r"));
    assert_says(&said, "\nNo room\n");
    let said = typed(&format!("{HUGE_FILES}INSTALL \"x\"\r"));
    assert_says(&said, "\nNo room for library\n");
    let said = typed(&format!("{HUGE_FILES}DIM o$(0):o$(0)=\"x\":OVERLAY o$()\r"));
    assert_says(&said, "\nNo room for library\n");
}
