//! Where a byte of the source lies: its line, by WGSL's line breaks, and its
//! column, in code points.

/// The line and column, each from 1, of byte `at` of `text`: a line ends at
/// each line break the specification names — line feed, vertical tab, form
/// feed, carriage return alone or before a line feed, next line, and the line
/// and paragraph separators.
pub(crate) fn place(text: &str, at: u32) -> (u32, u32) {
    let end = usize::try_from(at).map_or(text.len(), |at| at.min(text.len()));
    let mut line = 1u32;
    let mut column = 1u32;
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if index >= end {
            break;
        }
        let breaks = match ch {
            '\r' => {
                // A carriage return before a line feed ends its line at the
                // line feed.
                chars.peek().is_none_or(|&(_, next)| next != '\n')
            }
            '\n' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}' => true,
            _ => false,
        };
        if breaks {
            line = line.saturating_add(1);
            column = 1;
        } else if ch != '\r' {
            column = column.saturating_add(1);
        }
    }
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::place;

    #[test]
    fn each_line_break_the_specification_names_ends_a_line() {
        let text = "a\nb\u{b}c\u{c}d\r\ne\rf\u{85}g\u{2028}h\u{2029}i";
        let lines: [(char, u32); 9] = [
            ('a', 1),
            ('b', 2),
            ('c', 3),
            ('d', 4),
            ('e', 5),
            ('f', 6),
            ('g', 7),
            ('h', 8),
            ('i', 9),
        ];
        for (ch, line) in lines {
            let at = text.find(ch).expect("present");
            assert_eq!(
                place(text, u32::try_from(at).expect("small")),
                (line, 1),
                "{ch}"
            );
        }
    }

    #[test]
    fn a_column_counts_code_points() {
        let text = "αβ γ";
        let at = text.find('γ').expect("present");
        assert_eq!(place(text, u32::try_from(at).expect("small")), (1, 4));
        assert_eq!(place(text, 1000), (1, 5), "past the end, at the end");
    }
}
