//! The assembler's replay: every line of `tests/riscos5/assembler.txt`
//! assembled in its framing by the interpreter, as `cargo xtask riscos
//! --reference` had genuine RISC OS 5's BASIC V and BASIC VI assemble it, and
//! what each laid down, or the error it met, held to what genuine BASIC did
//! (`tests/riscos5/assembled.txt`).

mod common;

use common::run_hosted;
use tairix_fscopy::fixture::Tree;
use tairix_riscos_basic::corpus::assembler::{lines, program, recorded, results};
use tairix_riscos_filing::session::{disc_file, DISC};

const CORPUS: &str = include_str!("riscos5/assembler.txt");
const ANSWERS: &str = include_str!("riscos5/assembled.txt");

#[test]
fn every_line_lays_down_what_genuine_basic_laid_down() {
    let lines = lines(CORPUS).expect("the corpus");
    let answers = recorded(ANSWERS).expect("the answers");
    assert_eq!(lines.len(), answers.len(), "an answer for each line");
    for (line, answer) in lines.iter().zip(&answers) {
        assert_eq!(line, &answer.line, "the answers recorded for this corpus");
    }
    let text = program(&lines, "HostFS:$.Laid");
    let mut differ = Vec::new();
    for (command, vi) in [("BASIC", false), ("BASIC64", true)] {
        let tree = Tree::new()
            .dir(DISC)
            .file(&format!("{DISC}/Asm,fff"), text.as_bytes());
        run_hosted(&tree, &format!("{command} -quit HostFS:$.Asm"));
        let written = disc_file(&tree, "Laid").unwrap_or_default();
        let ours = results(&written, lines.len()).expect("a result for each line");
        for ((line, answer), ours) in lines.iter().zip(&answers).zip(&ours) {
            let genuine = if vi { &answer.vi } else { &answer.v };
            if ours != genuine {
                differ.push(format!(
                    "{command} {}: genuine {genuine:?}, ours {ours:?}",
                    line.spelt()
                ));
            }
        }
    }
    assert!(
        differ.is_empty(),
        "{} of {} lines laid down otherwise:\n{}",
        differ.len(),
        2 * lines.len(),
        differ.join("\n")
    );
}
