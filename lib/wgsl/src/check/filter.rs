//! Diagnostic filters: the global `diagnostic` directives and the
//! `@diagnostic` attributes, each a severity for a triggering rule over
//! its affected range, and the nearest one enclosing a location.

use core::fmt::Write as _;

use super::Checker;
use crate::diag::{error, push, Code, Outcome, Severity, Span, Stop};
use crate::syntax::{AttrArgs, Control, DeclKind, DirectiveKind, Run, StmtKind};

/// A triggering rule a filter may name.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Rule {
    DerivativeUniformity,
    SubgroupUniformity,
}

/// A severity a filter gives: a diagnostic's, or none.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Level {
    Off,
    Info,
    Warning,
    Error,
}

impl Level {
    /// The diagnostic severity, where the level reports one.
    pub(crate) const fn severity(self) -> Option<Severity> {
        match self {
            Self::Off => None,
            Self::Info => Some(Severity::Info),
            Self::Warning => Some(Severity::Warning),
            Self::Error => Some(Severity::Error),
        }
    }
}

/// One filter: its affected range, rule and level.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct Filter {
    pub(crate) range: (u32, u32),
    pub(crate) rule: Rule,
    pub(crate) level: Level,
}

impl<'a> Checker<'a> {
    /// Every filter of the module, refused where two on one range name one
    /// rule at different levels, or one form names a rule twice.
    pub(crate) fn filters(&mut self) -> Outcome<()> {
        let tree: &'a crate::syntax::Tree = self.tree;
        let whole = (
            0,
            u32::try_from(self.text.len()).map_err(|_| Stop::OutOfMemory)?,
        );
        for directive in &tree.directives {
            if let DirectiveKind::Diagnostic(control) = directive.kind {
                self.filter(control, whole)?;
            }
        }
        for decl in &tree.decls {
            if let DeclKind::Fn(_) = decl.kind {
                self.attribute_filters(decl.attrs, decl.span)?;
            }
        }
        for stmt in &tree.stmts {
            self.attribute_filters(stmt.attrs, stmt.span)?;
            match stmt.kind {
                StmtKind::Switch {
                    body_attrs,
                    clauses,
                    ..
                } => {
                    let clause_list = tree.clause_list(clauses);
                    let span = match (clause_list.first(), clause_list.last()) {
                        (Some(first), Some(last)) => first.span.to(last.span),
                        _ => stmt.span,
                    };
                    self.attribute_filters(body_attrs, span)?;
                }
                StmtKind::Loop {
                    body_attrs,
                    body,
                    continuing,
                } => {
                    let first = tree
                        .stmt_list(body)
                        .first()
                        .and_then(|&s| tree.stmt(s))
                        .map(|s| s.span);
                    let end = continuing
                        .and_then(|c| tree.continuings.get(c as usize))
                        .map(|c| c.span);
                    let span = match (first, end) {
                        (Some(a), Some(b)) => a.to(b),
                        (Some(a), None) => a,
                        (None, Some(b)) => b,
                        (None, None) => stmt.span,
                    };
                    self.attribute_filters(body_attrs, span)?;
                    if let Some(continuing) =
                        continuing.and_then(|c| tree.continuings.get(c as usize))
                    {
                        self.attribute_filters(continuing.attrs, continuing.span)?;
                    }
                }
                _ => {}
            }
        }
        self.filters.sort_unstable();
        for pair in self.filters.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if a.range == b.range && a.rule == b.rule && a.level != b.level {
                let span = Span::new(b.range.0, b.range.1);
                return Err(error(Code::Directive, span, |m| {
                    m.write_str(
                        "two diagnostic filters give one rule two severities over one range",
                    )
                }));
            }
        }
        self.filters.dedup();
        Ok(())
    }

    /// The `@diagnostic` attributes of `run`, over `span`: each names a
    /// rule its form names no other attribute for.
    fn attribute_filters(&mut self, run: Run, span: Span) -> Outcome<()> {
        let tree: &'a crate::syntax::Tree = self.tree;
        let range = (span.start, span.end);
        let first = self.filters.len();
        for attr in tree.attr_list(run) {
            let AttrArgs::Diagnostic(control) = attr.args else {
                continue;
            };
            let added = self.filter(control, range)?;
            if let Some(rule) = added {
                let repeated = self.filters[first..self.filters.len() - 1]
                    .iter()
                    .any(|f| f.rule == rule);
                if repeated {
                    return Err(error(Code::Attribute, attr.span, |m| {
                        m.write_str("one form's diagnostic attributes name each rule once")
                    }));
                }
            }
        }
        Ok(())
    }

    /// The filter `control` makes over `range`: none for a rule this front
    /// end does not know, a single-token one warned about.
    fn filter(&mut self, control: Control, range: (u32, u32)) -> Outcome<Option<Rule>> {
        let level = match self.spell(control.severity) {
            "off" => Level::Off,
            "info" => Level::Info,
            "warning" => Level::Warning,
            "error" => Level::Error,
            _ => {
                return Err(error(Code::Directive, control.severity, |m| {
                    m.write_str("a severity is error, warning, info or off")
                }))
            }
        };
        let (first, namespaced) = control.rule;
        // A two-token rule is another implementation's to know.
        if namespaced.is_some() {
            return Ok(None);
        }
        let rule = match self.spell(first) {
            "derivative_uniformity" => Rule::DerivativeUniformity,
            "subgroup_uniformity" => Rule::SubgroupUniformity,
            unknown => {
                self.note(Severity::Warning, Code::UnknownRule, first, |m| {
                    m.quote(unknown)?;
                    m.write_str(" is not a diagnostic rule")
                })?;
                return Ok(None);
            }
        };
        push(&mut self.filters, Filter { range, rule, level })?;
        Ok(Some(rule))
    }

    /// The level the nearest filter enclosing `at` gives `rule`: an error
    /// where none does.
    pub(crate) fn level_at(&self, rule: Rule, at: u32) -> Level {
        self.filters
            .iter()
            .filter(|f| f.rule == rule && f.range.0 <= at && at < f.range.1)
            .min_by_key(|f| f.range.1 - f.range.0)
            .map_or(Level::Error, |f| f.level)
    }
}
