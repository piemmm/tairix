//! Alias analysis: no function may reach one memory through two root
//! identifiers where either access writes. Each function's reads and
//! writes of module-scope variables and pointer parameters are gathered
//! callees first, and every call site is held to them.

use alloc::vec::Vec;
use core::fmt::Write as _;

use super::call::CallSite;
use super::{Checker, MemoryAccess, Root, NONE};
use crate::diag::{error, push, reserve, Code, Outcome, Stop};
use crate::syntax::ExprKind;
use crate::types::Ty;

/// What one function, with every function it calls, reads and writes:
/// module-scope variables by declaration, pointer parameters by place —
/// each list ascending.
#[derive(Default)]
struct Effects {
    reads: Vec<u32>,
    writes: Vec<u32>,
    pointer_reads: Vec<u32>,
    pointer_writes: Vec<u32>,
}

impl Effects {
    fn note(&mut self, root: Root, write: bool) -> Outcome<()> {
        match (root, write) {
            (Root::Global(decl), false) => push(&mut self.reads, decl),
            (Root::Global(decl), true) => push(&mut self.writes, decl),
            (Root::Param(place), false) => push(&mut self.pointer_reads, place),
            (Root::Param(place), true) => push(&mut self.pointer_writes, place),
            (Root::Local(_), _) => Ok(()),
        }
    }

    fn settle(&mut self) {
        for list in [
            &mut self.reads,
            &mut self.writes,
            &mut self.pointer_reads,
            &mut self.pointer_writes,
        ] {
            list.sort_unstable();
            list.dedup();
        }
    }
}

fn holds(list: &[u32], item: u32) -> bool {
    list.binary_search(&item).is_ok()
}

impl Checker<'_> {
    /// Hold every call site to the alias rules, callees first.
    pub(crate) fn alias_analysis(&mut self) -> Outcome<()> {
        let mut accesses: Vec<MemoryAccess> = Vec::new();
        reserve(&mut accesses, self.accesses.len())?;
        accesses.extend_from_slice(&self.accesses);
        accesses.sort_unstable();
        accesses.dedup();
        let mut sites: Vec<CallSite> = Vec::new();
        reserve(&mut sites, self.call_sites.len())?;
        sites.extend_from_slice(&self.call_sites);
        sites.sort_unstable_by_key(|site| site.caller);
        let count = self.tree.decls.len();
        let mut position: Vec<u32> = Vec::new();
        reserve(&mut position, count)?;
        position.resize(count, NONE);
        for (at, &function) in self.fn_order.iter().enumerate() {
            position[function as usize] = u32::try_from(at).map_err(|_| Stop::OutOfMemory)?;
        }
        let mut effects: Vec<Effects> = Vec::new();
        reserve(&mut effects, self.fn_order.len())?;
        let mut pointers: Vec<(Root, u32)> = Vec::new();
        for at in 0..self.fn_order.len() {
            let function = self.fn_order[at];
            let mut own = Effects::default();
            let first = accesses.partition_point(|a| a.function < function);
            for access in accesses[first..]
                .iter()
                .take_while(|a| a.function == function)
            {
                own.note(access.root, access.write)?;
            }
            let first = sites.partition_point(|s| s.caller < function);
            for site in sites[first..].iter().take_while(|s| s.caller == function) {
                let callee = position
                    .get(site.callee as usize)
                    .copied()
                    .filter(|&p| p != NONE)
                    .ok_or(Stop::Internal)?;
                let callee_effects = effects.get(callee as usize).ok_or(Stop::Internal)?;
                self.call_site(site, callee_effects, &mut own, &mut pointers)?;
            }
            own.settle();
            effects.push(own);
        }
        Ok(())
    }

    /// One call site: its pointer arguments held to what the callee does
    /// through them, and the callee's effects made the caller's.
    fn call_site(
        &self,
        site: &CallSite,
        called: &Effects,
        own: &mut Effects,
        pointers: &mut Vec<(Root, u32)>,
    ) -> Outcome<()> {
        let Some(ExprKind::Call(_, args)) = self.tree.expr(site.call).map(|e| e.kind) else {
            return Err(Stop::Internal);
        };
        pointers.clear();
        for (place, &arg) in self.tree.expr_list(args).iter().enumerate() {
            if !matches!(self.types.get(self.info(arg).converted), Ty::Pointer { .. }) {
                continue;
            }
            let Some(root) = self.root_of(arg) else {
                continue;
            };
            let place = u32::try_from(place).map_err(|_| Stop::OutOfMemory)?;
            push(pointers, (root, place))?;
            let writes = holds(&called.pointer_writes, place);
            if writes {
                own.note(root, true)?;
            }
            if holds(&called.pointer_reads, place) {
                own.note(root, false)?;
            }
            if let Root::Global(decl) = root {
                let through_write =
                    writes && (holds(&called.reads, decl) || holds(&called.writes, decl));
                let through_read =
                    holds(&called.pointer_reads, place) && holds(&called.writes, decl);
                if through_write || through_read {
                    return Err(error(Code::Pointer, self.span_of(arg), |m| {
                        m.write_str("this pointer reaches a module-scope variable the function also reaches, and one of them writes")
                    }));
                }
            }
        }
        pointers.sort_unstable();
        for pair in pointers.windows(2) {
            let ((a, place_a), (b, place_b)) = (pair[0], pair[1]);
            let written =
                holds(&called.pointer_writes, place_a) || holds(&called.pointer_writes, place_b);
            if a == b && written {
                let args = self.tree.expr_list(args);
                let span = args
                    .get(place_b as usize)
                    .map_or_else(|| self.span_of(site.call), |&e| self.span_of(e));
                return Err(error(Code::Pointer, span, |m| {
                    m.write_str("two pointer arguments reach one memory, and the function writes through one")
                }));
            }
        }
        for list in [
            (&called.reads, &mut own.reads),
            (&called.writes, &mut own.writes),
        ] {
            reserve(list.1, list.0.len())?;
            list.1.extend_from_slice(list.0);
        }
        Ok(())
    }
}
