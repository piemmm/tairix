//! Inline-assembly invocations read out of Rust source, for the host tests
//! that pin what a port's blocks run and the operands they bind: one lexer,
//! so no port re-derives where a block ends.

use std::collections::BTreeMap;

/// The `name!(…)` invocations in `source` — `asm`, `naked_asm` or
/// `global_asm` — each from its name to its closing parenthesis. A longer
/// macro name ending in `name` is not one, so `asm` never matches the others.
#[must_use]
pub fn invocations<'a>(source: &'a str, name: &str) -> Vec<&'a str> {
    let needle = format!("{name}!(");
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = source[from..].find(&needle).map(|at| from + at) {
        let named = source[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let open = at + needle.len() - 1;
        let Some(close) = closing_paren(&source[open..]) else {
            break;
        };
        if !named {
            found.push(&source[at..=open + close]);
        }
        from = open + close + 1;
    }
    found
}

/// The offset in `text`, which starts at an opening parenthesis, of the
/// parenthesis that closes it, counting none inside a string literal, a raw
/// string, or a comment.
fn closing_paren(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'(' => depth += 1,
            b')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(at);
                }
            }
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                at += text[at..].find('\n').unwrap_or(text.len() - at);
                continue;
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at += text[at..].find("*/").map_or(text.len() - at, |end| end + 2);
                continue;
            }
            b'"' => {
                at = string_end(bytes, at + 1)?;
                continue;
            }
            b'r' if raw_string_hashes(bytes, at).is_some() => {
                at = raw_string_end(text, at)?;
                continue;
            }
            _ => {}
        }
        at += 1;
    }
    None
}

/// The offset just past the closing quote of a plain string whose body
/// starts at `from`.
fn string_end(bytes: &[u8], from: usize) -> Option<usize> {
    let mut at = from;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 2,
            b'"' => return Some(at + 1),
            _ => at += 1,
        }
    }
    None
}

/// How many `#` open the raw string `r` at `at` starts, if it starts one:
/// the `r` follows no identifier character and is followed by `#`s and `"`.
fn raw_string_hashes(bytes: &[u8], at: usize) -> Option<usize> {
    let after_ident = at
        .checked_sub(1)
        .is_some_and(|before| bytes[before].is_ascii_alphanumeric() || bytes[before] == b'_');
    if after_ident {
        return None;
    }
    let hashes = bytes[at + 1..].iter().take_while(|&&b| b == b'#').count();
    (bytes.get(at + 1 + hashes) == Some(&b'"')).then_some(hashes)
}

/// The offset just past the raw string starting at `at`.
fn raw_string_end(text: &str, at: usize) -> Option<usize> {
    let hashes = raw_string_hashes(text.as_bytes(), at)?;
    let body = at + 2 + hashes;
    let close = format!("\"{}", "#".repeat(hashes));
    text[body..]
        .find(&close)
        .map(|end| body + end + close.len())
}

/// The top-level arguments of `invocation`, split at its commas outside any
/// nested parenthesis, literal or comment, each trimmed.
fn arguments(invocation: &str) -> Vec<&str> {
    let Some(open) = invocation.find('(') else {
        return Vec::new();
    };
    let inner = &invocation[open + 1..invocation.len().saturating_sub(1)];
    let bytes = inner.as_bytes();
    let (mut parts, mut start, mut depth, mut at) = (Vec::new(), 0, 0usize, 0);
    while at < bytes.len() {
        match bytes[at] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                parts.push(inner[start..at].trim());
                start = at + 1;
            }
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                at += inner[at..].find('\n').unwrap_or(inner.len() - at);
                continue;
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at += inner[at..]
                    .find("*/")
                    .map_or(inner.len() - at, |end| end + 2);
                continue;
            }
            b'"' => {
                at = string_end(bytes, at + 1).unwrap_or(bytes.len());
                continue;
            }
            b'r' if raw_string_hashes(bytes, at).is_some() => {
                at = raw_string_end(inner, at).unwrap_or(bytes.len());
                continue;
            }
            _ => {}
        }
        at += 1;
    }
    parts.push(inner[start..].trim());
    parts.retain(|part| !part.is_empty());
    parts
}

/// `text` with its comments taken out.
fn without_comments(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let (at, block) = match (rest.find("//"), rest.find("/*")) {
            (None, None) => {
                kept.push_str(rest);
                return kept;
            }
            (Some(line), Some(block)) if block < line => (block, true),
            (Some(line), _) => (line, false),
            (None, Some(block)) => (block, true),
        };
        kept.push_str(&rest[..at]);
        rest = if block {
            rest[at..]
                .find("*/")
                .map_or("", |end| &rest[at + end + 2..])
        } else {
            rest[at..].find('\n').map_or("", |end| &rest[at + end..])
        };
    }
}

/// The `const` and `sym` operands `invocation` binds by name: each to its
/// expression, whitespace-normalised, with `$crate::` read as `crate::`.
#[must_use]
pub fn bindings(invocation: &str) -> BTreeMap<String, String> {
    arguments(invocation)
        .into_iter()
        .filter(|argument| {
            !argument.starts_with('"')
                && !argument.starts_with("r#")
                && !argument.starts_with("r\"")
        })
        .map(without_comments)
        .filter_map(|argument| {
            let (name, value) = argument.split_once('=')?;
            let name = name.trim();
            let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
            let bound = value.starts_with("const ") || value.starts_with("sym ");
            (bound && name.chars().all(|c| c.is_alphanumeric() || c == '_'))
                .then(|| (name.to_string(), value.replace("$crate::", "crate::")))
        })
        .collect()
}

/// The instructions an invocation's template spells, whitespace-normalised,
/// without the registers its operands are pinned to (`in("dx") port`): each
/// literal's escapes decoded, then split at every line break and `;`.
#[must_use]
pub fn template_instructions(invocation: &str) -> Vec<String> {
    let mut instructions = Vec::new();
    for (at, literal) in literals(invocation) {
        if pins_a_register(&invocation[..at]) {
            continue;
        }
        let text = match literal {
            Literal::Plain(body) => unescape(body),
            Literal::Raw(body) => String::from(body),
        };
        instructions.extend(
            text.split(['\n', '\r', ';'])
                .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|instruction| !instruction.is_empty()),
        );
    }
    instructions
}

/// One string literal's body, as written.
enum Literal<'a> {
    Plain(&'a str),
    Raw(&'a str),
}

/// The string literals of `text` outside its comments, each with the offset
/// its token starts at.
fn literals(text: &str) -> Vec<(usize, Literal<'_>)> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                at += text[at..].find('\n').unwrap_or(text.len() - at);
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at += text[at..].find("*/").map_or(text.len() - at, |end| end + 2);
            }
            b'"' => {
                let Some(end) = string_end(bytes, at + 1) else {
                    break;
                };
                found.push((at, Literal::Plain(&text[at + 1..end - 1])));
                at = end;
            }
            b'r' if raw_string_hashes(bytes, at).is_some() => {
                let (Some(hashes), Some(end)) =
                    (raw_string_hashes(bytes, at), raw_string_end(text, at))
                else {
                    break;
                };
                found.push((at, Literal::Raw(&text[at + 2 + hashes..end - 1 - hashes])));
                at = end;
            }
            _ => at += 1,
        }
    }
    found
}

/// Whether the text ahead of a literal makes it a register an operand is
/// pinned to (`in("dx")`), not a line of the template.
fn pins_a_register(ahead: &str) -> bool {
    ahead.trim_end().strip_suffix('(').is_some_and(|head| {
        ["in", "out", "inout", "lateout", "inlateout"]
            .iter()
            .any(|kind| head.ends_with(kind))
    })
}

/// A plain literal's text: a `\`-newline continuation and the whitespace after
/// it dropped, `\n` and `\r` read as line breaks, `\t` and `\0` as spaces, and
/// any other escaped character as itself.
fn unescape(body: &str) -> String {
    let mut text = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            text.push(c);
            continue;
        }
        match chars.next() {
            Some('\n' | '\r') => while chars.next_if(|next| next.is_whitespace()).is_some() {},
            Some('n') => text.push('\n'),
            Some('r') => text.push('\r'),
            Some('t' | '0') => text.push(' '),
            Some(other) => text.push(other),
            None => {}
        }
    }
    text
}

/// The operation `instruction` performs: its first word that is neither a
/// label nor a prefix, or `None` for a line that is only a label.
#[must_use]
pub fn mnemonic(instruction: &str) -> Option<&str> {
    instruction.split(' ').find(|word| {
        !word.ends_with(':') && !["lock", "rep", "repe", "repne", "repz", "repnz"].contains(word)
    })
}

#[cfg(test)]
mod tests {
    use super::{arguments, bindings, invocations, mnemonic, template_instructions};

    #[test]
    fn an_invocation_ends_at_its_own_parenthesis_whatever_its_literals_and_comments_hold() {
        let source = r##"
            core::arch::global_asm!(r#"
                // a lone " and a (
                .global copy
            "#, options(att_syntax));
            core::arch::naked_asm!(
                "xorl %eax, %eax", // the user's own (unbalanced
                "ret",
                /* ) */ smap = const $crate::protection::Protections::SMAP.bits(),
            )
            core::arch::asm!("mov {t}, cr4", t = out(reg) _, options(nostack, preserves_flags));
        "##;
        let global = invocations(source, "global_asm");
        assert_eq!(global.len(), 1);
        assert!(global[0].ends_with("options(att_syntax))"), "{}", global[0]);
        let naked = invocations(source, "naked_asm");
        assert_eq!(naked.len(), 1);
        assert_eq!(
            bindings(naked[0]).get("smap").map(String::as_str),
            Some("const crate::protection::Protections::SMAP.bits()")
        );
        let inline = invocations(source, "asm");
        assert_eq!(inline.len(), 1, "`asm` is no other macro's suffix");
        assert_eq!(template_instructions(inline[0]), ["mov {t}, cr4"]);
        assert_eq!(arguments(inline[0]).len(), 3);
    }

    #[test]
    fn operands_sharing_a_line_are_bound_apart() {
        let block = "asm!(\"{a}{b}\", a = const 1, b = sym crate::B, c = in(reg) 0)";
        let bound = bindings(block);
        assert_eq!(bound.get("a").map(String::as_str), Some("const 1"));
        assert_eq!(bound.get("b").map(String::as_str), Some("sym crate::B"));
        assert!(
            !bound.contains_key("c"),
            "a register operand binds no constant"
        );
    }

    #[test]
    fn every_line_of_a_template_is_an_instruction_however_it_is_spelled() {
        let real_newline = "asm!(\"lfence\n    add {x}, 1\", x = inout(reg) x)";
        assert_eq!(
            template_instructions(real_newline),
            ["lfence", "add {x}, 1"]
        );
        let continued = "asm!(\"fld f0, 0({a})\\n\\\n     fadd.d f0, f0, f0\", a = in(reg) a)";
        assert_eq!(
            template_instructions(continued),
            ["fld f0, 0({a})", "fadd.d f0, f0, f0"]
        );
        let tabbed = "asm!(\"csrw\\tfflags, {t}\", t = in(reg) t)";
        assert_eq!(template_instructions(tabbed), ["csrw fflags, {t}"]);
        let commented = "asm!(\"push rax\", // a lone \" here\n \"pop rax\")";
        assert_eq!(template_instructions(commented), ["push rax", "pop rax"]);
        let raw = "asm!(r#\"mov rax, 1\n add rax, rbx\"#)";
        assert_eq!(template_instructions(raw), ["mov rax, 1", "add rax, rbx"]);
        let quoted = "asm!(\".ascii \\\"xy\\\"\", \"nop\")";
        assert_eq!(template_instructions(quoted), [".ascii \"xy\"", "nop"]);
    }

    #[test]
    fn a_mnemonic_skips_labels_and_prefixes() {
        assert_eq!(mnemonic("2: lock xadd [{p}], {v}"), Some("xadd"));
        assert_eq!(mnemonic("2:"), None, "a label runs nothing");
        assert_eq!(
            template_instructions("asm!(\"in al, dx\", in(\"dx\") port)"),
            ["in al, dx"]
        );
    }
}
