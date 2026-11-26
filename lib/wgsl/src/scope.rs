//! Names: every identifier token's spelling interned once, by sorting rather
//! than hashing, so no choice of names makes a lookup slow; and the scopes
//! that say what each name denotes where it is read.

use alloc::vec::Vec;

use crate::diag::{push, reserve, Outcome, Span, Stop};
use crate::lex::{Kind, Token};

/// A distinct spelling of an identifier.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct Symbol(pub(crate) u32);

/// No symbol, or no binding.
const NONE: u32 = u32::MAX;

/// Each identifier token's symbol.
pub(crate) struct Symbols {
    /// Each identifier token's start, ascending.
    starts: Vec<u32>,
    /// Each identifier token's symbol, beside its start.
    of: Vec<u32>,
    /// One spelling of each symbol.
    spellings: Vec<Span>,
}

impl Symbols {
    /// Intern the identifier tokens of `tokens`, spelled in `text`.
    pub(crate) fn intern(text: &str, tokens: &[Token]) -> Outcome<Self> {
        let spell = |span: Span| {
            text.get(span.start as usize..span.end as usize)
                .unwrap_or("")
        };
        let mut idents: Vec<Span> = Vec::new();
        for token in tokens.iter().filter(|t| t.kind == Kind::Ident) {
            push(&mut idents, token.span)?;
        }
        let mut order: Vec<u32> =
            collect(0..u32::try_from(idents.len()).map_err(|_| Stop::OutOfMemory)?)?;
        order.sort_unstable_by(|&a, &b| spell(idents[a as usize]).cmp(spell(idents[b as usize])));
        let mut of: Vec<u32> = Vec::new();
        reserve(&mut of, idents.len())?;
        of.resize(idents.len(), NONE);
        let mut spellings = Vec::new();
        let mut previous: Option<&str> = None;
        for &index in &order {
            let span = idents[index as usize];
            let spelled = spell(span);
            if previous != Some(spelled) {
                push(&mut spellings, span)?;
                previous = Some(spelled);
            }
            of[index as usize] =
                u32::try_from(spellings.len() - 1).map_err(|_| Stop::OutOfMemory)?;
        }
        let starts = collect(idents.iter().map(|span| span.start))?;
        Ok(Self {
            starts,
            of,
            spellings,
        })
    }

    /// The symbol of the identifier token at `span`.
    pub(crate) fn of(&self, span: Span) -> Option<Symbol> {
        let at = self.starts.binary_search(&span.start).ok()?;
        self.of.get(at).copied().filter(|&s| s != NONE).map(Symbol)
    }

    /// How many symbols there are.
    pub(crate) fn len(&self) -> usize {
        self.spellings.len()
    }
}

/// What each symbol denotes in a function's scopes: its innermost binding
/// and the depth of the scope that made it, and how to undo each binding
/// when its scope closes.
pub(crate) struct Locals {
    current: Vec<(u32, u32)>,
    undo: Vec<(u32, (u32, u32))>,
    marks: Vec<usize>,
}

impl Locals {
    /// Scopes over `symbols` symbols, none open.
    pub(crate) fn new(symbols: usize) -> Outcome<Self> {
        let mut current = Vec::new();
        reserve(&mut current, symbols)?;
        current.resize(symbols, (NONE, 0));
        Ok(Self {
            current,
            undo: Vec::new(),
            marks: Vec::new(),
        })
    }

    /// What `symbol` is bound to, if anything in an open scope binds it.
    pub(crate) fn get(&self, symbol: Symbol) -> Option<u32> {
        self.current
            .get(symbol.0 as usize)
            .map(|&(binding, _)| binding)
            .filter(|&b| b != NONE)
    }

    /// Whether the innermost scope binds `symbol` itself.
    pub(crate) fn declared_here(&self, symbol: Symbol) -> bool {
        self.current
            .get(symbol.0 as usize)
            .is_some_and(|&(binding, depth)| binding != NONE && depth as usize == self.marks.len())
    }

    /// Bind `symbol` to `binding` in the innermost scope.
    pub(crate) fn bind(&mut self, symbol: Symbol, binding: u32) -> Outcome<()> {
        let depth = u32::try_from(self.marks.len()).map_err(|_| Stop::OutOfMemory)?;
        reserve(&mut self.undo, 1)?;
        let slot = self
            .current
            .get_mut(symbol.0 as usize)
            .ok_or(Stop::Internal)?;
        let previous = core::mem::replace(slot, (binding, depth));
        self.undo.push((symbol.0, previous));
        Ok(())
    }

    /// Open a scope.
    pub(crate) fn open(&mut self) -> Outcome<()> {
        push(&mut self.marks, self.undo.len())
    }

    /// Close the innermost scope, undoing what it bound.
    pub(crate) fn close(&mut self) {
        let mark = self.marks.pop().unwrap_or(0);
        while self.undo.len() > mark {
            if let Some((symbol, previous)) = self.undo.pop() {
                if let Some(slot) = self.current.get_mut(symbol as usize) {
                    *slot = previous;
                }
            }
        }
    }
}

fn collect<T>(items: impl ExactSizeIterator<Item = T>) -> Outcome<Vec<T>> {
    let mut list = Vec::new();
    reserve(&mut list, items.len())?;
    list.extend(items);
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::{Locals, Symbols};
    use crate::lex::tokens;

    #[test]
    fn one_spelling_is_one_symbol_and_scopes_undo_what_they_bind() {
        let text = "a b a c b a";
        let tokens = tokens(text).expect("lexed");
        let symbols = Symbols::intern(text, &tokens).expect("interned");
        assert_eq!(symbols.len(), 3);
        let of: alloc::vec::Vec<u32> = tokens
            .iter()
            .filter_map(|t| symbols.of(t.span))
            .map(|s| s.0)
            .collect();
        assert_eq!(of[0], of[2]);
        assert_eq!(of[0], of[5]);
        assert_eq!(of[1], of[4]);
        assert_ne!(of[0], of[1]);
        let a = symbols.of(tokens[0].span).expect("a");
        let mut locals = Locals::new(symbols.len()).expect("scopes");
        locals.open().expect("open");
        locals.bind(a, 7).expect("bound");
        assert!(locals.declared_here(a));
        locals.open().expect("open");
        assert!(!locals.declared_here(a));
        locals.bind(a, 9).expect("shadowed");
        assert_eq!(locals.get(a), Some(9));
        locals.close();
        assert_eq!(locals.get(a), Some(7));
        locals.close();
        assert_eq!(locals.get(a), None);
    }
}
