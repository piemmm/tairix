//! The directives: the data `EQU` and `DC` lay down, `ALIGN` and `OPT`.

use tairix_riscos_arm::fpa::Format;

use super::core::start;
use super::mnemonic::{Equ, Form, Word};
use super::parse::{Item, Parse, Parser, Want};
use crate::messages::Error;
use crate::value::Value;

/// What a directive of `form` lays down or sets.
pub(super) fn directive(p: &mut Parser<'_>, word: &Word, form: Form) -> Parse<Item> {
    start(p, word)?;
    Ok(match form {
        Form::Align => Item::Align,
        Form::Opt => Item::Opt(p.int(Want::Expression)?),
        Form::Equ(equ) => data(p, equ)?,
        _ => return Err(Error::NoSuchMnemonic.into()),
    })
}

/// The bytes an `EQU` or `DC` lays down: a number's low bytes, a string's
/// bytes, or a number in a floating-point format, words in the FPA's order.
fn data(p: &mut Parser<'_>, equ: Equ) -> Parse<Item> {
    let item = match equ {
        Equ::Text => Item::Text(p.string()?),
        Equ::ByteOrText => match p.value(Want::Expression)? {
            Value::Str(text) => Item::Text(text),
            value => {
                let number = value.number().ok_or(Error::NeedNumber)?;
                Item::data(&[p.arith.int(number)?.to_le_bytes()[0]])
            }
        },
        Equ::Byte => Item::data(&p.int(Want::Expression)?.to_le_bytes()[..1]),
        Equ::Half => Item::data(&p.int(Want::Expression)?.to_le_bytes()[..2]),
        Equ::Word => Item::data(&p.int(Want::Expression)?.to_le_bytes()),
        Equ::Single | Equ::Double | Equ::Extended => {
            let (format, words) = match equ {
                Equ::Single => (Format::Single, 1),
                Equ::Double => (Format::Double, 2),
                _ => (Format::Extended, 3),
            };
            let number = p.number(Want::Expression)?;
            let stored = p.arith.fpa_words(number, format)?;
            let mut bytes = [0; 12];
            for (chunk, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(stored) {
                *chunk = word.to_le_bytes();
            }
            Item::data(&bytes[..4 * words])
        }
    };
    Ok(item)
}
