//! The drivers' modules' commands: each one's syntax message names it, so a
//! syntax error reads as RISC OS's do.

mod common;

use tairix_riscos_kernel::os::{Fail, Regs};
use tairix_riscos_kernel::swi::os::CLI;
use tairix_riscos_kernel::swi::X;

#[test]
fn every_command_s_syntax_begins_with_its_own_name() {
    for module in tairix_riscos_vdu::MODULES {
        for command in module.commands {
            if let Some(syntax) = command.syntax {
                assert!(
                    syntax.starts_with(&format!("Syntax: *{}", command.name)),
                    "{}'s {syntax:?}",
                    command.name
                );
            }
        }
    }
}

#[test]
fn a_command_given_too_much_is_refused_with_its_syntax() {
    let mut session = common::drivers(b"");
    let os = &mut session.kernel.os();
    os.write_bytes(0x9000, b"SNew x\0").expect("written");
    let mut regs = Regs::new([0x9000, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let Err(Fail::Error(block)) = os.perform(X | CLI, &mut regs) else {
        panic!("refused");
    };
    assert_eq!(os.read_word(block).expect("its number"), 0xDC);
    let text = os.read_text(block + 4, 256).expect("its text");
    assert_eq!(text, b"Syntax: *SNew");
}
