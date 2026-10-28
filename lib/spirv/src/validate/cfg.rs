//! Control flow: each function's blocks and branches, its dominator trees,
//! and the structured control-flow rules (SPIR-V 1.6 §2.11).
//!
//! Constructs are found in one walk down the structural dominator tree. That
//! is exact because each place a construct ends or begins — a merge block, a
//! continue target, a case target — is immediately dominated by its header:
//! a merge or continue edge, or the switch's own branch, runs straight from
//! the header to it. A continue construct is required to be its continue
//! target's dominator subtree less its loop merge's — which a valid module's
//! always is, and which is the whole subtree but for a single-block loop,
//! whose merge its continue target dominates — so every construct is a
//! difference of dominator subtrees and membership is a constant-time
//! question.

use alloc::vec::Vec;

use super::dominance::{Dominators, Graph, Work};
use super::scan::{len_u32, Block};
use super::{
    push, refill, reserve, Limit, Refusal, Rule, Validator, What, MAX_MEMBERS, MAX_NESTING, NONE,
};
use crate::enumerant::LoopControl;
use crate::{Id, Instruction, Op, Value, Version};

/// What closes a block.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Exit {
    Branch,
    Conditional,
    Switch,
    Terminate,
}

/// A merge instruction's declaration.
#[derive(Copy, Clone, Debug)]
struct Header {
    kind: Kind,
    merge: u32,
    /// A loop's continue target; [`NONE`] otherwise.
    cont: u32,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Kind {
    Selection,
    Switch,
    Loop,
    Continue,
    Case,
}

/// One construct of the nesting.
#[derive(Copy, Clone, Debug)]
struct Construct {
    kind: Kind,
    /// The block it starts at.
    header: u32,
    /// The merge block a selection, switch or loop declares; a case's
    /// switch's; a continue construct's loop's.
    merge: u32,
    /// A loop's continue target; a continue construct's loop header; a
    /// case's switch header.
    other: u32,
    /// A continue construct's back-edge block.
    back: u32,
    parent: u32,
    depth: u32,
    /// An ancestor on a skew-binary ladder (Myers, An Applicative
    /// Random-Access Stack, 1983): any ancestor is reached in a logarithmic
    /// number of steps.
    jump: u32,
    /// The nearest loop, switch and continue construct containing it, itself
    /// included.
    near_loop: u32,
    near_switch: u32,
    near_continue: u32,
}

/// What the rest of the function's checks need of its control flow.
#[derive(Debug, Default)]
pub(crate) struct Cfg {
    /// Each label and its block, by ascending label.
    labels: Vec<(Id, u32)>,
    /// Dominance over branch edges, which SSA is checked by.
    pub(crate) dom: Dominators,
    /// Each block's predecessors over branch edges.
    pub(crate) predecessors: Graph,
}

impl Cfg {
    /// The block `label` opens, if it is one of the function's.
    pub(crate) fn block(&self, label: Id) -> Option<u32> {
        find(&self.labels, label)
    }
}

fn find(labels: &[(Id, u32)], label: Id) -> Option<u32> {
    labels
        .binary_search_by_key(&label, |&(l, _)| l)
        .ok()
        .and_then(|at| labels.get(at))
        .map(|&(_, block)| block)
}

/// Every table the control-flow checks fill, kept from one function to the
/// next: a module of many small functions allocates once, not per function.
#[derive(Debug, Default)]
pub(crate) struct Scratch {
    /// The last function's answer to its body's checks.
    pub(crate) cfg: Cfg,
    /// An `OpPhi`'s parents.
    pub(crate) parents: Vec<u32>,
    parts: Parts,
    structure: Structure,
    work: Work,
}

/// A function's own tables while its structure is checked.
#[derive(Debug, Default)]
struct Parts {
    exits: Vec<Exit>,
    headers: Vec<Option<Header>>,
    merge_of: Vec<u32>,
    cont_of: Vec<u32>,
    case_of: Vec<u32>,
    back_of: Vec<u32>,
    edges: Vec<(u32, u32)>,
    literals: Vec<u64>,
    branch: Graph,
}

/// The structured graph, its trees and the walks over them.
#[derive(Debug, Default)]
struct Structure {
    structural: Vec<(u32, u32)>,
    graph: Graph,
    reverse: Graph,
    dominance: Dominators,
    post: Graph,
    post_predecessors: Graph,
    post_dominance: Dominators,
    walk: Walk,
    back_edges: Vec<(u32, u32)>,
    constructs: Vec<Construct>,
    state: Vec<u32>,
    falls: Falls,
}

/// A depth-first walk's marks and stack.
#[derive(Debug, Default)]
struct Walk {
    on_stack: Vec<bool>,
    seen: Vec<bool>,
    stack: Vec<(u32, usize)>,
}

/// Each fall from one case to another, and what checking their order takes.
#[derive(Debug, Default)]
struct Falls {
    /// Each fall's switch, the case it leaves and the case it enters.
    edges: Vec<(u32, u32, u32)>,
    into: Vec<(u32, u32)>,
    positions: Vec<(u32, u32)>,
}

/// The function's state while its structure is checked.
struct Flow<'v, 'm> {
    v: &'v Validator<'m>,
    blocks: &'v [Block],
    labels: Vec<(Id, u32)>,
    exits: Vec<Exit>,
    headers: Vec<Option<Header>>,
    /// Whose merge block, continue target, case target and back-edge block
    /// each block is.
    merge_of: Vec<u32>,
    cont_of: Vec<u32>,
    case_of: Vec<u32>,
    back_of: Vec<u32>,
    /// The branch edges, as collected and then packed.
    edges: Vec<(u32, u32)>,
    literals: Vec<u64>,
    branch: Graph,
}

impl Validator<'_> {
    /// Check function `function`'s control flow in `scratch`, whose `cfg` is
    /// then what its body's checks need.
    pub(crate) fn cfg(&self, function: usize, scratch: &mut Scratch) -> Result<(), Refusal> {
        let f = self.functions.get(function).ok_or(Rule::Block.module())?;
        let blocks = self
            .blocks
            .get(f.blocks.start as usize..f.blocks.end as usize)
            .unwrap_or(&[]);
        let n = blocks.len();
        let Scratch {
            cfg,
            parts,
            structure,
            work,
            ..
        } = scratch;
        let mut labels = core::mem::take(&mut cfg.labels);
        labels.clear();
        reserve(&mut labels, n)?;
        labels.extend(
            blocks
                .iter()
                .enumerate()
                .map(|(index, block)| (block.label, u32::try_from(index).unwrap_or(NONE))),
        );
        labels.sort_unstable_by_key(|&(label, _)| label);
        let mut flow = Flow::new(self, blocks, labels, parts)?;
        let checked = flow.check(function, cfg, structure, work);
        cfg.labels = flow.put_back(parts);
        checked
    }
}

impl<'v, 'm> Flow<'v, 'm> {
    /// A function's flow, in `parts`' memory.
    fn new(
        v: &'v Validator<'m>,
        blocks: &'v [Block],
        labels: Vec<(Id, u32)>,
        parts: &mut Parts,
    ) -> Result<Self, Refusal> {
        let n = blocks.len();
        let mut exits = core::mem::take(&mut parts.exits);
        exits.clear();
        reserve(&mut exits, n)?;
        let mut headers = core::mem::take(&mut parts.headers);
        headers.clear();
        reserve(&mut headers, n)?;
        let mut owners = [
            &mut parts.merge_of,
            &mut parts.cont_of,
            &mut parts.case_of,
            &mut parts.back_of,
        ]
        .map(core::mem::take);
        for owner in &mut owners {
            refill(owner, n, NONE)?;
        }
        let [merge_of, cont_of, case_of, back_of] = owners;
        let mut edges = core::mem::take(&mut parts.edges);
        edges.clear();
        Ok(Self {
            v,
            blocks,
            labels,
            exits,
            headers,
            merge_of,
            cont_of,
            case_of,
            back_of,
            edges,
            literals: core::mem::take(&mut parts.literals),
            branch: core::mem::take(&mut parts.branch),
        })
    }

    /// Return the tables to `parts`, and the labels.
    fn put_back(self, parts: &mut Parts) -> Vec<(Id, u32)> {
        *parts = Parts {
            exits: self.exits,
            headers: self.headers,
            merge_of: self.merge_of,
            cont_of: self.cont_of,
            case_of: self.case_of,
            back_of: self.back_of,
            edges: self.edges,
            literals: self.literals,
            branch: self.branch,
        };
        self.labels
    }

    /// Read the function's blocks, check its order and structure, and leave
    /// in `cfg` what its body's checks need.
    fn check(
        &mut self,
        function: usize,
        cfg: &mut Cfg,
        structure: &mut Structure,
        work: &mut Work,
    ) -> Result<(), Refusal> {
        let n = self.blocks.len();
        self.read(function)?;
        self.branch
            .rebuild(n, self.edges.iter().copied(), &mut work.next)?;
        self.branch
            .reverse_into(&mut cfg.predecessors, &mut work.next)?;
        cfg.dom.rebuild(&self.branch, &cfg.predecessors, 0, work)?;
        let n32 = len_u32(self.blocks);
        for block in 1..n32 {
            if cfg.dom.reaches(block) && cfg.dom.idom(block).is_some_and(|d| d >= block) {
                return Err(Rule::BlockOrder.at(self.label(block)));
            }
        }
        self.structure(n32, structure, work)
    }
}

impl<'m> Flow<'_, 'm> {
    fn label(&self, block: u32) -> &'m Instruction {
        self.v
            .instruction(self.blocks.get(block as usize).map_or(NONE, |b| b.at))
    }

    fn terminator(&self, block: u32) -> &'m Instruction {
        self.v.instruction(
            self.blocks
                .get(block as usize)
                .map_or(NONE, |b| b.terminator),
        )
    }

    fn header_of(&self, block: u32) -> Option<Header> {
        self.headers.get(block as usize).copied().flatten()
    }

    /// The block `id` labels, refusing an id not one of the function's
    /// labels.
    fn target(&self, id: Id, function: usize) -> Result<u32, Rule> {
        let def = self.v.def(id)?;
        if def.what != What::Label || def.scope as usize != function {
            return Err(Rule::BranchTarget);
        }
        find(&self.labels, id).ok_or(Rule::BranchTarget)
    }

    /// Read every terminator and merge instruction: the branch edges, and
    /// each header's declaration.
    fn read(&mut self, function: usize) -> Result<(), Refusal> {
        let m = self.v.m;
        let version = self.v.enabled.version;
        for block in 0..len_u32(self.blocks) {
            let terminator = self.terminator(block);
            let at = |rule: Rule| rule.at(terminator);
            let operands = m.operands(terminator);
            let label = |position: usize| match operands.get(position).map(|o| m.value(o)) {
                Some(Value::Id(id)) => self.target(id, function),
                _ => Err(Rule::BranchTarget),
            };
            let exit = match terminator.op() {
                Op::BRANCH => {
                    let target = label(0).map_err(at)?;
                    self.branch_to(block, target).map_err(at)?;
                    Exit::Branch
                }
                Op::BRANCH_CONDITIONAL => {
                    let (yes, no) = (label(1).map_err(at)?, label(2).map_err(at)?);
                    if yes == no && version >= (Version { major: 1, minor: 6 }) {
                        return Err(at(Rule::BranchTarget));
                    }
                    self.weights(terminator)?;
                    self.branch_to(block, yes).map_err(at)?;
                    self.branch_to(block, no).map_err(at)?;
                    Exit::Conditional
                }
                Op::SWITCH => {
                    self.switch(block, terminator, function)?;
                    Exit::Switch
                }
                _ => Exit::Terminate,
            };
            self.exits.push(exit);
            let header = self.header(block, exit, function)?;
            self.headers.push(header);
        }
        Ok(())
    }

    fn branch_to(&mut self, from: u32, to: u32) -> Result<(), Rule> {
        if to == 0 {
            return Err(Rule::BranchTarget);
        }
        push(&mut self.edges, (from, to)).map_err(|r| r.rule)
    }

    /// Branch weights: none, or two not both zero whose sum fits a word.
    fn weights(&self, terminator: &Instruction) -> Result<(), Refusal> {
        let m = self.v.m;
        let operands = m.operands(terminator).get(3..).unwrap_or(&[]);
        let weight = |o| match m.value(o) {
            Value::Word(w) => Some(w),
            _ => None,
        };
        let ok = match operands {
            [] => true,
            [a, b] => match (weight(a), weight(b)) {
                (Some(a), Some(b)) => (a != 0 || b != 0) && a.checked_add(b).is_some(),
                _ => false,
            },
            _ => false,
        };
        if ok {
            Ok(())
        } else {
            Err(Rule::OperandValue(3).at(terminator))
        }
    }

    /// An `OpSwitch`'s edges: its targets in operand order, then its default,
    /// the literals distinct.
    fn switch(
        &mut self,
        block: u32,
        terminator: &Instruction,
        function: usize,
    ) -> Result<(), Refusal> {
        let m = self.v.m;
        let at = |rule: Rule| rule.at(terminator);
        let operands = m.operands(terminator);
        let targets = operands.get(2..).unwrap_or(&[]);
        if targets.len() / 2 > MAX_MEMBERS {
            return Err(at(Rule::Limit(Limit::Members)));
        }
        self.literals.clear();
        reserve(&mut self.literals, targets.len() / 2)?;
        for pair in targets.chunks(2) {
            let [literal, label] = pair else {
                return Err(at(Rule::BranchTarget));
            };
            let (Value::Number(literal), Value::Id(label)) = (m.value(literal), m.value(label))
            else {
                return Err(at(Rule::BranchTarget));
            };
            self.literals.push(literal);
            let target = self.target(label, function).map_err(at)?;
            self.branch_to(block, target).map_err(at)?;
        }
        self.literals.sort_unstable();
        if self.literals.windows(2).any(|w| w[0] == w[1]) {
            return Err(at(Rule::OperandValue(2)));
        }
        let Some(Value::Id(default)) = operands.get(1).map(|o| m.value(o)) else {
            return Err(at(Rule::BranchTarget));
        };
        let default = self.target(default, function).map_err(at)?;
        self.branch_to(block, default).map_err(at)
    }

    /// The header `block`'s merge instruction declares, checked against its
    /// terminator.
    fn header(
        &mut self,
        block: u32,
        exit: Exit,
        function: usize,
    ) -> Result<Option<Header>, Refusal> {
        let Some(b) = self.blocks.get(block as usize) else {
            return Ok(None);
        };
        if b.merge == NONE {
            return Ok(None);
        }
        let m = self.v.m;
        let merge = self.v.instruction(b.merge);
        let at = |rule: Rule| rule.at(merge);
        let operands = m.operands(merge);
        let label = |position: usize| match operands.get(position).map(|o| m.value(o)) {
            Some(Value::Id(id)) => self.target(id, function),
            _ => Err(Rule::BranchTarget),
        };
        let header = match (merge.op(), exit) {
            (Op::SELECTION_MERGE, Exit::Conditional) => Header {
                kind: Kind::Selection,
                merge: label(0).map_err(at)?,
                cont: NONE,
            },
            (Op::SELECTION_MERGE, Exit::Switch) => Header {
                kind: Kind::Switch,
                merge: label(0).map_err(at)?,
                cont: NONE,
            },
            (Op::LOOP_MERGE, Exit::Branch | Exit::Conditional) => {
                self.loop_control(merge)?;
                Header {
                    kind: Kind::Loop,
                    merge: label(0).map_err(at)?,
                    cont: label(1).map_err(at)?,
                }
            }
            _ => return Err(at(Rule::Merge)),
        };
        mark(&mut self.merge_of, header.merge, block).map_err(|()| at(Rule::MergeBlock))?;
        if header.kind == Kind::Loop {
            if header.cont == header.merge {
                return Err(at(Rule::ContinueTarget));
            }
            mark(&mut self.cont_of, header.cont, block).map_err(|()| at(Rule::ContinueTarget))?;
        }
        Ok(Some(header))
    }

    /// A loop's control hints: `DontUnroll` with none that unrolls, and an
    /// iteration multiple above zero.
    fn loop_control(&self, merge: &Instruction) -> Result<(), Refusal> {
        let m = self.v.m;
        let operands = m.operands(merge);
        let mask = match operands.get(2).map(|o| m.value(o)) {
            Some(Value::Word(mask)) => LoopControl(mask),
            _ => return Err(Rule::Merge.at(merge)),
        };
        let has = |bit: LoopControl| mask.0 & bit.0 != 0;
        let unrolls = has(LoopControl::UNROLL)
            || has(LoopControl::PEEL_COUNT)
            || has(LoopControl::PARTIAL_COUNT);
        if has(LoopControl::DONT_UNROLL) && unrolls {
            return Err(Rule::OperandValue(2).at(merge));
        }
        if has(LoopControl::ITERATION_MULTIPLE) {
            // Parameters follow in ascending bit order.
            let lower = [
                LoopControl::DEPENDENCY_LENGTH,
                LoopControl::MIN_ITERATIONS,
                LoopControl::MAX_ITERATIONS,
            ]
            .into_iter()
            .filter(|&bit| has(bit))
            .count();
            let multiple = match operands.get(3 + lower).map(|o| m.value(o)) {
                Some(Value::Word(value)) => value,
                _ => 0,
            };
            if multiple == 0 {
                return Err(Rule::OperandValue(2).at(merge));
            }
        }
        Ok(())
    }

    /// Whether `block` is any header's merge block or continue target.
    fn declared(&self, block: u32) -> bool {
        self.merge_of
            .get(block as usize)
            .is_some_and(|&h| h != NONE)
            || self.cont_of.get(block as usize).is_some_and(|&h| h != NONE)
    }

    /// The `index`th structured successor of `block`: its branch targets,
    /// then its merge block and continue target; and whether it is a branch.
    fn structured(&self, block: u32, index: usize) -> Option<(u32, bool)> {
        let branches = self.branch.successors(block);
        if let Some(&to) = branches.get(index) {
            return Some((to, true));
        }
        let h = self.header_of(block)?;
        match index - branches.len() {
            0 => Some((h.merge, false)),
            1 if h.cont != NONE => Some((h.cont, false)),
            _ => None,
        }
    }

    /// Every structured control-flow rule.
    fn structure(&mut self, n: u32, s: &mut Structure, work: &mut Work) -> Result<(), Refusal> {
        self.selections_structured(n)?;
        let n_usize = n as usize;
        let Structure {
            structural,
            graph,
            reverse,
            dominance,
            post,
            post_predecessors,
            post_dominance,
            walk,
            back_edges,
            constructs,
            state,
            falls,
        } = s;
        structural.clear();
        reserve(structural, self.edges.len() + 2 * n_usize)?;
        structural.extend_from_slice(&self.edges);
        for block in 0..n {
            if let Some(h) = self.header_of(block) {
                structural.push((block, h.merge));
                if h.cont != NONE {
                    structural.push((block, h.cont));
                }
            }
        }
        graph.rebuild(n_usize, structural.iter().copied(), &mut work.next)?;
        graph.reverse_into(reverse, &mut work.next)?;
        dominance.rebuild(graph, reverse, 0, work)?;
        // Post-dominance over the structured graph, from a virtual exit every
        // function termination reaches.
        let exit = n;
        let terminating = (0..n).filter(|&b| self.exits.get(b as usize) == Some(&Exit::Terminate));
        post.rebuild(
            n_usize + 1,
            structural
                .iter()
                .map(|&(from, to)| (to, from))
                .chain(terminating.clone().map(|b| (exit, b))),
            &mut work.next,
        )?;
        post_predecessors.rebuild(
            n_usize + 1,
            structural
                .iter()
                .copied()
                .chain(terminating.map(|b| (b, exit))),
            &mut work.next,
        )?;
        post_dominance.rebuild(post, post_predecessors, exit, work)?;
        for block in 0..n {
            if dominance.reaches(block) && !post_dominance.reaches(block) {
                return Err(Rule::NoExit.at(self.label(block)));
            }
        }
        self.declarations(n, dominance)?;
        self.back_edges(n, dominance, post_dominance, walk, back_edges)?;
        self.cases(n, dominance)?;
        self.constructs(dominance, post_dominance, constructs, state)?;
        self.exits_and_entries(n, dominance, back_edges, constructs, state, falls)
    }

    /// A switch, or a conditional branch to two blocks neither of which is
    /// a merge block or continue target, has a selection merge.
    fn selections_structured(&self, n: u32) -> Result<(), Refusal> {
        for block in 0..n {
            let header = self.header_of(block);
            let unstructured = match self.exits.get(block as usize) {
                Some(Exit::Switch) => header.is_none(),
                Some(Exit::Conditional) => {
                    let targets = self.branch.successors(block);
                    let (yes, no) = (targets.first().copied(), targets.get(1).copied());
                    let declared = yes.is_some_and(|t| self.declared(t))
                        || no.is_some_and(|t| self.declared(t));
                    yes != no
                        && !declared
                        && !matches!(header, Some(h) if h.kind == Kind::Selection)
                }
                _ => false,
            };
            if unstructured {
                return Err(Rule::Merge.at(self.terminator(block)));
            }
        }
        Ok(())
    }

    /// Each header strictly dominates its merge, and each loop header its
    /// continue target; an unreached header's are unreached too.
    fn declarations(&self, n: u32, dominance: &Dominators) -> Result<(), Refusal> {
        for block in 0..n {
            let Some(h) = self.header_of(block) else {
                continue;
            };
            let merge = self
                .v
                .instruction(self.blocks.get(block as usize).map_or(NONE, |b| b.merge));
            let reached = dominance.reaches(block);
            let ok = if reached {
                dominance.strictly_dominates(block, h.merge)
                    && (h.kind != Kind::Loop || dominance.dominates(block, h.cont))
            } else {
                !dominance.reaches(h.merge) && (h.kind != Kind::Loop || !dominance.reaches(h.cont))
            };
            if !ok {
                return Err(Rule::MergeBlock.at(merge));
            }
        }
        Ok(())
    }

    /// The back edges of a depth-first walk over the structured graph, each
    /// to a loop header that has exactly that one, its continue target
    /// dominating it and post-dominated by it; sorted.
    fn back_edges(
        &mut self,
        n: u32,
        dominance: &Dominators,
        post_dominance: &Dominators,
        walk: &mut Walk,
        found: &mut Vec<(u32, u32)>,
    ) -> Result<(), Refusal> {
        let n_usize = n as usize;
        let Walk {
            on_stack,
            seen,
            stack,
        } = walk;
        refill(on_stack, n_usize, false)?;
        refill(seen, n_usize, false)?;
        found.clear();
        stack.clear();
        reserve(stack, n_usize)?;
        if n > 0 {
            seen[0] = true;
            on_stack[0] = true;
            stack.push((0, 0));
        }
        while let Some(&(node, next)) = stack.last() {
            let Some((to, is_branch)) = self.structured(node, next) else {
                on_stack[node as usize] = false;
                stack.pop();
                continue;
            };
            if let Some(top) = stack.last_mut() {
                top.1 += 1;
            }
            if on_stack[to as usize] {
                if is_branch {
                    push(found, (node, to))?;
                }
            } else if !seen[to as usize] {
                seen[to as usize] = true;
                on_stack[to as usize] = true;
                stack.push((to, 0));
            }
        }
        for &(from, to) in found.iter() {
            let at = Rule::BackEdge.at(self.terminator(from));
            let h = self
                .header_of(to)
                .filter(|h| h.kind == Kind::Loop)
                .ok_or(at)?;
            if !dominance.dominates(to, from) {
                return Err(at);
            }
            mark(&mut self.back_of, to, from).map_err(|()| at)?;
            if !dominance.dominates(h.cont, from) || !post_dominance.dominates(from, h.cont) {
                return Err(Rule::ContinueTarget.at(self.terminator(from)));
            }
        }
        for block in 0..n {
            let loop_header = self.header_of(block).is_some_and(|h| h.kind == Kind::Loop);
            if loop_header
                && dominance.reaches(block)
                && self.back_of.get(block as usize) == Some(&NONE)
            {
                return Err(Rule::BackEdge.at(self.label(block)));
            }
        }
        found.sort_unstable();
        Ok(())
    }

    /// Each case target is a target of one switch, which dominates it unless
    /// the switch's merge does.
    fn cases(&mut self, n: u32, dominance: &Dominators) -> Result<(), Refusal> {
        for block in 0..n {
            let Some(h) = self.header_of(block).filter(|h| h.kind == Kind::Switch) else {
                continue;
            };
            if !dominance.reaches(block) {
                continue;
            }
            let at = Rule::Fallthrough.at(self.terminator(block));
            for index in 0..self.branch.successors(block).len() {
                let Some(&target) = self.branch.successors(block).get(index) else {
                    continue;
                };
                if target == h.merge {
                    continue;
                }
                match self.case_of.get_mut(target as usize) {
                    Some(slot) if *slot == NONE || *slot == block => *slot = block,
                    _ => return Err(at),
                }
                if !dominance.dominates(h.merge, target) && !dominance.dominates(block, target) {
                    return Err(at);
                }
            }
        }
        Ok(())
    }

    /// Each structurally reached block's innermost construct, found in one
    /// walk down the structural dominator tree.
    fn constructs(
        &self,
        dominance: &Dominators,
        post_dominance: &Dominators,
        constructs: &mut Vec<Construct>,
        state: &mut Vec<u32>,
    ) -> Result<(), Refusal> {
        constructs.clear();
        refill(state, self.blocks.len(), NONE)?;
        for &x in dominance.preorder() {
            let mut current = dominance.idom(x).map_or(NONE, |p| state[p as usize]);
            while let Some(c) = constructs.get(current as usize) {
                if contains(dominance, c, x) {
                    break;
                }
                current = c.parent;
            }
            let label = self.label(x);
            let case = self.case_of.get(x as usize).copied().unwrap_or(NONE);
            if let Some(h) = self.header_of(case) {
                if !dominance.dominates(h.merge, x) {
                    open(
                        constructs,
                        &mut current,
                        label,
                        (Kind::Case, x, h.merge, case, NONE),
                    )?;
                }
            }
            let looped = self.cont_of.get(x as usize).copied().unwrap_or(NONE);
            if let Some(h) = self.header_of(looped) {
                let back = self.back_of.get(looped as usize).copied().unwrap_or(NONE);
                let enclosing = constructs
                    .get(current as usize)
                    .map_or(NONE, |c| c.near_continue);
                open(
                    constructs,
                    &mut current,
                    label,
                    (Kind::Continue, x, h.merge, looped, back),
                )?;
                // The enclosing continue construct's back-edge block
                // post-dominates this one's, and so, through it, every block
                // this one holds.
                if let Some(outer) = constructs.get(enclosing as usize) {
                    if back != NONE
                        && (outer.back == NONE || !post_dominance.dominates(outer.back, back))
                    {
                        return Err(Rule::ContinueTarget.at(label));
                    }
                }
            }
            if let Some(h) = self.header_of(x) {
                // A single-block loop's loop construct is empty: the block is
                // its own continue construct.
                if h.kind != Kind::Loop || h.cont != x {
                    open(
                        constructs,
                        &mut current,
                        label,
                        (h.kind, x, h.merge, h.cont, NONE),
                    )?;
                }
            }
            // A continue construct is its target's subtree less its merge's.
            let up = constructs
                .get(current as usize)
                .map_or(NONE, |c| c.near_continue);
            if let Some(c) = constructs.get(up as usize) {
                if c.back == NONE || !post_dominance.dominates(c.back, x) {
                    return Err(Rule::ContinueTarget.at(label));
                }
            }
            state[x as usize] = current;
        }
        Ok(())
    }

    /// Every branch edge leaving a construct does so as one of the exits the
    /// rules allow, and every one entering a construct does so at its header;
    /// a switch's cases fall through only in its targets' order.
    fn exits_and_entries(
        &self,
        n: u32,
        dominance: &Dominators,
        back_edges: &[(u32, u32)],
        constructs: &[Construct],
        state: &[u32],
        falls: &mut Falls,
    ) -> Result<(), Refusal> {
        let get = |c: u32| constructs.get(c as usize);
        falls.edges.clear();
        for &(a, b) in &self.edges {
            if !dominance.reaches(a) {
                continue;
            }
            let at = Rule::ConstructExit.at(self.terminator(a));
            let (from, to) = (state[a as usize], state[b as usize]);
            let lca = common(constructs, from, to);
            let mut entered = to;
            while let Some(c) = get(entered).filter(|_| entered != lca) {
                if c.header != b {
                    return Err(Rule::ConstructEntry.at(self.terminator(a)));
                }
                entered = c.parent;
            }
            let back_edge = back_edges.binary_search(&(a, b)).is_ok();
            // The constructs left are those on `from`'s chain below `lca`.
            let floor = nesting(constructs, lca);
            let left = |c: u32| nesting(constructs, c) > floor;
            let exits_loop = get(from).is_some_and(|c| left(c.near_loop));
            // Only a loop's header or merge may be the target of an exit from
            // its continue construct; a block is either of at most one loop,
            // so this ends within three steps.
            let mut cont = get(from).map_or(NONE, |c| c.near_continue);
            while let Some(c) = get(cont).filter(|_| left(cont)) {
                if b != c.other && b != c.merge {
                    return Err(at);
                }
                cont = get(c.parent).map_or(NONE, |p| p.near_continue);
            }
            // Every case construct of a switch lies directly inside the
            // switch's own, so a fall-through can only leave the one there.
            let mut falls_through = false;
            let switch = self.case_of.get(b as usize).copied().unwrap_or(NONE);
            if let Some(cases) = state
                .get(switch as usize)
                .and_then(|&s| get(s))
                .map(|s| s.depth + 1)
            {
                let case = ancestor_at(constructs, from, cases);
                if let Some(c) =
                    get(case).filter(|c| c.kind == Kind::Case && c.other == switch && left(case))
                {
                    falls_through = true;
                    push(&mut falls.edges, (switch, c.header, b))?;
                }
            }
            if let Some(looped) = self.cont_of.get(b as usize).copied().filter(|&l| l != NONE) {
                let single = self.header_of(looped).is_some_and(|h| h.cont == looped);
                if !back_edge && !single && !within_loop(constructs, state, from, looped) {
                    return Err(at);
                }
            }
            if from == lca {
                continue;
            }
            let innermost = get(from);
            let near_loop = innermost.and_then(|c| get(c.near_loop));
            let near_switch = innermost.and_then(|c| get(c.near_switch));
            let allowed = innermost.is_some_and(|c| c.kind == Kind::Selection && b == c.merge)
                || near_loop.is_some_and(|l| b == l.merge || b == l.other)
                || back_edge
                || self.back_to_merge(back_edges, a, b)
                || falls_through
                || (!exits_loop && near_switch.is_some_and(|s| b == s.merge));
            if !allowed {
                return Err(at);
            }
        }
        self.fallthrough(n, falls)
    }

    /// Whether `a` is the back-edge block of a loop whose merge block is `b`.
    fn back_to_merge(&self, back_edges: &[(u32, u32)], a: u32, b: u32) -> bool {
        let start = back_edges.partition_point(|&(from, _)| from < a);
        back_edges
            .get(start..)
            .unwrap_or(&[])
            .iter()
            .take_while(|&&(from, _)| from == a)
            .any(|&(_, header)| self.header_of(header).is_some_and(|h| h.merge == b))
    }

    /// Each case falls through to at most one other and is fallen into from
    /// at most one, in the order of its switch's targets.
    fn fallthrough(&self, n: u32, falls: &mut Falls) -> Result<(), Refusal> {
        let Falls {
            edges,
            into,
            positions,
        } = falls;
        edges.sort_unstable();
        edges.dedup();
        into.clear();
        reserve(into, edges.len())?;
        into.extend(edges.iter().map(|&(switch, _, to)| (switch, to)));
        into.sort_unstable();
        let twice_from = edges
            .windows(2)
            .find(|w| w[0].0 == w[1].0 && w[0].1 == w[1].1);
        let twice_into = into.windows(2).find(|w| w[0] == w[1]);
        if let Some(switch) = twice_from.map(|w| w[0].0).or(twice_into.map(|w| w[0].0)) {
            return Err(Rule::Fallthrough.at(self.terminator(switch)));
        }
        for switch in 0..n {
            if self
                .header_of(switch)
                .is_some_and(|h| h.kind == Kind::Switch)
            {
                self.target_order(switch, edges, positions)?;
            }
        }
        Ok(())
    }

    /// The order rules over one switch's targets: one label's targets
    /// consecutive, and a fall from one case to another from the last target
    /// of the first to the first of the second, directly or through a
    /// default that is no target.
    fn target_order(
        &self,
        switch: u32,
        falls: &[(u32, u32, u32)],
        positions: &mut Vec<(u32, u32)>,
    ) -> Result<(), Refusal> {
        let m = self.v.m;
        let terminator = self.terminator(switch);
        let at = Rule::Fallthrough.at(terminator);
        let operands = m.operands(terminator);
        let pairs = operands.get(2..).unwrap_or(&[]).chunks(2);
        // Each target's block and position, sorted by block then position.
        positions.clear();
        reserve(positions, pairs.len())?;
        positions.extend(pairs.enumerate().filter_map(|(position, pair)| {
            let block = match pair.get(1).map(|o| m.value(o)) {
                Some(Value::Id(id)) => find(&self.labels, id)?,
                _ => return None,
            };
            Some((block, u32::try_from(position).unwrap_or(NONE)))
        }));
        positions.sort_unstable();
        for pair in positions.windows(2) {
            if let [(b1, p1), (b2, p2)] = pair {
                if b1 == b2 && p1 + 1 != *p2 {
                    return Err(at);
                }
            }
        }
        let span = |block: u32| {
            let start = positions.partition_point(|&(b, _)| b < block);
            let run = positions.get(start..).unwrap_or(&[]);
            let count = run.iter().take_while(|&&(b, _)| b == block).count();
            let first = run.first().filter(|&&(b, _)| b == block)?.1;
            let last = run.get(count.checked_sub(1)?)?.1;
            Some((first, last))
        };
        let default = self
            .branch
            .successors(switch)
            .last()
            .copied()
            .unwrap_or(NONE);
        let start = falls.partition_point(|&(s, _, _)| s < switch);
        let mine = falls.get(start..).unwrap_or(&[]);
        let mine = &mine[..mine.iter().take_while(|&&(s, _, _)| s == switch).count()];
        for &(_, from, to) in mine {
            match (span(from), span(to)) {
                (Some((_, last)), Some((first, _))) if last + 1 != first => return Err(at),
                (Some((_, last)), None) if to == default => {
                    let onward = mine.iter().find(|&&(_, via, _)| via == default);
                    if let Some(&(_, _, beyond)) = onward {
                        if span(beyond).is_some_and(|(first, _)| last + 1 != first) {
                            return Err(at);
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Record `owner` at `block` of `list`, refusing a second owner.
fn mark(list: &mut [u32], block: u32, owner: u32) -> Result<(), ()> {
    match list.get_mut(block as usize) {
        Some(slot) if *slot == NONE => {
            *slot = owner;
            Ok(())
        }
        _ => Err(()),
    }
}

/// Whether construct `c` contains block `x`.
fn contains(dominance: &Dominators, c: &Construct, x: u32) -> bool {
    match c.kind {
        Kind::Selection | Kind::Switch | Kind::Case => {
            dominance.dominates(c.header, x) && !dominance.dominates(c.merge, x)
        }
        Kind::Loop => {
            dominance.dominates(c.header, x)
                && !dominance.dominates(c.merge, x)
                && !dominance.dominates(c.other, x)
        }
        Kind::Continue => dominance.dominates(c.header, x) && !dominance.dominates(c.merge, x),
    }
}

/// Open the construct `(kind, header, merge, other, back)` inside `current`.
fn open(
    list: &mut Vec<Construct>,
    current: &mut u32,
    label: &Instruction,
    (kind, header, merge, other, back): (Kind, u32, u32, u32, u32),
) -> Result<(), Refusal> {
    let parent = list.get(*current as usize).copied();
    let depth = parent.map_or(1, |p| p.depth + 1);
    if depth as usize > MAX_NESTING {
        return Err(Rule::Limit(Limit::Nesting).at(label));
    }
    let jump = ladder(list, *current);
    let index = len_u32(list);
    let near = |inherited: Option<u32>, own: bool| {
        if own {
            index
        } else {
            inherited.unwrap_or(NONE)
        }
    };
    push(
        list,
        Construct {
            kind,
            header,
            merge,
            other,
            back,
            parent: *current,
            depth,
            jump,
            near_loop: near(parent.map(|p| p.near_loop), kind == Kind::Loop),
            near_switch: near(parent.map(|p| p.near_switch), kind == Kind::Switch),
            near_continue: near(parent.map(|p| p.near_continue), kind == Kind::Continue),
        },
    )?;
    *current = index;
    Ok(())
}

/// Whether `from`'s chain holds the loop construct headed at `header`: the
/// last construct opened at the header, which is its loop's.
fn within_loop(constructs: &[Construct], state: &[u32], from: u32, header: u32) -> bool {
    let Some(&lp) = state.get(header as usize) else {
        return false;
    };
    constructs.get(lp as usize).is_some_and(|c| {
        c.kind == Kind::Loop && c.header == header && ancestor_at(constructs, from, c.depth) == lp
    })
}

/// The ladder ancestor of a construct opened inside `parent`: its
/// grandparent rung when the two rungs above are the same length, else its
/// parent. A rung's length then depends on its depth alone.
fn ladder(constructs: &[Construct], parent: u32) -> u32 {
    let up = jump_of(constructs, parent);
    let over = jump_of(constructs, up);
    if nesting(constructs, parent) - nesting(constructs, up)
        == nesting(constructs, up) - nesting(constructs, over)
    {
        over
    } else {
        parent
    }
}

/// How deep construct `c` is; zero outside every construct.
fn nesting(constructs: &[Construct], c: u32) -> u32 {
    constructs.get(c as usize).map_or(0, |c| c.depth)
}

fn jump_of(constructs: &[Construct], c: u32) -> u32 {
    constructs.get(c as usize).map_or(NONE, |c| c.jump)
}

/// The construct on `c`'s chain at depth `at`; [`NONE`] at depth zero.
fn ancestor_at(constructs: &[Construct], mut c: u32, at: u32) -> u32 {
    while let Some(construct) = constructs.get(c as usize).filter(|k| k.depth > at) {
        c = if nesting(constructs, construct.jump) < at {
            construct.parent
        } else {
            construct.jump
        };
    }
    c
}

/// The nearest construct both `a` and `b` lie in, or [`NONE`]. Two
/// constructs as deep as each other have ladder ancestors as deep as each
/// other, so the climb keeps them level.
fn common(constructs: &[Construct], a: u32, b: u32) -> u32 {
    let (mut a, mut b) = if nesting(constructs, a) >= nesting(constructs, b) {
        (a, b)
    } else {
        (b, a)
    };
    a = ancestor_at(constructs, a, nesting(constructs, b));
    while a != b {
        let (ja, jb) = (jump_of(constructs, a), jump_of(constructs, b));
        if ja == jb {
            a = constructs.get(a as usize).map_or(NONE, |c| c.parent);
            b = constructs.get(b as usize).map_or(NONE, |c| c.parent);
        } else {
            a = ja;
            b = jb;
        }
    }
    a
}

#[cfg(test)]
#[path = "ladder_tests.rs"]
mod ladder_tests;
