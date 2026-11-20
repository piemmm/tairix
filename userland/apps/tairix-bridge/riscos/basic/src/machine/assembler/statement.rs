//! One assembler statement parsed: empty, a comment, a directive or an
//! instruction, then its end.

use super::mnemonic::{matched, Form, Word};
use super::parse::{Item, Parse, Parsed, Parser};
use crate::messages::Error;
use crate::token;

/// A statement's parse, and whether its mnemonic named an instruction,
/// which BASIC aligns `P%` for before reading its operands.
pub(super) struct Outcome {
    pub(super) instruction: bool,
    pub(super) parsed: Parse<Parsed>,
}

/// The statement at the parser's start.
pub(super) fn statement(p: &mut Parser<'_>) -> Outcome {
    match p.spaces() {
        b':' | b'\r' => {
            let end = p.address();
            return Outcome {
                instruction: false,
                parsed: Ok(Parsed {
                    item: Item::Nothing,
                    end,
                }),
            };
        }
        b';' | b'\\' | token::REM => {
            let end = p.comment();
            return Outcome {
                instruction: false,
                parsed: Ok(Parsed {
                    item: Item::Nothing,
                    end,
                }),
            };
        }
        _ => {}
    }
    let mut word = Word::of(p.rest(), p.offset());
    if word.starts_with("V") {
        let Some((name, cond)) = super::simd::matched(&mut word) else {
            return Outcome {
                instruction: false,
                parsed: Err(Error::NoSuchMnemonic.into()),
            };
        };
        let parsed = super::simd::instruction(p, &mut word, name, cond)
            .map(Item::Instruction)
            .and_then(|item| {
                Ok(Parsed {
                    item,
                    end: p.end()?,
                })
            });
        return Outcome {
            instruction: true,
            parsed,
        };
    }
    let Some((stem, cond)) = matched(&mut word) else {
        return Outcome {
            instruction: false,
            parsed: Err(Error::NoSuchMnemonic.into()),
        };
    };
    let instruction = !matches!(stem.form, Form::Equ(_) | Form::Align | Form::Opt);
    let parsed = (|| {
        let item = match stem.form {
            Form::Equ(_) | Form::Align | Form::Opt => super::data::directive(p, &word, stem.form)?,
            Form::FpaDyadic(_)
            | Form::FpaMonadic(_)
            | Form::Flt
            | Form::Fix
            | Form::FpaStatus { .. }
            | Form::FpaControl { .. }
            | Form::FpaCompare(_)
            | Form::FpaTransfer(_)
            | Form::FpaMultiple(_)
            | Form::Coprocessor(_)
            | Form::CoRegister(_)
            | Form::CoRegisters(_)
            | Form::CoTransfer(_) => {
                Item::Instruction(super::fpa::instruction(p, &mut word, stem.form, cond)?)
            }
            form => Item::Instruction(super::core::instruction(p, &mut word, form, cond)?),
        };
        let end = p.end()?;
        Ok(Parsed { item, end })
    })();
    Outcome {
        instruction,
        parsed,
    }
}
