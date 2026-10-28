//! The shapes the generated grammar tables are written in, and the lookups the
//! decoder walks them through.

use crate::enumerant::Capability;
use crate::tables::{GLSL_STD_450, INSTRUCTIONS};
use crate::{Kind, Op, Version};

/// How many times an operand occurs: the grammar places every optional and
/// repeated operand last.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Quantity {
    One,
    Optional,
    Many,
}

/// How the decoder reads an operand kind.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Shape {
    /// One word naming an id.
    Id,
    /// One literal word.
    Word,
    /// A nul-terminated UTF-8 string, packed four bytes to a word.
    Text,
    /// A literal as wide as the instruction's numeric result type.
    Number,
    /// An opcode, then that opcode's own operands but its result type and
    /// result.
    SpecOp,
    /// A literal as wide as the switch's selector, then a label.
    SwitchTarget,
    /// Two operands of these kinds.
    Pair(Kind, Kind),
    /// One value, then the parameters its enumerant takes.
    Enum,
    /// A set of bits, then each set bit's parameters in ascending bit order.
    Bits,
}

/// What enables an instruction or enumerant in a module.
pub(crate) struct Gate {
    /// Any one of these must be declared; none, and it needs no capability.
    /// For a capability, the capabilities it implicitly declares instead.
    pub(crate) capabilities: &'static [Capability],
    /// Before `version`, any one of these extensions enables it.
    pub(crate) extensions: &'static [&'static str],
    /// The version it is core from; `None` where only an extension enables it.
    pub(crate) version: Option<Version>,
    /// The newest version it exists in.
    pub(crate) last: Option<Version>,
}

/// One instruction of the grammar.
pub(crate) struct Spec {
    pub(crate) op: Op,
    pub(crate) name: &'static str,
    pub(crate) operands: &'static [(Kind, Quantity)],
    pub(crate) gate: Gate,
}

impl Spec {
    /// How many of its leading operands are its result type and result.
    pub(crate) fn results(&self) -> usize {
        self.operands
            .iter()
            .take_while(|(kind, _)| matches!(kind, Kind::IdResultType | Kind::IdResult))
            .count()
    }

    /// The word, counted from its first operand's, holding its result.
    pub(crate) fn result_word(&self) -> Option<usize> {
        self.operands
            .iter()
            .position(|(kind, _)| *kind == Kind::IdResult)
    }

    /// Whether its first operand is its result type.
    pub(crate) fn has_result_type(&self) -> bool {
        matches!(self.operands.first(), Some((Kind::IdResultType, _)))
    }
}

/// One value of an enumerated kind, and the operands that follow it.
pub(crate) struct Enumerant {
    pub(crate) value: u32,
    pub(crate) parameters: &'static [Kind],
    pub(crate) gate: Gate,
}

/// One `GLSL.std.450` extended instruction, how many ids it takes, and the
/// capabilities any one of which it needs.
pub(crate) struct ExtInst {
    pub(crate) number: u32,
    pub(crate) operands: usize,
    pub(crate) capabilities: &'static [Capability],
}

/// The instruction `op` names, if the grammar recognises it.
pub(crate) fn instruction(op: Op) -> Option<&'static Spec> {
    let at = INSTRUCTIONS
        .binary_search_by_key(&op.code(), |spec| spec.op.code())
        .ok()?;
    INSTRUCTIONS.get(at)
}

/// The enumerant of `kind` whose value is `value`.
pub(crate) fn enumerant(kind: Kind, value: u32) -> Option<&'static Enumerant> {
    let enumerants = kind.enumerants();
    let at = enumerants
        .binary_search_by_key(&value, |enumerant| enumerant.value)
        .ok()?;
    enumerants.get(at)
}

/// The `GLSL.std.450` instruction numbered `number`.
pub(crate) fn glsl_std_450(number: u32) -> Option<&'static ExtInst> {
    let at = GLSL_STD_450
        .binary_search_by_key(&number, |inst| inst.number)
        .ok()?;
    GLSL_STD_450.get(at)
}
