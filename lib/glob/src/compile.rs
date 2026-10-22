//! Compiling pattern text into the components the matcher walks.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::bracket::{self, Bracket};
use crate::{Component, GlobError, Mark, Options, MAX_GROUPS, MAX_GROUP_DEPTH, MAX_TOKENS};

/// The one-to-one lower-case form of `c`: its lower-case mapping when that is
/// a single character, else `c` itself, as `towlower` behaves.
pub(crate) fn fold(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(single), None) => single,
        _ => c,
    }
}

/// An extended-pattern operator, named for how many of its alternatives'
/// matches it accepts in sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupOp {
    /// `?(…)`
    ZeroOrOne,
    /// `*(…)`
    ZeroOrMore,
    /// `+(…)`
    OneOrMore,
    /// `@(…)`
    ExactlyOne,
    /// `!(…)`: any text that none of the alternatives matches.
    Not,
}

impl GroupOp {
    fn from_char(c: char) -> Option<Self> {
        match c {
            '?' => Some(Self::ZeroOrOne),
            '*' => Some(Self::ZeroOrMore),
            '+' => Some(Self::OneOrMore),
            '@' => Some(Self::ExactlyOne),
            '!' => Some(Self::Not),
            _ => None,
        }
    }
}

/// One step of a compiled component. A leaf consumes exactly one character;
/// `leaf` numbers it within its component, for the matcher's per-leaf cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Node {
    /// A character, case-folded when matching ignores case.
    Literal { ch: char, leaf: usize },
    /// `?`
    AnyOne { leaf: usize },
    /// `[...]`
    Bracket { set: Box<Bracket>, leaf: usize },
    /// `*` (a run of them compiles to one).
    AnySequence,
    /// An extended-pattern group; `id` numbers it within its component.
    Group {
        op: GroupOp,
        alts: Vec<Vec<Node>>,
        id: usize,
    },
}

/// Turn pattern text into marks: `\` makes the next character literal.
pub(crate) fn marks_of(pattern: &str) -> Result<Vec<Mark>, GlobError> {
    let mut marks = Vec::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(ch) = chars.next() {
        marks.push(if ch == '\\' {
            Mark {
                ch: chars.next().ok_or(GlobError::DanglingEscape)?,
                literal: true,
            }
        } else {
            Mark { ch, literal: false }
        });
    }
    Ok(marks)
}

/// Compile `marks` into components: one in all, or one per `/`-separated
/// piece in pathname mode (every `/` separates, escaped or not, as a name can
/// never hold one).
pub(crate) fn compile(marks: &[Mark], options: Options) -> Result<Vec<Component>, GlobError> {
    let mut tokens = 0;
    if !options.contains(Options::PATHNAME) {
        return Ok(vec![component(marks, options, &mut tokens)?]);
    }
    marks
        .split(|mark| mark.ch == '/')
        .map(|piece| component(piece, options, &mut tokens))
        .collect()
}

fn component(input: &[Mark], options: Options, tokens: &mut usize) -> Result<Component, GlobError> {
    let mut parser = Parser {
        input,
        at: 0,
        options,
        tokens,
        leaves: 0,
        groups: 0,
        depth: 0,
    };
    let nodes = parser.sequence(false)?;
    // Every mark of an all-literal component became one literal node, so
    // the marks spell the exact name it matches.
    let literal = nodes
        .iter()
        .all(|node| matches!(node, Node::Literal { .. }))
        .then(|| input.iter().map(|mark| mark.ch).collect::<String>());
    let double_star = input.len() == 2 && input.iter().all(|m| !m.literal && m.ch == '*');
    Ok(Component {
        leaves: parser.leaves,
        groups: parser.groups,
        nodes,
        literal,
        double_star,
        options,
    })
}

struct Parser<'a> {
    input: &'a [Mark],
    at: usize,
    options: Options,
    tokens: &'a mut usize,
    leaves: usize,
    groups: usize,
    depth: usize,
}

impl Parser<'_> {
    fn peek(&self, offset: usize) -> Option<Mark> {
        self.input.get(self.at + offset).copied()
    }

    fn count_token(&mut self) -> Result<(), GlobError> {
        *self.tokens += 1;
        if *self.tokens > MAX_TOKENS {
            return Err(GlobError::TooManyTokens);
        }
        Ok(())
    }

    fn next_leaf(&mut self) -> Result<usize, GlobError> {
        self.count_token()?;
        self.leaves += 1;
        Ok(self.leaves - 1)
    }

    /// The operator of an extended group opening at the cursor.
    fn group_op(&self, mark: Mark) -> Option<GroupOp> {
        let opens = self
            .peek(1)
            .is_some_and(|next| !next.literal && next.ch == '(');
        if self.options.contains(Options::EXTENDED) && !mark.literal && opens {
            GroupOp::from_char(mark.ch)
        } else {
            None
        }
    }

    /// Parse nodes up to the end of input, or — inside a group — up to the
    /// `|` or `)` ending the alternative. Plain parentheses nest within an
    /// alternative and are literal there, as bash's pattern scanner counts
    /// them.
    fn sequence(&mut self, in_group: bool) -> Result<Vec<Node>, GlobError> {
        let mut nodes = Vec::new();
        let mut parens = 0usize;
        while let Some(mark) = self.peek(0) {
            if !mark.literal {
                if let Some(op) = self.group_op(mark) {
                    nodes.push(self.group(op)?);
                    continue;
                }
                if in_group {
                    match mark.ch {
                        '|' | ')' if parens == 0 => break,
                        '(' => parens += 1,
                        ')' => parens -= 1,
                        _ => {}
                    }
                }
                match mark.ch {
                    '*' => {
                        self.at += 1;
                        if !matches!(nodes.last(), Some(Node::AnySequence)) {
                            self.count_token()?;
                            nodes.push(Node::AnySequence);
                        }
                        continue;
                    }
                    '?' => {
                        self.at += 1;
                        let leaf = self.next_leaf()?;
                        nodes.push(Node::AnyOne { leaf });
                        continue;
                    }
                    '[' => {
                        let fold = self.options.contains(Options::CASE_INSENSITIVE);
                        if let Some(parsed) = bracket::parse(&self.input[self.at..], fold) {
                            let (set, width) = parsed?;
                            self.at += width;
                            let leaf = self.next_leaf()?;
                            nodes.push(Node::Bracket {
                                set: Box::new(set),
                                leaf,
                            });
                            continue;
                        }
                    }
                    _ => {}
                }
            }
            self.at += 1;
            let leaf = self.next_leaf()?;
            let ch = if self.options.contains(Options::CASE_INSENSITIVE) {
                fold(mark.ch)
            } else {
                mark.ch
            };
            nodes.push(Node::Literal { ch, leaf });
        }
        Ok(nodes)
    }

    fn group(&mut self, op: GroupOp) -> Result<Node, GlobError> {
        if self.depth >= MAX_GROUP_DEPTH {
            return Err(GlobError::TooDeep);
        }
        if self.groups >= MAX_GROUPS {
            return Err(GlobError::TooManyGroups);
        }
        self.count_token()?;
        let id = self.groups;
        self.groups += 1;
        self.at += 2;
        self.depth += 1;
        let mut alts = Vec::new();
        loop {
            alts.push(self.sequence(true)?);
            // An alternative stops only at its unescaped `|` or `)`, or at
            // the end of input.
            match self.peek(0) {
                Some(mark) if mark.ch == '|' => self.at += 1,
                Some(_) => {
                    self.at += 1;
                    break;
                }
                None => return Err(GlobError::UnterminatedGroup),
            }
        }
        self.depth -= 1;
        Ok(Node::Group { op, alts, id })
    }
}
