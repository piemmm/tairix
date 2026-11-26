//! Uniformity analysis, as the specification defines it: each function's
//! body walked, callees first, into a graph whose edge X → Y says "where X
//! must be uniform, Y must be"; a derivative or barrier whose control flow
//! reaches the may-be-non-uniform node is refused, or reported at the
//! severity its diagnostic filter gives it; and what each function asks of
//! its callers kept as its tags.

use alloc::vec::Vec;
use core::fmt::Write as _;

use super::decl::{BuiltinValue, DeclInfo, EntryStage, Io, Signature};
use super::filter::{Level, Rule};
use super::graph::Graph;
use super::stmt::{BREAK, CONTINUE, NEXT, RETURN};
use super::{Checker, Denotes, LocalKind, Root, NONE};
use crate::diag::{error, push, reserve, Code, Outcome, Severity, Span, Stop};
use crate::predeclared::Builtin;
use crate::syntax::{BinaryOp, ExprId, ExprKind, Run, StmtId, StmtKind, UnaryOp};
use crate::types::{Access, Space, Texture, Ty};

/// The requirements below error severity a function breaks: each one's
/// level, where it stands, and what asked for it.
type Broken = Vec<(Level, Span, Trigger)>;

/// The node a value no analysis can prove uniform reaches.
const MBNU: u32 = 0;
/// The control flow a function is entered in.
const CF_START: u32 = 1;

/// The most nodes, edges and call records one function's analysis holds:
/// what keeps the analysis within the worker however deep its constructs
/// nest over however many variables, each level joining every variable
/// assigned within it.
const MAX_GRAPH: usize = 1 << 19;

/// Why a place must be uniform: derivatives, which a filter may relax, or
/// a barrier, which none may.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Trigger {
    Derivative,
    Barrier,
}

/// At `level`, node `node` must be uniform, for `trigger`, as the code at
/// `span` asks.
#[derive(Copy, Clone)]
struct Requirement {
    level: Level,
    node: u32,
    span: Span,
    trigger: Trigger,
}

/// What a call of a function asks of its caller.
#[derive(Clone, Default)]
pub(crate) struct Tags {
    call_site: Option<(Level, Trigger)>,
    return_non_uniform: bool,
    params: Vec<ParamTags>,
}

#[derive(Copy, Clone, Default)]
struct ParamTags {
    /// The argument must be uniform.
    required: Option<(Level, Trigger)>,
    /// The memory a function-space pointer argument reaches must be.
    contents_required: Option<(Level, Trigger)>,
    /// The result is uniform only where the argument is.
    return_contents: bool,
    /// What a function-space pointer reaches may be non-uniform after.
    pointer_non_uniform: bool,
    /// The arguments, and the memory of pointer arguments, whose
    /// uniformity what this pointer reaches takes after the call.
    feeds: ParamSet,
    feeds_contents: ParamSet,
}

impl ParamTags {
    /// Whether what this pointer reaches takes any argument's uniformity.
    fn fed(&self) -> bool {
        self.feeds != ParamSet::default() || self.feeds_contents != ParamSet::default()
    }
}

/// A set of a function's parameters, a bit each.
#[derive(Copy, Clone, Default, Eq, PartialEq)]
struct ParamSet([u64; 4]);

const _: () = assert!(
    crate::MAX_PARAMETERS <= 256,
    "a parameter set holds every parameter"
);

impl ParamSet {
    fn insert(&mut self, place: usize) -> Outcome<()> {
        let word = self.0.get_mut(place / 64).ok_or(Stop::Internal)?;
        *word |= 1 << (place % 64);
        Ok(())
    }

    /// The members not in `other`.
    fn without(self, other: Self) -> Self {
        let mut words = self.0;
        for (word, taken) in words.iter_mut().zip(other.0) {
            *word &= !taken;
        }
        Self(words)
    }

    /// Each member, ascending.
    const fn members(self) -> Members {
        Members {
            words: self.0,
            word: 0,
        }
    }
}

/// A [`ParamSet`]'s members, ascending: the set's words, emptied as they are
/// read.
struct Members {
    words: [u64; 4],
    word: usize,
}

impl Iterator for Members {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        while let Some(bits) = self.words.get_mut(self.word) {
            if *bits != 0 {
                let bit = bits.trailing_zeros() as usize;
                *bits &= *bits - 1;
                return Some(self.word * 64 + bit);
            }
            self.word += 1;
        }
        None
    }
}

/// A call whose pointer arguments' new contents take other arguments'
/// uniformity, as its callee's tags say. Those edges are read from the tags
/// as the graph is walked, so a call holds its arguments, not their square.
#[derive(Copy, Clone)]
struct CallFeeds {
    /// The first pointer argument's new-contents node; each other such
    /// argument's follows it.
    first: u32,
    /// How many new-contents nodes the call made.
    written: u32,
    /// The callee's place among the tags.
    callee: u32,
    /// Where the call's run of `fed` begins: each new-contents node's
    /// parameter, then each argument's value, then the contents each
    /// argument reached before the call.
    at: usize,
    args: usize,
}

/// A function's graph as walked: its edges, and what each call's pointer
/// arguments' new contents reach through their callee's tags.
struct Flow<'w> {
    graph: Graph,
    calls: &'w [CallFeeds],
    fed: &'w [u32],
    tags: &'w [Tags],
}

impl Flow<'_> {
    /// Mark every node `from` reaches, itself included.
    fn reach(&self, from: u32, marks: &mut Marks, stack: &mut Vec<u32>) -> Outcome<()> {
        stack.clear();
        visit(from, &mut marks.seen, stack)?;
        while let Some(node) = stack.pop() {
            for &target in self.graph.successors(node) {
                visit(target, &mut marks.seen, stack)?;
            }
            let Some((call, fed)) = self.fed_by(node)? else {
                continue;
            };
            let [values, contents] = marks.through.get_mut(call).ok_or(Stop::Internal)?;
            for place in fed.feeds.without(*values).members() {
                let target = *fed.args.get(place).ok_or(Stop::Internal)?;
                if target != MBNU {
                    values.insert(place)?;
                }
                visit(target, &mut marks.seen, stack)?;
            }
            for place in fed.feeds_contents.without(*contents).members() {
                let Some(&target) = fed.contents.get(place) else {
                    continue;
                };
                if target != MBNU {
                    contents.insert(place)?;
                }
                visit(target, &mut marks.seen, stack)?;
            }
        }
        Ok(())
    }

    /// The call, by its place among the calls, and what `node` reaches
    /// through its callee's tags, where `node` is a call's new-contents node.
    fn fed_by(&self, node: u32) -> Outcome<Option<(usize, Fed<'_>)>> {
        let Some((index, call)) = self
            .calls
            .partition_point(|call| call.first <= node)
            .checked_sub(1)
            .and_then(|index| Some((index, self.calls.get(index)?)))
            .filter(|(_, call)| {
                node.checked_sub(call.first)
                    .is_some_and(|k| k < call.written)
            })
        else {
            return Ok(None);
        };
        let at = call.at;
        let values = at + call.written as usize;
        let place = self
            .fed
            .get(at + (node - call.first) as usize)
            .ok_or(Stop::Internal)?;
        let args = self
            .fed
            .get(values..values + call.args)
            .ok_or(Stop::Internal)?;
        let contents = self
            .fed
            .get(values + call.args..values + 2 * call.args)
            .ok_or(Stop::Internal)?;
        let param = self
            .tags
            .get(call.callee as usize)
            .and_then(|tags| tags.params.get(*place as usize))
            .ok_or(Stop::Internal)?;
        Ok(Some((
            index,
            Fed {
                feeds: param.feeds,
                feeds_contents: param.feeds_contents,
                args,
                contents,
            },
        )))
    }

    /// Whether `from` reaches the may-be-non-uniform node, the nodes already
    /// marked left unwalked: each was reached before without reaching it.
    /// The node itself is never left marked, for every walk may reach it
    /// anew.
    fn reach_mbnu(&self, from: u32, marks: &mut Marks, stack: &mut Vec<u32>) -> Outcome<bool> {
        if marks.seen.get(from as usize).copied().unwrap_or(true) {
            return Ok(false);
        }
        self.reach(from, marks, stack)?;
        let mark = marks.seen.get_mut(MBNU as usize).ok_or(Stop::Internal)?;
        Ok(core::mem::replace(mark, false))
    }
}

/// What walks of a function's graph have marked: each node reached, and for
/// each call the parameters whose argument value and whose contents were
/// reached through its tags, which its later new-contents nodes then pass
/// over. The may-be-non-uniform node is never among those, for its mark is
/// lifted after each walk.
struct Marks {
    seen: Vec<bool>,
    through: Vec<[ParamSet; 2]>,
}

impl Marks {
    fn new(nodes: u32, calls: usize) -> Outcome<Self> {
        Ok(Self {
            seen: filled(nodes as usize, false)?,
            through: filled(calls, [ParamSet::default(); 2])?,
        })
    }

    /// Unmark everything, for a walk that begins anew.
    fn clear(&mut self) {
        self.seen.fill(false);
        self.through.fill([ParamSet::default(); 2]);
    }

    fn reached(&self, node: u32) -> bool {
        self.seen.get(node as usize).copied().unwrap_or(false)
    }
}

/// What a call's new-contents node reaches through its callee's tags: the
/// parameters feeding it by value and by contents, and the call's argument
/// values and the contents each argument reached before it.
struct Fed<'f> {
    feeds: ParamSet,
    feeds_contents: ParamSet,
    args: &'f [u32],
    contents: &'f [u32],
}

/// Mark `node` in `seen` and stack it, where it is a node not yet marked.
fn visit(node: u32, seen: &mut [bool], stack: &mut Vec<u32>) -> Outcome<()> {
    if let Some(mark) = seen.get_mut(node as usize) {
        if !*mark {
            *mark = true;
            push(stack, node)?;
        }
    }
    Ok(())
}

/// A function's parameter nodes: its value, and for a function-space
/// pointer the memory it reaches on entry and where returns join it.
#[derive(Copy, Clone)]
struct ParamNodes {
    value: u32,
    contents: u32,
    returned: Join,
}

/// A construct `break` and `continue` leave, and where each variable
/// assigned within it joins.
struct Exit {
    looping: bool,
    vars: Vec<ExitVar>,
}

/// A variable a construct assigns, and where its values join at the
/// construct's continuing block and after it.
#[derive(Copy, Clone)]
struct ExitVar {
    var: u32,
    continuing: Join,
    after: Join,
}

/// A node the values control leaves with join at, and the value last
/// carried to it, so leaving again with that value adds no edge.
#[derive(Copy, Clone)]
struct Join {
    node: u32,
    carried: u32,
}

impl Join {
    const fn new(node: u32) -> Self {
        Self {
            node,
            carried: NONE,
        }
    }

    fn carry(&mut self, edges: &mut Vec<(u32, u32)>, value: u32) -> Outcome<()> {
        if self.carried == value {
            return Ok(());
        }
        self.carried = value;
        add_edge(edges, self.node, value)
    }
}

/// Add the edge `from` → `to` to `edges`: none from or to no node, and
/// none from a node to itself.
fn add_edge(edges: &mut Vec<(u32, u32)>, from: u32, to: u32) -> Outcome<()> {
    if from == NONE || to == NONE || from == to {
        return Ok(());
    }
    push(edges, (from, to))
}

/// One branch of a selection, walked: the control flow it ends with,
/// whether it reaches the statement after, where the log stood as it
/// began, and the run of the selection's finals holding its variables'
/// last values.
#[derive(Copy, Clone)]
struct Branch {
    cf: u32,
    reaches: bool,
    at: usize,
    finals: (usize, usize),
}

/// One function's walk.
struct Walk<'w, 'a> {
    c: &'w Checker<'a>,
    /// The function's name, where a walk past its bound is refused.
    name: Span,
    tags: &'w [Tags],
    position: &'w [u32],
    stage: Option<EntryStage>,
    signature: Signature,
    nodes: u32,
    edges: Vec<(u32, u32)>,
    requirements: Vec<Requirement>,
    first_local: u32,
    /// Each tracked variable's value node, by its local's slot.
    current: Vec<u32>,
    /// Each pointer let's value node, and whether it reaches a whole
    /// variable.
    let_value: Vec<u32>,
    let_full: Vec<bool>,
    log: Vec<(u32, u32)>,
    exits: Vec<Exit>,
    params: Vec<ParamNodes>,
    value_return: u32,
    /// The variables a branch assigned, gathered as it ends.
    assigned_scratch: Vec<u32>,
    /// The calls whose pointer arguments' new contents their callees' tags
    /// feed, in the order their nodes were made, and their runs.
    calls: Vec<CallFeeds>,
    fed: Vec<u32>,
}

impl Checker<'_> {
    /// Analyse every function, callees first.
    pub(crate) fn uniformity(&mut self) -> Outcome<()> {
        let count = self.tree.decls.len();
        let mut position: Vec<u32> = Vec::new();
        reserve(&mut position, count)?;
        position.resize(count, NONE);
        let mut tags: Vec<Tags> = Vec::new();
        reserve(&mut tags, self.fn_order.len())?;
        let mut notes: Broken = Vec::new();
        for at in 0..self.fn_order.len() {
            let function = self.fn_order[at];
            let (function_tags, reported) = self.analyse(function, &tags, &position)?;
            for report in reported {
                push(&mut notes, report)?;
            }
            position[function as usize] = u32::try_from(at).map_err(|_| Stop::OutOfMemory)?;
            tags.push(function_tags);
        }
        for (level, span, trigger) in notes {
            let severity = level.severity().unwrap_or(Severity::Info);
            self.note(severity, Code::Uniformity, span, |m| message(m, trigger))?;
        }
        Ok(())
    }

    /// One function's graph, walked and read: its tags, and the
    /// requirements below error severity it breaks.
    fn analyse(&self, function: u32, tags: &[Tags], position: &[u32]) -> Outcome<(Tags, Broken)> {
        let Some(&DeclInfo::Function(signature)) = self.decls.get(function as usize) else {
            return Err(Stop::Internal);
        };
        let crate::syntax::DeclKind::Fn(declaration) = self
            .tree
            .decls
            .get(function as usize)
            .ok_or(Stop::Internal)?
            .kind
        else {
            return Err(Stop::Internal);
        };
        let &(_, first_local, end_local) = self
            .fn_locals
            .iter()
            .find(|&&(decl, _, _)| decl == function)
            .ok_or(Stop::Internal)?;
        let slots = (end_local - first_local) as usize;
        let mut walk = Walk {
            c: self,
            name: declaration.name,
            tags,
            position,
            stage: signature.stage,
            signature,
            nodes: CF_START + 1,
            edges: Vec::new(),
            requirements: Vec::new(),
            first_local,
            current: filled(slots, NONE)?,
            let_value: filled(slots, NONE)?,
            let_full: filled(slots, true)?,
            log: Vec::new(),
            exits: Vec::new(),
            params: Vec::new(),
            value_return: NONE,
            assigned_scratch: Vec::new(),
            calls: Vec::new(),
            fed: Vec::new(),
        };
        let param_count = signature.params.1 - signature.params.0;
        for place in 0..param_count {
            let value = walk.node()?;
            let (contents, returned) = if walk.function_pointer(place) {
                let contents = walk.node()?;
                (contents, walk.node()?)
            } else {
                (NONE, NONE)
            };
            push(
                &mut walk.params,
                ParamNodes {
                    value,
                    contents,
                    returned: Join::new(returned),
                },
            )?;
            if contents != NONE {
                *walk.current.get_mut(place as usize).ok_or(Stop::Internal)? = contents;
            }
        }
        if signature.result.is_some() {
            walk.value_return = walk.node()?;
        }
        let Some(StmtKind::Compound(body)) = self.tree.stmt(declaration.body).map(|s| s.kind)
        else {
            return Err(Stop::Internal);
        };
        walk.block(CF_START, body)?;
        // Falling off the end returns.
        if self
            .stmt_behaviour
            .get(declaration.body.0 as usize)
            .is_some_and(|b| b & NEXT != 0)
        {
            walk.returned_contents()?;
        }
        walk.read_tags()
    }
}

/// Write the message for a broken requirement of `trigger`.
fn message(m: &mut crate::diag::Message, trigger: Trigger) -> core::fmt::Result {
    match trigger {
        Trigger::Derivative => m.write_str(
            "this computes derivatives in control flow the uniformity analysis cannot prove uniform",
        ),
        Trigger::Barrier => {
            m.write_str("this barrier stands in control flow the uniformity analysis cannot prove uniform")
        }
    }
}

fn filled<T: Clone>(count: usize, fill: T) -> Outcome<Vec<T>> {
    let mut list = Vec::new();
    reserve(&mut list, count)?;
    list.resize(count, fill);
    Ok(list)
}

impl Walk<'_, '_> {
    fn node(&mut self) -> Outcome<u32> {
        let node = self.nodes;
        self.nodes = node.checked_add(1).ok_or(Stop::OutOfMemory)?;
        Ok(node)
    }

    fn edge(&mut self, from: u32, to: u32) -> Outcome<()> {
        add_edge(&mut self.edges, from, to)
    }

    /// A new node with an edge to each of `to`.
    fn joined(&mut self, to: &[u32]) -> Outcome<u32> {
        let node = self.node()?;
        for &target in to {
            self.edge(node, target)?;
        }
        Ok(node)
    }

    fn require(&mut self, level: Level, node: u32, span: Span, trigger: Trigger) -> Outcome<()> {
        if level == Level::Off {
            return Ok(());
        }
        push(
            &mut self.requirements,
            Requirement {
                level,
                node,
                span,
                trigger,
            },
        )
    }

    /// Whether parameter `place` is a pointer to function memory, which the
    /// analysis follows as a variable.
    fn function_pointer(&self, place: u32) -> bool {
        self.c
            .params
            .get((self.signature.params.0 + place) as usize)
            .is_some_and(|p| {
                matches!(
                    self.c.types.get(p.ty),
                    Ty::Pointer {
                        space: Space::Function,
                        ..
                    }
                )
            })
    }

    /// The slot of the variable `root` names, where the analysis follows
    /// its value: a function-scope variable, or a function-space pointer
    /// parameter.
    fn slot(&self, root: Root) -> Option<u32> {
        match root {
            Root::Local(local) => local.checked_sub(self.first_local),
            Root::Param(place) if self.function_pointer(place) => Some(place),
            Root::Param(_) | Root::Global(_) => None,
        }
    }

    fn assign(&mut self, slot: u32, node: u32) -> Outcome<()> {
        let cell = self.current.get_mut(slot as usize).ok_or(Stop::Internal)?;
        let previous = core::mem::replace(cell, node);
        push(&mut self.log, (slot, previous))
    }

    fn undo(&mut self, to: usize) {
        while self.log.len() > to {
            if let Some((slot, previous)) = self.log.pop() {
                if let Some(cell) = self.current.get_mut(slot as usize) {
                    *cell = previous;
                }
            }
        }
    }

    fn behaviour(&self, id: StmtId) -> u8 {
        self.c
            .stmt_behaviour
            .get(id.0 as usize)
            .copied()
            .unwrap_or(0)
    }

    /// Statements in sequence; one that does not finish normally ends the
    /// walk, for nothing after it runs.
    fn block(&mut self, cf: u32, run: Run) -> Outcome<u32> {
        let tree = self.c.tree;
        let mut cf = cf;
        for &stmt in tree.stmt_list(run) {
            cf = self.statement(cf, stmt)?;
            self.within_bound()?;
            if self.behaviour(stmt) & NEXT == 0 {
                break;
            }
        }
        Ok(cf)
    }

    /// Refuse the function whose graph has grown past [`MAX_GRAPH`].
    fn within_bound(&self) -> Outcome<()> {
        if self.nodes as usize + self.edges.len() + self.fed.len() <= MAX_GRAPH {
            return Ok(());
        }
        Err(error(Code::ModuleTooLarge, self.name, |m| {
            write!(
                m,
                "the uniformity analysis of a function holds at most {MAX_GRAPH} nodes and edges"
            )
        }))
    }

    #[expect(clippy::too_many_lines, reason = "one arm per statement kind")]
    fn statement(&mut self, cf: u32, id: StmtId) -> Outcome<u32> {
        let tree = self.c.tree;
        let stmt = *tree.stmt(id).ok_or(Stop::Internal)?;
        match stmt.kind {
            StmtKind::Compound(run) => self.block(cf, run),
            StmtKind::Assign { lhs, op, rhs } => {
                let old = if op.is_some() {
                    Some(self.value(cf, lhs)?)
                } else {
                    None
                };
                let pending = self.target(cf, lhs)?;
                let value = self.value(cf, rhs)?;
                let value = match old {
                    Some(old) => self.joined(&[old, value])?,
                    None => value,
                };
                self.write(&pending, value)?;
                Ok(cf)
            }
            StmtKind::Increment(target) | StmtKind::Decrement(target) => {
                let old = self.value(cf, target)?;
                let pending = self.target(cf, target)?;
                self.write(&pending, old)?;
                Ok(cf)
            }
            StmtKind::Phony(e) | StmtKind::Call(e) => {
                self.value(cf, e)?;
                Ok(cf)
            }
            StmtKind::If { .. } => self.if_chain(cf, id),
            StmtKind::Switch {
                selector, clauses, ..
            } => self.switch(cf, id, selector, clauses),
            StmtKind::Loop {
                body, continuing, ..
            } => {
                let continuing = continuing
                    .map(|index| {
                        tree.continuings
                            .get(index as usize)
                            .copied()
                            .ok_or(Stop::Internal)
                    })
                    .transpose()?;
                self.looping(
                    cf,
                    id,
                    |walk, cf| walk.block(cf, body),
                    |walk, cf| match continuing {
                        Some(c) => {
                            let cf = walk.block(cf, c.body)?;
                            match c.break_if {
                                Some(condition) => {
                                    let value = walk.value(cf, condition)?;
                                    walk.leave(false)?;
                                    walk.joined(&[value])
                                }
                                None => Ok(cf),
                            }
                        }
                        None => Ok(cf),
                    },
                )
            }
            StmtKind::For {
                init,
                condition,
                update,
                body,
            } => {
                let cf = match init {
                    Some(init) => self.statement(cf, init)?,
                    None => cf,
                };
                self.looping(
                    cf,
                    id,
                    |walk, cf| {
                        let cf = match condition {
                            Some(condition) => walk.guard(cf, condition)?,
                            None => cf,
                        };
                        walk.statement(cf, body)
                    },
                    |walk, cf| match update {
                        Some(update) => walk.statement(cf, update),
                        None => Ok(cf),
                    },
                )
            }
            StmtKind::While { condition, body } => self.looping(
                cf,
                id,
                |walk, cf| {
                    let cf = walk.guard(cf, condition)?;
                    walk.statement(cf, body)
                },
                |_, cf| Ok(cf),
            ),
            StmtKind::Break => {
                self.leave(false)?;
                Ok(cf)
            }
            StmtKind::Continue => {
                self.leave(true)?;
                Ok(cf)
            }
            StmtKind::Return(value) => {
                if let Some(e) = value {
                    let v = self.value(cf, e)?;
                    self.edge(self.value_return, v)?;
                }
                self.returned_contents()?;
                Ok(cf)
            }
            StmtKind::Let(value) => {
                let v = self.value(cf, value.init)?;
                let slot = self.declared_slot(id)?;
                *self
                    .let_value
                    .get_mut(slot as usize)
                    .ok_or(Stop::Internal)? = v;
                let full = self.full_pointer(value.init);
                *self.let_full.get_mut(slot as usize).ok_or(Stop::Internal)? = full;
                Ok(cf)
            }
            StmtKind::Var(var) => {
                let v = match var.init {
                    Some(init) => self.value(cf, init)?,
                    None => cf,
                };
                let slot = self.declared_slot(id)?;
                self.assign(slot, v)?;
                Ok(cf)
            }
            StmtKind::Const(_) | StmtKind::ConstAssert(_) | StmtKind::Discard | StmtKind::Empty => {
                Ok(cf)
            }
        }
    }

    fn declared_slot(&self, id: StmtId) -> Outcome<u32> {
        let local = *self.c.stmt_local.get(id.0 as usize).ok_or(Stop::Internal)?;
        local.checked_sub(self.first_local).ok_or(Stop::Internal)
    }

    /// The memory function-space pointer parameters reach, as a return
    /// leaves it.
    fn returned_contents(&mut self) -> Outcome<()> {
        for (place, param) in self.params.iter_mut().enumerate() {
            if param.returned.node != NONE {
                let value = *self.current.get(place).ok_or(Stop::Internal)?;
                param.returned.carry(&mut self.edges, value)?;
            }
        }
        Ok(())
    }

    /// Control leaving the nearest loop — or, for a `break`, loop or
    /// switch — joins its variables' values at where it goes.
    fn leave(&mut self, continuing: bool) -> Outcome<()> {
        let exit = self
            .exits
            .iter_mut()
            .rev()
            .find(|exit| exit.looping || !continuing)
            .ok_or(Stop::Internal)?;
        for var in &mut exit.vars {
            let value = *self.current.get(var.var as usize).ok_or(Stop::Internal)?;
            let join = if continuing {
                &mut var.continuing
            } else {
                &mut var.after
            };
            join.carry(&mut self.edges, value)?;
        }
        Ok(())
    }

    /// `if !(condition) { break; }`, a `for` or `while` loop's guard.
    fn guard(&mut self, cf: u32, condition: ExprId) -> Outcome<u32> {
        let value = self.value(cf, condition)?;
        self.leave(false)?;
        self.joined(&[value])
    }

    /// The variables assigned anywhere in statement `id`: their values may
    /// differ wherever control leaves it.
    fn assigned(&self, id: StmtId) -> Outcome<Vec<u32>> {
        let mut vars: Vec<u32> = Vec::new();
        let mut roots: Vec<ExprId> = Vec::new();
        self.c.each_statement(id, |stmt_id, stmt| {
            match stmt.kind {
                StmtKind::Assign { lhs: target, .. }
                | StmtKind::Increment(target)
                | StmtKind::Decrement(target) => {
                    if let Some(slot) = self.c.root_of(target).and_then(|r| self.slot(r)) {
                        push(&mut vars, slot)?;
                    }
                }
                StmtKind::Var(_) => push(&mut vars, self.declared_slot(stmt_id)?)?,
                _ => {}
            }
            self.c.statement_roots(stmt, &mut roots)
        })?;
        for &root in &roots {
            for raw in self.c.shape.subtree(root) {
                let Some(ExprKind::Call(_, args)) = self.c.tree.expr(ExprId(raw)).map(|e| e.kind)
                else {
                    continue;
                };
                if !matches!(self.c.info(ExprId(raw)).denotes, Denotes::CallFunction(_)) {
                    continue;
                }
                for &arg in self.c.tree.expr_list(args) {
                    let pointer = matches!(
                        self.c.types.get(self.c.info(arg).converted),
                        Ty::Pointer {
                            space: Space::Function,
                            ..
                        }
                    );
                    if let Some(slot) = pointer
                        .then(|| self.c.root_of(arg))
                        .flatten()
                        .and_then(|r| self.slot(r))
                    {
                        push(&mut vars, slot)?;
                    }
                }
            }
        }
        vars.sort_unstable();
        vars.dedup();
        Ok(vars)
    }

    /// Open the exit of a loop or switch over `vars`.
    fn open_exit(&mut self, looping: bool, vars: &[u32]) -> Outcome<()> {
        let mut joins = Vec::new();
        reserve(&mut joins, vars.len())?;
        for &var in vars {
            let continuing = if looping { self.node()? } else { NONE };
            joins.push(ExitVar {
                var,
                continuing: Join::new(continuing),
                after: Join::new(self.node()?),
            });
        }
        push(
            &mut self.exits,
            Exit {
                looping,
                vars: joins,
            },
        )
    }

    /// A loop: its body walked from a fresh control-flow node where it may
    /// run again, its variables' values joined at the head, at the
    /// continuing block, and after it.
    fn looping(
        &mut self,
        cf: u32,
        id: StmtId,
        body: impl FnOnce(&mut Self, u32) -> Outcome<u32>,
        continuing: impl FnOnce(&mut Self, u32) -> Outcome<u32>,
    ) -> Outcome<u32> {
        let b1 = self.c.loop_bodies.get(id.0 as usize).copied().unwrap_or(0);
        let vars = self.assigned(id)?;
        let mut heads = Vec::new();
        reserve(&mut heads, vars.len())?;
        for &var in &vars {
            let previous = *self.current.get(var as usize).ok_or(Stop::Internal)?;
            let head = self.joined(&[previous])?;
            self.assign(var, head)?;
            heads.push(head);
        }
        self.open_exit(true, &vars)?;
        let repeats = b1 & (NEXT | CONTINUE) != 0;
        let entry = if repeats { self.node()? } else { cf };
        let cf1 = body(self, entry)?;
        let exit = self.exits.len() - 1;
        if b1 & NEXT != 0 {
            self.leave(true)?;
        }
        let result = if repeats {
            for index in 0..vars.len() {
                let joins = self.exits[exit].vars[index];
                self.assign(joins.var, joins.continuing.node)?;
            }
            let cf2 = continuing(self, cf1)?;
            for (&head, &var) in heads.iter().zip(&vars) {
                let value = *self.current.get(var as usize).ok_or(Stop::Internal)?;
                self.edge(head, value)?;
            }
            self.edge(entry, cf2)?;
            self.edge(entry, cf)?;
            if b1 & RETURN == 0 {
                cf
            } else {
                entry
            }
        } else if b1 == BREAK {
            cf
        } else {
            cf1
        };
        self.close_exit()?;
        Ok(result)
    }

    /// Close the innermost exit: each of its variables takes its join after
    /// the construct.
    fn close_exit(&mut self) -> Outcome<()> {
        let done = self.exits.pop().ok_or(Stop::Internal)?;
        for var in &done.vars {
            self.assign(var.var, var.after.node)?;
        }
        Ok(())
    }

    /// A switch: each clause walked from the selector's control flow and
    /// the state before any clause, their variables joined after it.
    fn switch(&mut self, cf: u32, id: StmtId, selector: ExprId, clauses: Run) -> Outcome<u32> {
        let tree = self.c.tree;
        let value = self.value(cf, selector)?;
        let vars = self.assigned(id)?;
        self.open_exit(false, &vars)?;
        let at = self.log.len();
        let mut ends: Vec<u32> = Vec::new();
        for clause in tree.clause_list(clauses) {
            let end = self.statement(value, clause.body)?;
            if self.behaviour(clause.body) & NEXT != 0 {
                self.leave(false)?;
            }
            push(&mut ends, end)?;
            self.undo(at);
        }
        self.close_exit()?;
        if self.behaviour(id) == NEXT {
            Ok(cf)
        } else {
            self.joined(&ends)
        }
    }

    /// An `if` and its `else if` chain, link by link without recursion:
    /// each branch walked from the state its conditions leave, the
    /// variables any part assigns joined after, and control flow
    /// reconverging link by link as nested `if`s would.
    fn if_chain(&mut self, cf: u32, first: StmtId) -> Outcome<u32> {
        struct Link {
            stmt: StmtId,
            cf_in: u32,
            condition: u32,
        }
        let tree = self.c.tree;
        let start = self.log.len();
        let mut links: Vec<Link> = Vec::new();
        let mut branches: Vec<Branch> = Vec::new();
        let mut finals: Vec<(u32, u32)> = Vec::new();
        let mut at = first;
        let mut cf_in = cf;
        let tail = loop {
            let StmtKind::If {
                condition,
                accept,
                reject,
            } = tree.stmt(at).ok_or(Stop::Internal)?.kind
            else {
                return Err(Stop::Internal);
            };
            let value = self.value(cf_in, condition)?;
            let accepted = self.branch(value, accept, &mut finals)?;
            push(&mut branches, accepted)?;
            push(
                &mut links,
                Link {
                    stmt: at,
                    cf_in,
                    condition: value,
                },
            )?;
            match reject {
                None => break None,
                Some(next)
                    if matches!(tree.stmt(next).map(|s| s.kind), Some(StmtKind::If { .. })) =>
                {
                    at = next;
                    cf_in = value;
                }
                Some(other) => break Some(other),
            }
        };
        let else_in = links.last().ok_or(Stop::Internal)?.condition;
        let otherwise = match tail {
            Some(other) => self.branch(else_in, other, &mut finals)?,
            None => Branch {
                cf: else_in,
                reaches: true,
                at: self.log.len(),
                finals: (finals.len(), finals.len()),
            },
        };
        let mut inner = otherwise.cf;
        for (link, accepted) in links.iter().zip(&branches).rev() {
            inner = if self.behaviour(link.stmt) == NEXT {
                link.cf_in
            } else {
                self.joined(&[accepted.cf, inner])?
            };
        }
        push(&mut branches, otherwise)?;
        self.join_branches(start, &branches, &finals)?;
        Ok(inner)
    }

    /// Join each variable a selection's branches or conditions assigned at
    /// a node with an edge to each value it holds where a reaching branch
    /// ends: what that branch left it, or what it held as a branch that
    /// left it alone began. The values are found from the assignments, so
    /// a long chain costs what it assigns, not its variables times its
    /// branches.
    fn join_branches(
        &mut self,
        start: usize,
        branches: &[Branch],
        finals: &[(u32, u32)],
    ) -> Outcome<()> {
        // Each value a reaching branch left, by variable and branch.
        let mut left: Vec<(u32, usize, u32)> = Vec::new();
        for (index, branch) in branches.iter().enumerate().filter(|(_, b)| b.reaches) {
            let (first, end) = branch.finals;
            for &(var, value) in finals.get(first..end).ok_or(Stop::Internal)? {
                push(&mut left, (var, index, value))?;
            }
        }
        left.sort_unstable_by_key(|&(var, index, _)| (var, index));
        // Each assignment the conditions made, by variable and position.
        let mut made: Vec<(u32, usize, u32)> = Vec::new();
        let conditions = self.log.get(start..).ok_or(Stop::Internal)?;
        for (position, &(var, previous)) in (start..).zip(conditions) {
            push(&mut made, (var, position, previous))?;
        }
        made.sort_unstable_by_key(|&(var, position, _)| (var, position));
        // How many branches before each reach the statement after.
        let mut reaching: Vec<usize> = Vec::new();
        reserve(&mut reaching, branches.len() + 1)?;
        reaching.push(0);
        let mut count = 0;
        for branch in branches {
            count += usize::from(branch.reaches);
            reaching.push(count);
        }
        let (mut left, mut made) = (left.as_slice(), made.as_slice());
        loop {
            let var = match (left.first(), made.first()) {
                (Some(l), Some(m)) => l.0.min(m.0),
                (Some(&(var, ..)), None) | (None, Some(&(var, ..))) => var,
                (None, None) => return Ok(()),
            };
            let (own, rest) = left.split_at(left.iter().take_while(|l| l.0 == var).count());
            left = rest;
            let (assigned, rest) = made.split_at(made.iter().take_while(|m| m.0 == var).count());
            made = rest;
            let join = self.node()?;
            for &(_, _, value) in own {
                self.edge(join, value)?;
            }
            // Each branch that began before a condition's assignment saw
            // the value that assignment replaced; those after it, the value
            // now.
            let now = *self.current.get(var as usize).ok_or(Stop::Internal)?;
            let bounds = assigned
                .iter()
                .map(|&(_, position, previous)| (Some(position), previous))
                .chain([(None, now)]);
            let mut owners = own.iter().map(|&(_, index, _)| index).peekable();
            let mut low = 0;
            for (bound, value) in bounds {
                let high = bound.map_or(branches.len(), |position| {
                    branches.partition_point(|b| b.at <= position)
                });
                let mut owning = 0;
                while owners.next_if(|&index| index < high).is_some() {
                    owning += 1;
                }
                let reached = reaching
                    .get(high)
                    .zip(reaching.get(low))
                    .and_then(|(high, low)| high.checked_sub(*low))
                    .ok_or(Stop::Internal)?;
                if reached > owning {
                    self.edge(join, value)?;
                }
                low = high;
            }
            self.assign(var, join)?;
        }
    }

    /// Walk `stmt` from `cf` as one branch of a selection, its variables'
    /// last values kept in `finals` where it reaches the statement after,
    /// and the state before it restored for the next.
    fn branch(&mut self, cf: u32, stmt: StmtId, finals: &mut Vec<(u32, u32)>) -> Outcome<Branch> {
        let at = self.log.len();
        let end = self.statement(cf, stmt)?;
        let reaches = self.behaviour(stmt) & NEXT != 0;
        let first = finals.len();
        if reaches {
            self.keep_finals(at, finals)?;
        }
        self.undo(at);
        Ok(Branch {
            cf: end,
            reaches,
            at,
            finals: (first, finals.len()),
        })
    }

    /// Record each variable the log assigns past `at`, once, with its value
    /// now.
    fn keep_finals(&mut self, at: usize, finals: &mut Vec<(u32, u32)>) -> Outcome<()> {
        let vars = &mut self.assigned_scratch;
        vars.clear();
        for &(var, _) in self.log.get(at..).ok_or(Stop::Internal)? {
            push(vars, var)?;
        }
        vars.sort_unstable();
        vars.dedup();
        for &var in vars.iter() {
            let value = *self.current.get(var as usize).ok_or(Stop::Internal)?;
            push(finals, (var, value))?;
        }
        Ok(())
    }

    /// Whether the pointer expression `id` reaches a whole variable rather
    /// than part of one.
    fn full_pointer(&self, id: ExprId) -> bool {
        let mut at = id;
        loop {
            match self.c.tree.expr(at).map(|e| e.kind) {
                Some(ExprKind::Unary(UnaryOp::AddressOf | UnaryOp::Indirection, inner)) => {
                    at = inner;
                }
                Some(ExprKind::Ident(_)) => {
                    return match self.c.info(at).denotes {
                        Denotes::Local(local) => {
                            let slot = local.checked_sub(self.first_local);
                            let kind = self.c.local_list.get(local as usize).map(|l| l.kind);
                            match kind {
                                Some(LocalKind::Let) => slot
                                    .and_then(|s| self.let_full.get(s as usize).copied())
                                    .unwrap_or(false),
                                _ => true,
                            }
                        }
                        _ => true,
                    }
                }
                _ => return false,
            }
        }
    }

    /// Where an assignment writes through `target`: the variable it
    /// follows, if any, the values of the indexes on the way, and whether
    /// the write covers the whole variable.
    fn target(&mut self, cf: u32, target: ExprId) -> Outcome<Pending> {
        let mut indexes: Vec<u32> = Vec::new();
        let mut full = true;
        let mut at = target;
        loop {
            match self.c.tree.expr(at).ok_or(Stop::Internal)?.kind {
                ExprKind::Unary(UnaryOp::Indirection | UnaryOp::AddressOf, inner) => at = inner,
                ExprKind::Index(base, index) => {
                    full = false;
                    push(&mut indexes, self.value(cf, index)?)?;
                    at = base;
                }
                ExprKind::Member(base, _) => {
                    full = full && self.whole_swizzle(at);
                    at = base;
                }
                _ => break,
            }
        }
        let mut via_let = NONE;
        let slot = match self.c.info(at).denotes {
            Denotes::Local(local) => {
                let kind = self.c.local_list.get(local as usize).map(|l| l.kind);
                let own = local.checked_sub(self.first_local).ok_or(Stop::Internal)?;
                if kind == Some(LocalKind::Let) {
                    via_let = self.let_value.get(own as usize).copied().unwrap_or(NONE);
                    full = full && self.let_full.get(own as usize).copied().unwrap_or(false);
                }
                self.c.root_of(at).and_then(|root| self.slot(root))
            }
            _ => None,
        };
        Ok(Pending {
            slot,
            full,
            indexes,
            via_let,
            cf,
        })
    }

    /// Whether member access `id` is a swizzle naming every component of
    /// its vector once, which writes the whole of it.
    fn whole_swizzle(&self, id: ExprId) -> bool {
        let Denotes::Swizzle(lanes, count) = self.c.info(id).denotes else {
            return false;
        };
        let Some(ExprKind::Member(base, _)) = self.c.tree.expr(id).map(|e| e.kind) else {
            return false;
        };
        let store = self.c.types.store_of(self.c.info(base).ty);
        let Some(Ty::Vector(n, _)) = store.map(|s| self.c.types.get(s)) else {
            return false;
        };
        let lanes = &lanes[..usize::from(count)];
        count == n && (0..n).all(|lane| lanes.contains(&lane))
    }

    /// The write `pending` describes, of a value whose node is `value`.
    fn write(&mut self, pending: &Pending, value: u32) -> Outcome<()> {
        let Some(slot) = pending.slot else {
            return Ok(());
        };
        let written = self.joined(&[pending.cf, value, pending.via_let])?;
        for &index in &pending.indexes {
            self.edge(written, index)?;
        }
        if !pending.full {
            let previous = *self.current.get(slot as usize).ok_or(Stop::Internal)?;
            self.edge(written, previous)?;
        }
        self.assign(slot, written)
    }

    /// Whether the memory view rooted at `id` is read by the load rule —
    /// at `id`, or at a view built on it.
    fn loaded(&self, id: ExprId) -> bool {
        let mut at = id;
        loop {
            let info = self.c.info(at);
            if matches!(self.c.types.get(info.ty), Ty::Reference { .. })
                && info.converted != info.ty
            {
                return true;
            }
            let Some(parent) = self.c.shape.parent(at) else {
                return false;
            };
            match self.c.tree.expr(parent).map(|e| e.kind) {
                Some(ExprKind::Unary(UnaryOp::AddressOf | UnaryOp::Indirection, _)) => at = parent,
                Some(ExprKind::Index(base, _)) if base == at => at = parent,
                Some(ExprKind::Member(base, _)) if base == at => {
                    let view = matches!(
                        self.c.types.get(info.ty),
                        Ty::Reference { .. } | Ty::Pointer { .. }
                    );
                    if view
                        && matches!(self.c.info(parent).denotes, Denotes::Swizzle(_, n) if n > 1)
                    {
                        return true;
                    }
                    at = parent;
                }
                _ => return false,
            }
        }
    }

    /// Whether a module-scope variable's memory cannot change under the
    /// shader, so reads of it are uniform.
    fn read_only(&self, decl: u32) -> bool {
        match self.c.decls.get(decl as usize) {
            Some(DeclInfo::Global(global)) => match global.space {
                Space::Uniform | Space::Immediate | Space::Handle => true,
                Space::Storage => global.access == Access::Read,
                _ => false,
            },
            _ => true,
        }
    }

    /// The value node of the memory `root` reaches, read at `cf`.
    fn root_value(&mut self, root: Root, cf: u32) -> Outcome<u32> {
        if let Some(slot) = self.slot(root) {
            let value = *self.current.get(slot as usize).ok_or(Stop::Internal)?;
            return self.joined(&[cf, value]);
        }
        Ok(match root {
            Root::Global(decl) if !self.read_only(decl) => MBNU,
            Root::Param(place) if self.writable_pointer(place) => MBNU,
            _ => cf,
        })
    }

    /// Whether pointer parameter `place` reaches memory other invocations
    /// may write: private, workgroup or writable storage memory.
    fn writable_pointer(&self, place: u32) -> bool {
        self.c
            .params
            .get((self.signature.params.0 + place) as usize)
            .is_some_and(|p| {
                matches!(
                    self.c.types.get(p.ty),
                    Ty::Pointer {
                        space: Space::Private | Space::Workgroup | Space::Storage,
                        access: Access::ReadWrite,
                        ..
                    }
                )
            })
    }

    /// The value node of an entry point's parameter `place`: uniform only
    /// for the built-ins every invocation reads alike.
    fn stage_input(&self, place: u32, cf: u32) -> u32 {
        let uniform_builtin = |io: Io| {
            matches!(
                io,
                Io::Builtin {
                    value: BuiltinValue::WorkgroupId | BuiltinValue::NumWorkgroups,
                    ..
                }
            )
        };
        let Some(param) = self
            .c
            .params
            .get((self.signature.params.0 + place) as usize)
        else {
            return MBNU;
        };
        let uniform = match (param.io, self.c.types.get(param.ty)) {
            (Io::None, Ty::Struct(structure)) => {
                let count = u32::try_from(self.c.types.members_of(structure).len()).unwrap_or(0);
                (0..count).all(|index| {
                    self.c
                        .member_attrs_of(structure, index)
                        .is_some_and(|attrs| uniform_builtin(attrs.io))
                })
            }
            (io, _) => uniform_builtin(io),
        };
        if uniform {
            cf
        } else {
            MBNU
        }
    }

    /// An identifier's value node, read at `cf`.
    fn ident(&mut self, id: ExprId, cf: u32) -> Outcome<u32> {
        match self.c.info(id).denotes {
            Denotes::Local(local) => {
                let info = *self
                    .c
                    .local_list
                    .get(local as usize)
                    .ok_or(Stop::Internal)?;
                let slot = local.checked_sub(self.first_local).ok_or(Stop::Internal)?;
                match info.kind {
                    LocalKind::Var => {
                        if self.loaded(id) {
                            self.root_value(Root::Local(local), cf)
                        } else {
                            Ok(cf)
                        }
                    }
                    LocalKind::Let => {
                        let own = self.let_value.get(slot as usize).copied().unwrap_or(NONE);
                        match info.root {
                            Some(root) if self.loaded(id) => {
                                let reached = self.root_value(root, cf)?;
                                self.joined(&[cf, own, reached])
                            }
                            _ => self.joined(&[cf, own]),
                        }
                    }
                    LocalKind::Const => Ok(cf),
                    LocalKind::Param(place) => {
                        if self.stage.is_some() {
                            return Ok(self.stage_input(place, cf));
                        }
                        let nodes = *self.params.get(place as usize).ok_or(Stop::Internal)?;
                        if self.function_pointer(place) {
                            if self.loaded(id) {
                                self.root_value(Root::Param(place), cf)
                            } else {
                                Ok(nodes.value)
                            }
                        } else if matches!(self.c.types.get(info.ty), Ty::Pointer { .. }) {
                            if self.loaded(id) {
                                self.root_value(Root::Param(place), cf)
                            } else {
                                Ok(cf)
                            }
                        } else {
                            self.joined(&[cf, nodes.value])
                        }
                    }
                }
            }
            Denotes::Global(decl) => {
                if self.loaded(id) {
                    self.root_value(Root::Global(decl), cf)
                } else {
                    Ok(cf)
                }
            }
            _ => Ok(cf),
        }
    }

    /// The value node of expression `root` evaluated at `cf`: its subtree
    /// walked children first, without recursion, a short-circuit's right
    /// side evaluated in the control flow its left side's value gives.
    fn value(&mut self, cf: u32, root: ExprId) -> Outcome<u32> {
        let range = self.c.shape.subtree(root);
        let base = *range.start();
        let mut values: Vec<u32> = filled((root.0 - base) as usize + 1, cf)?;
        let mut flows: Vec<(u32, u32)> = Vec::new();
        for raw in range {
            while flows.last().is_some_and(|&(until, _)| raw > until) {
                flows.pop();
            }
            let here = flows.last().map_or(cf, |&(_, flow)| flow);
            let id = ExprId(raw);
            let of = |values: &[u32], child: ExprId| {
                values
                    .get((child.0 - base) as usize)
                    .copied()
                    .unwrap_or(here)
            };
            let kind = self.c.tree.expr(id).ok_or(Stop::Internal)?.kind;
            let value = match kind {
                ExprKind::Literal(_) => here,
                ExprKind::Ident(_) => self.ident(id, here)?,
                ExprKind::Unary(_, operand) => of(&values, operand),
                ExprKind::Member(base_expr, _) => of(&values, base_expr),
                ExprKind::Binary(BinaryOp::LogicalAnd | BinaryOp::LogicalOr, _, rhs) => {
                    of(&values, rhs)
                }
                ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) => {
                    let (a, b) = (of(&values, a), of(&values, b));
                    self.joined(&[a, b])?
                }
                ExprKind::Call(_, args) => {
                    let mut arg_values: Vec<u32> = Vec::new();
                    for &arg in self.c.tree.expr_list(args) {
                        push(&mut arg_values, of(&values, arg))?;
                    }
                    self.call(id, here, args, &arg_values)?
                }
            };
            *values
                .get_mut((raw - base) as usize)
                .ok_or(Stop::Internal)? = value;
            if let Some(parent) = self.c.shape.parent(id) {
                if let Some(ExprKind::Binary(
                    BinaryOp::LogicalAnd | BinaryOp::LogicalOr,
                    lhs,
                    rhs,
                )) = self.c.tree.expr(parent).map(|e| e.kind)
                {
                    if lhs == id {
                        push(&mut flows, (rhs.0, value))?;
                    }
                }
            }
        }
        values
            .get((root.0 - base) as usize)
            .copied()
            .ok_or(Stop::Internal)
    }

    /// A call's value node, at `cf`, its arguments' nodes `args`.
    fn call(&mut self, id: ExprId, cf: u32, run: Run, args: &[u32]) -> Outcome<u32> {
        let span = self.c.span_of(id);
        let result = self.joined(&[cf])?;
        match self.c.info(id).denotes {
            Denotes::CallFunction(decl) => self.user_call(decl, (span, result, cf), run, args)?,
            Denotes::CallBuiltin(builtin) => {
                self.builtin_call(builtin, (span, result, cf), run, args)?;
            }
            _ => {
                for &arg in args {
                    self.edge(result, arg)?;
                }
            }
        }
        Ok(result)
    }

    fn user_call(
        &mut self,
        decl: u32,
        (span, result, cf): (Span, u32, u32),
        run: Run,
        args: &[u32],
    ) -> Outcome<()> {
        let at = self
            .position
            .get(decl as usize)
            .copied()
            .filter(|&p| p != NONE)
            .ok_or(Stop::Internal)?;
        let tags = self.tags.get(at as usize).ok_or(Stop::Internal)?;
        if let Some((level, trigger)) = tags.call_site {
            self.require(level, cf, span, trigger)?;
        }
        if tags.return_non_uniform {
            self.edge(result, MBNU)?;
        }
        let exprs = self.c.tree.expr_list(run);
        // What each function-space pointer argument reaches before the call.
        let mut before: Vec<Option<(u32, u32)>> = Vec::new();
        for &arg in exprs {
            let reached = self
                .c
                .root_of(arg)
                .and_then(|root| self.slot(root))
                .filter(|_| {
                    matches!(
                        self.c.types.get(self.c.info(arg).converted),
                        Ty::Pointer {
                            space: Space::Function,
                            ..
                        }
                    )
                });
            let value = reached
                .map(|slot| {
                    self.current
                        .get(slot as usize)
                        .copied()
                        .ok_or(Stop::Internal)
                        .map(|v| (slot, v))
                })
                .transpose()?;
            push(&mut before, value)?;
        }
        // The new-contents nodes are made one after another from here.
        let first = self.nodes;
        let fed_at = self.fed.len();
        let feeding = tags.params.iter().any(ParamTags::fed);
        let mut written_count = 0;
        for (place, (&expr, &arg)) in exprs.iter().zip(args).enumerate() {
            let param = tags.params.get(place).ok_or(Stop::Internal)?;
            let arg_span = self.c.span_of(expr);
            if let Some((level, trigger)) = param.required {
                self.require(level, arg, arg_span, trigger)?;
            }
            if param.return_contents {
                self.edge(result, arg)?;
            }
            let Some((slot, previous)) = before.get(place).copied().flatten() else {
                continue;
            };
            let written = self.node()?;
            if !self.full_pointer(expr) {
                self.edge(written, previous)?;
            }
            if param.pointer_non_uniform {
                self.edge(written, MBNU)?;
            }
            if feeding {
                if param.feeds.members().any(|feed| feed >= args.len()) {
                    return Err(Stop::Internal);
                }
                push(
                    &mut self.fed,
                    u32::try_from(place).map_err(|_| Stop::Internal)?,
                )?;
                written_count += 1;
            }
            if let Some((level, trigger)) = param.contents_required {
                self.require(level, previous, arg_span, trigger)?;
            }
            self.assign(slot, written)?;
        }
        if written_count > 0 {
            for &arg in args {
                push(&mut self.fed, arg)?;
            }
            for place in 0..args.len() {
                let reached = before.get(place).copied().flatten();
                push(&mut self.fed, reached.map_or(NONE, |(_, value)| value))?;
            }
            push(
                &mut self.calls,
                CallFeeds {
                    first,
                    written: written_count,
                    callee: at,
                    at: fed_at,
                    args: args.len(),
                },
            )?;
        }
        Ok(())
    }

    fn builtin_call(
        &mut self,
        builtin: Builtin,
        (span, result, cf): (Span, u32, u32),
        run: Run,
        args: &[u32],
    ) -> Outcome<()> {
        use Builtin as B;
        match builtin {
            B::StorageBarrier | B::TextureBarrier | B::WorkgroupBarrier => {
                self.require(Level::Error, cf, span, Trigger::Barrier)
            }
            B::WorkgroupUniformLoad => {
                self.require(Level::Error, cf, span, Trigger::Barrier)?;
                let pointer = args.first().copied().ok_or(Stop::Internal)?;
                self.require(Level::Error, pointer, span, Trigger::Barrier)
            }
            B::Dpdx
            | B::DpdxCoarse
            | B::DpdxFine
            | B::Dpdy
            | B::DpdyCoarse
            | B::DpdyFine
            | B::Fwidth
            | B::FwidthCoarse
            | B::FwidthFine
            | B::TextureSample
            | B::TextureSampleBias
            | B::TextureSampleCompare => {
                let level = self.c.level_at(Rule::DerivativeUniformity, span.start);
                self.require(level, cf, span, Trigger::Derivative)?;
                self.edge(result, MBNU)
            }
            B::TextureLoad => {
                let texture = self
                    .c
                    .tree
                    .expr_list(run)
                    .first()
                    .map(|&t| self.c.info(t).converted);
                let read_write = texture.is_some_and(|t| {
                    matches!(
                        self.c.types.get(t),
                        Ty::Texture(Texture::Storage {
                            access: Access::ReadWrite,
                            ..
                        })
                    )
                });
                if read_write {
                    return self.edge(result, MBNU);
                }
                for &arg in args {
                    self.edge(result, arg)?;
                }
                Ok(())
            }
            B::AtomicLoad
            | B::AtomicStore
            | B::AtomicAdd
            | B::AtomicSub
            | B::AtomicMax
            | B::AtomicMin
            | B::AtomicAnd
            | B::AtomicOr
            | B::AtomicXor
            | B::AtomicExchange
            | B::AtomicCompareExchangeWeak => self.edge(result, MBNU),
            _ => {
                for &arg in args {
                    self.edge(result, arg)?;
                }
                Ok(())
            }
        }
    }

    /// The graph read: each requirement held, at error severity on pain of
    /// a stop and below it reported; and the function's tags.
    fn read_tags(self) -> Outcome<(Tags, Broken)> {
        let Self {
            nodes,
            mut edges,
            requirements,
            params,
            value_return,
            calls,
            fed,
            tags: callees,
            log,
            current,
            exits,
            let_value,
            let_full,
            assigned_scratch,
            ..
        } = self;
        // What followed the values is let go before the graph is laid out
        // beside its edges, and the edges once it is.
        drop((log, current, exits, let_value, let_full, assigned_scratch));
        let graph = Graph::from_edges(nodes as usize, &mut edges)?;
        drop(edges);
        let flow = Flow {
            graph,
            calls: &calls,
            fed: &fed,
            tags: callees,
        };
        let mut marks = Marks::new(nodes, calls.len())?;
        let mut stack: Vec<u32> = Vec::new();
        let mut tags = Tags::default();
        reserve(&mut tags.params, params.len())?;
        tags.params.resize(params.len(), ParamTags::default());
        let mut reported: Broken = Vec::new();
        let mut cf_reached = false;
        for level in [Level::Error, Level::Warning, Level::Info] {
            let mut broken = false;
            for requirement in requirements.iter().filter(|r| r.level == level) {
                if broken {
                    break;
                }
                if requirement.node == MBNU
                    || flow.reach_mbnu(requirement.node, &mut marks, &mut stack)?
                {
                    if level == Level::Error {
                        return Err(error(Code::Uniformity, requirement.span, |m| {
                            message(m, requirement.trigger)
                        }));
                    }
                    push(
                        &mut reported,
                        (level, requirement.span, requirement.trigger),
                    )?;
                    broken = true;
                    continue;
                }
                let tag = Some((level, requirement.trigger));
                if marks.reached(CF_START) && !cf_reached {
                    cf_reached = true;
                    tags.call_site = tag;
                }
                for (place, nodes) in params.iter().enumerate() {
                    let param = &mut tags.params[place];
                    if param.required.is_none() && marks.reached(nodes.value) {
                        param.required = tag;
                    }
                    if nodes.contents != NONE
                        && param.contents_required.is_none()
                        && marks.reached(nodes.contents)
                    {
                        param.contents_required = tag;
                    }
                }
            }
        }
        if value_return != NONE {
            marks.clear();
            flow.reach(value_return, &mut marks, &mut stack)?;
            tags.return_non_uniform = marks.reached(MBNU);
            for (place, nodes) in params.iter().enumerate() {
                tags.params[place].return_contents = marks.reached(nodes.value);
            }
        }
        for place in 0..params.len() {
            let returned = params[place].returned.node;
            if returned == NONE {
                continue;
            }
            marks.clear();
            flow.reach(returned, &mut marks, &mut stack)?;
            tags.params[place].pointer_non_uniform = marks.reached(MBNU);
            for (other, nodes) in params.iter().enumerate() {
                if marks.reached(nodes.value) {
                    tags.params[place].feeds.insert(other)?;
                }
                if nodes.contents != NONE && marks.reached(nodes.contents) {
                    tags.params[place].feeds_contents.insert(other)?;
                }
            }
        }
        Ok((tags, reported))
    }
}

/// A write an assignment makes: the variable it follows, where the
/// analysis follows one, the index values and pointer it goes through,
/// whether it covers the whole variable, and its control flow.
struct Pending {
    slot: Option<u32>,
    full: bool,
    indexes: Vec<u32>,
    via_let: u32,
    cf: u32,
}
