//! `OS_SubstituteArgs`: a template's `%0`–`%9` and `%*0`–`%*9` replaced by a
//! command line's arguments, as aliases and Obey files take theirs.

use alloc::vec::Vec;

use crate::{byte, is_terminator, Error};

/// The arguments a template may name.
const ARGUMENTS: usize = 10;

/// Where each argument lies in the argument list.
struct Split {
    /// Each argument's start and end, then the rest of the line after the
    /// last; a missing one is empty at the end.
    ranges: [(usize, usize); ARGUMENTS + 1],
    /// Where the list ends: its terminator's offset.
    end: usize,
    /// The byte that ends it.
    terminator: u8,
}

/// `args` split as RISC OS splits it: at spaces, an argument opening with `"`
/// running to its closing quote, `""` within it skipped, quotes kept. A quote
/// left open takes the CR, LF or NUL it meets as its own last byte, and the
/// list goes on after it: only a terminator between arguments ends it.
fn split(args: &[u8]) -> Split {
    let mut ranges = [(0, 0); ARGUMENTS + 1];
    let mut found = 0;
    let mut at = 0;
    loop {
        at += args[at..].iter().take_while(|&&b| b == b' ').count();
        if is_terminator(byte(args, at)) {
            break;
        }
        let start = at;
        if args[at] == b'"' {
            at += 1;
            while at < args.len() {
                let b = args[at];
                at += 1;
                if b == b'"' {
                    if byte(args, at) != b'"' {
                        break;
                    }
                    at += 1;
                } else if is_terminator(b) {
                    break;
                }
            }
        } else {
            at += args[at..]
                .iter()
                .take_while(|&&b| b != b' ' && !is_terminator(b))
                .count();
        }
        if let Some(range) = ranges.get_mut(found) {
            *range = (start, at);
        }
        found += 1;
    }
    for range in ranges.iter_mut().skip(found) {
        *range = (at, at);
    }
    // Past the tenth, the rest of the line.
    if found > ARGUMENTS {
        ranges[ARGUMENTS].1 = at;
    }
    Split {
        ranges,
        end: at,
        terminator: byte(args, at),
    }
}

/// `OS_SubstituteArgs32`: `template` with its references to `args` replaced,
/// then, where `append` holds and no `%*n` appeared, the arguments past the
/// highest it named; the result ends with the byte that ended `args`. It
/// leaves a byte of its buffer of `room` spare, as RISC OS 5 does.
///
/// `%n` is the nth argument as written, `%*n` the rest of the line from it,
/// `%%` a `%`; anything else after a `%` is itself.
pub fn substitute(
    args: &[u8],
    template: &[u8],
    append: bool,
    room: usize,
) -> Result<Vec<u8>, Error> {
    let limit = room.checked_sub(1).ok_or(Error::BufferOverflow)?;
    let split = split(args);
    let mut out = Vec::new();
    let mut put = |bytes: &[u8]| -> Result<(), Error> {
        if out.len() + bytes.len() > limit {
            return Err(Error::BufferOverflow);
        }
        out.try_reserve(bytes.len())
            .map_err(|_| Error::SystemHeapFull)?;
        out.extend_from_slice(bytes);
        Ok(())
    };
    // One past the highest argument named, and whether the rest was taken.
    let mut used = 0;
    let mut rest_taken = false;
    let mut at = 0;
    while at < template.len() {
        let literal = template[at..]
            .iter()
            .position(|&b| b == b'%')
            .unwrap_or(template.len() - at);
        put(&template[at..at + literal])?;
        at += literal + 1;
        if at > template.len() {
            break;
        }
        match template.get(at) {
            None => put(b"%")?,
            Some(b'%') => {
                at += 1;
                put(b"%")?;
            }
            Some(&d @ b'0'..=b'9') => {
                at += 1;
                let n = usize::from(d - b'0');
                used = used.max(n + 1);
                let (start, end) = split.ranges[n];
                put(&args[start..end])?;
            }
            Some(b'*') => {
                at += 1;
                match template.get(at) {
                    Some(&d @ b'0'..=b'9') => {
                        at += 1;
                        rest_taken = true;
                        let (start, _) = split.ranges[usize::from(d - b'0')];
                        put(&args[start..split.end])?;
                    }
                    Some(&other) => {
                        at += 1;
                        put(&[b'%', b'*', other])?;
                    }
                    None => put(b"%*")?,
                }
            }
            Some(&other) => {
                at += 1;
                put(&[b'%', other])?;
            }
        }
    }
    if append && !rest_taken {
        let (from, _) = split.ranges[used];
        put(&args[from..split.end])?;
    }
    put(&[split.terminator])?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    fn run(args: &[u8], template: &[u8], append: bool) -> Vec<u8> {
        substitute(args, template, append, 1024).unwrap()
    }

    #[test]
    fn arguments_replace_their_references() {
        assert_eq!(run(b"a b c\r", b"x %1 %0", false), b"x b a\r");
        assert_eq!(
            run(b"a b\r", b"%%%9|", false),
            b"%|\r",
            "a missing argument is empty"
        );
        assert_eq!(run(b"a b\r", b"%10", false), b"b0\r", "one digit is read");
        assert_eq!(run(b"a b\r", b"%x %*x %", false), b"%x %*x %\r");
        assert_eq!(run(b"a\0", b"%*", false), b"%*\0");
    }

    #[test]
    fn the_rest_of_the_line_keeps_its_spacing_and_quotes() {
        assert_eq!(
            run(b"  one  \"two three\"  four \n", b"[%*1]", true),
            b"[\"two three\"  four ]\n"
        );
        assert_eq!(run(b"a\r", b"[%*5]", true), b"[]\r");
    }

    #[test]
    fn a_quoted_argument_keeps_its_quotes_and_spaces() {
        assert_eq!(run(b"\"a b\" c\r", b"<%0><%1>", false), b"<\"a b\"><c>\r");
        assert_eq!(
            run(b"\"a\"\"b\"c\r", b"<%0><%1>", false),
            b"<\"a\"\"b\"><c>\r"
        );
        assert_eq!(run(b"ab\"c d\"\r", b"<%0><%1>", false), b"<ab\"c><d\">\r");
        assert_eq!(
            run(b"\"open\rX Y\r", b"<%0><%1>", false),
            b"<\"open\r><X>\r",
            "an open quote takes the terminator it meets, and the list goes on"
        );
        assert_eq!(
            run(b"\"open", b"<%0>", false),
            b"<\"open>\0",
            "or ends with the text"
        );
    }

    #[test]
    fn unused_arguments_are_appended_from_past_the_highest_named() {
        assert_eq!(run(b"a b c\r", b"foo %0", true), b"foo ab c\r");
        assert_eq!(
            run(b"a b c d\r", b"x%2", true),
            b"xcd\r",
            "the ones below it are gone"
        );
        assert_eq!(
            run(b"a b\r", b"x %*0", true),
            b"x a b\r",
            "the rest taken, nothing appended"
        );
        assert_eq!(run(b"a b\r", b"Cat ", true), b"Cat a b\r");
        assert_eq!(run(b"a b\r", b"Cat ", false), b"Cat \r");
        assert_eq!(
            run(b"0 1 2 3 4 5 6 7 8 9  ten eleven\r", b"%9:", true),
            b"9:ten eleven\r",
            "past the tenth, the rest of the line"
        );
    }

    #[test]
    fn a_result_leaves_a_byte_of_its_room_spare() {
        assert_eq!(
            substitute(b"abc\r", b"%0", false, 4),
            Err(Error::BufferOverflow)
        );
        assert_eq!(
            substitute(b"abc\r", b"%0", false, 5).map(|o| o.len()),
            Ok(4)
        );
        assert_eq!(
            substitute(b"\r", b"", false, 0),
            Err(Error::BufferOverflow),
            "no room even for the terminator"
        );
    }
}
